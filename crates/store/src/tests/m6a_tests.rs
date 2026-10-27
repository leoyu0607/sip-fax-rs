//! M6a (ADR-070) store cases, run on every backend by the db-matrix: K12 (API keys in the
//! store), C01-C03 and C05 (the change feed), V01-V03 (the view `fax_records`, its columns as
//! `docs/integration.md` lists them, and the read-only account of the document).

use super::*;
use crate::admin::{AdminStore, NewApiKey};
use crate::db::{Kind, Val};
use crate::migrate::{Migration, apply, migrations};
use chrono::Duration;
use std::collections::{BTreeMap, HashMap, HashSet};

const INTEGRATION_MD: &str = include_str!("../../../../docs/integration.md");

fn hash(n: u32) -> String {
    format!("{n:064x}")
}

fn key(name: &str, n: u32) -> NewApiKey {
    NewApiKey {
        name: name.into(),
        token_hash: hash(n),
        prefix: format!("sfx_{n:08x}"),
        scopes: "fax:read".into(),
        created_by: "tester".into(),
        expires_at: None,
    }
}

// ---------------------------------------------------------------- K12 (store level)
db_case!(k12_api_keys_in_the_store, k12_api_keys_body);
async fn k12_api_keys_body(env: &Env) {
    let st = &env.store;
    // create: only the hash and the prefix are stored; the times have millisecond precision
    let exp = t(T0 + 86_400_123);
    let mut new = key("erp", 1);
    new.scopes = "fax:read,fax:write".into();
    new.expires_at = Some(exp);
    env.clock.set(t(T0 + 5));
    let a = st.api_key_create(&new).await.unwrap();
    assert_eq!(a.name, "erp");
    assert_eq!(a.token_hash, hash(1));
    assert_eq!(a.prefix, "sfx_00000001");
    assert_eq!(a.scopes, "fax:read,fax:write");
    assert_eq!(a.created_by, "tester");
    assert_eq!(a.created_at, t(T0 + 5));
    assert_eq!(a.expires_at, Some(exp), "[{}] expiry to the ms", env.label);
    assert!(a.revoked_at.is_none() && a.last_used_at.is_none() && a.last_used_ip.is_none());
    let got = st.api_key_get_by_hash(&hash(1)).await.unwrap().unwrap();
    assert_eq!(got, a);
    assert!(st.api_key_get_by_hash(&hash(2)).await.unwrap().is_none());
    // the hash lookup is exact (case sensitive hex): the upper-case spelling is another key
    assert!(
        st.api_key_get_by_hash(&hash(1).replace('0', "O"))
            .await
            .unwrap()
            .is_none()
    );
    // a second key without expiry; the list is ordered by name
    let b = st.api_key_create(&key("billing", 3)).await.unwrap();
    let names: Vec<_> = st
        .api_key_list()
        .await
        .unwrap()
        .into_iter()
        .map(|k| k.name)
        .collect();
    assert_eq!(names, ["billing", "erp"]);
    assert!(b.id != a.id);

    // names and hashes are unique
    assert!(matches!(
        st.api_key_create(&key("erp", 9)).await,
        Err(Error::Duplicate(_))
    ));
    assert!(matches!(
        st.api_key_create(&key("other", 1)).await,
        Err(Error::Duplicate(_))
    ));
    assert_eq!(
        st.api_key_list().await.unwrap().len(),
        2,
        "nothing was added"
    );

    // last use: time (ms) and address; an address that is too long is cut, not refused
    let used = t(T0 + 90_001);
    st.api_key_touch(a.id, used, Some("203.0.113.9"))
        .await
        .unwrap();
    let a2 = st.api_key_get_by_hash(&hash(1)).await.unwrap().unwrap();
    assert_eq!(a2.last_used_at, Some(used));
    assert_eq!(a2.last_used_ip.as_deref(), Some("203.0.113.9"));
    st.api_key_touch(a.id, t(T0 + 150_002), Some(&"x".repeat(100)))
        .await
        .unwrap();
    let a3 = st.api_key_get_by_hash(&hash(1)).await.unwrap().unwrap();
    assert_eq!(a3.last_used_at, Some(t(T0 + 150_002)));
    assert_eq!(a3.last_used_ip.as_deref().map(str::len), Some(64));
    st.api_key_touch(a.id, t(T0 + 200_000), None).await.unwrap();
    assert_eq!(
        st.api_key_get_by_hash(&hash(1))
            .await
            .unwrap()
            .unwrap()
            .last_used_ip,
        None
    );

    // revoke: once; the row stays (with who and when); the name stays taken
    let when = t(T0 + 300_003);
    assert!(st.api_key_revoke(a.id, "root", when).await.unwrap());
    assert!(
        !st.api_key_revoke(a.id, "other", t(T0 + 400_000))
            .await
            .unwrap()
    );
    assert!(!st.api_key_revoke(99_999, "root", when).await.unwrap());
    let r = st.api_key_get_by_hash(&hash(1)).await.unwrap().unwrap();
    assert_eq!(
        (r.revoked_at, r.revoked_by.as_deref()),
        (Some(when), Some("root"))
    );
    assert!(matches!(
        st.api_key_create(&key("erp", 7)).await,
        Err(Error::Duplicate(_))
    ));
    // the other key is untouched
    let other = st.api_key_get_by_hash(&hash(3)).await.unwrap().unwrap();
    assert_eq!(other, b);
    // length limits
    assert!(matches!(
        st.api_key_create(&key(&"n".repeat(49), 11)).await,
        Err(Error::TooLong { .. })
    ));
}

