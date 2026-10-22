//! `sip-faxd db check`: connect, ask the server for its version and the schema version, change
//! nothing (ADR-060).

use std::time::{Duration, Instant};

use crate::db::{BackendKind, Kind, Val};
use crate::error::{Error, Result};
use crate::migrate::{current_version, latest_version};
use crate::store::FaxStore;
use crate::{ConnectOptions, backend_for_url, open_sql};

/// What `db check` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CheckReport {
    /// Database family.
    pub kind: BackendKind,
    /// What the server says it is (`16.4`, `8.0.39`, `16.0.1000.6`, `3.46.0`).
    pub server_version: String,
    /// Schema version of the database (0: no tables yet).
    pub schema: i32,
    /// Schema version this binary needs.
    pub required: i32,
    /// From the start of the connection to the last answer.
    pub elapsed: Duration,
}

/// Connects to `url` and reads the server and schema versions, within `limit`. A SQLite file
/// that does not exist is reported as [`Error::NotFound`] (it is not created).
///
/// # Errors
/// Whatever the connection or the queries return; [`Error::Unavailable`] after `limit`.
pub async fn check_url(url: &str, limit: Duration) -> Result<CheckReport> {
    let kind = backend_for_url(url)?;
    if kind == BackendKind::Sqlite
        && let Some(path) = sqlite_file(url)
        && !std::path::Path::new(&path).exists()
    {
        return Err(Error::NotFound(format!(
            "the SQLite file {path} does not exist (it is created at the first start)"
        )));
    }
    let started = Instant::now();
    let run = async {
        let opts = ConnectOptions {
            max_connections: 1,
            ..ConnectOptions::default()
        };
        let st = open_sql(url, &opts).await?;
        let r = async {
            let be = st.raw();
            let sql = match kind {
                BackendKind::Sqlite => "SELECT sqlite_version()",
                BackendKind::Postgres => "SHOW server_version",
                BackendKind::Mysql => "SELECT CAST(VERSION() AS CHAR)",
                BackendKind::Mssql => {
                    "SELECT CAST(SERVERPROPERTY('ProductVersion') AS NVARCHAR(128))"
                }
            };
            let rows = be.query(sql, &[], &[Kind::Str]).await?;
            let server_version = match rows.first().and_then(|r| r.first()) {
                Some(Val::S(Some(v))) => v.clone(),
                _ => "?".into(),
            };
            let schema = current_version(be).await?;
            Ok::<_, Error>((server_version, schema))
        }
        .await;
        st.close().await;
        r
    };
    let (server_version, schema) = tokio::time::timeout(limit, run)
        .await
        .map_err(|_| Error::Unavailable(format!("no answer within {} s", limit.as_secs())))??;
    Ok(CheckReport {
        kind,
        server_version,
        schema,
        required: latest_version(kind),
        elapsed: started.elapsed(),
    })
}

/// The file of a SQLite URL (`None` for an in-memory database).
fn sqlite_file(url: &str) -> Option<String> {
    let rest = url.trim_start_matches("sqlite:").trim_start_matches("//");
    let path = rest.split('?').next().unwrap_or("");
    if path.is_empty() || path.contains(":memory:") || path.starts_with("file:") {
        None
    } else {
        Some(path.to_string())
    }
}
