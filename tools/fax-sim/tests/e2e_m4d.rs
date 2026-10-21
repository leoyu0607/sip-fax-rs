//! M4d end-to-end cases (M01..M12): the real `sip-faxd` binary as a service. `start` runs it in
//! the background (setsid, PID file with flock), the control socket answers `status` and
//! `stop`, and the simulated gateway (SpanDSP `t38_terminal` on a real UDP socket) sends faxes
//! that the stop sequence has to wait for, or to cut off.
//!
//! * M01/M02/M03: `start` (foreground log, Ctrl+C, `-d`, already running, a start that fails).
//! * M04/M05: `stop` with a fax in progress: 503 for new calls, the fax is completed; a timeout
//!   or `--force` ends it with the pages received so far (`partial` / `shutdown`).
//! * M06: `restart` waits for the call.
//! * M07/M08: `status` and the systemd detection.
//! * M09: SIGTERM to `serve` is the same stop.
//! * M10: the control socket is 0600; another uid is refused.
//! * M11/M12: the log file rotates; `logs -f` follows through it.
//!
//! Linux only (the daemon uses flock, setsid and a unix socket).
#![cfg(unix)]

mod e2e_common;

use std::io::{BufRead, BufReader};
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use e2e_common::*;
use fax_sim::gateway::FaxTx;
use fax_sim::sip_uac::{ReinviteBy, UacConfig, audio_offer, run_call};
use interop::{Faxd, FaxdOptions, Prepared, faxd_cli, faxd_command};
use sip_core::endpoint::{Endpoint, EndpointConfig, InviteError, InviteOptions};
use sip_core::message::SipUri;
use store::{ConnectOptions, FaxRecord, FaxStatus, FaxStore, ListFilter, PageRequest};

type Db = Arc<dyn FaxStore>;

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

/// Waits until the newest row shows at least `pages` confirmed pages.
async fn wait_pages(db: &Db, pages: i32) {
    let deadline = Instant::now() + Duration::from_secs(100);
    while Instant::now() < deadline {
        if let Ok(p) = db
            .list(&ListFilter::default(), PageRequest::default())
            .await
            && p.items.first().is_some_and(|r| r.pages >= pages)
        {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the database never showed {pages} page(s)");
}

fn opts(ports: u16, service: &str, log_file: bool, log_extra: &str) -> FaxdOptions {
    FaxdOptions {
        call: "station_id = \"0287654321\"\n".into(),
        extra: format!("\n[media]\nport_range = \"{}\"\n", media_range(ports)),
        real_fax: true,
        log_file,
        log_extra: log_extra.into(),
        service: service.into(),
        ..FaxdOptions::default()
    }
}

/// A daemon started with `sip-faxd start -d`; stopped (forced) when dropped, so that a failing
/// test leaves nothing running.
struct Daemon {
    prep: Prepared,
}

impl Daemon {
    fn prepare(name: &str, o: &FaxdOptions) -> Daemon {
        Daemon {
            prep: Faxd::prepare(name, o),
        }
    }

    fn start(name: &str, o: &FaxdOptions) -> Daemon {
        let d = Daemon::prepare(name, o);
        let (code, out, err) = faxd_cli(&["start", "-d"], &d.prep.config_path);
        assert_eq!(code, Some(0), "start -d failed:\n{out}\n{err}\n{}", d.log());
        d
    }

    fn addr(&self) -> String {
        format!("127.0.0.1:{}", self.prep.port)
    }

    fn cfg(&self) -> PathBuf {
        self.prep.config_path.clone()
    }

    fn pid(&self) -> u32 {
        std::fs::read_to_string(&self.prep.pid_file)
            .unwrap_or_else(|e| panic!("no pid file: {e}"))
            .trim()
            .parse()
            .unwrap()
    }

    fn log(&self) -> String {
        match &self.prep.log_path {
            Some(p) => std::fs::read_to_string(p).unwrap_or_default(),
            None => String::new(),
        }
    }

    fn cli(&self, args: &[&str]) -> (Option<i32>, String, String) {
        faxd_cli(args, &self.prep.config_path)
    }

    /// `status --json`, parsed; `None` when the service does not run.
    fn status(&self) -> Option<serde_json::Value> {
        let (code, out, _) = self.cli(&["status", "--json"]);
        (code == Some(0)).then(|| serde_json::from_str(&out).expect("status json"))
    }
}

impl Drop for Daemon {
    fn drop(&mut self) {
        let _ = faxd_cli(&["stop", "--force"], &self.prep.config_path);
        let _ = std::fs::remove_file(&self.prep.socket);
    }
}

fn gateway(addr: &str, src: &Path) -> UacConfig {
    let mut cfg = UacConfig::new(addr.parse().unwrap(), ReinviteBy::Gateway);
    let mut tx = FaxTx::new(src.to_path_buf());
    tx.ident = "0000000001".into();
    cfg.fax = Some(tx);
    cfg.wait_for_server = Duration::from_secs(20);
    cfg
}

/// Places a plain call at `addr` and returns the final status it was refused with, or 200.
async fn invite_status(addr: &str) -> u16 {
    let ep = Endpoint::bind(EndpointConfig::new().udp("127.0.0.1:0".parse().unwrap()))
        .await
        .unwrap()
        .0;
    let target: SocketAddr = addr.parse().unwrap();
    let uri = SipUri::parse(&format!("sip:fax@{target}")).unwrap();
    let o = InviteOptions::new(
        target,
        uri.clone(),
        SipUri::parse("sip:probe@127.0.0.1").unwrap(),
        uri,
        audio_offer("127.0.0.1", 40_000),
    );
    match ep.invite(o).await {
        Err(InviteError::Rejected(s)) => s.code(),
        Err(e) => panic!("the probe call failed differently: {e}"),
        Ok(_) => 200,
    }
}

fn wait_until(what: &str, secs: u64, mut f: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while !f() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(50));
    }
}

