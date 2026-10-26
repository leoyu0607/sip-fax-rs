//! M3b write path: events -> records (S02 flow on the fax-in side), the retry queue (S11,
//! fax-in side), C04 and backfill. The database is a real SQLite store; the outage is injected in
//! front of it ([`Flaky`] fails calls with the error a lost connection gives, and passes the rest
//! to the real store), and the backoff waits are recorded by an injected [`Sleeper`], so nothing
//! really sleeps.

use std::future::Future;
use std::path::Path;
use std::pin::Pin;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fax_in::event::{
    ByeBy, CallEvent, CallOutcome, CallResult, FailureReason, FaxReport, UdptlCounters,
};
use fax_in::numbers::Numbers;
use fax_in::recorder::{
    RecordSink, Recorder, RecorderConfig, RetryPolicy, Sleeper, StoreSink, backfill, fax_final,
    new_fax_of, relative_to_data_dir,
};
use store::{FaxFinal, FaxStatus, FaxStore, HangupDirection, NewFax};

const T0: u64 = 1_792_540_800; // 2026-10-21T00:00:00Z

fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(T0 + secs)
}

async fn memory_store() -> Arc<dyn FaxStore> {
    store::connect("sqlite::memory:").await.unwrap()
}

// ---------------------------------------------------------------- fixtures

fn numbers() -> Numbers {
    Numbers {
        ani: Some("0200000001".into()),
        ani_source: Some("from".into()),
        dnis: Some("0200000002".into()),
        dnis_source: Some("ruri".into()),
    }
}

fn started(call_id: &str) -> CallEvent {
    CallEvent::Started {
        call_id: call_id.into(),
        trunk: "audiocodes".into(),
        peer: "192.0.2.10:5060".into(),
        numbers: numbers(),
        at: at(0),
    }
}

fn page(call_id: &str, pages: u32) -> CallEvent {
    CallEvent::PageReceived {
        call_id: call_id.into(),
        pages,
        at: at(10 * u64::from(pages)),
    }
}

fn report(pages: u32) -> FaxReport {
    FaxReport {
        code: 0,
        code_name: "ok".into(),
        pages,
        bit_rate: 14_400,
        modem: Some("v17".into()),
        resolution: Some("fine".into()),
        compression: Some("mmr".into()),
        remote_ident: Some("0212345678".into()),
        bad_lines_per_page: vec![0; pages as usize],
        duration_ms: 31_000,
        partial: false,
        tiff: Some("/srv/fax/data/faxes/2026/10/21/c1.tif".into()),
        pdf: Some("/srv/fax/data/faxes/2026/10/21/c1.pdf".into()),
        store_error: None,
        udptl: UdptlCounters::default(),
        media: Default::default(),
        rtp: None,
        tiff_size: Some(12_345),
        pdf_size: Some(23_456),
        ecm: true,
        ecm_ppr_sent: 2,
        ecm_frames_lost: 0,
    }
}

fn finished_ok(call_id: &str, pages: u32) -> CallResult {
    CallResult {
        call_id: call_id.into(),
        trunk: "audiocodes".into(),
        peer: "192.0.2.10:5060".into(),
        from: "sip:0200000001@192.0.2.10".into(),
        to: "sip:0200000002@198.51.100.20".into(),
        numbers: numbers(),
        started_at: at(0),
        answered_at: Some(at(1)),
        t38_established_at: Some(at(1)),
        ended_at: at(64),
        bye_by: Some(ByeBy::Local),
        t38: None,
        audio: None,
        fax: Some(report(pages)),
        outcome: CallOutcome::Completed,
    }
}

fn fin(r: CallResult) -> CallEvent {
    CallEvent::Finished(Box::new(r))
}

// ---------------------------------------------------------------- fault injection

/// Records the waits instead of waiting.
#[derive(Default)]
struct RecordedSleeps(Mutex<Vec<Duration>>);

impl Sleeper for RecordedSleeps {
    fn sleep(&self, d: Duration) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        self.0.lock().unwrap().push(d);
        Box::pin(async {})
    }
}

impl RecordedSleeps {
    fn waits(&self) -> Vec<u64> {
        self.0
            .lock()
            .unwrap()
            .iter()
            .map(Duration::as_secs)
            .collect()
    }
}

