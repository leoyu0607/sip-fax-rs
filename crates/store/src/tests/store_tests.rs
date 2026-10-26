//! S01-S11: store behaviour, identical on every backend.

use super::*;
use crate::db::{Kind, Val};
use chrono::Duration;
use std::collections::HashSet;

fn lbl(env: &Env, what: &str) -> String {
    format!("[{}] {what}", env.label)
}

async fn count_rows(env: &Env, sql: &str) -> i64 {
    match env.store.raw().query(sql, &[], &[Kind::Int]).await.unwrap()[0][0] {
        Val::I(Some(n)) => n,
        ref v => panic!("{v:?}"),
    }
}

// ---------------------------------------------------------------- S01
db_case!(s01_migration, s01_migration_body);
async fn s01_migration_body(env: &Env) {
    let kind = env.store.backend();
    // connect() already migrated: version == latest, all tables usable.
    assert_eq!(
        env.store.schema_version().await.unwrap(),
        latest_version(kind),
        "{}",
        lbl(env, "version")
    );
    // Re-running is a no-op.
    let rep = env.store.migrate().await.unwrap();
    assert!(rep.applied.is_empty(), "{rep:?}");
    assert_eq!(rep.from, rep.to);
    for (tbl, want) in [
        ("faxes", 0),
        ("settings", 0),
        ("delivery_attempts", 0),
        ("schema_migrations", 3),
        ("config_sections", 0),
        ("config_audit", 0),
        ("users", 0),
        ("sessions", 0),
    ] {
        assert_eq!(
            count_rows(env, &format!("SELECT COUNT(*) FROM {tbl}")).await,
            want,
            "{tbl}"
        );
    }
    // Indexes from spec section 1.1 exist.
    let idx_sql = match kind {
        BackendKind::Sqlite => {
            "SELECT COUNT(*) FROM sqlite_master WHERE type = 'index' AND name LIKE 'idx_faxes_%'"
        }
        BackendKind::Postgres => {
            "SELECT COUNT(*) FROM pg_indexes WHERE tablename = 'faxes' AND indexname LIKE 'idx_faxes_%'"
        }
        BackendKind::Mysql => {
            "SELECT COUNT(DISTINCT index_name) FROM information_schema.statistics WHERE table_schema = DATABASE() AND table_name = 'faxes' AND index_name LIKE 'idx_faxes_%'"
        }
        BackendKind::Mssql => {
            "SELECT COUNT(*) FROM sys.indexes WHERE object_id = OBJECT_ID('faxes') AND name LIKE 'idx[_]faxes[_]%'"
        }
    };
    assert_eq!(
        count_rows(env, idx_sql).await,
        7,
        "{}",
        lbl(env, "index count")
    );

    // A database newer than the binary is refused (migrate() on the live store, and connect()).
    env.store
        .raw()
        .execute(
            "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?, ?, ?)",
            &[Val::from(99), "future".into(), t(T0).into()],
        )
        .await
        .unwrap();
    match env.store.migrate().await {
        Err(Error::SchemaTooNew { db: 99, supported }) => {
            assert_eq!(supported, latest_version(kind))
        }
        other => panic!(
            "{}: expected SchemaTooNew, got {other:?}",
            lbl(env, "newer")
        ),
    }
    if !env.is_memory() {
        let err = connect_with(&env.url, env.opts())
            .await
            .err()
            .expect("connect must refuse a newer database");
        assert!(matches!(err, Error::SchemaTooNew { .. }), "{err:?}");
        assert!(err.to_string().contains("newer"), "{err}");
    }
    env.store
        .raw()
        .execute(
            "DELETE FROM schema_migrations WHERE version = ?",
            &[Val::from(99)],
        )
        .await
        .unwrap();
    env.store.migrate().await.unwrap();
}

db_case!(
    s01b_empty_db_and_auto_migrate_off,
    s01b_empty_db_and_auto_migrate_off_body
);
async fn s01b_empty_db_and_auto_migrate_off_body(env: &Env) {
    if env.is_memory() {
        return; // every connection to :memory: is a new database; covered by the file target
    }
    // Drop everything, then connect with auto_migrate = false: clear "outdated" error.
    let be = env.store.raw();
    for stmt in [
        "DROP TABLE sessions",
        "DROP TABLE users",
        "DROP TABLE config_audit",
        "DROP TABLE config_sections",
        "DROP TABLE delivery_attempts",
        "DROP TABLE settings",
        "DROP TABLE faxes",
        "DROP TABLE schema_migrations",
    ] {
        be.execute(stmt, &[]).await.unwrap();
    }
    assert_eq!(env.store.schema_version().await.unwrap(), 0);
    let mut o = env.opts();
    o.auto_migrate = false;
    match connect_with(&env.url, o.clone()).await.err().unwrap() {
        Error::SchemaOutdated { db: 0, required } => {
            assert_eq!(required, latest_version(env.store.backend()))
        }
        other => panic!("{other:?}"),
    }
    // Migrating from empty applies version 1; then auto_migrate=false connects.
    let rep = env.store.migrate().await.unwrap();
    assert_eq!(
        (rep.from, rep.to, rep.applied.clone()),
        (0, 3, vec![1, 2, 3])
    );
    connect_with(&env.url, o).await.unwrap();
}

async fn drop_everything(env: &Env) {
    let be = env.store.raw();
    for stmt in [
        "DROP TABLE sessions",
        "DROP TABLE users",
        "DROP TABLE config_audit",
        "DROP TABLE config_sections",
        "DROP TABLE delivery_attempts",
        "DROP TABLE settings",
        "DROP TABLE faxes",
        "DROP TABLE schema_migrations",
    ] {
        be.execute(stmt, &[]).await.unwrap();
    }
}

// S01c (M3a review): several instances migrating the same empty database at the same instant.
db_case!(
    s01c_concurrent_first_start,
    s01c_concurrent_first_start_body
);
async fn s01c_concurrent_first_start_body(env: &Env) {
    if env.is_memory() {
        return; // every connection to :memory: is a new database
    }
    drop_everything(env).await;
    let mut tasks = Vec::new();
    for _ in 0..6 {
        let url = env.url.clone();
        let opts = env.opts();
        tasks.push(tokio::spawn(async move {
            let st = crate::open_sql(&url, &opts).await.unwrap();
            let rep = st.migrate().await;
            st.close().await;
            rep
        }));
    }
    let mut applied_by = 0;
    for t in tasks {
        let rep = t
            .await
            .unwrap()
            .unwrap_or_else(|e| panic!("{}: a racing migrate failed: {e}", lbl(env, "race")));
        assert_eq!(rep.to, latest_version(env.store.backend()), "{rep:?}");
        if !rep.applied.is_empty() {
            applied_by += 1;
            assert_eq!((rep.from, rep.applied.clone()), (0, vec![1, 2, 3]));
        } else {
            assert_eq!(
                rep.from, 3,
                "a loser starts from the version the winner wrote"
            );
        }
    }
    assert_eq!(applied_by, 1, "exactly one instance applies the migration");
    assert_eq!(
        count_rows(env, "SELECT COUNT(*) FROM schema_migrations").await,
        3
    );
    assert_eq!(env.store.schema_version().await.unwrap(), 3);
}

