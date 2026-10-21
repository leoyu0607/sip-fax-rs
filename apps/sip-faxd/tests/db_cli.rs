//! I05 (host side): the database subcommands of the real `sip-faxd` executable on a real SQLite
//! file: `db migrate`, `db backfill`, `purge [--dry-run]`, `config check`, and what `serve` does at
//! start with the database (S10, S08, S11). The same executable runs under SpanDSP in
//! `tools/fax-sim/tests/e2e_m3b.rs`.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use chrono::{Duration as CDuration, Utc};
use store::{FaxFinal, FaxStatus, FaxStore, HangupDirection, ListFilter, NewFax, PageRequest};

const F1: &str = include_str!(
    "../../../tools/fax-sim/tests/traces/audiocodes-m1000-6.80A-8page-fail.result.json"
);

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_sip-faxd"))
}

/// A fresh, empty directory per test.
fn scratch(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("i05-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn url_of(dir: &Path) -> String {
    format!("sqlite://{}", dir.join("fax.db").display()).replace('\\', "/")
}

/// A configuration whose database, data and capture directories are all under `dir`.
fn config(dir: &Path, extra: &str) -> PathBuf {
    let text = format!(
        "[sip]\nbind = \"127.0.0.1:0\"\n\
         [storage]\ndata_dir = {data:?}\n\
         [capture]\ndir = {calls:?}\n\
         [database]\nurl = {url:?}\n\
         [log]\nfile = \"\"\n\
         [service]\npid_file = {pid:?}\nsocket = {sock:?}\n\
         {extra}\n\
         [[trunks]]\nname = \"local\"\nallowed_sources = [\"127.0.0.0/8\"]\n",
        data = dir.join("data").display().to_string().replace('\\', "/"),
        calls = dir.join("calls").display().to_string().replace('\\', "/"),
        url = url_of(dir),
        // the run state belongs to the test, not to the default `run/` next to the executable
        pid = dir
            .join("sip-faxd.pid")
            .display()
            .to_string()
            .replace('\\', "/"),
        sock = dir
            .join("sip-faxd.sock")
            .display()
            .to_string()
            .replace('\\', "/"),
    );
    let p = dir.join("sip-faxd.toml");
    std::fs::write(&p, text).unwrap();
    p
}

fn run(args: &[&str], cfg: &Path) -> Output {
    let mut c = bin();
    c.args(args).arg("--config").arg(cfg);
    c.output().expect("run sip-faxd")
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

async fn open(dir: &Path) -> std::sync::Arc<dyn FaxStore> {
    store::connect(&url_of(dir)).await.unwrap()
}

fn new_fax(call_id: &str) -> NewFax {
    NewFax {
        call_id: call_id.into(),
        trunk: "local".into(),
        ani: Some("0933".into()),
        ani_source: Some("from".into()),
        dnis: Some("0255".into()),
        dnis_source: Some("ruri".into()),
        started_at: Utc::now() - CDuration::days(400),
        answered_at: None,
    }
}

fn fin(tiff: &str, pdf: &str, ended_days_ago: i64) -> FaxFinal {
    FaxFinal {
        ended_at: Utc::now() - CDuration::days(ended_days_ago),
        answered_at: None,
        duration_sec: Some(30),
        hangup_direction: HangupDirection::Local,
        hangup_code: "completed".into(),
        hangup_text: None,
        sip_status: None,
        t30_code: Some(0),
        fax_status: FaxStatus::Received,
        pages: 1,
        remote_ident: None,
        resolution: None,
        bit_rate: None,
        compression: None,
        ecm: false,
        tiff_path: Some(tiff.into()),
        tiff_size: Some(3),
        pdf_path: Some(pdf.into()),
        pdf_size: Some(3),
    }
}

// ---------------------------------------------------------------- db migrate

#[tokio::test]
async fn i05_db_migrate_creates_the_schema_and_is_repeatable() {
    let dir = scratch("migrate");
    let cfg = config(&dir, "");
    let o = run(&["db", "migrate"], &cfg);
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        out(&o).contains("database sqlite: schema version 0 -> 1 (applied 0001)"),
        "{}",
        out(&o)
    );
    assert!(dir.join("fax.db").is_file(), "the SQLite file was created");
    let o = run(&["db", "migrate"], &cfg);
    assert!(o.status.success());
    assert!(
        out(&o).contains("already at schema version 1, nothing to do"),
        "{}",
        out(&o)
    );
    let st = open(&dir).await;
    assert_eq!(st.schema_version().await.unwrap(), 1);
}

#[tokio::test]
async fn i05_db_migrate_refuses_a_database_from_a_newer_version() {
    let dir = scratch("newer");
    let cfg = config(&dir, "");
    assert!(run(&["db", "migrate"], &cfg).status.success());
    store::test_record_schema_version(&url_of(&dir), 99)
        .await
        .unwrap();
    let o = run(&["db", "migrate"], &cfg);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("newer"), "{}", err(&o));
    assert!(err(&o).contains("99"), "{}", err(&o));
    // and `serve` does not start on it either (before it opens any socket)
    let o = run(&["serve"], &cfg);
    assert_eq!(o.status.code(), Some(1), "{}", err(&o));
    assert!(err(&o).contains("newer"), "{}", err(&o));
}

