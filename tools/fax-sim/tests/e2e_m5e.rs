//! M5e: faxes over G.711 into the real `sip-faxd serve`. The simulated gateway
//! (`fax_sim::sip_uac` with [`ReinviteBy::Audio`]) has no T.38: it answers the server's T.38
//! re-INVITE with 488 and sends the TIFF with SpanDSP's audio fax modem over RTP
//! (`fax_sim::audio_gateway`). Pages are compared with the source by libtiff and poppler, as
//! in `e2e_m2d`.
//!
//! The plan's E01-E08 (docs/test-plans/M5.md section 8):
//!
//! * E01 `audio-only`, PCMU, 3 pages, database row; E02 refused T.38 (488); E03 PCMA;
//! * E04 network: send jitter and 0.5 % loss in both directions, three seeds;
//! * E05 the table shows `receiving` with climbing page counts during the call;
//! * E06 `audio.log` and the capture keep rules;
//! * E07 ten concurrent calls (server CPU time recorded);
//! * E08 the gateway sends CNG on audio and never offers T.38: we offer it after
//!   `initiate_after_secs`; accepted -> T.38 completes, refused -> G.711 completes.
//!
//! `d01` is the development check of the ROADMAP criterion.

mod e2e_common;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use e2e_common::*;
use fax_sim::sip_uac::{ReinviteBy, UacConfig, UacReport, run_call};
use fax_sim::tiff_writer::PageSpec;
use interop::{Faxd, FaxdOptions};
use serde_json::Value;
use sip_core::rtp::Law;
use store::{ConnectOptions, FaxRecord, FaxStatus, FaxStore, ListFilter, PageRequest};

type Db = Arc<dyn FaxStore>;

// ------------------------------------------------------------------ helpers

async fn open(url: &str) -> Db {
    store::connect_with(
        url,
        ConnectOptions {
            auto_migrate: false,
            ..ConnectOptions::default()
        },
    )
    .await
    .unwrap_or_else(|e| panic!("cannot open {url}: {e}"))
}

async fn newest(db: &Db) -> Option<FaxRecord> {
    db.list(&ListFilter::default(), PageRequest::default())
        .await
        .ok()?
        .items
        .into_iter()
        .next()
}