fn probe_sql(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::Mysql => "CREATE TABLE IF NOT EXISTS m3b_probe (id INT NOT NULL PRIMARY KEY)",
        _ => "CREATE TABLE m3b_probe (id INT NOT NULL PRIMARY KEY)",
    }
}

// S01d: a migration that fails half way. Transactional databases roll it back completely; MySQL /
// MariaDB cannot, and the next run (with the migration fixed) must still work.
db_case!(
    s01d_failed_migration_then_rerun,
    s01d_failed_migration_then_rerun_body
);
async fn s01d_failed_migration_then_rerun_body(env: &Env) {
    use crate::migrate::{Migration, apply, migrations};
    if env.is_memory() {
        return;
    }
    let kind = env.store.backend();
    // the real migrations plus a probe as the next version (the database is at the latest)
    let latest = crate::latest_version(kind);
    let with_probe = |sql: &'static str| -> Vec<Migration> {
        let mut v: Vec<Migration> = migrations(kind)
            .iter()
            .map(|m| Migration {
                version: m.version,
                name: m.name,
                sql: m.sql,
            })
            .collect();
        v.push(Migration {
            version: latest + 1,
            name: "probe",
            sql,
        });
        v
    };
    let ok_sql: &'static str = probe_sql(kind);
    let bad_sql: &'static str =
        Box::leak(format!("{ok_sql};\nINSERT INTO no_such_table VALUES (1)").into_boxed_str());
    let bad = with_probe(bad_sql);
    let err = apply(env.store.raw(), env.clock.as_ref(), &bad)
        .await
        .expect_err("the failing migration is reported");
    assert!(
        err.to_string()
            .contains(&format!("{:04}_probe", latest + 1)),
        "{err}"
    );
    assert_eq!(
        env.store.schema_version().await.unwrap(),
        latest,
        "the probe's version is not recorded"
    );
    let probe = env
        .store
        .raw()
        .query("SELECT COUNT(*) FROM m3b_probe", &[], &[Kind::Int])
        .await;
    if kind == BackendKind::Mysql {
        assert!(probe.is_ok(), "MySQL keeps the DDL that already ran");
    } else {
        assert!(
            probe.is_err(),
            "{}: undone completely",
            lbl(env, "failed migration")
        );
    }
    // the migration is fixed (it no longer fails): the rerun applies it exactly once
    let good = with_probe(ok_sql);
    let rep = apply(env.store.raw(), env.clock.as_ref(), &good)
        .await
        .unwrap();
    assert_eq!(
        (rep.from, rep.to, rep.applied),
        (latest, latest + 1, vec![latest + 1])
    );
    assert_eq!(count_rows(env, "SELECT COUNT(*) FROM m3b_probe").await, 0);
    // and the lock was released after the failure (a third run does not wait)
    let rep = apply(env.store.raw(), env.clock.as_ref(), &good)
        .await
        .unwrap();
    assert!(rep.applied.is_empty());
}

// S01e (MySQL / MariaDB only): the first run died after the first CREATE TABLEs of 0001.
db_case!(
    s01e_mysql_rerun_after_partial_init,
    s01e_mysql_rerun_after_partial_init_body
);
async fn s01e_mysql_rerun_after_partial_init_body(env: &Env) {
    use crate::migrate::{Migration, apply, migrations};
    if env.store.backend() != BackendKind::Mysql {
        return;
    }
    drop_everything(env).await;
    let init = &migrations(BackendKind::Mysql)[0];
    let stmts = crate::db::split_statements(init.sql);
    assert!(stmts.len() >= 3, "faxes, settings and delivery_attempts");
    // a run that created `faxes` and `settings` and then failed
    let dying: &'static str = Box::leak(
        format!(
            "{};\n{};\nINSERT INTO no_such_table VALUES (1)",
            stmts[0], stmts[1]
        )
        .into_boxed_str(),
    );
    let first = [Migration {
        version: 1,
        name: "init",
        sql: dying,
    }];
    assert!(
        apply(env.store.raw(), env.clock.as_ref(), &first)
            .await
            .is_err()
    );
    assert_eq!(env.store.schema_version().await.unwrap(), 0);
    assert_eq!(
        count_rows(env, "SELECT COUNT(*) FROM faxes").await,
        0,
        "half the schema exists"
    );
    // the real migration then runs through (the same statements again, then the rest)
    let rep = env.store.migrate().await.unwrap();
    assert_eq!((rep.from, rep.to, rep.applied), (0, 3, vec![1, 2, 3]));
    for tbl in ["faxes", "settings", "delivery_attempts"] {
        count_rows(env, &format!("SELECT COUNT(*) FROM {tbl}")).await;
    }
    let idx = count_rows(
        env,
        "SELECT COUNT(DISTINCT index_name) FROM information_schema.statistics WHERE table_schema = DATABASE() AND table_name = 'faxes' AND index_name LIKE 'idx_faxes_%'",
    )
    .await;
    assert_eq!(idx, 7);
    // running the whole file a second time is harmless as well
    for stmt in &stmts {
        env.store.raw().execute(stmt, &[]).await.unwrap();
    }
    // 0003 added its columns but the run died before recording the version: the next run
    // meets "Duplicate column name" and counts it as done
    env.store
        .raw()
        .execute("DELETE FROM schema_migrations WHERE version = 3", &[])
        .await
        .unwrap();
    let rep = env.store.migrate().await.unwrap();
    assert_eq!((rep.from, rep.to, rep.applied), (2, 3, vec![3]));
    count_rows(env, "SELECT COUNT(modem) + COUNT(media) FROM faxes").await;
}

