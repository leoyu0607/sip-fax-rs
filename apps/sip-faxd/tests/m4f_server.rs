//! M4f on the server databases (PostgreSQL, MySQL, MariaDB, SQL Server): D02 and K01 (a user
//! whose password has every awkward character, written field by field, is accepted by
//! `sip-faxd db check`), K02 (empty / older / newer schema, nothing changed), K03 (wrong password,
//! missing database, a certificate that is not trusted), K04 (`--password-stdin`), P01
//! (`db copy --to-config` / `--to-password-stdin` into the server).
//!
//! Runs only with `SIP_FAXD_TEST_DB=<admin URL of a server database>` and
//! `SIP_FAXD_TEST_DB_CONTAINER=<its Docker container>`: the account and the database are made
//! with the client of the container (`docker exec`), so that the test needs no driver of its own.
//! `scripts/db-matrix.sh` sets both for every backend.
#![cfg(unix)]

mod m4f_support;

use std::process::Command;
use std::time::{Duration, Instant};

use m4f_support::*;

/// Every character that breaks a URL, plus non-ASCII (the 4-byte emoji included).
const PW_EMOJI: &str = "Aa1@:/?#%41&+ 中😀x";
/// PostgreSQL's SCRAM (SASLprep, RFC 4013) refuses an emoji: its account gets the same without it.
const PW_PG: &str = "Aa1@:/?#%41&+ 中x";

struct Server {
    kind: String, // postgres | mysql | mariadb | mssql
    container: String,
    host: String,
    port: u16,
    admin_user: String,
    admin_pw: String,
}

fn server() -> Option<Server> {
    let url = std::env::var("SIP_FAXD_TEST_DB")
        .ok()
        .filter(|u| !u.trim().is_empty())?;
    if url.starts_with("sqlite") {
        return None;
    }
    let container = match std::env::var("SIP_FAXD_TEST_DB_CONTAINER") {
        Ok(c) if !c.is_empty() => c,
        _ => panic!(
            "SIP_FAXD_TEST_DB is a server: SIP_FAXD_TEST_DB_CONTAINER (its Docker container) is needed too"
        ),
    };
    let u = url::Url::parse(&url.replacen("mariadb:", "mysql:", 1).replacen(
        "sqlserver:",
        "mssql:",
        1,
    ))
    .unwrap();
    let kind = if url.starts_with("mariadb:") {
        "mariadb"
    } else if url.starts_with("mssql:") || url.starts_with("sqlserver:") {
        "mssql"
    } else if url.starts_with("mysql:") {
        "mysql"
    } else {
        "postgres"
    };
    let dec = |s: &str| {
        percent_encoding::percent_decode_str(s)
            .decode_utf8()
            .unwrap()
            .into_owned()
    };
    Some(Server {
        kind: kind.into(),
        container,
        host: u.host_str().unwrap().to_string(),
        port: u.port().expect("a port in SIP_FAXD_TEST_DB"),
        admin_user: dec(u.username()),
        admin_pw: dec(u.password().expect("a password in SIP_FAXD_TEST_DB")),
    })
}

fn sh(container: &str, args: &[&str]) {
    let o = Command::new("docker")
        .arg("exec")
        .arg(container)
        .args(args)
        .output()
        .expect("docker exec");
    assert!(
        o.status.success(),
        "docker exec {container} {}: {}{}",
        args[0],
        String::from_utf8_lossy(&o.stdout),
        String::from_utf8_lossy(&o.stderr)
    );
}

/// A database and an account that owns it, removed again when dropped.
struct Account {
    s: Server,
    user: String,
    db: String,
}

static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

