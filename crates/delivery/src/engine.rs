//! The delivery engine: creates `delivery_attempts` for finished faxes and works the queue.
//!
//! Queue model (one row per attempt, so the table is the full history):
//!
//! * [`DeliveryEngine::dispatch`] inserts one `pending` row per configured delivery of the matching
//!   route (`attempt = 1`, `next_try_at = now`). It is idempotent per `(fax, target)`.
//! * [`DeliveryEngine::run_due`] takes the `pending` rows whose `next_try_at <= now` and runs them.
//!   Success: the row becomes `done`. Transient failure: the row becomes `failed` (with
//!   `last_error`) and a new `pending` row with `attempt + 1` and `next_try_at = now + backoff` is
//!   inserted; when `attempt` reaches `max_attempts` the row becomes `giving_up` instead. Permanent
//!   failure: the row becomes `failed` and nothing more is scheduled.
//! * The queue lives in the database only, so after a restart the same rows are simply picked up
//!   again. A crash between "insert the next attempt" and "close the current one" leaves a
//!   `pending` row that already has a successor; it is closed as superseded instead of being run
//!   twice (delivery is at-least-once, never silently lost).
//! * Nothing here changes the fax record itself.

use crate::config::{DeliveryConfig, Routes};
use crate::error::{DeliveryError, Error, Result};
use crate::retry::{DEFAULT_MAX_ATTEMPTS, backoff};
use crate::router::Router;
use crate::{email, folder, webhook};
use chrono::{DateTime, FixedOffset, Utc};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;
use store::{
    Clock, DeliveryAttempt, DeliveryStatus, FaxRecord, FaxStatus, FaxStore, ListFilter,
    NewDelivery, PageRequest,
};
use tokio::sync::{Mutex, Notify, Semaphore, mpsc};
use tokio::task::JoinSet;

/// Tunables; `Default` is what sip-faxd uses.
#[derive(Debug, Clone)]
pub struct EngineOptions {
    /// Attempts before an attempt is closed as `giving_up` (default 10).
    pub max_attempts: u32,
    /// How often the background worker looks for due attempts (default 10 s).
    pub poll_interval: Duration,
    /// Due attempts fetched per round (default 50).
    pub batch: u32,
    /// Attempts running at the same time (default 4).
    pub concurrency: usize,
    /// Hard limit for one attempt (default 120 s).
    pub attempt_timeout: Duration,
    /// UTC offset in seconds used for `{date}` / `{time}` in templates (default 0 = UTC).
    pub utc_offset_secs: i32,
}

impl Default for EngineOptions {
    fn default() -> Self {
        Self {
            max_attempts: DEFAULT_MAX_ATTEMPTS,
            poll_interval: Duration::from_secs(10),
            batch: 50,
            concurrency: 4,
            attempt_timeout: Duration::from_secs(120),
            utc_offset_secs: 0,
        }
    }
}

/// What one [`DeliveryEngine::run_due`] round did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunReport {
    /// Attempts executed (superseded / orphaned rows are not counted).
    pub attempted: u32,
    /// Delivered.
    pub done: u32,
    /// Failed transiently, next attempt scheduled.
    pub retry_scheduled: u32,
    /// Closed as `giving_up`.
    pub gave_up: u32,
    /// Failed permanently, not retried (also: target no longer configured, record gone).
    pub failed: u32,
}

struct Inner {
    store: Arc<dyn FaxStore>,
    router: Router,
    data_dir: PathBuf,
    clock: Arc<dyn Clock>,
    opts: EngineOptions,
    http: reqwest::Client,
    dispatch_lock: Mutex<()>,
    on_change: std::sync::OnceLock<ChangeHook>,
}

/// Callback of [`DeliveryEngine::set_on_change`]: the id of a fax whose delivery attempts changed.
pub type ChangeHook = Arc<dyn Fn(i64) + Send + Sync>;

impl Inner {
    fn changed(&self, fax_id: i64) {
        if let Some(h) = self.on_change.get() {
            h(fax_id);
        }
    }
}

/// Creates delivery attempts and executes them. Cheap to clone.
#[derive(Clone)]
pub struct DeliveryEngine {
    inner: Arc<Inner>,
}

