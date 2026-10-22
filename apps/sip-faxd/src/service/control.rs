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
        // the socket is created with the mode 0777 minus the umask: close the window in which
        // another user could connect before the chmod
        let old = rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o177));
        let bound = std::os::unix::net::UnixListener::bind(path);
        rustix::process::umask(old);
        let std_listener = bound.map_err(|e| {
            io::Error::new(
                e.kind(),
                format!("cannot create the control socket {}: {e}", path.display()),
            )
        })?;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
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
mod tests {
    use super::*;
    use crate::service::hub::{HubInfo, Progress};
    use std::os::unix::fs::MetadataExt;

    fn hub() -> Arc<Hub> {
        Hub::new(
            HubInfo {
                pid: std::process::id(),
                version: "t".into(),
                listen: vec![],
            },
            Duration::from_secs(300),
            Vec::new,
        )
    }

    fn short_dir() -> tempfile::TempDir {
        // unix socket paths are limited to about 100 bytes
        tempfile::Builder::new()
            .prefix("sf")
            .tempdir_in("/tmp")
            .unwrap()
    }

    #[tokio::test]
    async fn m10_the_socket_is_mode_0600_and_answers_status_even_under_a_permissive_umask() {
        let d = short_dir();
        let p = d.path().join("run").join("c.sock");
        let old = rustix::process::umask(rustix::fs::Mode::from_raw_mode(0o000));
        let srv = ControlServer::bind(&p, hub());
        rustix::process::umask(old);
        let srv = srv.unwrap();
        let meta = std::fs::metadata(&p).unwrap();
        assert_eq!(meta.mode() & 0o777, 0o600, "mode {:o}", meta.mode());
        let s = fetch_status(&p).await.unwrap();
        assert_eq!((s.pid, s.state.as_str()), (std::process::id(), "running"));
        srv.shutdown().await;
        assert!(!p.exists(), "the socket file goes away");
    }

    #[tokio::test]
    async fn m03_a_stale_socket_file_is_replaced_and_a_live_one_is_refused() {
        let d = short_dir();
        let p = d.path().join("c.sock");
        // a socket nobody listens on: bind, then drop the listener without unlinking
        drop(std::os::unix::net::UnixListener::bind(&p).unwrap());
        assert!(p.exists());
        let srv = ControlServer::bind(&p, hub()).expect("stale file replaced");
        assert!(fetch_status(&p).await.is_ok());
        let second = ControlServer::bind(&p, hub());
        let e = second.err().expect("a live socket is refused");
        assert_eq!(e.kind(), io::ErrorKind::AddrInUse);
        assert!(e.to_string().contains("another sip-faxd"), "{e}");
        // the first one still works
        assert!(fetch_status(&p).await.is_ok());
        srv.shutdown().await;
    }

    #[tokio::test]
    async fn m03_a_path_that_is_too_long_for_a_unix_socket_is_reported_with_the_path() {
        let d = short_dir();
        let p = d.path().join("x".repeat(120)).join("c.sock");
        let e = ControlServer::bind(&p, hub()).err().expect("too long");
        assert!(e.to_string().contains("control socket"), "{e}");
    }

    #[tokio::test]
    async fn m09_a_stop_over_the_real_socket_ends_with_stopped_and_shutdown_waits_for_it() {
        let d = short_dir();
        let p = d.path().join("c.sock");
        let h = hub();
        let srv = ControlServer::bind(&p, h.clone()).unwrap();
        let mut c = Client::connect(&p).await.unwrap();
        c.send(&Request::Stop {
            force: false,
            timeout: Some(30),
        })
        .await
        .unwrap();
        assert!(matches!(
            c.next().await.unwrap(),
            Some(Reply::Stopping {
                timeout_secs: 30,
                ..
            })
        ));
        assert_eq!(h.stop_requested().await.timeout, Duration::from_secs(30));
        h.set_progress(Progress::Stopped);
        srv.shutdown().await;
        assert_eq!(c.next().await.unwrap(), Some(Reply::Stopped));
        assert_eq!(c.next().await.unwrap(), None);
    }

    #[tokio::test]
    async fn m07_the_client_reads_replies_line_by_line() {
        let d = short_dir();
        let p = d.path().join("c.sock");
        let srv = ControlServer::bind(&p, hub()).unwrap();
        let mut c = Client::connect(&p).await.unwrap();
        assert!(
            matches!(c.call(&Request::Calls).await.unwrap(), Reply::Calls { calls } if calls.is_empty())
        );
        assert!(matches!(
            c.call(&Request::Status).await.unwrap(),
            Reply::Status(_)
        ));
        srv.shutdown().await;
        assert!(Client::connect(&p).await.is_err(), "no socket any more");
    }
}
