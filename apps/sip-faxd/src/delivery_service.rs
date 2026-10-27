//! The delivery engine of `serve` (docs/design/m3-store.md section 9).
//!
//! The recorder calls the hook of [`DeliveryService::hook`] with the id of every fax row it has
//! written; a background task owns the engine and works the `delivery_attempts` queue. Nothing
//! here can hold up or fail a call: the hook only pushes the id into an unbounded channel, and
//! when the database is not reachable the task just waits for it (the recorder's own retry and
//! `db backfill` cover the record, `catch_up` the delivery).
//!
//! * start: wait for the database, `catch_up(now - catch_up_hours)`, then `spawn`;
//! * a change of `[[routes]]` / `[delivery]` ([`DeliveryService::reconfigure`], M4a): the running
//!   engine finishes its attempts and a new one takes over with the new routes; attempts that
//!   already exist keep their target and are found again by it;
//! * ids that arrive before the engine is ready are kept in the channel and forwarded;
//! * shutdown: ids still in the channel are dispatched (so their `pending` rows exist for the
//!   next start), running attempts get a grace period; an attempt that is still running after it
//!   is abandoned (the worker is aborted): its row is still `pending` and is tried again at the
//!   next start (R03).
//!
//! `db backfill` is an offline command and never delivers; a call it restores is delivered by the
//! next `serve` start through `catch_up` when it is recent enough (`catch_up_hours`).

use std::path::PathBuf;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use chrono::{Local, Utc};
use delivery::{ChangeHook, DeliveryEngine, EngineOptions, Routes};
use fax_in::recorder::FinalizedHook;
use store::SystemClock;
use tokio::sync::{Notify, mpsc, watch};
use tokio::task::JoinHandle;

use crate::db::Db;
use crate::settings::DeliverySection;

/// How long `shutdown` waits for attempts that are running.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(15);
/// Pause between connection attempts while the database is not reachable.
const DB_RETRY: Duration = Duration::from_secs(10);

/// Routes and engine parameters in force.
#[derive(Clone)]
struct Settings {
    routes: Routes,
    cfg: DeliverySection,
}

/// Changes the routes / parameters of a running [`DeliveryService`].
#[derive(Clone)]
pub struct Reconfigurer(Arc<watch::Sender<Arc<Settings>>>);

impl Reconfigurer {
    /// New routes / parameters (a configuration change): the engine is replaced at once; faxes
    /// that arrive from now on are routed by the new routes, attempts that already exist keep
    /// their target and are found again by it.
    ///
    /// # Errors
    /// The routes do not validate; nothing changed.
    pub fn reconfigure(&self, routes: Routes, cfg: DeliverySection) -> Result<(), Vec<String>> {
        routes.validate()?;
        if cfg.max_attempts == 0 {
            return Err(vec!["max_attempts must be at least 1".into()]);
        }
        self.0.send_replace(Arc::new(Settings { routes, cfg }));
        Ok(())
    }
}

/// Timing and clock parameters of a [`DeliveryService`].
#[derive(Debug, Clone, Copy)]
pub struct ServiceOptions {
    /// Pause between connection attempts while the database is not reachable.
    pub db_retry: Duration,
    /// How long `shutdown` waits for attempts that are running.
    pub grace: Duration,
    /// Offset from UTC (seconds) for `{date}` / `{time}` in file names; `None` = the time zone of
    /// the server (read when the engine is built).
    pub utc_offset_secs: Option<i32>,
}

impl Default for ServiceOptions {
    fn default() -> Self {
        ServiceOptions {
            db_retry: DB_RETRY,
            grace: SHUTDOWN_GRACE,
            utc_offset_secs: None,
        }
    }
}

/// The running delivery engine and the way to feed it.
pub struct DeliveryService {
    tx: mpsc::UnboundedSender<i64>,
    stop: Arc<Notify>,
    join: JoinHandle<()>,
    settings: Reconfigurer,
    grace: Duration,
    on_change: Arc<OnceLock<ChangeHook>>,
}

impl DeliveryService {
    /// Starts the background task. Must be called inside a tokio runtime.
    pub fn start(
        db: Arc<Db>,
        routes: Routes,
        data_dir: PathBuf,
        cfg: DeliverySection,
        db_retry: Duration,
    ) -> DeliveryService {
        Self::start_with(
            db,
            routes,
            data_dir,
            cfg,
            ServiceOptions {
                db_retry,
                ..ServiceOptions::default()
            },
        )
    }

    /// [`DeliveryService::start`] with every parameter.
    pub fn start_with(
        db: Arc<Db>,
        routes: Routes,
        data_dir: PathBuf,
        cfg: DeliverySection,
        opts: ServiceOptions,
    ) -> DeliveryService {
        let (db_retry, grace) = (opts.db_retry, opts.grace);
        let (tx, rx) = mpsc::unbounded_channel();
        let stop = Arc::new(Notify::new());
        let (settings, settings_rx) = watch::channel(Arc::new(Settings { routes, cfg }));
        let settings = Reconfigurer(Arc::new(settings));
        let on_change: Arc<OnceLock<ChangeHook>> = Arc::default();
        let join = tokio::spawn(run(
            db,
            data_dir,
            db_retry,
            opts.utc_offset_secs,
            rx,
            stop.clone(),
            settings_rx,
            on_change.clone(),
        ));
        DeliveryService {
            tx,
            stop,
            join,
            settings,
            grace,
            on_change,
        }
    }