impl Account {
    fn create(s: Server, password: &str) -> Account {
        let n = SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        let user = format!("m4f_u_{}_{n}", std::process::id());
        let db = format!("m4f_d_{}_{n}", std::process::id());
        let c = s.container.as_str();
        match s.kind.as_str() {
            "postgres" => sh(
                c,
                &[
                    "psql",
                    "-U",
                    &s.admin_user,
                    "-v",
                    "ON_ERROR_STOP=1",
                    "-c",
                    &format!("CREATE ROLE {user} LOGIN PASSWORD '{password}'"),
                    "-c",
                    &format!("CREATE DATABASE {db} OWNER {user}"),
                ],
            ),
            "mysql" | "mariadb" => {
                let client = if s.kind == "mariadb" {
                    "mariadb"
                } else {
                    "mysql"
                };
                sh(
                    c,
                    &[
                        client,
                        &format!("-u{}", s.admin_user),
                        &format!("-p{}", s.admin_pw),
                        "--default-character-set=utf8mb4",
                        "-e",
                        &format!(
                            "CREATE USER '{user}'@'%' IDENTIFIED BY '{password}'; CREATE DATABASE {db} CHARACTER SET utf8mb4; GRANT ALL ON {db}.* TO '{user}'@'%'"
                        ),
                    ],
                );
            }
            _ => sh(
                c,
                &[
                    "/opt/mssql-tools18/bin/sqlcmd",
                    "-C",
                    "-S",
                    "localhost",
                    "-U",
                    &s.admin_user,
                    "-P",
                    &s.admin_pw,
                    "-b",
                    "-Q",
                    &format!(
                        "CREATE LOGIN [{user}] WITH PASSWORD = N'{password}', CHECK_POLICY = OFF; CREATE DATABASE [{db}]; ALTER AUTHORIZATION ON DATABASE::[{db}] TO [{user}]"
                    ),
                ],
            ),
        }
        Account { s, user, db }
    }

    /// `[database]` fields for this account with `password` (as it is).
    fn fields(&self, password: &str, name: &str, port: Option<u16>) -> String {
        let t = if self.s.kind == "mssql" {
            "trust_server_certificate = true\n"
        } else {
            ""
        };
        format!(
            "type = \"{}\"\nhost = \"{}\"\nport = {}\nname = \"{name}\"\nuser = \"{}\"\npassword = {password:?}\n{t}",
            self.s.kind,
            self.s.host,
            port.unwrap_or(self.s.port),
            self.user
        )
    }

    /// The same as one URL (user and password percent-encoded).
    fn url(&self, password: &str) -> String {
        let scheme = &self.s.kind;
        let t = if self.s.kind == "mssql" {
            "?trust_server_certificate=true"
        } else {
            ""
        };
        format!(
            "{scheme}://{}:{}@{}:{}/{}{t}",
            encoded(&self.user),
            encoded(password),
            self.s.host,
            self.s.port,
            self.db
        )
    }
}

impl Drop for Account {
    fn drop(&mut self) {
        let c = self.s.container.as_str();
        let (u, d) = (&self.user, &self.db);
        let _ = Command::new("docker")
            .arg("exec")
            .arg(c)
            .args(match self.s.kind.as_str() {
                "postgres" => vec![
                    "psql".to_string(), "-U".into(), self.s.admin_user.clone(),
                    "-c".into(), format!("DROP DATABASE IF EXISTS {d} WITH (FORCE)"),
                    "-c".into(), format!("DROP ROLE IF EXISTS {u}"),
                ],
                "mssql" => vec![
                    "/opt/mssql-tools18/bin/sqlcmd".to_string(), "-C".into(), "-S".into(), "localhost".into(),
                    "-U".into(), self.s.admin_user.clone(), "-P".into(), self.s.admin_pw.clone(), "-Q".into(),
                    format!("ALTER DATABASE [{d}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{d}]; DROP LOGIN [{u}]"),
                ],
                k => vec![
                    if k == "mariadb" { "mariadb" } else { "mysql" }.to_string(),
                    format!("-u{}", self.s.admin_user), format!("-p{}", self.s.admin_pw), "-e".into(),
                    format!("DROP DATABASE IF EXISTS {d}; DROP USER IF EXISTS '{u}'@'%'"),
                ],
            })
            .output();
    }
}

fn skip(what: &str) {
    eprintln!(
        "{what}: skipped (SIP_FAXD_TEST_DB names no server database; scripts/db-matrix.sh sets it)"
    );
}

fn took_less_than_20s(t: Instant, what: &str) {
    assert!(
        t.elapsed() < Duration::from_secs(20),
        "{what} took {:?}",
        t.elapsed()
    );
}

