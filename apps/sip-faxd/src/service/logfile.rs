//! The log file: size / daily rotation with gzip, the writer `tracing` logs through, and the
//! tail / follow behind `sip-faxd logs`.
//!
//! Rotation moves `sip-faxd.log` to `sip-faxd.log.<YYYYmmdd-HHMMSS>` (the time of the rotation,
//! `-N` appended on a clash), opens a new file at once and compresses the old one to `.gz` on a
//! background thread. The check happens between two writes, and `tracing` hands over one whole
//! line per write, so no line is split or lost. The clock is injected ([`Clock`]).

use std::fs::{self, File, OpenOptions};
use std::io::{self, BufWriter, Read, Seek, SeekFrom, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, PoisonError};
use std::thread::JoinHandle;
use std::time::SystemTime;

use chrono::{DateTime, Local, NaiveDate, NaiveDateTime};
use flate2::Compression;
use flate2::write::GzEncoder;

/// Local wall-clock time, replaceable in tests.
pub trait Clock: Send + Sync {
    /// Now, local time.
    fn now(&self) -> NaiveDateTime;

    /// The local date of a file's modification time.
    fn day_of(&self, t: SystemTime) -> NaiveDate {
        DateTime::<Local>::from(t).date_naive()
    }
}

/// The real clock.
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> NaiveDateTime {
        Local::now().naive_local()
    }
}

/// What to rotate and when.
#[derive(Debug, Clone)]
pub struct RotateConfig {
    /// The live log file.
    pub path: PathBuf,
    /// Rotate before a write that would take the file beyond this size.
    pub max_bytes: u64,
    /// Rotate when the local date has changed since the file was last rotated (or, for a file
    /// found at start, since it was last written).
    pub daily: bool,
    /// Rotated files to keep.
    pub keep: usize,
    /// Compress rotated files.
    pub compress: bool,
}

/// An append-only log file that rotates itself.
pub struct RotatingFile {
    cfg: RotateConfig,
    clock: Arc<dyn Clock>,
    file: Option<File>,
    size: u64,
    day: NaiveDate,
    workers: Vec<JoinHandle<()>>,
}

impl RotatingFile {
    /// Opens (creating the directory and the file) and tidies up what an earlier run left:
    /// rotated files that were not compressed yet are compressed, surplus ones deleted.
    pub fn open(cfg: RotateConfig, clock: Arc<dyn Clock>) -> io::Result<RotatingFile> {
        if let Some(dir) = cfg.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&cfg.path)?;
        let meta = file.metadata()?;
        let today = clock.now().date();
        let day = match meta.modified() {
            Ok(t) if meta.len() > 0 => clock.day_of(t),
            _ => today,
        };
        let mut me = RotatingFile {
            size: meta.len(),
            cfg,
            clock,
            file: Some(file),
            day,
            workers: Vec::new(),
        };
        if me.cfg.compress {
            for a in me.archives() {
                if !a.gz {
                    me.compress_in_background(a.path);
                }
            }
        }
        me.prune();
        Ok(me)
    }

    /// The rotated files, newest first (compressed and not).
    pub fn archives(&self) -> Vec<Archive> {
        list_archives(&self.cfg.path)
    }

    /// Waits for the compression threads (tests; `drop` does it too).
    pub fn finish(&mut self) {
        for w in self.workers.drain(..) {
            let _ = w.join();
        }
        // a thread may have finished the compression of a file that was pruned meanwhile
        self.prune();
    }

    fn needs_rotation(&self, incoming: u64) -> bool {
        if self.size == 0 {
            // an empty file is not worth an archive; only the day marker moves
            return false;
        }
        (self.cfg.daily && self.clock.now().date() != self.day)
            || self.size + incoming > self.cfg.max_bytes
    }

    fn rotate(&mut self) -> io::Result<()> {
        let now = self.clock.now();
        let stamp = now.format("%Y%m%d-%H%M%S").to_string();
        let base = self.cfg.path.display().to_string();
        let mut name = PathBuf::from(format!("{base}.{stamp}"));
        let mut n = 0u32;
        while name.exists() || PathBuf::from(format!("{}.gz", name.display())).exists() {
            n += 1;
            name = PathBuf::from(format!("{base}.{stamp}-{n}"));
        }
        // close, move, reopen: whatever fails, the next write finds a file to append to
        drop(self.file.take());
        let moved = fs::rename(&self.cfg.path, &name);
        let reopened = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.cfg.path);
        self.file = Some(reopened?);
        if moved.is_ok() {
            self.size = 0;
        }
        moved?;
        self.day = now.date();
        if self.cfg.compress {
            self.compress_in_background(name);
        }
        self.prune();
        Ok(())
    }

    fn compress_in_background(&mut self, src: PathBuf) {
        self.workers.retain(|w| !w.is_finished());
        self.workers.push(std::thread::spawn(move || {
            // a failure leaves the plain file in place, which is still a valid archive
            let _ = gzip_file(&src);
        }));
    }

    fn prune(&mut self) {
        let mut seen: Vec<(String, u32)> = Vec::new();
        for a in list_archives(&self.cfg.path) {
            let key = (a.stamp.clone(), a.n);
            let idx = match seen.iter().position(|k| *k == key) {
                Some(i) => i,
                None => {
                    seen.push(key);
                    seen.len() - 1
                }
            };
            if idx >= self.cfg.keep {
                let _ = fs::remove_file(&a.path);
            }
        }
    }
}

impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if buf.is_empty() {
            return Ok(0);
        }
        if self.needs_rotation(buf.len() as u64) {
            // a failed rotation must not lose the line: it goes on into the current file
            let _ = self.rotate();
        } else if self.size == 0 {
            self.day = self.clock.now().date();
        }
        if self.file.is_none() {
            self.file = Some(
                OpenOptions::new()
                    .create(true)
                    .append(true)
                    .open(&self.cfg.path)?,
            );
        }
        let f = self.file.as_mut().expect("opened above");
        f.write_all(buf)?;
        self.size += buf.len() as u64;
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.file.as_mut() {
            Some(f) => f.flush(),
            None => Ok(()),
        }
    }
}

impl Drop for RotatingFile {
    fn drop(&mut self) {
        self.finish();
    }
}

/// One rotated file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Archive {
    /// Where it is.
    pub path: PathBuf,
    /// `YYYYmmdd-HHMMSS` of the rotation.
    pub stamp: String,
    /// The `-N` that told two rotations of the same second apart (0 without).
    pub n: u32,
    /// Compressed.
    pub gz: bool,
}

/// Splits `<stamp>[-N][.gz]`.
fn parse_suffix(s: &str) -> Option<(String, u32, bool)> {
    let (rest, gz) = match s.strip_suffix(".gz") {
        Some(r) => (r, true),
        None => (s, false),
    };
    let stamp = rest.get(..15)?;
    let (date, time) = stamp.split_once('-')?;
    if date.len() != 8
        || time.len() != 6
        || !(date.bytes().chain(time.bytes())).all(|b| b.is_ascii_digit())
    {
        return None;
    }
    let n = match &rest[15..] {
        "" => 0,
        t => t.strip_prefix('-')?.parse().ok()?,
    };
    Some((stamp.to_string(), n, gz))
}

/// The rotated files of `live`, newest first.
pub fn list_archives(live: &Path) -> Vec<Archive> {
    let (Some(dir), Some(name)) = (live.parent(), live.file_name().and_then(|n| n.to_str())) else {
        return Vec::new();
    };
    let prefix = format!("{name}.");
    let mut v: Vec<Archive> = fs::read_dir(dir)
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| {
            let fname = e.file_name().into_string().ok()?;
            let (stamp, n, gz) = parse_suffix(fname.strip_prefix(&prefix)?)?;
            Some(Archive {
                path: e.path(),
                stamp,
                n,
                gz,
            })
        })
        .collect();
    v.sort_by(|a, b| (&b.stamp, b.n, b.gz).cmp(&(&a.stamp, a.n, a.gz)));
    v
}

fn gzip_file(src: &Path) -> io::Result<()> {
    let dst = PathBuf::from(format!("{}.gz", src.display()));
    let tmp = PathBuf::from(format!("{}.gz.tmp", src.display()));
    {
        let mut input = File::open(src)?;
        let out = BufWriter::new(File::create(&tmp)?);
        let mut enc = GzEncoder::new(out, Compression::default());
        io::copy(&mut input, &mut enc)?;
        let mut out = enc.finish()?;
        out.flush()?;
        out.get_ref().sync_all()?;
    }
    fs::rename(&tmp, &dst)?;
    fs::remove_file(src)
}

// ------------------------------------------------------------------------ tracing writer

/// Where `tracing` writes: the rotating file and / or stderr.
#[derive(Clone)]
pub struct LogSink {
    file: Option<Arc<Mutex<RotatingFile>>>,
    stderr: bool,
}

impl LogSink {
    /// A sink over `file` (`None`: stderr only), echoing to stderr when `stderr` is set.
    pub fn new(file: Option<RotatingFile>, stderr: bool) -> LogSink {
        LogSink {
            file: file.map(|f| Arc::new(Mutex::new(f))),
            stderr: stderr || false,
        }
    }

    /// Waits for the compression threads and flushes.
    pub fn finish(&self) {
        if let Some(f) = &self.file {
            let mut g = f.lock().unwrap_or_else(PoisonError::into_inner);
            let _ = g.flush();
            g.finish();
        }
    }
}

