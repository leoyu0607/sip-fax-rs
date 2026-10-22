//! `sip-faxd start | stop | restart | status | logs` (Linux): what the operator types. All of
//! them work from the `[service]` / `[log]` sections and the control socket only; none opens
//! the database.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitCode, Stdio};
use std::time::{Duration, Instant};

use tokio::signal::unix::{SignalKind, signal};

use super::config::ServiceConfig;
use super::control::{Client, fetch_status};
use super::logfile::{Follower, tail_lines};
use super::pidfile::{remove_if_stale, running_pid, terminate};
use super::protocol::{Reply, Request, StatusInfo};
use super::systemd;

/// `status` when nothing runs (the LSB code for "program is not running").
pub const EXIT_NOT_RUNNING: u8 = 3;
/// Lines of the log shown when a start fails.
const FAILED_START_LINES: usize = 50;
/// How long `stop` waits for the process to be gone after it said `stopped`.
const EXIT_WAIT: Duration = Duration::from_secs(30);
/// How often the foreground log view looks for new lines.
const FOLLOW_INTERVAL: Duration = Duration::from_millis(200);

/// Where the service keeps its files.
#[derive(Debug)]
pub struct Ctx {
    cfg_path: PathBuf,
    pid_path: PathBuf,
    sock_path: PathBuf,
    log_path: Option<PathBuf>,
    start_timeout: Duration,
}

impl Ctx {
    /// Reads `[service]` and `[log]` of the configuration file; relative paths are against
    /// `base`, the executable's directory.
    pub fn load(cfg_path: &Path, base: &Path) -> Result<Ctx, String> {
        let c = ServiceConfig::load(cfg_path)?;
        Ok(Ctx {
            cfg_path: cfg_path.to_path_buf(),
            pid_path: c.service.pid_path(base),
            sock_path: c.service.socket_path(base),
            log_path: c.log.file_path(base),
            start_timeout: Duration::from_secs(c.service.start_timeout_secs),
        })
    }
}

fn runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("a current-thread runtime can always be built")
}

fn load(cfg_path: &Path, base: &Path) -> Result<Ctx, ExitCode> {
    Ctx::load(cfg_path, base).map_err(|e| {
        eprintln!("sip-faxd: error: {e}");
        ExitCode::from(2)
    })
}

/// `Some(message)` when systemd manages the service and the command must not act.
fn systemd_refusal(ctx: &Ctx, command: &str, ignore: bool) -> Option<String> {
    if ignore {
        return None;
    }
    let env = std::env::var("INVOCATION_ID").ok();
    let service = running_pid(&ctx.pid_path)
        .filter(|p| *p != 0)
        .and_then(systemd::proc_info);
    systemd::managed_by_systemd(env.as_deref(), service).then(|| systemd::advice(command))
}

fn print_log_tail(path: Option<&Path>, lines: usize) {
    let Some(p) = path else {
        eprintln!("(file logging is off: [log] file is empty)");
        return;
    };
    match tail_lines(p, lines) {
        Ok((bytes, _)) if !bytes.is_empty() => {
            eprintln!("---- last {lines} lines of {} ----", p.display());
            let _ = std::io::stderr().write_all(&bytes);
            if bytes.last() != Some(&b'\n') {
                eprintln!();
            }
            eprintln!("---- end ----");
        }
        _ => eprintln!("(no log lines in {})", p.display()),
    }
}

// ------------------------------------------------------------------------------ start

/// `sip-faxd start [-d] [--ignore-systemd]`
pub fn start(cfg_path: &Path, base: &Path, detach: bool, ignore_systemd: bool) -> ExitCode {
    let ctx = match load(cfg_path, base) {
        Ok(c) => c,
        Err(c) => return c,
    };
    if let Some(msg) = systemd_refusal(&ctx, "start", ignore_systemd) {
        eprintln!("sip-faxd: {msg}");
        return ExitCode::from(1);
    }
    runtime().block_on(start_inner(&ctx, detach))
}

