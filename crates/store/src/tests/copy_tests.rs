//! M4e store cases C01-C06, C10 and C13 (`sip-faxd db copy`, docs/test-plans/M4e.md). The matrix
//! cases (`copy_case!`) run SQLite -> SQLite locally; with `SIP_FAXD_TEST_DB=<server>` they run in
//! both directions between a fresh SQLite file and a fresh database on that server.
//!
//! Both sides are compared with raw queries (`SELECT <every column> ... ORDER BY id`), never through
//! `FaxStore`, and the source is also compared with the rows written by the test, so a decode
//! error that hits both sides cannot hide.

use super::*;
use crate::copy::{READ_PAGE, TABLES, batch_rows, copy_database, table_names};
use crate::db::{Backend, Kind, Val};
use crate::migrate::{Migration, apply, migrations};
use std::collections::BTreeSet;

use Kind::{Bool as B, Int as I, Str as S, Time as T};

type Row = Vec<Val>;

struct Spec {
    name: &'static str,
    /// every column in schema order, the key first
    cols: &'static [(&'static str, Kind)],
    identity: bool,
}

impl Spec {
    fn insert_cols(&self) -> Vec<&'static str> {
        let skip = usize::from(self.identity);
        self.cols[skip..].iter().map(|(n, _)| *n).collect()
    }
    fn idx(&self, col: &str) -> usize {
        let skip = usize::from(self.identity);
        self.cols[skip..]
            .iter()
            .position(|(n, _)| *n == col)
            .unwrap_or_else(|| panic!("no column {col} in {}", self.name))
    }
}

const FAXES: Spec = Spec {
    name: "faxes",
    identity: true,
    cols: &[
        ("id", I),
        ("call_id", S),
        ("trunk", S),
        ("ani", S),
        ("ani_source", S),
        ("dnis", S),
        ("dnis_source", S),
        ("remote_ident", S),
        ("started_at", T),
        ("answered_at", T),
        ("ended_at", T),
        ("duration_sec", I),
        ("hangup_direction", S),
        ("hangup_code", S),
        ("hangup_text", S),
        ("sip_status", I),
        ("t30_code", I),
        ("fax_status", S),
        ("pages", I),
        ("resolution", S),
        ("bit_rate", I),
        ("compression", S),
        ("ecm", B),
        ("tiff_path", S),
        ("tiff_size", I),
        ("pdf_path", S),
        ("pdf_size", I),
        ("files_purged_at", T),
        ("is_read", B),
        ("read_changed_by", S),
        ("read_changed_at", T),
        ("note", S),
        ("note_updated_by", S),
        ("note_updated_at", T),
        ("created_at", T),
        ("updated_at", T),
    ],
};
const DELIVERIES: Spec = Spec {
    name: "delivery_attempts",
    identity: true,
    cols: &[
        ("id", I),
        ("fax_id", I),
        ("target", S),
        ("kind", S),
        ("attempt", I),
        ("status", S),
        ("last_error", S),
        ("next_try_at", T),
        ("created_at", T),
        ("updated_at", T),
    ],
};
const SETTINGS: Spec = Spec {
    name: "settings",
    identity: false,
    cols: &[
        ("name", S),
        ("value", S),
        ("updated_by", S),
        ("updated_at", T),
    ],
};
const SECTIONS: Spec = Spec {
    name: "config_sections",
    identity: false,
    cols: &[
        ("name", S),
        ("body", S),
        ("version", I),
        ("updated_by", S),
        ("updated_at", T),
    ],
};
const AUDIT: Spec = Spec {
    name: "config_audit",
    identity: true,
    cols: &[
        ("id", I),
        ("section", S),
        ("action", S),
        ("before_json", S),
        ("after_json", S),
        ("changed_by", S),
        ("at", T),
    ],
};
const USERS: Spec = Spec {
    name: "users",
    identity: true,
    cols: &[
        ("id", I),
        ("username", S),
        ("display_name", S),
        ("role", S),
        ("password_hash", S),
        ("locale", S),
        ("disabled", B),
        ("must_change_password", B),
        ("created_at", T),
        ("updated_at", T),
        ("last_login_at", T),
    ],
};
const SESSIONS: Spec = Spec {
    name: "sessions",
    identity: true,
    cols: &[
        ("id", I),
        ("token_hash", S),
        ("user_id", I),
        ("created_at", T),
        ("last_seen_at", T),
        ("expires_at", T),
        ("ip", S),
        ("user_agent", S),
    ],
};
/// Copy order (parents first), the same as `store::copy`.
const SPECS: [&Spec; 7] = [
    &FAXES,
    &DELIVERIES,
    &SETTINGS,
    &SECTIONS,
    &AUDIT,
    &USERS,
    &SESSIONS,
];

// ---------------------------------------------------------------- value helpers
fn s(x: &str) -> Val {
    Val::S(Some(x.to_string()))
}
fn ns() -> Val {
    Val::S(None)
}
fn i(n: i64) -> Val {
    Val::I(Some(n))
}
fn ni() -> Val {
    Val::I(None)
}
fn b(x: bool) -> Val {
    Val::B(Some(x))
}
/// A time `ms` after T0 (millisecond precision on purpose).
fn tm(ms: i64) -> Val {
    Val::T(Some(t(T0 + ms)))
}
fn nt() -> Val {
    Val::T(None)
}
/// Exactly `n` characters (all in the BMP, so also `n` UTF-16 units): `prefix` then CJK.
fn fill(prefix: &str, n: usize) -> String {
    let mut out = String::from(prefix);
    let pad: Vec<char> = "傳真伺服器測試資料庫".chars().collect();
    let mut k = 0;
    while out.chars().count() < n {
        out.push(pad[k % pad.len()]);
        k += 1;
    }
    assert_eq!(out.chars().count(), n, "prefix longer than the limit");
    out
}
fn hex64(seed: u8) -> String {
    (0..64)
        .map(|k| format!("{:x}", (usize::from(seed) + k) % 16))
        .collect()
}
fn set(row: &mut Row, spec: &Spec, col: &str, v: Val) {
    row[spec.idx(col)] = v;
}