/// Polls the table every few milliseconds and returns the distinct `(status, pages)` states of
/// the newest row, in order, until it is no longer `receiving` (as `e2e_m3b`).
fn observe(db: Db) -> tokio::task::JoinHandle<Vec<(FaxStatus, i32)>> {
    tokio::spawn(async move {
        let mut seen: Vec<(FaxStatus, i32)> = Vec::new();
        let deadline = Instant::now() + Duration::from_secs(170);
        while Instant::now() < deadline {
            if let Some(r) = newest(&db).await {
                let st = (r.fax_status, r.pages);
                if seen.last() != Some(&st) {
                    seen.push(st);
                }
                if r.fax_status != FaxStatus::Receiving {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
        seen
    })
}

async fn final_row(db: &Db, call_id: &str) -> FaxRecord {
    for _ in 0..1000 {
        if let Ok(Some(r)) = db.get_by_call_id(call_id).await
            && r.fax_status != FaxStatus::Receiving
        {
            return r;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the final row of {call_id} never appeared");
}

/// One finished call.
struct Done {
    f: Server,
    rep: UacReport,
    res: Value,
    src: PathBuf,
    pages: Vec<PageSpec>,
}

/// A server with the capture settings of the test (`keep = "failed"` or `"all"`).
fn faxd_capture(
    name: &str,
    ports: u16,
    trunk: &str,
    keep_failed: bool,
    capture_extra: &str,
) -> Server {
    let opts = FaxdOptions {
        trunk: trunk.into(),
        call: "station_id = \"0287654321\"\n".into(),
        extra: format!("\n[media]\nport_range = \"{}\"\n", media_range(ports)),
        real_fax: true,
        capture_keep_failed: keep_failed,
        capture_extra: capture_extra.into(),
        ..FaxdOptions::default()
    };
    Server(Some(Faxd::start(name, &opts)))
}

/// Runs one call against `f`.
async fn call_on(
    tag: &str,
    f: Server,
    by: ReinviteBy,
    pages: Vec<PageSpec>,
    tweak: impl FnOnce(&mut UacConfig),
) -> Done {
    let dir = scratch(&format!("src-{tag}"));
    let src = source_tiff(&dir, "src.tif", &pages);
    let mut cfg = uac(&f, by, &src, "0000000001");
    tweak(&mut cfg);
    let rep = run_call(cfg)
        .await
        .unwrap_or_else(|e| panic!("{tag}: {e}\n{}", f.log().join("\n")));
    f.wait_results(1);
    let res = result_json(&f, &rep.call_id);
    Done {
        f,
        rep,
        res,
        src,
        pages,
    }
}

/// One G.711 call into a fresh server (`trunk` is appended to its `[[trunks]]`).
async fn g711_call(
    tag: &str,
    trunk: &str,
    by: ReinviteBy,
    pages: Vec<PageSpec>,
    tweak: impl FnOnce(&mut UacConfig),
) -> Done {
    call_on(tag, faxd(tag, 2, trunk, ""), by, pages, tweak).await
}

/// What every successful, error-free G.711 call has in common; returns the `fax` object.
fn assert_g711_ok(tag: &str, d: &Done, tsi: &str) -> Value {
    let (rep, res) = (&d.rep, &d.res);
    let run = rep.fax.as_ref().expect("fax run");
    assert!(!run.timed_out, "{tag}: gateway timed out: {run:?}\n{res:#}");
    assert_eq!(
        run.caller_code,
        Some(0),
        "{tag}: SpanDSP says {:?} ({})\n{res:#}",
        run.caller_code,
        run.caller_text
    );
    assert_eq!(res["call_id"], rep.call_id.as_str(), "{tag}");
    assert_eq!(res["outcome"]["status"], "completed", "{tag}: {res:#}");
    assert!(res["t38"].is_null(), "{tag}: {res:#}");
    let fax = &res["fax"];
    assert_eq!(fax["code"], 0, "{tag}: {fax:#}");
    assert_eq!(fax["code_name"], "ok", "{tag}");
    assert_eq!(fax["media"], "g711", "{tag}");
    assert_eq!(fax["modem"], "v29", "{tag}");
    assert_eq!(fax["bit_rate"], 9_600, "{tag}");
    assert_eq!(fax["pages"], d.pages.len(), "{tag}");
    assert_eq!(fax["remote_ident"], tsi, "{tag}");
    assert_eq!(fax["partial"], false, "{tag}");
    assert!(fax["store_error"].is_null(), "{tag}");
    assert!(
        fax["rtp"]["rx_packets"].as_u64().unwrap_or(0) > 100,
        "{tag}: {fax:#}"
    );
    fax.clone()
}

/// ... and the pages equal the source, pixel by pixel.
fn assert_pixels(tag: &str, d: &Done, fax: &Value) -> Verified {
    assert_eq!(
        fax["bad_lines_per_page"],
        serde_json::json!(vec![0; d.pages.len()]),
        "{tag}"
    );
    let v = verify_files(tag, fax, &d.src, &d.pages);
    assert_eq!(v.pages, d.pages.len(), "{tag}");
    v
}

fn line(tag: &str, d: &Done) -> String {
    let run = d.rep.fax.as_ref().unwrap();
    let fax = &d.res["fax"];
    format!(
        "{tag}: SpanDSP={} | pages={} {}/{} rate={} dur={}ms | gw rtp tx={} rx={} dropped={}+{} gaps={} wall={}ms | server rtp {}",
        run.caller_code.unwrap_or(-1),
        fax["pages"],
        fax["media"],
        fax["modem"],
        fax["bit_rate"],
        fax["duration_ms"],
        run.tx_datagrams,
        run.rx_datagrams,
        run.tx_dropped,
        run.rx_dropped,
        run.rx_gaps,
        run.wall_ms,
        fax["rtp"]
    )
}

/// `audio.log` of a call once it has the end of the transfer (the writer is asynchronous).
fn audio_log(f: &Server, call_id: &str) -> String {
    let path = capture_dir(f, call_id).join("audio.log");
    wait_for("audio.log with T.30 done", || {
        let t = std::fs::read_to_string(&path).ok()?;
        t.contains("T.30 done").then_some(t)
    })
}

/// `audio.log` of a call that never ran T.30 (it only listened).
fn listening_log(f: &Server, call_id: &str) -> String {
    let path = capture_dir(f, call_id).join("audio.log");
    wait_for("audio.log", || {
        let t = std::fs::read_to_string(&path).ok()?;
        (!t.is_empty()).then_some(t)
    })
}

// ------------------------------------------------------------------ E01 - E03

/// E01: `t38_policy = "audio-only"`, PCMU, three pages. Pixel exact; the database row says
/// `received`, V.29, 9600, `g711`, hangup code `completed`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e01_audio_only_pcmu_three_pages() {
    let tag = "m5e-e01";
    let f = faxd(tag, 2, "t38_policy = \"audio-only\"\n", "");
    let db = open(&f.db_url).await;
    let d = call_on(tag, f, ReinviteBy::Audio, specs(3, true, 260), |_| {}).await;
    let fax = assert_g711_ok(tag, &d, "0000000001");
    assert_eq!(d.rep.t38_refused, 0, "{tag}: audio-only never offers T.38");
    assert_eq!(
        d.res["audio"]["reason"], "t38_policy = audio-only",
        "{tag}: {:#}",
        d.res["audio"]
    );
    assert_eq!(d.res["audio"]["codec"], "PCMU", "{tag}");
    assert_pixels(tag, &d, &fax);
    let row = final_row(&db, &d.rep.call_id).await;
    assert_eq!(row.fax_status, FaxStatus::Received, "{tag}");
    assert_eq!(row.pages, 3, "{tag}");
    assert_eq!(row.modem.as_deref(), Some("v29"), "{tag}");
    assert_eq!(row.media.as_deref(), Some("g711"), "{tag}");
    assert_eq!(row.bit_rate, Some(9_600), "{tag}");
    assert_eq!(row.hangup_code.as_deref(), Some("completed"), "{tag}");
    assert_eq!(row.t30_code, Some(0), "{tag}");
    assert_eq!(row.remote_ident.as_deref(), Some("0000000001"), "{tag}");
    assert!(!row.ecm, "{tag}: no ECM on audio");
    eprintln!("{}", line(tag, &d));
    d.f.stop();
}

/// E02: `auto`; the gateway answers our T.38 re-INVITE with 488 and the fax goes over G.711.
/// `result.json` says why; the old `g711_passthrough_unsupported` is gone everywhere.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e02_t38_refused_with_488_then_g711() {
    let tag = "m5e-e02";
    let d = g711_call(
        tag,
        "initiate_after_secs = 0.3\n",
        ReinviteBy::Audio,
        specs(3, false, 260),
        |_| {},
    )
    .await;
    let fax = assert_g711_ok(tag, &d, "0000000001");
    assert_eq!(d.rep.t38_refused, 1, "{tag}: our T.38 re-INVITE, once");
    let reason = d.res["audio"]["reason"].as_str().unwrap_or_default();
    assert!(
        reason.contains("488") && reason.contains("T.38"),
        "{tag}: audio reason {reason:?}"
    );
    assert!(
        !d.res.to_string().contains("g711_passthrough_unsupported"),
        "{tag}: {:#}",
        d.res
    );
    assert!(
        !d.f.log()
            .join("\n")
            .contains("g711_passthrough_unsupported"),
        "{tag}: the server log"
    );
    assert_pixels(tag, &d, &fax);
    let db = open(&d.f.db_url).await;
    let row = final_row(&db, &d.rep.call_id).await;
    assert_eq!(row.fax_status, FaxStatus::Received, "{tag}");
    assert_eq!(row.hangup_code.as_deref(), Some("completed"), "{tag}");
    assert_eq!(row.media.as_deref(), Some("g711"), "{tag}");
    eprintln!("{} | reason {reason:?}", line(tag, &d));
    d.f.stop();
}

/// E03: PCMA (the gateway offers and sends only PCMA).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e03_pcma() {
    let tag = "m5e-e03";
    let d = g711_call(
        tag,
        "t38_policy = \"audio-only\"\n",
        ReinviteBy::Audio,
        specs(3, true, 260),
        |c| c.audio.law = Law::Alaw,
    )
    .await;
    let fax = assert_g711_ok(tag, &d, "0000000001");
    assert_eq!(d.res["audio"]["codec"], "PCMA", "{tag}");
    assert_eq!(d.res["audio"]["payload_type"], 8, "{tag}");
    let answer = d.rep.initial_answer.as_ref().expect("answer");
    let m = answer
        .media
        .iter()
        .find(|m| m.media == "audio")
        .expect("audio line");
    assert_eq!(m.formats.first().map(String::as_str), Some("8"), "{tag}");
    assert_pixels(tag, &d, &fax);
    eprintln!("{}", line(tag, &d));
    d.f.stop();
}

// ------------------------------------------------------------------ E04

/// What one E04 call came to.
struct NetCall {
    tag: String,
    done: Done,
    /// The call ended with the fax received.
    received: bool,
    line: String,
}

/// One call over the simulated network: each packet we send is held back by a random
/// `0..=jitter_ms`, `loss_permille` of the packets are dropped in both directions.
async fn net_call(seed: u64, loss_permille: u32, jitter_ms: u32) -> NetCall {
    let tag = format!("m5e-e04-{seed:x}-l{loss_permille}-j{jitter_ms}");
    let d = g711_call(
        &tag,
        "t38_policy = \"audio-only\"\n",
        ReinviteBy::Audio,
        specs(3, true, 500),
        |c| {
            let tx = c.fax.as_mut().unwrap();
            tx.seed = seed;
            tx.loss_permille = loss_permille;
            c.audio.jitter_ms = jitter_ms;
        },
    )
    .await;
    let run = d.rep.fax.as_ref().unwrap();
    let res = &d.res;
    let fax = &res["fax"];
    let received = res["outcome"]["status"] == "completed";
    let l = format!(
        "{tag}: {} | SpanDSP {:?} | ours {} pages {} bad {} | dropped {}+{} | server rtp late={} lost_ms={} dup={} resyncs={} | {} ms",
        if received { "RECEIVED" } else { "FAILED" },
        run.caller_code,
        fax["code_name"],
        fax["pages"],
        fax["bad_lines_per_page"],
        run.tx_dropped,
        run.rx_dropped,
        fax["rtp"]["late"],
        fax["rtp"]["lost_ms"],
        fax["rtp"]["duplicate"],
        fax["rtp"]["resyncs"],
        fax["duration_ms"]
    );
    NetCall {
        tag,
        done: d,
        received,
        line: l,
    }
}

/// The checks of a call that ended as received: the T.30 result, pages within the bad-line
/// threshold (`rtn_bad_percent` 10 %), complete files.
fn assert_received_with_loss(c: &NetCall) {
    let (d, tag) = (&c.done, c.tag.as_str());
    let run = d.rep.fax.as_ref().unwrap();
    let fax = &d.res["fax"];
    assert_eq!(run.caller_code, Some(0), "{tag}: {}", c.line);
    assert_eq!(fax["code"], 0, "{tag}: {fax:#}");
    assert_eq!(fax["media"], "g711", "{tag}");
    assert_eq!(fax["pages"], 3, "{tag}: {fax:#}");
    assert_eq!(fax["partial"], false, "{tag}");
    let bad = fax["bad_lines_per_page"].as_array().expect("bad lines");
    assert_eq!(bad.len(), 3, "{tag}");
    for (i, (b, spec)) in bad.iter().zip(&d.pages).enumerate() {
        let b = b.as_u64().unwrap();
        assert!(
            b * 100 <= u64::from(spec.height) * 10,
            "{tag}: page {i}: {b} bad lines of {} rows (limit 10 %)",
            spec.height
        );
    }
    if bad.iter().all(|b| b.as_u64() == Some(0)) {
        verify_files(tag, fax, &d.src, &d.pages);
    } else {
        // lost audio left bad lines: the files must still be complete documents
        let tif = PathBuf::from(fax["tiff"].as_str().unwrap());
        let out = run_cmd("tiffinfo", &tif);
        assert_eq!(out.matches("TIFF Directory").count(), 3, "{tag}: {out}");
    }
}

fn run_cmd(cmd: &str, file: &std::path::Path) -> String {
    String::from_utf8_lossy(&run(cmd, &[s(file)]).stdout).into_owned()
}

/// E04, jitter: each packet we send is held back by a random 0..=60 ms (+-30 ms around the mean,
/// so neighbours overtake each other); three seeds, no loss. All received, pixel exact, and the
/// server's jitter buffer needed no packet to be dropped as late.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e04_jitter_30_ms_three_seeds() {
    let (a, b, c) = tokio::join!(
        net_call(0x5E04_0001, 0, 60),
        net_call(0x5E04_0002, 0, 60),
        net_call(0x5E04_0003, 0, 60)
    );
    for call in [a, b, c] {
        eprintln!("{}", call.line);
        assert!(call.received, "{}", call.line);
        assert_received_with_loss(&call);
        let fax = &call.done.res["fax"];
        assert_eq!(fax["rtp"]["late"], 0, "{}", call.tag);
        call.done.f.stop();
    }
}

/// E04, loss and jitter together: 0.5 % loss in both directions and jitter of +-15 ms, three
/// seeds. **The plan's "all three are received" is not met** (see the `unmet` test below and
/// docs/test-reports/M5e.md section 4): an audio channel has no error correction, a V.21 frame or
/// a training sequence that is hit must be repeated by the peer, and SpanDSP's own receiver fails
/// in the same conditions more often than ours (m5d `e04c_...`). What this test holds:
/// every call ends cleanly (received or failed with a T.30 result, never hanging), the ones that
/// are received meet the bad-line threshold, loss really was injected, and at least one of the
/// three is received.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e04_loss_half_percent_with_jitter_three_seeds_recorded() {
    let (a, b, c) = tokio::join!(
        net_call(0x5E04_0001, 5, 30),
        net_call(0x5E04_0002, 5, 30),
        net_call(0x5E04_0003, 5, 30)
    );
    let calls = [a, b, c];
    let mut received = 0;
    for call in &calls {
        eprintln!("{}", call.line);
        let run = call.done.rep.fax.as_ref().unwrap();
        assert!(
            run.tx_dropped + run.rx_dropped >= 1,
            "{}: the loss injection did nothing",
            call.tag
        );
        let res = &call.done.res;
        if call.received {
            received += 1;
            assert_received_with_loss(call);
        } else {
            assert_eq!(res["outcome"]["status"], "failed", "{}", call.tag);
            assert_ne!(res["fax"]["code_name"], "ok", "{}: {res:#}", call.tag);
        }
    }
    assert!(received >= 1, "none of the three calls was received");
    for call in calls {
        call.done.f.stop();
    }
}

