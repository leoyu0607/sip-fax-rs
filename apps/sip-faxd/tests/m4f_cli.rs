//! M4f on the real `sip-faxd` executable (SQLite and closed ports; the server databases are
//! `m4f_server.rs`, the real fax is `tools/fax-sim/tests/e2e_m4f.rs`):
//! D05 (the password never shows), K01/K02/K03/K04 (`db check`), P01/P02 (`db copy` targets),
//! F01..F04 (the configuration file holds the startup settings only; the no-trunk start).
#![cfg(unix)]

mod m4f_support;

use std::net::UdpSocket;
use std::path::Path;
use std::time::{Duration, Instant};

use m4f_support::*;

/// `serve` without a `[sip]` section binds the default 0.0.0.0:5060: one such test at a time.
static SIP_5060: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

/// A database with the schema, made as the service would (and closed again).
async fn make_db(url: &str) {
    let st = store::connect(url).await.unwrap();
    st.close().await;
}

/// The files of `dir` (names and bytes), to see that a command changed nothing.
fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    let mut v: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| {
            let e = e.unwrap();
            (
                e.file_name().to_string_lossy().into_owned(),
                std::fs::read(e.path()).unwrap_or_default(),
            )
        })
        .collect();
    v.sort();
    v
}

fn closed_port() -> u16 {
    free_port()
}

// ---------------------------------------------------------------- D05
#[test]
fn d05_the_password_of_the_fields_never_shows_in_any_output_or_log() {
    let dir = scratch("d05");
    let port = closed_port();
    let pw = NASTY;
    let db = format!(
        "type = \"postgres\"\nhost = \"127.0.0.1\"\nport = {port}\nname = \"faxdb\"\nuser = \"faxu\"\npassword = {pw:?}\n"
    );
    let sip = free_port();
    let log_file = dir.join("serve.log");
    let extra = format!(
        "[sip]\nbind = \"127.0.0.1:{sip}\"\n[log]\nlevel = \"debug\"\nfile = {:?}\n",
        slashes(&log_file)
    );
    // (the [log] table in `config()` comes first; a second one would be a duplicate)
    let text = std::fs::read_to_string(config(&dir, "a.toml", &db, None, "")).unwrap();
    let text = text.replace("[log]\nfile = \"\"\n", "");
    let cfg = dir.join("a.toml");
    std::fs::write(&cfg, format!("{text}{extra}")).unwrap();

    let mut seen: Vec<(String, String)> = Vec::new();

    let o = run(&["config", "check"], &cfg);
    assert!(o.status.success(), "{}", both(&o));
    assert!(
        out(&o).contains("faxu:***@127.0.0.1"),
        "config check shows the masked URL: {}",
        out(&o)
    );
    seen.push(("config check".into(), both(&o)));

    let o = run(&["db", "check"], &cfg);
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(out(&o).contains("faxu:***@127.0.0.1"), "{}", out(&o));
    assert!(err(&o).contains("cannot use the database"), "{}", err(&o));
    seen.push(("db check".into(), both(&o)));

    // db check --url with the password from stdin
    let o = run_raw_stdin(
        &[
            "db",
            "check",
            "--url",
            &format!("postgres://faxu@127.0.0.1:{port}/faxdb"),
            "--password-stdin",
        ],
        &format!("{pw}\n"),
    );
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(
        out(&o).contains("faxu:***@") || !out(&o).contains("faxu:"),
        "{}",
        out(&o)
    );
    seen.push(("db check --url --password-stdin".into(), both(&o)));

    // db copy: from SQLite into the unreachable database named by fields (--to-config) ...
    let src = scratch("d05-src");
    let src_cfg = config(&src, "s.toml", &sqlite_fields(&src, "s.db"), None, "");
    let o = run(&["config", "import"], &src_cfg);
    assert!(o.status.success(), "{}", both(&o));
    let o = run(&["db", "copy", "--to-config", &slashes(&cfg)], &src_cfg);
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(out(&o).contains("faxu:***@127.0.0.1"), "{}", out(&o));
    seen.push(("db copy --to-config".into(), both(&o)));
    // ... and with --to-password-stdin
    let o = {
        let mut child = bin()
            .args([
                "db",
                "copy",
                "--to",
                &format!("postgres://faxu@127.0.0.1:{port}/faxdb"),
                "--to-password-stdin",
                "--config",
            ])
            .arg(&src_cfg)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped())
            .spawn()
            .unwrap();
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(format!("{pw}\n").as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    };
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(out(&o).contains("faxu:***@127.0.0.1"), "{}", out(&o));
    seen.push(("db copy --to-password-stdin".into(), both(&o)));

    // a start whose database cannot be reached: the log and stderr
    let mut s = Serve::start(&cfg);
    s.ready();
    s.wait_for("database not reachable");
    let stderr = s.stop();
    assert!(
        stderr.contains("faxu:***@127.0.0.1"),
        "the log shows the masked URL:\n{stderr}"
    );
    seen.push(("serve stderr".into(), stderr));
    seen.push((
        "serve log file".into(),
        std::fs::read_to_string(&log_file).unwrap_or_default(),
    ));
    assert!(
        !seen.last().unwrap().1.is_empty(),
        "the log file {} was written",
        log_file.display()
    );

    // a password file: same rule
    let pwf = dir.join("db-password");
    std::fs::write(&pwf, format!("{pw}\n")).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&pwf, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let db2 = format!(
        "type = \"postgres\"\nhost = \"127.0.0.1\"\nport = {port}\nname = \"faxdb\"\nuser = \"faxu\"\npassword_file = {:?}\n",
        slashes(&pwf)
    );
    let cfg2 = config(&dir, "b.toml", &db2, None, "");
    let o = run(&["config", "check"], &cfg2);
    assert!(o.status.success(), "{}", both(&o));
    seen.push(("config check (password_file)".into(), both(&o)));
    let o = run(&["db", "check"], &cfg2);
    assert_eq!(o.status.code(), Some(1));
    assert!(out(&o).contains("faxu:***@"), "{}", out(&o));
    seen.push(("db check (password_file)".into(), both(&o)));

    // a configuration the start refuses names the field and not the password
    let bad = config(
        &dir,
        "c.toml",
        &format!(
            "type = \"oracle\"\nhost = \"h\"\nname = \"d\"\nuser = \"u\"\npassword = {pw:?}\n"
        ),
        None,
        "",
    );
    for args in [
        &["config", "check"][..],
        &["serve"][..],
        &["db", "check"][..],
    ] {
        let o = run(args, &bad);
        assert_eq!(o.status.code(), Some(2), "{args:?}: {}", both(&o));
        assert!(err(&o).contains("[database] type"), "{args:?}: {}", err(&o));
        seen.push((format!("{args:?} with a bad type"), both(&o)));
    }

    for (what, text) in &seen {
        assert_no_password(what, pw, text);
        assert!(
            !text.contains("Aa1@"),
            "{what} shows the start of the password:\n{text}"
        );
    }
}