// ---------------------------------------------------------------- the change feed
/// Every record, in the order of the feed, read in pages of `limit` from `from`; returns them and
/// the position after the last page.
async fn read_feed(
    env: &Env,
    mut from: ChangesFrom,
    until: chrono::DateTime<chrono::Utc>,
    limit: u32,
) -> (Vec<FaxRecord>, ChangesFrom) {
    let mut all = Vec::new();
    loop {
        let page = env.store.changes(from, until, limit).await.unwrap();
        if page.is_empty() {
            return (all, from);
        }
        assert!(page.len() <= limit as usize);
        from = ChangesFrom::After(ChangeCursor::of(page.last().unwrap()));
        all.extend(page);
    }
}

/// 300 faxes, ten at a time with the same `updated_at` (so that ties cross page borders), in
/// every state a fax has: receiving, received, partial, failed, no_fax.
async fn feed_faxes(env: &Env) -> Vec<i64> {
    let st = &env.store;
    let mut ids = Vec::new();
    for i in 0..300i64 {
        // ten faxes share a millisecond; the groups are 1 ms apart (the odd ms of a time stamp
        // must survive on every database)
        env.clock.set(t(T0 + 1_000 + (i / 10) * 7 + 123));
        let id = st
            .insert_receiving(&nf(&format!("feed-{i}"), "gw", T0 + i))
            .await
            .unwrap();
        let status = match i % 5 {
            0 => None, // stays receiving
            1 => Some(FaxStatus::Received),
            2 => Some(FaxStatus::Partial),
            3 => Some(FaxStatus::Failed),
            _ => Some(FaxStatus::NoFax),
        };
        if let Some(s) = status {
            let pages = if s == FaxStatus::Failed || s == FaxStatus::NoFax {
                0
            } else {
                2
            };
            st.finalize(id, &ff(T0 + 1_000 + (i / 10) * 7 + 123, s, pages))
                .await
                .unwrap();
        }
        ids.push(id);
    }
    ids
}

fn feed_order(rs: &[FaxRecord]) -> Vec<(i64, i64)> {
    rs.iter()
        .map(|r| (r.updated_at.timestamp_millis(), r.id))
        .collect()
}

db_case!(c01_every_fax_once_in_order, c01_body);
async fn c01_body(env: &Env) {
    let ids = feed_faxes(env).await;
    let until = t(T0 + 3_600_000);
    for limit in [1u32, 7, 200] {
        let (all, end) = read_feed(env, ChangesFrom::Start, until, limit).await;
        assert_eq!(all.len(), 300, "[{}] limit {limit}", env.label);
        let got: HashSet<i64> = all.iter().map(|r| r.id).collect();
        assert_eq!(got, ids.iter().copied().collect(), "each fax once");
        let order = feed_order(&all);
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(order, sorted, "by (updated_at, id)");
        // the millisecond of the first group survived the database
        assert_eq!(order[0].0 % 1000, (T0 + 1_000 + 123) % 1000);
        // after the last page: the same position, nothing more
        let last = ChangeCursor::of(all.last().unwrap());
        assert_eq!(end, ChangesFrom::After(last));
        assert!(
            env.store
                .changes(end, until, limit)
                .await
                .unwrap()
                .is_empty()
        );
    }
    // `until` keeps back what is newer: the first group only
    let cut = t(T0 + 1_000 + 123);
    let (some, _) = read_feed(env, ChangesFrom::Start, cut, 50).await;
    assert_eq!(some.len(), 10);
    assert!(some.iter().all(|r| r.updated_at <= cut));
    // `since` starts at a time (inclusive)
    let (from_second, _) =
        read_feed(env, ChangesFrom::Since(t(T0 + 1_000 + 7 + 123)), until, 200).await;
    assert_eq!(from_second.len(), 290);
    // limit is clamped to 1..=200 at the store
    assert_eq!(
        env.store
            .changes(ChangesFrom::Start, until, 5_000)
            .await
            .unwrap()
            .len(),
        200
    );
    assert_eq!(
        env.store
            .changes(ChangesFrom::Start, until, 0)
            .await
            .unwrap()
            .len(),
        1
    );
}