// ---------------------------------------------------------------- S02
db_case!(s02_receiving_flow, s02_receiving_flow_body);
async fn s02_receiving_flow_body(env: &Env) {
    let st = &env.store;
    let started = T0 + 1_000;
    let id = st
        .insert_receiving(&nf("call-s02", "trunkA", started))
        .await
        .unwrap();
    let r = st.get(id).await.unwrap().unwrap();
    assert_eq!(r.fax_status, FaxStatus::Receiving);
    assert_eq!(
        (r.pages, r.ended_at, r.duration_sec, r.is_read, r.ecm),
        (0, None, None, false, false)
    );
    assert_eq!(r.started_at, t(started));
    assert_eq!(r.ani.as_deref(), Some("0227001234"));
    assert_eq!(r.dnis.as_deref(), Some("025570001"));
    assert_eq!((r.created_at, r.updated_at), (t(T0), t(T0)));
    assert_eq!(st.get_by_call_id("call-s02").await.unwrap().unwrap().id, id);
    assert!(st.get_by_call_id("nope").await.unwrap().is_none());

    for (i, pages) in [1, 2, 3].into_iter().enumerate() {
        env.clock.set(t(T0 + 5_000 + i as i64 * 1_000));
        assert!(
            st.update_pages(id, pages, env.clock.now_for_test())
                .await
                .unwrap()
        );
        let r = st.get(id).await.unwrap().unwrap();
        assert_eq!(
            (r.pages, r.fax_status, r.updated_at),
            (
                pages,
                FaxStatus::Receiving,
                t(T0 + 5_000 + i as i64 * 1_000)
            )
        );
    }
    // Pages never go backwards and a repeat is a no-op.
    assert!(!st.update_pages(id, 2, t(T0)).await.unwrap());
    assert!(!st.update_pages(id, 3, t(T0)).await.unwrap());
    assert_eq!(st.get(id).await.unwrap().unwrap().pages, 3);

    // Finalize: answered 2 s after start, ended 62.4 s later -> duration 62 s.
    let mut f = ff(started + 64_400, FaxStatus::Received, 3);
    f.answered_at = Some(t(started + 2_000));
    env.clock.set(t(T0 + 70_000));
    let r = st.finalize(id, &f).await.unwrap();
    assert_eq!(r.fax_status, FaxStatus::Received);
    assert_eq!(r.ended_at, Some(t(started + 64_400)));
    assert_eq!(r.answered_at, Some(t(started + 2_000)));
    assert_eq!(r.duration_sec, Some(62));
    assert_eq!(r.pages, 3);
    assert_eq!(r.hangup_direction, Some(HangupDirection::Local));
    assert_eq!(r.hangup_code.as_deref(), Some("completed"));
    assert_eq!(r.hangup_text.as_deref(), Some("正常完成"));
    assert_eq!((r.t30_code, r.sip_status), (Some(0), None));
    assert_eq!(r.remote_ident.as_deref(), Some("+886 2 2700 1234"));
    assert_eq!(
        (
            r.resolution.as_deref(),
            r.bit_rate,
            r.compression.as_deref(),
            r.ecm
        ),
        (Some("fine"), Some(14400), Some("MMR"), true)
    );
    assert_eq!((r.tiff_size, r.pdf_size), (Some(123_456), Some(78_901)));
    assert_eq!(r.tiff_path.as_deref(), f.tiff_path.as_deref());
    assert_eq!(r.updated_at, t(T0 + 70_000));
    // After finalize the progress counter is frozen.
    assert!(!st.update_pages(id, 9, t(T0)).await.unwrap());

    // Explicit duration override wins; unanswered call -> 0.
    let id2 = st
        .insert_receiving(&nf("call-s02-b", "trunkA", started))
        .await
        .unwrap();
    let mut f2 = ff(started + 9_000, FaxStatus::NoFax, 0);
    f2.hangup_direction = HangupDirection::System;
    f2.hangup_code = "rejected".into();
    f2.sip_status = Some(503);
    f2.t30_code = None;
    f2.tiff_path = None;
    f2.tiff_size = None;
    f2.pdf_path = None;
    f2.pdf_size = None;
    let r2 = st.finalize(id2, &f2).await.unwrap();
    assert_eq!(
        (
            r2.duration_sec,
            r2.sip_status,
            r2.t30_code,
            r2.tiff_path.clone()
        ),
        (Some(0), Some(503), None, None)
    );
    f2.duration_sec = Some(7);
    assert_eq!(st.finalize(id2, &f2).await.unwrap().duration_sec, Some(7));

    // Errors.
    assert!(matches!(
        st.insert_receiving(&nf("call-s02", "trunkA", started))
            .await,
        Err(Error::Duplicate(_))
    ));
    assert!(matches!(
        st.finalize(999_999, &f2).await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        st.finalize(id, &ff(T0, FaxStatus::Receiving, 0)).await,
        Err(Error::Invalid(_))
    ));
    assert!(st.get(999_999).await.unwrap().is_none());
    // Over-long values are rejected before they reach the database.
    let mut long = nf(&"c".repeat(256), "trunkA", started);
    assert!(matches!(
        st.insert_receiving(&long).await,
        Err(Error::TooLong {
            field: "call_id",
            ..
        })
    ));
    long.call_id = "ok".into();
    long.ani = Some("9".repeat(65));
    assert!(matches!(
        st.insert_receiving(&long).await,
        Err(Error::TooLong { field: "ani", .. })
    ));
}

trait ClockExt {
    fn now_for_test(&self) -> chrono::DateTime<chrono::Utc>;
}
impl ClockExt for ManualClock {
    fn now_for_test(&self) -> chrono::DateTime<chrono::Utc> {
        Clock::now(self)
    }
}

// ---------------------------------------------------------------- S03
db_case!(s03_time_precision, s03_time_precision_body);
async fn s03_time_precision_body(env: &Env) {
    let st = &env.store;
    let cases = [
        0i64,                                  // 1970
        T0 + 123,                              // millisecond precision
        T0 + 999,                              // .999 must not round up to the next second
        1_000,                                 // sub-second epoch
        4_102_444_800_000 + 7,                 // 2100
        1_792_540_800_000 - 8 * 3_600_000 + 1, // the instant UTC+8 midnight would be
    ];
    for (i, ms) in cases.into_iter().enumerate() {
        let id = st
            .insert_receiving(&nf(&format!("call-s03-{i}"), "s03", ms))
            .await
            .unwrap();
        assert_eq!(
            st.get(id).await.unwrap().unwrap().started_at,
            t(ms),
            "{}",
            lbl(env, "roundtrip")
        );
        assert_eq!(
            st.get(id)
                .await
                .unwrap()
                .unwrap()
                .started_at
                .timestamp_millis(),
            ms
        );
    }
    // Sub-millisecond input is truncated to the stored precision, identically everywhere.
    let micro = Utc.timestamp_micros(T0 * 1000 + 123_456).single().unwrap();
    let mut n = nf("call-s03-micro", "s03", 0);
    n.started_at = micro;
    n.answered_at = Some(micro);
    let id = st.insert_receiving(&n).await.unwrap();
    let r = st.get(id).await.unwrap().unwrap();
    assert_eq!(r.started_at, t(T0 + 123));
    assert_eq!(r.answered_at, Some(t(T0 + 123)));
    // Range filters see the same instants.
    let l = st
        .list(
            &ListFilter {
                trunk: Some("s03".into()),
                from: Some(t(T0)),
                to: Some(t(T0 + 1_000)),
                ..Default::default()
            },
            PageRequest::default(),
        )
        .await
        .unwrap();
    let mut got: Vec<i64> = l
        .items
        .iter()
        .map(|r| r.started_at.timestamp_millis())
        .collect();
    got.sort();
    assert_eq!(got, vec![T0 + 123, T0 + 123, T0 + 999]); // incl. the truncated micro-second row
}

