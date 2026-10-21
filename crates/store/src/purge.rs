//! Retention: file purge, record purge and the retention settings (spec section 7).

use crate::admin::ConfigAudit;
use crate::clock::Clock;
use crate::error::{Error, Result};
use crate::model::{PurgeRow, RetentionPolicy};
use crate::store::FaxStore;
use chrono::Duration;
use std::io;
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;

/// Rows per batch (spec section 7).
pub const BATCH: u32 = 500;

/// Name of the retention section in `config_sections` (M4a: the M3 `settings` keys moved there).
pub const RETENTION_SECTION: &str = "retention";

/// Retention policy plus the daily run time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionSettings {
    /// Days to keep files / records.
    pub policy: RetentionPolicy,
    /// `HH:MM`, local time.
    pub run_at: String,
}

impl Default for RetentionSettings {
    fn default() -> Self {
        Self {
            policy: RetentionPolicy::default(),
            run_at: "03:17".into(),
        }
    }
}

impl RetentionSettings {
    /// Check policy consistency and the `HH:MM` format.
    pub fn validate(&self) -> Result<()> {
        self.policy.validate()?;
        let ok = self.run_at.split_once(':').is_some_and(|(h, m)| {
            h.len() == 2
                && m.len() == 2
                && h.parse::<u8>().is_ok_and(|h| h < 24)
                && m.parse::<u8>().is_ok_and(|m| m < 60)
        });
        if ok {
            Ok(())
        } else {
            Err(Error::Invalid(format!(
                "retention.run_at {:?} is not HH:MM",
                self.run_at
            )))
        }
    }
}

#[derive(serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct RetentionBody {
    #[serde(default = "default_files_days")]
    files_days: u32,
    #[serde(default = "default_records_days")]
    records_days: u32,
    #[serde(default = "default_run_at")]
    run_at: String,
}

fn default_files_days() -> u32 {
    RetentionPolicy::default().files_days
}
fn default_records_days() -> u32 {
    RetentionPolicy::default().records_days
}
fn default_run_at() -> String {
    RetentionSettings::default().run_at
}

impl RetentionSettings {
    /// The JSON body stored in the `retention` section.
    pub fn to_json(&self) -> String {
        serde_json::to_string(&RetentionBody {
            files_days: self.policy.files_days,
            records_days: self.policy.records_days,
            run_at: self.run_at.clone(),
        })
        .expect("plain struct serialises")
    }

    /// Parses the section body (missing keys take the defaults) and validates it.
    pub fn from_json(text: &str) -> Result<RetentionSettings> {
        let b: RetentionBody = serde_json::from_str(text)
            .map_err(|e| Error::Invalid(format!("retention section is not valid: {e}")))?;
        let s = RetentionSettings {
            policy: RetentionPolicy {
                files_days: b.files_days,
                records_days: b.records_days,
            },
            run_at: b.run_at,
        };
        s.validate()?;
        Ok(s)
    }
}

/// First start: write the configuration file values as the `retention` section when the database
/// has none; an existing section is left alone (the database wins from then on). Returns whether
/// the section was written.
pub async fn seed_retention(store: &dyn FaxStore, cfg: &RetentionSettings) -> Result<bool> {
    cfg.validate()?;
    if store.config_get(RETENTION_SECTION).await?.is_some() {
        return Ok(false);
    }
    let body = cfg.to_json();
    let audit = ConfigAudit {
        action: "create".into(),
        before: None,
        after: Some(body.clone()),
    };
    match store
        .config_put(RETENTION_SECTION, &body, None, "config-file", &audit)
        .await
    {
        Ok(_) => Ok(true),
        Err(Error::Conflict(_)) => Ok(false),
        Err(e) => Err(e),
    }
}

/// Effective retention settings from the database (defaults while the section does not exist).
pub async fn load_retention(store: &dyn FaxStore) -> Result<RetentionSettings> {
    match store.config_get(RETENTION_SECTION).await? {
        Some(row) => RetentionSettings::from_json(&row.body),
        None => Ok(RetentionSettings::default()),
    }
}

