//! M4a on the real `sip-faxd` executable and a real SQLite file: the `user`, `fax` and `config`
//! subcommands (A13, M14, K03, K04), what `serve` does with the runtime configuration at start
//! (K01, K02, K11, A01) and `config check` against the database.

use std::io::{BufRead, BufReader, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::sync::mpsc;
use std::time::Duration;

use chrono::{TimeZone, Utc};
use store::{
    DeliveryStatus, FaxFinal, FaxStatus, FaxStore, HangupDirection, NewDelivery, NewFax,
};

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_sip-faxd"));
    // the operator recorded for changes: cli:tester
    c.env("USER", "tester").env("USERNAME", "tester").env_remove("SUDO_USER");
    c
}

fn scratch(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("m4a-{}-{name}", std::process::id()));
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

const RUNTIME: &str = "[sip]\nbind = \"127.0.0.1:0\"\n[call]\nmax_duration_secs = 600\n\
    [[trunks]]\nname = \"local\"\nallowed_sources = [\"127.0.0.0/8\"]\n\
    [[routes]]\nname = \"hooks\"\ndnis_prefix = \"2557\"\n\
    [[routes.deliveries]]\nkind = \"webhook\"\nurl = \"https://example.com/hook\"\nsecret = \"hook-secret-1\"\n";