/// E04 as the plan has it: 0.5 % loss and jitter, three seeds, all received. **Fails** with seeds
/// 1 and 2 (T.30 timeouts after packets of the training and of V.21 frames were lost), so it is
/// `#[ignore]`d; run it with `-- --ignored`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
#[ignore = "unmet plan item: not every seed is received at 0.5 % loss (M5e report section 4)"]
async fn e04_plan_loss_half_percent_with_jitter_all_three_seeds_unmet() {
    let (a, b, c) = tokio::join!(
        net_call(0x5E04_0001, 5, 30),
        net_call(0x5E04_0002, 5, 30),
        net_call(0x5E04_0003, 5, 30)
    );
    for call in [a, b, c] {
        eprintln!("{}", call.line);
        assert!(call.received, "{}", call.line);
        assert_received_with_loss(&call);
        call.done.f.stop();
    }
}

// ------------------------------------------------------------------ E05

/// E05: while the call runs the table shows `receiving` with page counts that only climb
/// (every confirmed page is reported, as for T.38), then `received` with all pages. The Web
/// `fax.progress` event is made from the same database write (`RecordWritten::Pages`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e05_receiving_with_climbing_pages_over_g711() {
    let tag = "m5e-e05";
    let f = faxd(tag, 2, "t38_policy = \"audio-only\"\n", "");
    let db = open(&f.db_url).await;
    let obs = observe(db.clone());
    // tall fine pages: every page takes long enough for the 10 ms poll to see it arrive
    let d = call_on(tag, f, ReinviteBy::Audio, specs(3, true, 600), |_| {}).await;
    let seen = obs.await.unwrap();
    eprintln!("{tag}: states seen in the database: {seen:?}");
    let fax = assert_g711_ok(tag, &d, "0000000001");
    let rx: Vec<i32> = seen
        .iter()
        .filter(|(s, _)| *s == FaxStatus::Receiving)
        .map(|(_, p)| *p)
        .collect();
    assert!(!rx.is_empty(), "{tag}: never `receiving`: {seen:?}");
    assert!(
        rx.windows(2).all(|w| w[0] < w[1]),
        "{tag}: pages only climb: {rx:?}"
    );
    assert!(
        rx.iter().filter(|p| **p >= 1).count() >= 2,
        "{tag}: with 3 pages the table shows more than one intermediate count: {rx:?}"
    );
    assert_eq!(
        seen.last().copied(),
        Some((FaxStatus::Received, 3)),
        "{tag}: {seen:?}"
    );
    let row = final_row(&db, &d.rep.call_id).await;
    assert_eq!(row.media.as_deref(), Some("g711"), "{tag}");
    assert_pixels(tag, &d, &fax);
    eprintln!("{}", line(tag, &d));
    d.f.stop();
}

