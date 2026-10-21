//! PNG preview of received faxes, created on first request and cached (spec section 6).
//!
//! `Preview::page_png(id, page, kind)` returns the path of
//! `data_dir/cache/png/<fax id>/<page>-<full|thumb>.png`, converting the page from the fax's TIFF
//! the first time it is asked for:
//!
//! * pages are numbered from 1, as a user sees them;
//! * the PNG is written to `<name>.part` and renamed, so a `.png` in the cache is always whole;
//! * concurrent requests for the same page share one conversion (single flight per cache file);
//! * the conversion runs on the blocking pool (`spawn_blocking`);
//! * a fax whose files were purged (`files_purged_at`), a missing or damaged TIFF and a page
//!   number outside the file are reported as distinct [`PreviewError`]s. Nothing panics on bad
//!   input; a panic inside the conversion is turned into [`PreviewError::Task`].
//!
//! [`crate::Purger`] deletes the whole `cache/png/<id>` directory together with the TIFF/PDF.
//!
//! M4a (R01, R02):
//!
//! * a purge that runs while a page is being converted cannot leave a PNG behind: after the
//!   conversion the record is looked at again, and when its files were purged meanwhile the new
//!   PNG is deleted and the request fails with [`PreviewError::Purged`] (the purger also sweeps
//!   the cache directory once more after it has marked the record, so both orders are covered);
//! * the cache has a size limit ([`Preview::with_cache_limit`]): after a new PNG is stored the
//!   least recently used files are deleted until the cache fits again. A file counts as used
//!   when it is stored or served (the modification time is refreshed on every hit); files used
//!   within the protection window, and pages that are being converted, are never deleted.

use crate::clock::{Clock, SystemClock};
use crate::purge::resolve_in_data_dir;
use crate::store::FaxStore;
use chrono::{DateTime, Utc};
use std::collections::HashMap;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

/// Which image to produce.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PngKind {
    /// The whole page, 1-bit grayscale (rows doubled for standard resolution).
    Full,
    /// 300 px wide 8-bit grayscale thumbnail.
    Thumb,
}

impl PngKind {
    /// Name used in the cache file name.
    pub fn as_str(self) -> &'static str {
        match self {
            PngKind::Full => "full",
            PngKind::Thumb => "thumb",
        }
    }
}

/// Why a preview could not be produced.
#[derive(Debug, thiserror::Error)]
pub enum PreviewError {
    /// No such fax record.
    #[error("fax {0} not found")]
    NotFound(i64),
    /// The files were removed by the retention job.
    #[error("files of fax {id} were purged at {at}")]
    Purged {
        /// Fax id.
        id: i64,
        /// When the files were purged.
        at: DateTime<Utc>,
    },
    /// The record has no TIFF (for example a call without fax signal).
    #[error("fax {0} has no TIFF file")]
    NoFile(i64),
    /// The page number is 0 or beyond the last page of the TIFF.
    #[error("page {page} is out of range (the fax has {pages} pages)")]
    PageOutOfRange {
        /// Requested page (1-based).
        page: u32,
        /// Pages the fax has.
        pages: u32,
    },
    /// The TIFF named by the record does not exist on disk.
    #[error("TIFF file {0} does not exist")]
    TiffMissing(PathBuf),
    /// The stored TIFF path is not inside `data_dir`.
    #[error("refusing to read TIFF: {0}")]
    UnsafePath(String),
    /// The TIFF is malformed or its page cannot be decoded.
    #[error("TIFF is damaged: {0}")]
    BadTiff(String),
    /// Reading the TIFF or writing the cache failed.
    #[error("preview I/O error: {0}")]
    Io(#[from] io::Error),
    /// The conversion task panicked or was cancelled.
    #[error("preview conversion failed: {0}")]
    Task(String),
    /// Database error while looking the record up.
    #[error(transparent)]
    Store(#[from] crate::Error),
}

/// Called with the `.part` file after it is written and before it is renamed (tests).
type FaultFn = dyn Fn(&Path) -> io::Result<()> + Send + Sync;
type FaultHook = Arc<FaultFn>;

/// PNG preview service.
pub struct Preview {
    store: Arc<dyn FaxStore>,
    data_dir: PathBuf,
    flights: Mutex<HashMap<PathBuf, Arc<tokio::sync::Mutex<()>>>>,
    conversions: AtomicU64,
    fault: Option<FaultHook>,
    clock: Arc<dyn Clock>,
    limit: Option<CacheLimit>,
}

/// Size limit of the PNG cache.
#[derive(Debug, Clone, Copy)]
struct CacheLimit {
    bytes: u64,
    protect: std::time::Duration,
}

/// What [`Preview::trim_cache`] did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TrimReport {
    /// Files deleted.
    pub removed_files: u64,
    /// Bytes freed.
    pub removed_bytes: u64,
    /// Bytes in the cache afterwards.
    pub kept_bytes: u64,
}

impl Preview {
    /// Create the service for the records in `store` and the files under `data_dir`.
    pub fn new(store: Arc<dyn FaxStore>, data_dir: impl Into<PathBuf>) -> Self {
        Self {
            store,
            data_dir: data_dir.into(),
            flights: Mutex::new(HashMap::new()),
            conversions: AtomicU64::new(0),
            fault: None,
            clock: Arc::new(SystemClock),
            limit: None,
        }
    }