/// A configuration file; `extra` is added to the runtime tables.
fn config(dir: &Path, extra: &str) -> PathBuf {
    let text = format!(
        "[storage]\ndata_dir = {data:?}\n[capture]\nenabled = false\n[database]\nurl = {url:?}\n{RUNTIME}{extra}",
        data = slashes(&dir.join("data")),
        url = url_of(dir),
    );
    let p = dir.join("sip-faxd.toml");
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
    child.stdin.take().unwrap().write_all(input.as_bytes()).unwrap();
    child.wait_with_output().unwrap()
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

// ---------------------------------------------------------------- A13
#[tokio::test(flavor = "multi_thread")]
async fn a13_user_commands_create_list_disable_enable_and_change_passwords() {
    let dir = scratch("a13");
    let cfg = config(&dir, "");
    let o = run_stdin(&["user", "add", "root", "--role", "admin", "--password-stdin"], &cfg, "root password 1\n");
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("user root (admin) created"), "{}", out(&o));
    let o = run_stdin(&["user", "add", "alice", "--password-stdin", "--display-name", "Alice A", "--must-change"], &cfg, "alice password 1\r\n");
    assert!(o.status.success(), "{}", err(&o));
    let st = open(&dir).await;
    let alice = st.user_get_by_name("alice").await.unwrap().unwrap();
    assert_eq!(
        (alice.role.as_str(), alice.display_name.as_deref(), alice.must_change_password),
        ("agent", Some("Alice A"), true),
        "agent is the default role"
    );
    assert!(accounts::password::verify(&alice.password_hash, "alice password 1"), "CRLF of a Windows pipe is stripped");
    assert!(!alice.password_hash.contains("alice password 1"));
    // refusals: weak password, duplicate, bad role, bad name: exit 1, a fixed code, nothing created
    for (args, input, code) in [
        (vec!["user", "add", "bob", "--password-stdin"], "short\n", "password_too_short"),
        (vec!["user", "add", "alice", "--password-stdin"], "another password 1\n", "username_taken"),
        (vec!["user", "add", "Bad Name", "--password-stdin"], "another password 1\n", "bad_username"),
    ] {
        let o = run_stdin(&args, &cfg, input);
        assert_eq!(o.status.code(), Some(1), "{args:?}");
        assert!(err(&o).contains(code), "{args:?}: {}", err(&o));
    }
    let o = run_stdin(&["user", "add", "bob", "--role", "root", "--password-stdin"], &cfg, "another password 1\n");
    assert_eq!(o.status.code(), Some(1));
    assert!(st.user_get_by_name("bob").await.unwrap().is_none());
    let o = run_stdin(&["user", "add", "bob", "--password-stdin"], &cfg, "");
    assert_eq!(o.status.code(), Some(1), "no password on stdin");
    // list
    let o = run(&["user", "list"], &cfg);
    let t = out(&o);
    assert!(t.contains("root") && t.contains("admin") && t.contains("alice") && t.contains("must-change"), "{t}");
    // an open session ends with the disable
    st.session_create(&store::NewSession {
        token_hash: "f".repeat(64),
        user_id: alice.id,
        created_at: Utc::now(),
        expires_at: Utc::now() + chrono::Duration::hours(1),
        ip: None,
        user_agent: None,
    })
    .await
    .unwrap();
    let o = run(&["user", "disable", "alice"], &cfg);
    assert!(o.status.success(), "{}", err(&o));
    assert!(st.user_get_by_name("alice").await.unwrap().unwrap().disabled);
    assert_eq!(st.session_count_for_user(alice.id).await.unwrap(), 0);
    assert!(out(&run(&["user", "list"], &cfg)).contains("disabled"));
    let o = run(&["user", "enable", "alice"], &cfg);
    assert!(o.status.success());
    assert!(!st.user_get_by_name("alice").await.unwrap().unwrap().disabled);
    // the last enabled administrator is protected
    let o = run(&["user", "disable", "root"], &cfg);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("last_admin"), "{}", err(&o));
    assert!(!st.user_get_by_name("root").await.unwrap().unwrap().disabled);
    let o = run(&["user", "disable", "nobody"], &cfg);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("not_found"));
    // passwd: the old password stops working, the new one does
    let o = run_stdin(&["user", "passwd", "alice", "--password-stdin"], &cfg, "alice password 2\n");
    assert!(o.status.success(), "{}", err(&o));
    let alice = st.user_get_by_name("alice").await.unwrap().unwrap();
    assert!(accounts::password::verify(&alice.password_hash, "alice password 2"));
    assert!(!accounts::password::verify(&alice.password_hash, "alice password 1"));
    assert!(!alice.must_change_password);
    let o = run_stdin(&["user", "passwd", "alice", "--password-stdin"], &cfg, "short\n");
    assert_eq!(o.status.code(), Some(1));
    assert!(accounts::password::verify(&st.user_get_by_name("alice").await.unwrap().unwrap().password_hash, "alice password 2"));
    // every operation is in the audit trail with the operator
    let rows = st.config_audit_list(100, None).await.unwrap();
    for (action, section) in [
        ("user_add", "user:root"),
        ("user_add", "user:alice"),
        ("user_disable", "user:alice"),
        ("user_enable", "user:alice"),
        ("password_reset", "user:alice"),
    ] {
        assert!(
            rows.iter().any(|r| r.action == action && r.section == section && r.by == "cli:tester"),
            "{action} {section} by cli:tester in {rows:?}"
        );
    }
    assert!(
        !rows.iter().any(|r| r.action == "user_disable" && r.section == "user:root"),
        "the refused change left no row"
    );
}

// ---------------------------------------------------------------- M14
fn nf(call: &str, ani: &str) -> NewFax {
    NewFax {
        call_id: call.into(),
        trunk: "local".into(),
        ani: Some(ani.into()),
        ani_source: Some("from".into()),
        dnis: Some("0255".into()),
        dnis_source: Some("ruri".into()),
        started_at: Utc.timestamp_millis_opt(1_792_540_800_000).unwrap(),
        answered_at: None,
    }
}

