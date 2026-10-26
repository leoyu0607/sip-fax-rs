//! M3f (host side): the delivery configuration of the real `sip-faxd` executable. `config check`
//! prints the routes and never a secret, a broken route stops `config check` and `serve`, and
//! `serve` queues and delivers (catch-up) a fax that an earlier run stored. The live-call path
//! through SpanDSP is in `tools/fax-sim/tests/e2e_m3f.rs`.

use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use chrono::{Duration as CDuration, Utc};
use store::{DeliveryStatus, FaxFinal, FaxStatus, HangupDirection, NewFax};

fn bin() -> Command {
    Command::new(env!("CARGO_BIN_EXE_sip-faxd"))
}

fn scratch(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("m3f-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn slashes(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

fn url_of(dir: &Path) -> String {
    format!("sqlite://{}", slashes(&dir.join("fax.db")))
}

fn config(dir: &Path, routes: &str) -> PathBuf {
    let text = format!(
        "[sip]\nbind = \"127.0.0.1:0\"\n\
         [web]\nenabled = false\n[storage]\ndata_dir = {data:?}\n\
         [capture]\ndir = {calls:?}\n\
         [database]\nurl = {url:?}\n\
         [log]\nfile = \"\"\n\
         [service]\npid_file = {pid:?}\nsocket = {sock:?}\n\
         [[trunks]]\nname = \"local\"\nallowed_sources = [\"127.0.0.0/8\"]\n\
         {routes}",
        data = slashes(&dir.join("data")),
        calls = slashes(&dir.join("calls")),
        url = url_of(dir),
        // the run state belongs to the test, not to the default `run/` next to the executable
        pid = slashes(&dir.join("sip-faxd.pid")),
        sock = slashes(&dir.join("sip-faxd.sock")),
        routes = routes.replace("OUT", &slashes(&dir.join("out"))),
    );
    let p = dir.join("sip-faxd.toml");
    std::fs::write(&p, text).unwrap();
    p
}

fn run(args: &[&str], cfg: &Path) -> Output {
    bin()
        .args(args)
        .arg("--config")
        .arg(cfg)
        .output()
        .expect("run sip-faxd")
}

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

const SECRET_ROUTES: &str = "[[routes]]\nname = \"sales\"\ndnis_prefix = \"2557\"\ndeliver_partial = true\n\
    [[routes.deliveries]]\nkind = \"folder\"\ndir = \"OUT\"\n\
    [[routes.deliveries]]\nkind = \"email\"\nhost = \"smtp.example.com\"\nusername = \"fax\"\npassword = \"mailpw-123\"\nfrom = \"fax@example.com\"\nto = [\"ops@example.com\"]\n\
    [[routes.deliveries]]\nkind = \"webhook\"\nurl = \"https://hookuser:urlpw-456@example.com/h?token=tok-789\"\nsecret = \"hmac-secret-000\"\n\
    [[routes]]\ndefault = true\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"OUT/other\"\n";

#[test]
fn f20_config_check_prints_the_routes_and_hides_every_secret() {
    let dir = scratch("check");
    let cfg = config(&dir, SECRET_ROUTES);
    let o = run(&["config", "check"], &cfg);
    assert_eq!(o.status.code(), Some(0), "{}", err(&o));
    let text = out(&o);
    for must in [
        "routes: 2 (first match wins; catch-up at start: last 24 hours; up to 10 attempts per delivery)",
        "route 1 (sales): dnis_prefix = 2557; partial faxes delivered",
        "route 2: default (everything else); partial faxes not delivered",
        "email smtp.example.com:587 Starttls, user fax password ***",
        "webhook https://hookuser:***@example.com/h?token=***, HMAC X-Sipfax-Signature secret ***",
    ] {
        assert!(text.contains(must), "missing {must:?} in:\n{text}");
    }
    for secret in ["mailpw-123", "urlpw-456", "tok-789", "hmac-secret-000"] {
        assert!(!text.contains(secret), "{secret} leaked on stdout:\n{text}");
        assert!(
            !err(&o).contains(secret),
            "{secret} leaked on stderr:\n{}",
            err(&o)
        );
    }

    // no routes at all is said plainly
    let cfg = config(&dir, "[delivery]\ncatch_up_hours = 0\n");
    let text = out(&run(&["config", "check"], &cfg));
    assert!(text.contains("routes: none"), "{text}");
}

#[test]
fn f21_config_check_and_serve_refuse_broken_routes_without_leaking_the_password() {
    let dir = scratch("broken");
    // a webhook URL that is not a URL, an e-mail with credentials over a plain connection, and a
    // default route in the middle
    let routes = "[[routes]]\ndefault = true\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"OUT\"\n\
        [[routes]]\ndnis = \"1\"\n[[routes.deliveries]]\nkind = \"webhook\"\nurl = \"ftp://nope\"\n\
        [[routes.deliveries]]\nkind = \"email\"\nhost = \"h\"\nsecurity = \"none\"\nusername = \"u\"\npassword = \"mailpw-123\"\nfrom = \"a@b.c\"\nto = [\"d@e.f\"]\n";
    let cfg = config(&dir, routes);
    for cmd in [vec!["config", "check"], vec!["serve"]] {
        let o = run(&cmd, &cfg);
        assert_eq!(o.status.code(), Some(2), "{cmd:?}: {}", err(&o));
        let e = err(&o);
        assert!(e.contains("default route must be last"), "{e}");
        assert!(e.contains("webhook url must be http(s)"), "{e}");
        assert!(e.contains("security = \"none\""), "{e}");
        assert!(!e.contains("mailpw-123"), "{e}");
        assert!(o.stdout.is_empty(), "nothing on stdout: {}", out(&o));
    }
}

// ---------------------------------------------------------------- serve: catch-up

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

async fn stored_fax(dir: &Path, call_id: &str, started_hours_ago: i64) -> i64 {
    let st = store::connect(&url_of(dir)).await.unwrap();
    let faxes = dir.join("data/faxes/2026/10/21");
    std::fs::create_dir_all(&faxes).unwrap();
    std::fs::write(
        faxes.join(format!("{call_id}.pdf")),
        b"%PDF-1.4 stored earlier",
    )
    .unwrap();
    std::fs::write(
        faxes.join(format!("{call_id}.tif")),
        b"II*\0 stored earlier",
    )
    .unwrap();
    let started = Utc::now() - CDuration::hours(started_hours_ago);
    let new = NewFax {
        call_id: call_id.into(),
        trunk: "local".into(),
        ani: Some("0933".into()),
        ani_source: Some("from".into()),
        dnis: Some("2557001".into()),
        dnis_source: Some("ruri".into()),
        started_at: started,
        answered_at: None,
    };
    let fin = FaxFinal {
        ended_at: started + CDuration::minutes(1),
        answered_at: None,
        duration_sec: Some(60),
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
        tiff_path: Some(format!("faxes/2026/10/21/{call_id}.tif")),
        tiff_size: Some(19),
        pdf_path: Some(format!("faxes/2026/10/21/{call_id}.pdf")),
        pdf_size: Some(23),
        modem: None,
        media: None,
    };
    let (id, inserted) = st.upsert_finished(&new, &fin).await.unwrap();
    assert!(inserted);
    st.close().await;
    id
}

#[tokio::test]
async fn f22_serve_delivers_a_fax_an_earlier_run_stored_and_ignores_one_older_than_the_window() {
    let dir = scratch("catchup");
    let cfg = config(
        &dir,
        "[delivery]\ncatch_up_hours = 24\n[[routes]]\ndnis_prefix = \"2557\"\n\
         [[routes.deliveries]]\nkind = \"folder\"\ndir = \"OUT\"\nfilename = \"f{id}\"\n",
    );
    assert!(run(&["db", "migrate"], &cfg).status.success());
    let recent = stored_fax(&dir, "recent@gw", 2).await;
    let old = stored_fax(&dir, "old@gw", 48).await;

    let (mut child, rx) = start_serve(&cfg);
    wait_for(&rx, "delivery engine running");
    let want = dir.join("out").join(format!("f{recent}.pdf"));
    let tif = dir.join("out").join(format!("f{recent}.tif"));
    // the two formats are written one after the other: wait for both
    for _ in 0..500 {
        if want.is_file() && tif.is_file() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    assert_eq!(std::fs::read(&want).unwrap(), b"%PDF-1.4 stored earlier");
    assert_eq!(std::fs::read(&tif).unwrap(), b"II*\0 stored earlier");
    // the attempt row says done (written after the file; poll briefly)
    let st = store::connect(&url_of(&dir)).await.unwrap();
    let mut done = false;
    for _ in 0..250 {
        let a = st.list_deliveries(recent).await.unwrap();
        if a.len() == 1 && a[0].status == DeliveryStatus::Done {
            done = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    child.kill().unwrap();
    let _ = child.wait();
    assert!(done, "{:?}", st.list_deliveries(recent).await.unwrap());
    assert!(
        st.list_deliveries(old).await.unwrap().is_empty(),
        "started 48 h ago: outside the 24 h catch-up window"
    );
    assert!(!dir.join("out").join(format!("f{old}.pdf")).exists());
}

/// R05 with a real time zone: `serve` under `TZ=Asia/Taipei` names the file with the local time
/// of the server (UTC+8), not with UTC.
#[tokio::test]
async fn r05_serve_under_tz_asia_taipei_names_files_with_the_local_time() {
    let dir = scratch("tz");
    let cfg = config(
        &dir,
        "[delivery]\ncatch_up_hours = 24\n[[routes]]\ndnis_prefix = \"2557\"\n\
         [[routes.deliveries]]\nkind = \"folder\"\ndir = \"OUT\"\nformats = [\"pdf\"]\nfilename = \"{date}_{time}\"\n",
    );
    assert!(run(&["db", "migrate"], &cfg).status.success());
    let id = stored_fax(&dir, "tz@gw", 2).await;
    let st = store::connect(&url_of(&dir)).await.unwrap();
    let started = st.get(id).await.unwrap().unwrap().started_at;
    let taipei = started + CDuration::hours(8);
    let want = dir
        .join("out")
        .join(format!("{}.pdf", taipei.format("%Y%m%d_%H%M%S")));
    let utc_name = dir
        .join("out")
        .join(format!("{}.pdf", started.format("%Y%m%d_%H%M%S")));

    let mut child = bin()
        .arg("serve")
        .arg("--config")
        .arg(&cfg)
        .env("TZ", "Asia/Taipei")
        .stderr(Stdio::null())
        .stdout(Stdio::null())
        .spawn()
        .unwrap();
    for _ in 0..500 {
        if want.is_file() {
            break;
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    child.kill().unwrap();
    let _ = child.wait();
    let names: Vec<String> = std::fs::read_dir(dir.join("out"))
        .map(|d| {
            d.filter_map(Result::ok)
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    assert!(
        want.is_file(),
        "expected {want:?}, the folder has {names:?}"
    );
    assert!(
        !utc_name.exists(),
        "the UTC name must not be used: {names:?}"
    );
    st.close().await;
}