// ---------------------------------------------------------------- K01
#[tokio::test(flavor = "multi_thread")]
async fn k01_db_check_on_sqlite_succeeds_and_reports_type_version_and_schema() {
    let dir = scratch("k01");
    let url = sqlite_url(&dir, "ok.db");
    make_db(&url).await;
    let cfg = config(&dir, "a.toml", &sqlite_fields(&dir, "ok.db"), None, "");
    let before = snapshot(&dir);
    let o = run(&["db", "check"], &cfg);
    assert_eq!(o.status.code(), Some(0), "{}", both(&o));
    let t = out(&o);
    assert!(t.contains("(sqlite)"), "{t}");
    assert!(t.contains("connected in "), "{t}");
    let ver = t
        .lines()
        .find_map(|l| l.trim().strip_prefix("server version: "))
        .unwrap_or_else(|| panic!("a server version line: {t}"));
    let parts: Vec<&str> = ver.split('.').collect();
    assert!(
        parts.len() >= 2 && parts.iter().all(|p| p.parse::<u32>().is_ok()),
        "a SQLite version number: {ver}"
    );
    let schema = t
        .lines()
        .find(|l| l.trim().starts_with("schema version: "))
        .unwrap_or_else(|| panic!("a schema line: {t}"));
    assert!(schema.contains("up to date"), "{schema}");
    let n: Vec<u32> = schema
        .split(|c: char| !c.is_ascii_digit())
        .filter_map(|p| p.parse().ok())
        .collect();
    assert_eq!(n.len(), 2, "{schema}");
    assert_eq!(
        n[0], n[1],
        "the database is at the version the binary needs: {schema}"
    );
    assert!(t.trim_end().ends_with("ok"), "{t}");
    assert!(err(&o).is_empty(), "{}", err(&o));
    // the same through --url, without any configuration file
    let o = run_raw_stdin(&["db", "check", "--url", &url], "");
    assert_eq!(o.status.code(), Some(0), "{}", both(&o));
    assert!(out(&o).contains("up to date"), "{}", out(&o));
    assert_eq!(snapshot(&dir).len(), before.len(), "no file appeared");
}