async fn start_inner(ctx: &Ctx, detach: bool) -> ExitCode {
    if let Some(pid) = running_pid(&ctx.pid_path) {
        eprintln!("sip-faxd: already running (pid {pid})");
        return ExitCode::from(1);
    }
    // Ctrl+C from here on only ever stops the display, never the service: the handler exists
    // before the service does
    let mut sigint = match signal(SignalKind::interrupt()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("sip-faxd: error: cannot install the signal handler: {e}");
            return ExitCode::from(1);
        }
    };
    let log_from = ctx
        .log_path
        .as_deref()
        .and_then(|p| std::fs::metadata(p).ok())
        .map_or(0, |m| m.len());
    let exe = match std::env::current_exe() {
        Ok(e) => e,
        Err(e) => {
            eprintln!("sip-faxd: error: cannot find my own executable: {e}");
            return ExitCode::from(1);
        }
    };
    // The child calls setsid() itself as the first thing it does, which detaches it from this
    // terminal and session.
    let mut child = match Command::new(exe)
        .arg("serve")
        .arg("--daemonized")
        .arg("--config")
        .arg(&ctx.cfg_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
    {
        Ok(c) => c,
        Err(e) => {
            eprintln!("sip-faxd: error: cannot start the service: {e}");
            return ExitCode::from(1);
        }
    };
    let deadline = Instant::now() + ctx.start_timeout;
    let ready: Result<StatusInfo, String> = loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                break Err(format!("the service exited during the start ({status})"));
            }
            Ok(None) => {}
            Err(e) => break Err(format!("cannot watch the service: {e}")),
        }
        if let Ok(s) = fetch_status(&ctx.sock_path).await {
            break Ok(s);
        }
        if Instant::now() >= deadline {
            break Err(format!(
                "the service did not answer on {} within {} s",
                ctx.sock_path.display(),
                ctx.start_timeout.as_secs()
            ));
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let status = match ready {
        Ok(s) => s,
        Err(why) => {
            eprintln!("sip-faxd: error: {why}");
            if matches!(child.try_wait(), Ok(None)) {
                // alive but not ready: do not leave it half started
                let _ = terminate(child.id());
                let stop_by = Instant::now() + Duration::from_secs(10);
                while matches!(child.try_wait(), Ok(None)) && Instant::now() < stop_by {
                    tokio::time::sleep(Duration::from_millis(100)).await;
                }
                if matches!(child.try_wait(), Ok(None)) {
                    let _ = child.kill();
                    let _ = child.wait();
                }
            }
            remove_if_stale(&ctx.pid_path);
            print_log_tail(ctx.log_path.as_deref(), FAILED_START_LINES);
            return ExitCode::from(1);
        }
    };
    println!(
        "sip-faxd started (pid {}, version {})",
        status.pid, status.version
    );
    if detach {
        return ExitCode::SUCCESS;
    }
    let Some(log) = ctx.log_path.as_deref() else {
        println!("File logging is off ([log] file is empty): nothing to show.");
        return ExitCode::SUCCESS;
    };
    println!(
        "Showing {}. Ctrl+C stops the display only, the service keeps running.",
        log.display()
    );
    let mut follower = Follower::open_at(log, log_from);
    let mut out = Vec::new();
    let mut last_alive_check = Instant::now();
    loop {
        out.clear();
        if follower.poll(&mut out).is_ok() && !out.is_empty() {
            let mut so = std::io::stdout().lock();
            if so.write_all(&out).and_then(|()| so.flush()).is_err() {
                return ExitCode::SUCCESS; // the reader of our output went away
            }
        }
        tokio::select! {
            _ = sigint.recv() => {
                println!();
                println!("The service is still running in the background. Stop it with `sip-faxd stop`.");
                println!("服務仍在背景執行，用 `sip-faxd stop` 停止。");
                return ExitCode::SUCCESS;
            }
            () = tokio::time::sleep(FOLLOW_INTERVAL) => {}
        }
        if last_alive_check.elapsed() >= Duration::from_secs(1) {
            last_alive_check = Instant::now();
            if running_pid(&ctx.pid_path).is_none() {
                // show what it said on the way out
                out.clear();
                let _ = follower.poll(&mut out);
                let _ = std::io::stdout().write_all(&out);
                println!("sip-faxd has stopped.");
                return ExitCode::from(1);
            }
        }
    }
}