db_case!(c02_changes_during_the_walk_come_again, c02_body);
async fn c02_body(env: &Env) {
    let st = &env.store;
    let ids = feed_faxes(env).await;
    let until = |c: &Env| c.clock.now() + Duration::seconds(10);
    // walk the first 100 changes
    let mut seen: HashMap<i64, i64> = HashMap::new(); // id -> updated_ms last seen
    let mut from = ChangesFrom::Start;
    for _ in 0..4 {
        let page = st.changes(from, until(env), 25).await.unwrap();
        for r in &page {
            seen.insert(r.id, r.updated_at.timestamp_millis());
        }
        from = ChangesFrom::After(ChangeCursor::of(page.last().unwrap()));
    }
    assert_eq!(seen.len(), 100);
    // in the middle of the walk: a fax already seen is marked read, one not yet seen gets a
    // note, a receiving one finishes, and a new one arrives
    env.clock.advance(Duration::seconds(30));
    let (seen_one, unseen_one) = (ids[3], ids[250]);
    st.set_read(seen_one, true, "amy").await.unwrap();
    st.set_note(unseen_one, "later", "ben", None).await.unwrap();
    let receiving = ids[200]; // i % 5 == 0: still receiving
    assert_eq!(
        st.get(receiving).await.unwrap().unwrap().fax_status,
        FaxStatus::Receiving
    );
    st.finalize(receiving, &ff(T0 + 5, FaxStatus::Received, 1))
        .await
        .unwrap();
    let fresh = st
        .insert_receiving(&nf("feed-new", "gw", T0 + 999))
        .await
        .unwrap();
    // finish the walk
    loop {
        let page = st.changes(from, until(env), 25).await.unwrap();
        if page.is_empty() {
            break;
        }
        for r in &page {
            seen.insert(r.id, r.updated_at.timestamp_millis());
        }
        from = ChangesFrom::After(ChangeCursor::of(page.last().unwrap()));
    }
    // nothing was missed: for every fax the feed has delivered its current state
    for id in ids.iter().chain([&fresh]) {
        let now = st.get(*id).await.unwrap().unwrap();
        assert_eq!(
            seen.get(id),
            Some(&now.updated_at.timestamp_millis()),
            "[{}] fax {id} is up to date in the feed",
            env.label
        );
    }
    // the four changes come exactly once more, after the position the walk had reached
    let (again, _) = read_feed(
        env,
        ChangesFrom::After(ChangeCursor {
            updated_ms: (T0 + 1_000 + 29 * 7 + 123) + 1,
            id: 0,
        }),
        until(env),
        200,
    )
    .await;
    let got: Vec<i64> = again.iter().map(|r| r.id).collect();
    assert_eq!(got.len(), 4, "{got:?}");
    assert_eq!(
        got.iter().copied().collect::<HashSet<_>>(),
        [seen_one, unseen_one, receiving, fresh]
            .into_iter()
            .collect()
    );
    // and a fax that changed twice shows once, in its latest state
    env.clock.advance(Duration::seconds(5));
    st.set_read(seen_one, false, "amy").await.unwrap();
    let (c, _) = read_feed(
        env,
        ChangesFrom::After(ChangeCursor::of(again.last().unwrap())),
        until(env),
        200,
    )
    .await;
    assert_eq!(c.len(), 1);
    assert_eq!((c[0].id, c[0].is_read), (seen_one, false));
}

db_case!(c03_equal_times_are_split_by_id, c03_body);
async fn c03_body(env: &Env) {
    let st = &env.store;
    // 25 faxes with one and the same updated_at, then 5 a millisecond later
    env.clock.set(t(T0 + 777));
    let mut ids = Vec::new();
    for i in 0..25 {
        let id = st
            .insert_receiving(&nf(&format!("tie-{i}"), "gw", T0 + i))
            .await
            .unwrap();
        ids.push(id);
    }
    env.clock.set(t(T0 + 778));
    for i in 25..30 {
        ids.push(
            st.insert_receiving(&nf(&format!("tie-{i}"), "gw", T0 + i))
                .await
                .unwrap(),
        );
    }
    let until = t(T0 + 10_000);
    for limit in [1u32, 4, 6, 25, 26] {
        let (all, _) = read_feed(env, ChangesFrom::Start, until, limit).await;
        let got: Vec<i64> = all.iter().map(|r| r.id).collect();
        assert_eq!(
            got.len(),
            30,
            "[{}] limit {limit}: no fax lost or repeated at a page border",
            env.label
        );
        let mut want = ids.clone();
        want.sort();
        // equal times by id, then the later group
        assert_eq!(got, want, "limit {limit}");
    }
    // a cursor in the middle of the tie: the rest of it, no more
    let mid = ChangeCursor {
        updated_ms: T0 + 777,
        id: ids[9],
    };
    let rest = st
        .changes(ChangesFrom::After(mid), until, 200)
        .await
        .unwrap();
    assert_eq!(rest.len(), 20);
    assert_eq!(rest[0].id, ids[10]);
}

// ---------------------------------------------------------------- the view fax_records
/// A row of the table of `docs/integration.md` section 5: the column and its concept type.
fn doc_view_columns() -> Vec<(String, String)> {
    let start = INTEGRATION_MD
        .find("| 欄位 | 型別（概念） | 說明 |")
        .expect("the column table of section 5");
    INTEGRATION_MD[start..]
        .lines()
        .skip(2)
        .take_while(|l| l.starts_with('|'))
        .map(|l| {
            let cells: Vec<&str> = l.split('|').map(str::trim).collect();
            let name = cells[1].trim_matches('`').to_string();
            let ty = cells[2].split('，').next().unwrap().to_string();
            (name, ty)
        })
        .collect()
}