impl DeliveryEngine {
    /// Build an engine. `routes` is validated; `data_dir` is the directory the record paths are
    /// relative to; `clock` should be the same clock the store uses (tests inject a manual clock).
    pub fn new(
        store: Arc<dyn FaxStore>,
        routes: &Routes,
        data_dir: impl Into<PathBuf>,
        clock: Arc<dyn Clock>,
        opts: EngineOptions,
    ) -> Result<Self> {
        routes.validate().map_err(Error::Config)?;
        if opts.max_attempts == 0 {
            return Err(Error::Config(vec!["max_attempts must be >= 1".into()]));
        }
        if FixedOffset::east_opt(opts.utc_offset_secs).is_none() {
            return Err(Error::Config(vec!["utc_offset_secs out of range".into()]));
        }
        Ok(Self {
            inner: Arc::new(Inner {
                store,
                router: Router::new(routes),
                data_dir: data_dir.into(),
                clock,
                opts,
                http: webhook::client(),
                dispatch_lock: Mutex::new(()),
                on_change: std::sync::OnceLock::new(),
            }),
        })
    }

    /// Tells `hook` the fax id whenever attempts of that fax are created or change (the web
    /// interface's `delivery.updated`). Must not block. Only the first hook counts.
    pub fn set_on_change(&self, hook: ChangeHook) {
        let _ = self.inner.on_change.set(hook);
    }

    /// The router (for `config check` previews and tests).
    pub fn router(&self) -> &Router {
        &self.inner.router
    }

    /// A fax was finalized: queue its deliveries. Returns the ids of the new `delivery_attempts`
    /// rows (empty when nothing applies: unknown id, not finished, status not deliverable, no
    /// matching route, or already queued).
    pub async fn dispatch(&self, fax_id: i64) -> Result<Vec<i64>> {
        match self.inner.store.get(fax_id).await? {
            Some(rec) => self.dispatch_record(&rec).await,
            None => {
                tracing::warn!(fax_id, "delivery: fax record not found");
                Ok(Vec::new())
            }
        }
    }

    async fn dispatch_record(&self, rec: &FaxRecord) -> Result<Vec<i64>> {
        let i = &self.inner;
        if rec.ended_at.is_none()
            || !matches!(rec.fax_status, FaxStatus::Received | FaxStatus::Partial)
        {
            return Ok(Vec::new());
        }
        let Some(route) = i.router.find(rec) else {
            tracing::debug!(fax_id = rec.id, trunk = %rec.trunk, "delivery: no matching route");
            return Ok(Vec::new());
        };
        if !route.wants(rec.fax_status) {
            tracing::debug!(fax_id = rec.id, status = %rec.fax_status, "delivery: status not delivered by route");
            return Ok(Vec::new());
        }
        let _guard = i.dispatch_lock.lock().await;
        let existing = i.store.list_deliveries(rec.id).await?;
        let now = i.clock.now();
        let mut created = Vec::new();
        for d in &route.deliveries {
            let target = d.target();
            if existing.iter().any(|e| e.target == target) {
                continue;
            }
            let id = i
                .store
                .add_delivery(&NewDelivery {
                    fax_id: rec.id,
                    target,
                    kind: d.kind().to_string(),
                    attempt: 1,
                    status: DeliveryStatus::Pending,
                    next_try_at: Some(now),
                })
                .await?;
            created.push(id);
        }
        if !created.is_empty() {
            i.changed(rec.id);
        }
        Ok(created)
    }

    /// Queue deliveries for finished `received`/`partial` faxes that started at or after `since`
    /// and have none yet (call once at startup to repair a crash between finalize and dispatch).
    /// Returns the number of attempts created.
    pub async fn catch_up(&self, since: DateTime<Utc>) -> Result<usize> {
        let mut created = 0;
        for status in [FaxStatus::Received, FaxStatus::Partial] {
            let filter = ListFilter {
                fax_status: Some(status),
                from: Some(since),
                ..Default::default()
            };
            let mut cursor = None;
            loop {
                let page = self
                    .inner
                    .store
                    .list(&filter, PageRequest { cursor, limit: 200 })
                    .await?;
                for rec in &page.items {
                    created += self.dispatch_record(rec).await?.len();
                }
                match page.next {
                    Some(c) => cursor = Some(c),
                    None => break,
                }
            }
        }
        Ok(created)
    }