    /// The clock that decides what "used recently" means (default: the system clock).
    pub fn with_clock(mut self, clock: Arc<dyn Clock>) -> Self {
        self.clock = clock;
        self
    }

    /// Limits the PNG cache to `bytes`; files used within `protect` are never deleted (so a page
    /// that was just served is still there when the client fetches it), which means the limit is
    /// a soft one while many pages are in use.
    pub fn with_cache_limit(mut self, bytes: u64, protect: std::time::Duration) -> Self {
        self.limit = Some(CacheLimit { bytes, protect });
        self
    }

    /// [`Preview::with_cache_limit`] in megabytes (`[storage] preview_cache_mb`) with a
    /// 60 second protection window.
    pub fn with_cache_limit_mb(self, mb: u64) -> Self {
        self.with_cache_limit(
            mb.saturating_mul(1024 * 1024),
            std::time::Duration::from_secs(60),
        )
    }

    /// Test seam: run `hook` on the finished `.part` file just before it is renamed into place.
    /// An error (or a panic) there simulates a conversion that dies half way.
    #[doc(hidden)]
    pub fn with_fault_hook(
        mut self,
        hook: impl Fn(&Path) -> io::Result<()> + Send + Sync + 'static,
    ) -> Self {
        self.fault = Some(Arc::new(hook));
        self
    }

    /// Number of conversions started so far (a cached page does not count).
    pub fn conversions(&self) -> u64 {
        self.conversions.load(Ordering::SeqCst)
    }

    /// Cache file of a page (it may not exist yet).
    pub fn cache_path(&self, id: i64, page: u32, kind: PngKind) -> PathBuf {
        self.data_dir
            .join("cache")
            .join("png")
            .join(id.to_string())
            .join(format!("{page}-{}.png", kind.as_str()))
    }