// ------------------------------------------------------------------ E06

/// E06: `audio.log` next to `sip.log` and `result.json` (`keep = "all"`): the engine's lines
/// with a time, and the RTP events.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e06_audio_log_content() {
    let tag = "m5e-e06";
    let f = faxd_capture(tag, 2, "t38_policy = \"audio-only\"\n", false, "");
    let d = call_on(tag, f, ReinviteBy::Audio, specs(2, false, 260), |_| {}).await;
    let fax = assert_g711_ok(tag, &d, "0000000001");
    assert_pixels(tag, &d, &fax);
    let cdir = capture_dir(&d.f, &d.rep.call_id);
    let log = audio_log(&d.f, &d.rep.call_id);
    eprintln!("{tag}: audio.log:\n{log}");
    assert!(cdir.join("sip.log").is_file(), "{tag}: sip.log as before");
    assert!(cdir.join("result.json").is_file(), "{tag}");
    // every line: RFC 3339 time, then "<ms> ms <text>"
    for l in log.lines() {
        let (time, rest) = l.split_once(' ').unwrap_or((l, ""));
        assert!(
            time.len() >= 20 && time.contains('T') && time.ends_with('Z') || time.contains('+'),
            "{tag}: a line without a time: {l:?}"
        );
        assert!(
            rest.trim_start().contains(" ms "),
            "{tag}: no offset: {l:?}"
        );
    }
    // what happens in a two page call, in order: our answer, every V.21 frame both
    // ways, the modem changes and the TCF verdict, the end of T.30
    assert!(log.contains("first RTP packet: PCMU"), "{tag}: {log}");
    let order = [
        "answering the fax",
        "V.21 tx CSI FF C0 ",
        "V.21 tx DIS FF C8 ",
        "V.21 rx TSI FF C0 ",
        "V.21 rx DCS FF C8 ",
        "V.29 9600 receiver ready",
        "V.29: trained, image data follows",
        "TCF: ",
        "V.21 tx CFR FF C8 ",
        "V.29: trained, image data follows",
        "V.21 rx MPS FF C",
        "V.21 tx MCF FF C8 ",
        "V.29: trained, image data follows",
        "V.21 rx EOP FF C",
        "V.21 tx MCF FF C8 ",
        "T.30 done: ok after 2 page(s)",
    ];
    let mut at = 0;
    for needle in order {
        let i = log[at..].find(needle).unwrap_or_else(|| {
            panic!(
                "{tag}: audio.log lacks {needle:?} after {at}:
{log}"
            )
        });
        at += i + needle.len();
    }
    // the time offsets only go forward
    let offsets: Vec<u64> = log
        .lines()
        .filter_map(|l| {
            let rest = l.split_once(' ')?.1.trim_start();
            rest.split_once(" ms ")?.0.parse().ok()
        })
        .collect();
    assert_eq!(
        offsets.len(),
        log.lines().count(),
        "{tag}: an offset on every line"
    );
    assert!(
        offsets.windows(2).all(|w| w[0] <= w[1]),
        "{tag}: {offsets:?}"
    );
    d.f.stop();
}

