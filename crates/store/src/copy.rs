//! `sip-faxd db copy`: every row of one database into another, empty one, possibly of another
//! kind (SQLite to MSSQL, ...). Ids are kept, so links between the tables, the URLs of the faxes
//! and the sessions stay valid (docs/test-plans/M4e.md 0, ADR-059).
//!
//! The target gets the schema first (its own migrations), then everything is written on one
//! dedicated connection (the migration session, which also holds the migration lock) inside one
//! transaction: a failure leaves the target empty.

use std::time::{Duration, Instant};

use crate::db::{Backend, BackendKind, Kind, MigrationSession, Val};
use crate::error::{Error, Result};
use crate::migrate::{current_version, latest_version};
use crate::store::FaxStore;
use crate::{ConnectOptions, open_sql};

/// One table: its columns (the first is the key), whether `id` is an identity column.
struct Table {
    name: &'static str,
    cols: &'static [(&'static str, Kind)],
    identity: bool,
}

use Kind::{Bool as B, Int as I, Str as S, Time as T};

/// Every table of the schema except `schema_migrations`, parents before children.
const TABLES: &[Table] = &[
    Table {
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
    },
    Table {
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
    },
    Table {
        name: "settings",
        identity: false,
        cols: &[
            ("name", S),
            ("value", S),
            ("updated_by", S),
            ("updated_at", T),
        ],
    },
    Table {
        name: "config_sections",
        identity: false,
        cols: &[
            ("name", S),
            ("body", S),
            ("version", I),
            ("updated_by", S),
            ("updated_at", T),
        ],
    },
    Table {
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
    },
    Table {
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
    },
    Table {
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
    },
];

/// The names of the copied tables, in copy order.
pub fn table_names() -> Vec<&'static str> {
    TABLES.iter().map(|t| t.name).collect()
}

/// Rows read from the source per query.
pub const READ_PAGE: u32 = 500;

/// Bound parameters per `INSERT` stay below this (MSSQL allows 2100).
pub const MAX_PARAMS: usize = 2000;

/// Rows per `INSERT` of a table with `cols` columns.
pub fn batch_rows(cols: usize) -> usize {
    (MAX_PARAMS / cols).clamp(1, 100)
}

/// What was copied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CopyReport {
    /// Source and target kind.
    pub from: BackendKind,
    /// Target kind.
    pub to: BackendKind,
    /// Schema version of both.
    pub schema: i32,
    /// `(table, rows)` in copy order.
    pub tables: Vec<(&'static str, u64)>,
    /// How long the copy took.
    pub elapsed: Duration,
}

/// Copies every row of `from_url` into `to_url` (see the module documentation). `progress` is
/// called after each table with its name and row count.
///
/// # Errors
/// [`Error::Invalid`] when the source is not at this binary's schema, when the URLs are the same
/// or when the target already has rows; any database error (then nothing is left in the target).
pub async fn copy_database(
    from_url: &str,
    to_url: &str,
    progress: &mut (dyn FnMut(&str, u64) + Send),
) -> Result<CopyReport> {
    if from_url.trim() == to_url.trim() {
        return Err(Error::Invalid(
            "the target is the source database (--to must name another database)".into(),
        ));
    }
    let started = Instant::now();
    let opts = ConnectOptions {
        max_connections: 2,
        ..ConnectOptions::default()
    };
    let src = open_sql(from_url, &opts).await?;
    let result = async {
        let have = current_version(src.raw()).await?;
        let want = latest_version(src.backend());
        if have > want {
            return Err(Error::SchemaTooNew {
                db: have,
                supported: want,
            });
        }
        if have < want {
            return Err(Error::SchemaOutdated {
                db: have,
                required: want,
            });
        }
        let dst = open_sql(to_url, &opts).await?;
        let r = copy_into(src.raw(), &dst, want, progress).await;
        dst.close().await;
        r
    }
    .await;
    src.close().await;
    let mut report = result?;
    report.elapsed = started.elapsed();
    Ok(report)
}

async fn copy_into(
    src: &dyn Backend,
    dst: &crate::SqlStore,
    schema: i32,
    progress: &mut (dyn FnMut(&str, u64) + Send),
) -> Result<CopyReport> {
    // the schema of this binary (a newer target is refused by the migration itself)
    dst.migrate().await?;
    for t in TABLES {
        let n = count(dst.raw(), t.name).await?;
        if n > 0 {
            return Err(Error::Invalid(format!(
                "the target is not empty: table {} has {n} rows (copy only into an empty database)",
                t.name
            )));
        }
    }
    let kind = dst.backend();
    let mut session = dst.raw().migration_session().await?;
    let copied = copy_all(src, session.as_mut(), kind, progress).await;
    let ok = copied.is_ok();
    if !ok {
        let _ = end_tx(session.as_mut(), kind, false).await;
    }
    session.finish(ok).await?;
    let tables = copied?;
    // what the target holds now, read back on another connection
    for (t, (name, n)) in TABLES.iter().zip(&tables) {
        let got = count(dst.raw(), name).await?;
        let src_max = max_key(src, t).await?;
        let dst_max = max_key(dst.raw(), t).await?;
        if got != *n || src_max != dst_max {
            return Err(Error::Backend(format!(
                "verification failed for table {name}: {got} rows in the target, {n} copied"
            )));
        }
    }
    Ok(CopyReport {
        from: src.kind(),
        to: kind,
        schema,
        tables,
        elapsed: Duration::ZERO,
    })
}

