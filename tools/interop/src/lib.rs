//! Harness for the interop tests (tools workspace, run inside the Rocky 9 container by
//! `scripts/e2e-docker.sh`).
//!
//! * [`Faxd`] builds the real `sip-faxd` (once per test process), starts it with a per-test
//!   configuration on free ports and collects its log, including the `call_result` JSON lines.
//! * [`Sipp`] runs a scenario from `tools/sipp/` against it and returns the exit code, the
//!   statistics counters and the timestamped message log.
//!
//! Scratch directories are unique per process (`<tmp>/<name>-<pid>`), so concurrent runs of
//! the suite never share files.

use std::io::{BufRead, BufReader};
use std::net::{TcpListener, UdpSocket};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{Duration, Instant};

/// Repository root (this crate lives in `tools/interop`).
pub fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// Directory with the SIPp scenarios.
pub fn sipp_dir() -> PathBuf {
    repo_root().join("tools/sipp")
}

/// A fresh, process-unique scratch directory.
pub fn scratch(name: &str) -> PathBuf {
    let base = std::env::var_os("CARGO_TARGET_DIR")
        .map(|d| PathBuf::from(d).join("tmp"))
        .unwrap_or_else(std::env::temp_dir);
    let d = base.join(format!("interop-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).expect("create scratch dir");
    d
}

/// A port that is free for both UDP and TCP on 127.0.0.1 right now.
pub fn free_port() -> u16 {
    for _ in 0..100 {
        let Ok(u) = UdpSocket::bind("127.0.0.1:0") else {
            continue;
        };
        let port = u.local_addr().expect("addr").port();
        if TcpListener::bind(("127.0.0.1", port)).is_ok() {
            return port;
        }
    }
    panic!("no free port");
}

static BINARY: OnceLock<PathBuf> = OnceLock::new();

/// Builds `sip-faxd` (debug profile) once and returns the executable.
pub fn faxd_binary() -> PathBuf {
    BINARY
        .get_or_init(|| {
            let manifest = repo_root().join("Cargo.toml");
            let out = Command::new(std::env::var("CARGO").unwrap_or_else(|_| "cargo".into()))
                .args([
                    "build",
                    "-p",
                    "sip-faxd",
                    "--message-format=json-render-diagnostics",
                ])
                .arg("--manifest-path")
                .arg(&manifest)
                .stderr(Stdio::inherit())
                .output()
                .expect("run cargo build");
            assert!(out.status.success(), "building sip-faxd failed");
            for line in String::from_utf8_lossy(&out.stdout).lines() {
                let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
                    continue;
                };
                if v["reason"] == "compiler-artifact"
                    && v["target"]["name"] == "sip-faxd"
                    && v["executable"].is_string()
                {
                    return PathBuf::from(v["executable"].as_str().expect("path"));
                }
            }
            panic!("cargo did not report the sip-faxd executable");
        })
        .clone()
}

/// What to put into the generated configuration.
#[derive(Debug, Clone, Default)]
pub struct FaxdOptions {
    /// Extra lines inside `[sip]`.
    pub sip: String,
    /// Extra lines inside `[call]`.
    pub call: String,
    /// Extra lines of the single `[[trunks]]` entry (and its sub-tables).
    pub trunk: String,
    /// Extra top-level tables appended verbatim.
    pub extra: String,
    /// `keep = "failed"` instead of `"all"`.
    pub capture_keep_failed: bool,
    /// Extra lines inside `[capture]` (e.g. `udptl = "full"`).
    pub capture_extra: String,
    /// Use this port instead of a free one.
    pub port: Option<u16>,
    /// Run the real T.38 fax session. Off by default: the signalling tests only want the
    /// call held (`fax_session = "hold"`).
    pub real_fax: bool,
    /// Data directory for received faxes (default: `<scratch>/data`).
    pub data_dir: Option<PathBuf>,
    /// `[database] url` (default: a SQLite file `fax.db` of its own in the temp dir, so that every
    /// instance has its own database).
    pub database_url: Option<String>,
    /// Write the log to `<scratch>/logs/sip-faxd.log` (default: no log file, stderr only).
    pub log_file: bool,
    /// Extra lines inside `[log]` (rotation settings).
    pub log_extra: String,
    /// Extra lines inside `[service]` (`stop_timeout_secs`, ...).
    pub service: String,
    /// Leave out the `[[trunks]]` entry: a new installation, a trunk is added through the web
    /// interface later (M4f).
    pub no_trunk: bool,
    /// Serve the web interface over plain HTTP on `127.0.0.1:<port>` (default: web off).
    pub web_port: Option<u16>,
}

/// A configuration written to disk, not started yet.
#[derive(Debug, Clone)]
pub struct Prepared {
    /// SIP port (UDP and TCP).
    pub port: u16,
    /// Scratch directory of this instance.
    pub dir: PathBuf,
    /// Capture directory (`logs/calls`).
    pub calls_dir: PathBuf,
    /// Data directory.
    pub data_dir: PathBuf,
    /// `[database] url`.
    pub db_url: String,
    /// The configuration file.
    pub config_path: PathBuf,
    /// `[service] pid_file`.
    pub pid_file: PathBuf,
    /// `[service] socket`.
    pub socket: PathBuf,
    /// `[log] file` (`None`: file logging off).
    pub log_path: Option<PathBuf>,
}

/// `sip-faxd <args> --config <config>` as a command to run or spawn (no `RUST_LOG`).
pub fn faxd_command(args: &[&str], config: &Path) -> Command {
    let mut c = Command::new(faxd_binary());
    c.args(args)
        .arg("--config")
        .arg(config)
        .env_remove("RUST_LOG")
        .env_remove("INVOCATION_ID");
    c
}

/// Runs `sip-faxd <args> --config <config>` to the end: exit code, stdout, stderr.
pub fn faxd_cli(args: &[&str], config: &Path) -> (Option<i32>, String, String) {
    let out = faxd_command(args, config).output().expect("run sip-faxd");
    (
        out.status.code(),
        String::from_utf8_lossy(&out.stdout).into_owned(),
        String::from_utf8_lossy(&out.stderr).into_owned(),
    )
}

/// A running `sip-faxd serve`.
pub struct Faxd {
    child: Option<Child>,
    /// SIP port (UDP and TCP).
    pub port: u16,
    /// Scratch directory of this instance.
    pub dir: PathBuf,
    /// Capture directory (`logs/calls`).
    pub calls_dir: PathBuf,
    /// Data directory (`<data_dir>/faxes/...`).
    pub data_dir: PathBuf,
    /// The `[database] url` the server runs with.
    pub db_url: String,
    /// The configuration file the server was started with.
    pub config_path: PathBuf,
    /// `[service] pid_file`.
    pub pid_file: PathBuf,
    /// `[service] socket`.
    pub socket: PathBuf,
    /// `[log] file`, when the options asked for one.
    pub log_path: Option<PathBuf>,
    log: Arc<Mutex<Vec<String>>>,
}

impl Faxd {
    /// Writes the configuration (and creates the scratch directory) without starting anything.
    pub fn prepare(name: &str, opts: &FaxdOptions) -> Prepared {
        let dir = scratch(name);
        let port = opts.port.unwrap_or_else(free_port);
        let calls_dir = dir.join("calls");
        let data_dir = opts.data_dir.clone().unwrap_or_else(|| dir.join("data"));
        let db_url = opts.database_url.clone().unwrap_or_else(|| {
            // Not in the scratch dir: on a Docker Desktop bind mount the file locks SQLite
            // relies on do not work between processes, and after a restart of the service a
            // reader could keep seeing an old WAL (M4d M06).
            let db_dir = std::env::temp_dir().join(format!("sf{}-{name}-db", std::process::id()));
            let _ = std::fs::remove_dir_all(&db_dir);
            std::fs::create_dir_all(&db_dir).expect("create the database dir");
            format!("sqlite://{}/fax.db", db_dir.display())
        });
        // unix socket paths are limited to about 100 bytes: keep the socket short
        static NEXT_SOCKET: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);
        let socket_no = NEXT_SOCKET.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let socket =
            std::env::temp_dir().join(format!("sf{}-{}.sock", std::process::id(), socket_no));
        // The PID file too: `flock` on a Docker Desktop bind mount does not exclude another
        // process, so a running service would look stopped there.
        let pid_file = std::env::temp_dir()
            .join(format!("sf{}-{}-run", std::process::id(), socket_no))
            .join("sip-faxd.pid");
        // The log goes to the temp dir, not to the scratch dir: on a Docker Desktop bind mount
        // inode numbers are not stable, and `logs -f` recognises a rotation by the inode.
        let log_path = opts.log_file.then(|| {
            std::env::temp_dir()
                .join(format!("sf{}-{}-logs", std::process::id(), socket_no))
                .join("sip-faxd.log")
        });
        let log_line = match &log_path {
            Some(p) => format!("file = {:?}", p.display().to_string()),
            None => "file = \"\"".to_string(),
        };
        let cfg = format!(
            "[log]\nlevel = \"debug\"\n{log_line}\n{}\n\n[service]\npid_file = {:?}\nsocket = {:?}\n{}\n\n\
             [sip]\nbind = \"127.0.0.1:{port}\"\ntransports = [\"udp\", \"tcp\"]\n{}\n\n\
             [call]\n{}\n{}\n\n{}[storage]\ndata_dir = {:?}\n\n[capture]\nenabled = true\ndir = {:?}\nkeep = \"{}\"\n{}\n\n\
             [database]\nurl = {:?}\n\n\
             {}\n\n{}\n",
            opts.log_extra,
            pid_file.display().to_string(),
            socket.display().to_string(),
            opts.service,
            opts.sip,
            opts.call,
            if opts.real_fax {
                ""
            } else {
                "fax_session = \"hold\""
            },
            match opts.web_port {
                Some(p) =>
                    format!("[web]\nenabled = true\nscheme = \"http\"\nbind = \"127.0.0.1:{p}\"\n"),
                None => "[web]\nenabled = false\n".to_string(),
            },
            data_dir.display().to_string(),
            calls_dir.display().to_string(),
            if opts.capture_keep_failed {
                "failed"
            } else {
                "all"
            },
            opts.capture_extra,
            db_url,
            if opts.no_trunk {
                String::new()
            } else {
                format!(
                    "[[trunks]]\nname = \"sipp\"\nallowed_sources = [\"127.0.0.0/8\"]\n{}",
                    opts.trunk
                )
            },
            opts.extra
        );
        let cfg_path = dir.join("sip-faxd.toml");
        std::fs::write(&cfg_path, cfg).expect("write config");
        Prepared {
            port,
            dir,
            calls_dir,
            data_dir,
            db_url,
            config_path: cfg_path,
            pid_file,
            socket,
            log_path,
        }
    }

    /// Builds the binary if needed, writes the configuration and waits until it is ready.
    pub fn start(name: &str, opts: &FaxdOptions) -> Faxd {
        let bin = faxd_binary();
        let Prepared {
            port,
            dir,
            calls_dir,
            data_dir,
            db_url,
            config_path: cfg_path,
            pid_file,
            socket,
            log_path,
        } = Faxd::prepare(name, opts);
        let mut child = Command::new(&bin)
            .args(["serve", "--config"])
            .arg(&cfg_path)
            .env_remove("RUST_LOG")
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn sip-faxd");
        let log = Arc::new(Mutex::new(Vec::new()));
        let l2 = log.clone();
        let stderr = child.stderr.take().expect("stderr");
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let mut ready = Some(ready_tx);
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                if line.contains("sip-faxd ready") {
                    if let Some(tx) = ready.take() {
                        let _ = tx.send(());
                    }
                }
                l2.lock().expect("log lock").push(line);
            }
        });
        if ready_rx.recv_timeout(Duration::from_secs(20)).is_err() {
            let _ = child.kill();
            panic!(
                "sip-faxd did not become ready:\n{}",
                log.lock().expect("lock").join("\n")
            );
        }
        Faxd {
            child: Some(child),
            port,
            dir,
            calls_dir,
            data_dir,
            db_url,
            config_path: cfg_path,
            pid_file,
            socket,
            log_path,
            log,
        }
    }

    /// Runs `sip-faxd <args> --config <config>` (a one-shot subcommand such as `db backfill`)
    /// and returns the exit code, stdout and stderr. `config` defaults to this server's own.
    pub fn cli(&self, args: &[&str], config: Option<&Path>) -> (Option<i32>, String, String) {
        let out = Command::new(faxd_binary())
            .args(args)
            .arg("--config")
            .arg(config.unwrap_or(&self.config_path))
            .env_remove("RUST_LOG")
            .output()
            .expect("run sip-faxd");
        (
            out.status.code(),
            String::from_utf8_lossy(&out.stdout).into_owned(),
            String::from_utf8_lossy(&out.stderr).into_owned(),
        )
    }

    /// `127.0.0.1:<port>`
    pub fn addr(&self) -> String {
        format!("127.0.0.1:{}", self.port)
    }

    /// The log lines so far.
    pub fn log(&self) -> Vec<String> {
        self.log.lock().expect("log lock").clone()
    }

    /// Waits (polling) until `n` call results have been logged; panics after 20 s.
    pub fn wait_results(&self, n: usize) -> Vec<serde_json::Value> {
        let deadline = Instant::now() + Duration::from_secs(20);
        loop {
            let r = self.results();
            if r.len() >= n {
                return r;
            }
            assert!(
                Instant::now() < deadline,
                "expected {n} call result(s), got {}:\n{}",
                r.len(),
                self.log().join("\n")
            );
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    /// The `call_result` JSON objects logged so far.
    pub fn results(&self) -> Vec<serde_json::Value> {
        self.log()
            .iter()
            .filter_map(|l| {
                let (_, json) = l.split_once("call_result: ")?;
                serde_json::from_str(json).ok()
            })
            .collect()
    }

    /// Stops the process with SIGTERM (graceful) and returns the complete log.
    pub fn stop(mut self) -> Vec<String> {
        self.terminate();
        self.log()
    }

    fn terminate(&mut self) {
        let Some(mut child) = self.child.take() else {
            return;
        };
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(child.id().to_string())
            .status();
        let deadline = Instant::now() + Duration::from_secs(15);
        loop {
            match child.try_wait() {
                Ok(Some(_)) => break,
                Ok(None) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(50))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    break;
                }
            }
        }
        // let the reader thread drain the pipe
        std::thread::sleep(Duration::from_millis(100));
    }
}

