//! M4e on the real `sip-faxd` executable: `db copy` (C07 refusals, C08 secrets, C09 accounts and
//! the web API on the copied database, C11 output). The copy itself is covered by the store suite
//! (`crates/store/src/tests/copy_tests.rs`); the end-to-end run with a real fax and PostgreSQL is
//! `tools/fax-sim/tests/e2e_m4e.rs`.
#![cfg(unix)]

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use store::{FaxFinal, FaxStatus, HangupDirection, NewFax};

const PW_ALICE: &str = "alice password 1";
const PW_BOB: &str = "bob password 22";

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_sip-faxd"));
    c.env("USER", "tester").env_remove("SUDO_USER");
    c
}

fn scratch(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("m4e-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn slashes(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

fn sqlite_url(dir: &Path, file: &str) -> String {
    format!("sqlite://{}", slashes(&dir.join(file)))
}

fn free_port() -> u16 {
    std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

/// Secrets of three kinds: a trunk registration password, an e-mail password, a webhook key.
fn runtime(sip_port: u16) -> String {
    format!(
        "[sip]\nbind = \"127.0.0.1:{sip_port}\"\n\
    [[trunks]]\nname = \"local\"\nallowed_sources = [\"127.0.0.0/8\"]\n\
    [trunks.register]\nenabled = true\nregistrar = \"127.0.0.1:5999\"\nusername = \"fax\"\npassword = \"trunk-reg-pw-1\"\n\
    [[routes]]\nname = \"mail\"\ndnis_prefix = \"2557\"\n\
    [[routes.deliveries]]\nkind = \"email\"\nhost = \"smtp.example.invalid\"\nusername = \"fax\"\npassword = \"mail-pw-2\"\nfrom = \"fax@example.com\"\nto = [\"ops@example.com\"]\n\
    [[routes.deliveries]]\nkind = \"webhook\"\nurl = \"https://example.com/hook\"\nsecret = \"hook-secret-3\"\n"
    )
}

const SECRETS: [&str; 3] = ["trunk-reg-pw-1", "mail-pw-2", "hook-secret-3"];

/// A configuration file `name` in `dir` whose database is `url`; the web interface listens on
/// `web_port` over plain HTTP. All files of the run (secret.key, the PID file) are next to it, so
/// two configuration files of one directory share `secret.key`.
fn config(dir: &Path, name: &str, url: &str, web_port: u16, runtime: &str) -> PathBuf {
    let text = format!(
        "[web]\nenabled = true\nscheme = \"http\"\nbind = \"127.0.0.1:{web_port}\"\n\
         [storage]\ndata_dir = {data:?}\n[capture]\nenabled = false\n\
         [database]\nurl = {url:?}\n[log]\nfile = \"\"\n\
         [service]\npid_file = {pid:?}\nsocket = {sock:?}\n{runtime}",
        data = slashes(&dir.join("data")),
        pid = slashes(&dir.join("sip-faxd.pid")),
        sock = slashes(&dir.join("sip-faxd.sock")),
    );
    let p = dir.join(name);
    std::fs::write(&p, text).unwrap();
    p
}

fn run(args: &[&str], cfg: &Path) -> Output {
    bin().args(args).arg("--config").arg(cfg).output().unwrap()
}

fn run_stdin(args: &[&str], cfg: &Path, input: &str) -> Output {
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

fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}

fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

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

fn until_ready(rx: &mpsc::Receiver<String>) -> String {
    let mut seen = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(l) => {
                let done = l.contains("sip-faxd ready");
                seen.push(l);
                if done {
                    return seen.join("\n");
                }
            }
            Err(_) => panic!("never ready; log so far:\n{}", seen.join("\n")),
        }
    }
}

fn stop(mut child: Child) {
    child.kill().unwrap();
    let _ = child.wait();
}

fn nf(call: &str, ani: &str) -> NewFax {
    NewFax {
        call_id: call.into(),
        trunk: "local".into(),
        ani: Some(ani.into()),
        ani_source: Some("from".into()),
        dnis: Some("0255".into()),
        dnis_source: Some("ruri".into()),
        started_at: Utc.timestamp_millis_opt(1_792_540_800_123).unwrap(),
        answered_at: None,
    }
}

fn ff(pages: i32) -> FaxFinal {
    FaxFinal {
        ended_at: Utc.timestamp_millis_opt(1_792_540_860_456).unwrap(),
        answered_at: None,
        duration_sec: Some(60),
        hangup_direction: HangupDirection::Local,
        hangup_code: "completed".into(),
        hangup_text: Some("正常完成".into()),
        sip_status: None,
        t30_code: Some(0),
        fax_status: FaxStatus::Received,
        pages,
        remote_ident: Some("0212345678".into()),
        resolution: None,
        bit_rate: None,
        compression: None,
        ecm: false,
        tiff_path: Some("faxes/a.tif".into()),
        tiff_size: Some(3),
        pdf_path: Some("faxes/a.pdf".into()),
        pdf_size: Some(3),
        modem: None,
        media: None,
    }
}

/// A source database set up with the commands an operator would use: the runtime configuration
/// (with secrets) imported, two accounts, three faxes (one read, one with a note).
struct Source {
    dir: PathBuf,
    cfg: PathBuf,
    url: String,
    web_port: u16,
    ids: Vec<i64>,
}

async fn source(name: &str) -> Source {
    let dir = scratch(name);
    let url = sqlite_url(&dir, "a.db");
    let web_port = free_port();
    let cfg = config(&dir, "a.toml", &url, web_port, &runtime(free_port()));
    let o = run(&["config", "import"], &cfg);
    assert!(o.status.success(), "{}", err(&o));
    for (name, role, pw) in [("alice", "admin", PW_ALICE), ("bob", "agent", PW_BOB)] {
        let o = run_stdin(
            &["user", "add", name, "--role", role, "--password-stdin"],
            &cfg,
            &format!("{pw}\n"),
        );
        assert!(o.status.success(), "{}", err(&o));
    }
    let st = store::connect(&url).await.unwrap();
    let mut ids = Vec::new();
    for (k, ani) in ["0211111111", "0222222222", "0233333333"]
        .iter()
        .enumerate()
    {
        let id = st
            .insert_receiving(&nf(&format!("call-{k}"), ani))
            .await
            .unwrap();
        st.finalize(id, &ff(k as i32 + 1)).await.unwrap();
        ids.push(id);
    }
    st.set_read(ids[0], true, "alice").await.unwrap();
    st.set_note(ids[1], "回電給客戶 📞 ref#42", "bob", None)
        .await
        .unwrap();
    st.add_delivery(&store::NewDelivery {
        fax_id: ids[0],
        target: "folder:/srv/fax".into(),
        kind: "folder".into(),
        attempt: 1,
        status: store::DeliveryStatus::Done,
        next_try_at: None,
    })
    .await
    .unwrap();
    st.set_setting("ui.title", "傳真 📠", Some("alice"))
        .await
        .unwrap();
    let alice = st.user_get_by_name("alice").await.unwrap().unwrap();
    st.session_create(&store::NewSession {
        token_hash: "ab".repeat(32),
        user_id: alice.id,
        created_at: Utc::now(),
        expires_at: Utc::now() + chrono::Duration::days(7),
        ip: Some("203.0.113.5".into()),
        user_agent: Some("測試 browser".into()),
    })
    .await
    .unwrap();
    st.close().await;
    Source {
        dir,
        cfg,
        url,
        web_port,
        ids,
    }
}

/// Every table's row count of a database (read-only).
async fn counts(url: &str) -> Vec<(String, u64)> {
    store::test_row_counts(url)
        .await
        .unwrap()
        .into_iter()
        .map(|(t, n)| (t.to_string(), n))
        .collect()
}

// ---------------------------------------------------------------- C07
#[tokio::test(flavor = "multi_thread")]
async fn c07_db_copy_refuses_while_the_service_runs_the_same_database_and_a_bad_url() {
    let s = source("c07").await;
    let target_file = s.dir.join("t.db");
    let target = sqlite_url(&s.dir, "t.db");
    let before = counts(&s.url).await;
    assert_eq!(before.iter().find(|(t, _)| t == "faxes").unwrap().1, 3);

    // the service is running: its PID file is locked
    let (child, rx) = start_serve(&s.cfg);
    until_ready(&rx);
    let o = run(&["db", "copy", "--to", &target], &s.cfg);
    assert_eq!(o.status.code(), Some(1), "{}{}", out(&o), err(&o));
    assert!(
        err(&o).contains("the service is running (pid"),
        "{}",
        err(&o)
    );
    assert!(err(&o).contains("stop it first"), "{}", err(&o));
    assert!(
        !target_file.exists(),
        "the target database was not even created"
    );
    assert!(
        !out(&o).contains("copying"),
        "nothing was started: {}",
        out(&o)
    );
    stop(child);

    // the same URL, written exactly as in the configuration
    let o = run(&["db", "copy", "--to", &s.url], &s.cfg);
    assert_eq!(o.status.code(), Some(1), "{}", out(&o));
    assert!(
        err(&o).contains("the target is the source database"),
        "{}",
        err(&o)
    );
    assert_eq!(counts(&s.url).await, before, "the source is unchanged");

    // the same file spelled differently: refused because the target is not empty, nothing changes
    let alias = format!("sqlite://{}/./a.db", slashes(&s.dir));
    let o = run(&["db", "copy", "--to", &alias], &s.cfg);
    assert_eq!(o.status.code(), Some(1), "{}", out(&o));
    assert!(err(&o).contains("not empty"), "{}", err(&o));
    assert_eq!(counts(&s.url).await, before, "the source is unchanged");

    // bad URLs: unknown scheme, not a URL at all, an empty one
    for bad in ["ftp://example.com/x", "not a url", "oracle://u:p@h/x", ""] {
        let o = run(&["db", "copy", "--to", bad], &s.cfg);
        assert_eq!(o.status.code(), Some(1), "{bad:?}: {}", out(&o));
        assert!(err(&o).contains("--to:"), "{bad:?}: {}", err(&o));
        assert!(out(&o).is_empty(), "{bad:?} printed {}", out(&o));
    }
    assert!(!target_file.exists());
    assert_eq!(counts(&s.url).await, before);

    // and once the service is stopped (the lock is gone with the process) the copy works
    let o = run(&["db", "copy", "--to", &target], &s.cfg);
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    assert!(target_file.is_file());
    assert_eq!(counts(&target).await, before);
}

// ---------------------------------------------------------------- C11
#[tokio::test(flavor = "multi_thread")]
async fn c11_output_has_the_rows_per_table_masks_the_password_and_tells_the_next_step() {
    let s = source("c11").await;
    let target = sqlite_url(&s.dir, "t.db");
    let o = run(&["db", "copy", "--to", &target], &s.cfg);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    let want = counts(&s.url).await;
    for (table, n) in &want {
        let line = text
            .lines()
            .find(|l| l.trim_start().starts_with(table.as_str()) && l.trim_end().ends_with("rows"))
            .unwrap_or_else(|| panic!("a line for {table}: {text}"));
        let got: u64 = line.split_whitespace().nth(1).unwrap().parse().unwrap();
        assert_eq!(got, *n, "rows of {table}: {line}");
    }
    assert!(
        want.iter().all(|(_, n)| *n > 0),
        "every table has rows: {want:?}"
    );
    let total: u64 = want.iter().map(|(_, n)| n).sum();
    assert!(
        text.contains(&format!(
            "done: {total} rows from sqlite into sqlite (schema version 3)"
        )),
        "{text}"
    );
    assert!(text.contains("verified"), "{text}");
    assert!(
        text.contains(&format!("copying {} -> {target}", s.url)),
        "{text}"
    );
    // the hint names the key to keep and the setting to change
    let hint = text
        .lines()
        .find(|l| l.starts_with("next:"))
        .unwrap_or_else(|| panic!("{text}"));
    assert!(
        hint.contains("[database]") && hint.contains(&target),
        "{hint}"
    );
    assert!(
        hint.contains("secret.key") && hint.contains("start the service"),
        "{hint}"
    );

    // a password in the URL is shown as *** in the progress line and never in an error
    let s2 = source("c11b").await;
    let secret_url = "postgres://faxuser:Sup3rS3cret@127.0.0.1:1/faxdb";
    let o = run(&["db", "copy", "--to", secret_url], &s2.cfg);
    assert_eq!(o.status.code(), Some(1), "{}", out(&o));
    assert!(
        out(&o).contains("-> postgres://faxuser:***@127.0.0.1:1/faxdb"),
        "{}",
        out(&o)
    );
    for stream in [out(&o), err(&o)] {
        assert!(
            !stream.contains("Sup3rS3cret"),
            "the password leaked: {stream}"
        );
    }
    assert!(err(&o).contains("db copy:"), "{}", err(&o));
}

// ---------------------------------------------------------------- C08
#[tokio::test(flavor = "multi_thread")]
async fn c08_secrets_copy_as_ciphertext_and_decrypt_with_the_same_key_only() {
    let s = source("c08").await;
    let target = sqlite_url(&s.dir, "b.db");
    let o = run(&["db", "copy", "--to", &target], &s.cfg);
    assert!(o.status.success(), "{}", err(&o));

    // the ciphertext is the same row for row and no plaintext is in the target
    let src = store::connect(&s.url).await.unwrap();
    let dst = store::connect(&target).await.unwrap();
    let (a, b) = (
        src.config_list().await.unwrap(),
        dst.config_list().await.unwrap(),
    );
    assert_eq!(a, b, "config_sections identical");
    let blob: String = b
        .iter()
        .map(|r| r.body.clone())
        .collect::<Vec<_>>()
        .join("\n");
    assert!(
        blob.matches("enc:v1:").count() >= 3,
        "three secrets are encrypted: {blob}"
    );
    for secret in SECRETS {
        assert!(
            !blob.contains(secret),
            "{secret} must not be in the target as plaintext"
        );
    }
    assert_eq!(
        src.config_audit_list(100, None).await.unwrap(),
        dst.config_audit_list(100, None).await.unwrap()
    );
    src.close().await;
    dst.close().await;

    // a configuration file for the new database (same directory: the same conf/secret.key)
    let cfg_b = config(&s.dir, "b.toml", &target, free_port(), "");
    let o = run(&["config", "check"], &cfg_b);
    assert!(o.status.success(), "config check: {}{}", out(&o), err(&o));
    assert!(!err(&o).contains("cannot be decrypted"), "{}", err(&o));
    assert!(out(&o).contains("stored section(s)"), "{}", out(&o));
    let new = run(&["config", "export", "--include-secrets"], &cfg_b);
    assert!(new.status.success(), "{}", err(&new));
    let old = run(&["config", "export", "--include-secrets"], &s.cfg);
    for secret in SECRETS {
        assert!(
            out(&new).contains(&format!("\"{secret}\"")),
            "{secret} decrypted from the copy:\n{}",
            out(&new)
        );
    }
    assert_eq!(
        out(&new),
        out(&old),
        "the copy exports exactly what the source exports"
    );

    // control: with another key the same database does not decrypt
    let other = scratch("c08-other-key");
    std::fs::write(other.join("secret.key"), [7u8; 32]).unwrap();
    let cfg_c = config(&other, "c.toml", &target, free_port(), "");
    let o = run(&["config", "check"], &cfg_c);
    assert_eq!(o.status.code(), Some(1), "{}{}", out(&o), err(&o));
    assert!(err(&o).contains("cannot be decrypted"), "{}", err(&o));
    let wrong = run(&["config", "export", "--include-secrets"], &cfg_c);
    for secret in SECRETS {
        assert!(
            !out(&wrong).contains(secret),
            "{secret} must not appear with the wrong key"
        );
    }
}

// ---------------------------------------------------------------- C09
fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap()
}