// ---------------------------------------------------------------- S04
db_case!(s04_text, s04_text_body);
async fn s04_text_body(env: &Env) {
    let st = &env.store;
    let id = st
        .insert_receiving(&nf("call-s04", "s04", T0))
        .await
        .unwrap();
    let samples = [
        "中文備註：客戶要求回電 0912-345-678".to_string(),
        "emoji 😀🎉📠 and 𠮷 outside the BMP".to_string(),
        "'\";-- ' OR 1=1; DROP TABLE faxes; --".to_string(),
        "line1\nline2\r\n\ttab \\ backslash % _ [x]".to_string(),
        "x".repeat(NOTE_MAX_CHARS),
        "中".repeat(NOTE_MAX_CHARS),
        "😀".repeat(NOTE_MAX_CHARS),
        String::new(),
    ];
    let mut expected: Option<chrono::DateTime<Utc>> = None;
    for s in &samples {
        let r = st.set_note(id, s, "分機<1001>", expected).await.unwrap();
        assert_eq!(
            r.note.as_deref(),
            Some(s.as_str()),
            "{}",
            lbl(env, "note roundtrip")
        );
        assert_eq!(r.note_updated_by.as_deref(), Some("分機<1001>"));
        expected = r.note_updated_at;
    }
    assert_eq!(st.get(id).await.unwrap().unwrap().note.as_deref(), Some(""));
    // Over the limit: error, row unchanged.
    let too_long = "😀".repeat(NOTE_MAX_CHARS + 1);
    match st.set_note(id, &too_long, "u", expected).await {
        Err(Error::TooLong {
            field: "note",
            len,
            max,
        }) => assert_eq!((len, max), (NOTE_MAX_CHARS + 1, NOTE_MAX_CHARS)),
        other => panic!("{other:?}"),
    }
    // Hostile values in every text position: stored verbatim, tables intact.
    let evil = "'; DROP TABLE faxes; --";
    let mut n = nf(evil, evil, T0);
    n.ani = Some("'\" OR 1=1".into());
    n.dnis = Some("%_[".into());
    let eid = st.insert_receiving(&n).await.unwrap();
    let r = st.get_by_call_id(evil).await.unwrap().unwrap();
    assert_eq!(
        (r.id, r.trunk.as_str(), r.ani.as_deref(), r.dnis.as_deref()),
        (eid, evil, Some("'\" OR 1=1"), Some("%_["))
    );
    let l = st
        .list(
            &ListFilter {
                trunk: Some(evil.into()),
                ani_prefix: Some("'\" OR".into()),
                ..Default::default()
            },
            PageRequest::default(),
        )
        .await
        .unwrap();
    assert_eq!(l.items.len(), 1);
    // LIKE wildcards in a prefix are literal.
    let l = st
        .list(
            &ListFilter {
                dnis_prefix: Some("%".into()),
                ..Default::default()
            },
            PageRequest::default(),
        )
        .await
        .unwrap();
    assert_eq!(l.items.iter().map(|r| r.id).collect::<Vec<_>>(), vec![eid]);
    let l = st
        .list(
            &ListFilter {
                dnis_prefix: Some("_".into()),
                ..Default::default()
            },
            PageRequest::default(),
        )
        .await
        .unwrap();
    assert!(l.items.is_empty());
    let l = st
        .list(
            &ListFilter {
                dnis_prefix: Some("%_[".into()),
                ..Default::default()
            },
            PageRequest::default(),
        )
        .await
        .unwrap();
    assert_eq!(l.items.len(), 1);
    assert!(count_rows(env, "SELECT COUNT(*) FROM faxes").await >= 2);
}

