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
mod tests {
    use super::*;
    use chrono::{Duration, NaiveDate};

    struct FakeClock(Mutex<NaiveDateTime>);

    impl FakeClock {
        fn new(y: i32, m: u32, d: u32, h: u32) -> Arc<FakeClock> {
            Arc::new(FakeClock(Mutex::new(
                NaiveDate::from_ymd_opt(y, m, d)
                    .unwrap()
                    .and_hms_opt(h, 0, 0)
                    .unwrap(),
            )))
        }
        fn advance(&self, d: Duration) {
            *self.0.lock().unwrap() += d;
        }
    }

    impl Clock for FakeClock {
        fn now(&self) -> NaiveDateTime {
            *self.0.lock().unwrap()
        }
    }

    fn cfg(dir: &Path, max: u64, daily: bool, keep: usize, compress: bool) -> RotateConfig {
        RotateConfig {
            path: dir.join("logs").join("sip-faxd.log"),
            max_bytes: max,
            daily,
            keep,
            compress,
        }
    }

    fn gunzip(p: &Path) -> String {
        let mut s = String::new();
        flate2::read::GzDecoder::new(File::open(p).unwrap())
            .read_to_string(&mut s)
            .unwrap();
        s
    }

    /// Every line of every archive (oldest first) and of the live file.
    fn all_lines(live: &Path) -> Vec<String> {
        let mut text = String::new();
        let mut arch = list_archives(live);
        arch.reverse();
        for a in arch {
            if a.gz {
                text += &gunzip(&a.path);
            } else {
                text += &fs::read_to_string(&a.path).unwrap();
            }
        }
        text += &fs::read_to_string(live).unwrap();
        text.lines().map(str::to_string).collect()
    }

    fn line(i: usize) -> String {
        format!("line {i:05} {}\n", "x".repeat(40))
    }

    #[test]
    fn m11_rotates_by_size_compresses_and_loses_no_line() {
        let d = tempfile::tempdir().unwrap();
        let clock = FakeClock::new(2026, 10, 21, 8);
        let c = cfg(d.path(), 1000, false, 100, true);
        let live = c.path.clone();
        let mut f = RotatingFile::open(c, clock.clone()).unwrap();
        for i in 0..100 {
            f.write_all(line(i).as_bytes()).unwrap();
            clock.advance(Duration::seconds(1));
        }
        f.finish();
        let arch = f.archives();
        // 46 bytes a line, 21 lines fill 966 bytes: about five lines short of 100 lines / file
        assert!(arch.len() >= 4, "{arch:?}");
        assert!(arch.iter().all(|a| a.gz), "all compressed: {arch:?}");
        assert!(
            !fs::read_dir(live.parent().unwrap()).unwrap().any(|e| e
                .unwrap()
                .file_name()
                .to_string_lossy()
                .ends_with(".tmp")),
            "no temporary file is left"
        );
        for a in &arch {
            assert!(
                gunzip(&a.path).len() <= 1000,
                "{:?} within the limit",
                a.path
            );
            // and a program that is not ours agrees that they are valid gzip files
            #[cfg(unix)]
            {
                let st = std::process::Command::new("gzip")
                    .arg("-t")
                    .arg(&a.path)
                    .status()
                    .expect("gzip is installed");
                assert!(st.success(), "gzip -t {:?}", a.path);
            }
        }
        assert!(fs::metadata(&live).unwrap().len() <= 1000);
        let got = all_lines(&live);
        let want: Vec<String> = (0..100).map(|i| line(i).trim_end().to_string()).collect();
        assert_eq!(got, want, "every line, once, in order");
    }

    #[test]
    fn m11_rotates_when_the_date_changes_and_not_before() {
        let d = tempfile::tempdir().unwrap();
        let clock = FakeClock::new(2026, 10, 21, 8);
        let c = cfg(d.path(), 1 << 30, true, 10, true);
        let live = c.path.clone();
        let mut f = RotatingFile::open(c, clock.clone()).unwrap();
        f.write_all(b"monday morning\n").unwrap();
        clock.advance(Duration::hours(15)); // 23:00, same day
        f.write_all(b"monday night\n").unwrap();
        f.finish();
        assert!(f.archives().is_empty(), "same date: no rotation");
        clock.advance(Duration::hours(2)); // 01:00 the next day
        f.write_all(b"tuesday\n").unwrap();
        f.finish();
        let arch = f.archives();
        assert_eq!(arch.len(), 1);
        assert_eq!(arch[0].stamp, "20261022-010000", "named after the rotation");
        assert_eq!(gunzip(&arch[0].path), "monday morning\nmonday night\n");
        assert_eq!(fs::read_to_string(&live).unwrap(), "tuesday\n");
        // the same day again: nothing more
        clock.advance(Duration::hours(3));
        f.write_all(b"tuesday again\n").unwrap();
        assert_eq!(f.archives().len(), 1);
    }

