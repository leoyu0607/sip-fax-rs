//! SQLite driver (sqlx, bundled libsqlite3).

use crate::db::*;
use crate::error::{Error, Result};
use async_trait::async_trait;
use sqlx::sqlite::{SqliteConnectOptions, SqliteJournalMode, SqlitePoolOptions, SqliteSynchronous};
use sqlx::{Row, SqlitePool};
use std::str::FromStr;
use std::time::Duration;

pub(crate) struct Sqlite {
    pool: SqlitePool,
}

impl Sqlite {
    pub(crate) async fn connect(url: &str, max_connections: u32) -> Result<Self> {
        let opts = SqliteConnectOptions::from_str(url).map_err(|e| Error::BadUrl(e.to_string()))?;
        let memory = url.contains(":memory:") || url.contains("mode=memory");
        let mut opts = opts
            .create_if_missing(true)
            .busy_timeout(Duration::from_secs(10))
            .foreign_keys(true);
        if !memory {
            opts = opts
                .journal_mode(SqliteJournalMode::Wal)
                .synchronous(SqliteSynchronous::Normal);
        }
        // An in-memory database lives and dies with its connection(s): keep exactly one, forever.
        let mut pool = SqlitePoolOptions::new().acquire_timeout(Duration::from_secs(10));
        pool = if memory {
            pool.max_connections(1)
                .min_connections(1)
                .idle_timeout(None)
                .max_lifetime(None)
        } else {
            pool.max_connections(max_connections.max(1))
        };
        let pool = pool.connect_with(opts).await.map_err(Error::from_sqlx)?;
        Ok(Self { pool })
    }
}

type Q = sqlx::query::Query<'static, sqlx::Sqlite, sqlx::sqlite::SqliteArguments>;

fn bind(q: Q, p: &Val) -> Q {
    match p {
        Val::I(v) => q.bind(*v),
        Val::S(v) => q.bind(v.clone()),
        Val::B(v) => q.bind(*v),
        Val::T(v) => q.bind(v.map(|t| t.format(TS_FORMAT).to_string())),
    }
}

#[async_trait]
impl Backend for Sqlite {
    fn kind(&self) -> BackendKind {
        BackendKind::Sqlite
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
        let mut out = Vec::with_capacity(rows.len());
        for row in rows {
            let mut vals = Vec::with_capacity(kinds.len());
            for (i, k) in kinds.iter().enumerate() {
                // SQLite is dynamically typed: decode by the requested kind, not the declared type.
                vals.push(match k {
                    Kind::Int => Val::I(
                        row.try_get_unchecked::<Option<i64>, _>(i)
                            .map_err(decode_err)?,
                    ),
                    Kind::Bool => Val::B(
                        row.try_get_unchecked::<Option<i64>, _>(i)
                            .map_err(decode_err)?
                            .map(|v| v != 0),
                    ),
                    Kind::Str => Val::S(
                        row.try_get_unchecked::<Option<String>, _>(i)
                            .map_err(decode_err)?,
                    ),
                    Kind::Time => {
                        let s = row
                            .try_get_unchecked::<Option<String>, _>(i)
                            .map_err(decode_err)?;
                        Val::T(s.as_deref().map(parse_ts).transpose()?)
                    }
                });
            }
            out.push(vals);
        }
        Ok(out)
    }

    async fn insert_id(&self, sql: &str, params: &[Val]) -> Result<i64> {
        let sql = format!("{sql} RETURNING id");
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