/// The real store behind an outage switch: while `down > 0` every call fails with the error a lost
/// connection produces (`Unavailable`) and uses up one unit; `hard` makes calls fail with a
/// non-transient error.
struct Flaky {
    inner: StoreSink,
    down: AtomicU32,
    hard: AtomicU32,
    calls: AtomicU32,
}

impl Flaky {
    fn new(store: Arc<dyn FaxStore>, down: u32) -> Arc<Flaky> {
        Arc::new(Flaky {
            inner: StoreSink(store),
            down: AtomicU32::new(down),
            hard: AtomicU32::new(0),
            calls: AtomicU32::new(0),
        })
    }
    fn fail(&self) -> store::Result<()> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        if self.hard.load(Ordering::SeqCst) > 0 {
            self.hard.fetch_sub(1, Ordering::SeqCst);
            return Err(store::Error::Backend("constraint failed".into()));
        }
        if self.down.load(Ordering::SeqCst) > 0 {
            self.down.fetch_sub(1, Ordering::SeqCst);
            return Err(store::Error::Unavailable("connection refused".into()));
        }
        Ok(())
    }
    fn set_down(&self, n: u32) {
        self.down.store(n, Ordering::SeqCst);
    }
}

#[async_trait]
impl RecordSink for Flaky {
    async fn insert_receiving(&self, new: &NewFax) -> store::Result<i64> {
        self.fail()?;
        self.inner.insert_receiving(new).await
    }
    async fn update_pages(&self, id: i64, pages: i32, at: DateTime<Utc>) -> store::Result<bool> {
        self.fail()?;
        self.inner.update_pages(id, pages, at).await
    }
    async fn upsert_finished(&self, new: &NewFax, f: &FaxFinal) -> store::Result<(i64, bool)> {
        self.fail()?;
        self.inner.upsert_finished(new, f).await
    }
}

fn recorder(
    sink: Arc<dyn RecordSink>,
    sleeps: &Arc<RecordedSleeps>,
    spill: Option<&Path>,
) -> Recorder {
    let mut cfg = RecorderConfig::new("/srv/fax/data".into());
    cfg.sleeper = sleeps.clone();
    cfg.spill_dir = spill.map(Path::to_path_buf);
    cfg.retry = RetryPolicy {
        initial: Duration::from_secs(1),
        max_delay: Duration::from_secs(30),
        max_attempts: 5,
    };
    Recorder::start(sink, cfg)
}

async fn settle(r: &Recorder) {
    assert!(r.flush(Duration::from_secs(20)).await, "the queue drains");
}

// ---------------------------------------------------------------- the normal flow