async fn copy_all(
    src: &dyn Backend,
    dst: &mut dyn MigrationSession,
    kind: BackendKind,
    progress: &mut (dyn FnMut(&str, u64) + Send),
) -> Result<Vec<(&'static str, u64)>> {
    begin_tx(dst, kind).await?;
    let mut out = Vec::new();
    for t in TABLES {
        let n = copy_table(src, dst, kind, t).await?;
        if kind == BackendKind::Postgres && t.identity && n > 0 {
            // the next generated id follows the copied ones
            dst.execute(
                &format!(
                    "SELECT setval(pg_get_serial_sequence('{0}', 'id'), (SELECT MAX(id) FROM {0}))",
                    t.name
                ),
                &[],
            )
            .await?;
        }
        progress(t.name, n);
        out.push((t.name, n));
    }
    end_tx(dst, kind, true).await?;
    Ok(out)
}

/// The transaction of the copy. SQLite's session is one exclusive transaction already; MySQL's
/// session does not open one (its DDL cannot be rolled back), so the copy opens its own, with
/// `autocommit = 0` (`START TRANSACTION` cannot be a prepared statement).
async fn begin_tx(s: &mut dyn MigrationSession, kind: BackendKind) -> Result<()> {
    match kind {
        BackendKind::Sqlite => Ok(()),
        BackendKind::Mysql => s.execute("SET autocommit = 0", &[]).await.map(|_| ()),
        BackendKind::Postgres | BackendKind::Mssql => s.begin().await,
    }
}

async fn end_tx(s: &mut dyn MigrationSession, kind: BackendKind, commit: bool) -> Result<()> {
    match (kind, commit) {
        (BackendKind::Sqlite, _) => Ok(()),
        (BackendKind::Mysql, done) => {
            s.execute(if done { "COMMIT" } else { "ROLLBACK" }, &[])
                .await?;
            s.execute("SET autocommit = 1", &[]).await.map(|_| ())
        }
        (_, true) => s.commit().await,
        (_, false) => s.rollback().await,
    }
}

async fn copy_table(
    src: &dyn Backend,
    dst: &mut dyn MigrationSession,
    kind: BackendKind,
    t: &Table,
) -> Result<u64> {
    let names: Vec<&str> = t.cols.iter().map(|(n, _)| *n).collect();
    let kinds: Vec<Kind> = t.cols.iter().map(|(_, k)| *k).collect();
    let key = names[0];
    let cols = names.join(", ");
    let (top, lim) = if src.kind() == BackendKind::Mssql {
        (format!("TOP ({READ_PAGE})"), String::new())
    } else {
        (String::new(), format!("LIMIT {READ_PAGE}"))
    };
    let batch = batch_rows(names.len());
    let overriding = if kind == BackendKind::Postgres && t.identity {
        " OVERRIDING SYSTEM VALUE"
    } else {
        ""
    };
    let row_ph = format!("({})", vec!["?"; names.len()].join(", "));
    let mut last: Option<Val> = None;
    let mut total = 0u64;
    loop {
        let (cond, params) = match &last {
            None => (String::new(), Vec::new()),
            Some(v) => (format!("WHERE {key} > ?"), vec![v.clone()]),
        };
        let rows = src
            .query(
                &format!(
                    "SELECT {top} {cols} FROM {} {cond} ORDER BY {key} {lim}",
                    t.name
                ),
                &params,
                &kinds,
            )
            .await?;
        if rows.is_empty() {
            break;
        }
        last = rows.last().and_then(|r| r.first().cloned());
        for chunk in rows.chunks(batch) {
            let mut sql = format!(
                "INSERT INTO {} ({cols}){overriding} VALUES {}",
                t.name,
                vec![row_ph.as_str(); chunk.len()].join(", ")
            );
            if kind == BackendKind::Mssql && t.identity {
                // a parameterised statement runs as its own batch (sp_executesql): the setting
                // has to be in the same batch to apply to the INSERT
                sql = format!(
                    "SET IDENTITY_INSERT {0} ON; {sql}; SET IDENTITY_INSERT {0} OFF",
                    t.name
                );
            }
            let params: Vec<Val> = chunk.iter().flatten().cloned().collect();
            dst.execute(&sql, &params).await.map_err(|e| {
                let first = chunk.first().and_then(|r| r.first());
                Error::Backend(format!(
                    "copying table {} (rows from {key} {}): {e}",
                    t.name,
                    show_key(first)
                ))
            })?;
            total += chunk.len() as u64;
        }
        if rows.len() < READ_PAGE as usize {
            break;
        }
    }
    Ok(total)
}

fn show_key(v: Option<&Val>) -> String {
    match v {
        Some(Val::I(Some(n))) => n.to_string(),
        Some(Val::S(Some(s))) => format!("{s:?}"),
        _ => "?".into(),
    }
}

async fn count(be: &dyn Backend, table: &str) -> Result<u64> {
    let rows = be
        .query(&format!("SELECT COUNT(*) FROM {table}"), &[], &[Kind::Int])
        .await?;
    match rows.first().and_then(|r| r.first()) {
        Some(Val::I(Some(n))) => Ok(*n as u64),
        _ => Ok(0),
    }
}

/// The largest key of a table with an id column (`None` for the tables keyed by name).
async fn max_key(be: &dyn Backend, t: &Table) -> Result<Option<i64>> {
    if !t.identity {
        return Ok(None);
    }
    let rows = be
        .query(
            &format!("SELECT MAX(id) FROM {}", t.name),
            &[],
            &[Kind::Int],
        )
        .await?;
    Ok(match rows.first().and_then(|r| r.first()) {
        Some(Val::I(v)) => *v,
        _ => None,
    })
}
