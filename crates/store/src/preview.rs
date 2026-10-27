//! PNG previews of received faxes, kept next to the fax's TIFF and PDF (ADR-057).
//!
//! `sip-faxd` writes them when the call ends, together with the TIFF and the PDF
//! ([`render_pages`], [`png_rel_path`]):
//!
//! `faxes/<YYYY>/<MM>/<DD>/<name>.tif`, `<name>.pdf`, `<name>-p<page>.png` (the whole page) and
//! `<name>-p<page>-thumb.png` (300 px wide), pages numbered from 1.
//!
//! `Preview::page_png(id, page, kind)` returns that file. When it is not there (a fax received
//! by a version before ADR-057, or a call whose PNGs could not be written) the page is converted
//! from the TIFF once and written to the same place:
//!
//! * the PNG is written to `<name>.part` and renamed, so a `.png` is always whole;
//! * concurrent requests for the same page share one conversion (single flight per file);
//! * the conversion runs on the blocking pool (`spawn_blocking`);
//! * a fax whose files were purged (`files_purged_at`), a missing or damaged TIFF and a page
//!   number outside the file are reported as distinct [`PreviewError`]s. Nothing panics on bad
//!   input; a panic inside the conversion is turned into [`PreviewError::Task`].
//!
//! [`crate::Purger`] deletes the PNGs together with the TIFF and the PDF. A purge that runs
//! while a page is being converted cannot leave a PNG behind: after the conversion the record is
//! looked at again, and when its files were purged meanwhile the new PNG is deleted and the
//! request fails with [`PreviewError::Purged`] (the purger also sweeps the PNGs once more after
//! it has marked the record, so both orders are covered).

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
    /// Name used in messages and the API (`full`, `thumb`).
    pub fn as_str(self) -> &'static str {
        match self {
            PngKind::Full => "full",
            PngKind::Thumb => "thumb",
        }
    }
}

/// The PNG of page `page` (1-based) of the fax whose TIFF is `tiff_rel` (as stored, relative to
/// `data_dir`): the TIFF's name without its extension, then `-p<page>.png` or
/// `-p<page>-thumb.png`.
pub fn png_rel_path(tiff_rel: &str, page: u32, kind: PngKind) -> String {
    let stem = tiff_rel
        .strip_suffix(".tif")
        .or_else(|| tiff_rel.strip_suffix(".tiff"))
        .unwrap_or(tiff_rel);
    match kind {
        PngKind::Full => format!("{stem}-p{page}.png"),
        PngKind::Thumb => format!("{stem}-p{page}-thumb.png"),
    }
}

/// Whether `file` (a bare file name) is one of the PNGs [`png_rel_path`] names for a TIFF
/// called `<stem>.tif`.
pub fn is_png_of(stem: &str, file: &str) -> bool {
    let Some(rest) = file.strip_prefix(stem).and_then(|r| r.strip_prefix("-p")) else {
        return false;
    };
    let digits = rest.bytes().take_while(u8::is_ascii_digit).count();
    digits > 0 && matches!(&rest[digits..], ".png" | "-thumb.png")
}

/// The PNG files of one page: the whole page and the thumbnail.
pub type PagePngs = (Vec<u8>, Vec<u8>);

/// The full page and the thumbnail of every page of a TIFF, in page order.
///
/// # Errors
/// The TIFF is malformed or a page cannot be decoded or encoded.
pub fn render_pages(tiff: &[u8]) -> Result<Vec<PagePngs>, t4::Error> {
    let reader = t4::TiffReader::new(tiff)?;
    (0..reader.page_count())
        .map(|i| {
            let bm = reader.decode_page(i)?;
            Ok((t4::png::encode_full(&bm)?, t4::png::encode_thumb(&bm)?))
        })
        .collect()
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
    /// Reading the TIFF or writing the PNG failed.
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
        }
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

    /// Number of conversions started so far (a PNG that was already there does not count).
    pub fn conversions(&self) -> u64 {
        self.conversions.load(Ordering::SeqCst)
    }

    /// The PNG of page `page` (1-based) of fax `id`: the file written when the fax was
    /// received, or converted from the TIFF now when it is not there.
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
        let out = resolve_in_data_dir(&self.data_dir, &png_rel_path(&tiff, page, kind))
            .map_err(PreviewError::UnsafePath)?;
        if tokio::fs::try_exists(&out).await? {
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
            // a purge that deleted the TIFF under a running conversion shows up as an I/O or a
            // missing-TIFF error: that is a purged fax (410), not a failure
            self.ensure_not_purged(id, &out).await?;
            return Err(e);
        }
        // the files may have been purged while the page was converted
        self.ensure_not_purged(id, &out).await?;
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
        Err(err)
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

#[cfg(test)]
mod tests;