// ---------------------------------------------------------------- D02 + K01 (+ K04, P01)
#[tokio::test(flavor = "multi_thread")]
async fn d02_k01_a_password_with_every_awkward_character_works_field_by_field_and_in_a_file() {
    let Some(s) = server() else {
        return skip("d02/k01");
    };
    let kind = s.kind.clone();
    let pw = if kind == "postgres" { PW_PG } else { PW_EMOJI };
    let a = Account::create(s, pw);
    let dir = scratch("d02");

    // fields: password as it is
    let cfg = config(&dir, "a.toml", &a.fields(pw, &a.db, None), None, "");
    let o = run(&["db", "check"], &cfg);
    assert_eq!(o.status.code(), Some(0), "{kind}: {}", both(&o));
    let t = out(&o);
    assert!(
        t.contains(&format!(
            "({})",
            if kind == "mariadb" {
                "mysql"
            } else {
                kind.as_str()
            }
        )) || t.contains(&format!("({kind})")),
        "{t}"
    );
    assert!(t.contains(&format!("{}:***@", encoded(&a.user))), "{t}");
    assert!(t.contains("no tables yet"), "an empty database: {t}");
    let ver = t
        .lines()
        .find_map(|l| l.trim().strip_prefix("server version: "))
        .unwrap_or_else(|| panic!("{t}"));
    assert!(
        ver.chars().next().unwrap().is_ascii_digit(),
        "a server version: {ver}"
    );
    assert_no_password(&kind, pw, &both(&o));

    // control: the same account with a slightly wrong password is refused (the account is checked)
    let bad = config(
        &dir,
        "bad.toml",
        &a.fields(&format!("{pw}y"), &a.db, None),
        None,
        "",
    );
    let t0 = Instant::now();
    let o = run(&["db", "check"], &bad);
    took_less_than_20s(t0, "wrong password");
    assert_eq!(o.status.code(), Some(1), "{kind}: {}", both(&o));

    // the password in a file
    let pwf = dir.join("pw");
    std::fs::write(&pwf, format!("{pw}\n")).unwrap();
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&pwf, std::fs::Permissions::from_mode(0o600)).unwrap();
    }
    let f = a.fields(pw, &a.db, None).replace(
        &format!("password = {pw:?}\n"),
        &format!("password_file = {:?}\n", slashes(&pwf)),
    );
    assert!(f.contains("password_file"));
    let cfg_f = config(&dir, "f.toml", &f, None, "");
    let o = run(&["db", "check"], &cfg_f);
    assert_eq!(o.status.code(), Some(0), "{kind}: {}", both(&o));

    // the URL spelled by hand (percent-encoded) reaches the same database
    let o = run_raw_stdin(&["db", "check", "--url", &a.url(pw)], "");
    assert_eq!(o.status.code(), Some(0), "{kind}: {}", both(&o));

    // K04: URL without the password, the password from stdin as it is
    let no_pw = a.url(pw).replacen(&format!(":{}@", encoded(pw)), "@", 1);
    assert!(!no_pw.contains(&encoded(pw)));
    let o = run_raw_stdin(
        &["db", "check", "--url", &no_pw, "--password-stdin"],
        &format!("{pw}\n"),
    );
    assert_eq!(o.status.code(), Some(0), "{kind}: {}", both(&o));
    assert!(
        out(&o).contains(&format!("{}:***@", encoded(&a.user))),
        "{}",
        out(&o)
    );
    assert_no_password(&kind, pw, &both(&o));
    // ... and refused when the URL already has one
    let o = run_raw_stdin(
        &["db", "check", "--url", &a.url(pw), "--password-stdin"],
        &format!("{pw}\n"),
    );
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(err(&o).contains("already has a password"), "{}", err(&o));

    // P01: a SQLite installation moves into this database, by fields and by stdin
    let src = scratch("d02-src");
    let src_cfg = config(
        &src,
        "s.toml",
        &sqlite_fields(&src, "s.db"),
        None,
        "[[trunks]]\nname = \"local\"\nallowed_sources = [\"127.0.0.0/8\"]\n",
    );
    let o = run(&["config", "import"], &src_cfg);
    assert!(o.status.success(), "{}", both(&o));
    let o = run_stdin(
        &["user", "add", "amy", "--role", "admin", "--password-stdin"],
        &src_cfg,
        "amy password 12\n",
    );
    assert!(o.status.success(), "{}", both(&o));
    let want = store::test_row_counts(&sqlite_url(&src, "s.db"))
        .await
        .unwrap();
    assert!(want.iter().map(|(_, n)| n).sum::<u64>() > 5);

    // (a) --to-password-stdin with a URL that has no password: copies, masks, leaks nothing
    let mut child = bin()
        .args([
            "db",
            "copy",
            "--to",
            &no_pw,
            "--to-password-stdin",
            "--config",
        ])
        .arg(&src_cfg)
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .unwrap();
    {
        use std::io::Write;
        child
            .stdin
            .take()
            .unwrap()
            .write_all(format!("{pw}\n").as_bytes())
            .unwrap();
    }
    let o = child.wait_with_output().unwrap();
    assert_eq!(o.status.code(), Some(0), "{kind}: {}", both(&o));
    assert_no_password(&kind, pw, &both(&o));
    assert!(out(&o).contains("verified"), "{}", out(&o));
    assert_eq!(
        store::test_row_counts(&a.url(pw)).await.unwrap(),
        want,
        "{kind}: the copy has every row"
    );

    // K01 again: now with its tables
    let o = run(&["db", "check"], &cfg);
    assert_eq!(o.status.code(), Some(0), "{kind}: {}", both(&o));
    assert!(out(&o).contains("up to date"), "{}", out(&o));
    assert!(out(&o).trim_end().ends_with("ok"), "{}", out(&o));

    // (b) a second, empty database of the same account, filled with --to-config (fields)
    let db2 = format!("{}_b", a.db);
    let b = Account2::create(&a, &db2);
    let tcfg = config(&dir, "t.toml", &a.fields(pw, &db2, None), None, "");
    let o = run(&["db", "copy", "--to-config", &slashes(&tcfg)], &src_cfg);
    assert_eq!(o.status.code(), Some(0), "{kind}: {}", both(&o));
    assert_no_password(&kind, pw, &both(&o));
    let url2 = a
        .url(pw)
        .replacen(&format!("/{}", a.db), &format!("/{db2}"), 1);
    assert_eq!(store::test_row_counts(&url2).await.unwrap(), want);
    drop(b);
}