/// A faxes row with every nullable column NULL.
fn fax_base(call_id: &str) -> Row {
    let mut r: Row = FAXES.insert_cols().iter().map(|_| ni()).collect();
    for (n, k) in &FAXES.cols[1..] {
        let v = match (*n, *k) {
            ("call_id", _) => s(call_id),
            ("trunk", _) => s("gw"),
            ("fax_status", _) => s("receiving"),
            ("pages", _) => i(0),
            ("ecm" | "is_read", _) => b(false),
            ("started_at" | "created_at" | "updated_at", _) => tm(1_000),
            (_, S) => ns(),
            (_, T) => nt(),
            (_, _) => ni(),
        };
        set(&mut r, &FAXES, n, v);
    }
    r
}

// ---------------------------------------------------------------- the data set
fn fax_rows() -> Vec<(bool, Row)> {
    // A: every column filled, strings at the column limits of docs/database.md
    let mut a = fax_base(&fill("call-", 255));
    for (c, v) in [
        ("trunk", s(&fill("幹線", 64))),
        ("ani", s(&fill("+886", 64))),
        ("ani_source", s(&fill("pai", 16))),
        ("dnis", s(&fill("02", 64))),
        ("dnis_source", s(&fill("ruri", 16))),
        ("remote_ident", s(&fill("+886 2 ", 32))),
        ("started_at", tm(123)),
        ("answered_at", tm(2_456)),
        ("ended_at", tm(61_789)),
        ("duration_sec", i(2_147_483_647)),
        ("hangup_direction", s(&fill("remote", 8))),
        ("hangup_code", s(&fill("completed", 32))),
        ("hangup_text", s(&fill("正常完成 ", 255))),
        ("sip_status", i(486)),
        ("t30_code", i(-1)),
        ("fax_status", s(&fill("received", 12))),
        ("pages", i(2_000_000_000)),
        ("resolution", s(&fill("fine", 12))),
        ("bit_rate", i(14_400)),
        ("compression", s(&fill("MMR", 8))),
        ("ecm", b(true)),
        ("tiff_path", s(&fill("faxes/2026/10/", 1024))),
        ("tiff_size", i(9_007_199_254_740_993)),
        ("pdf_path", s(&fill("faxes/2026/10/", 1024))),
        ("pdf_size", i(9_007_199_254_740_995)),
        ("files_purged_at", tm(86_400_001)),
        ("is_read", b(true)),
        ("read_changed_by", s(&fill("amy", 64))),
        ("read_changed_at", tm(70_001)),
        ("note", s(&format!("{}📝", fill("備註 ", 4000)))),
        ("note_updated_by", s(&fill("ben", 64))),
        ("note_updated_at", tm(70_999)),
        ("created_at", tm(124)),
        ("updated_at", tm(70_999)),
    ] {
        set(&mut a, &FAXES, c, v);
    }
    // B: a call that is still being received, everything optional NULL
    let b_row = fax_base("rx-still-receiving");
    // C: emoji, CJK, SQL-looking characters, line breaks
    let mut c = fax_base("call-emoji-📠-傳真");
    for (col, v) in [
        ("ani", s("+886 2 2700 1234")),
        ("hangup_text", s("對方掛斷 📠🚫 ñ é ß")),
        ("fax_status", s("partial")),
        ("pages", i(3)),
        ("is_read", b(true)),
        ("read_changed_by", s("amy")),
        ("read_changed_at", tm(5_000)),
        (
            "note",
            s("備註 📝 line1\nline2\ttab 'single' \"double\" ; -- % _ \\ ? 😀👨‍👩‍👧"),
        ),
        ("note_updated_by", s("amy")),
        ("note_updated_at", tm(5_001)),
    ] {
        set(&mut c, &FAXES, col, v);
    }
    // E: failed, empty note, purged files
    let mut e = fax_base("call-failed");
    for (col, v) in [
        ("fax_status", s("failed")),
        ("sip_status", i(0)),
        ("note", s("")),
        ("pages", i(0)),
        ("duration_sec", i(0)),
        ("files_purged_at", tm(999)),
    ] {
        set(&mut e, &FAXES, col, v);
    }
    // G: received, ECM, millisecond times
    let mut g = fax_base("call-received");
    for (col, v) in [
        ("fax_status", s("received")),
        ("pages", i(5)),
        ("ecm", b(true)),
        ("bit_rate", i(14_400)),
        ("ended_at", tm(31_007)),
        ("answered_at", tm(1_999)),
        ("tiff_size", i(123_456)),
        ("pdf_size", i(0)),
    ] {
        set(&mut g, &FAXES, col, v);
    }
    vec![
        (true, a),
        (true, b_row),
        (true, c),
        (false, fax_base("throwaway-1")),
        (true, e),
        (false, fax_base("throwaway-2")),
        (true, g),
    ]
}