fn signal(sig: &str, pid: u32) {
    let st = Command::new("kill")
        .arg(format!("-{sig}"))
        .arg(pid.to_string())
        .status()
        .unwrap();
    assert!(st.success(), "kill -{sig} {pid}");
}

fn alive(pid: u32) -> bool {
    Path::new(&format!("/proc/{pid}")).exists()
        && std::fs::read_to_string(format!("/proc/{pid}/stat"))
            .map(|s| !s.contains(") Z "))
            .unwrap_or(false)
}

/// The session id of a process (`/proc/<pid>/stat`, field 6).
fn session_of(pid: u32) -> u32 {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).unwrap();
    stat.rsplit_once(')')
        .unwrap()
        .1
        .split_whitespace()
        .nth(3)
        .unwrap()
        .parse()
        .unwrap()
}

/// `tiffinfo` / `pdfinfo`: the number of pages of the stored files.
fn stored_pages(data_dir: &Path, row: &FaxRecord) -> (usize, usize) {
    let tif = data_dir.join(row.tiff_path.as_deref().expect("tiff_path"));
    let pdf = data_dir.join(row.pdf_path.as_deref().expect("pdf_path"));
    let t = run("tiffinfo", &[s(&tif)]);
    let t = String::from_utf8_lossy(&t.stdout)
        .matches("TIFF Directory")
        .count();
    let p = run("pdfinfo", &[s(&pdf)]);
    let p = String::from_utf8_lossy(&p.stdout)
        .lines()
        .find(|l| l.starts_with("Pages:"))
        .and_then(|l| l.split_whitespace().last()?.parse().ok())
        .expect("Pages:");
    (t, p)
}

fn long_fax(tag: &str, pages: usize) -> PathBuf {
    let dir = scratch(&format!("src-{tag}"));
    source_tiff(&dir, "src.tif", &specs(pages, true, 500))
}

/// A child process whose stdout lines arrive on a channel.
struct Spawned {
    child: Child,
    lines: mpsc::Receiver<String>,
    seen: Vec<String>,
}

impl Spawned {
    fn new(mut cmd: Command) -> Spawned {
        let mut child = cmd
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        let (tx, rx) = mpsc::channel();
        let out = child.stdout.take().unwrap();
        std::thread::spawn(move || {
            for l in BufReader::new(out).lines().map_while(Result::ok) {
                if tx.send(l).is_err() {
                    break;
                }
            }
        });
        Spawned {
            child,
            lines: rx,
            seen: Vec::new(),
        }
    }