// ---------------------------------------------------------------- S05
db_case!(s05_read_flag, s05_read_flag_body);
async fn s05_read_flag_body(env: &Env) {
    let st = &env.store;
    assert_eq!(st.count_unread().await.unwrap(), 0);
    let a = st
        .insert_receiving(&nf("call-s05-a", "s05", T0))
        .await
        .unwrap();
    let b = st
        .insert_receiving(&nf("call-s05-b", "s05", T0 + 1))
        .await
        .unwrap();
    let c = st
        .insert_receiving(&nf("call-s05-c", "s05", T0 + 2))
        .await
        .unwrap();
    let unread = |read| ListFilter {
        trunk: Some("s05".into()),
        is_read: Some(read),
        ..Default::default()
    };
    let ids = |p: Page<FaxRecord>| p.items.iter().map(|r| r.id).collect::<Vec<_>>();
    // a record being received is unread too
    assert_eq!(st.count_unread().await.unwrap(), 3);
    // since: started_at >= since (the three start at T0, T0 + 1, T0 + 2 ms)
    assert_eq!(st.count_since(t(T0)).await.unwrap(), 3);
    assert_eq!(st.count_since(t(T0 + 1)).await.unwrap(), 2);
    assert_eq!(st.count_since(t(T0 + 3)).await.unwrap(), 0);
    assert_eq!(
        ids(st
            .list(&unread(false), PageRequest::default())
            .await
            .unwrap()),
        vec![c, b, a]
    );
    assert!(
        st.list(&unread(true), PageRequest::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );

    env.clock.set(t(T0 + 10_000));
    let r = st.set_read(b, true, "alice").await.unwrap();
    assert_eq!(
        (r.is_read, r.read_changed_by.as_deref(), r.read_changed_at),
        (true, Some("alice"), Some(t(T0 + 10_000)))
    );
    assert_eq!(st.count_unread().await.unwrap(), 2);
    assert_eq!(
        ids(st
            .list(&unread(true), PageRequest::default())
            .await
            .unwrap()),
        vec![b]
    );
    assert_eq!(
        ids(st
            .list(&unread(false), PageRequest::default())
            .await
            .unwrap()),
        vec![c, a]
    );

    // Shared flag: another agent switches it back to unread; the last switcher is recorded.
    env.clock.set(t(T0 + 20_000));
    let r = st.set_read(b, false, "bob").await.unwrap();
    assert_eq!(
        (r.is_read, r.read_changed_by.as_deref(), r.read_changed_at),
        (false, Some("bob"), Some(t(T0 + 20_000)))
    );
    assert_eq!(st.count_unread().await.unwrap(), 3);
    assert_eq!(
        ids(st
            .list(&unread(false), PageRequest::default())
            .await
            .unwrap()),
        vec![c, b, a]
    );
    // Setting the same value again still records who/when (unconditional).
    env.clock.set(t(T0 + 30_000));
    let r = st.set_read(b, false, "carol").await.unwrap();
    assert_eq!(
        (r.read_changed_by.as_deref(), r.read_changed_at),
        (Some("carol"), Some(t(T0 + 30_000)))
    );
    assert!(matches!(
        st.set_read(424_242, true, "x").await,
        Err(Error::NotFound(_))
    ));
    assert!(matches!(
        st.set_read(a, true, &"u".repeat(65)).await,
        Err(Error::TooLong { .. })
    ));
    // finalize keeps the read flag and note.
    st.set_read(a, true, "alice").await.unwrap();
    st.set_note(a, "keep me", "alice", None).await.unwrap();
    let r = st
        .finalize(a, &ff(T0 + 50_000, FaxStatus::Received, 1))
        .await
        .unwrap();
    assert_eq!((r.is_read, r.note.as_deref()), (true, Some("keep me")));
    assert_eq!(st.count_unread().await.unwrap(), 2);
}

// ---------------------------------------------------------------- S06
db_case!(s06_note_lock, s06_note_lock_body);
async fn s06_note_lock_body(env: &Env) {
    let st = &env.store;
    let id = st
        .insert_receiving(&nf("call-s06", "s06", T0))
        .await
        .unwrap();
    env.clock.set(t(T0 + 1_000));
    let v1 = st.set_note(id, "first", "alice", None).await.unwrap();
    assert_eq!(
        (v1.note_updated_by.as_deref(), v1.note_updated_at),
        (Some("alice"), Some(t(T0 + 1_000)))
    );
    // Never-edited expectation is now stale.
    assert!(matches!(
        st.set_note(id, "late", "bob", None).await,
        Err(Error::Conflict(_))
    ));
    // Someone else edits; alice's stale token loses.
    env.clock.set(t(T0 + 2_000));
    let v2 = st
        .set_note(id, "second", "bob", v1.note_updated_at)
        .await
        .unwrap();
    assert_eq!(v2.note_updated_at, Some(t(T0 + 2_000)));
    match st
        .set_note(id, "alice again", "alice", v1.note_updated_at)
        .await
    {
        Err(Error::Conflict(m)) => assert!(m.contains("bob"), "{m}"),
        other => panic!("{other:?}"),
    }
    assert_eq!(
        st.get(id).await.unwrap().unwrap().note.as_deref(),
        Some("second")
    );
    // Frozen clock: the version still advances so a stale token cannot match twice.
    let v3 = st
        .set_note(id, "third", "alice", v2.note_updated_at)
        .await
        .unwrap();
    let v3b = st
        .set_note(id, "fourth", "alice", v3.note_updated_at)
        .await
        .unwrap();
    assert!(v3.note_updated_at > v2.note_updated_at);
    assert!(v3b.note_updated_at > v3.note_updated_at);
    assert!(matches!(
        st.set_note(id, "x", "alice", v3.note_updated_at).await,
        Err(Error::Conflict(_))
    ));
    assert!(matches!(
        st.set_note(777_777, "x", "a", None).await,
        Err(Error::NotFound(_))
    ));
    // Sub-millisecond precision in the caller's token is tolerated.
    let tok = v3b.note_updated_at.unwrap() + Duration::microseconds(400);
    st.set_note(id, "fifth", "alice", Some(tok)).await.unwrap();
}

// ---------------------------------------------------------------- S07
db_case!(s07_list_pagination, s07_list_pagination_body);
async fn s07_list_pagination_body(env: &Env) {
    let st = &env.store;
    // 25 rows; every 5th millisecond value is shared by 5 rows (same-ms ties).
    let mut ids = Vec::new();
    for i in 0..25i64 {
        let ms = T0 + 10_000 + (i / 5) * 1_000;
        let mut n = nf(&format!("call-s07-{i}"), "s07", ms);
        n.ani = Some(format!("0{}{:03}", if i % 2 == 0 { "22" } else { "77" }, i));
        n.dnis = Some(format!("025{}", 570 + i % 3));
        ids.push(st.insert_receiving(&n).await.unwrap());
    }
    // Other trunk noise must not leak in.
    st.insert_receiving(&nf("call-s07-other", "s07-other", T0 + 10_000))
        .await
        .unwrap();
    let f = ListFilter {
        trunk: Some("s07".into()),
        ..Default::default()
    };

    let mut seen = Vec::new();
    let mut cursor = None;
    let mut pages = 0;
    loop {
        let p = st
            .list(&f, PageRequest { cursor, limit: 10 })
            .await
            .unwrap();
        pages += 1;
        seen.extend(p.items.iter().map(|r| (r.started_at, r.id)));
        match p.next {
            Some(c) => {
                assert_eq!(p.items.len(), 10);
                // Cursor survives a text round trip.
                cursor = Some(c.to_string().parse::<Cursor>().unwrap());
            }
            None => break,
        }
    }
    assert_eq!(pages, 3);
    assert_eq!(seen.len(), 25, "{}", lbl(env, "no row lost"));
    let uniq: HashSet<_> = seen.iter().map(|x| x.1).collect();
    assert_eq!(uniq.len(), 25, "{}", lbl(env, "no duplicates"));
    let mut sorted = seen.clone();
    sorted.sort_by(|a, b| b.cmp(a));
    assert_eq!(seen, sorted, "order is started_at DESC, id DESC");
    // Exactly 20 rows: the last full page has no next cursor.
    let p = st
        .list(
            &f,
            PageRequest {
                cursor: None,
                limit: 25,
            },
        )
        .await
        .unwrap();
    assert!(p.next.is_none() && p.items.len() == 25);
    let p = st
        .list(
            &f,
            PageRequest {
                cursor: None,
                limit: 24,
            },
        )
        .await
        .unwrap();
    assert!(p.next.is_some() && p.items.len() == 24);

    // Combined filters.
    let mut fc = f.clone();
    fc.ani_prefix = Some("022".into());
    fc.dnis_prefix = Some("02557".into());
    fc.from = Some(t(T0 + 11_000));
    fc.to = Some(t(T0 + 13_000)); // [11 s, 13 s): groups i/5 == 1 and 2
    fc.fax_status = Some(FaxStatus::Receiving);
    fc.is_read = Some(false);
    let got = st.list(&fc, PageRequest::default()).await.unwrap().items;
    let want: Vec<i64> = (5..15)
        .filter(|i| i % 2 == 0)
        .map(|i| ids[i as usize])
        .collect();
    let mut got_ids: Vec<i64> = got.iter().map(|r| r.id).collect();
    got_ids.sort();
    assert_eq!(got_ids, want, "{}", lbl(env, "combined filter"));
    fc.fax_status = Some(FaxStatus::Received);
    assert!(
        st.list(&fc, PageRequest::default())
            .await
            .unwrap()
            .items
            .is_empty()
    );

    // Limit semantics: 0 = default 50, > 200 capped at 200.
    assert_eq!(
        st.list(
            &f,
            PageRequest {
                cursor: None,
                limit: 0
            }
        )
        .await
        .unwrap()
        .items
        .len(),
        25
    );
    assert!("garbage".parse::<Cursor>().is_err());
}

db_case!(s07b_page_cap, s07b_page_cap_body);
async fn s07b_page_cap_body(env: &Env) {
    let st = env.dyn_store();
    let mut tasks = Vec::new();
    for chunk in 0..5i64 {
        let st = st.clone();
        tasks.push(tokio::spawn(async move {
            for i in 0..41i64 {
                let n = chunk * 41 + i;
                st.insert_receiving(&nf(&format!("call-s07b-{n}"), "s07b", T0 + n))
                    .await
                    .unwrap();
            }
        }));
    }
    for t in tasks {
        t.await.unwrap();
    }
    let f = ListFilter {
        trunk: Some("s07b".into()),
        ..Default::default()
    };
    let p = st
        .list(
            &f,
            PageRequest {
                cursor: None,
                limit: 10_000,
            },
        )
        .await
        .unwrap();
    assert_eq!(p.items.len(), 200, "{}", lbl(env, "cap"));
    let next = p.next.expect("205 rows -> second page");
    let p2 = st
        .list(
            &f,
            PageRequest {
                cursor: Some(next),
                limit: 10_000,
            },
        )
        .await
        .unwrap();
    assert_eq!(p2.items.len(), 5);
    assert!(p2.next.is_none());
}

// ---------------------------------------------------------------- S08
db_case!(s08_stale_receiving, s08_stale_receiving_body);
async fn s08_stale_receiving_body(env: &Env) {
    let st = &env.store;
    let a = st
        .insert_receiving(&nf("call-s08-a", "s08", T0))
        .await
        .unwrap(); // 0 pages -> failed
    let mut n = nf("call-s08-b", "s08", T0);
    n.answered_at = Some(t(T0 + 1_000));
    let b = st.insert_receiving(&n).await.unwrap(); // 2 pages -> partial
    st.update_pages(b, 2, t(T0 + 5_000)).await.unwrap();
    let c = st
        .insert_receiving(&nf("call-s08-c", "s08", T0))
        .await
        .unwrap();
    let done = st
        .finalize(c, &ff(T0 + 3_000, FaxStatus::Received, 1))
        .await
        .unwrap();

    env.clock.set(t(T0 + 31_000));
    assert_eq!(st.fix_stale_receiving().await.unwrap(), 2);
    let ra = st.get(a).await.unwrap().unwrap();
    assert_eq!(
        (
            ra.fax_status,
            ra.hangup_code.as_deref(),
            ra.hangup_direction
        ),
        (
            FaxStatus::Failed,
            Some("shutdown"),
            Some(HangupDirection::System)
        )
    );
    assert_eq!(ra.ended_at, Some(t(T0 + 31_000)));
    assert!(ra.hangup_text.as_deref().unwrap().contains("服務中斷"));
    let rb = st.get(b).await.unwrap().unwrap();
    assert_eq!(
        (rb.fax_status, rb.pages, rb.hangup_code.as_deref()),
        (FaxStatus::Partial, 2, Some("shutdown"))
    );
    assert_eq!(rb.duration_sec, Some(30));
    assert_eq!(
        st.get(c).await.unwrap().unwrap(),
        done,
        "finished rows are untouched"
    );
    assert_eq!(st.fix_stale_receiving().await.unwrap(), 0);
}

// ---------------------------------------------------------------- S09
db_case!(s09_concurrency, s09_concurrency_body);
async fn s09_concurrency_body(env: &Env) {
    let st = env.dyn_store();
    let mut tasks = Vec::new();
    for i in 0..10i64 {
        let st = st.clone();
        tasks.push(tokio::spawn(async move {
            let id = st
                .insert_receiving(&nf(&format!("call-s09-{i}"), "s09", T0 + i))
                .await
                .unwrap();
            for p in 1..=3 {
                st.update_pages(id, p, t(T0 + 100 + p as i64))
                    .await
                    .unwrap();
            }
            st.finalize(id, &ff(T0 + 60_000 + i, FaxStatus::Received, 3))
                .await
                .unwrap();
            id
        }));
    }
    let mut ids = Vec::new();
    for t in tasks {
        ids.push(t.await.unwrap());
    }
    for (i, id) in ids.iter().enumerate() {
        let r = st.get(*id).await.unwrap().unwrap();
        assert_eq!(
            (r.call_id.clone(), r.pages, r.fax_status),
            (format!("call-s09-{i}"), 3, FaxStatus::Received),
            "{}",
            lbl(env, "no lost update")
        );
        assert_eq!(r.ended_at, Some(t(T0 + 60_000 + i as i64)));
    }
    let all = st
        .list(
            &ListFilter {
                trunk: Some("s09".into()),
                ..Default::default()
            },
            PageRequest::default(),
        )
        .await
        .unwrap();
    assert_eq!(all.items.len(), 10);

    // Two writers racing on one note with the same token: exactly one wins, other gets Conflict.
    for &id in ids.iter().take(5) {
        let tok = st.get(id).await.unwrap().unwrap().note_updated_at;
        let (s1, s2) = (st.clone(), st.clone());
        let a = tokio::spawn(async move { s1.set_note(id, "from A", "A", tok).await });
        let b = tokio::spawn(async move { s2.set_note(id, "from B", "B", tok).await });
        let (ra, rb) = (a.await.unwrap(), b.await.unwrap());
        let wins = [&ra, &rb].iter().filter(|r| r.is_ok()).count();
        let conflicts = [&ra, &rb]
            .iter()
            .filter(|r| matches!(r, Err(Error::Conflict(_))))
            .count();
        assert_eq!(
            (wins, conflicts),
            (1, 1),
            "{}: {ra:?} / {rb:?}",
            lbl(env, "note race")
        );
        let note = st.get(id).await.unwrap().unwrap().note.unwrap();
        assert!(note == "from A" || note == "from B");
    }
    // Same Call-ID inserted concurrently: one row, the rest Duplicate.
    let mut ts = Vec::new();
    for _ in 0..6 {
        let st = st.clone();
        ts.push(tokio::spawn(async move {
            st.insert_receiving(&nf("call-s09-dup", "s09", T0)).await
        }));
    }
    let mut ok = 0;
    for t in ts {
        match t.await.unwrap() {
            Ok(_) => ok += 1,
            Err(Error::Duplicate(_)) => {}
            Err(e) => panic!("{e:?}"),
        }
    }
    assert_eq!(ok, 1);
}

// ---------------------------------------------------------------- S10
db_case!(s10_settings, s10_settings_body);
async fn s10_settings_body(env: &Env) {
    let st = env.dyn_store();
    let cfg = RetentionSettings {
        policy: RetentionPolicy {
            files_days: 90,
            records_days: 365,
        },
        run_at: "03:17".into(),
    };
    // Fresh database: defaults come from the configuration file.
    assert!(
        purge::load_retention(st.as_ref()).await.unwrap() == cfg,
        "no rows yet -> built-in defaults"
    );
    assert!(purge::seed_retention(st.as_ref(), &cfg).await.unwrap());
    assert_eq!(purge::load_retention(st.as_ref()).await.unwrap(), cfg);
    let row = st
        .config_get(purge::RETENTION_SECTION)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (row.updated_by.as_deref(), row.version),
        (Some("config-file"), 1)
    );

    // An operator changes it online; the DB now wins.
    env.clock.set(t(T0 + 5_000));
    let changed = RetentionSettings {
        policy: RetentionPolicy {
            files_days: 30,
            records_days: 400,
        },
        run_at: "01:05".into(),
    };
    purge::save_retention(st.as_ref(), &changed, "admin")
        .await
        .unwrap();
    let s = st
        .config_get(purge::RETENTION_SECTION)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        (s.version, s.updated_by.as_deref(), s.updated_at),
        (2, Some("admin"), t(T0 + 5_000))
    );
    assert!(s.body.contains("\"files_days\":30"), "{}", s.body);

    // "Restart" with the old config file: nothing is overwritten.
    assert!(!purge::seed_retention(st.as_ref(), &cfg).await.unwrap());
    assert_eq!(purge::load_retention(st.as_ref()).await.unwrap(), changed);
    // A real restart (new connection to the same DB) sees the same values.
    if !env.is_memory() {
        let again = connect_with(&env.url, env.opts()).await.unwrap();
        assert!(!purge::seed_retention(again.as_ref(), &cfg).await.unwrap());
        assert_eq!(
            purge::load_retention(again.as_ref()).await.unwrap(),
            changed
        );
        again.close().await;
    }
    // A new key added later is seeded without touching the others (the `settings` table is for
    // odds and ends now; the retention keys live in the `retention` section).
    assert_eq!(
        st.seed_settings(&[("other.key", "1"), ("new.key", "v")])
            .await
            .unwrap(),
        2
    );
    assert_eq!(
        st.seed_settings(&[("other.key", "2"), ("new.key", "w")])
            .await
            .unwrap(),
        0
    );
    assert_eq!(st.get_setting("new.key").await.unwrap().unwrap().value, "v");
    // Overwrite via set_setting, list, validation.
    st.set_setting("new.key", "中文 value '; --", None)
        .await
        .unwrap();
    assert_eq!(
        st.get_setting("new.key").await.unwrap().unwrap().value,
        "中文 value '; --"
    );
    assert_eq!(st.list_settings().await.unwrap().len(), 2);
    assert!(st.get_setting("missing").await.unwrap().is_none());
    let bad = RetentionSettings {
        policy: RetentionPolicy {
            files_days: 90,
            records_days: 30,
        },
        run_at: "03:17".into(),
    };
    assert!(matches!(
        purge::save_retention(st.as_ref(), &bad, "admin").await,
        Err(Error::Invalid(_))
    ));
    assert!(matches!(
        purge::seed_retention(
            st.as_ref(),
            &RetentionSettings {
                run_at: "25:00".into(),
                ..cfg.clone()
            }
        )
        .await,
        Err(Error::Invalid(_))
    ));
    // Garbage in the table is reported, not ignored.
    let cur = st
        .config_get(purge::RETENTION_SECTION)
        .await
        .unwrap()
        .unwrap();
    st.config_put(
        purge::RETENTION_SECTION,
        r#"{"files_days":"abc"}"#,
        Some(cur.version),
        "test",
        &ConfigAudit {
            action: "update".into(),
            before: None,
            after: None,
        },
    )
    .await
    .unwrap();
    assert!(matches!(
        purge::load_retention(st.as_ref()).await,
        Err(Error::Invalid(_))
    ));
}

