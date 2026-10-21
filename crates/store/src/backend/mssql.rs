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

#[async_trait]
impl Backend for Mssql {
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
mod tests {
    use super::*;

    #[test]
    fn parses_urls() {
        let c = parse_config("mssql://sa:p%40ss%21@db.example:1444/fax?encrypt=on&trust_server_certificate=true&application_name=x").unwrap();
        assert_eq!(c.get_addr(), "db.example:1444");
        assert_eq!(
            parse_config("mssql://sa:p@db/fax").unwrap().get_addr(),
            "db:1433"
        );
        for ok in ["off", "false", "on", "true", "required", "not_supported"] {
            parse_config(&format!("mssql://sa:p@db/fax?encrypt={ok}")).unwrap();
        }
        parse_config("mssql://sa:p@db").unwrap(); // default database of the login
        for bad in [
            "mssql://sa:p@db/fax?encrypt=sometimes",
            "mssql://sa:p@db/fax?nope=1",
            "mssql:///fax",
            "not a url",
        ] {
            assert!(matches!(parse_config(bad), Err(Error::BadUrl(_))), "{bad}");
        }
        assert_eq!(percent_decode("a%20b%2Fc"), "a b/c");
    }

    #[test]
    fn insert_rewrite_places_output_clause() {
        let sql = "INSERT INTO faxes (a, b) VALUES (?, ?)";
        assert_eq!(
            sql.replacen(") VALUES", ") OUTPUT INSERTED.id VALUES", 1),
            "INSERT INTO faxes (a, b) OUTPUT INSERTED.id VALUES (?, ?)"
        );
    }
}