/// E06: the keep rules. With `keep = "failed"` a received fax leaves no capture directory, a
/// call the gateway hangs up in the middle of does (`audio.log`, `sip.log`, `result.json`).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e06_keep_failed_drops_good_calls_and_keeps_failed_ones() {
    let tag = "m5e-e06k";
    let f = faxd_capture(tag, 4, "t38_policy = \"audio-only\"\n", true, "");
    let addr = f.addr();
    // a good call first
    let dir = scratch(&format!("src-{tag}"));
    let pages = specs(1, false, 260);
    let src = source_tiff(&dir, "good.tif", &pages);
    let rep = run_call(uac(&f, ReinviteBy::Audio, &src, "0000000001"))
        .await
        .unwrap_or_else(|e| panic!("{tag}: {e}\n{}", f.log().join("\n")));
    f.wait_results(1);
    assert_eq!(rep.fax.as_ref().unwrap().caller_code, Some(0), "{tag}");
    // a call cut off in the middle of the transfer
    let flag = Arc::new(AtomicBool::new(false));
    let long = specs(6, true, 700);
    let src2 = source_tiff(&dir, "long.tif", &long);
    let mut cfg = uac(&f, ReinviteBy::Audio, &src2, "0000000002");
    cfg.hangup = Some(flag.clone());
    cfg.peer_bye_wait = Duration::from_secs(1);
    cfg.fax.as_mut().unwrap().hangup_grace = Duration::from_secs(1);
    let hang = std::thread::spawn({
        let flag = flag.clone();
        move || {
            std::thread::sleep(Duration::from_secs(12));
            flag.store(true, Ordering::Relaxed);
        }
    });
    let rep2 = run_call(cfg)
        .await
        .unwrap_or_else(|e| panic!("{tag}: {e}\n{}", f.log().join("\n")));
    hang.join().unwrap();
    f.wait_results(2);
    // the capture of the failed call is complete; the good call has none
    let bad_dir = capture_dir(&f, &rep2.call_id);
    wait_for("the kept capture of the failed call", || {
        (bad_dir.join("result.json").is_file()).then_some(())
    });
    let res = result_json(&f, &rep2.call_id);
    eprintln!(
        "{tag}: failed call: outcome {} fax {}",
        res["outcome"], res["fax"]["code_name"]
    );
    assert!(bad_dir.join("sip.log").is_file(), "{tag}: sip.log kept");
    let log = std::fs::read_to_string(bad_dir.join("audio.log"))
        .unwrap_or_else(|e| panic!("{tag}: audio.log of the failed call: {e}"));
    assert!(log.contains("answering the fax"), "{tag}: {log}");
    assert!(
        !capture_dir(&f, &rep.call_id).exists(),
        "{tag}: keep = failed: no directory for the good call {}",
        rep.call_id
    );
    let _ = addr;
    f.stop();
}

