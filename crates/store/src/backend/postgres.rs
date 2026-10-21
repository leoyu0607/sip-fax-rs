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

#[async_trait]
impl Backend for Postgres {
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