    /// The PNG of page `page` (1-based) of fax `id`, converted on first use.
    pub async fn page_png(
        &self,
        id: i64,
        page: u32,
        kind: PngKind,
    ) -> Result<PathBuf, PreviewError> {
        let rec = self
            .store
            .get(id)
            .await?
            .ok_or(PreviewError::NotFound(id))?;
        if let Some(at) = rec.files_purged_at {
            return Err(PreviewError::Purged { id, at });
        }
        let tiff = rec.tiff_path.ok_or(PreviewError::NoFile(id))?;
        // The record knows the page count: refuse bogus page numbers without touching the disk.
        let recorded = rec.pages.max(0) as u32;
        if page == 0 || (recorded > 0 && page > recorded) {
            return Err(PreviewError::PageOutOfRange {
                page,
                pages: recorded,
            });
        }
        let out = self.cache_path(id, page, kind);
        if tokio::fs::try_exists(&out).await? {
            self.touch(&out);
            return Ok(out);
        }

        let flight = self.flight(&out);
        let result = async {
            let _turn = flight.lock().await;
            // Somebody else may have produced it while we waited.
            if tokio::fs::try_exists(&out).await? {
                return Ok(());
            }
            self.conversions.fetch_add(1, Ordering::SeqCst);
            let (data_dir, out2, fault) = (self.data_dir.clone(), out.clone(), self.fault.clone());
            tokio::task::spawn_blocking(move || {
                convert(&data_dir, &tiff, page, kind, &out2, fault.as_deref())
            })
            .await
            .map_err(|e| PreviewError::Task(e.to_string()))?
        }
        .await;
        self.release(&out, flight);
        if let Err(e) = result {
            // a purge that deleted the directory under a running conversion shows up as an I/O
            // error: that is a purged fax (410), not a failure
            self.ensure_not_purged(id, &out).await?;
            return Err(e);
        }
        // R01: the files may have been purged while the page was converted
        self.ensure_not_purged(id, &out).await?;
        if self.limit.is_some()
            && let Err(e) = self.trim_cache().await
        {
            tracing::warn!("preview cache could not be trimmed: {e}");
        }
        Ok(out)
    }

    /// A page that was just converted must not outlive the files of its fax.
    async fn ensure_not_purged(&self, id: i64, out: &Path) -> Result<(), PreviewError> {
        let err = match self.store.get(id).await? {
            Some(r) if r.files_purged_at.is_none() => return Ok(()),
            Some(r) => PreviewError::Purged {
                id,
                at: r.files_purged_at.expect("checked above"),
            },
            None => PreviewError::NotFound(id),
        };
        let _ = tokio::fs::remove_file(out).await;
        if let Some(dir) = out.parent() {
            let _ = tokio::fs::remove_dir(dir).await; // only when empty
        }
        Err(err)
    }

    /// Marks a cached page as used now.
    fn touch(&self, path: &Path) {
        let now: std::time::SystemTime = self.clock.now().into();
        if let Ok(f) = std::fs::OpenOptions::new().write(true).open(path) {
            let _ = f.set_modified(now);
        }
    }

    /// Deletes the least recently used PNGs until the cache fits its limit. Files used within
    /// the protection window and pages that are being converted stay. Does nothing without a
    /// limit.
    ///
    /// # Errors
    /// The cache directory cannot be read.
    pub async fn trim_cache(&self) -> io::Result<TrimReport> {
        let Some(limit) = self.limit else {
            return Ok(TrimReport::default());
        };
        let busy: Vec<PathBuf> = self
            .flights
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect();
        let root = self.data_dir.join("cache").join("png");
        let now = self.clock.now();
        tokio::task::spawn_blocking(move || trim(&root, limit, now, &busy))
            .await
            .map_err(|e| io::Error::other(e.to_string()))?
    }

    /// Pages currently being converted or waited for (0 when idle).
    #[cfg(test)]
    pub(crate) fn in_flight(&self) -> usize {
        self.flights.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    fn flight(&self, key: &Path) -> Arc<tokio::sync::Mutex<()>> {
        let mut m = self.flights.lock().unwrap_or_else(|e| e.into_inner());
        m.entry(key.to_path_buf()).or_default().clone()
    }

    /// Forget the lock of `key` when nobody else holds or waits for it.
    fn release(&self, key: &Path, mine: Arc<tokio::sync::Mutex<()>>) {
        let mut m = self.flights.lock().unwrap_or_else(|e| e.into_inner());
        // The map holds one reference and `mine` another; more means somebody is waiting.
        if Arc::strong_count(&mine) <= 2 {
            m.remove(key);
        }
    }
}

/// Removes the `.part` file on drop unless it was renamed, also when the code panics.
struct PartGuard<'a> {
    path: &'a Path,
    armed: bool,
}

impl Drop for PartGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            let _ = std::fs::remove_file(self.path);
        }
    }
}