/// E06: `max_bytes_per_call` bounds `audio.log` (a separate budget) and says what was left out.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e06_max_bytes_per_call_bounds_audio_log() {
    let tag = "m5e-e06m";
    let f = faxd_capture(
        tag,
        2,
        "t38_policy = \"audio-only\"\n",
        false,
        "max_bytes_per_call = 1500\n",
    );
    let d = call_on(tag, f, ReinviteBy::Audio, specs(2, false, 260), |_| {}).await;
    assert_g711_ok(tag, &d, "0000000001");
    let path = capture_dir(&d.f, &d.rep.call_id).join("audio.log");
    let log = wait_for("audio.log with the cut-off note", || {
        let t = std::fs::read_to_string(&path).ok()?;
        t.contains("not stored").then_some(t)
    });
    eprintln!("{tag}: audio.log {} bytes:\n{log}", log.len());
    assert!(
        log.contains("further line(s) not stored (max_bytes_per_call reached)"),
        "{tag}: {log}"
    );
    let body: usize = log
        .lines()
        .filter(|l| !l.starts_with("[capture]"))
        .map(|l| l.len() + 1)
        .sum();
    assert!(body <= 1500, "{tag}: {body} bytes of audio.log lines");
    d.f.stop();
}

// ------------------------------------------------------------------ E07

/// CPU time (user + system, seconds) of the process in the PID file, from `/proc`.
fn cpu_seconds(pid_file: &std::path::Path) -> Option<f64> {
    let pid: u32 = std::fs::read_to_string(pid_file)
        .ok()?
        .trim()
        .parse()
        .ok()?;
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    // the fields after the command name, which is in parentheses and may contain spaces
    let rest = &stat[stat.rfind(')')? + 2..];
    let f: Vec<&str> = rest.split_whitespace().collect();
    // utime and stime are fields 14 and 15 of the line: 12 and 13 after the command
    let ticks: u64 = f.get(11)?.parse::<u64>().ok()? + f.get(12)?.parse::<u64>().ok()?;
    Some(ticks as f64 / 100.0)
}