// ---------------------------------------------------------------- S11
db_case!(s11_backfill, s11_backfill_body);
async fn s11_backfill_body(env: &Env) {
    let st = &env.store;
    // Store-level part of `db backfill`: idempotent re-import of a finished call.
    let n = nf("call-s11", "s11", T0);
    let f = ff(T0 + 20_000, FaxStatus::Received, 2);
    let (id, inserted) = st.upsert_finished(&n, &f).await.unwrap();
    assert!(inserted);
    let first = st.get(id).await.unwrap().unwrap();
    assert_eq!((first.fax_status, first.pages), (FaxStatus::Received, 2));
    st.set_read(id, true, "alice").await.unwrap();
    let (id2, inserted2) = st.upsert_finished(&n, &f).await.unwrap();
    assert_eq!((id2, inserted2), (id, false));
    assert!(
        st.get(id).await.unwrap().unwrap().is_read,
        "replay keeps agent data"
    );
    // A call that only has a `receiving` row gets completed by the replay.
    let rid = st
        .insert_receiving(&nf("call-s11-r", "s11", T0))
        .await
        .unwrap();
    let (rid2, ins) = st
        .upsert_finished(
            &nf("call-s11-r", "s11", T0),
            &ff(T0 + 9_000, FaxStatus::Partial, 1),
        )
        .await
        .unwrap();
    assert_eq!((rid2, ins), (rid, false));
    assert_eq!(
        st.get(rid).await.unwrap().unwrap().fax_status,
        FaxStatus::Partial
    );
}