fn ff(status: FaxStatus, pages: i32) -> FaxFinal {
    FaxFinal {
        ended_at: Utc.timestamp_millis_opt(1_792_540_860_000).unwrap(),
        answered_at: None,
        duration_sec: Some(60),
        hangup_direction: HangupDirection::Local,
        hangup_code: "completed".into(),
        hangup_text: None,
        sip_status: None,
        t30_code: Some(0),
        fax_status: status,
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
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn m14_fax_commands_show_the_database_and_record_cli_as_the_operator() {
    let dir = scratch("m14");
    let cfg = config(&dir, "");
    let st = open(&dir).await;
    let (a, _) = st.upsert_finished(&nf("call-a", "0211111111"), &ff(FaxStatus::Received, 2)).await.unwrap();
    let (b, _) = st.upsert_finished(&nf("call-b", "0222222222"), &ff(FaxStatus::Failed, 0)).await.unwrap();
    st.set_read(b, true, "web-user").await.unwrap();
    // list: both; --unread: only a; --status failed: only b; --limit 1
    let o = run(&["fax", "list"], &cfg);
    assert!(o.status.success(), "{}", err(&o));
    let t = out(&o);
    assert!(t.contains("0211111111") && t.contains("0222222222"), "{t}");
    assert!(t.lines().any(|l| l.contains("0211111111") && l.ends_with(" UNREAD")), "{t}");
    assert!(t.lines().any(|l| l.contains("0222222222") && l.ends_with(" read")), "{t}");
    let t = out(&run(&["fax", "list", "--unread"], &cfg));
    assert!(t.contains("0211111111") && !t.contains("0222222222"), "{t}");
    let t = out(&run(&["fax", "list", "--status", "failed"], &cfg));
    assert!(t.contains("0222222222") && !t.contains("0211111111"), "{t}");
    let t = out(&run(&["fax", "list", "--limit", "1"], &cfg));
    assert_eq!(t.lines().count(), 2, "header and one row: {t}");
    let o = run(&["fax", "list", "--status", "bogus"], &cfg);
    assert_eq!(o.status.code(), Some(1));
    // show
    let o = run(&["fax", "show", &a.to_string()], &cfg);
    assert!(o.status.success(), "{}", err(&o));
    let t = out(&o);
    for needle in ["call-a", "0211111111 (from)", "received", "pages           2", "unread", "faxes/a.pdf", "deliveries      none"] {
        assert!(t.contains(needle), "{needle:?} in:\n{t}");
    }
    let o = run(&["fax", "show", "9999"], &cfg);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("no fax with id 9999"));
    // read / unread: the operator is cli:tester
    let o = run(&["fax", "read", &a.to_string()], &cfg);
    assert!(o.status.success());
    let r = st.get(a).await.unwrap().unwrap();
    assert!(r.is_read);
    assert_eq!(r.read_changed_by.as_deref(), Some("cli:tester"));
    let o = run(&["fax", "read", &a.to_string(), "--unread"], &cfg);
    assert!(o.status.success());
    let r = st.get(a).await.unwrap().unwrap();
    assert!(!r.is_read);
    assert_eq!(r.read_changed_by.as_deref(), Some("cli:tester"));
    assert_eq!(run(&["fax", "read", "9999"], &cfg).status.code(), Some(1));
    // note: set, replace, clear
    let o = run(&["fax", "note", &a.to_string(), "call back \u{4e2d}\u{6587} after 3pm"], &cfg);
    assert!(o.status.success(), "{}", err(&o));
    let r = st.get(a).await.unwrap().unwrap();
    assert_eq!(r.note.as_deref(), Some("call back \u{4e2d}\u{6587} after 3pm"));
    assert_eq!(r.note_updated_by.as_deref(), Some("cli:tester"));
    let t = out(&run(&["fax", "show", &a.to_string()], &cfg));
    assert!(t.contains("call back") && t.contains("by cli:tester"), "{t}");
    run(&["fax", "note", &a.to_string(), "second"], &cfg);
    assert_eq!(st.get(a).await.unwrap().unwrap().note.as_deref(), Some("second"));
    run(&["fax", "note", &a.to_string(), ""], &cfg);
    assert_eq!(st.get(a).await.unwrap().unwrap().note.as_deref(), Some(""));
    let too_long = "x".repeat(4001);
    let o = run(&["fax", "note", &a.to_string(), &too_long], &cfg);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(st.get(a).await.unwrap().unwrap().note.as_deref(), Some(""), "the refused note changed nothing");
    // redeliver: nothing to do, then a failed and an abandoned delivery are queued again
    let o = run(&["fax", "redeliver", &a.to_string()], &cfg);
    assert!(o.status.success());
    assert!(out(&o).contains("nothing to redeliver"), "{}", out(&o));
    for (target, status) in [("folder:/out", DeliveryStatus::Failed), ("webhook:https://h/x", DeliveryStatus::GivingUp), ("email:a@b", DeliveryStatus::Done)] {
        st.add_delivery(&NewDelivery {
            fax_id: a,
            target: target.into(),
            kind: target.split(':').next().unwrap().into(),
            attempt: 1,
            status,
            next_try_at: None,
        })
        .await
        .unwrap();
    }
    let o = run(&["fax", "redeliver", &a.to_string()], &cfg);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("2 delivery attempt(s) queued by cli:tester"), "{}", out(&o));
    let list = st.list_deliveries(a).await.unwrap();
    let pending: Vec<_> = list.iter().filter(|d| d.status == DeliveryStatus::Pending).map(|d| (d.target.as_str(), d.attempt)).collect();
    assert_eq!(pending.len(), 2);
    assert!(pending.contains(&("folder:/out", 2)) && pending.contains(&("webhook:https://h/x", 2)));
    assert!(!list.iter().any(|d| d.target == "email:a@b" && d.status == DeliveryStatus::Pending), "a delivered one is not repeated");
    let t = out(&run(&["fax", "show", &a.to_string()], &cfg));
    assert!(t.contains("deliveries:") && t.contains("folder:/out"), "{t}");
    // asking again does nothing (the new attempts are pending)
    assert!(out(&run(&["fax", "redeliver", &a.to_string()], &cfg)).contains("nothing to redeliver"));
    assert_eq!(run(&["fax", "redeliver", "9999"], &cfg).status.code(), Some(1));
}