#[tokio::test]
async fn i05_auto_migrate_false_stops_the_start_on_an_empty_database() {
    let dir = scratch("nomigrate");
    let cfg = config(&dir, "");
    // `[database]` is already in the file: write a second variant with the switch off
    let text = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace("[database]", "[database]\nauto_migrate = false");
    let off = dir.join("off.toml");
    std::fs::write(&off, text).unwrap();
    let o = run(&["serve"], &off);
    assert_eq!(o.status.code(), Some(1));
    assert!(
        err(&o).contains("db migrate"),
        "tells what to do: {}",
        err(&o)
    );
    // `purge` honours it as well
    let o = run(&["purge", "--dry-run"], &off);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("db migrate"), "{}", err(&o));
}

// ---------------------------------------------------------------- db backfill

#[tokio::test]
async fn i05_db_backfill_restores_calls_from_result_json() {
    let dir = scratch("backfill");
    let cfg = config(&dir, "");
    for id in ["71865083527201022173@192.0.2.10", "second@192.0.2.10"] {
        let d = dir.join("calls").join(id);
        std::fs::create_dir_all(&d).unwrap();
        std::fs::write(
            d.join("result.json"),
            F1.replace("71865083527201022173@192.0.2.10", id),
        )
        .unwrap();
    }
    let o = run(&["db", "backfill"], &cfg);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    assert!(text.contains("2 result.json found, 2 inserted"), "{text}");
    assert!(text.contains("0 error(s)"), "{text}");
    let st = open(&dir).await;
    let all = st
        .list(&ListFilter::default(), PageRequest::default())
        .await
        .unwrap();
    assert_eq!(all.items.len(), 2);
    let row = st
        .get_by_call_id("second@192.0.2.10")
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.fax_status, FaxStatus::Partial);
    assert_eq!(row.pages, 1);
    assert_eq!(row.hangup_code.as_deref(), Some("t30_failed"));
    assert_eq!(row.ani.as_deref(), Some("0200000001"));
    assert_eq!(row.dnis.as_deref(), Some("0200000002"));
    // the field machine's absolute path is stored relative to data_dir
    assert_eq!(
        row.tiff_path.as_deref(),
        Some("faxes/2026/10/21/second@192.0.2.10.tif")
    );
    // a second run finds everything there already
    let o = run(&["db", "backfill"], &cfg);
    assert!(o.status.success());
    assert!(out(&o).contains("2 already recorded"), "{}", out(&o));
    assert!(out(&o).contains("0 inserted"), "{}", out(&o));
    // a corrupt file is reported, the rest still processed, exit code 1
    let bad = dir.join("calls").join("broken");
    std::fs::create_dir_all(&bad).unwrap();
    std::fs::write(bad.join("result.json"), "{").unwrap();
    let o = run(&["db", "backfill"], &cfg);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("broken"), "{}", err(&o));
    assert!(out(&o).contains("1 error(s)"), "{}", out(&o));
    // --dir reads another directory
    let other = scratch("backfill-other");
    let o = bin()
        .args(["db", "backfill", "--dir"])
        .arg(&other)
        .arg("--config")
        .arg(&cfg)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("0 result.json found"), "{}", out(&o));
}

// ---------------------------------------------------------------- purge