/// A second database owned by the account of `a` (dropped with the account's server session).
struct Account2<'a>(&'a Account, String);
impl<'a> Account2<'a> {
    fn create(a: &'a Account, db: &str) -> Self {
        let c = a.s.container.as_str();
        match a.s.kind.as_str() {
            "postgres" => sh(
                c,
                &[
                    "psql",
                    "-U",
                    &a.s.admin_user,
                    "-v",
                    "ON_ERROR_STOP=1",
                    "-c",
                    &format!("CREATE DATABASE {db} OWNER {}", a.user),
                ],
            ),
            "mssql" => sh(
                c,
                &[
                    "/opt/mssql-tools18/bin/sqlcmd",
                    "-C",
                    "-S",
                    "localhost",
                    "-U",
                    &a.s.admin_user,
                    "-P",
                    &a.s.admin_pw,
                    "-b",
                    "-Q",
                    &format!(
                        "CREATE DATABASE [{db}]; ALTER AUTHORIZATION ON DATABASE::[{db}] TO [{}]",
                        a.user
                    ),
                ],
            ),
            k => sh(
                c,
                &[
                    if k == "mariadb" { "mariadb" } else { "mysql" },
                    &format!("-u{}", a.s.admin_user),
                    &format!("-p{}", a.s.admin_pw),
                    "-e",
                    &format!(
                        "CREATE DATABASE {db} CHARACTER SET utf8mb4; GRANT ALL ON {db}.* TO '{}'@'%'",
                        a.user
                    ),
                ],
            ),
        }
        Account2(a, db.to_string())
    }
}
impl Drop for Account2<'_> {
    fn drop(&mut self) {
        let (a, d) = (self.0, &self.1);
        let _ = Command::new("docker")
            .arg("exec")
            .arg(&a.s.container)
            .args(match a.s.kind.as_str() {
                "postgres" => vec!["psql".to_string(), "-U".into(), a.s.admin_user.clone(), "-c".into(), format!("DROP DATABASE IF EXISTS {d} WITH (FORCE)")],
                "mssql" => vec![
                    "/opt/mssql-tools18/bin/sqlcmd".to_string(), "-C".into(), "-S".into(), "localhost".into(), "-U".into(), a.s.admin_user.clone(),
                    "-P".into(), a.s.admin_pw.clone(), "-Q".into(),
                    format!("ALTER DATABASE [{d}] SET SINGLE_USER WITH ROLLBACK IMMEDIATE; DROP DATABASE [{d}]"),
                ],
                k => vec![
                    if k == "mariadb" { "mariadb" } else { "mysql" }.to_string(), format!("-u{}", a.s.admin_user), format!("-p{}", a.s.admin_pw), "-e".into(),
                    format!("DROP DATABASE IF EXISTS {d}"),
                ],
            })
            .output();
    }
}