// ---------------------------------------------------------------- K02
#[tokio::test(flavor = "multi_thread")]
async fn k02_db_check_reports_empty_older_and_newer_schemas_and_changes_nothing() {
    let dir = scratch("k02");
    let cfg_for = |file: &str| {
        config(
            &dir,
            &format!("{file}.toml"),
            &sqlite_fields(&dir, file),
            None,
            "",
        )
    };

    // a new installation: no file at all, and none is created
    let cfg = cfg_for("new.db");
    let o = run(&["db", "check"], &cfg);
    assert_eq!(o.status.code(), Some(0), "{}", both(&o));
    assert!(out(&o).contains("does not exist"), "{}", out(&o));
    assert!(
        !dir.join("new.db").exists(),
        "db check created the SQLite file"
    );

    // an empty file (a database without tables): says migrate, writes nothing
    let empty = dir.join("empty.db");
    std::fs::write(&empty, b"").unwrap();
    let cfg = cfg_for("empty.db");
    let o = run(&["db", "check"], &cfg);
    assert_eq!(o.status.code(), Some(0), "{}", both(&o));
    assert!(out(&o).contains("no tables yet"), "{}", out(&o));
    assert!(out(&o).contains("db migrate"), "{}", out(&o));
    assert_eq!(
        store::test_schema_versions(&sqlite_url(&dir, "empty.db"))
            .await
            .unwrap(),
        None,
        "no schema_migrations table was created (and no other table)"
    );

    // older: the last version missing from schema_migrations
    let old_url = sqlite_url(&dir, "old.db");
    make_db(&old_url).await;
    let all = store::test_schema_versions(&old_url)
        .await
        .unwrap()
        .unwrap();
    let latest = *all.last().unwrap();
    assert!(latest >= 2, "{all:?}");
    store::test_forget_schema_version(&old_url, latest)
        .await
        .unwrap();
    let want = store::test_schema_versions(&old_url)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(want.len(), all.len() - 1);
    let snap = snapshot(&dir);
    let cfg = cfg_for("old.db");
    let snap_all = snapshot(&dir); // (the configuration file itself was just written)
    let o = run(&["db", "check"], &cfg);
    assert_eq!(o.status.code(), Some(0), "{}", both(&o));
    assert!(out(&o).contains("older"), "{}", out(&o));
    assert!(out(&o).contains("db migrate"), "{}", out(&o));
    assert!(
        out(&o).contains(&format!(
            "schema version: {} (this sip-faxd needs {latest})",
            want.last().unwrap()
        )),
        "{}",
        out(&o)
    );
    assert_eq!(
        snapshot(&dir),
        snap_all,
        "db check changed a file ({} files before)",
        snap.len()
    );
    assert_eq!(
        store::test_schema_versions(&old_url)
            .await
            .unwrap()
            .unwrap(),
        want
    );

    // newer: a version this binary does not know; non-zero, tells to upgrade
    let new_url = sqlite_url(&dir, "newer.db");
    make_db(&new_url).await;
    store::test_record_schema_version(&new_url, latest + 5)
        .await
        .unwrap();
    let cfg = cfg_for("newer.db");
    let snap_new = snapshot(&dir);
    let o = run(&["db", "check"], &cfg);
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(out(&o).contains("NEWER"), "{}", out(&o));
    assert!(out(&o).contains("newer sip-faxd"), "{}", out(&o));
    assert!(!out(&o).trim_end().ends_with("ok"), "{}", out(&o));
    assert_eq!(snapshot(&dir), snap_new, "db check changed a file");
    assert_eq!(
        store::test_schema_versions(&new_url)
            .await
            .unwrap()
            .unwrap()
            .last(),
        Some(&(latest + 5))
    );
}

// ---------------------------------------------------------------- K03
fn k03_closed_port(kind: &str) {
    let dir = scratch(&format!("k03-port-{kind}"));
    let port = closed_port();
    let db = format!(
        "type = \"{kind}\"\nhost = \"127.0.0.1\"\nport = {port}\nname = \"faxdb\"\nuser = \"faxu\"\npassword = \"Sup3rS3cret\"\n"
    );
    let cfg = config(&dir, "a.toml", &db, None, "");
    let t = Instant::now();
    let o = run(&["db", "check"], &cfg);
    let took = t.elapsed();
    assert_eq!(o.status.code(), Some(1), "{kind}: {}", both(&o));
    assert!(took < Duration::from_secs(20), "{kind}: took {took:?}");
    assert!(
        err(&o).contains("cannot use the database"),
        "{kind}: {}",
        err(&o)
    );
    assert!(
        err(&o).contains("hint: ") && err(&o).contains("nothing listens on that host and port"),
        "{kind}: {}",
        err(&o)
    );
    assert!(!out(&o).trim_end().ends_with("ok"), "{kind}: {}", out(&o));
    assert_no_password(kind, "Sup3rS3cret", &both(&o));
}

fn k03_unknown_host(kind: &str) {
    let dir = scratch(&format!("k03-dns-{kind}"));
    let db = format!(
        "type = \"{kind}\"\nhost = \"no-such-host.invalid\"\nname = \"faxdb\"\nuser = \"faxu\"\npassword = \"Sup3rS3cret\"\n"
    );
    let cfg = config(&dir, "a.toml", &db, None, "");
    let t = Instant::now();
    let o = run(&["db", "check"], &cfg);
    let took = t.elapsed();
    assert_eq!(o.status.code(), Some(1), "{kind}: {}", both(&o));
    assert!(took < Duration::from_secs(20), "{kind}: took {took:?}");
    assert!(
        err(&o).contains("hint: the host name cannot be resolved"),
        "{kind}: {}",
        err(&o)
    );
    assert_no_password(kind, "Sup3rS3cret", &both(&o));
}

#[test]
fn k03_postgres_closed_port_fails_with_the_hint_within_20_seconds() {
    k03_closed_port("postgres");
}
#[test]
fn k03_mysql_closed_port_fails_with_the_hint_within_20_seconds() {
    k03_closed_port("mysql");
}
#[test]
fn k03_mariadb_closed_port_fails_with_the_hint_within_20_seconds() {
    k03_closed_port("mariadb");
}
#[test]
fn k03_mssql_closed_port_fails_with_the_hint_within_20_seconds() {
    k03_closed_port("mssql");
}
#[test]
fn k03_postgres_unknown_host_fails_with_the_hint_within_20_seconds() {
    k03_unknown_host("postgres");
}
#[test]
fn k03_mysql_unknown_host_fails_with_the_hint_within_20_seconds() {
    k03_unknown_host("mysql");
}
#[test]
fn k03_mssql_unknown_host_fails_with_the_hint_within_20_seconds() {
    k03_unknown_host("mssql");
}

