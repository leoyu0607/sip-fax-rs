//! Writes call records to the database without ever holding up a call (docs/design/m3-store.md
//! sections 0 and 4).
//!
//! The controller's [`CallEvent`]s are mapped to three kinds of jobs and put in an in-memory
//! queue that one worker drains in order:
//!
//! * `Started`       -> INSERT a `receiving` row,
//! * `PageReceived`  -> UPDATE its page count,
//! * `Finished`      -> finalize the row from [`classify`] (an `upsert`, so a row whose INSERT
//!   was lost is created now).
//!
//! A transient database error (`store::Error::is_transient`) is retried with exponential
//! backoff; when the attempts run out the failure is logged as an error. A call that could not
//! be recorded keeps its `result.json` (written to the spill directory if the capture did not
//! write one), and `sip-faxd db backfill` restores it later. While the database stays down the
//! worker is "degraded": every job gets a single attempt, so the queue drains instead of
//! waiting minutes per job.

use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use store::{FaxFinal, FaxStore, NewFax};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

use crate::event::{CallEvent, CallOutcome, CallResult, FailureReason};
use crate::numbers::Numbers;
use crate::summary::classify;

// ------------------------------------------------------------------------ mapping

fn utc(t: SystemTime) -> DateTime<Utc> {
    DateTime::<Utc>::from(t)
}

/// Cuts `s` to at most `max` characters (the column width), so that one long header can never
/// make the whole record fail with `TooLong`.
fn fit(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}

fn fit_opt(s: &Option<String>, max: usize) -> Option<String> {
    s.as_deref().map(|s| fit(s, max))
}

/// The row inserted when a call starts.
pub fn new_fax(
    call_id: &str,
    trunk: &str,
    numbers: &Numbers,
    started: SystemTime,
    answered: Option<SystemTime>,
) -> NewFax {
    NewFax {
        call_id: fit(call_id, 255),
        trunk: fit(trunk, 64),
        ani: fit_opt(&numbers.ani, 64),
        ani_source: fit_opt(&numbers.ani_source, 16),
        dnis: fit_opt(&numbers.dnis, 64),
        dnis_source: fit_opt(&numbers.dnis_source, 16),
        started_at: utc(started),
        answered_at: answered.map(utc),
    }
}

/// `path` relative to `data_dir` (the form stored in the database). Paths below `data_dir`
/// are stripped of it; absolute paths of another installation (a `result.json` copied from
/// another host) are cut at their `faxes` directory; anything else is kept as it is, which
/// the purge job will refuse to touch.
pub fn relative_to_data_dir(path: &str, data_dir: &Path) -> String {
    let p = Path::new(path);
    if let Ok(rel) = p.strip_prefix(data_dir) {
        return slashes(rel);
    }
    let comps: Vec<_> = p.components().collect();
    if let Some(i) = comps
        .iter()
        .rposition(|c| c.as_os_str() == "faxes")
        .filter(|i| *i + 1 < comps.len())
    {
        let mut out = PathBuf::new();
        for c in &comps[i..] {
            out.push(c.as_os_str());
        }
        return slashes(&out);
    }
    path.to_string()
}