/// The columns of the view as the database reports them: `(name, type)` in column order.
async fn view_columns(env: &Env) -> Vec<(String, String)> {
    let sql = match env.store.backend() {
        BackendKind::Sqlite => {
            "SELECT name, type FROM pragma_table_info('fax_records') ORDER BY cid"
        }
        BackendKind::Postgres => {
            "SELECT column_name::text, data_type::text FROM information_schema.columns WHERE table_schema = current_schema() AND table_name = 'fax_records' ORDER BY ordinal_position"
        }
        BackendKind::Mysql => {
            "SELECT CAST(column_name AS CHAR), CAST(data_type AS CHAR) FROM information_schema.columns WHERE table_schema = DATABASE() AND table_name = 'fax_records' ORDER BY ordinal_position"
        }
        BackendKind::Mssql => {
            "SELECT CAST(column_name AS NVARCHAR(128)), CAST(data_type AS NVARCHAR(128)) FROM information_schema.columns WHERE table_name = 'fax_records' ORDER BY ordinal_position"
        }
    };
    env.store
        .raw()
        .query(sql, &[], &[Kind::Str, Kind::Str])
        .await
        .unwrap()
        .into_iter()
        .map(|r| match (&r[0], &r[1]) {
            (Val::S(Some(a)), Val::S(b)) => (a.clone(), b.clone().unwrap_or_default()),
            other => panic!("{other:?}"),
        })
        .collect()
}

fn type_matches(kind: BackendKind, concept: &str, db: &str) -> bool {
    let db = db.to_ascii_lowercase();
    // SQLite keeps the declared types of the table; an expression has none
    if kind == BackendKind::Sqlite {
        return match concept {
            "整數" => db.is_empty() || db.contains("int"),
            "字串" => db.is_empty() || db.contains("text") || db.contains("char"),
            "時間" => db.is_empty() || db.contains("text") || db.contains("time"),
            "布林" => db.is_empty() || db.contains("int") || db.contains("bool"),
            c => panic!("concept type {c}"),
        };
    }
    let (int, text, time, boolean): (&[&str], &[&str], &[&str], &[&str]) = match kind {
        BackendKind::Postgres => (
            &["integer", "bigint", "smallint"],
            &["text", "character varying", "character"],
            &["timestamp with time zone"],
            &["boolean"],
        ),
        BackendKind::Mysql => (
            &["int", "bigint", "smallint", "mediumint"],
            &["varchar", "text", "char", "longtext", "mediumtext"],
            &["datetime", "timestamp"],
            &["tinyint", "int", "bigint", "bit"],
        ),
        _ => (
            &["int", "bigint", "smallint"],
            &["nvarchar", "varchar", "nchar", "char", "ntext"],
            &["datetime2"],
            &["bit"],
        ),
    };
    let set = match concept {
        "整數" => int,
        "字串" => text,
        "時間" => time,
        "布林" => boolean,
        c => panic!("concept type {c}"),
    };
    set.contains(&db.as_str())
}

async fn check_view_against_the_document(env: &Env, what: &str) {
    let doc = doc_view_columns();
    assert_eq!(doc.len(), 38, "the table of the document: {doc:?}");
    let got = view_columns(env).await;
    let kind = env.store.backend();
    assert_eq!(
        got.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(),
        doc.iter().map(|c| c.0.as_str()).collect::<Vec<_>>(),
        "[{}] {what}: names and order",
        env.label
    );
    for ((name, concept), (_, db)) in doc.iter().zip(&got) {
        assert!(
            type_matches(kind, concept, db),
            "[{}] {what}: column {name} is documented as {concept}, the database says {db:?}",
            env.label
        );
    }
}

/// The database as an older sip-faxd left it: migrations up to `version`.
async fn at_version(env: &Env, version: i32) {
    for stmt in [
        "DROP VIEW fax_records",
        "DROP TABLE api_keys",
        "DROP TABLE sessions",
        "DROP TABLE users",
        "DROP TABLE config_audit",
        "DROP TABLE config_sections",
        "DROP TABLE delivery_attempts",
        "DROP TABLE settings",
        "DROP TABLE faxes",
        "DROP TABLE schema_migrations",
    ] {
        env.store.raw().execute(stmt, &[]).await.unwrap();
    }
    let some: Vec<Migration> = migrations(env.store.backend())
        .iter()
        .filter(|m| m.version <= version)
        .map(|m| Migration {
            version: m.version,
            name: m.name,
            sql: m.sql,
        })
        .collect();
    let rep = apply(env.store.raw(), env.clock.as_ref(), &some)
        .await
        .unwrap();
    assert_eq!(rep.to, version);
}