    #[test]
    fn m11_a_file_left_from_an_earlier_day_is_rotated_at_the_first_write() {
        let d = tempfile::tempdir().unwrap();
        let c = cfg(d.path(), 1 << 30, true, 10, false);
        fs::create_dir_all(c.path.parent().unwrap()).unwrap();
        fs::write(&c.path, "from the last run\n").unwrap();
        let two_days_ago = SystemTime::now() - std::time::Duration::from_secs(2 * 86_400);
        File::options()
            .write(true)
            .open(&c.path)
            .unwrap()
            .set_modified(two_days_ago)
            .unwrap();
        let live = c.path.clone();
        let mut f = RotatingFile::open(c, Arc::new(SystemClock)).unwrap();
        f.write_all(b"new run\n").unwrap();
        let arch = f.archives();
        assert_eq!(arch.len(), 1, "{arch:?}");
        assert_eq!(
            fs::read_to_string(&arch[0].path).unwrap(),
            "from the last run\n"
        );
        assert_eq!(fs::read_to_string(&live).unwrap(), "new run\n");
    }

    #[test]
    fn m11_only_the_newest_keep_files_are_kept() {
        let d = tempfile::tempdir().unwrap();
        let clock = FakeClock::new(2026, 10, 1, 8);
        let c = cfg(d.path(), 1 << 30, true, 3, true);
        let live = c.path.clone();
        let mut f = RotatingFile::open(c, clock.clone()).unwrap();
        for day in 1..=8 {
            f.write_all(format!("day {day}\n").as_bytes()).unwrap();
            clock.advance(Duration::days(1));
        }
        f.finish();
        let arch = f.archives();
        assert_eq!(arch.len(), 3, "{arch:?}");
        // days 5, 6, 7 were rotated last; day 8 is in the live file
        let texts: Vec<String> = arch.iter().map(|a| gunzip(&a.path)).collect();
        assert_eq!(texts, ["day 7\n", "day 6\n", "day 5\n"]);
        assert_eq!(fs::read_to_string(&live).unwrap(), "day 8\n");
        let names = fs::read_dir(live.parent().unwrap()).unwrap().count();
        assert_eq!(names, 4, "the live file and three archives, nothing else");
    }

    #[test]
    fn m11_without_compression_the_archives_stay_plain_and_two_rotations_in_a_second_differ() {
        let d = tempfile::tempdir().unwrap();
        let clock = FakeClock::new(2026, 10, 21, 8);
        let c = cfg(d.path(), 100, false, 10, false);
        let live = c.path.clone();
        let mut f = RotatingFile::open(c, clock).unwrap(); // the clock never moves
        for i in 0..6 {
            f.write_all(line(i).as_bytes()).unwrap();
        }
        let arch = f.archives();
        assert!(arch.len() >= 2, "{arch:?}");
        assert!(arch.iter().all(|a| !a.gz));
        let names: std::collections::BTreeSet<_> = arch.iter().map(|a| a.path.clone()).collect();
        assert_eq!(names.len(), arch.len(), "distinct names within one second");
        assert!(arch.iter().any(|a| a.n > 0));
        let want: Vec<String> = (0..6).map(|i| line(i).trim_end().to_string()).collect();
        assert_eq!(all_lines(&live), want, "order kept across the -N names");
    }

    #[test]
    fn m11_uncompressed_leftovers_of_a_crashed_run_are_compressed_at_the_next_start() {
        let d = tempfile::tempdir().unwrap();
        let c = cfg(d.path(), 1 << 30, false, 10, true);
        fs::create_dir_all(c.path.parent().unwrap()).unwrap();
        let old = PathBuf::from(format!("{}.20260101-000000", c.path.display()));
        fs::write(&old, "left behind\n").unwrap();
        let mut f = RotatingFile::open(c, FakeClock::new(2026, 10, 21, 8)).unwrap();
        f.finish();
        let arch = f.archives();
        assert_eq!(arch.len(), 1);
        assert!(arch[0].gz && !old.exists());
        assert_eq!(gunzip(&arch[0].path), "left behind\n");
    }