fn slashes(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

/// The values written when the call ends.
pub fn fax_final(r: &CallResult, data_dir: &Path) -> FaxFinal {
    let s = classify(r);
    let f = r.fax.as_ref();
    let size = |s: Option<u64>| s.and_then(|v| i64::try_from(v).ok());
    FaxFinal {
        ended_at: utc(r.ended_at),
        answered_at: r.answered_at.map(utc),
        duration_sec: Some(s.duration_sec),
        hangup_direction: s.hangup_direction,
        hangup_code: s.hangup_code.as_str().to_string(),
        hangup_text: Some(fit(&s.hangup_text, 255)),
        sip_status: s.sip_status,
        t30_code: s.t30_code,
        fax_status: s.fax_status,
        pages: s.pages,
        remote_ident: f
            .and_then(|f| f.remote_ident.as_deref())
            .map(|s| fit(s, 32)),
        resolution: f.and_then(|f| f.resolution.as_deref()).map(|s| fit(s, 12)),
        bit_rate: f
            .filter(|f| f.bit_rate > 0)
            .and_then(|f| i32::try_from(f.bit_rate).ok()),
        compression: f
            .and_then(|f| f.compression.as_deref())
            .map(|s| fit(&s.to_ascii_uppercase(), 8)),
        ecm: f.is_some_and(|f| f.ecm),
        tiff_path: f
            .and_then(|f| f.tiff.as_deref())
            .map(|p| fit(&relative_to_data_dir(p, data_dir), 1024)),
        tiff_size: f.and_then(|f| size(f.tiff_size)),
        pdf_path: f
            .and_then(|f| f.pdf.as_deref())
            .map(|p| fit(&relative_to_data_dir(p, data_dir), 1024)),
        pdf_size: f.and_then(|f| size(f.pdf_size)),
        modem: f.and_then(|f| f.modem.as_deref()).map(|s| fit(s, 16)),
        media: f.map(|f| {
            match f.media {
                crate::event::FaxMedia::T38 => "t38",
                crate::event::FaxMedia::G711 => "g711",
            }
            .to_string()
        }),
    }
}

/// The insert part of a finished call (what `Started` would have written).
pub fn new_fax_of(r: &CallResult) -> NewFax {
    let numbers = if r.numbers == Numbers::default() {
        // result.json files written before the numbers were recorded
        crate::numbers::from_uris(&r.from, &r.to)
    } else {
        r.numbers.clone()
    };
    new_fax(&r.call_id, &r.trunk, &numbers, r.started_at, r.answered_at)
}

/// True for the results that are never stored: sources outside every trunk that were refused
/// before a call existed (docs/design/m3-store.md section 3: port scans must not fill the table).
pub fn is_unrecorded(r: &CallResult) -> bool {
    matches!(
        r.outcome,
        CallOutcome::Failed {
            reason: FailureReason::UnknownSource
        }
    )
}

// ------------------------------------------------------------------------ sink

/// The three database operations the recorder needs. [`StoreSink`] runs them on a real
/// [`FaxStore`]; tests wrap that to inject outages.
#[async_trait]
pub trait RecordSink: Send + Sync {
    /// INSERT a `receiving` row.
    async fn insert_receiving(&self, new: &NewFax) -> store::Result<i64>;
    /// UPDATE the page count of a `receiving` row.
    async fn update_pages(&self, id: i64, pages: i32, at: DateTime<Utc>) -> store::Result<bool>;
    /// Finalize the row of the call, inserting it first if it does not exist.
    async fn upsert_finished(&self, new: &NewFax, fin: &FaxFinal) -> store::Result<(i64, bool)>;
}

/// [`RecordSink`] on a [`FaxStore`].
pub struct StoreSink(pub Arc<dyn FaxStore>);

#[async_trait]
impl RecordSink for StoreSink {
    async fn insert_receiving(&self, new: &NewFax) -> store::Result<i64> {
        self.0.insert_receiving(new).await
    }
    async fn update_pages(&self, id: i64, pages: i32, at: DateTime<Utc>) -> store::Result<bool> {
        self.0.update_pages(id, pages, at).await
    }
    async fn upsert_finished(&self, new: &NewFax, fin: &FaxFinal) -> store::Result<(i64, bool)> {
        self.0.upsert_finished(new, fin).await
    }
}

// ------------------------------------------------------------------------ retry

/// Exponential backoff: `initial`, `2 * initial`, ... capped at `max_delay`, at most
/// `max_attempts` attempts per job.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetryPolicy {
    /// Wait after the first failed attempt.
    pub initial: Duration,
    /// Upper bound of one wait.
    pub max_delay: Duration,
    /// Attempts per job (the first one included).
    pub max_attempts: u32,
}

impl Default for RetryPolicy {
    fn default() -> Self {
        RetryPolicy {
            initial: Duration::from_secs(1),
            max_delay: Duration::from_secs(60),
            max_attempts: 8,
        }
    }
}