db_case!(v01_the_view_is_what_the_document_says, v01_body);
async fn v01_body(env: &Env) {
    check_view_against_the_document(env, "fresh database").await;
    if env.is_memory() {
        return; // `at_version` reconnects: a new in-memory database every time
    }
    // an upgrade from 0003 with a fax in it: the view appears, the fax shows in it
    at_version(env, 3).await;
    let id = env
        .store
        .insert_receiving(&nf("before-0004", "gw", T0))
        .await
        .unwrap();
    assert!(
        env.store
            .raw()
            .query("SELECT COUNT(*) FROM fax_records", &[], &[Kind::Int])
            .await
            .is_err(),
        "no view before 0004"
    );
    let rep = env.store.migrate().await.unwrap();
    assert_eq!((rep.from, rep.to, rep.applied), (3, 4, vec![4]));
    check_view_against_the_document(env, "upgrade from 0003").await;
    let rows = env
        .store
        .raw()
        .query(
            "SELECT id, call_id FROM fax_records",
            &[],
            &[Kind::Int, Kind::Str],
        )
        .await
        .unwrap();
    assert_eq!(
        rows,
        vec![vec![Val::I(Some(id)), Val::S(Some("before-0004".into()))]]
    );
    // running 0004 again (what a MySQL rerun does after a failed run) changes nothing
    env.store
        .raw()
        .execute("DELETE FROM schema_migrations WHERE version = 4", &[])
        .await
        .unwrap();
    if env.store.backend() == BackendKind::Mysql {
        let rep = env.store.migrate().await.unwrap();
        assert_eq!(rep.applied, vec![4]);
        check_view_against_the_document(env, "rerun of 0004").await;
    }
}

/// `SELECT <list of columns>` of the view or of `faxes`, as values.
const SAME_COLS: &[(&str, Kind)] = &[
    ("id", Kind::Int),
    ("call_id", Kind::Str),
    ("trunk", Kind::Str),
    ("ani", Kind::Str),
    ("ani_source", Kind::Str),
    ("dnis", Kind::Str),
    ("dnis_source", Kind::Str),
    ("remote_ident", Kind::Str),
    ("started_at", Kind::Time),
    ("answered_at", Kind::Time),
    ("ended_at", Kind::Time),
    ("duration_sec", Kind::Int),
    ("hangup_direction", Kind::Str),
    ("hangup_code", Kind::Str),
    ("hangup_text", Kind::Str),
    ("sip_status", Kind::Int),
    ("t30_code", Kind::Int),
    ("fax_status", Kind::Str),
    ("pages", Kind::Int),
    ("resolution", Kind::Str),
    ("bit_rate", Kind::Int),
    ("compression", Kind::Str),
    ("ecm", Kind::Bool),
    ("modem", Kind::Str),
    ("media", Kind::Str),
    ("tiff_size", Kind::Int),
    ("pdf_size", Kind::Int),
    ("files_purged_at", Kind::Time),
    ("is_read", Kind::Bool),
    ("read_changed_by", Kind::Str),
    ("read_changed_at", Kind::Time),
    ("note", Kind::Str),
    ("note_updated_by", Kind::Str),
    ("note_updated_at", Kind::Time),
    ("created_at", Kind::Time),
    ("updated_at", Kind::Time),
];

fn as_int(kind: BackendKind, col: &str) -> String {
    match kind {
        BackendKind::Mysql => format!("CAST({col} AS SIGNED)"),
        _ => format!("CAST({col} AS INTEGER)"),
    }
}