impl Drop for Faxd {
    fn drop(&mut self) {
        self.terminate();
    }
}

/// Settings of one SIPp run.
#[derive(Debug, Clone)]
pub struct SippOptions {
    /// Scenario file name inside `tools/sipp/` (or an absolute path).
    pub scenario: String,
    /// `ip:port` of the server under test; empty when SIPp is the server (UAS scenarios).
    pub remote: String,
    /// Local SIPp port (`-p`); a free one when `None`. UAS scenarios listen here.
    pub local_port: Option<u16>,
    /// `-m`: number of calls.
    pub calls: u32,
    /// `-t`: `u1` (UDP) or `t1` (TCP).
    pub transport: String,
    /// `-r`: calls per second (0 = SIPp default).
    pub rate: u32,
    /// Extra raw arguments (`-s`, `-set`, `-rrs`, ...).
    pub args: Vec<String>,
    /// Wall-clock limit for the whole run.
    pub timeout: Duration,
    /// Text replaced in the scenario file before the run (`(placeholder, value)`).
    pub substitutions: Vec<(String, String)>,
    /// Name of the scratch directory.
    pub name: String,
}

impl SippOptions {
    /// One UDP call of `scenario` against `remote`.
    pub fn new(name: &str, scenario: &str, remote: &str) -> Self {
        SippOptions {
            scenario: scenario.into(),
            remote: remote.into(),
            local_port: None,
            calls: 1,
            transport: "u1".into(),
            rate: 0,
            args: Vec::new(),
            timeout: Duration::from_secs(90),
            substitutions: Vec::new(),
            name: name.into(),
        }
    }
}

