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
use std::sync::Arc;
use std::time::Duration;

use chrono::{Local, Utc};
use delivery::{DeliveryEngine, EngineOptions, Routes};
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
        let join = tokio::spawn(run(
            db,
            data_dir,
            db_retry,
            opts.utc_offset_secs,
            rx,
            stop.clone(),
            settings_rx,
        ));
        DeliveryService {
            tx,
            stop,
            join,
            settings,
            grace,
        }
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

async fn run(
    db: Arc<Db>,
    data_dir: PathBuf,
    db_retry: Duration,
    utc_offset_secs: Option<i32>,
    mut rx: mpsc::UnboundedReceiver<i64>,
    stop: Arc<Notify>,
    mut settings: watch::Receiver<Arc<Settings>>,
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
        DeliveryEngine::new(
            store.clone(),
            &st.routes,
            data_dir.clone(),
            Arc::new(SystemClock),
            opts,
        )
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
mod tests {
    //! F10..F15: the real SQLite store, the real recorder and a real folder delivery in a
    //! temporary directory. The only waiting is polling the file system / database for the
    //! background worker's result.
    use super::*;
    use crate::settings::AppConfig;
    use fax_in::event::{
        ByeBy, CallEvent, CallOutcome, CallResult, FailureReason, FaxReport, UdptlCounters,
    };
    use fax_in::numbers::Numbers;
    use fax_in::recorder::{Recorder, RecorderConfig};
    use std::path::Path;
    use std::time::SystemTime;
    use store::{DeliveryAttempt, DeliveryStatus, FaxStatus, FaxStore};

    struct Env {
        _dir: tempfile::TempDir,
        db: Arc<Db>,
        cfg: AppConfig,
        data: PathBuf,
        out: PathBuf,
    }

    fn slashes(p: &Path) -> String {
        p.display().to_string().replace('\\', "/")
    }

    async fn env(routes: &str) -> Env {
        env_with_url(routes, None).await
    }

    async fn env_with_url(routes: &str, url: Option<String>) -> Env {
        let dir = tempfile::tempdir().unwrap();
        let data = dir.path().join("data");
        let out = dir.path().join("out");
        let text = format!(
            "[storage]\ndata_dir = {data:?}\n[database]\nurl = {url:?}\n\
             [[trunks]]\nname = \"t\"\nallowed_sources = [\"10.0.0.0/8\"]\n{routes}",
            data = slashes(&data),
            url =
                url.unwrap_or_else(|| format!("sqlite://{}", slashes(&dir.path().join("fax.db")))),
            routes = routes.replace("OUT", &slashes(&out)),
        );
        let cfg = AppConfig::parse(&text, Path::new("test.toml")).unwrap();
        let db = Arc::new(Db::new(&cfg, dir.path()));
        assert!(matches!(
            db.start().await.unwrap(),
            crate::db::Startup::Ready { .. }
        ));
        Env {
            _dir: dir,
            db,
            cfg,
            data,
            out,
        }
    }

    const ROUTE: &str = "[[routes]]\ndnis = \"200\"\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"OUT\"\nfilename = \"fax-{id}\"\n";

    fn recorder(e: &Env, hook: Option<FinalizedHook>) -> Recorder {
        let mut rc = RecorderConfig::new(e.data.clone());
        rc.on_finalized = hook;
        Recorder::start(e.db.clone(), rc)
    }

    fn service(e: &Env) -> DeliveryService {
        DeliveryService::start(
            e.db.clone(),
            e.cfg.routes.clone(),
            e.data.clone(),
            e.cfg.delivery.clone(),
            Duration::from_millis(50),
        )
    }

    /// A finished call whose stored files exist: PDF and TIFF with distinctive bytes.
    fn finished(e: &Env, call_id: &str, dnis: &str, pages: u32, partial: bool) -> CallResult {
        let sub = e.data.join("faxes/2026/10/21");
        std::fs::create_dir_all(&sub).unwrap();
        let pdf = sub.join(format!("{call_id}.pdf"));
        let tif = sub.join(format!("{call_id}.tif"));
        std::fs::write(&pdf, format!("%PDF-1.4 {call_id} \u{0}\u{1}").as_bytes()).unwrap();
        std::fs::write(&tif, format!("II*\u{0} {call_id}").as_bytes()).unwrap();
        let code = if partial { 7 } else { 0 };
        let now = SystemTime::now();
        CallResult {
            call_id: call_id.into(),
            trunk: "t".into(),
            peer: "10.0.0.1:5060".into(),
            from: "sip:100@10.0.0.1".into(),
            to: format!("sip:{dnis}@10.0.0.9"),
            numbers: Numbers {
                ani: Some("100".into()),
                ani_source: Some("from".into()),
                dnis: Some(dnis.into()),
                dnis_source: Some("ruri".into()),
            },
            started_at: now - Duration::from_secs(60),
            answered_at: Some(now - Duration::from_secs(59)),
            t38_established_at: None,
            ended_at: now,
            bye_by: Some(ByeBy::Local),
            t38: None,
            fax: Some(FaxReport {
                code,
                code_name: if partial { "partial_dcn" } else { "ok" }.into(),
                pages,
                bit_rate: 14_400,
                modem: Some("v17".into()),
                resolution: Some("fine".into()),
                compression: Some("mmr".into()),
                remote_ident: Some("0212345678".into()),
                bad_lines_per_page: vec![0; pages as usize],
                duration_ms: 1000,
                partial,
                tiff: Some(tif.display().to_string()),
                pdf: Some(pdf.display().to_string()),
                store_error: None,
                udptl: UdptlCounters::default(),
                tiff_size: Some(std::fs::metadata(&tif).unwrap().len()),
                pdf_size: Some(std::fs::metadata(&pdf).unwrap().len()),
                ecm: false,
                ecm_ppr_sent: 0,
                ecm_frames_lost: 0,
            }),
            outcome: if partial {
                CallOutcome::Failed {
                    reason: FailureReason::Session {
                        detail: "T.30 partial_dcn (7) after 1 page(s)".into(),
                    },
                }
            } else {
                CallOutcome::Completed
            },
        }
    }

    async fn record_final(rec: &Recorder, r: CallResult) {
        rec.record(&CallEvent::Started {
            call_id: r.call_id.clone(),
            trunk: r.trunk.clone(),
            peer: r.peer.clone(),
            numbers: r.numbers.clone(),
            at: r.started_at,
        });
        rec.record(&CallEvent::Finished(Box::new(r)));
        assert!(rec.flush(Duration::from_secs(20)).await);
    }

    /// Polls until `pred` holds for the attempts of fax `id` (or fails after 15 s).
    async fn attempts_where(
        st: &Arc<dyn FaxStore>,
        id: i64,
        pred: impl Fn(&[DeliveryAttempt]) -> bool,
    ) -> Vec<DeliveryAttempt> {
        for _ in 0..750 {
            let a = st.list_deliveries(id).await.unwrap();
            if pred(&a) {
                return a;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        panic!(
            "attempts never reached the expected state: {:?}",
            st.list_deliveries(id).await.unwrap()
        );
    }

    fn any_done(a: &[DeliveryAttempt]) -> bool {
        a.iter().any(|x| x.status == DeliveryStatus::Done)
    }

    fn files_in(dir: &Path) -> Vec<String> {
        let mut v: Vec<String> = std::fs::read_dir(dir)
            .map(|d| {
                d.map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                    .collect()
            })
            .unwrap_or_default();
        v.sort();
        v
    }

    #[tokio::test]
    async fn f10_a_finalized_call_is_delivered_to_the_folder_and_the_attempt_is_done() {
        let e = env(ROUTE).await;
        let svc = service(&e);
        let rec = recorder(&e, Some(svc.hook()));
        let r = finished(&e, "c10@gw", "200", 2, false);
        let pdf_bytes = std::fs::read(r.fax.as_ref().unwrap().pdf.as_ref().unwrap()).unwrap();
        let tif_bytes = std::fs::read(r.fax.as_ref().unwrap().tiff.as_ref().unwrap()).unwrap();
        record_final(&rec, r).await;
        let st = e.db.store().await.unwrap();
        let row = st.get_by_call_id("c10@gw").await.unwrap().unwrap();
        assert_eq!(row.fax_status, FaxStatus::Received);

        let a = attempts_where(&st, row.id, any_done).await;
        assert_eq!(a.len(), 1, "{a:?}");
        assert_eq!(a[0].kind, "folder");
        assert_eq!(a[0].target, format!("folder:{}", slashes(&e.out)));
        assert_eq!(a[0].attempt, 1);
        assert!(a[0].last_error.is_none());
        assert_eq!(
            files_in(&e.out),
            vec![format!("fax-{}.pdf", row.id), format!("fax-{}.tif", row.id)]
        );
        assert_eq!(
            std::fs::read(e.out.join(format!("fax-{}.pdf", row.id))).unwrap(),
            pdf_bytes
        );
        assert_eq!(
            std::fs::read(e.out.join(format!("fax-{}.tif", row.id))).unwrap(),
            tif_bytes
        );
        // the delivery changed nothing about the record
        let after = st.get_by_call_id("c10@gw").await.unwrap().unwrap();
        assert_eq!((after.fax_status, after.pages), (FaxStatus::Received, 2));
        rec.shutdown(Duration::from_secs(5)).await;
        svc.shutdown().await;
    }

    #[tokio::test]
    async fn f11_a_failing_delivery_neither_touches_the_record_nor_the_recorder() {
        // the destination does not exist and must not be created: every attempt fails, and the
        // failure is a transient one (an unmounted share), so it is retried
        let route = format!("{ROUTE}create_dir = false\n");
        let e = env(&route).await;
        let svc = service(&e);
        let rec = recorder(&e, Some(svc.hook()));
        record_final(&rec, finished(&e, "c11@gw", "200", 1, false)).await;
        let st = e.db.store().await.unwrap();
        let row = st.get_by_call_id("c11@gw").await.unwrap().unwrap();
        // the engine writes the retry row first and closes the failed one after it
        let a = attempts_where(&st, row.id, |a| {
            a.len() >= 2 && a[0].status != DeliveryStatus::Pending
        })
        .await;
        assert_eq!(a[0].status, DeliveryStatus::Failed, "{a:?}");
        assert!(a[0].last_error.as_deref().is_some_and(|m| !m.is_empty()));
        assert_eq!(a[1].status, DeliveryStatus::Pending, "retry scheduled");
        assert_eq!(a[1].attempt, 2);
        assert!(a[1].next_try_at.unwrap() > Utc::now(), "backed off");
        let after = st.get_by_call_id("c11@gw").await.unwrap().unwrap();
        assert_eq!(after.fax_status, FaxStatus::Received);
        let stats = rec.stats();
        assert_eq!((stats.failed, stats.dropped, stats.spilled), (0, 0, 0));
        assert!(!e.out.exists(), "nothing was created");
        rec.shutdown(Duration::from_secs(5)).await;
        svc.shutdown().await;
    }

    #[tokio::test]
    async fn f12_partial_faxes_follow_deliver_partial_and_failed_ones_are_never_delivered() {
        let routes = "[[routes]]\ndnis = \"201\"\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"OUT/no-partial\"\nfilename = \"f{id}\"\n\
                      [[routes]]\ndnis = \"202\"\ndeliver_partial = true\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"OUT/with-partial\"\nfilename = \"f{id}\"\n";
        let e = env(routes).await;
        let svc = service(&e);
        let rec = recorder(&e, Some(svc.hook()));
        let st = e.db.store().await.unwrap();
        record_final(&rec, finished(&e, "p1@gw", "201", 1, true)).await;
        // a call that failed outright (no pages)
        let mut bad = finished(&e, "p3@gw", "202", 0, true);
        bad.fax = None;
        bad.outcome = CallOutcome::Failed {
            reason: FailureReason::Session {
                detail: "T.30 no_response_to_dis (1)".into(),
            },
        };
        record_final(&rec, bad).await;
        record_final(&rec, finished(&e, "p2@gw", "202", 1, true)).await;
        let p1 = st.get_by_call_id("p1@gw").await.unwrap().unwrap();
        let p2 = st.get_by_call_id("p2@gw").await.unwrap().unwrap();
        let p3 = st.get_by_call_id("p3@gw").await.unwrap().unwrap();
        assert_eq!(p1.fax_status, FaxStatus::Partial);
        assert_eq!(p2.fax_status, FaxStatus::Partial);
        assert_eq!(p3.fax_status, FaxStatus::Failed);
        attempts_where(&st, p2.id, any_done).await;
        // the ids were handled in order, so p1 and p3 are decided by now
        svc.shutdown().await;
        assert!(st.list_deliveries(p1.id).await.unwrap().is_empty());
        assert!(st.list_deliveries(p3.id).await.unwrap().is_empty());
        assert!(!e.out.join("no-partial").exists());
        assert_eq!(
            files_in(&e.out.join("with-partial")),
            vec![format!("f{}.pdf", p2.id), format!("f{}.tif", p2.id)]
        );
        rec.shutdown(Duration::from_secs(5)).await;
    }

    #[tokio::test]
    async fn f13_catch_up_delivers_what_was_stored_while_the_engine_was_not_running() {
        let e = env(ROUTE).await;
        // received and recorded, but nobody was listening (crash, or `db backfill` restored it)
        let rec = recorder(&e, None);
        record_final(&rec, finished(&e, "c13@gw", "200", 3, false)).await;
        rec.shutdown(Duration::from_secs(5)).await;
        let st = e.db.store().await.unwrap();
        let row = st.get_by_call_id("c13@gw").await.unwrap().unwrap();
        assert!(st.list_deliveries(row.id).await.unwrap().is_empty());
        assert!(!e.out.exists());

        // the restart
        let svc = service(&e);
        let a = attempts_where(&st, row.id, any_done).await;
        assert_eq!(a.len(), 1);
        assert_eq!(files_in(&e.out).len(), 2);
        svc.shutdown().await;

        // another restart does not deliver it a second time
        let svc = service(&e);
        svc.shutdown().await;
        assert_eq!(st.list_deliveries(row.id).await.unwrap().len(), 1);
        assert_eq!(files_in(&e.out).len(), 2);
    }

    #[tokio::test]
    async fn f14_catch_up_hours_zero_turns_the_catch_up_off_but_not_the_notifications() {
        let mut e = env(ROUTE).await;
        let rec = recorder(&e, None);
        record_final(&rec, finished(&e, "c14@gw", "200", 1, false)).await;
        rec.shutdown(Duration::from_secs(5)).await;
        let st = e.db.store().await.unwrap();
        let row = st.get_by_call_id("c14@gw").await.unwrap().unwrap();
        e.cfg.delivery.catch_up_hours = 0;
        let svc = service(&e);
        svc.shutdown().await;
        assert!(st.list_deliveries(row.id).await.unwrap().is_empty());
        assert!(!e.out.exists());
        let svc = service(&e);
        svc.hook()(row.id);
        attempts_where(&st, row.id, any_done).await;
        svc.shutdown().await;
        assert_eq!(files_in(&e.out).len(), 2);
    }

    #[tokio::test]
    async fn f15_stopping_while_the_database_is_unreachable_does_not_hang() {
        // nothing listens on port 1: every connection attempt is refused
        let dir = tempfile::tempdir().unwrap();
        let text = format!(
            "[database]\nurl = {url:?}\n[[trunks]]\nname = \"t\"\nallowed_sources = [\"10.0.0.0/8\"]\n{routes}",
            url = "postgres://fax:secret@127.0.0.1:1/fax",
            routes = ROUTE.replace("OUT", &slashes(&dir.path().join("out"))),
        );
        let cfg = AppConfig::parse(&text, Path::new("test.toml")).unwrap();
        let db = Arc::new(Db::new(&cfg, dir.path()));
        assert!(
            db.store().await.is_err(),
            "the database is really not there"
        );
        let svc = DeliveryService::start(
            db.clone(),
            cfg.routes.clone(),
            dir.path().join("data"),
            cfg.delivery.clone(),
            Duration::from_millis(50),
        );
        svc.hook()(1);
        tokio::time::timeout(Duration::from_secs(5), svc.shutdown())
            .await
            .expect("shutdown does not wait for a database that is not there");
        assert!(!dir.path().join("out").exists());
    }

    // ------------------------------------------------------------------ M4a

    /// A reconfigured route set takes effect for the next fax; a broken one is refused.
    #[tokio::test]
    async fn k07_new_routes_apply_to_the_next_fax_and_a_broken_set_is_refused() {
        let route_a = "[[routes]]\ndnis = \"200\"\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"OUT/a\"\nfilename = \"f{id}\"\n";
        let e = env(route_a).await;
        let svc = service(&e);
        let rec = recorder(&e, Some(svc.hook()));
        let st = e.db.store().await.unwrap();
        record_final(&rec, finished(&e, "h1@gw", "200", 1, false)).await;
        let id1 = st.get_by_call_id("h1@gw").await.unwrap().unwrap().id;
        attempts_where(&st, id1, any_done).await;
        assert_eq!(files_in(&e.out.join("a")).len(), 2);

        let route_b = format!(
            "[[routes]]\ndnis = \"200\"\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"{}\"\nfilename = \"f{{id}}\"\n",
            slashes(&e.out.join("b"))
        );
        let routes_b: Routes = {
            let c = AppConfig::parse(&route_b, Path::new("b.toml")).unwrap();
            c.routes
        };
        svc.reconfigurer()
            .reconfigure(routes_b.clone(), DeliverySection::default())
            .unwrap();
        record_final(&rec, finished(&e, "h2@gw", "200", 1, false)).await;
        let id2 = st.get_by_call_id("h2@gw").await.unwrap().unwrap().id;
        attempts_where(&st, id2, any_done).await;
        assert_eq!(
            files_in(&e.out.join("b")).len(),
            2,
            "the second fax went by the new route"
        );
        assert_eq!(
            files_in(&e.out.join("a")).len(),
            2,
            "and nothing more went the old way"
        );

        // a broken set (default route that is not last) is refused and the engine keeps working
        let broken: Routes = toml::from_str(
            "[[routes]]\ndefault = true\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"/x\"\n[[routes]]\ndnis = \"1\"\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"/y\"\n",
        )
        .unwrap();
        let errs = svc
            .reconfigurer()
            .reconfigure(broken, DeliverySection::default())
            .unwrap_err();
        assert!(errs.iter().any(|m| m.contains("must be last")), "{errs:?}");
        assert!(
            svc.reconfigurer()
                .reconfigure(
                    routes_b,
                    DeliverySection {
                        max_attempts: 0,
                        ..DeliverySection::default()
                    }
                )
                .is_err()
        );
        record_final(&rec, finished(&e, "h3@gw", "200", 1, false)).await;
        let id3 = st.get_by_call_id("h3@gw").await.unwrap().unwrap().id;
        attempts_where(&st, id3, any_done).await;
        assert_eq!(files_in(&e.out.join("b")).len(), 4);
        rec.shutdown(Duration::from_secs(5)).await;
        svc.shutdown().await;
    }

    /// R05: `{date}` / `{time}` of a file name are local time (here UTC+8 as in Taipei).
    #[tokio::test]
    async fn r05_file_names_use_the_local_time_of_the_server() {
        let route = "[[routes]]\ndnis = \"200\"\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"OUT\"\nfilename = \"{date}_{time}_{id}\"\nformats = [\"pdf\"]\n";
        let e = env(route).await;
        let svc = DeliveryService::start_with(
            e.db.clone(),
            e.cfg.routes.clone(),
            e.data.clone(),
            e.cfg.delivery.clone(),
            ServiceOptions {
                db_retry: Duration::from_millis(50),
                utc_offset_secs: Some(8 * 3600),
                ..ServiceOptions::default()
            },
        );
        let rec = recorder(&e, Some(svc.hook()));
        record_final(&rec, finished(&e, "r05@gw", "200", 1, false)).await;
        let st = e.db.store().await.unwrap();
        let row = st.get_by_call_id("r05@gw").await.unwrap().unwrap();
        attempts_where(&st, row.id, any_done).await;
        let local = (row.started_at + chrono::Duration::hours(8)).format("%Y%m%d_%H%M%S");
        let utc = row.started_at.format("%Y%m%d_%H%M%S");
        let names = files_in(&e.out);
        assert_eq!(names, vec![format!("{local}_{}.pdf", row.id)], "{names:?}");
        assert_ne!(
            local.to_string(),
            utc.to_string(),
            "the test would prove nothing at UTC+0"
        );
        assert!(!names[0].starts_with(&utc.to_string()));
        rec.shutdown(Duration::from_secs(5)).await;
        svc.shutdown().await;
    }

    /// A tiny HTTP server for webhook tests: `answer = false` reads the request and never
    /// replies; `true` answers 200. Returns the number of requests seen.
    async fn http_server(
        listener: tokio::net::TcpListener,
        answer: bool,
    ) -> (
        Arc<std::sync::atomic::AtomicUsize>,
        tokio::task::JoinHandle<()>,
    ) {
        use std::sync::atomic::{AtomicUsize, Ordering};
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        let seen = Arc::new(AtomicUsize::new(0));
        let s2 = seen.clone();
        let j = tokio::spawn(async move {
            let mut conns = Vec::new();
            loop {
                let Ok((mut sock, _)) = listener.accept().await else {
                    return;
                };
                let s3 = s2.clone();
                conns.push(tokio::spawn(async move {
                    let mut buf = vec![0u8; 65536];
                    let mut have = 0usize;
                    loop {
                        let Ok(n) = sock.read(&mut buf[have..]).await else {
                            return;
                        };
                        if n == 0 {
                            return;
                        }
                        have += n;
                        let text = String::from_utf8_lossy(&buf[..have]).to_string();
                        if let Some(h) = text.find("\r\n\r\n") {
                            let len = text
                                .lines()
                                .find_map(|l| {
                                    l.to_ascii_lowercase()
                                        .strip_prefix("content-length:")
                                        .and_then(|v| v.trim().parse::<usize>().ok())
                                })
                                .unwrap_or(0);
                            if have >= h + 4 + len {
                                break;
                            }
                        }
                    }
                    s3.fetch_add(1, Ordering::SeqCst);
                    if answer {
                        let _ = sock
                            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n")
                            .await;
                    } else {
                        tokio::time::sleep(Duration::from_secs(600)).await;
                    }
                }));
            }
        });
        (seen, j)
    }

    /// R03: an attempt that is still running when the grace period ends is abandoned; its row is
    /// still `pending`, the next start delivers it, once.
    #[tokio::test]
    async fn r03_an_attempt_beyond_the_grace_stays_pending_and_the_next_start_finishes_it() {
        use std::sync::atomic::Ordering;
        let hang = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = hang.local_addr().unwrap();
        let (hang_seen, hang_task) = http_server(hang, false).await;
        let route = format!(
            "[[routes]]\ndnis = \"200\"\n[[routes.deliveries]]\nkind = \"webhook\"\nurl = \"http://{addr}/hook\"\n"
        );
        let e = env(&route).await;
        let opts = ServiceOptions {
            db_retry: Duration::from_millis(50),
            grace: Duration::from_millis(300),
            utc_offset_secs: Some(0),
        };
        let svc = DeliveryService::start_with(
            e.db.clone(),
            e.cfg.routes.clone(),
            e.data.clone(),
            e.cfg.delivery.clone(),
            opts,
        );
        let rec = recorder(&e, Some(svc.hook()));
        record_final(&rec, finished(&e, "r03@gw", "200", 1, false)).await;
        rec.shutdown(Duration::from_secs(5)).await;
        for _ in 0..500 {
            if hang_seen.load(Ordering::SeqCst) == 1 {
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert_eq!(
            hang_seen.load(Ordering::SeqCst),
            1,
            "the attempt is in flight"
        );
        let started = std::time::Instant::now();
        svc.shutdown().await;
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "shutdown gave up after the grace period ({:?})",
            started.elapsed()
        );
        let st = e.db.store().await.unwrap();
        let row = st.get_by_call_id("r03@gw").await.unwrap().unwrap();
        let a = st.list_deliveries(row.id).await.unwrap();
        assert_eq!(a.len(), 1, "{a:?}");
        assert_eq!(
            (a[0].status, a[0].attempt),
            (DeliveryStatus::Pending, 1),
            "the row is still retryable"
        );
        // the next start: the same address now answers
        hang_task.abort();
        let _ = hang_task.await;
        let ok = tokio::net::TcpListener::bind(addr).await.unwrap();
        let (ok_seen, ok_task) = http_server(ok, true).await;
        let svc = DeliveryService::start_with(
            e.db.clone(),
            e.cfg.routes.clone(),
            e.data.clone(),
            e.cfg.delivery.clone(),
            opts,
        );
        let a = attempts_where(&st, row.id, any_done).await;
        assert_eq!(a.len(), 1, "no duplicate row: {a:?}");
        assert_eq!(a[0].attempt, 1);
        assert_eq!(ok_seen.load(Ordering::SeqCst), 1, "delivered exactly once");
        assert_eq!(hang_seen.load(Ordering::SeqCst), 1);
        svc.shutdown().await;
        ok_task.abort();
    }

    /// R04 (needs a database server in Docker: `scripts/db-matrix.sh` sets the variables): the
    /// database restarts while faxes are being received and delivered; afterwards every fax has
    /// been delivered once.
    #[tokio::test]
    async fn r04_a_database_restart_loses_and_duplicates_nothing() {
        let (Ok(url), Ok(container)) = (
            std::env::var("SIP_FAXD_TEST_DB"),
            std::env::var("SIP_FAXD_TEST_DB_CONTAINER"),
        ) else {
            eprintln!(
                "r04: skipped (set SIP_FAXD_TEST_DB and SIP_FAXD_TEST_DB_CONTAINER; scripts/db-matrix.sh does)"
            );
            return;
        };
        if url.starts_with("sqlite") {
            return;
        }
        // a database of our own on that server
        let name = format!("r04_{}", std::process::id());
        let docker = |args: &[&str]| {
            std::process::Command::new("docker")
                .args(args)
                .output()
                .expect("docker runs")
        };
        let create = if url.starts_with("postgres") {
            vec![
                "exec".to_string(),
                container.clone(),
                "psql".into(),
                "-U".into(),
                "postgres".into(),
                "-c".into(),
                format!("CREATE DATABASE {name}"),
            ]
        } else if url.starts_with("mssql") {
            vec![
                "exec".to_string(),
                container.clone(),
                "/opt/mssql-tools18/bin/sqlcmd".into(),
                "-C".into(),
                "-S".into(),
                "localhost".into(),
                "-U".into(),
                "sa".into(),
                "-P".into(),
                "Sipfax_Test_1".into(),
                "-Q".into(),
                format!("CREATE DATABASE {name}"),
            ]
        } else {
            let client = if url.starts_with("mariadb") {
                "mariadb"
            } else {
                "mysql"
            };
            vec![
                "exec".to_string(),
                container.clone(),
                client.into(),
                "-uroot".into(),
                "-pSipfax_Test_1".into(),
                "-e".into(),
                format!("CREATE DATABASE {name} CHARACTER SET utf8mb4"),
            ]
        };
        let refs: Vec<&str> = create.iter().map(String::as_str).collect();
        let o = docker(&refs);
        assert!(o.status.success(), "{}", String::from_utf8_lossy(&o.stderr));
        let mut u = url::Url::parse(&url.replacen("mariadb:", "mysql:", 1)).unwrap();
        u.set_path(&format!("/{name}"));
        let db_url = u.to_string().replacen(
            "mysql:",
            if url.starts_with("mariadb") {
                "mariadb:"
            } else {
                "mysql:"
            },
            1,
        );

        let e = env_with_url(ROUTE, Some(db_url)).await;
        let svc = service(&e);
        let rec = recorder(&e, Some(svc.hook()));
        // restart the database while faxes keep coming
        let c2 = container.clone();
        let restart = tokio::task::spawn_blocking(move || {
            std::process::Command::new("docker")
                .args(["restart", &c2])
                .output()
                .unwrap()
        });
        for i in 0..6 {
            let r = finished(&e, &format!("r04-{i}@gw"), "200", 1, false);
            rec.record(&CallEvent::Started {
                call_id: r.call_id.clone(),
                trunk: r.trunk.clone(),
                peer: r.peer.clone(),
                numbers: r.numbers.clone(),
                at: r.started_at,
            });
            rec.record(&CallEvent::Finished(Box::new(r)));
            tokio::time::sleep(Duration::from_millis(700)).await;
        }
        assert!(restart.await.unwrap().status.success());
        // every fax gets recorded (the recorder retries while the database is away) and delivered
        let st = e.db.store().await.unwrap();
        let mut ids = Vec::new();
        for i in 0..6 {
            let mut found = None;
            for _ in 0..1500 {
                if let Ok(Some(r)) = st.get_by_call_id(&format!("r04-{i}@gw")).await {
                    found = Some(r.id);
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
            ids.push(found.unwrap_or_else(|| panic!("fax r04-{i} was lost")));
        }
        for id in &ids {
            let a = attempts_where(&st, *id, any_done).await;
            assert_eq!(
                a.iter()
                    .filter(|x| x.status == DeliveryStatus::Done)
                    .count(),
                1,
                "delivered once: {a:?}"
            );
        }
        let names = files_in(&e.out);
        assert_eq!(
            names.len(),
            2 * ids.len(),
            "a pdf and a tif per fax, no duplicates: {names:?}"
        );
        rec.shutdown(Duration::from_secs(10)).await;
        svc.shutdown().await;
    }
}