    #[test]
    fn m11_files_that_only_look_like_archives_are_left_alone() {
        let d = tempfile::tempdir().unwrap();
        let c = cfg(d.path(), 100, false, 1, true);
        let dir = c.path.parent().unwrap().to_path_buf();
        fs::create_dir_all(&dir).unwrap();
        for n in [
            "sip-faxd.log.bak",
            "sip-faxd.log.2026-10-21",
            "sip-faxd.log.20261021-12345",
            "other.log.20260101-000000",
        ] {
            fs::write(dir.join(n), "keep me").unwrap();
        }
        let mut f = RotatingFile::open(c, FakeClock::new(2026, 10, 21, 8)).unwrap();
        for i in 0..20 {
            f.write_all(line(i).as_bytes()).unwrap();
        }
        f.finish();
        for n in [
            "sip-faxd.log.bak",
            "sip-faxd.log.2026-10-21",
            "sip-faxd.log.20261021-12345",
            "other.log.20260101-000000",
        ] {
            assert_eq!(fs::read_to_string(dir.join(n)).unwrap(), "keep me", "{n}");
        }
        assert_eq!(f.archives().len(), 1, "keep = 1");
    }

    #[test]
    fn m11_many_writers_through_the_tracing_sink_lose_and_split_no_line() {
        let d = tempfile::tempdir().unwrap();
        let c = cfg(d.path(), 4000, false, 1000, true);
        let live = c.path.clone();
        let sink = LogSink::new(
            Some(RotatingFile::open(c, Arc::new(SystemClock)).unwrap()),
            false,
        );
        let subscriber = tracing_subscriber::fmt()
            .with_writer(sink.clone())
            .with_ansi(false)
            .finish();
        let dispatch = tracing::Dispatch::new(subscriber);
        std::thread::scope(|s| {
            for t in 0..4 {
                let dispatch = dispatch.clone();
                s.spawn(move || {
                    tracing::dispatcher::with_default(&dispatch, || {
                        for i in 0..250 {
                            tracing::info!("writer {t} number {i:04} {}", "y".repeat(30));
                        }
                    });
                });
            }
        });
        sink.finish();
        let lines = all_lines(&live);
        assert_eq!(lines.len(), 1000);
        let mut seen = std::collections::BTreeSet::new();
        for l in &lines {
            let (_, rest) = l
                .split_once(" INFO ")
                .unwrap_or_else(|| panic!("whole line: {l}"));
            let msg = rest.rsplit(": ").next().unwrap();
            assert!(
                msg.starts_with("writer ") && msg.ends_with(&"y".repeat(30)),
                "{l}"
            );
            assert!(seen.insert(msg.to_string()), "duplicate: {l}");
        }
        assert!(list_archives(&live).len() >= 10, "it really rotated");
    }

    // ---------------------------------------------------------------- tail / follow