/// One line of SIPp's message log.
#[derive(Debug, Clone)]
pub struct LoggedMessage {
    /// Seconds since the epoch (microsecond resolution).
    pub at: f64,
    /// `true` = received by SIPp.
    pub received: bool,
    /// The message text (CRLF kept).
    pub text: String,
}

impl LoggedMessage {
    /// First line of the message.
    pub fn first_line(&self) -> &str {
        self.text.lines().next().unwrap_or("")
    }
}

/// Everything a SIPp run produced.
#[derive(Debug)]
pub struct SippRun {
    /// Process exit code (0 = all calls successful).
    pub code: Option<i32>,
    /// Captured stdout.
    pub stdout: String,
    /// Captured stderr.
    pub stderr: String,
    /// Final row of the statistics CSV: column name to value.
    pub stats: Vec<(String, String)>,
    /// The message log.
    pub messages: Vec<LoggedMessage>,
    /// Scratch directory with all SIPp output files.
    pub dir: PathBuf,
}

impl SippRun {
    /// A statistics column of the final row.
    pub fn stat(&self, name: &str) -> Option<&str> {
        self.stats
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    }

    /// A numeric statistics column.
    pub fn stat_u64(&self, name: &str) -> u64 {
        self.stat(name)
            .and_then(|v| v.trim().parse().ok())
            .unwrap_or_else(|| panic!("no numeric stat {name} in {:?}", self.stats))
    }