// ---------------------------------------------------------------- K02
#[tokio::test(flavor = "multi_thread")]
async fn k02_on_a_server_empty_older_and_newer_schemas_are_reported_and_nothing_is_changed() {
    let Some(s) = server() else {
        return skip("k02");
    };
    let kind = s.kind.clone();
    let a = Account::create(s, "Plain_pw_1");
    let dir = scratch("k02s");
    let cfg = config(
        &dir,
        "a.toml",
        &a.fields("Plain_pw_1", &a.db, None),
        None,
        "",
    );
    let url = a.url("Plain_pw_1");

    // empty: no schema_migrations is created
    let o = run(&["db", "check"], &cfg);
    assert_eq!(o.status.code(), Some(0), "{kind}: {}", both(&o));
    assert!(
        out(&o).contains("no tables yet") && out(&o).contains("db migrate"),
        "{}",
        out(&o)
    );
    assert_eq!(
        store::test_schema_versions(&url).await.unwrap(),
        None,
        "{kind}: db check created schema_migrations"
    );

    // newer: a version this binary does not know (made on top of the migrated schema)
    let o = run(&["db", "migrate"], &cfg);
    assert_eq!(o.status.code(), Some(0), "{kind}: {}", both(&o));
    let all = store::test_schema_versions(&url).await.unwrap().unwrap();
    let latest = *all.last().unwrap();
    store::test_record_schema_version(&url, latest + 5)
        .await
        .unwrap();
    let now = store::test_schema_versions(&url).await.unwrap().unwrap();
    assert_eq!(now.last(), Some(&(latest + 5)), "{kind}: {now:?}");
    let o = run(&["db", "check"], &cfg);
    assert_eq!(o.status.code(), Some(1), "{kind}: {}", both(&o));
    assert!(
        out(&o).contains("NEWER") && out(&o).contains("newer sip-faxd"),
        "{}",
        out(&o)
    );
    assert_eq!(
        store::test_schema_versions(&url).await.unwrap().unwrap(),
        now,
        "{kind}: db check changed schema_migrations"
    );

    // older: the last real version missing
    store::test_forget_schema_version(&url, latest + 5)
        .await
        .unwrap();
    store::test_forget_schema_version(&url, latest)
        .await
        .unwrap();
    let before = store::test_schema_versions(&url).await.unwrap().unwrap();
    assert_eq!(before.len(), all.len() - 1);
    let o = run(&["db", "check"], &cfg);
    assert_eq!(o.status.code(), Some(0), "{kind}: {}", both(&o));
    assert!(
        out(&o).contains("older") && out(&o).contains("db migrate"),
        "{}",
        out(&o)
    );
    assert_eq!(
        store::test_schema_versions(&url).await.unwrap().unwrap(),
        before,
        "{kind}: db check changed schema_migrations"
    );
}