impl RetryPolicy {
    /// The wait after `failed` failed attempts (`failed >= 1`).
    pub fn delay(&self, failed: u32) -> Duration {
        let shift = failed.saturating_sub(1).min(20);
        self.initial
            .saturating_mul(1u32 << shift)
            .min(self.max_delay)
    }
}

/// How the worker waits between attempts (a test injects its own).
pub trait Sleeper: Send + Sync {
    /// Waits for `d`.
    fn sleep(&self, d: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}

/// [`Sleeper`] on the tokio timer.
pub struct TokioSleeper;

impl Sleeper for TokioSleeper {
    fn sleep(&self, d: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(tokio::time::sleep(d))
    }
}

// ------------------------------------------------------------------------ recorder

/// Settings of a [`Recorder`].
#[derive(Clone)]
pub struct RecorderConfig {
    /// `[storage] data_dir`, resolved: the database stores file paths relative to it.
    pub data_dir: PathBuf,
    /// Where a `result.json` is written for a call whose final write failed and whose capture
    /// did not keep one (`<spill_dir>/<call-id>/result.json`, the capture layout). `None`: do
    /// not write.
    pub spill_dir: Option<PathBuf>,
    /// Retry schedule.
    pub retry: RetryPolicy,
    /// Queue length above which page updates and inserts are dropped (finals never are).
    pub max_queue: usize,
    /// Wait between attempts.
    pub sleeper: Arc<dyn Sleeper>,
    /// Called with the row id after a *final* write reached the database (also after retries
    /// that succeeded). Must not block: sip-faxd hands the id to the delivery engine here.
    /// Never called for a write that was given up on, nor by [`backfill`].
    pub on_finalized: Option<FinalizedHook>,
    /// Called after every write that reached the database (the web interface's events). Must
    /// not block.
    pub on_written: Option<WrittenHook>,
}

/// Callback of [`RecorderConfig::on_finalized`].
pub type FinalizedHook = Arc<dyn Fn(i64) + Send + Sync>;

/// Callback of [`RecorderConfig::on_written`].
pub type WrittenHook = Arc<dyn Fn(&RecordWritten) + Send + Sync>;

/// A write of the recorder that reached the database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecordWritten {
    /// The `receiving` row of an answered call.
    Created {
        /// Row id.
        id: i64,
        /// SIP Call-ID.
        call_id: String,
    },
    /// The page count of a call in progress.
    Pages {
        /// Row id.
        id: i64,
        /// SIP Call-ID.
        call_id: String,
        /// Pages so far.
        pages: u32,
    },
    /// The final result.
    Finalized {
        /// Row id.
        id: i64,
        /// SIP Call-ID.
        call_id: String,
    },
}

impl RecorderConfig {
    /// Defaults for `data_dir`.
    pub fn new(data_dir: PathBuf) -> Self {
        RecorderConfig {
            data_dir,
            spill_dir: None,
            retry: RetryPolicy::default(),
            max_queue: 10_000,
            sleeper: Arc::new(TokioSleeper),
            on_finalized: None,
            on_written: None,
        }
    }
}

/// Counters of a [`Recorder`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RecorderStats {
    /// Jobs waiting or in progress.
    pub queued: usize,
    /// Jobs that reached the database.
    pub written: u64,
    /// Failed attempts that were retried.
    pub retries: u64,
    /// Jobs given up on (logged as errors; finals are restored by `db backfill`).
    pub failed: u64,
    /// Jobs dropped because the queue was full.
    pub dropped: u64,
    /// `result.json` files written for failed finals.
    pub spilled: u64,
}

#[derive(Default)]
struct Counters {
    queued: AtomicUsize,
    written: AtomicU64,
    retries: AtomicU64,
    failed: AtomicU64,
    dropped: AtomicU64,
    spilled: AtomicU64,
    closing: AtomicBool,
}

enum Job {
    Insert(NewFax),
    Pages {
        call_id: String,
        pages: u32,
        at: SystemTime,
    },
    Finish {
        new: Box<NewFax>,
        fin: Box<FaxFinal>,
        call_id: String,
        json: String,
    },
}