// ---------------------------------------------------------------- deliveries (table created now, logic in M3c)
db_case!(s12_deliveries_table, s12_deliveries_table_body);
async fn s12_deliveries_table_body(env: &Env) {
    let st = &env.store;
    let id = st
        .insert_receiving(&nf("call-s12", "s12", T0))
        .await
        .unwrap();
    let d = NewDelivery {
        fax_id: id,
        target: "/srv/fax-inbox".into(),
        kind: "folder".into(),
        attempt: 1,
        status: DeliveryStatus::Pending,
        next_try_at: Some(t(T0 + 60_000)),
    };
    let did = st.add_delivery(&d).await.unwrap();
    let list = st.list_deliveries(id).await.unwrap();
    assert_eq!(list.len(), 1);
    assert_eq!(
        (
            list[0].id,
            list[0].fax_id,
            list[0].status,
            list[0].next_try_at,
            list[0].created_at
        ),
        (
            did,
            id,
            DeliveryStatus::Pending,
            Some(t(T0 + 60_000)),
            t(T0)
        )
    );
    assert!(st.list_deliveries(id + 1000).await.unwrap().is_empty());
}

// ---------------------------------------------------------------- delivery queue (M3c)
db_case!(s13_delivery_queue, s13_delivery_queue_body);
async fn s13_delivery_queue_body(env: &Env) {
    let st = &env.store;
    let id = st
        .insert_receiving(&nf("call-s13", "s13", T0))
        .await
        .unwrap();
    let mk = |target: &str, status, next: Option<i64>| NewDelivery {
        fax_id: id,
        target: target.into(),
        kind: "folder".into(),
        attempt: 1,
        status,
        next_try_at: next.map(t),
    };
    let a = st
        .add_delivery(&mk("a", DeliveryStatus::Pending, Some(T0 + 2_000)))
        .await
        .unwrap();
    let b = st
        .add_delivery(&mk("b", DeliveryStatus::Pending, Some(T0 + 1_000)))
        .await
        .unwrap();
    let c = st
        .add_delivery(&mk("c", DeliveryStatus::Pending, None))
        .await
        .unwrap();
    let _done = st
        .add_delivery(&mk("d", DeliveryStatus::Done, Some(T0 - 1_000)))
        .await
        .unwrap();
    assert_eq!(
        st.count_deliveries(DeliveryStatus::Pending).await.unwrap(),
        3
    );
    assert_eq!(st.count_deliveries(DeliveryStatus::Done).await.unwrap(), 1);
    assert_eq!(
        st.count_deliveries(DeliveryStatus::Failed).await.unwrap(),
        0
    );
    // Nothing due before the first next_try_at except the NULL one.
    let due = st.due_deliveries(t(T0 + 500), 10).await.unwrap();
    assert_eq!(due.iter().map(|d| d.id).collect::<Vec<_>>(), vec![c]);
    // Ordered by next_try_at (then id); exactly-equal times are due; only pending rows.
    let due = st.due_deliveries(t(T0 + 2_000), 10).await.unwrap();
    assert_eq!(due.iter().map(|d| d.id).collect::<Vec<_>>(), vec![c, b, a]);
    // The limit is honoured.
    assert_eq!(st.due_deliveries(t(T0 + 2_000), 2).await.unwrap().len(), 2);

    // update_delivery changes status, error, next_try_at and updated_at; created_at is kept.
    env.clock.advance(Duration::seconds(30));
    let err = "連線失敗 '\";--";
    assert!(
        st.update_delivery(b, DeliveryStatus::Failed, Some(err), None)
            .await
            .unwrap()
    );
    let row = st
        .list_deliveries(id)
        .await
        .unwrap()
        .into_iter()
        .find(|d| d.id == b)
        .unwrap();
    assert_eq!(row.status, DeliveryStatus::Failed);
    assert_eq!(
        st.count_deliveries(DeliveryStatus::Failed).await.unwrap(),
        1
    );
    assert_eq!(
        st.count_deliveries(DeliveryStatus::Pending).await.unwrap(),
        2
    );
    assert_eq!(
        st.count_deliveries(DeliveryStatus::GivingUp).await.unwrap(),
        0
    );
    assert_eq!(row.last_error.as_deref(), Some(err));
    assert_eq!(row.next_try_at, None);
    assert_eq!(row.created_at, t(T0));
    assert_eq!(row.updated_at, t(T0 + 30_000));
    // A failed row is no longer due.
    let due = st.due_deliveries(t(T0 + 9_000), 10).await.unwrap();
    assert_eq!(due.iter().map(|d| d.id).collect::<Vec<_>>(), vec![c, a]);
    // Unknown id and over-long error text.
    assert!(
        !st.update_delivery(a + 1000, DeliveryStatus::Done, None, None)
            .await
            .unwrap()
    );
    assert!(matches!(
        st.update_delivery(a, DeliveryStatus::Failed, Some(&"x".repeat(4001)), None)
            .await,
        Err(Error::TooLong { .. })
    ));
}