    /// Reads lines until one contains `needle` (everything before it is kept in `seen`).
    fn wait_line(&mut self, needle: &str, secs: u64) -> String {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            let left = deadline.saturating_duration_since(Instant::now());
            match self.lines.recv_timeout(left) {
                Ok(l) if l.contains(needle) => {
                    self.seen.push(l.clone());
                    return l;
                }
                Ok(l) => self.seen.push(l),
                Err(_) => panic!(
                    "never saw {needle:?} on stdout; saw:\n{}",
                    self.seen.join("\n")
                ),
            }
        }
    }

    fn drain(&mut self) {
        while let Ok(l) = self.lines.try_recv() {
            self.seen.push(l);
        }
    }

    fn wait_exit(&mut self, secs: u64) -> Option<i32> {
        let deadline = Instant::now() + Duration::from_secs(secs);
        loop {
            if let Some(st) = self.child.try_wait().unwrap() {
                std::thread::sleep(Duration::from_millis(100));
                self.drain();
                return st.code();
            }
            assert!(
                Instant::now() < deadline,
                "the process did not exit; saw:\n{}",
                self.seen.join("\n")
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }
}

impl Drop for Spawned {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

// ------------------------------------------------------------------------------ M01

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m01_start_runs_in_the_background_shows_the_log_and_ctrl_c_leaves_the_service_alone() {
    let d = Daemon::prepare("m4d-m01", &opts(2, "", true, ""));
    let mut start = Spawned::new(faxd_command(&["start"], &d.prep.config_path));
    start.wait_line("sip-faxd started", 60);
    start.wait_line("Showing", 5);
    // the log of the start is shown, from the file the service writes
    start.wait_line("sip-faxd ready", 10);

    // PID file, flock, a session of its own
    let pid = d.pid();
    assert!(alive(pid));
    let (code, out, _) = d.cli(&["status"]);
    assert_eq!(code, Some(0), "{out}");
    assert!(out.contains(&format!("PID:      {pid}")), "{out}");
    assert_ne!(
        session_of(pid),
        session_of(std::process::id()),
        "the service left our session"
    );
    assert_eq!(session_of(pid), pid, "and leads its own (setsid)");
    let before_sock = std::fs::metadata(&d.prep.socket).unwrap();
    assert!(!before_sock.is_dir());

    // Ctrl+C: the display stops, the service does not
    signal("INT", start.child.id());
    assert_eq!(start.wait_exit(10), Some(0), "{:?}", start.seen);
    let said = start.seen.join("\n");
    assert!(
        said.contains("still running in the background") && said.contains("sip-faxd stop"),
        "{said}"
    );
    assert!(said.contains("服務仍在背景執行"), "{said}");
    std::thread::sleep(Duration::from_millis(500));
    assert!(alive(pid), "the service survived the Ctrl+C");
    assert_eq!(d.status().unwrap()["pid"], pid);
    // and it really serves: a stop through the socket works
    let (code, out, err) = d.cli(&["stop"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    wait_until("the process to be gone", 10, || !alive(pid));
    assert!(!d.prep.pid_file.exists() && !d.prep.socket.exists());
}

// ------------------------------------------------------------------------------ M02

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m02_start_d_returns_at_once_and_a_second_start_is_refused_with_the_pid() {
    let d = Daemon::prepare("m4d-m02", &opts(2, "", true, ""));
    interop::faxd_binary(); // built here, not inside the timing below
    let t0 = Instant::now();
    let (code, out, err) = d.cli(&["start", "-d"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    assert!(out.contains("sip-faxd started (pid"), "{out}");
    assert!(
        t0.elapsed() < Duration::from_secs(30),
        "returned when the service was ready"
    );
    assert!(!out.contains("Showing"), "-d shows no log: {out}");
    let pid = d.pid();

    // a second one: refused, says who runs, starts nothing
    let (code, out, err) = d.cli(&["start", "-d"]);
    assert_ne!(code, Some(0));
    assert!(
        err.contains(&format!("already running (pid {pid})")),
        "{out}\n{err}"
    );
    let (code, _, err) = d.cli(&["start"]);
    assert_ne!(code, Some(0), "also without -d");
    assert!(err.contains("already running"), "{err}");
    assert_eq!(d.pid(), pid, "the PID file is still the first one's");
    assert!(alive(pid));

    // a second `serve` by hand cannot take the lock either
    let o = faxd_command(&["serve"], &d.prep.config_path)
        .output()
        .unwrap();
    assert_eq!(o.status.code(), Some(1));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("already running"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(alive(pid), "and did not disturb it");
    assert_eq!(d.status().unwrap()["pid"], pid);
}

// ------------------------------------------------------------------------------ M03

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m03_a_start_that_fails_shows_the_end_of_the_log_and_leaves_no_pid_file() {
    // the SIP port is taken: the service exits during the start
    let port = interop::free_port();
    let _udp = std::net::UdpSocket::bind(("127.0.0.1", port)).unwrap();
    let _tcp = std::net::TcpListener::bind(("127.0.0.1", port)).unwrap();
    let mut o = opts(2, "start_timeout_secs = 20", true, "");
    o.port = Some(port);
    let d = Daemon::prepare("m4d-m03-bind", &o);
    interop::faxd_binary(); // built here, not inside the timing below
    let t0 = Instant::now();
    let (code, out, err) = d.cli(&["start", "-d"]);
    assert_eq!(code, Some(1), "{out}\n{err}");
    assert!(
        t0.elapsed() < Duration::from_secs(15),
        "an early exit is noticed at once, not after the timeout"
    );
    assert!(err.contains("exited during the start"), "{err}");
    assert!(
        err.contains("cannot bind the SIP sockets"),
        "the reason from the log is shown: {err}"
    );
    assert!(err.contains("last 50 lines"), "{err}");
    assert!(!d.prep.pid_file.exists(), "no PID file is left");
    assert!(!d.prep.socket.exists());
    assert_eq!(d.cli(&["status"]).0, Some(3));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m03_a_service_that_never_becomes_ready_is_stopped_after_the_timeout() {
    // the database accepts the connection and never speaks: the service hangs in its start
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dbport = listener.local_addr().unwrap().port();
    let held = Arc::new(Mutex::new(Vec::new()));
    let h2 = held.clone();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            h2.lock().unwrap().push(s);
        }
    });
    let mut o = opts(2, "start_timeout_secs = 2", true, "");
    o.database_url = Some(format!("mysql://fax:pw@127.0.0.1:{dbport}/fax"));
    let d = Daemon::prepare("m4d-m03-hang", &o);
    interop::faxd_binary(); // built here, not inside the timing below
    let t0 = Instant::now();
    let (code, out, err) = d.cli(&["start", "-d"]);
    assert_eq!(code, Some(1), "{out}\n{err}");
    assert!(err.contains("did not answer"), "{err}");
    assert!(t0.elapsed() >= Duration::from_secs(2));
    assert!(
        !d.prep.pid_file.exists(),
        "the half-started service is gone and its PID file with it"
    );
    assert_eq!(d.cli(&["status"]).0, Some(3));
}

// ------------------------------------------------------------------------------ M04

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m04_stop_waits_for_the_fax_in_progress_and_refuses_new_calls() {
    let tag = "m4d-m04";
    let d = Daemon::start(tag, &opts(2, "", true, ""));
    let db = open(&d.prep.db_url).await;
    let pages = specs(5, true, 500);
    let src = source_tiff(&scratch(&format!("src-{tag}")), "src.tif", &pages);
    let fax = tokio::spawn(run_call(gateway(&d.addr(), &src)));
    wait_pages(&db, 1).await;
    let busy = d.status().expect("running");
    assert_eq!(busy["active_calls"], 1, "{busy}");

    let cfg = d.cfg();
    let stop = tokio::task::spawn_blocking(move || faxd_cli(&["stop"], &cfg));
    wait_until("the service to say it is stopping", 20, || {
        d.status().is_some_and(|s| s["state"] == "stopping")
    });
    // a new call now: 503
    assert_eq!(invite_status(&d.addr()).await, 503);
    assert_eq!(invite_status(&d.addr()).await, 503, "every time");
    assert!(!stop.is_finished(), "`stop` is waiting for the fax");

    // the fax goes on to the end
    let rep = fax.await.unwrap().expect("the fax call");
    let run = rep.fax.as_ref().expect("fax run");
    assert_eq!(run.caller_code, Some(0), "SpanDSP: {}", run.caller_text);
    let (code, out, err) = stop.await.unwrap();
    assert_eq!(code, Some(0), "{out}\n{err}");
    assert!(out.contains("Stopping sip-faxd"), "{out}");
    assert!(out.contains("1 call(s) in progress"), "{out}");
    assert!(out.contains("sip-faxd stopped"), "{out}");
    let row = final_row(&db, &rep.call_id).await;
    assert_eq!(row.fax_status, FaxStatus::Received, "{row:?}");
    assert_eq!(row.pages, 5);
    assert_eq!(row.hangup_code.as_deref(), Some("completed"));
    assert_eq!(stored_pages(&d.prep.data_dir, &row), (5, 5));
    // the refused calls are not faxes
    let all = db
        .list(&ListFilter::default(), PageRequest::default())
        .await
        .unwrap();
    assert_eq!(all.items.len(), 1, "{:?}", all.items);
    // everything is cleaned up
    assert!(!d.prep.pid_file.exists() && !d.prep.socket.exists());
    assert_eq!(d.cli(&["status"]).0, Some(3));
    let log = d.log();
    for want in [
        "stopping: no new calls are accepted",
        "call refused (503): the service is stopping",
        "waiting for the calls to end",
        "sip-faxd stopped",
    ] {
        assert!(log.contains(want), "{want:?} missing from the log:\n{log}");
    }
    assert!(
        !log.contains("ending the calls that are still running"),
        "{log}"
    );
    assert!(!log.contains(" ERROR "), "no error in the log:\n{log}");
}

// ------------------------------------------------------------------------------ M05

/// Starts a long fax, waits for the first page and runs `stop <args>`; returns the stop's
/// (code, stdout, seconds), the row and the gateway's call id.
async fn cut_off(tag: &str, service: &str, stop_args: &[&str]) -> (String, f64, FaxRecord, Daemon) {
    let d = Daemon::start(tag, &opts(2, service, true, ""));
    let db = open(&d.prep.db_url).await;
    let src = long_fax(tag, 30);
    let fax = tokio::spawn(run_call(gateway(&d.addr(), &src)));
    wait_pages(&db, 1).await;
    let cfg = d.cfg();
    let args: Vec<String> = stop_args.iter().map(|s| (*s).to_string()).collect();
    let t0 = Instant::now();
    let (code, out, err) = tokio::task::spawn_blocking(move || {
        let a: Vec<&str> = args.iter().map(String::as_str).collect();
        faxd_cli(&a, &cfg)
    })
    .await
    .unwrap();
    let took = t0.elapsed().as_secs_f64();
    assert_eq!(code, Some(0), "{out}\n{err}\n{}", d.log());
    // the gateway notices the BYE (its result does not matter here)
    let _ = tokio::time::timeout(Duration::from_secs(30), fax).await;
    let rows = db
        .list(&ListFilter::default(), PageRequest::default())
        .await
        .unwrap();
    assert_eq!(rows.items.len(), 1);
    let row = final_row(&db, &rows.items[0].call_id).await;
    (out, took, row, d)
}

fn assert_cut_off(row: &FaxRecord, d: &Daemon) {
    assert_eq!(row.fax_status, FaxStatus::Partial, "{row:?}");
    assert_eq!(row.hangup_code.as_deref(), Some("shutdown"), "{row:?}");
    assert_eq!(row.hangup_direction.map(|x| x.as_str()), Some("system"));
    assert!(row.pages >= 1 && row.pages < 30, "pages: {}", row.pages);
    // the pages that were confirmed are on disk, readable by tools that are not ours
    let (tiff, pdf) = stored_pages(&d.prep.data_dir, row);
    assert_eq!((tiff, pdf), (row.pages as usize, row.pages as usize));
    assert_eq!(
        row.tiff_size,
        Some(
            std::fs::metadata(d.prep.data_dir.join(row.tiff_path.as_deref().unwrap()))
                .unwrap()
                .len() as i64
        )
    );
    assert!(d.log().contains("ending the calls that are still running"));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m05_a_stop_that_runs_into_stop_timeout_secs_ends_the_call_and_keeps_the_pages() {
    let (out, took, row, d) = cut_off("m4d-m05-timeout", "stop_timeout_secs = 7", &["stop"]).await;
    assert!(
        (6.0..30.0).contains(&took),
        "it waited for the timeout, not less and not for ever: {took} s\n{out}"
    );
    assert!(out.contains("waiting up to 7 s"), "{out}");
    assert!(
        out.contains("Waiting for 1 call(s) to finish"),
        "5 s report: {out}"
    );
    assert_cut_off(&row, &d);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m05_stop_force_ends_the_call_at_once_and_keeps_the_pages() {
    // the default timeout is 300 s: only --force can make this quick
    let (out, took, row, d) = cut_off("m4d-m05-force", "", &["stop", "--force"]).await;
    assert!(took < 20.0, "forced: {took} s\n{out}");
    assert!(
        out.contains("now: 1 call(s) in progress are ended"),
        "{out}"
    );
    assert_cut_off(&row, &d);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m05_the_timeout_option_overrides_the_configured_one() {
    let (out, took, row, d) = cut_off("m4d-m05-opt", "", &["stop", "--timeout", "4"]).await;
    assert!((3.5..25.0).contains(&took), "{took} s\n{out}");
    assert!(out.contains("waiting up to 4 s"), "{out}");
    assert_cut_off(&row, &d);
}

// ------------------------------------------------------------------------------ M06

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m06_restart_waits_for_the_call_then_starts_a_new_process_with_the_same_settings() {
    let tag = "m4d-m06";
    let d = Daemon::start(tag, &opts(2, "", true, ""));
    let db = open(&d.prep.db_url).await;
    let old = d.pid();
    let before = d.status().unwrap();
    let src = source_tiff(
        &scratch(&format!("src-{tag}")),
        "src.tif",
        &specs(4, true, 500),
    );
    let fax = tokio::spawn(run_call(gateway(&d.addr(), &src)));
    wait_pages(&db, 1).await;

    let cfg = d.cfg();
    let restart = tokio::task::spawn_blocking(move || faxd_cli(&["restart", "-d"], &cfg));
    wait_until("the stop to begin", 20, || {
        d.status().is_some_and(|s| s["state"] == "stopping")
    });
    assert!(alive(old) && !restart.is_finished(), "waiting for the fax");
    let rep = fax.await.unwrap().expect("fax call");
    assert_eq!(rep.fax.as_ref().unwrap().caller_code, Some(0));
    let (code, out, err) = restart.await.unwrap();
    assert_eq!(code, Some(0), "{out}\n{err}");
    assert!(
        out.contains("sip-faxd stopped") && out.contains("sip-faxd started (pid"),
        "{out}"
    );

    let new = d.pid();
    assert_ne!(new, old, "a new process");
    wait_until("the old process to be gone", 10, || !alive(old));
    let after = d.status().expect("running again");
    assert_eq!(after["pid"], new);
    assert_eq!(after["listen"], before["listen"], "the same listeners");
    assert_eq!(after["state"], "running");
    // the call made before the restart is complete and kept
    let row = final_row(&db, &rep.call_id).await;
    assert_eq!((row.fax_status, row.pages), (FaxStatus::Received, 4));
    // and the new process takes calls
    let rep2 = run_call(gateway(&d.addr(), &src))
        .await
        .expect("second fax");
    assert_eq!(rep2.fax.as_ref().unwrap().caller_code, Some(0));
    assert_eq!(
        final_row(&db, &rep2.call_id).await.fax_status,
        FaxStatus::Received
    );
}

// ------------------------------------------------------------------------------ M07

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m07_status_shows_the_running_service_and_exits_3_when_there_is_none() {
    let d = Daemon::prepare("m4d-m07", &opts(2, "", true, ""));
    let (code, out, _) = d.cli(&["status"]);
    assert_eq!(code, Some(3), "{out}");
    assert!(out.contains("not running"), "{out}");
    let (code, _, _) = d.cli(&["status", "--json"]);
    assert_eq!(code, Some(3));

    let (code, out, err) = d.cli(&["start", "-d"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    let pid = d.pid();
    let (code, out, _) = d.cli(&["status"]);
    assert_eq!(code, Some(0));
    for want in [
        "sip-faxd is running".to_string(),
        format!("PID:      {pid}"),
        format!("Version:  {}", env!("CARGO_PKG_VERSION")),
        format!("udp 127.0.0.1:{}", d.prep.port),
        format!("tcp 127.0.0.1:{}", d.prep.port),
        "Calls:    0 in progress".to_string(),
        "sipp: none".to_string(),
    ] {
        assert!(out.contains(&want), "{want:?} missing from\n{out}");
    }
    let j = d.status().unwrap();
    assert_eq!(j["pid"], pid);
    assert_eq!(j["version"], env!("CARGO_PKG_VERSION"));
    assert!(j["uptime_secs"].as_u64().unwrap() < 120);
    assert!(j["started_at"].as_str().unwrap().ends_with('Z'));

    // a call in progress is counted
    let db = open(&d.prep.db_url).await;
    let src = long_fax("m4d-m07", 6);
    let fax = tokio::spawn(run_call(gateway(&d.addr(), &src)));
    wait_pages(&db, 1).await;
    assert_eq!(d.status().unwrap()["active_calls"], 1);
    let (_, out, _) = d.cli(&["status"]);
    assert!(out.contains("Calls:    1 in progress"), "{out}");
    let _ = d.cli(&["stop", "--force"]);
    let _ = tokio::time::timeout(Duration::from_secs(30), fax).await;
    let (code, out, _) = d.cli(&["status"]);
    assert_eq!(code, Some(3), "{out}");
}

// ------------------------------------------------------------------------------ M08

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m08_under_systemd_start_stop_and_restart_point_to_systemctl_unless_overridden() {
    let d = Daemon::prepare("m4d-m08", &opts(2, "", true, ""));
    let sd = |args: &[&str]| {
        let o = faxd_command(args, &d.prep.config_path)
            .env("INVOCATION_ID", "0123456789abcdef")
            .output()
            .unwrap();
        (
            o.status.code(),
            String::from_utf8_lossy(&o.stdout).into_owned(),
            String::from_utf8_lossy(&o.stderr).into_owned(),
        )
    };
    for cmd in ["start", "stop", "restart"] {
        let (code, out, err) = sd(&[cmd]);
        assert_eq!(code, Some(1), "{cmd}: {out}\n{err}");
        assert!(
            err.contains(&format!("systemctl {cmd} sip-faxd")),
            "{cmd}: {err}"
        );
        assert!(err.contains("--ignore-systemd"), "{err}");
    }
    assert!(!d.prep.pid_file.exists(), "nothing was started");
    assert_eq!(d.cli(&["status"]).0, Some(3));

    // --ignore-systemd goes ahead
    let (code, out, err) = sd(&["start", "-d", "--ignore-systemd"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    let pid = d.pid();
    assert!(alive(pid));
    // now a stop is refused and the service keeps running
    let (code, _, err) = sd(&["stop"]);
    assert_eq!(code, Some(1));
    assert!(err.contains("systemctl stop sip-faxd"), "{err}");
    assert!(alive(pid) && d.status().is_some(), "still running");
    // status works under systemd too (it only reads)
    assert_eq!(sd(&["status"]).0, Some(0));
    let (code, out, err) = sd(&["stop", "--ignore-systemd"]);
    assert_eq!(code, Some(0), "{out}\n{err}");
    wait_until("the process to be gone", 10, || !alive(pid));
}

// ------------------------------------------------------------------------------ M09

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m09_sigterm_to_serve_is_the_same_stop_and_a_second_signal_forces_it() {
    let tag = "m4d-m09";
    let f = Server(Some(Faxd::start(
        tag,
        &opts(2, "stop_timeout_secs = 300", false, ""),
    )));
    let db = open(&f.db_url).await;
    let pid = std::fs::read_to_string(&f.pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let src = long_fax(tag, 30);
    let fax = tokio::spawn(run_call(gateway(&f.addr(), &src)));
    wait_pages(&db, 1).await;

    signal("TERM", pid);
    wait_until("the stop to begin", 20, || {
        faxd_cli(&["status", "--json"], &f.config_path)
            .1
            .contains("\"stopping\"")
    });
    assert_eq!(invite_status(&f.addr()).await, 503, "new calls are refused");
    std::thread::sleep(Duration::from_secs(2));
    assert!(alive(pid), "the fax is being waited for");
    // second signal: now
    let t0 = Instant::now();
    signal("TERM", pid);
    wait_until("the process to exit", 30, || !alive(pid));
    assert!(t0.elapsed() < Duration::from_secs(25));
    let _ = tokio::time::timeout(Duration::from_secs(30), fax).await;
    let list = db
        .list(&ListFilter::default(), PageRequest::default())
        .await
        .unwrap();
    let row = final_row(&db, &list.items[0].call_id).await;
    assert_eq!(row.fax_status, FaxStatus::Partial);
    assert_eq!(row.hangup_code.as_deref(), Some("shutdown"));
    assert!(row.pages >= 1);
    let log = f.stop();
    let log = log.join("\n");
    assert!(log.contains("stopping: no new calls are accepted"), "{log}");
    assert!(log.contains("second stop signal"), "{log}");
    assert!(log.contains("sip-faxd stopped"), "{log}");
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m09_sigterm_waits_for_the_fax_which_then_completes() {
    let tag = "m4d-m09b";
    let f = Server(Some(Faxd::start(tag, &opts(2, "", false, ""))));
    let db = open(&f.db_url).await;
    let pid: u32 = std::fs::read_to_string(&f.pid_file)
        .unwrap()
        .trim()
        .parse()
        .unwrap();
    let src = source_tiff(
        &scratch(&format!("src-{tag}")),
        "src.tif",
        &specs(4, true, 500),
    );
    let fax = tokio::spawn(run_call(gateway(&f.addr(), &src)));
    wait_pages(&db, 1).await;
    signal("TERM", pid);
    let rep = fax.await.unwrap().expect("fax call");
    assert_eq!(rep.fax.as_ref().unwrap().caller_code, Some(0));
    let row = final_row(&db, &rep.call_id).await;
    assert_eq!((row.fax_status, row.pages), (FaxStatus::Received, 4));
    wait_until("the process to exit", 30, || !alive(pid));
    assert!(!f.pid_file.exists() && !f.socket.exists());
    let log = f.stop().join("\n");
    assert!(
        !log.contains("ending the calls that are still running"),
        "{log}"
    );
}

// ------------------------------------------------------------------------------ M10

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m10_the_control_socket_is_0600_and_another_uid_cannot_connect() {
    use std::os::unix::fs::MetadataExt;
    let d = Daemon::start("m4d-m10", &opts(2, "", false, ""));
    let meta = std::fs::metadata(&d.prep.socket).unwrap();
    assert_eq!(meta.mode() & 0o777, 0o600, "mode {:o}", meta.mode());
    assert_eq!(meta.uid(), my_uid(), "owned by the service's user");

    // a tiny client in an independent implementation (Python): the owner gets an answer ...
    let py = r#"
import socket, sys, json
s = socket.socket(socket.AF_UNIX)
try:
    s.connect(sys.argv[1])
except OSError as e:
    print("ERR", e.errno, e.strerror); sys.exit(0)
s.sendall(b'{"cmd":"status"}\n')
print("OK", json.loads(s.makefile().readline())["pid"])
"#;
    let sock = s(&d.prep.socket).to_string();
    let as_owner = Command::new("python3")
        .args(["-c", py, &sock])
        .output()
        .unwrap();
    let as_owner = String::from_utf8_lossy(&as_owner.stdout).into_owned();
    assert_eq!(
        as_owner.trim(),
        format!("OK {}", d.pid()),
        "owner: {as_owner}"
    );
    // ... and uid 1234 is refused (EACCES = 13); the directory is open to it
    let other = Command::new("setpriv")
        .args([
            "--reuid=1234",
            "--regid=1234",
            "--clear-groups",
            "python3",
            "-c",
            py,
            &sock,
        ])
        .output()
        .unwrap();
    let other_out = String::from_utf8_lossy(&other.stdout).into_owned();
    assert!(
        other_out.starts_with("ERR 13"),
        "another uid must get EACCES: {other_out} {}",
        String::from_utf8_lossy(&other.stderr)
    );
    // the CLI of the other user: sees a running service it cannot talk to, changes nothing
    let o = Command::new("setpriv")
        .args(["--reuid=1234", "--regid=1234", "--clear-groups"])
        .arg(interop::faxd_binary())
        .args(["stop", "--config"])
        .arg(&d.prep.config_path)
        .output()
        .unwrap();
    assert_ne!(o.status.code(), Some(0));
    assert!(
        String::from_utf8_lossy(&o.stderr).contains("cannot be reached"),
        "{}",
        String::from_utf8_lossy(&o.stderr)
    );
    assert!(
        alive(d.pid()) && d.status().is_some(),
        "the service did not stop"
    );
}

/// The real uid of this process.
fn my_uid() -> u32 {
    let st = std::fs::read_to_string("/proc/self/status").unwrap();
    st.lines()
        .find(|l| l.starts_with("Uid:"))
        .and_then(|l| l.split_whitespace().nth(1)?.parse().ok())
        .unwrap()
}

// ------------------------------------------------------------------------------ M11 / M12

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m12_logs_f_keeps_following_across_rotations_and_m11_the_old_files_are_valid_gzip() {
    // 12 KB files: the calls below rotate the log several times
    let d = Daemon::start(
        "m4d-m12",
        &opts(2, "", true, "max_size_mb = 0.012\nkeep = 50\n"),
    );
    let log_path = d.prep.log_path.clone().unwrap();
    let mut follow = Spawned::new(faxd_command(
        &["logs", "-f", "-n", "0"],
        &d.prep.config_path,
    ));
    std::thread::sleep(Duration::from_millis(800));

    // a burst of short calls (no fax): every one writes a handful of lines
    let mut ids = Vec::new();
    for _ in 0..25 {
        let mut cfg = UacConfig::new(d.addr().parse().unwrap(), ReinviteBy::Gateway);
        cfg.hold = Duration::from_millis(50);
        let rep = run_call(cfg).await.expect("short call");
        ids.push(rep.call_id);
        // the follower looks every 200 ms: keep to at most one rotation per look
        tokio::time::sleep(Duration::from_millis(300)).await;
    }
    // each call's "inbound call" line has to arrive through the follower, once
    let last = ids.last().unwrap().clone();
    follow.wait_line(&last, 20);
    follow.drain();
    let shown = follow.seen.join("\n");
    for id in &ids {
        assert!(
            shown.contains(id.as_str()),
            "{id} is missing from the followed output"
        );
    }
    let inbound = follow
        .seen
        .iter()
        .filter(|l| l.contains("inbound call"))
        .count();
    if inbound != ids.len() {
        let mut firsts = std::collections::HashMap::new();
        for (n, l) in follow.seen.iter().enumerate() {
            if let Some(first) = firsts.insert(l.clone(), n) {
                eprintln!(
                    "DUP line {n} repeats line {first}: ...{}",
                    &l[l.len().saturating_sub(60)..]
                );
            }
        }
        eprintln!("total lines {}", follow.seen.len());
    }
    assert_eq!(
        inbound,
        ids.len(),
        "one `inbound call` line per call, none lost, none twice"
    );

    // it did rotate, several times, and the old files are valid gzip with the right names
    let dir = log_path.parent().unwrap();
    let mut gz: Vec<PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| p.extension().is_some_and(|e| e == "gz"))
        .collect();
    gz.sort();
    assert!(gz.len() >= 3, "rotated several times: {gz:?}");
    for p in &gz {
        let st = Command::new("gzip").arg("-t").arg(p).status().unwrap();
        assert!(st.success(), "gzip -t {p:?}");
        let n = p.file_name().unwrap().to_str().unwrap();
        assert!(
            n.starts_with("sip-faxd.log.20") && n.ends_with(".gz"),
            "{n}"
        );
    }
    // nothing is lost between the archives and the live file: all calls are in them, once
    let mut all = String::new();
    for p in &gz {
        let o = Command::new("gzip").arg("-dc").arg(p).output().unwrap();
        all += &String::from_utf8_lossy(&o.stdout);
    }
    all += &std::fs::read_to_string(&log_path).unwrap();
    for id in &ids {
        assert_eq!(
            all.lines()
                .filter(|l| l.contains("inbound call") && l.contains(id.as_str()))
                .count(),
            1,
            "{id} in the files"
        );
    }
    // `logs -n` shows the end of the live file
    let (code, out, _) = d.cli(&["logs", "-n", "3"]);
    assert_eq!(code, Some(0));
    assert_eq!(out.lines().count(), 3, "{out}");
}
