//! Thin backend abstraction. Every backend only has to run parameterised SQL (written with `?`
//! placeholders) and decode rows by declared column kind; all store logic lives once in
//! [`crate::sqlstore`]. This is what lets one behaviour suite cover four databases.

use crate::error::{Error, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// Which database family a store talks to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendKind {
    /// SQLite (bundled).
    Sqlite,
    /// PostgreSQL.
    Postgres,
    /// MySQL or MariaDB.
    Mysql,
    /// Microsoft SQL Server.
    Mssql,
}

impl BackendKind {
    /// Short name (also the migrations directory).
    pub fn name(&self) -> &'static str {
        match self {
            BackendKind::Sqlite => "sqlite",
            BackendKind::Postgres => "postgres",
            BackendKind::Mysql => "mysql",
            BackendKind::Mssql => "mssql",
        }
    }
}

/// A bound parameter or a decoded value. Every variant is nullable so NULLs keep their type
/// (PostgreSQL needs typed parameters).
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    /// Integer.
    I(Option<i64>),
    /// Text.
    S(Option<String>),
    /// Boolean.
    B(Option<bool>),
    /// Timestamp (UTC, millisecond precision).
    T(Option<DateTime<Utc>>),
}

impl From<i64> for Val {
    fn from(v: i64) -> Self {
        Val::I(Some(v))
    }
}
impl From<i32> for Val {
    fn from(v: i32) -> Self {
        Val::I(Some(v.into()))
    }
}
impl From<Option<i64>> for Val {
    fn from(v: Option<i64>) -> Self {
        Val::I(v)
    }
}
impl From<Option<i32>> for Val {
    fn from(v: Option<i32>) -> Self {
        Val::I(v.map(Into::into))
    }
}
impl From<&str> for Val {
    fn from(v: &str) -> Self {
        Val::S(Some(v.to_string()))
    }
}
impl From<String> for Val {
    fn from(v: String) -> Self {
        Val::S(Some(v))
    }
}
impl From<Option<String>> for Val {
    fn from(v: Option<String>) -> Self {
        Val::S(v)
    }
}
impl From<Option<&str>> for Val {
    fn from(v: Option<&str>) -> Self {
        Val::S(v.map(str::to_string))
    }
}
impl From<bool> for Val {
    fn from(v: bool) -> Self {
        Val::B(Some(v))
    }
}
impl From<DateTime<Utc>> for Val {
    fn from(v: DateTime<Utc>) -> Self {
        Val::T(Some(v))
    }
}
impl From<Option<DateTime<Utc>>> for Val {
    fn from(v: Option<DateTime<Utc>>) -> Self {
        Val::T(v)
    }
}

/// Declared type of a result column, used by backends to decode.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// Any integer width.
    Int,
    /// Text.
    Str,
    /// Boolean.
    Bool,
    /// Timestamp.
    Time,
}

/// Sequential reader over one decoded row.
pub struct RowReader {
    vals: std::vec::IntoIter<Val>,
}

impl RowReader {
    pub(crate) fn new(vals: Vec<Val>) -> Self {
        Self {
            vals: vals.into_iter(),
        }
    }
    fn next(&mut self) -> Val {
        self.vals.next().expect("column count matches kinds")
    }
    pub(crate) fn int(&mut self) -> Option<i64> {
        match self.next() {
            Val::I(v) => v,
            other => panic!("expected int, got {other:?}"),
        }
    }
    pub(crate) fn i64(&mut self) -> i64 {
        self.int().expect("non-null integer column")
    }
    pub(crate) fn i32(&mut self) -> i32 {
        self.i64() as i32
    }
    pub(crate) fn opt_i32(&mut self) -> Option<i32> {
        self.int().map(|v| v as i32)
    }
    pub(crate) fn opt_str(&mut self) -> Option<String> {
        match self.next() {
            Val::S(v) => v,
            other => panic!("expected str, got {other:?}"),
        }
    }
    pub(crate) fn str(&mut self) -> String {
        self.opt_str().expect("non-null text column")
    }
    pub(crate) fn bool(&mut self) -> bool {
        match self.next() {
            Val::B(v) => v.expect("non-null bool column"),
            other => panic!("expected bool, got {other:?}"),
        }
    }
    pub(crate) fn opt_time(&mut self) -> Option<DateTime<Utc>> {
        match self.next() {
            Val::T(v) => v,
            other => panic!("expected time, got {other:?}"),
        }
    }
    pub(crate) fn time(&mut self) -> DateTime<Utc> {
        self.opt_time().expect("non-null time column")
    }
}

