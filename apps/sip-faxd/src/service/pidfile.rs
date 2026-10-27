//! The PID file with `flock`: the lock is held for as long as the service runs, so "is it
//! running?" is answered by the kernel (a crashed process leaves a file but no lock), and a
//! second `serve` / `start` cannot get in.

use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use rustix::fs::{FlockOperation, flock};
use rustix::io::Errno;

/// Why the PID file could not be taken.
#[derive(Debug)]
pub enum LockError {
    /// Another process holds it.
    Held {
        /// Its pid, when the file says.
        pid: Option<u32>,
    },
    /// The file could not be created or written.
    Io(io::Error),
}

impl fmt::Display for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            LockError::Held { pid: Some(p) } => {
                write!(f, "sip-faxd is already running (pid {p})")
            }
            LockError::Held { pid: None } => write!(f, "sip-faxd is already running"),
            LockError::Io(e) => write!(f, "cannot use the PID file: {e}"),
        }
    }
}

impl std::error::Error for LockError {}

impl From<io::Error> for LockError {
    fn from(e: io::Error) -> Self {
        LockError::Io(e)
    }
}

/// The locked PID file of this process. Dropping it removes the file and then the lock.
#[derive(Debug)]
pub struct PidLock {
    // the descriptor keeps the lock; it is closed (and the lock released) after the file is gone
    _file: File,
    path: PathBuf,
}

impl PidLock {
    /// Locks `path` exclusively and writes this process's pid into it.
    pub fn acquire(path: &Path) -> Result<PidLock, LockError> {
        if let Some(dir) = path.parent() {
            fs::create_dir_all(dir)?;
        }
        let mut file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)?;
        match flock(&file, FlockOperation::NonBlockingLockExclusive) {
            Ok(()) => {}
            Err(Errno::WOULDBLOCK) => {
                return Err(LockError::Held {
                    pid: read_pid(path),
                });
            }
            Err(e) => return Err(LockError::Io(e.into())),
        }
        file.set_len(0)?;
        file.write_all(format!("{}\n", std::process::id()).as_bytes())?;
        file.sync_all()?;
        Ok(PidLock {
            _file: file,
            path: path.to_path_buf(),
        })
    }
}

impl Drop for PidLock {
    fn drop(&mut self) {
        // still holding the lock: nobody can have replaced the file in between
        let _ = fs::remove_file(&self.path);
    }
}

/// The pid the file says (whether or not that process lives).
pub fn read_pid(path: &Path) -> Option<u32> {
    let mut s = String::new();
    File::open(path).ok()?.read_to_string(&mut s).ok()?;
    s.trim().parse().ok()
}

/// `Some(pid)` when a process holds the lock of `path` (`0` when its pid is not written yet),
/// `None` when nobody does, whatever the file contains.
pub fn running_pid(path: &Path) -> Option<u32> {
    let file = File::open(path).ok()?;
    match flock(&file, FlockOperation::NonBlockingLockShared) {
        // nobody holds an exclusive lock; ours is released when `file` closes
        Ok(()) => None,
        Err(Errno::WOULDBLOCK) => Some(read_pid(path).unwrap_or(0)),
        Err(_) => None,
    }
}

/// Removes a PID file that no process holds (what a crash or a failed start leaves).
pub fn remove_if_stale(path: &Path) {
    if running_pid(path).is_none() {
        let _ = fs::remove_file(path);
    }
}

/// Sends SIGTERM to `pid`.
pub fn terminate(pid: u32) -> io::Result<()> {
    let pid = i32::try_from(pid)
        .ok()
        .and_then(rustix::process::Pid::from_raw)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "bad pid"))?;
    rustix::process::kill_process(pid, rustix::process::Signal::TERM).map_err(io::Error::from)
}

#[cfg(test)]
mod tests;