    #[test]
    fn m12_tail_returns_the_last_lines_and_where_the_file_ends() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.log");
        let text: String = (1..=10).map(|i| format!("l{i}\n")).collect();
        fs::write(&p, &text).unwrap();
        let (b, len) = tail_lines(&p, 3).unwrap();
        assert_eq!(String::from_utf8(b).unwrap(), "l8\nl9\nl10\n");
        assert_eq!(len, text.len() as u64);
        assert_eq!(
            tail_lines(&p, 50).unwrap().0,
            text.as_bytes(),
            "fewer lines than asked"
        );
        assert!(tail_lines(&p, 0).unwrap().0.is_empty());
        // no trailing newline, and a file bigger than one read chunk
        let big: String = (0..3000)
            .map(|i| format!("row {i:05}\n"))
            .collect::<String>()
            + "last";
        fs::write(&p, &big).unwrap();
        let (b, _) = tail_lines(&p, 2).unwrap();
        assert_eq!(String::from_utf8(b).unwrap(), "row 02999\nlast");
        let (b, _) = tail_lines(&p, 1500).unwrap();
        let s = String::from_utf8(b).unwrap();
        assert_eq!(s.lines().count(), 1500);
        assert!(s.starts_with("row 01501\n"));
        fs::write(&p, "").unwrap();
        assert_eq!(tail_lines(&p, 5).unwrap(), (Vec::new(), 0));
        assert!(tail_lines(&d.path().join("missing"), 5).is_err());
    }

    fn poll_str(f: &mut Follower) -> String {
        let mut v = Vec::new();
        f.poll(&mut v).unwrap();
        String::from_utf8(v).unwrap()
    }

    #[test]
    fn m12_the_follower_reads_new_lines_and_nothing_twice() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.log");
        fs::write(&p, "old\n").unwrap();
        let (_, len) = tail_lines(&p, 10).unwrap();
        let mut f = Follower::open_at(&p, len);
        assert_eq!(poll_str(&mut f), "");
        let mut w = OpenOptions::new().append(true).open(&p).unwrap();
        w.write_all(b"one\ntwo\n").unwrap();
        assert_eq!(poll_str(&mut f), "one\ntwo\n");
        assert_eq!(poll_str(&mut f), "");
        w.write_all(b"three\n").unwrap();
        assert_eq!(poll_str(&mut f), "three\n");
        // truncated in place: from the start again
        fs::write(&p, "fresh\n").unwrap();
        assert_eq!(poll_str(&mut f), "fresh\n");
    }

    #[test]
    fn m12_the_follower_waits_for_a_file_that_does_not_exist_yet() {
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("later.log");
        let mut f = Follower::open_at(&p, 0);
        assert_eq!(poll_str(&mut f), "");
        fs::write(&p, "hello\n").unwrap();
        assert_eq!(poll_str(&mut f), "hello\n");
    }

    #[cfg(unix)]
    #[test]
    fn m12_the_follower_continues_in_the_new_file_after_a_rotation_without_losing_a_line() {
        let d = tempfile::tempdir().unwrap();
        let clock = FakeClock::new(2026, 10, 21, 8);
        let c = cfg(d.path(), 300, false, 10, true);
        let live = c.path.clone();
        let mut w = RotatingFile::open(c, clock.clone()).unwrap();
        w.write_all(b"before 1\n").unwrap();
        let (_, len) = tail_lines(&live, 10).unwrap();
        let mut f = Follower::open_at(&live, len);
        let mut got = String::new();
        // several rotations, at most one between two polls (every 4 lines; a file holds 5)
        for i in 0..60 {
            w.write_all(line(i).as_bytes()).unwrap();
            clock.advance(Duration::seconds(1));
            if i % 4 == 0 {
                got += &poll_str(&mut f);
            }
        }
        got += &poll_str(&mut f);
        got += &poll_str(&mut f);
        w.finish();
        assert!(list_archives(&live).len() >= 5, "it rotated several times");
        let want: String = (0..60).map(line).collect();
        assert_eq!(got, want, "every line once, in order, across the rotations");
    }

    #[cfg(unix)]
    #[test]
    fn m12_a_follower_polling_while_another_thread_writes_and_rotates_sees_every_line_once() {
        let d = tempfile::tempdir().unwrap();
        let c = cfg(d.path(), 1500, false, 10_000, true);
        let live = c.path.clone();
        let mut w = RotatingFile::open(c, Arc::new(SystemClock)).unwrap();
        w.write_all(b"start\n").unwrap();
        let mut f = Follower::open_at(&live, 6);
        use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
        let done = Arc::new(AtomicBool::new(false));
        let polls = Arc::new(AtomicUsize::new(0));
        let (d2, p2) = (done.clone(), polls.clone());
        // the writer lets the follower look once per 5 lines (a file holds about 29), however
        // slowly the follower thread gets scheduled
        let writer = std::thread::spawn(move || {
            for i in 0..3000 {
                w.write_all(line(i).as_bytes()).unwrap();
                if i % 5 == 4 {
                    let seen = p2.load(SeqCst);
                    while p2.load(SeqCst) == seen {
                        std::thread::yield_now();
                    }
                }
            }
            w.finish();
            d2.store(true, SeqCst);
        });
        let mut got = String::new();
        while !done.load(SeqCst) {
            got += &poll_str(&mut f);
            polls.fetch_add(1, SeqCst);
        }
        got += &poll_str(&mut f);
        writer.join().unwrap();
        let want: String = (0..3000).map(line).collect();
        let lines: Vec<&str> = got.lines().collect();
        let wl: Vec<&str> = want.lines().collect();
        assert_eq!(lines.len(), wl.len(), "count");
        assert_eq!(lines, wl);
    }

    #[cfg(unix)]
    #[test]
    fn m12_a_line_written_just_before_the_rename_is_still_delivered() {
        // the follower has read everything, then the writer adds a last line to the old file
        // and rotates: the follower must see that line before it moves on
        let d = tempfile::tempdir().unwrap();
        let p = d.path().join("a.log");
        fs::write(&p, "a\n").unwrap();
        let mut f = Follower::open_at(&p, 0);
        assert_eq!(poll_str(&mut f), "a\n");
        {
            let mut w = OpenOptions::new().append(true).open(&p).unwrap();
            w.write_all(b"last of the old file\n").unwrap();
        }
        fs::rename(&p, d.path().join("a.log.1")).unwrap();
        fs::write(&p, "first of the new file\n").unwrap();
        assert_eq!(
            poll_str(&mut f),
            "last of the old file\nfirst of the new file\n"
        );
    }
}