    /// Asserts exit code 0 and the expected `Successful call` / `Failed call` counters.
    pub fn assert_successful(&self, calls: u64) {
        let ctx = self.context();
        println!("{}", self.summary());
        assert_eq!(self.code, Some(0), "sipp exit code\n{ctx}");
        assert_eq!(
            self.stat_u64("SuccessfulCall(C)"),
            calls,
            "Successful call(s)\n{ctx}"
        );
        assert_eq!(self.stat_u64("FailedCall(C)"), 0, "Failed call(s)\n{ctx}");
    }

    /// One line with the counters the report quotes.
    pub fn summary(&self) -> String {
        let g = |k: &str| self.stat(k).unwrap_or("?").to_string();
        format!(
            "sipp exit={:?} TotalCallCreated={} SuccessfulCall(C)={} FailedCall(C)={} Retransmissions(C)={} messages={}",
            self.code,
            g("TotalCallCreated"),
            g("SuccessfulCall(C)"),
            g("FailedCall(C)"),
            g("Retransmissions(C)"),
            self.messages.len()
        )
    }

    /// Diagnostics for assertion messages.
    pub fn context(&self) -> String {
        let errs = std::fs::read_dir(&self.dir)
            .ok()
            .into_iter()
            .flatten()
            .filter_map(Result::ok)
            .filter(|e| e.file_name().to_string_lossy().contains("errors"))
            .filter_map(|e| std::fs::read_to_string(e.path()).ok())
            .collect::<Vec<_>>()
            .join("\n");
        format!(
            "--- sipp stderr ---\n{}\n--- sipp error log ---\n{errs}\n--- non-zero stats ---\n{:?}",
            self.stderr,
            self.stats
                .iter()
                .filter(|(k, v)| v != "0"
                    && !v.is_empty()
                    && !k.contains("Repartition")
                    && !k.contains("Time"))
                .collect::<Vec<_>>()
        )
    }