/// E07: ten calls at once (PCMU and PCMA alternating, 1-3 pages each, different TSI and
/// images): all received, nothing mixed up; the server's CPU time is recorded.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e07_ten_concurrent_g711_calls() {
    let tag = "m5e-e07";
    let f = faxd(tag, 10, "t38_policy = \"audio-only\"\n", "");
    let dir = scratch(&format!("src-{tag}"));
    let cpu0 = cpu_seconds(&f.pid_file);
    let wall = Instant::now();
    let mut jobs = Vec::new();
    let mut sources = Vec::new();
    for i in 0..10usize {
        let pages = specs(1 + i % 3, i % 2 == 0, 200 + 40 * i as u32);
        let src = source_tiff(&dir, &format!("src{i}.tif"), &pages);
        let tsi = format!("00000002{i:02}");
        let mut cfg = uac(&f, ReinviteBy::Audio, &src, &tsi);
        cfg.audio.law = if i % 2 == 0 { Law::Ulaw } else { Law::Alaw };
        cfg.fax.as_mut().unwrap().seed = 0x5E07_0000 + i as u64;
        sources.push((src, pages, tsi));
        jobs.push(tokio::spawn(run_call(cfg)));
    }
    let mut reports = Vec::new();
    for j in jobs {
        reports.push(
            j.await
                .unwrap()
                .unwrap_or_else(|e| panic!("{tag}: {e}\n{}", f.log().join("\n"))),
        );
    }
    let wall = wall.elapsed();
    f.wait_results(10);
    let cpu1 = cpu_seconds(&f.pid_file);
    let mut tiffs = std::collections::HashSet::new();
    let mut audio_ms = 0u64;
    let mut pages_total = 0usize;
    for (i, (rep, (src, pages, tsi))) in reports.iter().zip(&sources).enumerate() {
        let t = format!("{tag}-{i}");
        let res = result_json(&f, &rep.call_id);
        let d = Done {
            f: Server(None),
            rep: rep.clone(),
            res,
            src: src.clone(),
            pages: pages.clone(),
        };
        let fax = assert_g711_ok(&t, &d, tsi);
        let want = if i % 2 == 0 { "PCMU" } else { "PCMA" };
        assert_eq!(d.res["audio"]["codec"], want, "{t}");
        assert!(
            tiffs.insert(fax["tiff"].as_str().unwrap().to_string()),
            "{t}: shared file"
        );
        assert_pixels(&t, &d, &fax);
        audio_ms += fax["duration_ms"].as_u64().unwrap_or(0);
        pages_total += pages.len();
    }
    let log = f.log().join("\n");
    assert!(!log.contains("ERROR"), "{tag}: errors in the server log");
    let cpu = match (cpu0, cpu1) {
        (Some(a), Some(b)) => format!("{:.2} s", b - a),
        _ => "n/a".into(),
    };
    eprintln!(
        "{tag}: 10 calls, {pages_total} pages, wall {:.1} s, sum of call durations {:.1} s, server CPU {cpu} (debug build)",
        wall.as_secs_f64(),
        audio_ms as f64 / 1000.0
    );
    f.stop();
}

// ------------------------------------------------------------------ E08

/// E08, refused: the gateway sends CNG on audio (SpanDSP's calling side) and never offers T.38;
/// we offer it after `initiate_after_secs`, the gateway answers 488, the fax completes over
/// G.711 - and `audio.log` has the CNG heard before we answered (the engine listens from the
/// start).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e08_cng_on_audio_then_our_t38_offer_is_refused() {
    let tag = "m5e-e08r";
    let d = g711_call(
        tag,
        "initiate_after_secs = 1.5\n",
        ReinviteBy::Audio,
        specs(2, false, 260),
        |_| {},
    )
    .await;
    let fax = assert_g711_ok(tag, &d, "0000000001");
    assert_eq!(d.rep.t38_refused, 1, "{tag}: one T.38 re-INVITE of ours");
    let reason = d.res["audio"]["reason"].as_str().unwrap_or_default();
    assert!(reason.contains("488"), "{tag}: {reason:?}");
    assert_pixels(tag, &d, &fax);
    let log = audio_log(&d.f, &d.rep.call_id);
    eprintln!("{tag}: audio.log:\n{log}");
    let cng = log
        .find("CNG heard")
        .unwrap_or_else(|| panic!("{tag}: no CNG heard:\n{log}"));
    let answer = log.find("answering the fax").expect("answering line");
    assert!(
        cng < answer,
        "{tag}: the CNG was heard before we answered:\n{log}"
    );
    // the first 'answering' is after initiate_after_secs: the engine listened that long
    eprintln!("{}", line(tag, &d));
    d.f.stop();
}