db_case!(v02_the_view_is_the_fax_without_paths, v02_body);
async fn v02_body(env: &Env) {
    let st = &env.store;
    env.clock.set(t(T0 + 10_000));
    // received (T.38), G.711, receiving, failed without files, one whose files were purged
    let mut a_fin = ff(T0 + 5_000, FaxStatus::Received, 3);
    a_fin.modem = Some("v17".into());
    a_fin.media = Some("t38".into());
    let a = st.insert_receiving(&nf("v-t38", "gw1", T0)).await.unwrap();
    st.finalize(a, &a_fin).await.unwrap();
    let mut g_fin = ff(T0 + 6_000, FaxStatus::Received, 1);
    g_fin.media = Some("g711".into());
    g_fin.modem = Some("v29".into());
    let g = st
        .insert_receiving(&nf("v-g711", "gw2", T0 + 1))
        .await
        .unwrap();
    st.finalize(g, &g_fin).await.unwrap();
    let r = st
        .insert_receiving(&nf("v-live", "gw1", T0 + 2))
        .await
        .unwrap();
    st.update_pages(r, 2, t(T0 + 7_000)).await.unwrap();
    let f = st
        .insert_receiving(&nf("v-failed", "gw1", T0 + 3))
        .await
        .unwrap();
    let mut f_fin = ff(T0 + 8_000, FaxStatus::Failed, 0);
    f_fin.tiff_path = None;
    f_fin.tiff_size = None;
    f_fin.pdf_path = None;
    f_fin.pdf_size = None;
    st.finalize(f, &f_fin).await.unwrap();
    let p = st
        .insert_receiving(&nf("v-purged", "gw1", T0 + 4))
        .await
        .unwrap();
    st.finalize(p, &ff(T0 + 9_000, FaxStatus::Received, 2))
        .await
        .unwrap();
    st.set_read(a, true, "api:erp").await.unwrap();
    st.set_note(a, "hello 備註", "amy", None).await.unwrap();
    // the files of `p` go: has_tiff and has_pdf turn false, the record stays
    st.mark_files_purged(&[p], t(T0 + 12_000)).await.unwrap();

    let kind = st.backend();
    let cols: Vec<&str> = SAME_COLS.iter().map(|c| c.0).collect();
    let kinds: Vec<Kind> = SAME_COLS.iter().map(|c| c.1).collect();
    let list = cols.join(", ");
    let from_table = st
        .raw()
        .query(
            &format!("SELECT {list} FROM faxes ORDER BY id"),
            &[],
            &kinds,
        )
        .await
        .unwrap();
    let from_view = st
        .raw()
        .query(
            &format!("SELECT {list} FROM fax_records ORDER BY id"),
            &[],
            &kinds,
        )
        .await
        .unwrap();
    assert_eq!(from_table.len(), 5);
    assert_eq!(
        from_view, from_table,
        "[{}] every column of every row is the one of the table",
        env.label
    );
    // has_tiff / has_pdf
    let flags = st
        .raw()
        .query(
            &format!(
                "SELECT id, {}, {} FROM fax_records ORDER BY id",
                as_int(kind, "has_tiff"),
                as_int(kind, "has_pdf")
            ),
            &[],
            &[Kind::Int, Kind::Int, Kind::Int],
        )
        .await
        .unwrap();
    let flags: BTreeMap<i64, (i64, i64)> = flags
        .iter()
        .map(|r| match (&r[0], &r[1], &r[2]) {
            (Val::I(Some(i)), Val::I(Some(x)), Val::I(Some(y))) => (*i, (*x, *y)),
            o => panic!("{o:?}"),
        })
        .collect();
    assert_eq!(flags[&a], (1, 1), "received: both files");
    assert_eq!(flags[&g], (1, 1));
    // the receiving one has no files yet; the failed one never had any; the purged one lost them
    assert_eq!(flags[&r], (0, 0), "receiving");
    assert_eq!(flags[&f], (0, 0), "failed without files");
    assert_eq!(flags[&p], (0, 0), "purged");
    // G.711 and the modem are there
    let m = st
        .raw()
        .query(
            "SELECT media, modem FROM fax_records WHERE id = ?",
            &[g.into()],
            &[Kind::Str, Kind::Str],
        )
        .await
        .unwrap();
    assert_eq!(
        m,
        vec![vec![
            Val::S(Some("g711".into())),
            Val::S(Some("v29".into()))
        ]]
    );
    // no path column: not in the view, not in the document
    let names: Vec<String> = view_columns(env).await.into_iter().map(|c| c.0).collect();
    assert!(
        names.iter().all(|n| !n.contains("path")),
        "no path: {names:?}"
    );
    assert!(!INTEGRATION_MD.contains("tiff_path") && !INTEGRATION_MD.contains("pdf_path"));
}

// ---------------------------------------------------------------- V03
/// The SQL of the document for `heading` ("PostgreSQL：" ...): the statements of its code block.
fn doc_account_sql(heading: &str, db: &str) -> Vec<String> {
    let section = INTEGRATION_MD
        .split("### 建立唯讀帳號")
        .nth(1)
        .expect("the section on the read-only account");
    let at = section
        .find(&format!("\n{heading}\n"))
        .unwrap_or_else(|| panic!("{heading} in the document"));
    let block = section[at..]
        .split("```sql\n")
        .nth(1)
        .and_then(|b| b.split("\n```").next())
        .expect("a sql block");
    block
        .split(';')
        .map(|s| s.trim().replace("sipfax", db))
        .filter(|s| !s.is_empty())
        .collect()
}

/// `url` with another account.
fn as_account(url: &str, user: &str, password: &str) -> String {
    let mut u = url::Url::parse(&url.replacen("mariadb:", "mysql:", 1)).unwrap();
    u.set_username(user).unwrap();
    u.set_password(Some(password)).unwrap();
    let s = u.to_string();
    if url.starts_with("mariadb:") {
        s.replacen("mysql:", "mariadb:", 1)
    } else {
        s
    }
}

/// A statement the account runs on the server: `Ok(rows)` or the server's refusal.
async fn run_as(url: &str, sql: &str) -> std::result::Result<usize, String> {
    match backend_for_url(url).unwrap() {
        BackendKind::Postgres => {
            let mut c = sqlx::PgConnection::connect(url)
                .await
                .map_err(|e| format!("connect: {e}"))?;
            sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_string()))
                .fetch_all(&mut c)
                .await
                .map(|r| r.len())
                .map_err(|e| e.to_string())
        }
        BackendKind::Mysql => {
            let url = url.replacen("mariadb:", "mysql:", 1);
            let mut c = sqlx::MySqlConnection::connect(&url)
                .await
                .map_err(|e| format!("connect: {e}"))?;
            sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_string()))
                .fetch_all(&mut c)
                .await
                .map(|r| r.len())
                .map_err(|e| e.to_string())
        }
        BackendKind::Mssql => {
            let be = crate::backend::mssql::Mssql::connect(url, 1)
                .await
                .map_err(|e| format!("connect: {e}"))?;
            // (one integer column: a count or an id)
            if sql.to_ascii_uppercase().starts_with("SELECT") {
                db::Backend::query(&be, sql, &[], &[Kind::Int])
                    .await
                    .map(|r| r.len())
                    .map_err(|e| e.to_string())
            } else {
                db::Backend::execute(&be, sql, &[])
                    .await
                    .map(|n| n as usize)
                    .map_err(|e| e.to_string())
            }
        }
        BackendKind::Sqlite => unreachable!(),
    }
}