/// The write queue. Cheap to call from the event loop: [`Recorder::record`] never blocks.
pub struct Recorder {
    tx: std::sync::Mutex<Option<mpsc::UnboundedSender<Job>>>,
    counters: Arc<Counters>,
    cfg: RecorderConfig,
    worker: std::sync::Mutex<Option<JoinHandle<()>>>,
}

impl Recorder {
    /// Starts the worker.
    pub fn start(sink: Arc<dyn RecordSink>, cfg: RecorderConfig) -> Recorder {
        let (tx, rx) = mpsc::unbounded_channel();
        let counters = Arc::new(Counters::default());
        let worker = tokio::spawn(run_worker(sink, cfg.clone(), counters.clone(), rx));
        Recorder {
            tx: std::sync::Mutex::new(Some(tx)),
            counters,
            cfg,
            worker: std::sync::Mutex::new(Some(worker)),
        }
    }

    /// Queues what `ev` means for the database. Never blocks and never fails.
    pub fn record(&self, ev: &CallEvent) {
        let job = match ev {
            CallEvent::Started {
                call_id,
                trunk,
                numbers,
                at,
                ..
            } => Job::Insert(new_fax(call_id, trunk, numbers, *at, None)),
            CallEvent::PageReceived { call_id, pages, at } => Job::Pages {
                call_id: call_id.clone(),
                pages: *pages,
                at: *at,
            },
            CallEvent::Finished(r) => {
                if is_unrecorded(r) {
                    tracing::debug!(call_id = %r.call_id, "call refused before it existed: not recorded");
                    return;
                }
                Job::Finish {
                    new: Box::new(new_fax_of(r)),
                    fin: Box::new(fax_final(r, &self.cfg.data_dir)),
                    call_id: r.call_id.clone(),
                    json: serde_json::to_string_pretty(&**r).unwrap_or_default(),
                }
            }
            CallEvent::Answered { .. } | CallEvent::T38Established { .. } => return,
        };
        let is_final = matches!(job, Job::Finish { .. });
        if !is_final && self.counters.queued.load(Ordering::Relaxed) >= self.cfg.max_queue {
            self.counters.dropped.fetch_add(1, Ordering::Relaxed);
            tracing::error!("database write queue is full: dropping a record update");
            return;
        }
        let guard = self
            .tx
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(tx) = guard.as_ref() {
            self.counters.queued.fetch_add(1, Ordering::Relaxed);
            if tx.send(job).is_err() {
                self.counters.queued.fetch_sub(1, Ordering::Relaxed);
            }
        }
    }

    /// Current counters.
    pub fn stats(&self) -> RecorderStats {
        let c = &self.counters;
        RecorderStats {
            queued: c.queued.load(Ordering::Relaxed),
            written: c.written.load(Ordering::Relaxed),
            retries: c.retries.load(Ordering::Relaxed),
            failed: c.failed.load(Ordering::Relaxed),
            dropped: c.dropped.load(Ordering::Relaxed),
            spilled: c.spilled.load(Ordering::Relaxed),
        }
    }

