//! Microsoft SQL Server driver: tiberius (rustls) behind a small bb8 pool.

use crate::db::*;
use crate::error::{Error, Result};
use async_trait::async_trait;
use bb8::{ManageConnection, Pool, RunError};
use std::time::Duration;
use tiberius::{AuthMethod, Client, Config, EncryptionLevel, Query};
use tokio::net::TcpStream;
use tokio_util::compat::{Compat, TokioAsyncWriteCompatExt};

type Conn = Client<Compat<TcpStream>>;

struct Manager {
    config: Config,
}

impl ManageConnection for Manager {
    type Connection = Conn;
    type Error = tiberius::error::Error;

    async fn connect(&self) -> std::result::Result<Conn, Self::Error> {
        let tcp = TcpStream::connect(self.config.get_addr()).await?;
        tcp.set_nodelay(true)?;
        Client::connect(self.config.clone(), tcp.compat_write()).await
    }

    async fn is_valid(&self, conn: &mut Conn) -> std::result::Result<(), Self::Error> {
        conn.simple_query("SELECT 1").await?.into_row().await?;
        Ok(())
    }

    fn has_broken(&self, _conn: &mut Conn) -> bool {
        false
    }
}

pub(crate) struct Mssql {
    pool: Pool<Manager>,
}

/// `mssql://user:pass@host[:port]/database?encrypt=on|off|required|not_supported&trust_server_certificate=true`
fn parse_config(url: &str) -> Result<Config> {
    let u = url::Url::parse(url).map_err(|e| Error::BadUrl(e.to_string()))?;
    let mut c = Config::new();
    c.host(
        u.host_str()
            .ok_or_else(|| Error::BadUrl("missing host".into()))?,
    );
    c.port(u.port().unwrap_or(1433));
    let user = percent_decode(u.username());
    let pass = percent_decode(u.password().unwrap_or(""));
    c.authentication(AuthMethod::sql_server(user, pass));
    let db = u.path().trim_start_matches('/');
    if !db.is_empty() {
        c.database(percent_decode(db));
    }
    // Default: encrypt the whole connection and verify the certificate (like every other backend).
    c.encryption(EncryptionLevel::Required);
    for (k, v) in u.query_pairs() {
        match k.as_ref() {
            "encrypt" => {
                c.encryption(match v.as_ref() {
                    "off" | "false" => EncryptionLevel::Off,
                    "on" | "true" => EncryptionLevel::On,
                    "required" => EncryptionLevel::Required,
                    "not_supported" => EncryptionLevel::NotSupported,
                    other => return Err(Error::BadUrl(format!("bad encrypt={other}"))),
                });
            }
            "trust_server_certificate" | "trust_cert" => {
                if matches!(v.as_ref(), "true" | "1" | "yes") {
                    c.trust_cert();
                }
            }
            "application_name" => c.application_name(v.to_string()),
            other => return Err(Error::BadUrl(format!("unknown mssql option {other}"))),
        }
    }
    Ok(c)
}

fn percent_decode(s: &str) -> String {
    percent_encoding::percent_decode_str(s)
        .decode_utf8_lossy()
        .into_owned()
}

fn map_run(e: RunError<tiberius::error::Error>) -> Error {
    match e {
        RunError::TimedOut => Error::Unavailable("mssql pool timed out".into()),
        RunError::User(e) => Error::from_tiberius(e),
    }
}

impl Mssql {
    pub(crate) async fn connect(url: &str, max_connections: u32) -> Result<Self> {
        let config = parse_config(url)?;
        let pool = Pool::builder()
            .max_size(max_connections.max(1))
            .min_idle(Some(1))
            .connection_timeout(Duration::from_secs(10))
            .build(Manager { config })
            .await
            .map_err(Error::from_tiberius)?;
        Ok(Self { pool })
    }

    fn make_query<'a>(sql: &'a str, params: &'a [Val]) -> Query<'a> {
        let mut q = Query::new(sql);
        for p in params {
            match p {
                Val::I(v) => q.bind(*v),
                Val::S(v) => q.bind(v.clone()),
                Val::B(v) => q.bind(*v),
                Val::T(v) => q.bind(v.map(|t| t.naive_utc())),
            }
        }
        q
    }
}

fn int(row: &tiberius::Row, i: usize) -> Result<Option<i64>> {
    if let Ok(v) = row.try_get::<i64, _>(i) {
        return Ok(v);
    }
    if let Ok(v) = row.try_get::<i32, _>(i) {
        return Ok(v.map(i64::from));
    }
    if let Ok(v) = row.try_get::<i16, _>(i) {
        return Ok(v.map(i64::from));
    }
    row.try_get::<u8, _>(i)
        .map(|v| v.map(i64::from))
        .map_err(decode_err)
}

fn decode(rows: Vec<tiberius::Row>, kinds: &[Kind]) -> Result<Vec<Vec<Val>>> {
    let mut out = Vec::with_capacity(rows.len());
    for row in rows {
        let mut vals = Vec::with_capacity(kinds.len());
        for (i, k) in kinds.iter().enumerate() {
            vals.push(match k {
                Kind::Int => Val::I(int(&row, i)?),
                Kind::Bool => Val::B(row.try_get::<bool, _>(i).map_err(decode_err)?),
                Kind::Str => Val::S(
                    row.try_get::<&str, _>(i)
                        .map_err(decode_err)?
                        .map(str::to_string),
                ),
                Kind::Time => Val::T(
                    row.try_get::<chrono::NaiveDateTime, _>(i)
                        .map_err(decode_err)?
                        .map(|n| n.and_utc()),
                ),
            });
        }
        out.push(vals);
    }
    Ok(out)
}