async fn login(
    c: &reqwest::Client,
    base: &str,
    user: &str,
    pw: &str,
) -> (reqwest::StatusCode, Option<String>) {
    let r = c
        .post(format!("{base}/api/v1/auth/login"))
        .json(&serde_json::json!({"username": user, "password": pw}))
        .send()
        .await
        .unwrap();
    let status = r.status();
    let cookie = r
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("sipfax_session="))
        .map(|v| v.split(';').next().unwrap().to_string());
    (status, cookie)
}

#[tokio::test(flavor = "multi_thread")]
async fn c09_the_web_api_on_the_copied_database_keeps_accounts_sessions_read_flags_and_notes() {
    let s = source("c09").await;
    let c = client();

    // a session on the source: log in over HTTP, then stop the service
    let (child, rx) = start_serve(&s.cfg);
    until_ready(&rx);
    let base_a = format!("http://127.0.0.1:{}", s.web_port);
    let (st, old_cookie) = login(&c, &base_a, "alice", PW_ALICE).await;
    assert_eq!(st, 200);
    let old_cookie = old_cookie.expect("a session cookie");
    stop(child);

    let target = sqlite_url(&s.dir, "b.db");
    let o = run(&["db", "copy", "--to", &target], &s.cfg);
    assert!(o.status.success(), "{}{}", out(&o), err(&o));

    // start on the copy
    let port_b = free_port();
    let cfg_b = config(&s.dir, "b.toml", &target, port_b, "");
    let (child, rx) = start_serve(&cfg_b);
    let log = until_ready(&rx);
    let base = format!("http://127.0.0.1:{port_b}");
    assert!(
        !log.contains("created the administrator"),
        "no new admin: the accounts came along\n{log}"
    );

    // the old cookie, nothing else, is a valid session of alice
    let me = c
        .get(format!("{base}/api/v1/auth/me"))
        .header("Cookie", &old_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(
        me.status(),
        200,
        "the session cookie of the source works on the copy"
    );
    let me: serde_json::Value = me.json().await.unwrap();
    assert_eq!(me["user"]["username"], "alice", "{me}");
    // control: a cookie that is in no database is refused
    let bogus = c
        .get(format!("{base}/api/v1/auth/me"))
        .header(
            "Cookie",
            "sipfax_session=0000000000000000000000000000000000000000000000000000000000000000",
        )
        .send()
        .await
        .unwrap();
    assert_eq!(bogus.status(), 401);

    // logins: the old passwords work, a wrong one does not
    let (st, cookie) = login(&c, &base, "alice", PW_ALICE).await;
    assert_eq!(st, 200);
    assert!(cookie.is_some());
    let (st, _) = login(&c, &base, "bob", PW_BOB).await;
    assert_eq!(st, 200);
    let (st, cookie) = login(&c, &base, "alice", "not the password").await;
    assert_eq!(st, 401);
    assert!(cookie.is_none());

    // GET /faxes: ids, read flags and notes as they were
    let r = c
        .get(format!("{base}/api/v1/faxes"))
        .header("Cookie", &old_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let v: serde_json::Value = r.json().await.unwrap();
    let items = v["items"].as_array().unwrap();
    assert_eq!(items.len(), 3, "{v}");
    let by_id = |id: i64| {
        items
            .iter()
            .find(|f| f["id"] == id)
            .unwrap_or_else(|| panic!("fax {id} in {v}"))
    };
    let (f0, f1, f2) = (by_id(s.ids[0]), by_id(s.ids[1]), by_id(s.ids[2]));
    assert_eq!(f0["is_read"], true);
    assert_eq!(f0["read_changed_by"], "alice");
    assert_eq!(
        (f1["is_read"].clone(), f2["is_read"].clone()),
        (false.into(), false.into())
    );
    assert_eq!(f1["note"], "回電給客戶 📞 ref#42");
    assert_eq!(f1["note_updated_by"], "bob");
    assert!(f0["note"].is_null() && f2["note"].is_null(), "{v}");
    assert_eq!(f0["ani"], "0211111111");
    assert_eq!(f2["pages"], 3);
    // and one fax by id
    let r = c
        .get(format!("{base}/api/v1/faxes/{}", s.ids[1]))
        .header("Cookie", &old_cookie)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let one: serde_json::Value = r.json().await.unwrap();
    assert_eq!(one["note"], "回電給客戶 📞 ref#42");
    stop(child);
}