#[tokio::test]
async fn s02_a_call_is_recorded_receiving_then_pages_then_final() {
    let store = memory_store().await;
    let sleeps = Arc::new(RecordedSleeps::default());
    let rec = recorder(Arc::new(StoreSink(store.clone())), &sleeps, None);

    rec.record(&started("c1@gw"));
    settle(&rec).await;
    let row = store.get_by_call_id("c1@gw").await.unwrap().unwrap();
    assert_eq!(row.fax_status, FaxStatus::Receiving);
    assert_eq!(row.pages, 0);
    assert_eq!(row.trunk, "audiocodes");
    assert_eq!(
        (row.ani.as_deref(), row.ani_source.as_deref()),
        (Some("0200000001"), Some("from"))
    );
    assert_eq!(
        (row.dnis.as_deref(), row.dnis_source.as_deref()),
        (Some("0200000002"), Some("ruri"))
    );
    assert!(row.ended_at.is_none() && row.hangup_code.is_none());

    for n in 1..=3 {
        rec.record(&page("c1@gw", n));
        settle(&rec).await;
        let row = store.get_by_call_id("c1@gw").await.unwrap().unwrap();
        assert_eq!(
            (row.fax_status, row.pages),
            (FaxStatus::Receiving, n as i32)
        );
    }

    rec.record(&fin(finished_ok("c1@gw", 3)));
    settle(&rec).await;
    let row = store.get_by_call_id("c1@gw").await.unwrap().unwrap();
    assert_eq!(row.fax_status, FaxStatus::Received);
    assert_eq!(row.pages, 3);
    assert_eq!(row.hangup_code.as_deref(), Some("completed"));
    assert_eq!(row.hangup_direction, Some(HangupDirection::Local));
    assert_eq!(row.duration_sec, Some(63));
    assert_eq!(row.t30_code, Some(0));
    assert_eq!(row.remote_ident.as_deref(), Some("0212345678"));
    assert_eq!(row.resolution.as_deref(), Some("fine"));
    assert_eq!(row.compression.as_deref(), Some("MMR"));
    assert_eq!(row.bit_rate, Some(14_400));
    assert!(row.ecm, "the negotiated ECM reaches the database");
    // paths are stored relative to data_dir, sizes as reported by the session
    assert_eq!(row.tiff_path.as_deref(), Some("faxes/2026/10/21/c1.tif"));
    assert_eq!(row.pdf_path.as_deref(), Some("faxes/2026/10/21/c1.pdf"));
    assert_eq!((row.tiff_size, row.pdf_size), (Some(12_345), Some(23_456)));
    assert_eq!(row.ended_at.unwrap().timestamp(), (T0 + 64) as i64);
    assert!(
        sleeps.waits().is_empty(),
        "a healthy database means no waiting"
    );
    let st = rec.stats();
    assert_eq!((st.failed, st.retries, st.dropped, st.queued), (0, 0, 0, 0));
    assert_eq!(st.written, 5);
    rec.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn s02_a_late_or_reordered_page_update_never_moves_the_count_back() {
    let store = memory_store().await;
    let sleeps = Arc::new(RecordedSleeps::default());
    let rec = recorder(Arc::new(StoreSink(store.clone())), &sleeps, None);
    rec.record(&started("c2@gw"));
    rec.record(&page("c2@gw", 2));
    rec.record(&page("c2@gw", 1));
    settle(&rec).await;
    let row = store.get_by_call_id("c2@gw").await.unwrap().unwrap();
    assert_eq!(row.pages, 2);
    // a page update after the final one cannot reopen the row
    rec.record(&fin(finished_ok("c2@gw", 2)));
    rec.record(&page("c2@gw", 3));
    settle(&rec).await;
    let row = store.get_by_call_id("c2@gw").await.unwrap().unwrap();
    assert_eq!((row.fax_status, row.pages), (FaxStatus::Received, 2));
    rec.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn c04_a_call_refused_before_it_existed_is_not_recorded() {
    let store = memory_store().await;
    let sleeps = Arc::new(RecordedSleeps::default());
    let rec = recorder(Arc::new(StoreSink(store.clone())), &sleeps, None);
    // what the controller emits for a source outside every trunk (unknown_source = reject):
    // a Finished with UnknownSource and no Started before it
    let mut r = finished_ok("scan@198.51.100.99", 0);
    r.fax = None;
    r.answered_at = None;
    r.numbers = Numbers::default();
    r.outcome = CallOutcome::Failed {
        reason: FailureReason::UnknownSource,
    };
    for _ in 0..3 {
        rec.record(&fin(r.clone()));
    }
    // a refused call that did exist (no media port) IS recorded
    let mut busy = r.clone();
    busy.call_id = "busy@gw".into();
    busy.outcome = CallOutcome::Failed {
        reason: FailureReason::NoMediaPort,
    };
    rec.record(&fin(busy));
    settle(&rec).await;
    let all = store
        .list(&store::ListFilter::default(), store::PageRequest::default())
        .await
        .unwrap();
    assert_eq!(all.items.len(), 1, "{:?}", all.items);
    assert_eq!(all.items[0].call_id, "busy@gw");
    assert_eq!(all.items[0].hangup_code.as_deref(), Some("rejected"));
    assert_eq!(all.items[0].sip_status, Some(503));
    assert_eq!(all.items[0].fax_status, FaxStatus::NoFax);
    assert!(
        store
            .get_by_call_id("scan@198.51.100.99")
            .await
            .unwrap()
            .is_none()
    );
    rec.shutdown(Duration::from_secs(5)).await;
}

// ---------------------------------------------------------------- outage and retry (S11)

#[test]
fn s11_the_backoff_schedule_doubles_and_is_capped() {
    let p = RetryPolicy {
        initial: Duration::from_secs(1),
        max_delay: Duration::from_secs(60),
        max_attempts: 8,
    };
    let secs: Vec<u64> = (1..=9).map(|n| p.delay(n).as_secs()).collect();
    assert_eq!(secs, vec![1, 2, 4, 8, 16, 32, 60, 60, 60]);
    // a huge failure count must not overflow
    assert_eq!(p.delay(u32::MAX), Duration::from_secs(60));
    assert_eq!(RetryPolicy::default().max_attempts, 8);
}

#[tokio::test]
async fn s11_writes_survive_a_short_outage_with_exponential_backoff() {
    let store = memory_store().await;
    let sleeps = Arc::new(RecordedSleeps::default());
    // the database is down for the first 3 calls (the INSERT is tried 4 times)
    let flaky = Flaky::new(store.clone(), 3);
    let rec = recorder(flaky.clone(), &sleeps, None);
    rec.record(&started("c3@gw"));
    settle(&rec).await;
    assert_eq!(
        sleeps.waits(),
        vec![1, 2, 4],
        "1 s, 2 s, 4 s between the attempts"
    );
    assert_eq!(flaky.calls.load(Ordering::SeqCst), 4);
    let row = store
        .get_by_call_id("c3@gw")
        .await
        .unwrap()
        .expect("written after the outage");
    assert_eq!(row.fax_status, FaxStatus::Receiving);
    let st = rec.stats();
    assert_eq!((st.retries, st.failed, st.written), (3, 0, 1));
    rec.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn s11_a_lost_insert_is_made_up_by_the_final_write() {
    let store = memory_store().await;
    let sleeps = Arc::new(RecordedSleeps::default());
    // the INSERT exhausts its 5 attempts (the database is down the whole time) ...
    let flaky = Flaky::new(store.clone(), 5);
    let spill = tempfile::tempdir().unwrap();
    let rec = recorder(flaky.clone(), &sleeps, Some(spill.path()));
    rec.record(&started("c4@gw"));
    settle(&rec).await;
    assert_eq!(rec.stats().failed, 1);
    assert_eq!(sleeps.waits(), vec![1, 2, 4, 8], "4 waits for 5 attempts");
    assert!(store.get_by_call_id("c4@gw").await.unwrap().is_none());
    // ... the page updates have no row to update and are skipped ...
    rec.record(&page("c4@gw", 1));
    // ... and the final write (database back) inserts the complete row
    rec.record(&fin(finished_ok("c4@gw", 1)));
    settle(&rec).await;
    let row = store.get_by_call_id("c4@gw").await.unwrap().unwrap();
    assert_eq!((row.fax_status, row.pages), (FaxStatus::Received, 1));
    assert_eq!(
        row.ani.as_deref(),
        Some("0200000001"),
        "the numbers come with the final write"
    );
    assert_eq!(rec.stats().spilled, 0, "nothing to restore later");
    rec.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn s11_when_the_final_write_gives_up_result_json_is_kept_for_backfill() {
    let store = memory_store().await;
    let sleeps = Arc::new(RecordedSleeps::default());
    let flaky = Flaky::new(store.clone(), 1_000);
    let spill = tempfile::tempdir().unwrap();
    let rec = recorder(flaky.clone(), &sleeps, Some(spill.path()));
    rec.record(&started("c5@gw"));
    rec.record(&fin(finished_ok("c5@gw", 2)));
    settle(&rec).await;
    let st = rec.stats();
    assert_eq!((st.failed, st.written, st.spilled), (2, 0, 1));
    let file = spill.path().join("c5@gw").join("result.json");
    assert!(file.is_file(), "result.json is written for the lost final");
    // a capture-written result.json is not overwritten
    std::fs::write(&file, "original").unwrap();
    rec.record(&fin(finished_ok("c5@gw", 2)));
    settle(&rec).await;
    assert_eq!(std::fs::read_to_string(&file).unwrap(), "original");

    // the database comes back: backfill restores the call from that file
    flaky.set_down(0);
    std::fs::write(
        &file,
        serde_json::to_string_pretty(&finished_ok("c5@gw", 2)).unwrap(),
    )
    .unwrap();
    let rep = backfill(&*store, spill.path(), Path::new("/srv/fax/data")).await;
    assert_eq!(
        (rep.scanned, rep.inserted, rep.updated),
        (1, 1, 0),
        "{rep:?}"
    );
    let row = store.get_by_call_id("c5@gw").await.unwrap().unwrap();
    assert_eq!((row.fax_status, row.pages), (FaxStatus::Received, 2));
    // and a second run changes nothing
    let rep = backfill(&*store, spill.path(), Path::new("/srv/fax/data")).await;
    assert_eq!((rep.inserted, rep.updated, rep.already_recorded), (0, 0, 1));
    rec.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn s11_while_degraded_every_job_gets_one_attempt_so_the_queue_drains() {
    let store = memory_store().await;
    let sleeps = Arc::new(RecordedSleeps::default());
    let flaky = Flaky::new(store.clone(), 1_000_000);
    let rec = recorder(flaky.clone(), &sleeps, None);
    rec.record(&started("d0@gw")); // exhausts 5 attempts: 4 waits
    settle(&rec).await;
    assert_eq!(sleeps.waits().len(), 4);
    // 20 more jobs, database still down: one attempt each, no further waiting
    for i in 0..20 {
        rec.record(&started(&format!("d{}@gw", i + 1)));
    }
    settle(&rec).await;
    assert_eq!(sleeps.waits().len(), 4, "no more waits once degraded");
    assert_eq!(flaky.calls.load(Ordering::SeqCst), 5 + 20);
    assert_eq!(rec.stats().failed, 21);
    // the database returns: the next job is written (and the retrying mode is back)
    flaky.set_down(0);
    rec.record(&started("back@gw"));
    settle(&rec).await;
    assert!(store.get_by_call_id("back@gw").await.unwrap().is_some());
    flaky.set_down(2);
    rec.record(&started("again@gw"));
    settle(&rec).await;
    assert!(store.get_by_call_id("again@gw").await.unwrap().is_some());
    assert_eq!(sleeps.waits().len(), 6, "retrying again after the recovery");
    rec.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn s11_an_error_that_is_not_transient_is_not_retried() {
    let store = memory_store().await;
    let sleeps = Arc::new(RecordedSleeps::default());
    let flaky = Flaky::new(store.clone(), 0);
    flaky.hard.store(1, Ordering::SeqCst);
    let rec = recorder(flaky.clone(), &sleeps, None);
    rec.record(&started("h1@gw"));
    settle(&rec).await;
    assert!(sleeps.waits().is_empty());
    assert_eq!(flaky.calls.load(Ordering::SeqCst), 1);
    assert_eq!(rec.stats().failed, 1);
    rec.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn s11_a_full_queue_drops_updates_but_never_the_final_write() {
    let store = memory_store().await;
    let sleeps = Arc::new(RecordedSleeps::default());
    let mut cfg = RecorderConfig::new("/srv/fax/data".into());
    cfg.sleeper = sleeps.clone();
    cfg.max_queue = 3;
    // the worker is held up by a database that is down, so the queue fills
    let flaky = Flaky::new(store.clone(), 10_000);
    let rec = Recorder::start(flaky.clone(), cfg);
    for i in 0..10 {
        rec.record(&started(&format!("q{i}@gw")));
    }
    rec.record(&fin(finished_ok("q0@gw", 1)));
    // let the database come back before the worker drains
    flaky.set_down(0);
    settle(&rec).await;
    let st = rec.stats();
    assert!(st.dropped >= 1, "{st:?}");
    let row = store.get_by_call_id("q0@gw").await.unwrap();
    assert!(
        row.is_some_and(|r| r.fax_status == FaxStatus::Received),
        "the final write of q0 is never dropped"
    );
    rec.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn s11_shutdown_drains_what_is_queued_and_stops_waiting_for_a_dead_database() {
    let store = memory_store().await;
    let sleeps = Arc::new(RecordedSleeps::default());
    let rec = recorder(Arc::new(StoreSink(store.clone())), &sleeps, None);
    rec.record(&started("z1@gw"));
    rec.record(&fin(finished_ok("z1@gw", 1)));
    rec.shutdown(Duration::from_secs(5)).await;
    let row = store.get_by_call_id("z1@gw").await.unwrap().unwrap();
    assert_eq!(row.fax_status, FaxStatus::Received);
    // events after shutdown are ignored, not a panic
    rec.record(&started("late@gw"));

    // with a dead database a shutdown makes one attempt per job and spills the final
    let spill = tempfile::tempdir().unwrap();
    let flaky = Flaky::new(store.clone(), 1_000_000);
    let rec = recorder(flaky.clone(), &sleeps, Some(spill.path()));
    rec.record(&fin(finished_ok("z2@gw", 1)));
    rec.shutdown(Duration::from_secs(5)).await;
    assert!(spill.path().join("z2@gw").join("result.json").is_file());
}

// ---------------------------------------------------------------- mapping

#[test]
fn paths_are_stored_relative_to_the_data_dir() {
    let d = Path::new("/srv/fax/data");
    assert_eq!(
        relative_to_data_dir("/srv/fax/data/faxes/a.tif", d),
        "faxes/a.tif"
    );
    // another installation's absolute path: cut at the faxes directory
    assert_eq!(
        relative_to_data_dir("/home/fax/data/faxes/2026/10/21/x.tif", d),
        "faxes/2026/10/21/x.tif"
    );
    // a path with no faxes directory is kept as it is (the purge job refuses it later)
    assert_eq!(relative_to_data_dir("/etc/passwd", d), "/etc/passwd");
    assert_eq!(relative_to_data_dir("faxes/", d), "faxes/");
}

#[test]
fn mapping_cuts_values_to_their_column_widths() {
    let mut r = finished_ok(&"x".repeat(300), 1);
    r.trunk = "t".repeat(100);
    r.numbers.ani = Some("9".repeat(80));
    r.fax.as_mut().unwrap().remote_ident = Some("r".repeat(50));
    let new = new_fax_of(&r);
    assert_eq!(new.call_id.chars().count(), 255);
    assert_eq!(new.trunk.chars().count(), 64);
    assert_eq!(new.ani.unwrap().chars().count(), 64);
    let f = fax_final(&r, Path::new("/srv/fax/data"));
    assert_eq!(f.remote_ident.unwrap().chars().count(), 32);
    // a long failure detail fits the 255 character text column
    let mut r = finished_ok("long@gw", 0);
    r.fax = None;
    r.outcome = CallOutcome::Failed {
        reason: FailureReason::Internal {
            detail: "d".repeat(1000),
        },
    };
    assert_eq!(
        fax_final(&r, Path::new("/x"))
            .hangup_text
            .unwrap()
            .chars()
            .count(),
        255
    );
}

// ---------------------------------------------------------------- backfill from real files

#[tokio::test]
async fn s11_backfill_reads_the_field_result_json_of_an_older_version() {
    // the F1 field capture was written by 0.1.1: no numbers, no ECM fields, no file sizes,
    // absolute paths of the field machine
    const F1: &str = include_str!(
        "../../../tools/fax-sim/tests/traces/audiocodes-m1000-6.80A-8page-fail.result.json"
    );
    let calls = tempfile::tempdir().unwrap();
    let d = calls.path().join("71865083527201022173@192.0.2.10");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(d.join("result.json"), F1).unwrap();
    // a broken file and a directory without result.json do not stop the run
    let bad = calls.path().join("broken");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("result.json"), "{ not json").unwrap();
    std::fs::create_dir_all(calls.path().join("empty")).unwrap();

    let store = memory_store().await;
    let rep = backfill(&*store, calls.path(), Path::new("/home/fax/data")).await;
    assert_eq!(
        (rep.scanned, rep.inserted, rep.errors.len()),
        (2, 1, 1),
        "{rep:?}"
    );
    assert!(rep.errors[0].contains("broken"), "{:?}", rep.errors);
    let row = store
        .get_by_call_id("71865083527201022173@192.0.2.10")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.fax_status, FaxStatus::Partial);
    assert_eq!(row.pages, 1);
    assert_eq!(row.hangup_code.as_deref(), Some("t30_failed"));
    assert_eq!(row.hangup_direction, Some(HangupDirection::Local));
    assert_eq!(row.t30_code, Some(4));
    assert_eq!(row.trunk, "audiocode");
    // ANI / DNIS from the From / To URIs of the old file
    assert_eq!(
        (row.ani.as_deref(), row.ani_source.as_deref()),
        (Some("0200000001"), Some("from"))
    );
    assert_eq!(
        (row.dnis.as_deref(), row.dnis_source.as_deref()),
        (Some("0200000002"), Some("to"))
    );
    assert_eq!(
        row.tiff_path.as_deref(),
        Some("faxes/2026/10/21/71865083527201022173@192.0.2.10.tif")
    );
    assert_eq!(row.tiff_size, None, "old files carry no size");
    assert!(!row.ecm);
    assert_eq!(row.compression.as_deref(), Some("MR"));
    assert_eq!(row.duration_sec, Some(32)); // 03:42:02.267 -> 03:42:34.426
}

#[tokio::test]
async fn s11_backfill_finishes_a_row_that_was_left_receiving() {
    let store = memory_store().await;
    let sleeps = Arc::new(RecordedSleeps::default());
    let rec = recorder(Arc::new(StoreSink(store.clone())), &sleeps, None);
    rec.record(&started("stuck@gw")); // the process died before the final write
    settle(&rec).await;
    rec.shutdown(Duration::from_secs(5)).await;
    let calls = tempfile::tempdir().unwrap();
    let d = calls.path().join("stuck@gw");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("result.json"),
        serde_json::to_string(&finished_ok("stuck@gw", 4)).unwrap(),
    )
    .unwrap();
    let rep = backfill(&*store, calls.path(), Path::new("/srv/fax/data")).await;
    assert_eq!((rep.inserted, rep.updated), (0, 1), "{rep:?}");
    let row = store.get_by_call_id("stuck@gw").await.unwrap().unwrap();
    assert_eq!((row.fax_status, row.pages), (FaxStatus::Received, 4));
    // a directory that does not exist is an error entry, not a panic
    let rep = backfill(&*store, &calls.path().join("nope"), Path::new("/x")).await;
    assert_eq!(rep.scanned, 0);
    assert_eq!(rep.errors.len(), 1);
}

#[test]
fn result_json_round_trips_through_serde() {
    let r = finished_ok("rt@gw", 2);
    let json = serde_json::to_string(&r).unwrap();
    let back: CallResult = serde_json::from_str(&json).unwrap();
    assert_eq!(back, r);
    assert!(json.contains("\"bye_by\":\"local\""));
    assert!(json.contains("\"ecm_ppr_sent\":2"));
}

// ---------------------------------------------------------------- M3f: the finalized hook

fn recorder_with_hook(
    sink: Arc<dyn RecordSink>,
    sleeps: &Arc<RecordedSleeps>,
) -> (Recorder, Arc<Mutex<Vec<i64>>>) {
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    let mut cfg = RecorderConfig::new("/srv/fax/data".into());
    cfg.sleeper = sleeps.clone();
    cfg.retry = RetryPolicy {
        initial: Duration::from_secs(1),
        max_delay: Duration::from_secs(30),
        max_attempts: 3,
    };
    cfg.on_finalized = Some(Arc::new(move |id| s2.lock().unwrap().push(id)));
    (Recorder::start(sink, cfg), seen)
}

#[tokio::test]
async fn f01_the_hook_gets_the_row_id_once_the_final_write_is_in_the_database() {
    let store = memory_store().await;
    let sleeps = Arc::new(RecordedSleeps::default());
    let (rec, seen) = recorder_with_hook(Arc::new(StoreSink(store.clone())), &sleeps);
    rec.record(&started("h1@gw"));
    rec.record(&page("h1@gw", 1));
    settle(&rec).await;
    assert!(
        seen.lock().unwrap().is_empty(),
        "nothing is announced before the final write"
    );
    rec.record(&fin(finished_ok("h1@gw", 2)));
    settle(&rec).await;
    let row = store.get_by_call_id("h1@gw").await.unwrap().unwrap();
    assert_eq!(row.fax_status, FaxStatus::Received);
    assert_eq!(*seen.lock().unwrap(), vec![row.id]);
    rec.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn f02_the_hook_fires_after_a_final_write_that_needed_a_retry() {
    let store = memory_store().await;
    let sleeps = Arc::new(RecordedSleeps::default());
    // the database is down for the first attempt of the final write only
    let flaky = Flaky::new(store.clone(), 0);
    let (rec, seen) = recorder_with_hook(flaky.clone(), &sleeps);
    rec.record(&started("h2@gw"));
    settle(&rec).await;
    flaky.set_down(1);
    rec.record(&fin(finished_ok("h2@gw", 1)));
    settle(&rec).await;
    let row = store.get_by_call_id("h2@gw").await.unwrap().unwrap();
    assert_eq!(row.fax_status, FaxStatus::Received);
    assert_eq!(
        *seen.lock().unwrap(),
        vec![row.id],
        "exactly one announcement, after the retry succeeded"
    );
    rec.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn f03_the_hook_is_not_called_for_a_final_write_that_was_given_up_on() {
    let store = memory_store().await;
    let sleeps = Arc::new(RecordedSleeps::default());
    let flaky = Flaky::new(store.clone(), 1_000);
    let (rec, seen) = recorder_with_hook(flaky, &sleeps);
    rec.record(&fin(finished_ok("h3@gw", 1)));
    settle(&rec).await;
    assert_eq!(rec.stats().failed, 1);
    assert!(seen.lock().unwrap().is_empty());
    assert!(store.get_by_call_id("h3@gw").await.unwrap().is_none());
    rec.shutdown(Duration::from_secs(5)).await;
}

#[tokio::test]
async fn f04_backfill_creates_no_delivery_attempts() {
    // backfill has no hook parameter: restored calls are queued for delivery only by the next
    // `serve` start (catch_up), see docs/test-reports/M3f.md
    let store = memory_store().await;
    let dir = tempfile::tempdir().unwrap();
    let d = dir.path().join("h4@gw");
    std::fs::create_dir_all(&d).unwrap();
    std::fs::write(
        d.join("result.json"),
        serde_json::to_string(&finished_ok("h4@gw", 2)).unwrap(),
    )
    .unwrap();
    let rep = backfill(&*store, dir.path(), Path::new("/srv/fax/data")).await;
    assert_eq!(rep.inserted, 1);
    let row = store.get_by_call_id("h4@gw").await.unwrap().unwrap();
    assert!(
        store.list_deliveries(row.id).await.unwrap().is_empty(),
        "no delivery attempts were created"
    );
}

// ---------------------------------------------------------------- M4b: the written hook

#[tokio::test]
async fn f03_every_write_that_reaches_the_database_is_announced_with_the_row_id() {
    use fax_in::recorder::RecordWritten;
    let store = memory_store().await;
    let sleeps = Arc::new(RecordedSleeps::default());
    let seen = Arc::new(Mutex::new(Vec::new()));
    let s2 = seen.clone();
    let mut cfg = RecorderConfig::new("/srv/fax/data".into());
    cfg.sleeper = sleeps.clone();
    cfg.on_written = Some(Arc::new(move |w: &RecordWritten| {
        s2.lock().unwrap().push(w.clone())
    }));
    let rec = Recorder::start(Arc::new(StoreSink(store.clone())), cfg);
    rec.record(&started("w1@gw"));
    rec.record(&page("w1@gw", 1));
    rec.record(&page("w1@gw", 2));
    rec.record(&fin(finished_ok("w1@gw", 2)));
    settle(&rec).await;
    let id = store.get_by_call_id("w1@gw").await.unwrap().unwrap().id;
    let c = "w1@gw".to_string();
    assert_eq!(
        *seen.lock().unwrap(),
        vec![
            RecordWritten::Created {
                id,
                call_id: c.clone()
            },
            RecordWritten::Pages {
                id,
                call_id: c.clone(),
                pages: 1
            },
            RecordWritten::Pages {
                id,
                call_id: c.clone(),
                pages: 2
            },
            RecordWritten::Finalized { id, call_id: c },
        ]
    );
    rec.shutdown(Duration::from_secs(5)).await;
}
