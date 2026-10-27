//! The control socket (`run/sip-faxd.sock`): a unix domain socket, mode 0600, that puts
//! [`Hub::serve_connection`] in front of whoever may open it, and the client side the
//! `stop` / `status` commands use.

use std::io;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream as StdStream;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::unix::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::watch;
use tokio::task::{JoinHandle, JoinSet};

use super::hub::Hub;
use super::protocol::{Reply, Request};

/// How long `shutdown` lets the open connections finish (the `stopped` line of a `stop`).
const CONNECTION_GRACE: Duration = Duration::from_secs(2);

/// The listening socket.
pub struct ControlServer {
    path: PathBuf,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl ControlServer {
    /// Creates the socket (replacing a stale file, refusing a live one) with mode 0600 and starts
    /// serving. Only the user that runs the service (and root) are let in: the mode keeps the
    /// others out of the file, and every connection's peer uid is checked as well.
    pub fn bind(path: &Path, hub: Arc<Hub>) -> io::Result<ControlServer> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        if path.exists() {
            match StdStream::connect(path) {
                Ok(_) => {
                    return Err(io::Error::new(
                        io::ErrorKind::AddrInUse,
                        format!("another sip-faxd answers on {}", path.display()),
                    ));
                }
                // nobody listens: what a crash left behind
                Err(_) => std::fs::remove_file(path)?,
            }
        }
        let std_listener = bind_private(path).map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("cannot create the control socket {}: {e}", path.display()),
            )
        })?;
        std_listener.set_nonblocking(true)?;
        let listener = UnixListener::from_std(std_listener)?;
        let (stop, stop_rx) = watch::channel(false);
        let task = tokio::spawn(accept_loop(listener, hub, stop_rx));
        Ok(ControlServer {
            path: path.to_path_buf(),
            stop,
            task,
        })
    }

    /// Stops accepting, lets the running connections finish, removes the socket file.
    pub async fn shutdown(self) {
        let _ = self.stop.send(true);
        let _ = self.task.await;
        let _ = std::fs::remove_file(&self.path);
    }
}

/// Binds `path` with mode 0600 and no window in which another user could connect. The socket is
/// created with the mode 0777 minus the umask, so it is bound inside a fresh directory of mode
/// 0700, set to 0600 there and renamed into place. (Changing the umask instead would change it
/// for the whole process: directories other threads create meanwhile would lose their x bits.)
fn bind_private(path: &Path) -> io::Result<std::os::unix::net::UnixListener> {
    use std::os::unix::fs::DirBuilderExt;
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = path.file_name().unwrap_or_default().to_string_lossy();
    let private = parent.join(format!(".{name}.{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&private);
    std::fs::DirBuilder::new().mode(0o700).create(&private)?;
    let result = (|| {
        // the umask may have taken bits from the directory too; chmod is not subject to it
        std::fs::set_permissions(&private, std::fs::Permissions::from_mode(0o700))?;
        let tmp = private.join("s");
        let listener = std::os::unix::net::UnixListener::bind(&tmp)?;
        std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
        std::fs::rename(&tmp, path)?;
        Ok(listener)
    })();
    let _ = std::fs::remove_dir_all(&private);
    result
}

async fn accept_loop(listener: UnixListener, hub: Arc<Hub>, mut stop: watch::Receiver<bool>) {
    let me = rustix::process::geteuid().as_raw();
    let mut conns = JoinSet::new();
    loop {
        tokio::select! {
            _ = stop.changed() => break,
            Some(_) = conns.join_next(), if !conns.is_empty() => {}
            r = listener.accept() => {
                let Ok((stream, _)) = r else { continue };
                match stream.peer_cred() {
                    Ok(c) if c.uid() == me || c.uid() == 0 => {}
                    other => {
                        tracing::warn!(peer = ?other.map(|c| c.uid()),
                            "control socket: connection from another user refused");
                        continue;
                    }
                }
                let hub = hub.clone();
                conns.spawn(async move {
                    let (r, w) = stream.into_split();
                    hub.serve_connection(r, w).await;
                });
            }
        }
    }
    let _ = tokio::time::timeout(CONNECTION_GRACE, async {
        while conns.join_next().await.is_some() {}
    })
    .await;
}

// ------------------------------------------------------------------------ client

/// One connection to a running service.
pub struct Client {
    reader: BufReader<OwnedReadHalf>,
    writer: OwnedWriteHalf,
}

impl Client {
    /// Connects to the control socket.
    pub async fn connect(path: &Path) -> io::Result<Client> {
        let (r, w) = UnixStream::connect(path).await?.into_split();
        Ok(Client {
            reader: BufReader::new(r),
            writer: w,
        })
    }

    /// Sends a request.
    pub async fn send(&mut self, req: &Request) -> io::Result<()> {
        let mut line = serde_json::to_string(req).map_err(io::Error::other)?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.flush().await
    }

    /// The next reply line; `None` when the service closed the connection.
    pub async fn next(&mut self) -> io::Result<Option<Reply>> {
        let mut line = String::new();
        if self.reader.read_line(&mut line).await? == 0 {
            return Ok(None);
        }
        serde_json::from_str(line.trim())
            .map(Some)
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("{e}: {line:?}")))
    }

    /// One request, one reply.
    pub async fn call(&mut self, req: &Request) -> io::Result<Reply> {
        self.send(req).await?;
        self.next().await?.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "the service closed the connection",
            )
        })
    }
}

/// `status` of the service behind `path`, or why it cannot be had.
pub async fn fetch_status(path: &Path) -> io::Result<super::protocol::StatusInfo> {
    let mut c = Client::connect(path).await?;
    match c.call(&Request::Status).await? {
        Reply::Status(s) => Ok(s),
        other => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("unexpected reply: {other:?}"),
        )),
    }
}

#[cfg(test)]
mod tests;
