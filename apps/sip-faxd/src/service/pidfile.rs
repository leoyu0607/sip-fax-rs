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
mod tests {
    use super::*;

    #[test]
    fn m01_the_lock_writes_the_pid_excludes_a_second_holder_and_cleans_up() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("run").join("x.pid");
        assert_eq!(running_pid(&p), None, "no file");
        let lock = PidLock::acquire(&p).unwrap();
        assert_eq!(read_pid(&p), Some(std::process::id()));
        // flock locks belong to the open file description: a second open in this process
        // conflicts just like another process would
        assert_eq!(running_pid(&p), Some(std::process::id()));
        match PidLock::acquire(&p) {
            Err(LockError::Held { pid }) => assert_eq!(pid, Some(std::process::id())),
            other => panic!("second lock: {other:?}"),
        }
        assert!(
            PidLock::acquire(&p)
                .unwrap_err()
                .to_string()
                .contains("already running")
        );
        drop(lock);
        assert!(!p.exists(), "the file goes away with the lock");
        assert_eq!(running_pid(&p), None);
        // and it can be taken again
        drop(PidLock::acquire(&p).unwrap());
    }

    #[test]
    fn m03_a_stale_file_without_a_lock_is_not_a_running_service() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.pid");
        fs::write(&p, "999999\n").unwrap();
        assert_eq!(read_pid(&p), Some(999_999));
        assert_eq!(running_pid(&p), None, "the pid in the file means nothing");
        remove_if_stale(&p);
        assert!(!p.exists());
        // taking it over works too
        fs::write(&p, "999999\n").unwrap();
        let _l = PidLock::acquire(&p).unwrap();
        assert_eq!(read_pid(&p), Some(std::process::id()));
        // a held lock is not removed as stale
        remove_if_stale(&p);
        assert!(p.exists());
    }

    #[test]
    fn m01_a_garbage_pid_file_reads_as_none() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("x.pid");
        fs::write(&p, "not a pid").unwrap();
        assert_eq!(read_pid(&p), None);
        assert_eq!(read_pid(&d.path().join("missing")), None);
    }
}