    /// Messages whose first line starts with `prefix`, with direction.
    pub fn messages_starting(&self, prefix: &str, received: bool) -> Vec<&LoggedMessage> {
        self.messages
            .iter()
            .filter(|m| m.received == received && m.first_line().starts_with(prefix))
            .collect()
    }
}

/// A SIPp process that has been started and not yet waited for.
pub struct SippHandle {
    child: Child,
    t_out: std::thread::JoinHandle<String>,
    t_err: std::thread::JoinHandle<String>,
    dir: PathBuf,
    timeout: Duration,
    /// The local port SIPp uses.
    pub local_port: u16,
}

impl SippHandle {
    /// Waits for SIPp to finish (killing it after the configured timeout) and collects the
    /// results.
    pub fn wait(mut self) -> SippRun {
        let deadline = Instant::now() + self.timeout;
        let code = loop {
            match self.child.try_wait().expect("wait sipp") {
                Some(st) => break st.code(),
                None if Instant::now() > deadline => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break Some(-999);
                }
                None => std::thread::sleep(Duration::from_millis(50)),
            }
        };
        let stdout = self.t_out.join().unwrap_or_default();
        let stderr = self.t_err.join().unwrap_or_default();
        SippRun {
            code,
            stdout,
            stderr,
            stats: parse_stats(&self.dir.join("stats.csv")),
            messages: parse_messages(&self.dir.join("messages.log")),
            dir: self.dir,
        }
    }
}