// ---------------------------------------------------------------- K03 / K04 on the command line
#[tokio::test(flavor = "multi_thread")]
async fn k03_k04_config_import_export_round_trip_on_the_command_line() {
    let dir = scratch("k0304");
    let cfg = config(&dir, "");
    let o = run(&["config", "import"], &cfg);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("imported"), "{}", out(&o));
    let st = open(&dir).await;
    let versions: Vec<_> = st.config_list().await.unwrap().into_iter().map(|r| (r.name, r.version)).collect();
    assert!(versions.iter().any(|(n, _)| n == "trunk:local"), "{versions:?}");
    // a second import without --force is refused and changes nothing
    let o = run(&["config", "import", "--file", cfg.to_str().unwrap()], &cfg);
    assert_eq!(o.status.code(), Some(1));
    assert!(err(&o).contains("--force"), "{}", err(&o));
    let after: Vec<_> = st.config_list().await.unwrap().into_iter().map(|r| (r.name, r.version)).collect();
    assert_eq!(versions, after);
    // the secret is encrypted in the database, and exports without it by default
    let route = st.config_get("routes").await.unwrap().unwrap();
    assert!(route.body.contains("enc:v1:") && !route.body.contains("hook-secret-1"), "{}", route.body);
    let plain = run(&["config", "export"], &cfg);
    assert!(plain.status.success(), "{}", err(&plain));
    assert!(!out(&plain).contains("hook-secret-1"));
    assert!(out(&plain).contains("WITHOUT secrets"));
    let full = run(&["config", "export", "--include-secrets"], &cfg);
    assert!(out(&full).contains("secret = \"hook-secret-1\""), "{}", out(&full));
    assert!(out(&full).contains("max_duration_secs = 600"));
    // a changed file with --force replaces; the export shows it
    let cfg2 = config(&dir, "").with_file_name("changed.toml");
    let changed = std::fs::read_to_string(&cfg).unwrap().replace("max_duration_secs = 600", "max_duration_secs = 321");
    std::fs::write(&cfg2, changed).unwrap();
    let o = run(&["config", "import", "--force", "--file", cfg2.to_str().unwrap()], &cfg);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&run(&["config", "export", "--include-secrets"], &cfg)).contains("max_duration_secs = 321"));
    let audit = st.config_audit_list(200, None).await.unwrap();
    assert!(audit.iter().any(|r| r.by == "cli:tester" && r.action == "import" && r.section == "call"), "{audit:?}");
    // the exported text imports into an empty database and exports the same again
    let exported = out(&run(&["config", "export", "--include-secrets"], &cfg));
    let dir2 = scratch("k0304b");
    let cfg_b = config(&dir2, "");
    let exp_file = dir2.join("exported.toml");
    std::fs::write(&exp_file, &exported).unwrap();
    // the second database has its own key: secrets are re-encrypted with it
    let o = run(&["config", "import", "--force", "--file", exp_file.to_str().unwrap()], &cfg_b);
    assert!(o.status.success(), "{}", err(&o));
    let again = out(&run(&["config", "export", "--include-secrets"], &cfg_b));
    assert_eq!(again, exported, "export -> import -> export is the identity");
}