#[tokio::test]
async fn i05_purge_dry_run_counts_and_the_real_run_deletes() {
    let dir = scratch("purge");
    let cfg = config(&dir, "[retention]\nfiles_days = 90\nrecords_days = 365\n");
    assert!(run(&["db", "migrate"], &cfg).status.success());
    let faxes = dir.join("data").join("faxes");
    std::fs::create_dir_all(&faxes).unwrap();
    let st = open(&dir).await;
    // old: 100 days (files due), ancient: 400 days (record due), fresh: 10 days (nothing)
    for (id, days) in [("old", 100), ("ancient", 400), ("fresh", 10)] {
        std::fs::write(faxes.join(format!("{id}.tif")), b"tif").unwrap();
        std::fs::write(faxes.join(format!("{id}.pdf")), b"pdf").unwrap();
        let n = st.insert_receiving(&new_fax(id)).await.unwrap();
        st.finalize(
            n,
            &fin(&format!("faxes/{id}.tif"), &format!("faxes/{id}.pdf"), days),
        )
        .await
        .unwrap();
    }
    let o = run(&["purge", "--dry-run"], &cfg);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    assert!(text.contains("files 90 days, records 365 days"), "{text}");
    assert!(
        text.contains(
            "files of 2 record(s) are due, 1 record(s) are due for deletion; nothing was deleted"
        ),
        "{text}"
    );
    assert!(faxes.join("old.tif").is_file() && faxes.join("ancient.pdf").is_file());
    assert_eq!(
        st.list(&ListFilter::default(), PageRequest::default())
            .await
            .unwrap()
            .items
            .len(),
        3
    );

    let o = run(&["purge"], &cfg);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    assert!(
        text.contains("files of 2 record(s) removed (4 file(s)), 1 record(s) deleted"),
        "{text}"
    );
    // (the ancient one is past files_days as well, so its files go first, then its record)
    // the old fax lost its files but keeps its record, marked purged; the ancient record is gone
    assert!(!faxes.join("old.tif").exists() && !faxes.join("old.pdf").exists());
    assert!(!faxes.join("ancient.tif").exists());
    assert!(faxes.join("fresh.tif").is_file() && faxes.join("fresh.pdf").is_file());
    let old = st.get_by_call_id("old").await.unwrap().unwrap();
    assert!(old.files_purged_at.is_some());
    assert!(st.get_by_call_id("ancient").await.unwrap().is_none());
    assert!(
        st.get_by_call_id("fresh")
            .await
            .unwrap()
            .unwrap()
            .files_purged_at
            .is_none()
    );
    // nothing left to do the second time
    let o = run(&["purge"], &cfg);
    assert!(
        out(&o).contains("files of 0 record(s) removed (0 file(s)), 0 record(s) deleted"),
        "{}",
        out(&o)
    );
}

/// S10, daemon side: the `[retention]` values of the file are written on the first start; after
/// that the database decides and the file is ignored.
#[tokio::test]
async fn s10_the_database_wins_over_the_config_file_after_the_first_start() {
    let dir = scratch("s10");
    let cfg = config(
        &dir,
        "[retention]\nfiles_days = 30\nrecords_days = 60\nrun_at = \"04:20\"\n",
    );
    let o = run(&["purge", "--dry-run"], &cfg);
    assert!(o.status.success(), "{}", err(&o));
    assert!(
        out(&o).contains("files 30 days, records 60 days"),
        "first start seeds the settings: {}",
        out(&o)
    );
    let st = open(&dir).await;
    let s = store::purge::load_retention(&*st).await.unwrap();
    assert_eq!(
        (
            s.policy.files_days,
            s.policy.records_days,
            s.run_at.as_str()
        ),
        (30, 60, "04:20")
    );
    // the operator changes the value in the database (what the web UI will do) ...
    st.set_setting("retention.files_days", "7", Some("admin"))
        .await
        .unwrap();
    // ... and a new configuration file with different values does not override it
    let cfg2 = config(
        &dir,
        "[retention]\nfiles_days = 999\nrecords_days = 999\nrun_at = \"01:01\"\n",
    );
    let o = run(&["purge", "--dry-run"], &cfg2);
    assert!(
        out(&o).contains("files 7 days, records 60 days"),
        "{}",
        out(&o)
    );
    let s = store::purge::load_retention(&*st).await.unwrap();
    assert_eq!(s.run_at, "04:20");
}

// ---------------------------------------------------------------- config check

