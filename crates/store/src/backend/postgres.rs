//! PostgreSQL driver (sqlx + rustls).

use crate::db::*;
use crate::error::{Error, Result};
use async_trait::async_trait;
use chrono::{DateTime, Utc};
use sqlx::postgres::{PgConnectOptions, PgPoolOptions, PgRow};
use sqlx::{PgPool, Row};
use std::str::FromStr;
use std::time::Duration;

pub(crate) struct Postgres {
    pool: PgPool,
}

impl Postgres {
    pub(crate) async fn connect(url: &str, max_connections: u32) -> Result<Self> {
        let opts = PgConnectOptions::from_str(url).map_err(|e| Error::BadUrl(e.to_string()))?;
        let pool = PgPoolOptions::new()
            .max_connections(max_connections.max(1))
            .acquire_timeout(Duration::from_secs(10))
            .connect_with(opts)
            .await
            .map_err(Error::from_sqlx)?;
        Ok(Self { pool })
    }
}

type Q = sqlx::query::Query<'static, sqlx::Postgres, sqlx::postgres::PgArguments>;

fn bind(q: Q, p: &Val) -> Q {
    match p {
        Val::I(v) => q.bind(*v),
        Val::S(v) => q.bind(v.clone()),
        Val::B(v) => q.bind(*v),
        Val::T(v) => q.bind(*v),
    }
}

fn int(row: &PgRow, i: usize) -> Result<Option<i64>> {
    if let Ok(v) = row.try_get::<Option<i64>, _>(i) {
        return Ok(v);
    }
    if let Ok(v) = row.try_get::<Option<i32>, _>(i) {
        return Ok(v.map(i64::from));
    }
    row.try_get::<Option<i16>, _>(i)
        .map(|v| v.map(i64::from))
        .map_err(decode_err)
}

fn rewrite(sql: &str) -> String {
    rewrite_placeholders(sql, |n| format!("${n}"))
}

fn decode(rows: Vec<PgRow>, kinds: &[Kind]) -> Result<Vec<Vec<Val>>> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let mut vals = Vec::with_capacity(kinds.len());
        for (i, k) in kinds.iter().enumerate() {
            vals.push(match k {
                Kind::Int => Val::I(int(&row, i)?),
                Kind::Bool => Val::B(row.try_get::<Option<bool>, _>(i).map_err(decode_err)?),
                Kind::Str => Val::S(row.try_get::<Option<String>, _>(i).map_err(decode_err)?),
                Kind::Time => Val::T(
                    row.try_get::<Option<DateTime<Utc>>, _>(i)
                        .map_err(decode_err)?,
                ),
            });
        }
        out.push(vals);
    }
    Ok(out)
}

/// Advisory lock key shared by every sip-faxd migrating this database ("SIPFAX").
const LOCK_KEY: i64 = 0x5349_5046_4158;

struct PgSession {
    conn: sqlx::pool::PoolConnection<sqlx::Postgres>,
}

#[async_trait]
impl MigrationSession for PgSession {
    async fn execute(&mut self, sql: &str, params: &[Val]) -> Result<u64> {
        let sql = rewrite(sql);
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
        for p in params {
            q = bind(q, p);
        }
        Ok(q.execute(&mut *self.conn)
            .await
            .map_err(Error::from_sqlx)?
            .rows_affected())
    }
    async fn query(&mut self, sql: &str, params: &[Val], kinds: &[Kind]) -> Result<Vec<Vec<Val>>> {
        let sql = rewrite(sql);
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql));
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
        true
    }
    async fn begin(&mut self) -> Result<()> {
        self.execute("BEGIN", &[]).await.map(|_| ())
    }
    async fn commit(&mut self) -> Result<()> {
        self.execute("COMMIT", &[]).await.map(|_| ())
    }
    async fn rollback(&mut self) -> Result<()> {
        self.execute("ROLLBACK", &[]).await.map(|_| ())
    }
    async fn finish(mut self: Box<Self>, ok: bool) -> Result<()> {
        if !ok {
            // a failed statement leaves the transaction aborted; the unlock needs a clean session
            let _ = self.rollback().await;
        }
        sqlx::query("SELECT pg_advisory_unlock($1)")
            .bind(LOCK_KEY)
            .fetch_one(&mut *self.conn)
            .await
            .map_err(Error::from_sqlx)?;
        Ok(())
    }
}

#[async_trait]
impl Backend for Postgres {
    async fn migration_session(&self) -> Result<Box<dyn MigrationSession>> {
        let mut conn = self.pool.acquire().await.map_err(Error::from_sqlx)?;
        // blocks until the other migrating instance has released it; the lock belongs to this
        // session, so a crashed process cannot leave it behind
        sqlx::query("SELECT pg_advisory_lock($1)")
            .bind(LOCK_KEY)
            .fetch_one(&mut *conn)
            .await
            .map_err(Error::from_sqlx)?;
        Ok(Box::new(PgSession { conn }))
    }

    fn kind(&self) -> BackendKind {
        BackendKind::Postgres
    }

    async fn execute(&self, sql: &str, params: &[Val]) -> Result<u64> {
        let sql = rewrite(sql);
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
        let sql = rewrite(sql);
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.to_string()));
        for p in params {
            q = bind(q, p);
        }
        let rows = q.fetch_all(&self.pool).await.map_err(Error::from_sqlx)?;
        decode(rows, kinds)
    }

    async fn insert_id(&self, sql: &str, params: &[Val]) -> Result<i64> {
        let sql = format!("{} RETURNING id", rewrite(sql));
        let mut q = sqlx::query(sqlx::AssertSqlSafe(sql.to_string()));
        for p in params {
            q = bind(q, p);
        }
        let row = q.fetch_one(&self.pool).await.map_err(Error::from_sqlx)?;
        row.try_get::<i64, _>(0).map_err(decode_err)
    }

    async fn close(&self) {
        self.pool.close().await;
    }
}