// ---------------------------------------------------------------- K03
#[tokio::test(flavor = "multi_thread")]
async fn k03_on_a_server_a_wrong_password_a_missing_database_and_an_untrusted_certificate_are_told_apart()
 {
    let Some(s) = server() else {
        return skip("k03");
    };
    let kind = s.kind.clone();
    let a = Account::create(s, "Plain_pw_1");
    let dir = scratch("k03s");

    // wrong password
    let cfg = config(
        &dir,
        "pw.toml",
        &a.fields("Wr0ng_pw_Zzz", &a.db, None),
        None,
        "",
    );
    let t = Instant::now();
    let o = run(&["db", "check"], &cfg);
    took_less_than_20s(t, "wrong password");
    assert_eq!(o.status.code(), Some(1), "{kind}: {}", both(&o));
    assert!(
        err(&o).contains("hint: the user name or the password is wrong"),
        "{kind}: {}",
        err(&o)
    );
    assert_no_password(&kind, "Wr0ng_pw_Zzz", &both(&o));
    assert_no_password(&kind, "Plain_pw_1", &both(&o));

    // a database that does not exist
    let cfg = config(
        &dir,
        "db.toml",
        &a.fields("Plain_pw_1", "no_such_db_m4f", None),
        None,
        "",
    );
    let t = Instant::now();
    let o = run(&["db", "check"], &cfg);
    took_less_than_20s(t, "missing database");
    assert_eq!(o.status.code(), Some(1), "{kind}: {}", both(&o));
    // (MySQL and MariaDB answer a login to an unknown database with "access denied": either
    // hint tells the person where to look)
    let e = err(&o);
    let by_kind = if kind == "mysql" || kind == "mariadb" {
        e.contains("hint: the database does not exist")
            || e.contains("hint: the user name or the password is wrong")
    } else {
        e.contains("hint: the database does not exist")
    };
    assert!(by_kind, "{kind}: {e}");

    // the port is closed
    let t = Instant::now();
    let cfg = config(
        &dir,
        "port.toml",
        &a.fields("Plain_pw_1", &a.db, Some(free_port())),
        None,
        "",
    );
    let o = run(&["db", "check"], &cfg);
    took_less_than_20s(t, "closed port");
    assert_eq!(o.status.code(), Some(1), "{kind}: {}", both(&o));
    assert!(
        err(&o).contains("nothing listens on that host and port"),
        "{kind}: {}",
        err(&o)
    );

    // SQL Server with its self-signed certificate: without trust_server_certificate the hint
    // names that setting; with it the same account works
    if kind == "mssql" {
        let strict = a
            .fields("Plain_pw_1", &a.db, None)
            .replace("trust_server_certificate = true\n", "");
        assert!(!strict.contains("trust_server_certificate"));
        let cfg = config(&dir, "tls.toml", &strict, None, "");
        let t = Instant::now();
        let o = run(&["db", "check"], &cfg);
        took_less_than_20s(t, "untrusted certificate");
        assert_eq!(o.status.code(), Some(1), "{}", both(&o));
        assert!(err(&o).contains("trust_server_certificate"), "{}", err(&o));
        let cfg = config(
            &dir,
            "tls-ok.toml",
            &a.fields("Plain_pw_1", &a.db, None),
            None,
            "",
        );
        let o = run(&["db", "check"], &cfg);
        assert_eq!(o.status.code(), Some(0), "{}", both(&o));
    }
}

// ---------------------------------------------------------------- D02 (PostgreSQL and the emoji)
#[tokio::test(flavor = "multi_thread")]
async fn d02_postgres_refuses_an_emoji_in_the_password_and_the_hint_says_so() {
    let Some(s) = server() else {
        return skip("d02 emoji");
    };
    if s.kind != "postgres" {
        return eprintln!(
            "d02 emoji: only PostgreSQL has the rule (the others accepted the emoji in d02_k01)"
        );
    }
    // the account is made with a password that is fine; the program is given one with an emoji
    let a = Account::create(s, "Plain_pw_1");
    let dir = scratch("d02-emoji");
    let cfg = config(&dir, "a.toml", &a.fields(PW_EMOJI, &a.db, None), None, "");
    let o = run(&["db", "check"], &cfg);
    assert_eq!(o.status.code(), Some(1), "{}", both(&o));
    assert!(
        err(&o).contains("hint: PostgreSQL's password check"),
        "{}",
        err(&o)
    );
    assert_no_password("postgres emoji", PW_EMOJI, &both(&o));
}