#[test]
fn i05_config_check_shows_database_retention_fax_and_numbers() {
    let dir = scratch("check");
    let text = "[database]\nurl = \"postgres://fax:S3cret!@db.example:5432/faxdb\"\nmax_connections = 9\n\
                [retention]\nfiles_days = 30\nrecords_days = 0\nrun_at = \"02:45\"\n\
                [[trunks]]\nname = \"cht\"\npeer = \"203.0.113.10:5060\"\n\
                [trunks.fax]\nresolutions = [\"fine\"]\necm = true\n\
                [trunks.numbers]\nani_sources = [\"pai\", \"from\"]\ndnis_sources = [\"diversion\", \"ruri\"]\n\
                strip_plus = true\nani_strip_prefix = \"+886\"\n";
    let cfg = dir.join("c.toml");
    std::fs::write(&cfg, text).unwrap();
    let o = bin()
        .args(["config", "check", "--config"])
        .arg(&cfg)
        .output()
        .unwrap();
    assert!(o.status.success(), "{}", err(&o));
    let t = out(&o);
    assert!(
        t.contains("database: postgres://fax:***@db.example:5432/faxdb (postgres), auto_migrate = true, max_connections = 9"),
        "{t}"
    );
    assert!(!t.contains("S3cret"), "the password is never printed: {t}");
    assert!(
        t.contains("retention: files after 30 days, records never, daily run at 02:45"),
        "{t}"
    );
    assert!(t.contains("fax: resolutions fine, ecm on"), "{t}");
    assert!(
        t.contains("numbers: ani from pai>from, dnis from diversion>ruri, strip_plus on, ani_strip_prefix +886, dnis_strip_prefix -"),
        "{t}"
    );
    // defaults are shown too
    let o = bin()
        .args(["config", "check", "--config"])
        .arg(config(&scratch("check2"), ""))
        .output()
        .unwrap();
    let t = out(&o);
    assert!(t.contains("fax: resolutions standard+fine, ecm off"), "{t}");
    assert!(
        t.contains("numbers: ani from pai>rpid>from, dnis from ruri>to, strip_plus off"),
        "{t}"
    );
    assert!(
        t.contains("retention: files after 90 days, records after 365 days, daily run at 03:17"),
        "{t}"
    );
    // bad values are rejected with exit code 2
    for (bad, needle) in [
        ("[trunks.numbers]\nani_sources = [\"contact\"]\n", "contact"),
        ("[trunks.fax]\nresolutions = []\n", "resolutions"),
    ] {
        let p = dir.join("bad.toml");
        std::fs::write(
            &p,
            format!("[[trunks]]\nname = \"t\"\npeer = \"203.0.113.10:5060\"\n{bad}"),
        )
        .unwrap();
        let o = bin()
            .args(["config", "check", "--config"])
            .arg(&p)
            .output()
            .unwrap();
        assert_eq!(o.status.code(), Some(2), "{bad}");
        assert!(err(&o).contains(needle), "{}", err(&o));
    }
}

// ---------------------------------------------------------------- serve at start

/// Starts `serve` and waits for the line that says the SIP sockets are open (or for the exit).
fn start_serve(cfg: &Path) -> (Child, mpsc::Receiver<String>) {
    let mut child = bin()
        .arg("serve")
        .arg("--config")
        .arg(cfg)
        .stderr(Stdio::piped())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    let stderr = child.stderr.take().unwrap();
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for line in BufReader::new(stderr).lines().map_while(Result::ok) {
            if tx.send(line).is_err() {
                break;
            }
        }
    });
    (child, rx)
}

fn wait_for(rx: &mpsc::Receiver<String>, needle: &str) -> String {
    let mut seen = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(l) if l.contains(needle) => return l,
            Ok(l) => seen.push(l),
            Err(_) => panic!("never saw {needle:?}; log so far:\n{}", seen.join("\n")),
        }
    }
}

/// S08, daemon side: a row left `receiving` by a crashed run is closed when the daemon starts.
#[tokio::test]
async fn s08_serve_closes_the_receiving_rows_a_crash_left_behind() {
    let dir = scratch("stale");
    let cfg = config(&dir, "");
    assert!(run(&["db", "migrate"], &cfg).status.success());
    let st = open(&dir).await;
    let a = st
        .insert_receiving(&new_fax("died-with-pages"))
        .await
        .unwrap();
    st.update_pages(a, 2, Utc::now()).await.unwrap();
    st.insert_receiving(&new_fax("died-early")).await.unwrap();
    let (mut child, rx) = start_serve(&cfg);
    wait_for(&rx, "sip-faxd ready");
    child.kill().unwrap();
    let _ = child.wait();
    let r = st.get_by_call_id("died-with-pages").await.unwrap().unwrap();
    assert_eq!((r.fax_status, r.pages), (FaxStatus::Partial, 2));
    assert_eq!(r.hangup_code.as_deref(), Some("shutdown"));
    let r = st.get_by_call_id("died-early").await.unwrap().unwrap();
    assert_eq!(r.fax_status, FaxStatus::Failed);
    assert_eq!(r.hangup_code.as_deref(), Some("shutdown"));
    // the first start also wrote the retention settings
    assert!(
        st.get_setting("retention.files_days")
            .await
            .unwrap()
            .is_some()
    );
}

/// S11, daemon side: a database that is down at start does not stop the daemon from answering.
#[test]
fn s11_serve_starts_even_when_the_database_is_unreachable() {
    let dir = scratch("down");
    let cfg = config(&dir, "");
    // nothing listens on port 1
    let text = std::fs::read_to_string(&cfg)
        .unwrap()
        .replace(&url_of(&dir), "postgres://u:pw@127.0.0.1:1/fax");
    let cfg = dir.join("down.toml");
    std::fs::write(&cfg, text).unwrap();
    let (mut child, rx) = start_serve(&cfg);
    let l = wait_for(&rx, "database not reachable");
    assert!(!l.contains("pw@"), "no password in the log: {l}");
    wait_for(&rx, "sip-faxd ready");
    child.kill().unwrap();
    let _ = child.wait();
}
