//! M3b end-to-end cases (I01..I05) through the real `sip-faxd serve` binary and its SQLite
//! database: the simulated gateway (SpanDSP `t38_terminal` on a real UDP socket and clock) calls
//! the server, and the test reads the `faxes` table from a second connection while the call is
//! running and after it ended.
//!
//! * I01: three pages. The table shows `receiving` with climbing `pages` during the call
//!   (non-ECM and ECM), then every field of the final row, sizes equal to the files on disk.
//! * I02: an ordinary phone call (T.38 established, no fax signal): `no_fax` / `no_fax_signal`.
//! * I03: the gateway hangs up in the middle: `partial` / `peer_hangup` / `remote`.
//! * I04: DB columns of the `resolutions` / `ecm` settings (the DIS bits are asserted in
//!   `e2e_m3d.rs`).
//! * I05: the CLI against the data these calls produced: `db migrate`, `db backfill` (after a
//!   call that was received while the database was unreachable), `purge --dry-run`, `purge`.

mod e2e_common;

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use chrono::{Duration as CDuration, Utc};
use e2e_common::*;
use fax_sim::sip_uac::{ReinviteBy, run_call};
use serde_json::Value;
use store::{ConnectOptions, FaxFinal, FaxRecord, FaxStatus, FaxStore, ListFilter, PageRequest};

type Db = Arc<dyn FaxStore>;