    /// Waits until the queue is empty (for tests and for `shutdown`).
    pub async fn flush(&self, timeout: Duration) -> bool {
        let deadline = tokio::time::Instant::now() + timeout;
        while self.counters.queued.load(Ordering::Relaxed) > 0 {
            if tokio::time::Instant::now() >= deadline {
                return false;
            }
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        true
    }

    /// Stops accepting events, gives the worker `grace` to drain the queue (retries stop
    /// waiting at once: every remaining job gets one attempt), and ends it.
    pub async fn shutdown(&self, grace: Duration) {
        self.counters.closing.store(true, Ordering::SeqCst);
        drop(
            self.tx
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .take(),
        );
        let worker = self
            .worker
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(w) = worker {
            let abort = w.abort_handle();
            if tokio::time::timeout(grace, w).await.is_err() {
                abort.abort();
                tracing::error!(
                    "database write queue did not drain in time; `sip-faxd db backfill` restores the rest"
                );
            }
        }
    }
}

struct Worker {
    sink: Arc<dyn RecordSink>,
    cfg: RecorderConfig,
    c: Arc<Counters>,
    ids: HashMap<String, i64>,
    degraded: bool,
}

async fn run_worker(
    sink: Arc<dyn RecordSink>,
    cfg: RecorderConfig,
    c: Arc<Counters>,
    mut rx: mpsc::UnboundedReceiver<Job>,
) {
    let mut w = Worker {
        sink,
        cfg,
        c,
        ids: HashMap::new(),
        degraded: false,
    };
    while let Some(job) = rx.recv().await {
        w.handle(job).await;
        w.c.queued.fetch_sub(1, Ordering::Relaxed);
    }
}

impl Worker {
    /// Runs `op` until it works, fails for good, or the attempts run out.
    async fn attempt<T, F, Fut>(&mut self, what: &str, op: F) -> Option<T>
    where
        F: Fn() -> Fut,
        Fut: Future<Output = store::Result<T>>,
    {
        let max = if self.degraded || self.c.closing.load(Ordering::SeqCst) {
            1
        } else {
            self.cfg.retry.max_attempts.max(1)
        };
        let mut failed = 0u32;
        loop {
            match op().await {
                Ok(v) => {
                    self.degraded = false;
                    self.c.written.fetch_add(1, Ordering::Relaxed);
                    return Some(v);
                }
                Err(e) if e.is_transient() => {
                    failed += 1;
                    if failed >= max {
                        self.degraded = true;
                        self.c.failed.fetch_add(1, Ordering::Relaxed);
                        tracing::error!(
                            attempts = failed,
                            "database unavailable, giving up on {what}: {e}"
                        );
                        return None;
                    }
                    let d = self.cfg.retry.delay(failed);
                    self.c.retries.fetch_add(1, Ordering::Relaxed);
                    tracing::warn!(
                        attempt = failed,
                        retry_in_ms = d.as_millis() as u64,
                        "database unavailable while {what}: {e}"
                    );
                    self.cfg.sleeper.sleep(d).await;
                }
                Err(e) => {
                    self.c.failed.fetch_add(1, Ordering::Relaxed);
                    tracing::error!("cannot {what}: {e}");
                    return None;
                }
            }
        }
    }

    fn written(&self, w: &RecordWritten) {
        if let Some(hook) = &self.cfg.on_written {
            hook(w);
        }
    }

    async fn handle(&mut self, job: Job) {
        let sink = self.sink.clone();
        match job {
            Job::Insert(new) => {
                let what = format!("record call {}", new.call_id);
                if let Some(id) = self
                    .attempt(&what, || {
                        let sink = sink.clone();
                        let new = new.clone();
                        async move { sink.insert_receiving(&new).await }
                    })
                    .await
                {
                    self.ids.insert(new.call_id.clone(), id);
                    self.written(&RecordWritten::Created {
                        id,
                        call_id: new.call_id.clone(),
                    });
                }
            }
            Job::Pages { call_id, pages, at } => {
                // without an id the INSERT was lost; the final write creates the row anyway
                let Some(id) = self.ids.get(&call_id).copied() else {
                    return;
                };
                let what = format!("update the page count of call {call_id}");
                let count = i32::try_from(pages).unwrap_or(i32::MAX);
                if self
                    .attempt(&what, || {
                        let sink = sink.clone();
                        async move { sink.update_pages(id, count, utc(at)).await }
                    })
                    .await
                    .is_some()
                {
                    self.written(&RecordWritten::Pages { id, call_id, pages });
                }
            }
            Job::Finish {
                new,
                fin,
                call_id,
                json,
            } => {
                self.ids.remove(&call_id);
                let what = format!("finish the record of call {call_id}");
                let done = self
                    .attempt(&what, || {
                        let sink = sink.clone();
                        let (new, fin) = (new.clone(), fin.clone());
                        async move { sink.upsert_finished(&new, &fin).await }
                    })
                    .await;
                match done {
                    Some((id, _)) => {
                        if let Some(hook) = &self.cfg.on_finalized {
                            hook(id);
                        }
                        self.written(&RecordWritten::Finalized { id, call_id });
                    }
                    None => self.spill(&call_id, &json).await,
                }
            }
        }
    }