/// One dedicated connection that holds the cross-process migration lock (`pg_advisory_lock`,
/// `GET_LOCK`, `sp_getapplock`, or SQLite's exclusive transaction). Everything the migration does
/// goes through it, so the lock and the statements share one session.
#[async_trait]
pub trait MigrationSession: Send {
    /// Run a statement on the locked connection.
    async fn execute(&mut self, sql: &str, params: &[Val]) -> Result<u64>;
    /// Run a query on the locked connection.
    async fn query(&mut self, sql: &str, params: &[Val], kinds: &[Kind]) -> Result<Vec<Vec<Val>>>;
    /// True when DDL can be rolled back (SQLite, PostgreSQL, MSSQL); false for MySQL/MariaDB,
    /// whose migrations therefore have to be safe to run again.
    fn transactional_ddl(&self) -> bool;
    /// Start a transaction around one migration (a no-op where the whole session is one).
    async fn begin(&mut self) -> Result<()>;
    /// Commit it.
    async fn commit(&mut self) -> Result<()>;
    /// Roll it back.
    async fn rollback(&mut self) -> Result<()>;
    /// Release the lock and give the connection back. `ok = false` rolls back whatever is open.
    async fn finish(self: Box<Self>, ok: bool) -> Result<()>;
}

/// What a database driver must provide.
#[async_trait]
pub trait Backend: Send + Sync {
    /// Wait for the migration lock (up to a minute) on a dedicated connection and return it.
    async fn migration_session(&self) -> Result<Box<dyn MigrationSession>>;
    /// Database family.
    fn kind(&self) -> BackendKind;
    /// Run a statement, returning affected rows. `sql` uses `?` placeholders.
    async fn execute(&self, sql: &str, params: &[Val]) -> Result<u64>;
    /// Run a query and decode every row according to `kinds`.
    async fn query(&self, sql: &str, params: &[Val], kinds: &[Kind]) -> Result<Vec<Vec<Val>>>;
    /// Run an `INSERT INTO t (...) VALUES (...)` into a table with an `id` identity column and
    /// return the generated id. The driver adds `RETURNING` / `OUTPUT` as needed.
    async fn insert_id(&self, sql: &str, params: &[Val]) -> Result<i64>;
    /// Close the pool.
    async fn close(&self);
}

/// Rewrite `?` placeholders for a backend. `style` is called with the 1-based index.
pub(crate) fn rewrite_placeholders(sql: &str, style: impl Fn(usize) -> String) -> String {
    let mut out = String::with_capacity(sql.len() + 8);
    let mut n = 0;
    for ch in sql.chars() {
        if ch == '?' {
            n += 1;
            out.push_str(&style(n));
        } else {
            out.push(ch);
        }
    }
    out
}

/// Split a migration file into statements. Migration files must not contain `;` inside
/// literals or comments.
pub(crate) fn split_statements(sql: &str) -> Vec<String> {
    sql.split(';')
        .map(|s| {
            s.lines()
                .filter(|l| !l.trim_start().starts_with("--"))
                .collect::<Vec<_>>()
                .join("\n")
                .trim()
                .to_string()
        })
        .filter(|s| !s.is_empty())
        .collect()
}

pub(crate) fn decode_err(what: impl std::fmt::Display) -> Error {
    Error::Backend(format!("decode: {what}"))
}

pub(crate) const TS_FORMAT: &str = "%Y-%m-%dT%H:%M:%S%.3fZ";

/// Parse the SQLite text timestamp.
pub(crate) fn parse_ts(s: &str) -> Result<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&Utc))
        .map_err(|e| Error::Corrupt(format!("timestamp {s:?}: {e}")))
}