/// Opens the server's database from the test (no migration: the server owns the schema).
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
/// the newest row, in order, until it is no longer `receiving`. `on_pages` is told every page
/// count seen (I03 uses it to hang up).
fn observe(
    db: Db,
    on_pages: impl Fn(i32) + Send + 'static,
) -> tokio::task::JoinHandle<Vec<(FaxStatus, i32)>> {
    tokio::spawn(async move {
        let mut seen: Vec<(FaxStatus, i32)> = Vec::new();
        let deadline = std::time::Instant::now() + Duration::from_secs(110);
        while std::time::Instant::now() < deadline {
            if let Some(r) = newest(&db).await {
                let st = (r.fax_status, r.pages);
                if seen.last() != Some(&st) {
                    on_pages(r.pages);
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

/// The rows of the table that are `receiving` states with `pages` pages.
fn receiving(seen: &[(FaxStatus, i32)]) -> Vec<i32> {
    seen.iter()
        .filter(|(s, _)| *s == FaxStatus::Receiving)
        .map(|(_, p)| *p)
        .collect()
}

/// The final row of the call whose Call-ID is `call_id` (waits until the server has written it).
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

fn tiff_pages(path: &std::path::Path) -> usize {
    let o = run("tiffinfo", &[s(path)]);
    String::from_utf8_lossy(&o.stdout)
        .matches("TIFF Directory")
        .count()
}

// ------------------------------------------------------------------------------ I01

/// One three-page call; returns after every field of the final row was checked.
async fn i01(tag: &str, trunk: &str, ecm: bool, pages_n: usize) {
    let f = faxd(tag, 2, trunk, "");
    let db = open(&f.db_url).await;
    let dir = scratch(&format!("src-{tag}"));
    // tall fine pages: every page takes long enough for the 10 ms poll to see it arrive
    let pages = specs(pages_n, true, 1500);
    let src = source_tiff(&dir, "src.tif", &pages);
    let mut cfg = uac(&f, ReinviteBy::Gateway, &src, "0000000001");
    cfg.fax.as_mut().unwrap().ecm = ecm;
    let obs = observe(db.clone(), |_| {});
    let rep = run_call(cfg)
        .await
        .unwrap_or_else(|e| panic!("{tag}: {e}\n{}", f.log().join("\n")));
    f.wait_results(1);
    let seen = obs.await.unwrap();
    eprintln!("{tag}: states seen in the database: {seen:?}");

    // --- while it ran
    let rx = receiving(&seen);
    assert!(
        !rx.is_empty(),
        "{tag}: the row was never `receiving`: {seen:?}"
    );
    assert!(
        rx.windows(2).all(|w| w[0] < w[1]),
        "{tag}: pages only climb: {rx:?}"
    );
    assert!(
        rx.iter().any(|p| *p >= 1),
        "{tag}: the table showed at least one page before the end: {rx:?}"
    );
    assert!(
        rx.iter().filter(|p| **p >= 1).count() >= 2 || pages_n < 3,
        "{tag}: with {pages_n} pages the table shows more than one intermediate count: {rx:?}"
    );
    assert_eq!(
        seen.last().copied(),
        Some((FaxStatus::Received, pages_n as i32)),
        "{tag}"
    );

    // --- after it ended: every field
    let res = result_json(&f, &rep.call_id);
    let fax = assert_success(tag, &rep, &res, pages_n, "0000000001", "peer_reinvite");
    let row = final_row(&db, &rep.call_id).await;
    assert_eq!(row.call_id, rep.call_id);
    assert_eq!(row.trunk, "sipp");
    assert_eq!(
        (row.ani.as_deref(), row.ani_source.as_deref()),
        (Some("gw"), Some("from")),
        "From: sip:gw@127.0.0.1"
    );
    assert_eq!(
        (row.dnis.as_deref(), row.dnis_source.as_deref()),
        (Some("fax"), Some("ruri")),
        "Request-URI: sip:fax@..."
    );
    assert_eq!(row.fax_status, FaxStatus::Received);
    assert_eq!(row.pages, pages_n as i32);
    assert_eq!(row.hangup_code.as_deref(), Some("completed"));
    assert_eq!(
        row.hangup_direction.map(|d| d.as_str()),
        Some("local"),
        "the server sends the BYE when the fax is complete"
    );
    assert_eq!(row.t30_code, Some(0));
    assert_eq!(row.remote_ident.as_deref(), Some("0000000001"));
    assert_eq!(row.resolution.as_deref(), Some("fine"));
    assert_eq!(row.bit_rate, Some(14_400));
    assert_eq!(row.ecm, ecm, "{tag}: negotiated ECM");
    assert!(row.sip_status.is_none());
    assert!(row.started_at <= row.answered_at.unwrap());
    assert!(row.answered_at.unwrap() <= row.ended_at.unwrap());
    let secs =
        (row.ended_at.unwrap() - row.answered_at.unwrap()).num_milliseconds() as f64 / 1000.0;
    assert_eq!(
        f64::from(row.duration_sec.unwrap()),
        secs.round(),
        "duration = ended - answered"
    );
    assert!(row.duration_sec.unwrap() >= 1);
    // files: relative paths under data_dir, sizes equal to the bytes on disk
    let tiff_rel = row.tiff_path.clone().expect("tiff_path");
    let pdf_rel = row.pdf_path.clone().expect("pdf_path");
    assert!(
        tiff_rel.starts_with("faxes/") && !tiff_rel.starts_with('/'),
        "{tiff_rel}"
    );
    let tiff = f.data_dir.join(&tiff_rel);
    let pdf = f.data_dir.join(&pdf_rel);
    assert_eq!(
        row.tiff_size,
        Some(std::fs::metadata(&tiff).unwrap().len() as i64)
    );
    assert_eq!(
        row.pdf_size,
        Some(std::fs::metadata(&pdf).unwrap().len() as i64)
    );
    assert_eq!(tiff_pages(&tiff), pages_n);
    assert_eq!(fax["tiff"].as_str().map(PathBuf::from).unwrap(), tiff);
    // result.json carries the negotiation result (M3d review)
    assert_eq!(fax["ecm"], ecm, "{tag}: result.json ecm");
    assert!(fax["ecm_ppr_sent"].is_u64() && fax["ecm_frames_lost"].is_u64());
    assert_eq!(fax["tiff_size"], row.tiff_size.unwrap());
    assert_eq!(fax["pdf_size"], row.pdf_size.unwrap());
    let v = verify_files(tag, &fax, &src, &pages);
    eprintln!("{}", summary(tag, &rep, &res, &v));
    f.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn i01_three_pages_receiving_then_received() {
    i01("m3b-i01", "", false, 3).await;
}

/// The same with ECM: `PageReceived` is sent at every MCF of the ECM procedure too.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn i01_three_pages_with_ecm_receiving_then_received() {
    i01("m3b-i01e", "[trunks.fax]\necm = true\n", true, 3).await;
}

// ------------------------------------------------------------------------------ I02

/// A call that is only a voice call: T.38 is established (the gateway switches on CED / CNG of a
/// fax; here nobody speaks fax at all), the server sends its DIS and nothing ever answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn i02_a_voice_call_is_no_fax_with_no_fax_signal() {
    let tag = "m3b-i02";
    let f = faxd(tag, 2, "", "");
    let db = open(&f.db_url).await;
    let mut cfg = fax_sim::sip_uac::UacConfig::new(f.addr().parse().unwrap(), ReinviteBy::Gateway);
    // far longer than the server's T.30 give-up time (3 x T4 retransmissions of the DIS)
    cfg.hold = Duration::from_secs(90);
    let rep = run_call(cfg)
        .await
        .unwrap_or_else(|e| panic!("{tag}: {e}\n{}", f.log().join("\n")));
    assert!(rep.peer_bye, "the server hung up first: {rep:?}");
    f.wait_results(1);
    let row = final_row(&db, &rep.call_id).await;
    assert_eq!(row.fax_status, FaxStatus::NoFax);
    assert_eq!(row.hangup_code.as_deref(), Some("no_fax_signal"));
    assert_eq!(
        row.hangup_direction.map(|d| d.as_str()),
        Some("local"),
        "we ended it"
    );
    assert!(
        matches!(row.t30_code, Some(1) | Some(2)),
        "no_response_to_dis or t1_expired: {:?}",
        row.t30_code
    );
    assert_eq!(row.pages, 0);
    assert!(row.tiff_path.is_none() && row.pdf_path.is_none() && row.tiff_size.is_none());
    assert!(row.answered_at.is_some());
    assert!(row.duration_sec.unwrap() >= 5, "{:?}", row.duration_sec);
    assert!(row.hangup_text.as_deref().unwrap().contains("傳真訊號"));
    let res = result_json(&f, &rep.call_id);
    assert_eq!(res["outcome"]["status"], "failed", "{res:#}");
    f.stop();
}

// ------------------------------------------------------------------------------ I03

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn i03_the_gateway_hangs_up_in_the_middle_partial_peer_hangup() {
    let tag = "m3b-i03";
    let f = faxd(tag, 2, "", "");
    let db = open(&f.db_url).await;
    let dir = scratch(&format!("src-{tag}"));
    let pages = specs(6, true, 1500);
    let src = source_tiff(&dir, "src.tif", &pages);
    let hangup = Arc::new(AtomicBool::new(false));
    let mut cfg = uac(&f, ReinviteBy::Gateway, &src, "0000000001");
    cfg.peer_bye_wait = Duration::ZERO;
    {
        let t = cfg.fax.as_mut().unwrap();
        t.hangup_grace = Duration::from_millis(500);
    }
    cfg.hangup = Some(hangup.clone());
    // hang up as soon as the table says two pages are in
    let h = hangup.clone();
    let obs = observe(db.clone(), move |p| {
        if p >= 2 {
            h.store(true, Ordering::Relaxed);
        }
    });
    let rep = run_call(cfg)
        .await
        .unwrap_or_else(|e| panic!("{tag}: {e}\n{}", f.log().join("\n")));
    f.wait_results(1);
    let seen = obs.await.unwrap();
    eprintln!("{tag}: states seen: {seen:?}");
    assert!(
        receiving(&seen).contains(&2),
        "the table showed two pages while the call was up: {seen:?}"
    );
    let row = final_row(&db, &rep.call_id).await;
    assert_eq!(row.fax_status, FaxStatus::Partial, "{row:?}");
    assert_eq!(row.hangup_code.as_deref(), Some("peer_hangup"));
    assert_eq!(row.hangup_direction.map(|d| d.as_str()), Some("remote"));
    assert!(
        (2..6).contains(&row.pages),
        "2 pages were in when the gateway hung up, never all 6: {}",
        row.pages
    );
    assert_eq!(
        row.t30_code,
        Some(11),
        "T.30 was aborted by the end of the call"
    );
    // the pages received so far are stored and counted
    let tiff = f
        .data_dir
        .join(row.tiff_path.as_deref().expect("tiff of a partial fax"));
    assert_eq!(tiff_pages(&tiff), row.pages as usize);
    assert_eq!(
        row.tiff_size,
        Some(std::fs::metadata(&tiff).unwrap().len() as i64)
    );
    let res = result_json(&f, &rep.call_id);
    assert_eq!(res["bye_by"], "remote");
    assert_eq!(res["fax"]["partial"], true);
    assert_eq!(res["fax"]["pages"], row.pages);
    f.stop();
}

// ------------------------------------------------------------------------------ I04

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn i04_resolutions_and_ecm_reach_the_database_columns() {
    // standard only (+ ECM): a fine sender drops to standard, the row says so
    let tag = "m3b-i04a";
    let f = faxd(
        tag,
        2,
        "[trunks.fax]\nresolutions = [\"standard\"]\necm = true\n",
        "",
    );
    let db = open(&f.db_url).await;
    let dir = scratch(&format!("src-{tag}"));
    let pages = specs(1, true, 260);
    let src = source_tiff(&dir, "src.tif", &pages);
    let mut cfg = uac(&f, ReinviteBy::Gateway, &src, "0000000001");
    {
        let t = cfg.fax.as_mut().unwrap();
        t.ecm = true;
        t.compressions = Some(spandsp_sys::T4_COMPRESSION_T6);
    }
    let rep = run_call(cfg).await.unwrap();
    f.wait_results(1);
    let row = final_row(&db, &rep.call_id).await;
    assert_eq!(row.fax_status, FaxStatus::Received, "{row:?}");
    assert_eq!(row.resolution.as_deref(), Some("standard"));
    assert!(row.ecm);
    assert_eq!(row.compression.as_deref(), Some("MMR"));
    let res = result_json(&f, &rep.call_id);
    assert_eq!(res["fax"]["ecm"], true);
    assert_eq!(res["fax"]["resolution"], "standard");
    f.stop();

    // ECM off (the default): an ECM-capable sender falls back, the row says ecm = false
    let tag = "m3b-i04b";
    let f = faxd(tag, 2, "", "");
    let db = open(&f.db_url).await;
    let dir = scratch(&format!("src-{tag}"));
    let pages = specs(1, false, 260);
    let src = source_tiff(&dir, "src.tif", &pages);
    let mut cfg = uac(&f, ReinviteBy::Gateway, &src, "0000000001");
    cfg.fax.as_mut().unwrap().ecm = true;
    let rep = run_call(cfg).await.unwrap();
    f.wait_results(1);
    let row = final_row(&db, &rep.call_id).await;
    assert_eq!(row.fax_status, FaxStatus::Received);
    assert!(!row.ecm, "the DIS did not offer ECM");
    assert_eq!(row.resolution.as_deref(), Some("standard"));
    f.stop();
}

// ------------------------------------------------------------------------------ I05

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn i05_cli_migrate_purge_and_backfill_after_a_call_with_the_database_down() {
    let tag = "m3b-i05";
    // 1. the server runs with a database that is not there (nothing listens on port 1): the call
    //    is still received, stored and logged (S11, fax-in side)
    let f = faxd_db(tag, 2, "", "postgres://fax:secret@127.0.0.1:1/fax");
    let dir = scratch(&format!("src-{tag}"));
    let pages = specs(2, false, 260);
    let src = source_tiff(&dir, "src.tif", &pages);
    let rep = run_call(uac(&f, ReinviteBy::Gateway, &src, "0000000001"))
        .await
        .unwrap_or_else(|e| panic!("{tag}: {e}\n{}", f.log().join("\n")));
    f.wait_results(1);
    let res = result_json(&f, &rep.call_id);
    let fax = assert_success(tag, &rep, &res, 2, "0000000001", "peer_reinvite");
    verify_files(tag, &fax, &src, &pages);
    let log = f.log().join("\n");
    assert!(log.contains("database not reachable"), "{log}");
    assert!(
        log.contains("database unavailable"),
        "retries are logged: {log}"
    );
    assert!(!log.contains(":secret@"), "no password in the log: {log}");
    let faxd_cfg = f.config_path.clone();
    let calls_dir = f.calls_dir.clone();
    let data_dir = f.data_dir.clone();
    let server_dir = f.dir.clone();
    f.stop();

    // 2. a configuration for the same directories with a real (SQLite) database
    let url = format!("sqlite://{}/restored.db", server_dir.display());
    let text = std::fs::read_to_string(&faxd_cfg).unwrap();
    let text = text.replace("postgres://fax:secret@127.0.0.1:1/fax", &url);
    let cfg2 = server_dir.join("with-db.toml");
    std::fs::write(&cfg2, &text).unwrap();
    // `serve` is not needed again: any Faxd handle can run the CLI, so start none and call the binary
    let cli = |args: &[&str]| {
        let o = std::process::Command::new(interop::faxd_binary())
            .args(args)
            .arg("--config")
            .arg(&cfg2)
            .env_remove("RUST_LOG")
            .output()
            .unwrap();
        (
            o.status.code(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    };

    // db migrate: creates the schema, the second run has nothing to do
    let (code, out, err) = cli(&["db", "migrate"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(
        out.contains("schema version 0 -> 2 (applied 0001, 0002)"),
        "{out}"
    );
    let (_, out, _) = cli(&["db", "migrate"]);
    assert!(out.contains("already at schema version 2"), "{out}");

    // db backfill restores the call that the live server could not write
    let (code, out, err) = cli(&["db", "backfill"]);
    assert_eq!(code, Some(0), "{err}");
    assert!(out.contains("1 result.json found, 1 inserted"), "{out}");
    let db = open(&url).await;
    let row = db
        .get_by_call_id(&rep.call_id)
        .await
        .unwrap()
        .expect("restored");
    assert_eq!(row.fax_status, FaxStatus::Received);
    assert_eq!(row.pages, 2);
    assert_eq!(row.hangup_code.as_deref(), Some("completed"));
    assert_eq!(row.hangup_direction.map(|d| d.as_str()), Some("local"));
    assert_eq!(
        row.ani.as_deref(),
        Some("gw"),
        "from the INVITE, kept in result.json"
    );
    assert_eq!(row.dnis.as_deref(), Some("fax"));
    let tiff_rel = row.tiff_path.clone().unwrap();
    assert!(tiff_rel.starts_with("faxes/"), "{tiff_rel}");
    assert_eq!(
        row.tiff_size,
        Some(std::fs::metadata(data_dir.join(&tiff_rel)).unwrap().len() as i64)
    );
    let (_, out, _) = cli(&["db", "backfill"]);
    assert!(
        out.contains("0 inserted") && out.contains("1 already recorded"),
        "{out}"
    );
    assert!(
        calls_dir
            .join(fax_in::capture::dir_name(&rep.call_id))
            .join("result.json")
            .is_file()
    );

    // purge: nothing is due yet ...
    let (code, out, _) = cli(&["purge", "--dry-run"]);
    assert_eq!(code, Some(0));
    assert!(
        out.contains("files of 0 record(s) are due, 0 record(s) are due for deletion"),
        "{out}"
    );
    // ... then the call is 100 days old: its files are due, its record is not
    let old = FaxFinal {
        ended_at: Utc::now() - CDuration::days(100),
        answered_at: row.answered_at,
        duration_sec: row.duration_sec,
        hangup_direction: row.hangup_direction.unwrap(),
        hangup_code: row.hangup_code.clone().unwrap(),
        hangup_text: row.hangup_text.clone(),
        sip_status: None,
        t30_code: row.t30_code,
        fax_status: row.fax_status,
        pages: row.pages,
        remote_ident: row.remote_ident.clone(),
        resolution: row.resolution.clone(),
        bit_rate: row.bit_rate,
        compression: row.compression.clone(),
        ecm: row.ecm,
        tiff_path: row.tiff_path.clone(),
        tiff_size: row.tiff_size,
        pdf_path: row.pdf_path.clone(),
        pdf_size: row.pdf_size,
    };
    db.finalize(row.id, &old).await.unwrap();
    let (code, out, _) = cli(&["purge", "--dry-run"]);
    assert_eq!(code, Some(0));
    assert!(
        out.contains("files of 1 record(s) are due, 0 record(s) are due for deletion"),
        "{out}"
    );
    assert!(
        data_dir.join(&tiff_rel).is_file(),
        "a dry run deletes nothing"
    );
    // ADR-057: the PNG previews (a full page and a thumbnail per page) are next to the TIFF
    let pngs: Vec<std::path::PathBuf> = (1..=row.pages as u32)
        .flat_map(|p| {
            [store::PngKind::Full, store::PngKind::Thumb]
                .map(|k| data_dir.join(store::png_rel_path(&tiff_rel, p, k)))
        })
        .collect();
    assert!(
        !pngs.is_empty() && pngs.iter().all(|p| p.is_file()),
        "{pngs:?}"
    );
    let (code, out, err) = cli(&["purge"]);
    assert_eq!(code, Some(0), "{err}");
    let files = 2 + pngs.len();
    assert!(
        out.contains(&format!(
            "files of 1 record(s) removed ({files} file(s)), 0 record(s) deleted"
        )),
        "{out}"
    );
    assert!(
        pngs.iter().all(|p| !p.exists()),
        "the purge removed the PNGs"
    );
    assert!(!data_dir.join(&tiff_rel).exists());
    assert!(!data_dir.join(row.pdf_path.as_deref().unwrap()).exists());
    let after = db.get_by_call_id(&rep.call_id).await.unwrap().unwrap();
    assert!(
        after.files_purged_at.is_some(),
        "the record stays, marked purged"
    );
    assert_eq!(after.pages, 2);
    let _: Value = res;
}