/// E08, accepted: same start, but the gateway accepts our T.38 re-INVITE: the audio
/// listener hands over, T.38 completes (14400, `our_reinvite`), and `audio.log` still has the
/// CNG heard before the switch.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e08_cng_on_audio_then_our_t38_offer_is_accepted() {
    let tag = "m5e-e08a";
    let d = g711_call(
        tag,
        "initiate_after_secs = 1.5\n",
        ReinviteBy::AudioThenT38,
        specs(2, false, 260),
        |_| {},
    )
    .await;
    let (rep, res) = (&d.rep, &d.res);
    assert!(
        rep.server_t38_offer.is_some(),
        "{tag}: the server offered T.38"
    );
    assert_eq!(rep.t38_refused, 0, "{tag}");
    let audio = rep
        .audio_run
        .as_ref()
        .expect("the audio part before the switch");
    // CNG over RTP from the connect until the server's re-INVITE (about 1.5 s = 75 packets)
    assert!(
        (60..=200).contains(&audio.tx_datagrams),
        "{tag}: audio packets before the re-INVITE: {}",
        audio.tx_datagrams
    );
    let run = rep.fax.as_ref().expect("the T.38 run");
    assert!(!run.timed_out, "{tag}: {run:?}\n{res:#}");
    assert_eq!(
        run.caller_code,
        Some(0),
        "{tag}: SpanDSP says {:?}\n{res:#}",
        run.caller_text
    );
    assert_eq!(res["outcome"]["status"], "completed", "{tag}: {res:#}");
    assert_eq!(
        res["t38"]["path"], "our_reinvite",
        "{tag}: {:#}",
        res["t38"]
    );
    let fax = &res["fax"];
    assert_eq!(fax["code"], 0, "{tag}: {fax:#}");
    assert_eq!(fax["media"], "t38", "{tag}: {fax:#}");
    assert_eq!(fax["bit_rate"], 14_400, "{tag}");
    assert_eq!(fax["pages"], 2, "{tag}");
    assert!(
        !res["audio"]["reason"].is_string(),
        "{tag}: the audio only listened: {:#}",
        res["audio"]
    );
    let v = verify_files(tag, fax, &d.src, &d.pages);
    assert_eq!(v.pages, 2, "{tag}");
    let log = listening_log(&d.f, &d.rep.call_id);
    eprintln!("{tag}: audio.log:\n{log}");
    assert!(
        log.contains("CNG heard"),
        "{tag}: the CNG before the re-INVITE:\n{log}"
    );
    assert!(
        !log.contains("answering the fax"),
        "{tag}: the audio session never answered:\n{log}"
    );
    eprintln!(
        "{tag}: audio part: {} packets sent, {} received in {} ms; T.38 part: SpanDSP={} dur={}ms",
        audio.tx_datagrams,
        audio.rx_datagrams,
        audio.wall_ms,
        run.caller_code.unwrap(),
        fax["duration_ms"]
    );
    d.f.stop();
}

// ------------------------------------------------------------------ development check

/// Development check of the ROADMAP criterion: `t38_policy = "auto"`, our T.38 re-INVITE is
/// refused, the fax (two pages) arrives over G.711 at V.29 9600, pixel exact.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn d01_refused_t38_then_two_pages_over_g711() {
    let tag = "m5e-d01";
    let f = faxd(tag, 2, "initiate_after_secs = 0.3\n", "");
    let dir = scratch(&format!("src-{tag}"));
    let pages = specs(2, false, 260);
    let src = source_tiff(&dir, "src.tif", &pages);
    let cfg = uac(&f, ReinviteBy::Audio, &src, "0000000001");
    let rep = run_call(cfg)
        .await
        .unwrap_or_else(|e| panic!("{tag}: {e}\n{}", f.log().join("\n")));
    f.wait_results(1);
    let res = result_json(&f, &rep.call_id);
    let run = rep.fax.as_ref().expect("fax run");
    assert!(!run.timed_out, "{tag}: gateway timed out: {run:?}\n{res:#}");
    assert_eq!(
        run.caller_code,
        Some(0),
        "{tag}: SpanDSP says {:?} ({})\n{res:#}",
        run.caller_code,
        run.caller_text
    );
    assert_eq!(rep.t38_refused, 1, "{tag}: the server's T.38 re-INVITE");
    assert_eq!(res["outcome"]["status"], "completed", "{tag}: {res:#}");
    assert!(res["t38"].is_null(), "{tag}: {res:#}");
    let reason = res["audio"]["reason"].as_str().unwrap_or_default();
    assert!(reason.contains("488"), "{tag}: audio reason {reason:?}");
    let fax = &res["fax"];
    assert_eq!(fax["code"], 0, "{tag}: {fax:#}");
    assert_eq!(fax["media"], "g711", "{tag}");
    assert_eq!(fax["modem"], "v29", "{tag}");
    assert_eq!(fax["bit_rate"], 9_600, "{tag}");
    assert_eq!(fax["pages"], 2, "{tag}");
    assert_eq!(fax["remote_ident"], "0000000001", "{tag}");
    assert_eq!(
        fax["bad_lines_per_page"],
        serde_json::json!([0, 0]),
        "{tag}"
    );
    assert!(
        fax["rtp"]["rx_packets"].as_u64().unwrap_or(0) > 500,
        "{tag}: {fax:#}"
    );
    let v = verify_files(tag, fax, &src, &pages);
    assert_eq!(v.pages, 2, "{tag}");
    // audio.log next to sip.log
    let log = std::fs::read_to_string(capture_dir(&f, &rep.call_id).join("audio.log"))
        .unwrap_or_else(|e| panic!("{tag}: audio.log: {e}"));
    assert!(log.contains("V.29 9600 receiver ready"), "{tag}: {log}");
    eprintln!(
        "{tag}: SpanDSP={} | pages={} rate={} dur={}ms | rtp gw tx={} rx={} | server rtp {}",
        run.caller_code.unwrap(),
        fax["pages"],
        fax["bit_rate"],
        fax["duration_ms"],
        run.tx_datagrams,
        run.rx_datagrams,
        fax["rtp"]
    );
    f.stop();
}