/// Validate and write the retention settings (a change of the `retention` section, with audit).
pub async fn save_retention(store: &dyn FaxStore, s: &RetentionSettings, by: &str) -> Result<()> {
    s.validate()?;
    let cur = store.config_get(RETENTION_SECTION).await?;
    let body = s.to_json();
    let audit = ConfigAudit {
        action: if cur.is_some() { "update" } else { "create" }.into(),
        before: cur.as_ref().map(|r| r.body.clone()),
        after: Some(body.clone()),
    };
    store
        .config_put(
            RETENTION_SECTION,
            &body,
            cur.map(|r| r.version),
            by,
            &audit,
        )
        .await
        .map(|_| ())
}

/// File deletion, abstracted so tests can simulate permission errors.
pub trait FileOps: Send + Sync {
    /// Delete one file.
    fn remove_file(&self, path: &Path) -> io::Result<()>;
    /// Delete a directory tree.
    fn remove_dir_all(&self, path: &Path) -> io::Result<()>;
}

/// The real file system.
#[derive(Debug, Default, Clone, Copy)]
pub struct StdFileOps;

impl FileOps for StdFileOps {
    fn remove_file(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }
    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_dir_all(path)
    }
}

/// Outcome of a purge run.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PurgeReport {
    /// `--dry-run`: nothing was deleted, the `*_due` counters are filled.
    pub dry_run: bool,
    /// Dry run: records whose files would be purged.
    pub files_due: u64,
    /// Dry run: records that would be deleted.
    pub records_due: u64,
    /// Records marked `files_purged_at`.
    pub files_purged: u64,
    /// Individual files (TIFF/PDF) deleted or already gone.
    pub files_removed: u64,
    /// Records deleted.
    pub records_deleted: u64,
    /// Batches processed in the file phase.
    pub file_batches: u32,
    /// Batches processed in the record phase.
    pub record_batches: u32,
    /// Rows skipped because a file could not be deleted or its path was unsafe (retried next run).
    pub skipped: u64,
    /// One message per failure (also logged at error level).
    pub errors: Vec<String>,
}

/// Retention executor.
pub struct Purger {
    store: Arc<dyn FaxStore>,
    data_dir: PathBuf,
    clock: Arc<dyn Clock>,
    files: Arc<dyn FileOps>,
    batch: u32,
}

/// Resolve a stored (relative) path and verify it stays inside `data_dir`.
///
/// Rejects `..` escapes, absolute paths outside `data_dir`, and (when the file exists) symlinks
/// that point outside.
pub fn resolve_in_data_dir(data_dir: &Path, stored: &str) -> std::result::Result<PathBuf, String> {
    if stored.is_empty() || stored.contains('\0') {
        return Err(format!("invalid stored path {stored:?}"));
    }
    let base = normalize(data_dir).ok_or_else(|| format!("bad data_dir {}", data_dir.display()))?;
    let p = Path::new(stored);
    let joined = if p.is_absolute() {
        p.to_path_buf()
    } else {
        base.join(p)
    };
    let norm =
        normalize(&joined).ok_or_else(|| format!("path {stored:?} escapes the filesystem root"))?;
    if !norm.starts_with(&base) || norm == base {
        return Err(format!(
            "path {stored:?} is outside data_dir {}",
            base.display()
        ));
    }
    if let (Ok(real), Ok(real_base)) = (std::fs::canonicalize(&norm), std::fs::canonicalize(&base))
        && !real.starts_with(&real_base)
    {
        return Err(format!(
            "path {stored:?} resolves outside data_dir via a symlink"
        ));
    }
    Ok(norm)
}

/// Lexically normalise (`.` and `..` removed). `None` when `..` climbs above the root.
fn normalize(p: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for c in p.components() {
        match c {
            Component::ParentDir => {
                if !out.pop() {
                    return None;
                }
            }
            Component::CurDir => {}
            other => out.push(other.as_os_str()),
        }
    }
    Some(out)
}

impl Purger {
    /// Create a purger working on `data_dir` with the real file system.
    pub fn new(
        store: Arc<dyn FaxStore>,
        data_dir: impl Into<PathBuf>,
        clock: Arc<dyn Clock>,
    ) -> Self {
        Self {
            store,
            data_dir: data_dir.into(),
            clock,
            files: Arc::new(StdFileOps),
            batch: BATCH,
        }
    }