// ---------------------------------------------------------------- K04
#[tokio::test(flavor = "multi_thread")]
async fn k04_password_stdin_is_refused_when_the_url_has_a_password_or_no_user_or_stdin_is_empty() {
    let dir = scratch("k04");
    let port = closed_port();
    // the password is already in the URL
    let o = run_raw_stdin(
        &[
            "db",
            "check",
            "--url",
            &format!("postgres://u:inurl@127.0.0.1:{port}/d"),
            "--password-stdin",
        ],
        "pw\n",
    );
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(err(&o).contains("already has a password"), "{}", err(&o));
    assert!(out(&o).is_empty(), "nothing was tried: {}", out(&o));
    assert_no_password("url with a password", "inurl", &both(&o));
    // no user to put it with
    let o = run_raw_stdin(
        &[
            "db",
            "check",
            "--url",
            &format!("postgres://127.0.0.1:{port}/d"),
            "--password-stdin",
        ],
        "pw\n",
    );
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(err(&o).contains("needs a user"), "{}", err(&o));
    // an empty line
    let o = run_raw_stdin(
        &[
            "db",
            "check",
            "--url",
            &format!("postgres://u@127.0.0.1:{port}/d"),
            "--password-stdin",
        ],
        "\n",
    );
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(err(&o).contains("empty"), "{}", err(&o));
    // --password-stdin needs --url (clap)
    let o = run_raw_stdin(&["db", "check", "--password-stdin"], "pw\n");
    assert!(!o.status.success());
    assert!(err(&o).contains("--url"), "{}", err(&o));
    // a good one gets as far as the connection (the closed port), with the password put in
    let o = run_raw_stdin(
        &[
            "db",
            "check",
            "--url",
            &format!("postgres://u@127.0.0.1:{port}/d"),
            "--password-stdin",
        ],
        "p@ss:w/rd\n",
    );
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(err(&o).contains("nothing listens"), "{}", err(&o));
    assert!(
        out(&o).contains("postgres://u:***@127.0.0.1"),
        "{}",
        out(&o)
    );
    // sqlite has no password: the same flag is refused with the reason
    let url = sqlite_url(&dir, "x.db");
    make_db(&url).await;
    let o = run_raw_stdin(&["db", "check", "--url", &url, "--password-stdin"], "pw\n");
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(err(&o).contains("needs a user"), "{}", err(&o));
}

// ---------------------------------------------------------------- P01 / P02
/// A SQLite source with the runtime configuration and two accounts.
async fn source(name: &str) -> (std::path::PathBuf, std::path::PathBuf, String) {
    let dir = scratch(name);
    let rt = "[sip]\nbind = \"127.0.0.1:5999\"\n[[trunks]]\nname = \"local\"\nallowed_sources = [\"127.0.0.0/8\"]\n";
    let cfg = config(&dir, "a.toml", &sqlite_fields(&dir, "a.db"), None, rt);
    let o = run(&["config", "import"], &cfg);
    assert!(o.status.success(), "{}", both(&o));
    for (n, role) in [("alice", "admin"), ("bob", "agent")] {
        let o = run_stdin(
            &["user", "add", n, "--role", role, "--password-stdin"],
            &cfg,
            "a password 12345\n",
        );
        assert!(o.status.success(), "{}", both(&o));
    }
    let url = sqlite_url(&dir, "a.db");
    (dir, cfg, url)
}