    /// Run every attempt that is due now. Returns when all of them have finished.
    pub async fn run_due(&self) -> Result<RunReport> {
        let i = &self.inner;
        let mut report = RunReport::default();
        let sem = Arc::new(Semaphore::new(i.opts.concurrency.max(1)));
        loop {
            let due = i.store.due_deliveries(i.clock.now(), i.opts.batch).await?;
            if due.is_empty() {
                break;
            }
            let mut set = JoinSet::new();
            for row in due {
                let inner = i.clone();
                let sem = sem.clone();
                set.spawn(async move {
                    let _permit = sem.acquire_owned().await.expect("semaphore open");
                    let fax_id = row.fax_id;
                    let r = process(&inner, row).await;
                    if r.is_ok() {
                        inner.changed(fax_id);
                    }
                    r
                });
            }
            while let Some(res) = set.join_next().await {
                match res {
                    Ok(Ok(o)) => {
                        o.add_to(&mut report);
                    }
                    Ok(Err(e)) => return Err(e),
                    Err(e) => tracing::error!("delivery task panicked: {e}"),
                }
            }
            // Every fetched row left `pending`, so the next fetch only returns newly due rows.
        }
        Ok(report)
    }

    /// Start the background worker: it dispatches faxes announced through the returned handle and
    /// runs due attempts every `poll_interval`. Must be called inside a tokio runtime.
    pub fn spawn(self) -> DeliveryHandle {
        let (tx, mut rx) = mpsc::unbounded_channel::<i64>();
        let stop = Arc::new(Notify::new());
        let stop2 = stop.clone();
        let poll = self.inner.opts.poll_interval;
        let join = tokio::spawn(async move {
            // what a catch-up queued (or an earlier run left due) goes out now, not one poll later
            if let Err(e) = self.run_due().await {
                tracing::error!("delivery run failed: {e}");
            }
            loop {
                tokio::select! {
                    _ = stop2.notified() => break,
                    msg = rx.recv() => {
                        let Some(id) = msg else { break };
                        if let Err(e) = self.dispatch(id).await {
                            tracing::error!(fax_id = id, "delivery dispatch failed: {e}");
                        }
                        if let Err(e) = self.run_due().await {
                            tracing::error!("delivery run failed: {e}");
                        }
                    }
                    _ = tokio::time::sleep(poll) => {
                        if let Err(e) = self.run_due().await {
                            tracing::error!("delivery run failed: {e}");
                        }
                    }
                }
            }
        });
        DeliveryHandle { tx, stop, join }
    }
}

/// Handle to the background worker started by [`DeliveryEngine::spawn`].
pub struct DeliveryHandle {
    tx: mpsc::UnboundedSender<i64>,
    stop: Arc<Notify>,
    join: tokio::task::JoinHandle<()>,
}

impl DeliveryHandle {
    /// Announce a finalized fax. Never blocks and never fails the caller: if the worker is gone the
    /// notification is dropped (the next [`DeliveryEngine::catch_up`] will find the fax).
    pub fn notify_finalized(&self, fax_id: i64) {
        let _ = self.tx.send(fax_id);
    }

    /// A cloneable sender for the same notifications (handy for fax-in's event loop).
    pub fn notifier(&self) -> mpsc::UnboundedSender<i64> {
        self.tx.clone()
    }

    /// Stop the worker after the attempts in flight have finished.
    pub async fn shutdown(self) {
        self.stop.notify_one();
        let _ = self.join.await;
    }
}

enum Outcome {
    Skipped,
    Done,
    Retry,
    GaveUp,
    Failed,
}

impl Outcome {
    fn add_to(&self, r: &mut RunReport) {
        match self {
            Outcome::Skipped => {}
            Outcome::Done => {
                r.attempted += 1;
                r.done += 1;
            }
            Outcome::Retry => {
                r.attempted += 1;
                r.retry_scheduled += 1;
            }
            Outcome::GaveUp => {
                r.attempted += 1;
                r.gave_up += 1;
            }
            Outcome::Failed => {
                r.attempted += 1;
                r.failed += 1;
            }
        }
    }
}

fn clip(s: &str) -> String {
    const MAX: usize = 1000;
    if s.chars().count() <= MAX {
        s.to_string()
    } else {
        let mut t: String = s.chars().take(MAX - 1).collect();
        t.push('…');
        t
    }
}