db_case!(v03_the_read_only_account_of_the_document, v03_body);
async fn v03_body(env: &Env) {
    let Target::Server(_) = &env.target else {
        return; // SQLite has no accounts (the document says so)
    };
    let kind = env.store.backend();
    let dbname = env
        .url
        .rsplit('/')
        .next()
        .unwrap()
        .split('?')
        .next()
        .unwrap();
    let (heading, password) = match (kind, env.label.as_str()) {
        (BackendKind::Postgres, _) => ("PostgreSQL：", "change-me"),
        (BackendKind::Mysql, _) => ("MySQL／MariaDB：", "change-me"),
        _ => ("SQL Server：", "Change-me-1"),
    };
    // two faxes, a user and a key that the account must not reach
    for i in 0..2 {
        env.store
            .insert_receiving(&nf(&format!("v03-{i}"), "gw", T0 + i))
            .await
            .unwrap();
    }
    env.store.api_key_create(&key("v03", 5)).await.unwrap();

    // the statements of the document, as written (only the database name is the test's own)
    let stmts = doc_account_sql(heading, dbname);
    assert!(stmts.len() >= 2, "{stmts:?}");
    let drop_first = match kind {
        BackendKind::Postgres => "DROP ROLE IF EXISTS fax_reader",
        BackendKind::Mysql => "DROP USER IF EXISTS 'fax_reader'@'%'",
        _ => "SELECT 1",
    };
    admin_exec(&env.url, drop_first).await;
    for stmt in &stmts {
        admin_exec(&env.url, stmt).await;
    }
    let result = async {
        let reader = as_account(&env.url, "fax_reader", password);
        // the view, yes
        assert_eq!(
            run_as(&reader, "SELECT id FROM fax_records").await,
            Ok(2),
            "[{}] the account reads the view",
            env.label
        );
        // the tables and everything that writes, no
        let denied = [
            "SELECT COUNT(*) FROM faxes",
            "SELECT COUNT(*) FROM users",
            "SELECT COUNT(*) FROM api_keys",
            "SELECT COUNT(*) FROM sessions",
            "SELECT COUNT(*) FROM config_sections",
            "INSERT INTO api_keys (name, token_hash, prefix, scopes, created_by, created_at) VALUES ('x', 'y', 'z', 'fax:read', 'w', '2026-01-01 00:00:00')",
            "UPDATE faxes SET note = 'x'",
            "UPDATE fax_records SET note = 'x'",
            "DELETE FROM faxes",
            "DELETE FROM fax_records",
            "DROP VIEW fax_records",
            "DROP TABLE api_keys",
        ];
        for sql in denied {
            let r = run_as(&reader, sql).await;
            assert!(r.is_err(), "[{}] `{sql}` must be refused, got {r:?}", env.label);
        }
        // and nothing of it happened
        let faxes = env
            .store
            .raw()
            .query("SELECT COUNT(*) FROM faxes", &[], &[Kind::Int])
            .await
            .unwrap();
        assert_eq!(faxes, vec![vec![Val::I(Some(2))]]);
        assert_eq!(
            env.store
                .raw()
                .query(
                    "SELECT COUNT(*) FROM faxes WHERE note IS NOT NULL",
                    &[],
                    &[Kind::Int]
                )
                .await
                .unwrap(),
            vec![vec![Val::I(Some(0))]]
        );
        assert_eq!(env.store.api_key_list().await.unwrap().len(), 1);
    }
    .await;
    let cleanup = match kind {
        BackendKind::Postgres => "DROP OWNED BY fax_reader;;DROP ROLE fax_reader",
        BackendKind::Mysql => "DROP USER 'fax_reader'@'%'",
        _ => "DROP USER fax_reader;;DROP LOGIN fax_reader",
    };
    admin_exec(&env.url, cleanup).await;
    result
}

// ---------------------------------------------------------------- D04 (the SQL of the document)
/// The incremental query of the document for `label` (`-- PostgreSQL` ...).
fn doc_incremental_sql(label: &str) -> String {
    let section = INTEGRATION_MD
        .split("### 增量查詢")
        .nth(1)
        .expect("the section on the incremental query");
    let block = section
        .split("```sql\n")
        .nth(1)
        .and_then(|b| b.split("\n```").next())
        .unwrap();
    let at = block
        .find(&format!("-- {label}\n"))
        .unwrap_or_else(|| panic!("{label} in the document"));
    let rest = &block[at + label.len() + 4..];
    let end = rest.find("\n\n").unwrap_or(rest.len());
    rest[..end].trim().to_string()
}