    /// Tells `hook` the fax id whenever its delivery attempts change, for every engine this
    /// service runs (also the ones a reconfiguration makes). Only the first hook counts.
    pub fn set_on_change(&self, hook: ChangeHook) {
        let _ = self.on_change.set(hook);
    }

    /// A handle that changes the routes and parameters of this service from elsewhere (the
    /// configuration applier).
    pub fn reconfigurer(&self) -> Reconfigurer {
        self.settings.clone()
    }

    /// [`DeliveryService::start`] with the production retry pause.
    pub fn start_default(
        db: Arc<Db>,
        routes: Routes,
        data_dir: PathBuf,
        cfg: DeliverySection,
    ) -> DeliveryService {
        Self::start(db, routes, data_dir, cfg, DB_RETRY)
    }

    /// The hook for `RecorderConfig::on_finalized`.
    pub fn hook(&self) -> FinalizedHook {
        let tx = self.tx.clone();
        Arc::new(move |id| {
            let _ = tx.send(id);
        })
    }

    /// Stops the task (see the module documentation).
    pub async fn shutdown(self) {
        self.stop.notify_one();
        let mut join = self.join;
        if tokio::time::timeout(self.grace, &mut join).await.is_err() {
            // the attempt in flight cannot finish in time: abandon it. Its row is still
            // `pending` (a row is only closed when the attempt returns), so the next start
            // tries it again.
            join.abort();
            tracing::error!(
                "delivery attempts still running at shutdown: they stay pending and are retried at the next start"
            );
        }
    }
}

// (the one task of the service and everything it owns)
#[allow(clippy::too_many_arguments)]
async fn run(
    db: Arc<Db>,
    data_dir: PathBuf,
    db_retry: Duration,
    utc_offset_secs: Option<i32>,
    mut rx: mpsc::UnboundedReceiver<i64>,
    stop: Arc<Notify>,
    mut settings: watch::Receiver<Arc<Settings>>,
    on_change: Arc<OnceLock<ChangeHook>>,
) {
    let store = loop {
        let attempt = tokio::select! {
            () = stop.notified() => return,
            r = db.store() => r,
        };
        match attempt {
            Ok(s) => break s,
            Err(e) => {
                tracing::warn!("delivery waits for the database: {e}");
                tokio::select! {
                    () = stop.notified() => return,
                    () = tokio::time::sleep(db_retry) => {}
                }
            }
        }
    };
    let build = |st: &Settings| {
        let opts = EngineOptions {
            max_attempts: st.cfg.max_attempts,
            utc_offset_secs: utc_offset_secs
                .unwrap_or_else(|| Local::now().offset().local_minus_utc()),
            ..EngineOptions::default()
        };
        let e = DeliveryEngine::new(
            store.clone(),
            &st.routes,
            data_dir.clone(),
            Arc::new(SystemClock),
            opts,
        )?;
        // the hook may be set after this engine was made: look it up at every change
        let slot = on_change.clone();
        e.set_on_change(Arc::new(move |id| {
            if let Some(h) = slot.get() {
                h(id);
            }
        }));
        Ok::<_, delivery::Error>(e)
    };
    let current = settings.borrow_and_update().clone();
    let mut engine = match build(&current) {
        Ok(e) => Some(e),
        Err(e) => {
            tracing::error!("delivery is off until the routes are fixed: {e}");
            None
        }
    };
    if let Some(engine) = &engine
        && current.cfg.catch_up_hours > 0
    {
        let since = Utc::now() - chrono::Duration::hours(i64::from(current.cfg.catch_up_hours));
        match engine.catch_up(since).await {
            Ok(0) => tracing::debug!("delivery catch-up: nothing to queue"),
            Ok(n) => tracing::info!(
                attempts = n,
                "delivery catch-up queued deliveries of faxes received earlier"
            ),
            Err(e) => tracing::error!("delivery catch-up failed: {e}"),
        }
    }
    let mut handle = engine.clone().map(DeliveryEngine::spawn);
    if engine.is_some() {
        tracing::info!(
            routes = current.routes.routes.len(),
            "delivery engine running"
        );
    }
    loop {
        tokio::select! {
            () = stop.notified() => break,
            msg = rx.recv() => match msg {
                Some(id) => match &handle {
                    Some(h) => h.notify_finalized(id),
                    // no engine (invalid routes): the fax is found by the catch-up of the next start
                    None => tracing::warn!(fax_id = id, "delivery is off: the fax is not queued"),
                },
                None => break,
            },
            changed = settings.changed() => {
                if changed.is_err() {
                    continue;
                }
                let new = settings.borrow_and_update().clone();
                // the old engine finishes the attempts it runs, then the new one takes over
                if let Some(h) = handle.take() {
                    h.shutdown().await;
                }
                match build(&new) {
                    Ok(e) => {
                        tracing::info!(routes = new.routes.routes.len(), "delivery engine restarted with the new routes");
                        handle = Some(e.clone().spawn());
                        engine = Some(e);
                    }
                    Err(e) => {
                        tracing::error!("delivery is off until the routes are fixed: {e}");
                        engine = None;
                    }
                }
            }
        }
    }
    // what the recorder announced during its own shutdown: make sure the rows exist
    if let Some(engine) = &engine {
        while let Ok(id) = rx.try_recv() {
            if let Err(e) = engine.dispatch(id).await {
                tracing::error!(fax_id = id, "delivery dispatch at shutdown failed: {e}");
            }
        }
    }
    if let Some(h) = handle {
        h.shutdown().await;
    }
}

#[cfg(test)]
mod tests;
