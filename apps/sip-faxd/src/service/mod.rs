//! Service management (M4d): the control socket, `start` / `stop` / `restart` / `status` /
//! `logs`, the log file with its rotation, and the PID file.
//!
//! The process side (unix socket, flock, setsid) only exists on unix; on other systems the
//! commands answer that they are not supported there, and `serve` runs without a PID file and
//! control socket, so the workspace still builds and tests on Windows.

// Without a unix socket (Windows) the hub, the protocol and the log tail are built and tested
// but nothing starts them.
#![cfg_attr(not(unix), allow(dead_code))]

pub mod config;
#[cfg(unix)]
pub mod control;
pub mod hub;
pub mod logfile;
pub mod protocol;
pub mod systemd;

#[cfg(unix)]
pub mod cli;
#[cfg(unix)]
pub mod pidfile;

use std::path::Path;
use std::sync::Arc;

pub use config::LogSection;

/// The PID file lock of the running service (a placeholder where there is no flock).
#[cfg(unix)]
pub use pidfile::PidLock;

/// Placeholder: no PID file on this platform.
#[cfg(not(unix))]
pub struct PidLock;

/// Placeholder error type.
#[cfg(not(unix))]
#[derive(Debug)]
pub enum LockError {}

#[cfg(not(unix))]
impl std::fmt::Display for LockError {
    fn fmt(&self, _: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match *self {}
    }
}

#[cfg(not(unix))]
impl PidLock {
    /// Always succeeds: nothing to lock.
    pub fn acquire(_: &Path) -> Result<PidLock, LockError> {
        Ok(PidLock)
    }
}

/// The control socket of the running service.
#[cfg(unix)]
pub struct Control(control::ControlServer);

/// Placeholder: no control socket on this platform.
#[cfg(not(unix))]
pub struct Control;

impl Control {
    /// Opens the control socket.
    #[cfg(unix)]
    pub fn bind(path: &Path, hub: Arc<hub::Hub>) -> std::io::Result<Control> {
        control::ControlServer::bind(path, hub).map(Control)
    }

    /// Not available here.
    #[cfg(not(unix))]
    pub fn bind(_: &Path, _: Arc<hub::Hub>) -> std::io::Result<Control> {
        Err(std::io::Error::new(
            std::io::ErrorKind::Unsupported,
            "the control socket needs a unix system",
        ))
    }

    /// Closes it (clients waiting for `stopped` get it first).
    pub async fn shutdown(self) {
        #[cfg(unix)]
        self.0.shutdown().await;
    }
}

/// What a command prints where it cannot work.
#[cfg(not(unix))]
pub fn unsupported(command: &str) -> std::process::ExitCode {
    eprintln!(
        "sip-faxd: error: `{command}` is only available on Linux; run `sip-faxd serve` in the foreground here"
    );
    std::process::ExitCode::from(1)
}

/// Detaches the process from its terminal and session (`serve --daemonized`).
#[cfg(unix)]
pub fn detach_from_terminal() {
    // fails with EPERM when the process already leads a group (started by hand): harmless
    let _ = rustix::process::setsid();
}

/// No terminal to detach from.
#[cfg(not(unix))]
pub fn detach_from_terminal() {}

/// SIGTERM / SIGINT (Ctrl+C on other systems), as many times as they come.
pub struct Signals {
    #[cfg(unix)]
    term: Option<tokio::signal::unix::Signal>,
    #[cfg(unix)]
    int: Option<tokio::signal::unix::Signal>,
}

impl Signals {
    /// Installs the handlers. Do it before anything that takes long, so a signal is never lost
    /// to the default action.
    pub fn install() -> Signals {
        #[cfg(unix)]
        {
            use tokio::signal::unix::{SignalKind, signal};
            Signals {
                term: signal(SignalKind::terminate()).ok(),
                int: signal(SignalKind::interrupt()).ok(),
            }
        }
        #[cfg(not(unix))]
        Signals {}
    }

    /// Resolves at the next signal.
    pub async fn next(&mut self) {
        #[cfg(unix)]
        {
            async fn one(s: &mut Option<tokio::signal::unix::Signal>) {
                match s {
                    Some(s) => {
                        s.recv().await;
                    }
                    None => std::future::pending().await,
                }
            }
            if self.term.is_none() && self.int.is_none() {
                let _ = tokio::signal::ctrl_c().await;
                return;
            }
            tokio::select! {
                () = one(&mut self.term) => {}
                () = one(&mut self.int) => {}
            }
        }
        #[cfg(not(unix))]
        {
            let _ = tokio::signal::ctrl_c().await;
        }
    }
}

/// Opens the `tracing` output of `serve`: the rotating log file (when `[log] file` is set) and,
/// unless detached, stderr as well. A log file that cannot be opened is an error only for a
/// detached service (nobody would see stderr then).
pub fn init_logging(
    log: &LogSection,
    base: &Path,
    detached: bool,
) -> Result<logfile::LogSink, String> {
    let level = if log.level.is_empty() {
        "info".to_string()
    } else {
        log.level.clone()
    };
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new(&level));
    let mut file = None;
    let mut problem = None;
    if let Some(path) = log.file_path(base) {
        let cfg = logfile::RotateConfig {
            path: path.clone(),
            max_bytes: (log.max_size_mb * 1024.0 * 1024.0) as u64,
            daily: log.daily,
            keep: log.keep as usize,
            compress: log.compress,
        };
        match logfile::RotatingFile::open(cfg, Arc::new(logfile::SystemClock)) {
            Ok(f) => file = Some(f),
            Err(e) => problem = Some(format!("cannot open the log file {}: {e}", path.display())),
        }
    }
    if let (Some(p), true) = (&problem, detached) {
        return Err(p.clone());
    }
    let sink = logfile::LogSink::new(file, !detached);
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(sink.clone())
        .with_ansi(false)
        .init();
    if let Some(p) = problem {
        tracing::error!("{p}; logging to stderr only");
    }
    Ok(sink)
}