    /// Keeps the result of a call that did not reach the database (unless the capture already did).
    async fn spill(&mut self, call_id: &str, json: &str) {
        let Some(dir) = &self.cfg.spill_dir else {
            return;
        };
        let dir = dir.join(crate::capture::dir_name(call_id));
        let file = dir.join("result.json");
        if tokio::fs::try_exists(&file).await.unwrap_or(false) {
            return;
        }
        let res = async {
            tokio::fs::create_dir_all(&dir).await?;
            tokio::fs::write(&file, json).await
        }
        .await;
        match res {
            Ok(()) => {
                self.c.spilled.fetch_add(1, Ordering::Relaxed);
                tracing::error!(call_id, file = %file.display(),
                    "the call is not in the database yet; result.json kept for `sip-faxd db backfill`");
            }
            Err(e) => tracing::error!(call_id, "cannot write {}: {e}", file.display()),
        }
    }
}

// ------------------------------------------------------------------------ backfill

/// What [`backfill`] did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct BackfillReport {
    /// `result.json` files found.
    pub scanned: u64,
    /// Calls that were missing and have been inserted.
    pub inserted: u64,
    /// Calls that were stuck in `receiving` and have been finished.
    pub updated: u64,
    /// Calls that were already complete in the database.
    pub already_recorded: u64,
    /// Results that are never stored (refused sources).
    pub not_recordable: u64,
    /// Files that could not be read, parsed or written, with the reason.
    pub errors: Vec<String>,
}

/// Restores the calls of `calls_dir/*/result.json` (the capture layout) that are missing from
/// the database, or still `receiving`. Safe to repeat: a call that is already complete is left
/// alone. `data_dir` turns the absolute file paths of the results into the relative ones the
/// database stores.
pub async fn backfill(store: &dyn FaxStore, calls_dir: &Path, data_dir: &Path) -> BackfillReport {
    let mut rep = BackfillReport::default();
    let mut entries = match tokio::fs::read_dir(calls_dir).await {
        Ok(e) => e,
        Err(e) => {
            rep.errors
                .push(format!("cannot read {}: {e}", calls_dir.display()));
            return rep;
        }
    };
    let mut files = Vec::new();
    while let Ok(Some(ent)) = entries.next_entry().await {
        let f = ent.path().join("result.json");
        if tokio::fs::try_exists(&f).await.unwrap_or(false) {
            files.push(f);
        }
    }
    files.sort();
    for f in files {
        rep.scanned += 1;
        let text = match tokio::fs::read_to_string(&f).await {
            Ok(t) => t,
            Err(e) => {
                rep.errors.push(format!("{}: {e}", f.display()));
                continue;
            }
        };
        let r: CallResult = match serde_json::from_str(&text) {
            Ok(r) => r,
            Err(e) => {
                rep.errors.push(format!("{}: {e}", f.display()));
                continue;
            }
        };
        if is_unrecorded(&r) {
            rep.not_recordable += 1;
            continue;
        }
        let new = new_fax_of(&r);
        let existing = match store.get_by_call_id(&new.call_id).await {
            Ok(e) => e,
            Err(e) => {
                rep.errors.push(format!("{}: {e}", f.display()));
                continue;
            }
        };
        if existing
            .as_ref()
            .is_some_and(|x| x.fax_status != store::FaxStatus::Receiving)
        {
            rep.already_recorded += 1;
            continue;
        }
        match store.upsert_finished(&new, &fax_final(&r, data_dir)).await {
            Ok((_, true)) => rep.inserted += 1,
            Ok((_, false)) => rep.updated += 1,
            Err(e) => rep.errors.push(format!("{}: {e}", f.display())),
        }
    }
    rep
}
