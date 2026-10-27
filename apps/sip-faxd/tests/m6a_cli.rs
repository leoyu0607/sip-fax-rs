//! M6a (ADR-070) on the real `sip-faxd` executable and a real SQLite file: K10 (the `apikey`
//! subcommands, with no service running, and keys that are the same keys as those of the web
//! interface) and K11 (`db copy` takes the keys along, and a copied key works in the target).

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::Arc;

use accounts::{ApiKeyError, AuthConfig, AuthService};
use chrono::Utc;
use store::{FaxStore, StoreSource};

fn bin() -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_sip-faxd"));
    // the operator recorded for changes: cli:tester
    c.env("USER", "tester")
        .env("USERNAME", "tester")
        .env_remove("SUDO_USER");
    c
}

fn scratch(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("m6a-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn slashes(p: &Path) -> String {
    p.display().to_string().replace('\\', "/")
}

fn url_of(dir: &Path, file: &str) -> String {
    format!("sqlite://{}", slashes(&dir.join(file)))
}

fn config(dir: &Path) -> PathBuf {
    let text = format!(
        "[web]\nenabled = false\n[storage]\ndata_dir = {data:?}\n[capture]\nenabled = false\n[database]\nurl = {url:?}\n\
         [log]\nfile = \"\"\n[service]\npid_file = {pid:?}\nsocket = {sock:?}\n\
         [sip]\nbind = \"127.0.0.1:0\"\n[[trunks]]\nname = \"local\"\nallowed_sources = [\"127.0.0.0/8\"]\n",
        data = slashes(&dir.join("data")),
        url = url_of(dir, "fax.db"),
        pid = slashes(&dir.join("sip-faxd.pid")),
        sock = slashes(&dir.join("sip-faxd.sock")),
    );
    let p = dir.join("sip-faxd.toml");
    std::fs::write(&p, text).unwrap();
    p
}

fn run(args: &[&str], cfg: &Path) -> Output {
    bin().args(args).arg("--config").arg(cfg).output().unwrap()
}
fn out(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn err(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

/// The key a `create` printed (the one line that starts with `sfx_`).
fn printed_key(o: &Output) -> String {
    out(o)
        .lines()
        .find(|l| l.starts_with("sfx_"))
        .unwrap_or_else(|| panic!("a key line in {}", out(o)))
        .trim()
        .to_string()
}

async fn auth_of(url: &str) -> (AuthService, Arc<dyn FaxStore>) {
    let st = store::connect(url).await.unwrap();
    let src: Arc<dyn StoreSource> = Arc::new(st.clone());
    (
        AuthService::new(src, Arc::new(store::SystemClock), AuthConfig::default()),
        st,
    )
}

/// The file contains `needle` nowhere (not as text, not in the write-ahead log).
fn file_lacks(dir: &Path, needle: &str) {
    for f in std::fs::read_dir(dir).unwrap().flatten() {
        let p = f.path();
        if p.is_file()
            && p.file_name()
                .is_some_and(|n| n.to_string_lossy().starts_with("fax.db"))
        {
            let b = std::fs::read(&p).unwrap();
            assert!(
                !b.windows(needle.len()).any(|w| w == needle.as_bytes()),
                "{} has the key",
                p.display()
            );
        }
    }
}

// ---------------------------------------------------------------- K10
#[tokio::test(flavor = "multi_thread")]
async fn k10_create_list_and_revoke_without_a_running_service() {
    let dir = scratch("k10");
    let cfg = config(&dir);

    // create: the key is printed once, to stdout
    let o = run(&["apikey", "create", "erp"], &cfg);
    assert!(o.status.success(), "{}", err(&o));
    let erp = printed_key(&o);
    assert!(erp.starts_with("sfx_") && erp.len() == 47, "{erp}");
    assert!(
        out(&o).contains("API key erp created (fax:read)"),
        "{}",
        out(&o)
    );
    assert!(out(&o).contains("cannot be shown again"), "{}", out(&o));
    assert!(!err(&o).contains(&erp), "not on stderr");
    // write rights and an expiry (a day, in local time)
    let o = run(
        &[
            "apikey",
            "create",
            "billing",
            "--write",
            "--expires",
            "2099-12-31",
        ],
        &cfg,
    );
    assert!(o.status.success(), "{}", err(&o));
    let billing = printed_key(&o);
    assert_ne!(billing, erp);
    assert!(out(&o).contains("fax:read, fax:write"), "{}", out(&o));
    assert!(
        out(&o).contains("valid until 2099-12-31 23:59:59")
            || out(&o).contains("valid until 2100-01-01 00:00:00"),
        "{}",
        out(&o)
    );
    // an RFC 3339 expiry works as well
    let o = run(
        &[
            "apikey",
            "create",
            "rfc",
            "--expires",
            "2099-01-01T00:00:00Z",
        ],
        &cfg,
    );
    assert!(o.status.success(), "{}", err(&o));

    // refused: a name that is taken, a bad name, an expiry in the past or unreadable; no key printed
    for args in [
        vec!["apikey", "create", "erp"],
        vec!["apikey", "create", "ERP"],
        vec!["apikey", "create", "bad name"],
        vec!["apikey", "create", "x/y"],
        vec!["apikey", "create", "late", "--expires", "2000-01-01"],
        vec!["apikey", "create", "junk", "--expires", "next week"],
    ] {
        let o = run(&args, &cfg);
        assert!(!o.status.success(), "{args:?} must fail: {}", out(&o));
        assert!(!out(&o).contains("sfx_"), "{args:?} printed a key");
        assert!(!err(&o).is_empty(), "{args:?} says why");
    }
    let o = run(&["apikey", "create", "erp"], &cfg);
    assert!(err(&o).contains("name_taken"), "{}", err(&o));

    // list: a table with the prefix, the rights, the state and who made it; never the key
    let o = run(&["apikey", "list"], &cfg);
    assert!(o.status.success(), "{}", err(&o));
    let text = out(&o);
    assert!(text.lines().next().unwrap().starts_with("NAME"), "{text}");
    for k in [&erp, &billing] {
        assert!(!text.contains(&k[12..]), "the list does not show the key");
    }
    let line = |name: &str| {
        text.lines()
            .find(|l| l.starts_with(name))
            .unwrap_or_else(|| panic!("{name} in {text}"))
            .to_string()
    };
    assert!(
        line("erp").contains(&erp[..12])
            && line("erp").contains("fax:read")
            && line("erp").contains("active")
            && line("erp").contains("cli:tester"),
        "{}",
        line("erp")
    );
    assert!(
        line("billing").contains("fax:read,fax:write"),
        "{}",
        line("billing")
    );
    assert!(
        !text
            .lines()
            .any(|l| l.starts_with("late") || l.starts_with("junk") || l.starts_with("x/y")),
        "{text}"
    );

    // the same keys as the web interface's: this service object finds the CLI's key valid, and a
    // key made here (as the web does) is in the CLI's list
    let (auth, _st) = auth_of(&url_of(&dir, "fax.db")).await;
    let a = auth
        .authenticate_api_key(&erp, Some("127.0.0.1"))
        .await
        .unwrap();
    assert_eq!(a.name, "erp");
    assert!(!a.write);
    assert!(
        auth.authenticate_api_key(&billing, None)
            .await
            .unwrap()
            .write
    );
    let made = auth
        .create_api_key("from-web", false, None, "boss")
        .await
        .unwrap();
    let o = run(&["apikey", "list"], &cfg);
    assert!(
        out(&o).contains("from-web") && out(&o).contains("boss"),
        "{}",
        out(&o)
    );
    assert!(out(&o).contains("LAST USED"));
    // ... and its last use, written by the authentication above
    let o = run(&["apikey", "list"], &cfg);
    let erp_line = out(&o)
        .lines()
        .find(|l| l.starts_with("erp"))
        .unwrap()
        .to_string();
    assert!(
        !erp_line.contains(" - cli:"),
        "the last use shows: {erp_line}"
    );

    // revoke: at once, for the service object too; the row stays (listed as revoked)
    let o = run(&["apikey", "revoke", "erp"], &cfg);
    assert!(o.status.success(), "{}", err(&o));
    assert!(out(&o).contains("API key erp revoked"), "{}", out(&o));
    assert!(matches!(
        auth.authenticate_api_key(&erp, None).await,
        Err(ApiKeyError::Invalid)
    ));
    let o = run(&["apikey", "list"], &cfg);
    let l = out(&o)
        .lines()
        .find(|l| l.starts_with("erp"))
        .unwrap()
        .to_string();
    assert!(l.contains("revoked"), "{l}");
    // an unknown name, or a key revoked already: non-zero, and nothing else changes
    for name in ["nobody", "erp", "ERP"] {
        let o = run(&["apikey", "revoke", name], &cfg);
        assert_eq!(o.status.code(), Some(1), "{name}: {}", out(&o));
        assert!(!err(&o).is_empty());
    }
    assert!(
        auth.authenticate_api_key(&billing, None).await.is_ok(),
        "billing is untouched"
    );
    assert!(auth.authenticate_api_key(&made.key, None).await.is_ok());
    // the CLI audit rows: who did it
    let (_, st) = auth_of(&url_of(&dir, "fax.db")).await;
    let audit = st.config_audit_list(100, None).await.unwrap();
    let rows: Vec<_> = audit
        .iter()
        .filter(|r| r.section == "api_key:erp")
        .collect();
    assert_eq!(rows.len(), 2, "{rows:?}");
    assert!(rows.iter().all(|r| r.by == "cli:tester"), "{rows:?}");
    // the key is nowhere: not in the file, not in the audit text
    for k in [&erp, &billing, &made.key] {
        file_lacks(&dir, k);
        assert!(!format!("{audit:?}").contains(&k[12..]));
    }
    // a wrong database does not print a key
    let bad = dir.join("bad.toml");
    std::fs::write(
        &bad,
        std::fs::read_to_string(&cfg)
            .unwrap()
            .replace("sqlite://", "sqlite:///nonexistent-dir-m6a/"),
    )
    .unwrap();
    let o = run(&["apikey", "create", "nope"], &bad);
    assert!(!o.status.success());
    assert!(!out(&o).contains("sfx_"));
}

// ---------------------------------------------------------------- K11
#[tokio::test(flavor = "multi_thread")]
async fn k11_db_copy_takes_the_keys_and_a_copied_key_works_in_the_target() {
    let dir = scratch("k11");
    let cfg = config(&dir);
    let keep = printed_key(&run(&["apikey", "create", "keep", "--write"], &cfg));
    let gone = printed_key(&run(&["apikey", "create", "gone"], &cfg));
    let late = printed_key(&run(
        &["apikey", "create", "late", "--expires", "2099-06-30"],
        &cfg,
    ));
    assert!(run(&["apikey", "revoke", "gone"], &cfg).status.success());
    // a use that was written
    let (src_auth, src_st) = auth_of(&url_of(&dir, "fax.db")).await;
    src_auth
        .authenticate_api_key(&keep, Some("192.0.2.44"))
        .await
        .unwrap();
    let source_rows = src_st.api_key_list().await.unwrap();
    assert_eq!(source_rows.len(), 3);
    src_st.close().await;

    let target = url_of(&dir, "target.db");
    let o = run(&["db", "copy", "--to", &target], &cfg);
    assert!(o.status.success(), "{}{}", out(&o), err(&o));
    assert!(
        out(&o)
            .lines()
            .any(|l| l.trim_start().starts_with("api_keys") && l.contains("3")),
        "{}",
        out(&o)
    );

    let (auth, st) = auth_of(&target).await;
    // row for row the same, ids included
    assert_eq!(st.api_key_list().await.unwrap(), source_rows);
    // the keys work in the target as they did in the source
    let a = auth.authenticate_api_key(&keep, None).await.unwrap();
    assert_eq!((a.name.as_str(), a.write), ("keep", true));
    assert!(!auth.authenticate_api_key(&late, None).await.unwrap().write);
    assert!(
        matches!(
            auth.authenticate_api_key(&gone, None).await,
            Err(ApiKeyError::Invalid)
        ),
        "a revoked key stays revoked"
    );
    // the names stay taken in the target, and a new key gets a fresh id
    assert!(auth.create_api_key("gone", false, None, "x").await.is_err());
    let fresh = auth
        .create_api_key("fresh", false, None, "x")
        .await
        .unwrap();
    assert!(fresh.info.id > source_rows.iter().map(|k| k.id).max().unwrap());
    // the last use came along (the copy keeps what the source had)
    let keep_row = st
        .api_key_list()
        .await
        .unwrap()
        .into_iter()
        .find(|k| k.name == "keep")
        .unwrap();
    assert_eq!(keep_row.last_used_ip.as_deref(), Some("192.0.2.44"));
    assert!(keep_row.last_used_at.unwrap() <= Utc::now());
}