fn bad_tiff(e: t4::Error) -> PreviewError {
    PreviewError::BadTiff(e.to_string())
}

fn convert(
    data_dir: &Path,
    tiff_rel: &str,
    page: u32,
    kind: PngKind,
    out: &Path,
    fault: Option<&FaultFn>,
) -> Result<(), PreviewError> {
    let tiff = resolve_in_data_dir(data_dir, tiff_rel).map_err(PreviewError::UnsafePath)?;
    let bytes = match std::fs::read(&tiff) {
        Ok(b) => b,
        Err(e) if e.kind() == io::ErrorKind::NotFound => {
            return Err(PreviewError::TiffMissing(tiff));
        }
        Err(e) => return Err(e.into()),
    };
    let reader = t4::TiffReader::new(&bytes).map_err(bad_tiff)?;
    let pages = reader.page_count() as u32;
    if page > pages {
        return Err(PreviewError::PageOutOfRange { page, pages });
    }
    let bitmap = reader.decode_page(page as usize - 1).map_err(bad_tiff)?;
    let png = match kind {
        PngKind::Full => t4::png::encode_full(&bitmap),
        PngKind::Thumb => t4::png::encode_thumb(&bitmap),
    }
    .map_err(bad_tiff)?;

    if let Some(dir) = out.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let mut part = out.as_os_str().to_owned();
    part.push(".part");
    let part = PathBuf::from(part);
    let mut guard = PartGuard {
        path: &part,
        armed: true,
    };
    {
        let mut f = std::fs::File::create(&part)?;
        f.write_all(&png)?;
        f.sync_all()?;
    }
    if let Some(hook) = fault {
        hook(&part)?;
    }
    std::fs::rename(&part, out)?;
    guard.armed = false;
    Ok(())
}

fn trim(
    root: &Path,
    limit: CacheLimit,
    now: DateTime<Utc>,
    busy: &[PathBuf],
) -> io::Result<TrimReport> {
    let mut files: Vec<(std::time::SystemTime, PathBuf, u64)> = Vec::new();
    let dirs = match std::fs::read_dir(root) {
        Ok(d) => d,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(TrimReport::default()),
        Err(e) => return Err(e),
    };
    for dir in dirs.flatten() {
        let Ok(inner) = std::fs::read_dir(dir.path()) else {
            continue;
        };
        for f in inner.flatten() {
            let p = f.path();
            if p.extension().is_none_or(|e| e != "png") {
                continue; // `.part` files belong to a conversion in progress
            }
            if let Ok(m) = f.metadata()
                && m.is_file()
            {
                files.push((m.modified().unwrap_or(std::time::UNIX_EPOCH), p, m.len()));
            }
        }
    }
    let mut total: u64 = files.iter().map(|f| f.2).sum();
    let mut rep = TrimReport::default();
    files.sort();
    let protect = chrono::Duration::from_std(limit.protect).unwrap_or(chrono::Duration::zero());
    for (mtime, path, size) in &files {
        if total <= limit.bytes {
            break;
        }
        let used: DateTime<Utc> = (*mtime).into();
        if now - used < protect || busy.contains(path) {
            continue;
        }
        if std::fs::remove_file(path).is_ok() {
            total -= size;
            rep.removed_files += 1;
            rep.removed_bytes += size;
            if let Some(dir) = path.parent() {
                let _ = std::fs::remove_dir(dir); // only when empty
            }
        }
    }
    rep.kept_bytes = total;
    Ok(rep)
}