/// One `tracing` event on its way out.
pub struct SinkWriter {
    sink: LogSink,
}

impl Write for SinkWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(f) = &self.sink.file {
            let mut g = f.lock().unwrap_or_else(PoisonError::into_inner);
            // a full disk must not take the service down; stderr still gets the line
            let _ = g.write(buf);
        }
        if self.sink.stderr {
            let _ = io::stderr().write_all(buf);
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for LogSink {
    type Writer = SinkWriter;

    fn make_writer(&'a self) -> SinkWriter {
        SinkWriter { sink: self.clone() }
    }
}

// ------------------------------------------------------------------------ tail / follow

/// The last `n` lines of `path` and the length of the file at that moment (where
/// [`Follower::open_at`] continues).
pub fn tail_lines(path: &Path, n: usize) -> io::Result<(Vec<u8>, u64)> {
    let mut f = File::open(path)?;
    let len = f.metadata()?.len();
    if n == 0 || len == 0 {
        return Ok((Vec::new(), len));
    }
    let mut pos = len;
    let mut data: Vec<u8> = Vec::new();
    // a trailing newline ends the last line instead of starting another
    let mut newlines = 0usize;
    while pos > 0 && newlines <= n {
        let step = pos.min(8192);
        pos -= step;
        f.seek(SeekFrom::Start(pos))?;
        let mut chunk = vec![0u8; step as usize];
        f.read_exact(&mut chunk)?;
        newlines += chunk.iter().filter(|&&b| b == b'\n').count();
        chunk.extend_from_slice(&data);
        data = chunk;
    }
    // data may hold more than n lines (and start in the middle of one)
    let body_end = if data.last() == Some(&b'\n') {
        data.len() - 1
    } else {
        data.len()
    };
    let mut start = 0usize;
    let mut seen = 0usize;
    for i in (0..body_end).rev() {
        if data[i] == b'\n' {
            seen += 1;
            if seen == n {
                start = i + 1;
                break;
            }
        }
    }
    // fewer than n lines in the whole file: everything (start stays 0)
    Ok((data[start..].to_vec(), len))
}

#[cfg(unix)]
fn file_id(m: &fs::Metadata) -> Option<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Some((m.dev(), m.ino()))
}

#[cfg(not(unix))]
fn file_id(_: &fs::Metadata) -> Option<(u64, u64)> {
    None
}

/// `tail -F`: follows `path` through rotations and truncations without dropping a byte, as long
/// as the file is not rotated twice between two polls (the CLI polls every 200 ms; a rotation
/// takes megabytes or a day).
pub struct Follower {
    path: PathBuf,
    file: Option<File>,
    id: Option<(u64, u64)>,
    pos: u64,
}

impl Follower {
    /// Starts reading `path` at byte `pos` (the file may not exist yet).
    pub fn open_at(path: &Path, pos: u64) -> Follower {
        let mut f = Follower {
            path: path.to_path_buf(),
            file: None,
            id: None,
            pos,
        };
        f.reopen(pos);
        f
    }

    fn reopen(&mut self, pos: u64) -> bool {
        match File::open(&self.path) {
            Ok(mut file) => {
                let meta = file.metadata().ok();
                let len = meta.as_ref().map_or(0, fs::Metadata::len);
                self.id = meta.as_ref().and_then(file_id);
                let pos = pos.min(len);
                let _ = file.seek(SeekFrom::Start(pos));
                self.pos = pos;
                self.file = Some(file);
                true
            }
            Err(_) => false,
        }
    }

    fn drain(&mut self, out: &mut Vec<u8>) -> io::Result<()> {
        let Some(f) = self.file.as_mut() else {
            return Ok(());
        };
        // a file that shrank was truncated: start again from its beginning
        let len = f.metadata()?.len();
        if len < self.pos {
            f.seek(SeekFrom::Start(0))?;
            self.pos = 0;
        }
        let n = f.read_to_end(out)?;
        self.pos += n as u64;
        Ok(())
    }

    /// Appends whatever was written since the last call to `out`.
    pub fn poll(&mut self, out: &mut Vec<u8>) -> io::Result<()> {
        if self.file.is_none() && !self.reopen(0) {
            return Ok(());
        }
        loop {
            self.drain(out)?;
            let Ok(meta) = fs::metadata(&self.path) else {
                // between the rename and the creation of the new file: keep what we have
                return Ok(());
            };
            if file_id(&meta) == self.id {
                return Ok(());
            }
            // rotated: nothing is written to the old file any more, so one more read gets its
            // last lines; then the new file from its start
            self.drain(out)?;
            if !self.reopen(0) {
                return Ok(());
            }
        }
    }
}

#[cfg(test)]
mod tests;