// ---------------------------------------------------------------- serve
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

/// Reads log lines until "sip-faxd ready" and returns all of them.
fn until_ready(rx: &mpsc::Receiver<String>) -> Vec<String> {
    let mut seen = Vec::new();
    loop {
        match rx.recv_timeout(Duration::from_secs(60)) {
            Ok(l) => {
                let done = l.contains("sip-faxd ready");
                seen.push(l);
                if done {
                    return seen;
                }
            }
            Err(_) => panic!("never ready; log so far:\n{}", seen.join("\n")),
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn k01_k02_a01_serve_imports_once_warns_about_ignored_sections_and_creates_the_admin() {
    let dir = scratch("serve");
    let cfg = config(&dir, "");
    let (mut child, rx) = start_serve(&cfg);
    let log = until_ready(&rx).join("\n");
    child.kill().unwrap();
    let _ = child.wait();
    let st = open(&dir).await;
    // K01: imported, audited as config-file
    for s in ["sip", "call", "routes", "trunk:local", "retention"] {
        assert!(st.config_get(s).await.unwrap().is_some(), "{s}");
    }
    assert!(log.contains("imported into the database"), "{log}");
    let audit = st.config_audit_list(100, None).await.unwrap();
    assert!(
        audit.iter().filter(|r| !r.section.starts_with("user:")).all(|r| r.by == "config-file"),
        "{audit:?}"
    );
    assert!(audit.iter().any(|r| r.section == "user:admin" && r.by == "system"));
    // A01: admin with a private password file, the password is not in the log
    let pwfile = dir.join("initial-admin-password");
    let pw = std::fs::read_to_string(&pwfile).unwrap();
    let pw = pw.trim();
    assert_eq!(pw.chars().count(), 16);
    assert!(log.contains("initial-admin-password"), "the log names the file: {log}");
    assert!(!log.contains(pw), "the log must not contain the password");
    let admin = st.user_get_by_name("admin").await.unwrap().unwrap();
    assert!(admin.must_change_password && admin.role == "admin");
    assert!(accounts::password::verify(&admin.password_hash, pw));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(std::fs::metadata(&pwfile).unwrap().permissions().mode() & 0o777, 0o600);
        let key = dir.join("secret.key");
        assert_eq!(std::fs::metadata(&key).unwrap().permissions().mode() & 0o777, 0o600);
    }
    assert_eq!(std::fs::read(dir.join("secret.key")).unwrap().len(), 32);
    // K02: the file changes, the database wins, the warning names the sections
    let changed = std::fs::read_to_string(&cfg).unwrap().replace("max_duration_secs = 600", "max_duration_secs = 11");
    std::fs::write(&cfg, changed).unwrap();
    let (mut child, rx) = start_serve(&cfg);
    let log2 = until_ready(&rx).join("\n");
    child.kill().unwrap();
    let _ = child.wait();
    assert!(log2.contains("are managed by the database now"), "{log2}");
    for s in ["sip", "call", "trunks", "routes"] {
        assert!(
            log2.lines().any(|l| l.contains("managed by the database") && l.contains(s)),
            "{s} named in the warning: {log2}"
        );
    }
    assert!(!log2.contains("imported into the database"), "no second import");
    assert!(!log2.contains("created the administrator"), "no second admin");
    let call = st.config_get("call").await.unwrap().unwrap();
    assert!(call.body.contains("600") && !call.body.contains("\"max_duration_secs\":11"), "{}", call.body);
    assert_eq!(call.version, 1);
    assert_eq!(st.user_count().await.unwrap(), 1);
    assert_eq!(std::fs::read_to_string(&pwfile).unwrap().trim(), pw, "the password file stays until the first change");
}

#[tokio::test(flavor = "multi_thread")]
async fn k11_serve_starts_without_secret_key_and_says_which_secrets_are_lost() {
    let dir = scratch("k11");
    let cfg = config(&dir, "");
    let (mut child, rx) = start_serve(&cfg);
    until_ready(&rx);
    child.kill().unwrap();
    let _ = child.wait();
    std::fs::remove_file(dir.join("secret.key")).unwrap();
    let (mut child, rx) = start_serve(&cfg);
    let log = until_ready(&rx).join("\n");
    child.kill().unwrap();
    let _ = child.wait();
    assert!(log.contains("did not exist: a new key was created"), "{log}");
    assert!(log.contains("routes") && log.contains("cannot be decrypted"), "{log}");
    assert!(log.contains("sip-faxd ready"), "the server runs");
    // config check says it, too, and fails
    let o = run(&["config", "check"], &cfg);
    assert_eq!(o.status.code(), Some(1), "{}", out(&o));
    assert!(err(&o).contains("routes: routes[0].deliveries[0].secret"), "{}", err(&o));
    // entering the secret again (an import of the file) repairs it
    let o = run(&["config", "import", "--force"], &cfg);
    assert!(o.status.success(), "{}", err(&o));
    assert!(run(&["config", "check"], &cfg).status.success());
}

#[tokio::test(flavor = "multi_thread")]
async fn config_check_reads_the_database_and_never_changes_it() {
    let dir = scratch("check");
    let cfg = config(&dir, "");
    // before the first start: the database has no schema; nothing is created by `check`
    let o = run(&["config", "check"], &cfg);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("database runtime configuration:"), "{}", out(&o));
    assert!(out(&o).contains("schema is older") || out(&o).contains("is not reachable"), "{}", out(&o));
    assert!(!dir.join("secret.key").exists(), "check creates no key");
    // after an import: it shows the stored sections
    assert!(run(&["config", "import"], &cfg).status.success());
    let t = out(&run(&["config", "check"], &cfg));
    assert!(t.contains("stored section(s)"), "{t}");
    assert!(t.contains("trunks (match order): local"), "{t}");
    assert!(t.contains("max_duration 600 s"), "{t}");
    assert!(!t.contains("hook-secret-1"), "no secret in the output");
    // a section that is not valid is an error with the section named
    let st = open(&dir).await;
    let cur = st.config_get("media").await.unwrap().unwrap();
    st.config_put(
        "media",
        r#"{"port_range":"banana"}"#,
        Some(cur.version),
        "test",
        &store::ConfigAudit { action: "update".into(), before: None, after: None },
    )
    .await
    .unwrap();
    let o = run(&["config", "check"], &cfg);
    assert_eq!(o.status.code(), Some(1), "{}", out(&o));
    assert!(err(&o).contains("section media is not valid"), "{}", err(&o));
}