// S11 (connection loss). Needs a restartable server container, so it takes the write lock: no other
// case may run against the same server while it is stopped.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn s11b_outage_and_reconnect() {
    let Ok(container) = std::env::var("SIP_FAXD_TEST_DB_CONTAINER") else {
        eprintln!(
            "S11 outage part skipped (SIP_FAXD_TEST_DB_CONTAINER not set; not applicable to SQLite)"
        );
        return;
    };
    let _guard = DB_LOCK.write().await;
    for target in targets() {
        if !matches!(target, Target::Server(_)) {
            continue;
        }
        let env = Env::create(target).await;
        let st = &env.store;
        let (id, _) = st
            .upsert_finished(
                &nf("call-s11", "s11", T0),
                &ff(T0 + 20_000, FaxStatus::Received, 2),
            )
            .await
            .unwrap();
        outage_body(&env, &container, id).await;
        env.cleanup().await;
    }
}

async fn outage_body(env: &Env, container: &str, id: i64) {
    let st = &env.store;
    let docker = |args: &[&str]| {
        let out = std::process::Command::new("docker")
            .args(args)
            .output()
            .expect("docker");
        assert!(
            out.status.success(),
            "docker {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    };
    docker(&["stop", "-t", "5", container]);
    // While down: writes fail with a transient error, quickly enough and without panicking.
    let t0 = std::time::Instant::now();
    let err = st
        .insert_receiving(&nf("call-s11-down", "s11", T0))
        .await
        .expect_err("db is down");
    assert!(err.is_transient(), "expected transient error, got {err:?}");
    assert!(
        t0.elapsed() < std::time::Duration::from_secs(40),
        "failure took {:?}",
        t0.elapsed()
    );
    docker(&["start", container]);
    // The pool must recover by itself; wait up to 120 s for the server to accept connections.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(120);
    let id3 = loop {
        match st.insert_receiving(&nf("call-s11-up", "s11", T0)).await {
            Ok(id) => break id,
            Err(e) if e.is_transient() && std::time::Instant::now() < deadline => {
                tokio::time::sleep(std::time::Duration::from_secs(2)).await
            }
            Err(e) => panic!("{}: no recovery: {e:?}", lbl(env, "reconnect")),
        }
    };
    // Data written before the outage is intact, the failed write left nothing behind, the new write works.
    assert_eq!(st.get(id).await.unwrap().unwrap().call_id, "call-s11");
    assert!(st.get_by_call_id("call-s11-down").await.unwrap().is_none());
    assert_eq!(st.get(id3).await.unwrap().unwrap().call_id, "call-s11-up");
    st.finalize(id3, &ff(T0 + 1, FaxStatus::Failed, 0))
        .await
        .unwrap();
}

db_case!(s07c_list_search, s07c_list_search_body);
async fn s07c_list_search_body(env: &Env) {
    let st = env.dyn_store();
    let tr = "s07c";
    let mut a = nf("call-s07c-a", tr, T0 + 1);
    a.ani = Some("0912345678".into());
    a.dnis = Some("025570001".into());
    let a = st.insert_receiving(&a).await.unwrap();
    let mut b = nf("call-s07c-b", tr, T0 + 2);
    b.ani = None;
    b.dnis = Some("0800%_[9".into());
    let b = st.insert_receiving(&b).await.unwrap();
    let mut fb = ff(T0 + 3, FaxStatus::Received, 1);
    fb.remote_ident = Some("ACME Sales".into());
    st.finalize(b, &fb).await.unwrap();
    let mut c = nf("call-s07c-c", tr, T0 + 4);
    c.ani = None;
    c.dnis = None;
    let c = st.insert_receiving(&c).await.unwrap();
    st.set_note(c, "請回電 Wang 先生", "u", None).await.unwrap();
    let find = |q: &str| {
        let st = st.clone();
        let q = q.to_string();
        async move {
            let f = ListFilter {
                trunk: Some(tr.into()),
                q: Some(q),
                ..Default::default()
            };
            let mut ids: Vec<i64> = st
                .list(&f, PageRequest::default())
                .await
                .unwrap()
                .items
                .iter()
                .map(|r| r.id)
                .collect();
            ids.sort();
            ids
        }
    };
    // anywhere in the ANI / DNIS, not only the start
    assert_eq!(find("2345").await, vec![a], "{}", lbl(env, "ani"));
    assert_eq!(find("5570").await, vec![a], "{}", lbl(env, "dnis"));
    // the remote ident and the note, ignoring case
    assert_eq!(find("acme sales").await, vec![b], "{}", lbl(env, "ident"));
    assert_eq!(find("WANG").await, vec![c], "{}", lbl(env, "note"));
    assert_eq!(find("回電").await, vec![c], "{}", lbl(env, "note cjk"));
    // wildcards are literal
    assert_eq!(find("%_[").await, vec![b], "{}", lbl(env, "literal"));
    assert_eq!(find("_").await, vec![b], "{}", lbl(env, "underscore"));
    assert_eq!(find("%").await, vec![b], "{}", lbl(env, "percent"));
    assert!(find("!").await.is_empty(), "{}", lbl(env, "escape char"));
    assert!(find("nothing-like-this").await.is_empty());
    // with the other filters (AND)
    let f = ListFilter {
        trunk: Some(tr.into()),
        q: Some("0".into()),
        fax_status: Some(FaxStatus::Received),
        ..Default::default()
    };
    let got: Vec<i64> = st
        .list(&f, PageRequest::default())
        .await
        .unwrap()
        .items
        .iter()
        .map(|r| r.id)
        .collect();
    assert_eq!(got, vec![b], "{}", lbl(env, "and"));
}