fn delivery_rows() -> Vec<(bool, Row)> {
    let mk = |fax: i64,
              target: &str,
              kind: &str,
              attempt: i64,
              status: &str,
              err: Option<&str>,
              next: Val| {
        vec![
            i(fax),
            s(target),
            s(kind),
            i(attempt),
            s(status),
            err.map_or_else(ns, s),
            next,
            tm(2_000 + attempt),
            tm(2_100 + attempt),
        ]
    };
    vec![
        (
            true,
            mk(
                1,
                &fill("folder:/srv/fax/", 255),
                "folder",
                1,
                "done",
                None,
                nt(),
            ),
        ),
        (
            true,
            mk(
                1,
                "email:a@example.com,b@example.com",
                "email",
                2,
                "failed",
                Some(&fill("SMTP 550 錯誤 📧 ", 3000)),
                tm(9_000_123),
            ),
        ),
        (
            true,
            mk(
                3,
                "webhook:https://hook.example/fax",
                "webhook",
                4,
                "giving_up",
                Some("HTTP 500"),
                nt(),
            ),
        ),
        (
            false,
            mk(3, "folder:/tmp/x", "folder", 1, "pending", None, tm(1)),
        ),
        (
            true,
            mk(
                5,
                "folder:/srv/fax/b",
                "folder",
                1,
                "pending",
                None,
                tm(3_600_000),
            ),
        ),
        (
            true,
            mk(
                7,
                "email:ops@example.com",
                "email",
                1,
                "pending",
                None,
                tm(60_000),
            ),
        ),
    ]
}

fn setting_rows() -> Vec<(bool, Row)> {
    vec![
        (true, vec![s(&fill("a.", 64)), s("1"), ns(), tm(10)]),
        (
            true,
            vec![s("ui.title"), s("傳真 📠 server"), s("admin"), tm(11)],
        ),
        (true, vec![s("z.last"), s(""), s(&fill("u", 64)), tm(12)]),
    ]
}