/// Runs SIPp to completion.
pub fn run_sipp(opts: &SippOptions) -> SippRun {
    spawn_sipp(opts).wait()
}

/// Starts SIPp (built-in substitutions: `@LOCAL_PORT@`, `@REMOTE_PORT@`).
pub fn spawn_sipp(opts: &SippOptions) -> SippHandle {
    let dir = scratch(&opts.name);
    let scenario_src = if Path::new(&opts.scenario).is_absolute() {
        PathBuf::from(&opts.scenario)
    } else {
        sipp_dir().join(&opts.scenario)
    };
    let mut xml = std::fs::read_to_string(&scenario_src)
        .unwrap_or_else(|e| panic!("cannot read {}: {e}", scenario_src.display()));
    let local_port = opts.local_port.unwrap_or_else(free_port);
    let remote_port = opts.remote.rsplit(':').next().unwrap_or("").to_string();
    xml = xml
        .replace("@LOCAL_PORT@", &local_port.to_string())
        .replace("@REMOTE_PORT@", &remote_port);
    for (from, to) in &opts.substitutions {
        assert!(
            xml.contains(from.as_str()),
            "placeholder {from} not in {}",
            scenario_src.display()
        );
        xml = xml.replace(from.as_str(), to);
    }
    let scenario = dir.join("scenario.xml");
    std::fs::write(&scenario, xml).expect("write scenario");
    let stats = dir.join("stats.csv");
    let msgs = dir.join("messages.log");
    let errs = dir.join("errors.log");
    let mut cmd = Command::new("sipp");
    cmd.arg("-sf").arg(&scenario);
    if !opts.remote.is_empty() {
        cmd.arg(&opts.remote);
    }
    cmd.args(["-i", "127.0.0.1", "-p", &local_port.to_string()])
        .args(["-m", &opts.calls.to_string()])
        .args(["-t", &opts.transport])
        .args(["-nostdin", "-trace_stat", "-trace_msg", "-trace_err"])
        .arg("-stf")
        .arg(&stats)
        .arg("-message_file")
        .arg(&msgs)
        .arg("-error_file")
        .arg(&errs)
        .args(["-stat_delimiter", ","])
        .args(&opts.args)
        .current_dir(&dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if opts.rate > 0 {
        cmd.args(["-r", &opts.rate.to_string(), "-rp", "1000"]);
    }
    let mut child = cmd
        .spawn()
        .expect("spawn sipp (is it installed? run inside the Rocky 9 container)");
    let (so, se) = (
        child.stdout.take().expect("stdout"),
        child.stderr.take().expect("stderr"),
    );
    let t_out = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut BufReader::new(so), &mut s);
        s
    });
    let t_err = std::thread::spawn(move || {
        let mut s = String::new();
        let _ = std::io::Read::read_to_string(&mut BufReader::new(se), &mut s);
        s
    });
    SippHandle {
        child,
        t_out,
        t_err,
        dir,
        timeout: opts.timeout,
        local_port,
    }
}