async fn close(
    inner: &Inner,
    row: &DeliveryAttempt,
    status: DeliveryStatus,
    err: &str,
) -> Result<()> {
    inner
        .store
        .update_delivery(row.id, status, Some(&clip(err)), None)
        .await?;
    Ok(())
}

async fn process(inner: &Inner, row: DeliveryAttempt) -> Result<Outcome> {
    // The record may have been purged meanwhile.
    let Some(rec) = inner.store.get(row.fax_id).await? else {
        close(
            inner,
            &row,
            DeliveryStatus::Failed,
            "fax record no longer exists",
        )
        .await?;
        return Ok(Outcome::Failed);
    };
    // Crash recovery: a successor row exists, so this attempt was already accounted for.
    let siblings = inner.store.list_deliveries(row.fax_id).await?;
    if siblings
        .iter()
        .any(|s| s.target == row.target && s.id > row.id)
    {
        close(
            inner,
            &row,
            DeliveryStatus::Failed,
            "superseded by a later attempt",
        )
        .await?;
        return Ok(Outcome::Skipped);
    }
    let cfg = inner
        .router
        .find(&rec)
        .filter(|r| r.wants(rec.fax_status))
        .and_then(|r| r.deliveries.iter().find(|d| d.target() == row.target));
    let Some(cfg) = cfg else {
        close(
            inner,
            &row,
            DeliveryStatus::Failed,
            "delivery target is no longer configured for this fax",
        )
        .await?;
        return Ok(Outcome::Failed);
    };

    let offset = FixedOffset::east_opt(inner.opts.utc_offset_secs).expect("validated");
    let result = tokio::time::timeout(
        inner.opts.attempt_timeout,
        run_one(inner, cfg, &rec, &row, offset),
    )
    .await
    .unwrap_or_else(|_| {
        Err(DeliveryError::transient(format!(
            "attempt timed out after {:?}",
            inner.opts.attempt_timeout
        )))
    });

    match result {
        Ok(()) => {
            inner
                .store
                .update_delivery(row.id, DeliveryStatus::Done, None, None)
                .await?;
            tracing::info!(fax_id = rec.id, target = %row.target, attempt = row.attempt, "delivered");
            Ok(Outcome::Done)
        }
        Err(e) if !e.is_transient() => {
            tracing::warn!(fax_id = rec.id, target = %row.target, "delivery failed permanently: {}", e.message);
            close(inner, &row, DeliveryStatus::Failed, &e.message).await?;
            Ok(Outcome::Failed)
        }
        Err(e) if row.attempt as u32 >= inner.opts.max_attempts => {
            tracing::error!(fax_id = rec.id, target = %row.target, "delivery giving up after {} attempts: {}", row.attempt, e.message);
            close(inner, &row, DeliveryStatus::GivingUp, &e.message).await?;
            Ok(Outcome::GaveUp)
        }
        Err(e) => {
            let next = inner.clock.now() + backoff(row.attempt as u32);
            tracing::warn!(fax_id = rec.id, target = %row.target, attempt = row.attempt, "delivery failed, retry at {next}: {}", e.message);
            // Successor first: a crash in between leaves a superseded row, never a lost retry.
            inner
                .store
                .add_delivery(&NewDelivery {
                    fax_id: row.fax_id,
                    target: row.target.clone(),
                    kind: row.kind.clone(),
                    attempt: row.attempt + 1,
                    status: DeliveryStatus::Pending,
                    next_try_at: Some(next),
                })
                .await?;
            close(inner, &row, DeliveryStatus::Failed, &e.message).await?;
            Ok(Outcome::Retry)
        }
    }
}

async fn run_one(
    inner: &Inner,
    cfg: &DeliveryConfig,
    rec: &FaxRecord,
    row: &DeliveryAttempt,
    offset: FixedOffset,
) -> std::result::Result<(), DeliveryError> {
    match cfg {
        DeliveryConfig::Folder(f) => folder::deliver(f, rec, &inner.data_dir, offset)
            .await
            .map(|_| ()),
        DeliveryConfig::Email(e) => email::deliver(e, rec, &inner.data_dir, offset).await,
        DeliveryConfig::Webhook(w) => {
            webhook::deliver(&inner.http, w, rec, &inner.data_dir, row.attempt).await
        }
    }
}
