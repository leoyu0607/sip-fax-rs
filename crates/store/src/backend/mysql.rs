//! MySQL / MariaDB driver (sqlx + rustls).

use crate::db::*;
use crate::error::{Error, Result};
use async_trait::async_trait;
use chrono::NaiveDateTime;
use sqlx::mysql::{MySqlConnectOptions, MySqlPoolOptions, MySqlRow};
use sqlx::{MySqlPool, Row};
use std::str::FromStr;
use std::time::Duration;

pub(crate) struct Mysql {
    pool: MySqlPool,
}

impl Mysql {
    pub(crate) async fn connect(url: &str, max_connections: u32) -> Result<Self> {
        let opts = MySqlConnectOptions::from_str(url).map_err(|e| Error::BadUrl(e.to_string()))?;
        let pool = MySqlPoolOptions::new()
            .max_connections(max_connections.max(1))
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(opts)
            .await
            .map_err(Error::from_sqlx)?;
        Ok(Self { pool })
    }
}

type Q = sqlx::query::Query<'static, sqlx::MySql, sqlx::mysql::MySqlArguments>;

fn bind(q: Q, p: &Val) -> Q {
    match p {
        Val::I(v) => q.bind(*v),
        Val::S(v) => q.bind(v.clone()),
        Val::B(v) => q.bind(*v),
        // DATETIME(3) has no time zone: always bind the UTC wall clock as a naive value so the
        // server/session TZ can never shift it.
        Val::T(v) => q.bind(v.map(|t| t.naive_utc())),
    }
}

fn int(row: &MySqlRow, i: usize) -> Result<Option<i64>> {
    if let Ok(v) = row.try_get::<Option<i64>, _>(i) {
        return Ok(v);
    }
    if let Ok(v) = row.try_get::<Option<i32>, _>(i) {
        return Ok(v.map(i64::from));
    }
    if let Ok(v) = row.try_get::<Option<i16>, _>(i) {
        return Ok(v.map(i64::from));
    }
    row.try_get::<Option<i8>, _>(i)
        .map(|v| v.map(i64::from))
        .map_err(decode_err)
}

/// Text column. Columns with a `_bin` collation (call_id) are reported by the server with the
/// binary flag, which sqlx maps to VARBINARY, so fall back to decoding the bytes as UTF-8.
fn text(row: &MySqlRow, i: usize) -> Result<Option<String>> {
    if let Ok(v) = row.try_get::<Option<String>, _>(i) {
        return Ok(v);
    }
    match row.try_get::<Option<Vec<u8>>, _>(i).map_err(decode_err)? {
        Some(b) => String::from_utf8(b).map(Some).map_err(decode_err),
        None => Ok(None),
    }
}

fn decode(rows: Vec<MySqlRow>, kinds: &[Kind]) -> Result<Vec<Vec<Val>>> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let mut vals = Vec::with_capacity(kinds.len());
        for (i, k) in kinds.iter().enumerate() {
            vals.push(match k {
                Kind::Int => Val::I(int(&row, i)?),
                Kind::Bool => Val::B(int(&row, i)?.map(|v| v != 0)),
                Kind::Str => Val::S(text(&row, i)?),
                Kind::Time => Val::T(
                    row.try_get::<Option<NaiveDateTime>, _>(i)
                        .map_err(decode_err)?
                        .map(|n| n.and_utc()),
                ),
            });
        }
        out.push(vals);
    }
    Ok(out)
}

/// Name of the user lock every migrating sip-faxd takes (the schema name is appended: lock names
/// are server wide).
const LOCK_NAME: &str = "sipfaxd_migrate";
/// Seconds to wait for it.
const LOCK_WAIT_SECS: i64 = 60;

struct MySqlSession {
    conn: sqlx::pool::PoolConnection<sqlx::MySql>,
    lock: String,
}

#[async_trait]
impl MigrationSession for MySqlSession {
    async fn execute(&mut self, sql: &str, params: &[Val]) -> Result<u64> {
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.to_string()));
        for p in params {
            q = bind(q, p);
        }
        Ok(q.execute(&mut *self.conn)
            .await
            .map_err(Error::from_sqlx)?
            .rows_affected())
    }
    async fn query(&mut self, sql: &str, params: &[Val], kinds: &[Kind]) -> Result<Vec<Vec<Val>>> {
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.to_string()));
        for p in params {
            q = bind(q, p);
        }
        let rows = q
            .fetch_all(&mut *self.conn)
            .await
            .map_err(Error::from_sqlx)?;
        decode(rows, kinds)
    }
    fn transactional_ddl(&self) -> bool {
        false
    }
    async fn begin(&mut self) -> Result<()> {
        Ok(()) // DDL commits implicitly: nothing to wrap
    }
    async fn commit(&mut self) -> Result<()> {
        Ok(())
    }
    async fn rollback(&mut self) -> Result<()> {
        Ok(())
    }
    async fn finish(mut self: Box<Self>, _ok: bool) -> Result<()> {
        sqlx::query("SELECT RELEASE_LOCK(?)")
            .bind(self.lock.clone())
            .fetch_one(&mut *self.conn)
            .await
            .map_err(Error::from_sqlx)?;
        Ok(())
    }
}

#[async_trait]
impl Backend for Mysql {
    async fn migration_session(&self) -> Result<Box<dyn MigrationSession>> {
        let mut conn = self.pool.acquire().await.map_err(Error::from_sqlx)?;
        let row = sqlx::query("SELECT CONCAT(?, ':', DATABASE())")
            .bind(LOCK_NAME)
            .fetch_one(&mut *conn)
            .await
            .map_err(Error::from_sqlx)?;
        let lock = match row.try_get::<String, _>(0) {
            Ok(s) => s,
            Err(_) => String::from_utf8_lossy(&row.try_get::<Vec<u8>, _>(0).map_err(decode_err)?)
                .into_owned(),
        };
        let got: Option<i64> = sqlx::query("SELECT GET_LOCK(?, ?)")
            .bind(lock.clone())
            .bind(LOCK_WAIT_SECS)
            .fetch_one(&mut *conn)
            .await
            .map_err(Error::from_sqlx)?
            .try_get(0)
            .map_err(decode_err)?;
        if got != Some(1) {
            return Err(Error::Unavailable(format!(
                "could not get the migration lock {lock} within {LOCK_WAIT_SECS} s (another sip-faxd is migrating?)"
            )));
        }
        Ok(Box::new(MySqlSession { conn, lock }))
    }

    fn kind(&self) -> BackendKind {
        BackendKind::Mysql
    }

    async fn execute(&self, sql: &str, params: &[Val]) -> Result<u64> {
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.to_string()));
        for p in params {
            q = bind(q, p);
        }
        Ok(q.execute(&self.pool)
            .await
            .map_err(Error::from_sqlx)?
            .rows_affected())
    }

    async fn query(&self, sql: &str, params: &[Val], kinds: &[Kind]) -> Result<Vec<Vec<Val>>> {
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.to_string()));
        for p in params {
            q = bind(q, p);
        }
        let rows = q.fetch_all(&self.pool).await.map_err(Error::from_sqlx)?;
        decode(rows, kinds)
    }

    async fn insert_id(&self, sql: &str, params: &[Val]) -> Result<i64> {
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.to_string()));
        for p in params {
            q = bind(q, p);
        }
        let res = q.execute(&self.pool).await.map_err(Error::from_sqlx)?;
        Ok(res.last_insert_id() as i64)
    }

    async fn close(&self) {
        self.pool.close().await;
    }
}
