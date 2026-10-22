//! M4f K01/K02/K03 at the store level (`check_url`, behind `sip-faxd db check`): the report on a
//! current, an older, a newer and an empty database, and a check that changes nothing. Run on
//! every backend by the db-matrix. (The command's output and exit codes are
//! `apps/sip-faxd/tests/m4f_cli.rs` and `m4f_server.rs`.)

use super::*;
use crate::check::check_url;
use crate::migrate::{current_version, latest_version};
use std::time::Duration as StdDuration;

const LIMIT: StdDuration = StdDuration::from_secs(15);

/// A file or server database; an in-memory one is a different database for every connection.
fn checkable(env: &Env) -> bool {
    if env.url.contains(":memory:") {
        eprintln!("[{}] check_url: skipped (in-memory database)", env.label);
        return false;
    }
    true
}

async fn versions(env: &Env) -> Vec<i64> {
    let rows = env
        .store
        .raw()
        .query(
            "SELECT version FROM schema_migrations ORDER BY version",
            &[],
            &[db::Kind::Int],
        )
        .await
        .unwrap();
    rows.iter()
        .map(|r| match &r[0] {
            db::Val::I(Some(v)) => *v,
            other => panic!("{other:?}"),
        })
        .collect()
}

async fn k01_current_schema(env: &Env) {
    if !checkable(env) {
        return;
    }
    let r = check_url(&env.url, LIMIT).await.unwrap();
    assert_eq!(r.kind, env.store.backend());
    let latest = latest_version(r.kind);
    assert!(latest >= 2);
    assert_eq!((r.schema, r.required), (latest, latest));
    assert!(
        r.server_version
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_digit()),
        "a server version: {:?}",
        r.server_version
    );
    assert!(r.elapsed < LIMIT);
    // nothing changed
    assert_eq!(versions(env).await.len(), latest as usize);
}
db_case!(
    k01_check_reports_the_server_and_a_current_schema,
    k01_current_schema
);

async fn k02_older_newer_empty(env: &Env) {
    if !checkable(env) {
        return;
    }
    let kind = env.store.backend();
    let latest = latest_version(kind);
    let be = env.store.raw();
    let all = versions(env).await;

    // older: the last version is not recorded
    be.execute(
        "DELETE FROM schema_migrations WHERE version = ?",
        &[db::Val::from(latest)],
    )
    .await
    .unwrap();
    let older = versions(env).await;
    let r = check_url(&env.url, LIMIT).await.unwrap();
    assert_eq!((r.schema, r.required), (latest - 1, latest));
    assert_eq!(versions(env).await, older, "the check wrote nothing");

    // newer: a version this binary does not know
    be.execute(
        "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?, ?, ?)",
        &[
            db::Val::from(latest + 5),
            db::Val::from("from-the-future"),
            db::Val::from(chrono::Utc::now()),
        ],
    )
    .await
    .unwrap();
    let newer = versions(env).await;
    let r = check_url(&env.url, LIMIT).await.unwrap();
    assert_eq!((r.schema, r.required), (latest + 5, latest));
    assert_eq!(versions(env).await, newer, "the check wrote nothing");
    assert_eq!(newer.len(), all.len());

    // empty: no schema_migrations table; the check does not create it
    be.execute("DROP TABLE schema_migrations", &[])
        .await
        .unwrap();
    let r = check_url(&env.url, LIMIT).await.unwrap();
    assert_eq!((r.schema, r.required), (0, latest));
    assert_eq!(
        current_version(be).await.unwrap(),
        0,
        "the check did not create schema_migrations"
    );
}
db_case!(
    k02_check_reports_older_newer_and_empty_schemas_and_writes_nothing,
    k02_older_newer_empty
);

#[tokio::test]
async fn k03_a_missing_sqlite_file_is_not_created_and_bad_targets_are_errors() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("nothing-here.db");
    let url = format!("sqlite://{}", file.display());
    let e = check_url(&url, LIMIT).await.unwrap_err();
    assert!(matches!(e, Error::NotFound(_)), "{e:?}");
    assert!(e.to_string().contains("does not exist"), "{e}");
    assert!(!file.exists(), "the check created the SQLite file");
    assert_eq!(std::fs::read_dir(dir.path()).unwrap().count(), 0);

    // not a database URL
    assert!(matches!(
        check_url("ftp://example.com/x", LIMIT).await.unwrap_err(),
        Error::BadUrl(_)
    ));
    // a port nobody listens on, with a limit shorter than the driver's own: the limit speaks
    let t = std::time::Instant::now();
    let e = check_url("postgres://u:p@127.0.0.1:1/x", StdDuration::from_millis(30))
        .await
        .unwrap_err();
    assert!(matches!(e, Error::Unavailable(_)), "{e:?}");
    assert!(e.to_string().contains("no answer within"), "{e}");
    assert!(t.elapsed() < StdDuration::from_secs(5));
    // and without the limit the driver's own answer is an error too, with no password in it
    let e = check_url(
        "mysql://u:Sup3rS3cret@127.0.0.1:1/x",
        StdDuration::from_secs(20),
    )
    .await
    .unwrap_err();
    assert!(!e.to_string().contains("Sup3rS3cret"), "{e}");
}