    /// Replace the file operations (tests).
    pub fn with_file_ops(mut self, ops: Arc<dyn FileOps>) -> Self {
        self.files = ops;
        self
    }

    /// Override the batch size (tests).
    pub fn with_batch(mut self, n: u32) -> Self {
        self.batch = n.max(1);
        self
    }

    /// PNG preview cache of one fax (`data_dir/cache/png/<id>`).
    pub fn png_cache_dir(&self, id: i64) -> PathBuf {
        self.data_dir.join("cache").join("png").join(id.to_string())
    }

    /// Delete the files of one record. `Ok(n)` = files removed; `Err(msg)` = something failed or
    /// was refused, in which case the caller must keep the row.
    fn remove_row_files(&self, row: &PurgeRow) -> std::result::Result<u64, String> {
        let mut removed = 0;
        for stored in [&row.tiff_path, &row.pdf_path].into_iter().flatten() {
            let path = resolve_in_data_dir(&self.data_dir, stored)
                .map_err(|e| format!("fax {}: refusing to delete: {e}", row.id))?;
            match self.files.remove_file(&path) {
                Ok(()) => removed += 1,
                Err(e) if e.kind() == io::ErrorKind::NotFound => removed += 1, // already gone: success
                Err(e) => return Err(format!("fax {}: delete {}: {e}", row.id, path.display())),
            }
        }
        let cache = self.png_cache_dir(row.id);
        match self.files.remove_dir_all(&cache) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(format!("fax {}: delete {}: {e}", row.id, cache.display())),
        }
        Ok(removed)
    }

    /// Run the retention job. With `dry_run` only counts are produced.
    pub async fn run(&self, policy: RetentionPolicy, dry_run: bool) -> Result<PurgeReport> {
        policy.validate()?;
        let now = self.clock.now();
        let mut rep = PurgeReport {
            dry_run,
            ..Default::default()
        };
        let files_cutoff =
            (policy.files_days > 0).then(|| now - Duration::days(policy.files_days.into()));
        let records_cutoff =
            (policy.records_days > 0).then(|| now - Duration::days(policy.records_days.into()));

        if dry_run {
            if let Some(c) = files_cutoff {
                rep.files_due = self.store.count_files_due(c).await?;
            }
            if let Some(c) = records_cutoff {
                rep.records_due = self.store.count_records_due(c).await?;
            }
            return Ok(rep);
        }

        if let Some(cutoff) = files_cutoff {
            let mut after = 0;
            loop {
                let rows = self.store.files_due(cutoff, after, self.batch).await?;
                let Some(last) = rows.last() else { break };
                after = last.id;
                rep.file_batches += 1;
                let mut done = Vec::new();
                for row in &rows {
                    match self.remove_row_files(row) {
                        Ok(n) => {
                            rep.files_removed += n;
                            done.push(row.id);
                        }
                        Err(msg) => {
                            tracing::error!("purge: {msg}");
                            rep.errors.push(msg);
                            rep.skipped += 1;
                        }
                    }
                }
                rep.files_purged += self.store.mark_files_purged(&done, now).await?;
            }
        }

        if let Some(cutoff) = records_cutoff {
            let mut after = 0;
            loop {
                let rows = self.store.records_due(cutoff, after, self.batch).await?;
                let Some(last) = rows.last() else { break };
                after = last.id;
                rep.record_batches += 1;
                let mut delete = Vec::new();
                for row in &rows {
                    // Leftover files first; a row whose files cannot be removed is kept.
                    let res = if row.files_purged {
                        Ok(0)
                    } else {
                        self.remove_row_files(row)
                    };
                    match res {
                        Ok(n) => {
                            rep.files_removed += n;
                            delete.push(row.id);
                        }
                        Err(msg) => {
                            tracing::error!("purge: {msg}");
                            rep.errors.push(msg);
                            rep.skipped += 1;
                        }
                    }
                }
                rep.records_deleted += self.store.delete_records(&delete).await?;
            }
        }
        Ok(rep)
    }
}