async fn counts(url: &str) -> Vec<(String, u64)> {
    store::test_row_counts(url)
        .await
        .unwrap()
        .into_iter()
        .map(|(t, n)| (t.to_string(), n))
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn p01_to_config_with_fields_copies_the_same_as_to_with_the_full_url() {
    let (dir, cfg, url) = source("p01").await;
    let want = counts(&url).await;
    assert!(want.iter().map(|(_, n)| n).sum::<u64>() > 5, "{want:?}");

    // (a) --to <full URL>
    let o = run(&["db", "copy", "--to", &sqlite_url(&dir, "a1.db")], &cfg);
    assert!(o.status.success(), "{}", both(&o));
    // (b) --to-config: another configuration file whose [database] is written field by field
    let tcfg = config(&dir, "t.toml", &sqlite_fields(&dir, "b1.db"), None, "");
    let o = run(&["db", "copy", "--to-config", &slashes(&tcfg)], &cfg);
    assert!(o.status.success(), "{}", both(&o));
    assert!(
        out(&o).contains(&format!("-> sqlite://{}", slashes(&dir.join("b1.db")))),
        "{}",
        out(&o)
    );
    assert!(out(&o).contains("verified"), "{}", out(&o));

    let (a, b) = (sqlite_url(&dir, "a1.db"), sqlite_url(&dir, "b1.db"));
    assert_eq!(counts(&a).await, want);
    assert_eq!(counts(&b).await, want);
    let (sa, sb) = (
        store::connect(&a).await.unwrap(),
        store::connect(&b).await.unwrap(),
    );
    let (ra, rb) = (
        sa.config_list().await.unwrap(),
        sb.config_list().await.unwrap(),
    );
    assert!(!ra.is_empty());
    assert_eq!(
        format!("{ra:?}"),
        format!("{rb:?}"),
        "the two copies hold the same rows"
    );
    let (ua, ub) = (
        sa.user_count().await.unwrap(),
        sb.user_count().await.unwrap(),
    );
    assert_eq!((ua, ub), (2, 2));
    sa.close().await;
    sb.close().await;
    // the source is untouched
    assert_eq!(counts(&url).await, want);
}

#[tokio::test(flavor = "multi_thread")]
async fn p02_wrong_combinations_of_the_copy_options_are_refused_and_nothing_is_written() {
    let (dir, cfg, url) = source("p02").await;
    let want = counts(&url).await;
    let tfile = dir.join("t.db");
    let target = sqlite_url(&dir, "t.db");
    let tcfg = config(&dir, "t.toml", &sqlite_fields(&dir, "t.db"), None, "");
    let tcfg_s = slashes(&tcfg);

    let refused: Vec<(&str, Vec<&str>, &str)> = vec![
        (
            "--to and --to-config",
            vec!["db", "copy", "--to", &target, "--to-config", &tcfg_s],
            "cannot be used with",
        ),
        ("neither", vec!["db", "copy"], "required"),
        (
            "--to-password-stdin alone",
            vec!["db", "copy", "--to-password-stdin"],
            "--to",
        ),
    ];
    for (what, args, why) in refused {
        let o = run_raw_stdin(&[&args[..], &["--config", &slashes(&cfg)]].concat(), "pw\n");
        assert_eq!(o.status.code(), Some(2), "{what}: {}", both(&o));
        assert!(err(&o).contains(why), "{what}: {}", err(&o));
        assert!(!tfile.exists(), "{what}: the target was created");
        assert_eq!(counts(&url).await, want, "{what}: the source changed");
    }

    // refused by the program: the URL has a password / no user (sqlite), stdin is empty
    let o = run_raw_stdin(
        &[
            "db",
            "copy",
            "--to",
            "postgres://u:p@127.0.0.1:1/x",
            "--to-password-stdin",
            "--config",
            &slashes(&cfg),
        ],
        "pw\n",
    );
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(
        err(&o).contains("--to:") && err(&o).contains("already has a password"),
        "{}",
        err(&o)
    );
    assert!(out(&o).is_empty(), "{}", out(&o));
    let o = run_raw_stdin(
        &[
            "db",
            "copy",
            "--to",
            &target,
            "--to-password-stdin",
            "--config",
            &slashes(&cfg),
        ],
        "pw\n",
    );
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(err(&o).contains("needs a user"), "{}", err(&o));
    assert!(!tfile.exists());
    let o = run_raw_stdin(
        &[
            "db",
            "copy",
            "--to",
            "postgres://u@127.0.0.1:1/x",
            "--to-password-stdin",
            "--config",
            &slashes(&cfg),
        ],
        "",
    );
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(
        err(&o).contains("empty") || err(&o).contains("stdin"),
        "{}",
        err(&o)
    );

    // --to-password-stdin has no meaning with --to-config: refused, nothing copied
    let o = run_raw_stdin(
        &[
            "db",
            "copy",
            "--to-password-stdin",
            "--to-config",
            &tcfg_s,
            "--config",
            &slashes(&cfg),
        ],
        "pw\n",
    );
    assert!(!o.status.success(), "{}", both(&o));
    assert!(err(&o).contains("--to-password-stdin"), "{}", err(&o));
    assert!(out(&o).is_empty(), "{}", out(&o));
    assert!(!tfile.exists());

    // --to-config: no such file, not TOML, a bad [database], and the source itself
    let o = run(
        &[
            "db",
            "copy",
            "--to-config",
            &slashes(&dir.join("missing.toml")),
        ],
        &cfg,
    );
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(err(&o).contains("--to-config"), "{}", err(&o));
    let junk = dir.join("junk.toml");
    std::fs::write(&junk, "this is = = not toml").unwrap();
    let o = run(&["db", "copy", "--to-config", &slashes(&junk)], &cfg);
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(err(&o).contains("--to-config"), "{}", err(&o));
    let bad = config(
        &dir,
        "bad.toml",
        "type = \"sqlite\"\nhost = \"h\"\n",
        None,
        "",
    );
    let o = run(&["db", "copy", "--to-config", &slashes(&bad)], &cfg);
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(err(&o).contains("[database] host"), "{}", err(&o));
    let same = config(&dir, "same.toml", &sqlite_fields(&dir, "a.db"), None, "");
    let o = run(&["db", "copy", "--to-config", &slashes(&same)], &cfg);
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(
        err(&o).contains("the target is the source database"),
        "{}",
        err(&o)
    );
    assert!(!tfile.exists());
    assert_eq!(
        counts(&url).await,
        want,
        "the source is unchanged by every refusal"
    );
}

// ---------------------------------------------------------------- F01
/// The top-level tables of the example file (comments do not count).
fn example_tables() -> Vec<String> {
    let text = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../conf/sip-faxd.toml.example"
    ))
    .unwrap();
    let t: toml::Table = text.parse().unwrap();
    let mut k: Vec<String> = t.keys().cloned().collect();
    k.sort();
    k
}