/// Application lock resource of the migration (scoped to the database).
const LOCK_RESOURCE: &str = "sipfaxd_migrate";
/// Milliseconds to wait for it.
const LOCK_WAIT_MS: u32 = 60_000;

struct MssqlSession {
    conn: bb8::PooledConnection<'static, Manager>,
}

#[async_trait]
impl MigrationSession for MssqlSession {
    async fn execute(&mut self, sql: &str, params: &[Val]) -> Result<u64> {
        let sql = rewrite_placeholders(sql, |n| format!("@P{n}"));
        let res = Mssql::make_query(&sql, params)
            .execute(&mut *self.conn)
            .await
            .map_err(Error::from_tiberius)?;
        Ok(res.total())
    }
    async fn query(&mut self, sql: &str, params: &[Val], kinds: &[Kind]) -> Result<Vec<Vec<Val>>> {
        let sql = rewrite_placeholders(sql, |n| format!("@P{n}"));
        let rows = Mssql::make_query(&sql, params)
            .query(&mut *self.conn)
            .await
            .map_err(Error::from_tiberius)?
            .into_first_result()
            .await
            .map_err(Error::from_tiberius)?;
        decode(rows, kinds)
    }
    fn transactional_ddl(&self) -> bool {
        true
    }
    async fn begin(&mut self) -> Result<()> {
        self.conn
            .simple_query("BEGIN TRANSACTION")
            .await
            .map_err(Error::from_tiberius)?
            .into_results()
            .await
            .map_err(Error::from_tiberius)?;
        Ok(())
    }
    async fn commit(&mut self) -> Result<()> {
        self.conn
            .simple_query("COMMIT TRANSACTION")
            .await
            .map_err(Error::from_tiberius)?
            .into_results()
            .await
            .map_err(Error::from_tiberius)?;
        Ok(())
    }
    async fn rollback(&mut self) -> Result<()> {
        self.conn
            .simple_query("IF @@TRANCOUNT > 0 ROLLBACK TRANSACTION")
            .await
            .map_err(Error::from_tiberius)?
            .into_results()
            .await
            .map_err(Error::from_tiberius)?;
        Ok(())
    }
    async fn finish(mut self: Box<Self>, ok: bool) -> Result<()> {
        if !ok {
            let _ = self.rollback().await;
        }
        self.conn
            .simple_query(format!(
                "EXEC sp_releaseapplock @Resource = N'{LOCK_RESOURCE}', @LockOwner = 'Session'"
            ))
            .await
            .map_err(Error::from_tiberius)?
            .into_results()
            .await
            .map_err(Error::from_tiberius)?;
        Ok(())
    }
}

#[async_trait]
impl Backend for Mssql {
    async fn migration_session(&self) -> Result<Box<dyn MigrationSession>> {
        let mut conn = self.pool.get_owned().await.map_err(map_run)?;
        let rows = conn
            .simple_query(format!(
                "DECLARE @r INT; EXEC @r = sp_getapplock @Resource = N'{LOCK_RESOURCE}', @LockMode = 'Exclusive', @LockOwner = 'Session', @LockTimeout = {LOCK_WAIT_MS}; SELECT @r"
            ))
            .await
            .map_err(Error::from_tiberius)?
            .into_first_result()
            .await
            .map_err(Error::from_tiberius)?;
        let status = rows
            .first()
            .and_then(|r| r.try_get::<i32, _>(0).ok().flatten())
            .unwrap_or(-999);
        if status < 0 {
            return Err(Error::Unavailable(format!(
                "could not get the migration lock within {} s (sp_getapplock returned {status}; another sip-faxd is migrating?)",
                LOCK_WAIT_MS / 1000
            )));
        }
        Ok(Box::new(MssqlSession { conn }))
    }

    fn kind(&self) -> BackendKind {
        BackendKind::Mssql
    }

    async fn execute(&self, sql: &str, params: &[Val]) -> Result<u64> {
        let sql = rewrite_placeholders(sql, |n| format!("@P{n}"));
        let mut conn = self.pool.get().await.map_err(map_run)?;
        let res = Self::make_query(&sql, params)
            .execute(&mut *conn)
            .await
            .map_err(Error::from_tiberius)?;
        Ok(res.total())
    }

    async fn query(&self, sql: &str, params: &[Val], kinds: &[Kind]) -> Result<Vec<Vec<Val>>> {
        let sql = rewrite_placeholders(sql, |n| format!("@P{n}"));
        let mut conn = self.pool.get().await.map_err(map_run)?;
        let rows = Self::make_query(&sql, params)
            .query(&mut *conn)
            .await
            .map_err(Error::from_tiberius)?
            .into_first_result()
            .await
            .map_err(Error::from_tiberius)?;
        decode(rows, kinds)
    }

    async fn insert_id(&self, sql: &str, params: &[Val]) -> Result<i64> {
        // INSERT INTO t (cols) VALUES (...)  ->  INSERT INTO t (cols) OUTPUT INSERTED.id VALUES (...)
        let sql = sql.replacen(") VALUES", ") OUTPUT INSERTED.id VALUES", 1);
        let rows = self.query(&sql, params, &[Kind::Int]).await?;
        match rows.first().and_then(|r| r.first()) {
            Some(Val::I(Some(id))) => Ok(*id),
            _ => Err(Error::Backend("insert returned no id".into())),
        }
    }

    async fn close(&self) {}
}

#[cfg(test)]
mod tests;