fn section_rows() -> Vec<(bool, Row)> {
    vec![
        (
            true,
            vec![
                s("retention"),
                s(r#"{"files_days":90,"records_days":365,"run_at":"03:17"}"#),
                i(1),
                s("migration"),
                tm(20),
            ],
        ),
        (
            true,
            vec![
                s("trunk:gw"),
                s(r#"{"name":"gw","register":{"password":"enc:v1:Zm9vYmFyMTIzNDU2Nzg5MA+/AB=="}}"#),
                i(7),
                s("admin"),
                tm(21),
            ],
        ),
        (
            true,
            vec![
                s(&fill("trunk:", 64)),
                s(&format!("{{\"note\":\"{}\"}}", fill("幹線 📞 ", 6000))),
                i(2_147_483_647),
                ns(),
                tm(22),
            ],
        ),
    ]
}

fn audit_rows() -> Vec<(bool, Row)> {
    let mk =
        |sec: &str, act: &str, before: Option<&str>, after: Option<&str>, by: &str, at: i64| {
            vec![
                s(sec),
                s(act),
                before.map_or_else(ns, s),
                after.map_or_else(ns, s),
                s(by),
                tm(at),
            ]
        };
    vec![
        (
            true,
            mk(
                "trunk:gw",
                "create",
                None,
                Some(r#"{"password":"***"}"#),
                "admin",
                30_001,
            ),
        ),
        (
            false,
            mk("sip", "update", Some("{}"), Some("{}"), "x", 30_002),
        ),
        (
            true,
            mk(
                "sip",
                "update",
                Some(r#"{"bind":"0.0.0.0:5060"}"#),
                Some(r#"{"bind":"0.0.0.0:5061"}"#),
                "cli:root",
                30_003,
            ),
        ),
        (
            true,
            mk(
                &fill("user:", 64),
                "user_add",
                None,
                None,
                &fill("config-file", 64),
                30_004,
            ),
        ),
        (
            true,
            mk(
                "trunk:gw",
                "delete",
                Some(&fill("{\"長\":\"", 5000)),
                None,
                "amy",
                30_005,
            ),
        ),
    ]
}

fn user_rows() -> Vec<(bool, Row)> {
    let mk = |name: &str,
              disp: Option<&str>,
              role: &str,
              loc: Option<&str>,
              dis: bool,
              must: bool,
              last: Val| {
        vec![
            s(name),
            disp.map_or_else(ns, s),
            s(role),
            s("$argon2id$v=19$m=19456,t=2,p=1$c2FsdHNhbHQ$aGFzaGhhc2hoYXNoaGFzaA"),
            loc.map_or_else(ns, s),
            b(dis),
            b(must),
            tm(40_000),
            tm(40_001),
            last,
        ]
    };
    vec![
        (
            true,
            mk(
                "admin",
                Some("管理員 👑"),
                "admin",
                Some("zh-TW"),
                false,
                false,
                tm(50_123),
            ),
        ),
        (true, mk("amy", None, "agent", None, false, false, nt())),
        (false, mk("gone", None, "agent", None, false, false, nt())),
        (
            true,
            mk(
                &"b".repeat(64),
                Some(&fill("名", 128)),
                "agent",
                Some("en"),
                true,
                true,
                nt(),
            ),
        ),
    ]
}

fn session_rows() -> Vec<(bool, Row)> {
    let mk = |tok: u8, user: i64, ip: Option<&str>, ua: Option<&str>| {
        vec![
            s(&hex64(tok)),
            i(user),
            tm(60_000),
            tm(60_500),
            tm(60_000 + 7 * 86_400_000),
            ip.map_or_else(ns, s),
            ua.map_or_else(ns, s),
        ]
    };
    vec![
        (
            true,
            mk(
                1,
                1,
                Some("203.0.113.9"),
                Some("Mozilla/5.0 (X11; Linux) 瀏覽器 😀"),
            ),
        ),
        (true, mk(2, 2, None, None)),
        (
            true,
            mk(3, 4, Some(&fill("2001:db8::", 45)), Some(&fill("UA ", 255))),
        ),
    ]
}

fn seed_rows() -> Vec<Vec<(bool, Row)>> {
    vec![
        fax_rows(),
        delivery_rows(),
        setting_rows(),
        section_rows(),
        audit_rows(),
        user_rows(),
        session_rows(),
    ]
}

// ---------------------------------------------------------------- raw access
/// Inserts `rows` (ids come from the table, 1.. in order), deletes the ones marked as throw-away
/// (so the ids have gaps) and returns what must be in the table, ids included.
async fn put(be: &dyn Backend, spec: &Spec, rows: &[(bool, Row)]) -> Vec<Row> {
    let cols = spec.insert_cols();
    let ph = format!("({})", vec!["?"; cols.len()].join(", "));
    let per = (1500 / cols.len()).clamp(1, 100);
    for chunk in rows.chunks(per) {
        let sql = format!(
            "INSERT INTO {} ({}) VALUES {}",
            spec.name,
            cols.join(", "),
            vec![ph.as_str(); chunk.len()].join(", ")
        );
        let params: Vec<Val> = chunk.iter().flat_map(|(_, r)| r.iter().cloned()).collect();
        be.execute(&sql, &params)
            .await
            .unwrap_or_else(|e| panic!("seed {}: {e}", spec.name));
    }
    let mut expected = Vec::new();
    for (n, (keep, row)) in rows.iter().enumerate() {
        let id = n as i64 + 1;
        if *keep {
            let mut r = row.clone();
            if spec.identity {
                r.insert(0, i(id));
            }
            expected.push(r);
        } else {
            be.execute(
                &format!("DELETE FROM {} WHERE id = ?", spec.name),
                &[Val::from(id)],
            )
            .await
            .unwrap();
        }
    }
    expected
}

async fn seed(be: &dyn Backend) -> Vec<Vec<Row>> {
    let mut out = Vec::new();
    for (spec, rows) in SPECS.iter().zip(seed_rows()) {
        out.push(put(be, spec, &rows).await);
    }
    out
}

fn key_order(a: &Row, b: &Row) -> std::cmp::Ordering {
    match (&a[0], &b[0]) {
        (Val::I(x), Val::I(y)) => x.cmp(y),
        (Val::S(x), Val::S(y)) => x.cmp(y),
        other => panic!("key {other:?}"),
    }
}

/// Every column of every table, read with raw queries, in key order (sorted here so that the
/// databases' collations cannot reorder the name-keyed tables).
async fn dump(be: &dyn Backend) -> Vec<Vec<Row>> {
    let mut out = Vec::new();
    for spec in SPECS {
        let names: Vec<&str> = spec.cols.iter().map(|(n, _)| *n).collect();
        let kinds: Vec<Kind> = spec.cols.iter().map(|(_, k)| *k).collect();
        let mut rows = be
            .query(
                &format!(
                    "SELECT {} FROM {} ORDER BY {}",
                    names.join(", "),
                    spec.name,
                    names[0]
                ),
                &[],
                &kinds,
            )
            .await
            .unwrap_or_else(|e| panic!("dump {}: {e}", spec.name));
        rows.sort_by(key_order);
        out.push(rows);
    }
    out
}

fn assert_tables(what: &str, left: &[Vec<Row>], right: &[Vec<Row>]) {
    for ((spec, l), r) in SPECS.iter().zip(left).zip(right) {
        assert_eq!(l.len(), r.len(), "{what}: row count of {}", spec.name);
        for (lr, rr) in l.iter().zip(r) {
            for (k, (name, _)) in spec.cols.iter().enumerate() {
                assert_eq!(
                    lr[k], rr[k],
                    "{what}: {}.{name} of the row with key {:?}",
                    spec.name, lr[0]
                );
            }
        }
    }
}

async fn counts(be: &dyn Backend) -> Vec<u64> {
    let mut out = Vec::new();
    for spec in SPECS {
        let r = be
            .query(&format!("SELECT COUNT(*) FROM {}", spec.name), &[], &[I])
            .await
            .unwrap();
        out.push(match r[0][0] {
            Val::I(Some(n)) => n as u64,
            _ => panic!("count"),
        });
    }
    out
}

async fn assert_empty(what: &str, be: &dyn Backend) {
    for (spec, n) in SPECS.iter().zip(counts(be).await) {
        assert_eq!(n, 0, "{what}: {} must be empty", spec.name);
    }
}

async fn run_copy(src: &Env, dst: &Env) -> crate::Result<crate::CopyReport> {
    let mut seen = Vec::new();
    let r = copy_database(&src.url, &dst.url, &mut |t, n| {
        seen.push((t.to_string(), n))
    })
    .await;
    if let Ok(rep) = &r {
        let from_report: Vec<(String, u64)> = rep
            .tables
            .iter()
            .map(|(t, n)| (t.to_string(), *n))
            .collect();
        assert_eq!(seen, from_report, "progress callback and report agree");
    }
    r
}

/// The data set has every column filled somewhere, and NULL where the column allows it.
fn assert_seed_is_thorough(expected: &[Vec<Row>]) {
    for (spec, rows) in SPECS.iter().zip(expected) {
        assert!(!rows.is_empty(), "{} has rows", spec.name);
        for (k, (name, _)) in spec.cols.iter().enumerate() {
            assert!(
                rows.iter().any(|r| !matches!(
                    &r[k],
                    Val::I(None) | Val::S(None) | Val::T(None) | Val::B(None)
                )),
                "{}.{name} is never filled",
                spec.name
            );
        }
    }
    let nullable_faxes: Vec<&str> = FAXES.cols[1..]
        .iter()
        .map(|(n, _)| *n)
        .filter(|n| {
            ![
                "call_id",
                "trunk",
                "started_at",
                "fax_status",
                "pages",
                "ecm",
                "is_read",
                "created_at",
                "updated_at",
            ]
            .contains(n)
        })
        .collect();
    for n in nullable_faxes {
        let k = FAXES.cols.iter().position(|(c, _)| *c == n).unwrap();
        assert!(
            expected[0]
                .iter()
                .any(|r| matches!(&r[k], Val::I(None) | Val::S(None) | Val::T(None))),
            "faxes.{n} is never NULL"
        );
    }
}

/// C01/C02 body: seed `src`, copy, compare every column of every table.
async fn full_copy(src: &Env, dst: &Env) {
    let label = format!("{} -> {}", src.label, dst.label);
    let expected = seed(src.store.raw()).await;
    assert_seed_is_thorough(&expected);
    let src_rows = dump(src.store.raw()).await;
    assert_tables(
        &format!("{label}: source against what was written"),
        &src_rows,
        &expected,
    );
    assert_empty(&format!("{label}: before"), dst.store.raw()).await;

    let rep = run_copy(src, dst)
        .await
        .unwrap_or_else(|e| panic!("[{label}] copy: {e}"));
    assert_eq!(rep.from, src.store.backend());
    assert_eq!(rep.to, dst.store.backend());
    assert_eq!(rep.schema, crate::latest_version(dst.store.backend()));
    let names: Vec<&str> = rep.tables.iter().map(|(n, _)| *n).collect();
    assert_eq!(names, table_names());
    let want: Vec<u64> = expected.iter().map(|r| r.len() as u64).collect();
    let got: Vec<u64> = rep.tables.iter().map(|(_, n)| *n).collect();
    assert_eq!(got, want, "[{label}] rows reported per table");

    let dst_rows = dump(dst.store.raw()).await;
    assert_tables(
        &format!("{label}: target against what was written"),
        &dst_rows,
        &expected,
    );
    assert_tables(
        &format!("{label}: target against source"),
        &dst_rows,
        &src_rows,
    );
    // the source is untouched
    assert_tables(
        &format!("{label}: source after"),
        &dump(src.store.raw()).await,
        &expected,
    );
    // the max ids are not the counts: the gaps were kept
    assert_eq!(expected[0].last().unwrap()[0], i(7));
    assert_eq!(expected[0].len(), 5);
}

macro_rules! copy_case {
    ($name:ident, $body:ident) => {
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn $name() {
            for target in $crate::tests::targets() {
                if matches!(target, Target::SqliteMemory) {
                    continue; // a copy needs two databases that can be named by URL
                }
                let _guard = $crate::tests::DB_LOCK.read().await;
                let dirs = match &target {
                    Target::Server(_) => vec![
                        (Target::SqliteFile, target.clone()),
                        (target.clone(), Target::SqliteFile),
                    ],
                    _ => vec![(Target::SqliteFile, Target::SqliteFile)],
                };
                for (from, to) in dirs {
                    let src = Env::create(from).await;
                    let dst = Env::create(to).await;
                    eprintln!("[{} -> {}] {}", src.label, dst.label, stringify!($name));
                    tokio::time::timeout(std::time::Duration::from_secs(300), $body(&src, &dst))
                        .await
                        .unwrap_or_else(|_| {
                            panic!(
                                "[{} -> {}] {} timed out",
                                src.label,
                                dst.label,
                                stringify!($name)
                            )
                        });
                    src.cleanup().await;
                    dst.cleanup().await;
                }
            }
        }
    };
}

// ---------------------------------------------------------------- C01
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c01_sqlite_to_sqlite_copies_every_row_and_column_with_the_same_ids() {
    let src = Env::create(Target::SqliteFile).await;
    let dst = Env::create(Target::SqliteFile).await;
    full_copy(&src, &dst).await;
    src.cleanup().await;
    dst.cleanup().await;
}

// ---------------------------------------------------------------- C02
copy_case!(
    c02_matrix_copy_keeps_nulls_booleans_milliseconds_unicode_and_limit_length_values,
    c02_body
);
async fn c02_body(src: &Env, dst: &Env) {
    full_copy(src, dst).await;
}

// ---------------------------------------------------------------- C03
copy_case!(
    c03_new_ids_after_the_copy_continue_above_the_copied_ones,
    c03_body
);
async fn c03_body(src: &Env, dst: &Env) {
    let expected = seed(src.store.raw()).await;
    run_copy(src, dst).await.unwrap();
    let max = |t: usize| match expected[t]
        .iter()
        .map(|r| r[0].clone())
        .max_by(|a, b| key_order(&vec![a.clone()], &vec![b.clone()]))
    {
        Some(Val::I(Some(n))) => n,
        other => panic!("max id {other:?}"),
    };
    let st = &dst.store;
    // a fax
    let fax = st
        .insert_receiving(&nf("after-the-copy", "gw", 500))
        .await
        .unwrap();
    assert!(fax > max(0), "fax id {fax} > {}", max(0));
    // a delivery of the new fax
    let delivery = st
        .add_delivery(&NewDelivery {
            fax_id: fax,
            target: "folder:/x".into(),
            kind: "folder".into(),
            attempt: 1,
            status: DeliveryStatus::Pending,
            next_try_at: None,
        })
        .await
        .unwrap();
    assert!(delivery > max(1), "delivery id {delivery} > {}", max(1));
    // an audit row
    st.config_audit_add(
        "user:new",
        "admin",
        &ConfigAudit {
            action: "user_add".into(),
            before: None,
            after: None,
        },
    )
    .await
    .unwrap();
    let audit = st.config_audit_list(1, None).await.unwrap();
    assert_eq!(audit[0].section, "user:new");
    assert!(
        audit[0].id > max(4),
        "audit id {} > {}",
        audit[0].id,
        max(4)
    );
    // a user and its session
    let user = st
        .user_create(&NewUser {
            username: "after".into(),
            display_name: None,
            role: "agent".into(),
            password_hash: "h".into(),
            locale: None,
            must_change_password: false,
        })
        .await
        .unwrap();
    assert!(user.id > max(5), "user id {} > {}", user.id, max(5));
    let session = st
        .session_create(&NewSession {
            token_hash: hex64(9),
            user_id: user.id,
            created_at: t(T0),
            expires_at: t(T0 + 1000),
            ip: None,
            user_agent: None,
        })
        .await
        .unwrap();
    assert!(session > max(6), "session id {session} > {}", max(6));
    // the copied rows are as they were, the new ones are added
    let after = dump(dst.store.raw()).await;
    for (n, spec) in SPECS.iter().enumerate() {
        if !spec.identity {
            continue;
        }
        assert_eq!(
            after[n].len(),
            expected[n].len() + 1,
            "{} got exactly one new row",
            spec.name
        );
        assert_tables_one(spec, &after[n][..expected[n].len()], &expected[n]);
    }
}

fn assert_tables_one(spec: &Spec, got: &[Row], want: &[Row]) {
    assert_eq!(got.len(), want.len());
    for (g, w) in got.iter().zip(want) {
        assert_eq!(g, w, "copied row of {} unchanged", spec.name);
    }
}

// ---------------------------------------------------------------- C04
copy_case!(
    c04_a_target_with_data_in_any_table_is_refused_and_left_alone,
    c04_body
);
async fn c04_body(src: &Env, dst: &Env) {
    seed(src.store.raw()).await;
    let src_before = dump(src.store.raw()).await;
    let st = &dst.store;
    // (what is planted, the table the message must name: the first non-empty one in copy order)
    let plants = [
        "faxes",
        "delivery_attempts",
        "settings",
        "config_sections",
        "config_audit",
        "users",
        "sessions",
    ];
    for planted in plants {
        match planted {
            "faxes" | "delivery_attempts" => {
                let id = st.insert_receiving(&nf("planted", "gw", 1)).await.unwrap();
                if planted == "delivery_attempts" {
                    st.add_delivery(&NewDelivery {
                        fax_id: id,
                        target: "folder:/p".into(),
                        kind: "folder".into(),
                        attempt: 1,
                        status: DeliveryStatus::Done,
                        next_try_at: None,
                    })
                    .await
                    .unwrap();
                }
            }
            "settings" => st.set_setting("planted", "1", Some("t")).await.unwrap(),
            "config_sections" => {
                st.config_put(
                    "planted",
                    "{}",
                    None,
                    "t",
                    &ConfigAudit {
                        action: "create".into(),
                        before: None,
                        after: None,
                    },
                )
                .await
                .unwrap();
                // that also wrote an audit row: remove it so only config_sections is planted
                dst.store
                    .raw()
                    .execute("DELETE FROM config_audit", &[])
                    .await
                    .unwrap();
            }
            "config_audit" => st
                .config_audit_add(
                    "planted",
                    "t",
                    &ConfigAudit {
                        action: "update".into(),
                        before: None,
                        after: None,
                    },
                )
                .await
                .unwrap(),
            "users" | "sessions" => {
                let u = st
                    .user_create(&NewUser {
                        username: "planted".into(),
                        display_name: None,
                        role: "agent".into(),
                        password_hash: "h".into(),
                        locale: None,
                        must_change_password: false,
                    })
                    .await
                    .unwrap();
                if planted == "sessions" {
                    st.session_create(&NewSession {
                        token_hash: hex64(5),
                        user_id: u.id,
                        created_at: t(T0),
                        expires_at: t(T0 + 1),
                        ip: None,
                        user_agent: None,
                    })
                    .await
                    .unwrap();
                }
            }
            _ => unreachable!(),
        }
        let first_nonempty = counts(dst.store.raw())
            .await
            .iter()
            .position(|n| *n > 0)
            .map(|k| SPECS[k].name)
            .unwrap();
        let before = dump(dst.store.raw()).await;
        let err = run_copy(src, dst).await.expect_err(planted);
        let msg = err.to_string();
        assert!(matches!(err, crate::Error::Invalid(_)), "{planted}: {msg}");
        assert!(msg.contains("not empty"), "{planted}: {msg}");
        assert!(
            msg.contains(&format!("table {first_nonempty} ")),
            "{planted}: message must name {first_nonempty}: {msg}"
        );
        assert_tables(
            &format!("target after refusing {planted}"),
            &dump(dst.store.raw()).await,
            &before,
        );
        assert_tables(
            "source after a refused copy",
            &dump(src.store.raw()).await,
            &src_before,
        );
        // empty the target again for the next table
        for spec in SPECS.iter().rev() {
            dst.store
                .raw()
                .execute(&format!("DELETE FROM {}", spec.name), &[])
                .await
                .unwrap();
        }
        assert_empty("target cleaned", dst.store.raw()).await;
    }
    // and with the target empty again the copy goes through
    run_copy(src, dst).await.unwrap();
    assert_tables(
        "copy after the refusals",
        &dump(dst.store.raw()).await,
        &src_before,
    );
}

// ---------------------------------------------------------------- C05
async fn at_version_one(env: &Env) {
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
    let first: Vec<Migration> = migrations(env.store.backend())[..1]
        .iter()
        .map(|m| Migration {
            version: m.version,
            name: m.name,
            sql: m.sql,
        })
        .collect();
    apply(env.store.raw(), env.clock.as_ref(), &first)
        .await
        .unwrap();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c05_an_older_or_newer_source_is_refused_and_the_target_is_not_written() {
    let dst = Env::create(Target::SqliteFile).await;
    let never = dst.dir.path().join("never-created.db");
    let never_url = format!("sqlite://{}", never.display());

    // older: the hint names `db migrate`
    let old = Env::create(Target::SqliteFile).await;
    at_version_one(&old).await;
    let err = copy_database(&old.url, &dst.url, &mut |_, _| {})
        .await
        .unwrap_err();
    assert!(
        matches!(err, crate::Error::SchemaOutdated { db: 1, .. }),
        "{err}"
    );
    assert!(err.to_string().contains("db migrate"), "{err}");
    assert_empty("target after an outdated source", dst.store.raw()).await;
    let err = copy_database(&old.url, &never_url, &mut |_, _| {})
        .await
        .unwrap_err();
    assert!(matches!(err, crate::Error::SchemaOutdated { .. }), "{err}");
    assert!(
        !never.exists(),
        "the target database file was not even created"
    );
    old.cleanup().await;

    // newer
    let new = Env::create(Target::SqliteFile).await;
    new.store.set_setting("keep", "me", None).await.unwrap();
    crate::test_record_schema_version(&new.url, 99)
        .await
        .unwrap();
    let err = copy_database(&new.url, &dst.url, &mut |_, _| {})
        .await
        .unwrap_err();
    assert!(
        matches!(err, crate::Error::SchemaTooNew { db: 99, .. }),
        "{err}"
    );
    assert!(err.to_string().contains("newer"), "{err}");
    assert_empty("target after a newer source", dst.store.raw()).await;
    let err = copy_database(&new.url, &never_url, &mut |_, _| {})
        .await
        .unwrap_err();
    assert!(matches!(err, crate::Error::SchemaTooNew { .. }), "{err}");
    assert!(!never.exists(), "the target database file was not created");
    // the source keeps its row
    assert_eq!(counts(new.store.raw()).await[2], 1);
    new.cleanup().await;
    dst.cleanup().await;
}

// ---------------------------------------------------------------- C06
copy_case!(c06_a_row_the_target_refuses_rolls_everything_back, c06_body);
async fn c06_body(src: &Env, dst: &Env) {
    let expected = seed(src.store.raw()).await;
    let src_rows = dump(src.store.raw()).await;
    let bad_session = expected[6][2][0].clone(); // the third session, the last row of the last table
    let Val::I(Some(bad_id)) = bad_session else {
        panic!()
    };
    let first_session = match expected[6][0][0] {
        Val::I(Some(n)) => n,
        _ => panic!(),
    };
    let server_target = dst.store.backend() != BackendKind::Sqlite;
    if server_target {
        // the server refuses a text longer than the column (SQLite, the source, has no limit)
        src.store
            .raw()
            .execute(
                "UPDATE sessions SET user_agent = ? WHERE id = ?",
                &[s(&"x".repeat(300)), Val::I(Some(bad_id))],
            )
            .await
            .unwrap();
    } else {
        // SQLite has no column limits: the target refuses with a trigger
        dst.store
            .raw()
            .execute(
                &format!(
                    "CREATE TRIGGER refuse_a_session BEFORE INSERT ON sessions WHEN NEW.id = {bad_id} \
                     BEGIN SELECT RAISE(ABORT, 'refused by the target'); END"
                ),
                &[],
            )
            .await
            .unwrap();
    }
    let err = run_copy(src, dst)
        .await
        .expect_err("the target refuses a row");
    let msg = err.to_string();
    assert!(
        msg.contains("copying table sessions") && msg.contains(&format!("from id {first_session}")),
        "the message names the table and the id: {msg}"
    );
    assert_empty("target after the refused copy", dst.store.raw()).await;
    // the six tables copied before it were rolled back too: the next try starts from nothing
    if server_target {
        src.store
            .raw()
            .execute(
                "UPDATE sessions SET user_agent = ? WHERE id = ?",
                &[s("fixed"), Val::I(Some(bad_id))],
            )
            .await
            .unwrap();
    } else {
        dst.store
            .raw()
            .execute("DROP TRIGGER refuse_a_session", &[])
            .await
            .unwrap();
    }
    let rep = run_copy(src, dst)
        .await
        .expect("the retry works on the same target");
    assert_eq!(
        rep.tables.iter().map(|(_, n)| *n).sum::<u64>(),
        expected.iter().map(|r| r.len() as u64).sum::<u64>()
    );
    let now_src = dump(src.store.raw()).await;
    assert_tables(
        "target after the retry",
        &dump(dst.store.raw()).await,
        &now_src,
    );
    // only the user agent we changed on the server source differs from before
    if !server_target {
        assert_tables("source unchanged", &now_src, &src_rows);
    }
}

// ---------------------------------------------------------------- C10
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn c10_batch_boundaries_copy_every_row_in_order() {
    let insert = batch_rows(FAXES.cols.len()) as i64;
    let page = i64::from(READ_PAGE);
    let del_batch = batch_rows(DELIVERIES.cols.len()) as i64;
    assert_eq!(
        (insert, del_batch),
        (55, 100),
        "the boundaries below were chosen for these sizes"
    );
    let sizes = [
        0,
        1,
        insert - 1,
        insert,
        insert + 1,
        2 * insert,
        page - 1,
        page,
        page + 1,
        2 * page + 1,
        5000,
    ];
    for n in sizes {
        let src = Env::create(Target::SqliteFile).await;
        let dst = Env::create(Target::SqliteFile).await;
        let mut faxes = Vec::new();
        let mut deliveries = Vec::new();
        for k in 1..=n {
            let mut f = fax_base(&format!("bulk-{k:05}"));
            set(&mut f, &FAXES, "ani", s(&format!("02-{k:06}")));
            set(&mut f, &FAXES, "pages", i(k % 7));
            set(&mut f, &FAXES, "note", s(&format!("備註 {k} 📝")));
            set(&mut f, &FAXES, "started_at", tm(k * 1000 + k % 1000));
            set(&mut f, &FAXES, "is_read", b(k % 3 == 0));
            faxes.push((true, f));
            deliveries.push((
                true,
                vec![
                    i(k),
                    s(&format!("folder:/srv/{k}")),
                    s("folder"),
                    i(1 + k % 3),
                    s("done"),
                    ns(),
                    nt(),
                    tm(k),
                    tm(k + 1),
                ],
            ));
        }
        let want_f = put(src.store.raw(), &FAXES, &faxes).await;
        let want_d = put(src.store.raw(), &DELIVERIES, &deliveries).await;
        let rep = run_copy(&src, &dst)
            .await
            .unwrap_or_else(|e| panic!("{n} faxes: {e}"));
        assert_eq!(rep.tables[0], ("faxes", n as u64));
        assert_eq!(rep.tables[1], ("delivery_attempts", n as u64));
        let got = dump(dst.store.raw()).await;
        assert_eq!(got[0].len() as i64, n, "{n} faxes copied");
        assert_eq!(got[1].len() as i64, n, "{n} deliveries copied");
        assert_tables(
            &format!("{n} faxes: target against source"),
            &got,
            &dump(src.store.raw()).await,
        );
        // order and content against what was written (ids 1..n, no gaps, none twice)
        for (k, (g, w)) in got[0].iter().zip(&want_f).enumerate() {
            assert_eq!(g, w, "{n} faxes: row {k}");
        }
        for (k, (g, w)) in got[1].iter().zip(&want_d).enumerate() {
            assert_eq!(g, w, "{n} deliveries: row {k}");
        }
        if n > 0 {
            assert_eq!(got[0].last().unwrap()[0], i(n));
        }
        src.cleanup().await;
        dst.cleanup().await;
    }
}

// ---------------------------------------------------------------- C13
db_case!(
    c13_the_copy_lists_exactly_the_columns_of_the_schema,
    c13_body
);
async fn c13_body(env: &Env) {
    let be = env.store.raw();
    // (table, column) pairs of the live schema
    let mut live: BTreeSet<(String, String)> = BTreeSet::new();
    let rows = match env.store.backend() {
        BackendKind::Sqlite => {
            let tables = be
                .query(
                    "SELECT name FROM sqlite_master WHERE type = 'table' AND name NOT LIKE 'sqlite_%'",
                    &[],
                    &[S],
                )
                .await
                .unwrap();
            let mut all = Vec::new();
            for t in tables {
                let Val::S(Some(table)) = &t[0] else { panic!() };
                let cols = be.query(&format!("PRAGMA table_info({table})"), &[], &[I, S, S, I, S, I]).await.unwrap();
                for c in cols {
                    all.push(vec![s(table), c[1].clone()]);
                }
            }
            all
        }
        BackendKind::Postgres => be
            .query(
                "SELECT table_name, column_name FROM information_schema.columns WHERE table_schema = current_schema()",
                &[],
                &[S, S],
            )
            .await
            .unwrap(),
        BackendKind::Mysql => be
            .query(
                "SELECT CAST(table_name AS CHAR), CAST(column_name AS CHAR) FROM information_schema.columns WHERE table_schema = DATABASE()",
                &[],
                &[S, S],
            )
            .await
            .unwrap(),
        BackendKind::Mssql => be
            .query(
                "SELECT table_name, column_name FROM information_schema.columns WHERE table_schema = SCHEMA_NAME()",
                &[],
                &[S, S],
            )
            .await
            .unwrap(),
    };
    for r in rows {
        let (Val::S(Some(t)), Val::S(Some(c))) = (&r[0], &r[1]) else {
            panic!("{r:?}")
        };
        if t != "schema_migrations" {
            live.insert((t.to_lowercase(), c.to_lowercase()));
        }
    }
    let live_tables: BTreeSet<&str> = live.iter().map(|(t, _)| t.as_str()).collect();
    let listed: BTreeSet<&str> = table_names().into_iter().collect();
    assert_eq!(
        live_tables, listed,
        "[{}] the tables of the schema and of the copy",
        env.label
    );
    let from_copy: BTreeSet<(String, String)> = TABLES
        .iter()
        .flat_map(|t| {
            t.cols
                .iter()
                .map(move |(c, _)| (t.name.to_string(), (*c).to_string()))
        })
        .collect();
    assert_eq!(
        live, from_copy,
        "[{}] the columns of the schema and of the copy",
        env.label
    );
    let from_test: BTreeSet<(String, String)> = SPECS
        .iter()
        .flat_map(|t| {
            t.cols
                .iter()
                .map(move |(c, _)| (t.name.to_string(), (*c).to_string()))
        })
        .collect();
    assert_eq!(
        live, from_test,
        "[{}] the columns of the schema and of the test data",
        env.label
    );
    // the copy's kinds and the test's agree, and the key comes first
    for (a, b2) in TABLES.iter().zip(SPECS) {
        assert_eq!(a.name, b2.name);
        assert_eq!(a.cols, b2.cols, "kinds and order of {}", a.name);
        assert_eq!(a.identity, b2.identity);
    }
}