#[tokio::test(flavor = "multi_thread")]
async fn f01_the_example_file_has_only_startup_settings_checks_and_starts_the_first_time() {
    assert_eq!(
        example_tables(),
        ["database", "log", "service", "storage", "web"],
        "conf/sip-faxd.toml.example holds the startup settings only"
    );
    let _g = SIP_5060.lock().await;
    let dir = scratch("f01");
    // the example, moved into a scratch directory: the paths and the web port are the only
    // changes (everything else is read as shipped)
    let ex = std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../conf/sip-faxd.toml.example"
    ))
    .unwrap();
    let web_port = free_port();
    let mut text = String::new();
    let mut seen = [false; 4];
    for line in ex.lines() {
        let l = line.trim_start();
        if l.starts_with("path = ") {
            text.push_str(&format!("path = {:?}\n", slashes(&dir.join("sip-faxd.db"))));
            seen[0] = true;
        } else if l.starts_with("data_dir = ") {
            text.push_str(&format!("data_dir = {:?}\n", slashes(&dir.join("data"))));
            seen[1] = true;
        } else if l.starts_with("enabled = ") {
            text.push_str(&format!(
                "enabled = true\nscheme = \"http\"\nbind = \"127.0.0.1:{web_port}\"\n"
            ));
            seen[2] = true;
        } else {
            text.push_str(line);
            text.push('\n');
        }
    }
    seen[3] = true;
    assert!(
        seen.iter().all(|s| *s),
        "the example has the lines the test replaces: {seen:?}"
    );
    // the service files go next to the file, not into the executable directory, and the log
    // is stderr only (not <executable directory>/logs)
    let text = text
        .replacen(
            "\n[service]\n",
            &format!(
                "\n[service]\npid_file = {:?}\nsocket = {:?}\n",
                slashes(&dir.join("f.pid")),
                slashes(&dir.join("f.sock"))
            ),
            1,
        )
        .replacen(
            "\nlevel = \"info\"\n",
            "\nlevel = \"info\"\nfile = \"\"\n",
            1,
        );
    let cfg = dir.join("sip-faxd.toml");
    let parsed: toml::Table = text.parse().unwrap_or_else(|e| panic!("{e}\n{text}"));
    assert!(parsed.contains_key("service"));
    std::fs::write(&cfg, &text).unwrap();

    let o = run(&["config", "check"], &cfg);
    assert_eq!(o.status.code(), Some(0), "{}", both(&o));
    assert!(out(&o).contains("configuration OK"), "{}", out(&o));
    assert!(
        out(&o).contains("none in this file, they live in the database"),
        "{}",
        out(&o)
    );
    assert!(
        !out(&o).contains("runtime sections in this file"),
        "{}",
        out(&o)
    );

    // the first start with it
    let mut s = Serve::start(&cfg);
    let log = s.ready();
    assert!(log.contains("no trunk is configured"), "{log}");
    let r = reqwest::get(format!("http://127.0.0.1:{web_port}/"))
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "the web interface answers");
    let log = s.stop();
    assert!(!log.contains(" ERROR "), "{log}");
    assert!(
        dir.join("sip-faxd.db").is_file(),
        "the database from [database] path was created"
    );
}

// ---------------------------------------------------------------- F02
/// One INVITE from 127.0.0.1 with a T.38 offer; the first line of the first answer, if any.
fn invite(to: &str, call_id: &str, wait: Duration) -> Option<String> {
    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    let lp = sock.local_addr().unwrap().port();
    let sdp = "v=0\r\no=gw 1 1 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=image 40000 udptl t38\r\na=T38FaxVersion:0\r\na=T38MaxBitRate:14400\r\na=T38FaxRateManagement:transferredTCF\r\na=T38FaxUdpEC:t38UDPRedundancy\r\n";
    let msg = format!(
        "INVITE sip:0255@{to} SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:{lp};branch=z9hG4bKm4f{lp}\r\nMax-Forwards: 70\r\nFrom: <sip:0911000111@127.0.0.1>;tag=m4f\r\nTo: <sip:0255@{to}>\r\nCall-ID: {call_id}\r\nCSeq: 1 INVITE\r\nContact: <sip:0911000111@127.0.0.1:{lp}>\r\nContent-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{sdp}",
        sdp.len()
    );
    sock.send_to(msg.as_bytes(), to).unwrap();
    sock.set_read_timeout(Some(wait)).unwrap();
    let mut buf = [0u8; 4096];
    let n = sock.recv(&mut buf).ok()?;
    let text = String::from_utf8_lossy(&buf[..n]).into_owned();
    // end the call if it was taken, so that the next one starts clean
    let line = text.lines().next().map(str::to_string);
    if text.starts_with("SIP/2.0 1") {
        let cancel = format!(
            "CANCEL sip:0255@{to} SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:{lp};branch=z9hG4bKm4f{lp}\r\nMax-Forwards: 70\r\nFrom: <sip:0911000111@127.0.0.1>;tag=m4f\r\nTo: <sip:0255@{to}>\r\nCall-ID: {call_id}\r\nCSeq: 1 CANCEL\r\nContent-Length: 0\r\n\r\n"
        );
        let _ = sock.send_to(cancel.as_bytes(), to);
        std::thread::sleep(Duration::from_millis(300));
    }
    if text.contains("SIP/2.0 200") {
        let bye = format!(
            "BYE sip:0255@{to} SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:{lp};branch=z9hG4bKm4fb{lp}\r\nMax-Forwards: 70\r\nFrom: <sip:0911000111@127.0.0.1>;tag=m4f\r\nTo: <sip:0255@{to}>\r\nCall-ID: {call_id}\r\nCSeq: 2 BYE\r\nContent-Length: 0\r\n\r\n"
        );
        let _ = sock.send_to(bye.as_bytes(), to);
    }
    line
}

/// A provisional or final 2xx answer: the call was accepted for processing.
fn taken(status_line: &str) -> bool {
    status_line
        .split_whitespace()
        .nth(1)
        .and_then(|c| c.parse::<u16>().ok())
        .is_some_and(|c| (100..300).contains(&c))
}