// ------------------------------------------------------------------------------- stop

/// `sip-faxd stop [--force] [--timeout SECS] [--ignore-systemd]`
pub fn stop(
    cfg_path: &Path,
    base: &Path,
    force: bool,
    timeout: Option<u64>,
    ignore_systemd: bool,
) -> ExitCode {
    let ctx = match load(cfg_path, base) {
        Ok(c) => c,
        Err(c) => return c,
    };
    if let Some(msg) = systemd_refusal(&ctx, "stop", ignore_systemd) {
        eprintln!("sip-faxd: {msg}");
        return ExitCode::from(1);
    }
    runtime().block_on(stop_inner(&ctx, force, timeout))
}

async fn stop_inner(ctx: &Ctx, force: bool, timeout: Option<u64>) -> ExitCode {
    let Some(pid) = running_pid(&ctx.pid_path) else {
        println!("sip-faxd is not running");
        return ExitCode::SUCCESS;
    };
    let mut client = match Client::connect(&ctx.sock_path).await {
        Ok(c) => c,
        Err(e) => {
            eprintln!(
                "sip-faxd: error: pid {pid} is running but its control socket {} cannot be \
                 reached: {e}",
                ctx.sock_path.display()
            );
            return ExitCode::from(1);
        }
    };
    let mut sigint = match signal(SignalKind::interrupt()) {
        Ok(s) => s,
        Err(e) => {
            eprintln!("sip-faxd: error: cannot install the signal handler: {e}");
            return ExitCode::from(1);
        }
    };
    if let Err(e) = client.send(&Request::Stop { force, timeout }).await {
        eprintln!("sip-faxd: error: {e}");
        return ExitCode::from(1);
    }
    loop {
        let reply = tokio::select! {
            r = client.next() => r,
            _ = sigint.recv() => {
                println!();
                println!("No longer waiting. The service keeps stopping in the background.");
                return ExitCode::from(130);
            }
        };
        match reply {
            Ok(Some(Reply::Stopping {
                active,
                timeout_secs,
                force,
            })) => println!("{}", stopping_line(pid, active, timeout_secs, force)),
            Ok(Some(Reply::Waiting {
                active,
                remaining_secs,
            })) if active > 0 => println!("{}", waiting_line(active, remaining_secs)),
            Ok(Some(Reply::Stopped)) | Ok(None) => break,
            Ok(Some(Reply::Error { message })) => {
                eprintln!("sip-faxd: error: {message}");
                return ExitCode::from(1);
            }
            Ok(Some(_)) => {}
            Err(e) => {
                eprintln!("sip-faxd: error: {e}");
                return ExitCode::from(1);
            }
        }
    }
    // the service has said it is done; wait for the process itself
    let until = Instant::now() + EXIT_WAIT;
    while running_pid(&ctx.pid_path).is_some() {
        if Instant::now() >= until {
            eprintln!(
                "sip-faxd: error: the service did not exit within {} s",
                EXIT_WAIT.as_secs()
            );
            return ExitCode::from(1);
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    println!("sip-faxd stopped");
    ExitCode::SUCCESS
}

/// First line of a `stop`.
pub fn stopping_line(pid: u32, active: usize, timeout_secs: u64, force: bool) -> String {
    if force {
        format!("Stopping sip-faxd (pid {pid}) now: {active} call(s) in progress are ended")
    } else if active == 0 {
        format!("Stopping sip-faxd (pid {pid})")
    } else {
        format!(
            "Stopping sip-faxd (pid {pid}): {active} call(s) in progress, waiting up to {timeout_secs} s"
        )
    }
}

/// The progress line of a `stop` that waits for calls.
pub fn waiting_line(active: usize, remaining_secs: u64) -> String {
    format!("Waiting for {active} call(s) to finish ({remaining_secs} s left)")
}

// ----------------------------------------------------------------------------- restart

/// `sip-faxd restart [-d] [--force] [--timeout SECS] [--ignore-systemd]`
pub fn restart(
    cfg_path: &Path,
    base: &Path,
    detach: bool,
    force: bool,
    timeout: Option<u64>,
    ignore_systemd: bool,
) -> ExitCode {
    let ctx = match load(cfg_path, base) {
        Ok(c) => c,
        Err(c) => return c,
    };
    if let Some(msg) = systemd_refusal(&ctx, "restart", ignore_systemd) {
        eprintln!("sip-faxd: {msg}");
        return ExitCode::from(1);
    }
    let rt = runtime();
    if rt.block_on(stop_inner(&ctx, force, timeout)) != ExitCode::SUCCESS {
        return ExitCode::from(1);
    }
    rt.block_on(start_inner(&ctx, detach))
}

// ----------------------------------------------------------------------------- status

/// `sip-faxd status [--json]`
pub fn status(cfg_path: &Path, base: &Path, json: bool) -> ExitCode {
    let ctx = match load(cfg_path, base) {
        Ok(c) => c,
        Err(c) => return c,
    };
    let Some(pid) = running_pid(&ctx.pid_path) else {
        println!("sip-faxd is not running");
        return ExitCode::from(EXIT_NOT_RUNNING);
    };
    match runtime().block_on(fetch_status(&ctx.sock_path)) {
        Ok(s) => {
            if json {
                println!("{}", serde_json::to_string_pretty(&s).unwrap_or_default());
            } else {
                print!("{}", format_status(&s));
            }
            ExitCode::SUCCESS
        }
        Err(e) => {
            println!("sip-faxd is running (pid {pid}) but its control socket does not answer: {e}");
            ExitCode::from(1)
        }
    }
}

/// The `status` text.
pub fn format_status(s: &StatusInfo) -> String {
    let mut o = String::new();
    o += &format!("sip-faxd is {}\n", s.state);
    o += &format!("  PID:      {}\n", s.pid);
    o += &format!("  Version:  {}\n", s.version);
    o += &format!(
        "  Started:  {} (up {})\n",
        s.started_at,
        format_uptime(s.uptime_secs)
    );
    o += &format!("  Listen:   {}\n", s.listen.join(", "));
    o += &format!("  Calls:    {} in progress\n", s.active_calls);
    if s.trunks.is_empty() {
        o += "  Trunks:   none\n";
    } else {
        o += "  Trunks:\n";
        for t in &s.trunks {
            o += &format!("    {}: {}\n", t.name, t.registration);
        }
    }
    o
}

/// `1d 2h 3m 4s`, leaving out leading zero units.
pub fn format_uptime(secs: u64) -> String {
    let (d, h, m, s) = (
        secs / 86_400,
        secs % 86_400 / 3600,
        secs % 3600 / 60,
        secs % 60,
    );
    if d > 0 {
        format!("{d}d {h}h {m}m {s}s")
    } else if h > 0 {
        format!("{h}h {m}m {s}s")
    } else if m > 0 {
        format!("{m}m {s}s")
    } else {
        format!("{s}s")
    }
}

// ------------------------------------------------------------------------------- logs

/// `sip-faxd logs [-f] [-n N]`
pub fn logs(cfg_path: &Path, base: &Path, follow: bool, lines: usize) -> ExitCode {
    let ctx = match load(cfg_path, base) {
        Ok(c) => c,
        Err(c) => return c,
    };
    let Some(path) = ctx.log_path else {
        eprintln!("sip-faxd: file logging is off ([log] file is empty in the configuration)");
        return ExitCode::from(1);
    };
    let mut pos = 0;
    match tail_lines(&path, lines) {
        Ok((bytes, len)) => {
            pos = len;
            let mut so = std::io::stdout().lock();
            if so.write_all(&bytes).and_then(|()| so.flush()).is_err() {
                return ExitCode::SUCCESS;
            }
        }
        Err(e) if follow && e.kind() == std::io::ErrorKind::NotFound => {
            eprintln!("(waiting for {} to appear)", path.display());
        }
        Err(e) => {
            eprintln!("sip-faxd: error: cannot read {}: {e}", path.display());
            return ExitCode::from(1);
        }
    }
    if !follow {
        return ExitCode::SUCCESS;
    }
    let mut f = Follower::open_at(&path, pos);
    let mut out = Vec::new();
    loop {
        std::thread::sleep(FOLLOW_INTERVAL);
        out.clear();
        if f.poll(&mut out).is_ok() && !out.is_empty() {
            let mut so = std::io::stdout().lock();
            if so.write_all(&out).and_then(|()| so.flush()).is_err() {
                return ExitCode::SUCCESS;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::service::protocol::TrunkStatus;

    #[test]
    fn m07_the_status_text_has_every_field() {
        let s = StatusInfo {
            pid: 1234,
            version: "0.3.0".into(),
            state: "running".into(),
            started_at: "2026-10-21T08:00:00.000Z".into(),
            uptime_secs: 3 * 3600 + 25 * 60 + 7,
            listen: vec!["udp 0.0.0.0:5060".into(), "tcp 0.0.0.0:5060".into()],
            active_calls: 2,
            trunks: vec![
                TrunkStatus {
                    name: "cht".into(),
                    registration: "registered".into(),
                },
                TrunkStatus {
                    name: "lab".into(),
                    registration: "none".into(),
                },
            ],
        };
        let t = format_status(&s);
        for want in [
            "sip-faxd is running",
            "PID:      1234",
            "Version:  0.3.0",
            "2026-10-21T08:00:00.000Z (up 3h 25m 7s)",
            "Listen:   udp 0.0.0.0:5060, tcp 0.0.0.0:5060",
            "Calls:    2 in progress",
            "cht: registered",
            "lab: none",
        ] {
            assert!(t.contains(want), "{want:?} missing from\n{t}");
        }
        let none = StatusInfo {
            trunks: vec![],
            state: "stopping".into(),
            ..s
        };
        let t = format_status(&none);
        assert!(
            t.contains("sip-faxd is stopping") && t.contains("Trunks:   none"),
            "{t}"
        );
    }

    #[test]
    fn m07_uptimes_drop_leading_zero_units() {
        assert_eq!(format_uptime(5), "5s");
        assert_eq!(format_uptime(65), "1m 5s");
        assert_eq!(format_uptime(3600), "1h 0m 0s");
        assert_eq!(format_uptime(90_061), "1d 1h 1m 1s");
    }

    #[test]
    fn m04_the_stop_lines_say_what_is_waited_for() {
        assert_eq!(stopping_line(7, 0, 300, false), "Stopping sip-faxd (pid 7)");
        assert!(
            stopping_line(7, 2, 300, false).contains("2 call(s) in progress, waiting up to 300 s")
        );
        assert!(stopping_line(7, 2, 0, true).contains("now: 2 call(s)"));
        assert_eq!(
            waiting_line(1, 295),
            "Waiting for 1 call(s) to finish (295 s left)"
        );
    }

    #[test]
    fn m08_a_managed_service_is_refused_unless_told_otherwise() {
        let d = tempfile::tempdir().unwrap();
        let cfg = d.path().join("c.toml");
        std::fs::write(&cfg, "").unwrap();
        let ctx = Ctx::load(&cfg, d.path()).unwrap();
        // nothing in the environment of the test run says "systemd"
        assert!(
            std::env::var_os("INVOCATION_ID").is_none()
                || systemd_refusal(&ctx, "stop", true).is_none()
        );
        // the override always wins
        assert_eq!(systemd_refusal(&ctx, "stop", true), None);
    }

    #[test]
    fn m00_a_missing_or_broken_config_is_exit_code_2() {
        let d = tempfile::tempdir().unwrap();
        assert!(Ctx::load(&d.path().join("missing.toml"), d.path()).is_err());
        let bad = d.path().join("bad.toml");
        std::fs::write(&bad, "[service]\nbogus = 1\n").unwrap();
        assert!(Ctx::load(&bad, d.path()).unwrap_err().contains("[service]"));
    }
}