db_case!(d04_the_incremental_query_of_the_document_runs, d04_sql_body);
async fn d04_sql_body(env: &Env) {
    let Target::Server(_) = &env.target else {
        return; // the document has no SQLite variant (SQLite is no source for another machine)
    };
    let kind = env.store.backend();
    let (label, params): (&str, [&str; 3]) = match kind {
        BackendKind::Postgres => ("PostgreSQL", ["'1970-01-01'", "'1970-01-01'", "0"]),
        BackendKind::Mysql => ("MySQL / MariaDB", ["'1970-01-01'", "'1970-01-01'", "0"]),
        _ => ("SQL Server", ["'1970-01-01'", "'1970-01-01'", "0"]),
    };
    let sql_of = |last_updated: &str, last_id: i64| -> String {
        let mut sql = doc_incremental_sql(label);
        match kind {
            BackendKind::Postgres => {
                sql = sql
                    .replace(":last_updated", last_updated)
                    .replace(":last_id", &last_id.to_string());
            }
            BackendKind::Mysql => {
                // `?` three times: updated_at, updated_at, id
                for v in [last_updated, last_updated, &last_id.to_string()] {
                    sql = sql.replacen('?', v, 1);
                }
            }
            _ => {
                sql = sql
                    .replace("@last_updated", last_updated)
                    .replace("@last_id", &last_id.to_string());
            }
        }
        let _ = params;
        sql
    };
    // the view's columns in the document's order, as the values the driver decodes
    let boolish = if matches!(kind, BackendKind::Mysql) {
        Kind::Int
    } else {
        Kind::Bool
    };
    let kinds: Vec<Kind> = doc_view_columns()
        .iter()
        .map(|(name, ty)| match (name.as_str(), ty.as_str()) {
            ("has_tiff" | "has_pdf", _) => boolish,
            (_, "整數") => Kind::Int,
            (_, "字串") => Kind::Str,
            (_, "時間") => Kind::Time,
            (_, "布林") => Kind::Bool,
            other => panic!("{other:?}"),
        })
        .collect();
    let ids_of = |rows: Vec<Vec<Val>>| -> Vec<i64> {
        rows.iter()
            .map(|r| match r[0] {
                Val::I(Some(i)) => i,
                ref o => panic!("{o:?}"),
            })
            .collect()
    };
    let st = &env.store;
    // three faxes some time ago (the clock of the test), two in the same millisecond
    let mut ids = Vec::new();
    for (i, at) in [T0 + 10, T0 + 20, T0 + 20].into_iter().enumerate() {
        env.clock.set(t(at));
        ids.push(
            st.insert_receiving(&nf(&format!("d04-{i}"), "gw", at))
                .await
                .unwrap(),
        );
    }
    // from the start: all three, in the order of (updated_at, id), with the 38 columns
    let rows = st
        .raw()
        .query(&sql_of("'1970-01-01'", 0), &[], &kinds)
        .await
        .unwrap();
    assert_eq!(rows[0].len(), kinds.len());
    assert_eq!(ids_of(rows), ids, "[{}] all three", env.label);
    // from the position after the first one, in the middle of the tie, after the tie
    for (after_ms, after_id, want) in [
        (T0 + 10, ids[0], ids[1..].to_vec()),
        (T0 + 20, ids[1], vec![ids[2]]),
        (T0 + 20, ids[2], vec![]),
    ] {
        let lit = format!("'{}'", t(after_ms).format("%Y-%m-%d %H:%M:%S%.3f"));
        let rows = st
            .raw()
            .query(&sql_of(&lit, after_id), &[], &kinds)
            .await
            .unwrap();
        assert_eq!(
            ids_of(rows),
            want,
            "[{}] after {after_ms}.{after_id}",
            env.label
        );
    }
    // a row written just now is held back for 5 seconds (by the clock of the database server,
    // in UTC whatever the time zone of the session), an older one is not
    env.clock.set(chrono::Utc::now());
    let fresh = st.insert_receiving(&nf("d04-now", "gw", 1)).await.unwrap();
    let rows = st
        .raw()
        .query(&sql_of("'1970-01-01'", 0), &[], &kinds)
        .await
        .unwrap();
    let got = ids_of(rows);
    assert!(
        !got.contains(&fresh),
        "[{}] a row of this moment waits: {got:?}",
        env.label
    );
    assert_eq!(got, ids);
    env.clock.set(chrono::Utc::now() - Duration::seconds(60));
    let old = st.insert_receiving(&nf("d04-old", "gw", 2)).await.unwrap();
    let rows = st
        .raw()
        .query(&sql_of("'1970-01-01'", 0), &[], &kinds)
        .await
        .unwrap();
    let got = ids_of(rows);
    assert!(
        got.contains(&old),
        "[{}] a row of a minute ago is there: {got:?}",
        env.label
    );
}