/// Last data row of a SIPp statistics CSV as (column, value) pairs.
pub fn parse_stats(path: &Path) -> Vec<(String, String)> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut lines = text.lines().filter(|l| !l.trim().is_empty());
    let Some(header) = lines.next() else {
        return Vec::new();
    };
    let Some(last) = lines.next_back() else {
        return Vec::new();
    };
    header
        .split(',')
        .map(|s| s.trim().to_string())
        .zip(last.split(',').map(|s| s.trim().to_string()))
        .collect()
}

/// Seconds since 1970 for `YYYY-MM-DDTHH:MM:SS.ffffff` (taken as UTC; only differences matter).
fn iso_seconds(ts: &str) -> Option<f64> {
    let (date, time) = ts.trim().split_once('T')?;
    let mut d = date.split('-');
    let (y, m, day): (i64, i64, i64) = (
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
        d.next()?.parse().ok()?,
    );
    let mut t = time.split(':');
    let (hh, mm): (f64, f64) = (t.next()?.parse().ok()?, t.next()?.parse().ok()?);
    let ss: f64 = t.next()?.parse().ok()?;
    // days from civil (Howard Hinnant)
    let y = if m <= 2 { y - 1 } else { y };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + day - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    Some(days as f64 * 86_400.0 + hh * 3600.0 + mm * 60.0 + ss)
}

/// Parses SIPp's `-trace_msg` log: blocks of
/// `---- <ISO timestamp>` / `UDP message sent|received [N] bytes:` / blank / message.
pub fn parse_messages(path: &Path) -> Vec<LoggedMessage> {
    let Ok(text) = std::fs::read_to_string(path) else {
        return Vec::new();
    };
    let mut out: Vec<LoggedMessage> = Vec::new();
    let mut cur: Option<LoggedMessage> = None;
    let mut expect_kind = false;
    for line in text.split('\n') {
        let l = line.trim_end_matches('\r');
        if l.starts_with("-----------") {
            if let Some(m) = cur.take() {
                out.push(m);
            }
            let stamp = l.trim_start_matches('-').trim();
            if let Some(at) = iso_seconds(stamp) {
                cur = Some(LoggedMessage {
                    at,
                    received: false,
                    text: String::new(),
                });
                expect_kind = true;
            }
            continue;
        }
        if let Some(m) = cur.as_mut() {
            if expect_kind {
                expect_kind = false;
                m.received = l.contains("message received");
                continue;
            }
            m.text.push_str(line);
            m.text.push('\n');
        }
    }
    if let Some(m) = cur.take() {
        out.push(m);
    }
    for m in &mut out {
        m.text = m.text.trim_matches(['\r', '\n']).to_string();
    }
    out
}

/// The Digest `response` (RFC 2617, MD5, no qop) the harness expects from a client.
pub fn digest_response(
    user: &str,
    realm: &str,
    pass: &str,
    nonce: &str,
    method: &str,
    uri: &str,
) -> String {
    use md5::{Digest, Md5};
    let md5 = |s: String| -> String {
        let mut h = Md5::new();
        h.update(s.as_bytes());
        h.finalize().iter().map(|b| format!("{b:02x}")).collect()
    };
    let ha1 = md5(format!("{user}:{realm}:{pass}"));
    let ha2 = md5(format!("{method}:{uri}"));
    md5(format!("{ha1}:{nonce}:{ha2}"))
}
