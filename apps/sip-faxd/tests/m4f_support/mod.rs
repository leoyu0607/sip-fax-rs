//! Shared by `m4f_cli.rs` and `m4f_server.rs` (included with `mod m4f_support;`).
#![allow(dead_code)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

pub fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_sip-faxd"));
    c.env("USER", "tester")
        .env_remove("SUDO_USER")
        .env_remove("RUST_LOG");
    c
}

pub fn scratch(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("m4f-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

pub fn slashes(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

pub fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// A configuration file `name` in `dir`. `database` is the body of `[database]` (lines);
/// `web_port` is the plain-HTTP web port (`None`: the web interface is off); `extra` is appended.
pub fn config(
    dir: &Path,
    name: &str,
    database: &str,
    web_port: Option<u16>,
    extra: &str,
) -> PathBuf {
    let web = match web_port {
        Some(p) => format!("[web]\nenabled = true\nscheme = \"http\"\nbind = \"127.0.0.1:{p}\"\n"),
        None => "[web]\nenabled = false\n".to_string(),
    };
    let text = format!(
        "{web}[storage]\ndata_dir = {data:?}\n\
         [database]\n{database}\n[log]\nfile = \"\"\n\
         [service]\npid_file = {pid:?}\nsocket = {sock:?}\nstop_timeout_secs = 0\n{extra}",
        data = slashes(&dir.join("data")),
        pid = slashes(&dir.join("f.pid")),
        sock = slashes(&dir.join("f.sock")),
    );
    let p = dir.join(name);
    std::fs::write(&p, text).unwrap();
    p
}

pub fn sqlite_fields(dir: &Path, file: &str) -> String {
    format!("type = \"sqlite\"\npath = {:?}\n", slashes(&dir.join(file)))
}

pub fn sqlite_url(dir: &Path, file: &str) -> String {
    format!("sqlite://{}", slashes(&dir.join(file)))
}

pub fn run(args: &[&str], cfg: &Path) -> Output {
    bin().args(args).arg("--config").arg(cfg).output().unwrap()
}

/// `sip-faxd <args>` with `input` on stdin (no `--config` added).
pub fn run_raw_stdin(args: &[&str], input: &str) -> Output {
    let mut child = bin()
        .args(args)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

pub fn run_stdin(args: &[&str], cfg: &Path, input: &str) -> Output {
    let mut child = bin()
        .args(args)
        .arg("--config")
        .arg(cfg)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    child.wait_with_output().unwrap()
}

pub fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

pub fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// stdout and stderr together.
pub fn both(o: &Output) -> String {
    format!("{}\n{}", out(o), err(o))
}

/// A running `sip-faxd serve` whose stderr is collected.
pub struct Serve {
    pub child: Child,
    log: Arc<Mutex<Vec<String>>>,
}

impl Serve {
    pub fn start(cfg: &Path) -> Serve {
        let mut child = bin()
            .arg("serve")
            .arg("--config")
            .arg(cfg)
            .stderr(Stdio::piped())
            .stdout(Stdio::null())
            .spawn()
            .unwrap();
        let stderr = child.stderr.take().unwrap();
        let log = Arc::new(Mutex::new(Vec::new()));
        let l2 = log.clone();
        std::thread::spawn(move || {
            for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                l2.lock().unwrap().push(line);
            }
        });
        Serve { child, log }
    }

    pub fn log(&self) -> String {
        self.log.lock().unwrap().join("\n")
    }

    pub fn wait_for(&mut self, needle: &str) -> String {
        let end = Instant::now() + Duration::from_secs(60);
        loop {
            let l = self.log();
            if l.contains(needle) {
                return l;
            }
            if let Ok(Some(st)) = self.child.try_wait() {
                panic!("sip-faxd exited ({st}) before {needle:?}:\n{l}");
            }
            assert!(Instant::now() < end, "no {needle:?} within 60 s:\n{l}");
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    pub fn ready(&mut self) -> String {
        self.wait_for("sip-faxd ready")
    }

    /// SIGTERM, wait, and the whole log.
    pub fn stop(mut self) -> String {
        let _ = Command::new("kill")
            .arg("-TERM")
            .arg(self.child.id().to_string())
            .status();
        let start = Instant::now();
        let end = start + Duration::from_secs(20);
        loop {
            match self.child.try_wait() {
                Ok(Some(_)) => break,
                _ if Instant::now() > end => {
                    let _ = self.child.kill();
                    let _ = self.child.wait();
                    break;
                }
                _ => std::thread::sleep(Duration::from_millis(50)),
            }
        }
        std::thread::sleep(Duration::from_millis(150));
        eprintln!("stop took {:?}", start.elapsed());
        self.log()
    }
}

impl Drop for Serve {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// The password used wherever a test needs one that URL syntax would break.
pub const NASTY: &str = "Aa1@:/?#%41&+ 中😀x";

/// The password as it would appear percent-encoded in a URL.
pub fn encoded(pw: &str) -> String {
    percent_encoding::utf8_percent_encode(pw, percent_encoding::NON_ALPHANUMERIC).to_string()
}

/// Panics when `text` shows the password (plain, or percent-encoded in any of the usual ways).
pub fn assert_no_password(what: &str, pw: &str, text: &str) {
    for form in [
        pw.to_string(),
        encoded(pw),
        pw.replace(' ', "+"),
        pw.replace(' ', "%20"),
    ] {
        assert!(
            !text.contains(&form),
            "{what} shows the password ({form}):\n{text}"
        );
    }
}