async fn login(c: &reqwest::Client, base: &str, user: &str, pw: &str) -> (String, String) {
    let r = c
        .post(format!("{base}/api/v1/auth/login"))
        .json(&serde_json::json!({"username": user, "password": pw}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "login {user}");
    let cookie = r
        .headers()
        .get_all("set-cookie")
        .iter()
        .filter_map(|v| v.to_str().ok())
        .find(|v| v.starts_with("sipfax_session="))
        .map(|v| v.split(';').next().unwrap().to_string())
        .expect("a session cookie");
    let me: serde_json::Value = c
        .get(format!("{base}/api/v1/auth/me"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    (
        cookie,
        me["csrf"].as_str().expect("a csrf token").to_string(),
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn f02_a_new_installation_starts_without_a_trunk_refuses_calls_and_takes_a_trunk_without_a_restart()
 {
    let _g = SIP_5060.lock().await;
    let dir = scratch("f02");
    let web_port = free_port();
    // no runtime section in the file at all
    let cfg = config(
        &dir,
        "a.toml",
        &sqlite_fields(&dir, "a.db"),
        Some(web_port),
        "",
    );
    assert!(
        !std::fs::read_to_string(&cfg)
            .unwrap()
            .contains("[[trunks]]"),
        "no trunk in the file"
    );
    // an account to use the web API with (this makes the schema; no runtime configuration yet)
    let o = run_stdin(
        &["user", "add", "root", "--role", "admin", "--password-stdin"],
        &cfg,
        "root password 12\n",
    );
    assert!(o.status.success(), "{}", both(&o));
    let url = sqlite_url(&dir, "a.db");
    {
        let st = store::connect(&url).await.unwrap();
        assert!(
            st.config_list().await.unwrap().is_empty(),
            "the database has no runtime configuration yet"
        );
        st.close().await;
    }

    // config check: the file has none, the database is empty: says what the first start does
    let o = run(&["config", "check"], &cfg);
    assert_eq!(o.status.code(), Some(0), "{}", both(&o));
    assert!(
        out(&o).contains("the defaults, without any trunk"),
        "{}",
        out(&o)
    );

    let mut s = Serve::start(&cfg);
    let log = s.ready();
    assert!(
        log.contains("no trunk is configured: every call is refused"),
        "the no-trunk warning:\n{log}"
    );
    assert!(log.contains("Settings -> Trunks"), "{log}");
    // the database now has the default runtime configuration and no trunk
    {
        let st = store::connect_with(
            &url,
            store::ConnectOptions {
                auto_migrate: false,
                ..store::ConnectOptions::default()
            },
        )
        .await
        .unwrap();
        let rows = st.config_list().await.unwrap();
        let names: Vec<String> = rows.iter().map(|r| r.name.clone()).collect();
        assert!(names.iter().any(|n| n == "sip"), "{names:?}");
        assert!(
            !names.iter().any(|n| n.starts_with("trunk")),
            "no trunk section: {names:?}"
        );
        st.close().await;
    }

    // a call from a gateway is refused: unknown_source = "drop" gives no answer at all
    let dropped = invite("127.0.0.1:5060", "m4f-f02-before@x", Duration::from_secs(3));
    assert_eq!(
        dropped, None,
        "a call without a trunk gets no answer (dropped)"
    );

    // add a trunk through the web API
    let c = reqwest::Client::builder()
        .timeout(Duration::from_secs(20))
        .build()
        .unwrap();
    let base = format!("http://127.0.0.1:{web_port}");
    let (cookie, csrf) = login(&c, &base, "root", "root password 12").await;
    let status: serde_json::Value = c
        .get(format!("{base}/api/v1/status"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        status["service"]["trunks"].as_array().unwrap().len(),
        0,
        "{status}"
    );
    let r = c
        .put(format!("{base}/api/v1/config/trunk%3Agw"))
        .header("Cookie", &cookie)
        .header("X-Sipfax-Csrf", &csrf)
        .json(&serde_json::json!({
            "version": 0,
            "body": {"name": "gw", "preset": "generic", "peer": "127.0.0.1:5090", "allowed_sources": ["127.0.0.0/8"]}
        }))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "{}", r.text().await.unwrap());
    let status: serde_json::Value = c
        .get(format!("{base}/api/v1/status"))
        .header("Cookie", &cookie)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        status["service"]["trunks"].as_array().unwrap().len(),
        1,
        "{status}"
    );

    // the very same process now answers the very same call
    let first = invite("127.0.0.1:5060", "m4f-f02-after@x", Duration::from_secs(5));
    let line = first.expect("an answer once there is a trunk (no restart)");
    assert!(line.starts_with("SIP/2.0 "), "{line}");
    assert!(taken(&line), "the call is taken: {line}");
    let log = s.stop();
    // (the capture of an accepted call went into <executable directory>/logs/calls: tidy up)
    let exe_dir = Path::new(env!("CARGO_BIN_EXE_sip-faxd"))
        .parent()
        .unwrap()
        .to_path_buf();
    let _ = std::fs::remove_dir_all(exe_dir.join("logs/calls/m4f-f02-after@x"));
    assert!(
        !log.contains("ready") || log.matches("sip-faxd ready").count() == 1,
        "one start only:\n{log}"
    );
}

// ---------------------------------------------------------------- F03 / F04
const OLD_RUNTIME: &str = "[capture]\nenabled = false\n[sip]\nbind = \"127.0.0.1:BIND\"\nunknown_source = \"drop\"\n\
    [call]\nstation_id = \"0287654321\"\n\
    [[trunks]]\nname = \"gw\"\nallowed_sources = [\"127.0.0.0/8\"]\n\
    [[routes]]\nname = \"all\"\ndefault = true\n[[routes.deliveries]]\nkind = \"folder\"\ndir = \"/tmp/m4f-out\"\n";

#[tokio::test(flavor = "multi_thread")]
async fn f03_an_old_file_with_runtime_sections_is_imported_once_into_an_empty_database() {
    let dir = scratch("f03");
    let sip = free_port();
    let rt = OLD_RUNTIME.replace("BIND", &sip.to_string());
    let cfg = config(&dir, "a.toml", &sqlite_fields(&dir, "a.db"), None, &rt);
    let o = run(&["config", "check"], &cfg);
    assert_eq!(o.status.code(), Some(0), "{}", both(&o));
    assert!(
        out(&o).contains("runtime sections in this file"),
        "{}",
        out(&o)
    );
    assert!(
        out(&o).contains("runtime sections in this file: sip, call, capture, trunks, routes"),
        "{}",
        out(&o)
    );
    assert!(out(&o).contains("trunk gw:"), "{}", out(&o));

    let mut s = Serve::start(&cfg);
    let log = s.ready();
    assert!(log.contains("was imported into the database"), "{log}");
    assert!(!log.contains("no trunk is configured"), "{log}");
    assert!(!log.contains("can be removed from the file"), "{log}");
    // a call from the trunk's network is taken: the trunk of the file is in force
    let first = invite(
        &format!("127.0.0.1:{sip}"),
        "m4f-f03@x",
        Duration::from_secs(5),
    );
    let line = first.expect("the imported trunk takes the call");
    assert!(taken(&line), "{line}");
    s.stop();

    let url = sqlite_url(&dir, "a.db");
    let st = store::connect(&url).await.unwrap();
    let rows = st.config_list().await.unwrap();
    let names: Vec<String> = rows.iter().map(|r| r.name.clone()).collect();
    for want in ["sip", "call", "trunk:gw", "routes"] {
        assert!(names.iter().any(|n| n == want), "{want} in {names:?}");
    }
    let audit = st.config_audit_list(100, None).await.unwrap();
    let imported: Vec<_> = audit
        .iter()
        .filter(|a| !a.section.starts_with("user:"))
        .collect();
    assert!(imported.len() >= 4, "{audit:?}");
    assert!(imported.iter().all(|a| a.by == "config-file"), "{audit:?}");
    let sip_row = rows.iter().find(|r| r.name == "sip").unwrap();
    assert!(
        sip_row.body.contains(&format!("127.0.0.1:{sip}")),
        "{}",
        sip_row.body
    );
    st.close().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn f04_a_file_with_runtime_sections_is_not_imported_again_and_the_log_lists_what_can_go() {
    let dir = scratch("f04");
    let sip = free_port();
    let rt = OLD_RUNTIME.replace("BIND", &sip.to_string());
    let cfg = config(&dir, "a.toml", &sqlite_fields(&dir, "a.db"), None, &rt);
    // the first start imports
    let mut s = Serve::start(&cfg);
    s.ready();
    s.stop();
    let url = sqlite_url(&dir, "a.db");
    let rows_of = |url: String| async move {
        let st = store::connect(&url).await.unwrap();
        let r: Vec<(String, String, i32)> = st
            .config_list()
            .await
            .unwrap()
            .into_iter()
            .map(|r| (r.name, r.body, r.version))
            .collect();
        let a = st.config_audit_list(1000, None).await.unwrap().len();
        st.close().await;
        (r, a)
    };
    let before = rows_of(url.clone()).await;

    // the file now says something else; the database already has its own
    let changed = rt
        .replace("0287654321", "0299999999")
        .replace("127.0.0.0/8", "10.0.0.0/8");
    let cfg = config(&dir, "a.toml", &sqlite_fields(&dir, "a.db"), None, &changed);
    let o = run(&["config", "check"], &cfg);
    assert_eq!(o.status.code(), Some(0), "{}", both(&o));
    assert!(
        out(&o).contains("warning: the runtime sections of the configuration file are not used"),
        "{}",
        out(&o)
    );
    for name in ["sip", "call", "trunks", "routes"] {
        assert!(
            out(&o).contains(name),
            "config check lists {name}: {}",
            out(&o)
        );
    }
    assert!(out(&o).contains("remove them from the file"), "{}", out(&o));
    assert!(
        out(&o).contains("0287654321"),
        "config check shows the database's value: {}",
        out(&o)
    );
    assert!(!out(&o).contains("0299999999,"), "{}", out(&o));

    let mut s = Serve::start(&cfg);
    let log = s.ready();
    assert!(
        log.contains("not used (the settings live in the database")
            && log.contains("can be removed from the file"),
        "{log}"
    );
    let line = log
        .lines()
        .find(|l| l.contains("can be removed from the file"))
        .unwrap();
    for name in ["sip", "call", "trunks", "routes"] {
        assert!(line.contains(name), "{name} in: {line}");
    }
    assert!(!log.contains("was imported into the database"), "{log}");
    s.stop();
    assert_eq!(
        rows_of(url).await,
        before,
        "the database is exactly as it was"
    );
}
