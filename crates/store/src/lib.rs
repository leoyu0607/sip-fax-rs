//! Fax record store: one [`FaxStore`] API over SQLite, PostgreSQL, MySQL/MariaDB and MSSQL
//! (ADR-003). See `docs/design/m3-store.md` and `docs/database.md`.
//!
//! ```no_run
//! # async fn demo() -> store::Result<()> {
//! let st = store::connect("sqlite://data/sip-faxd.db").await?; // postgres:// mysql:// mssql:// too
//! # Ok(()) }
//! ```

mod backend;
pub mod clock;
pub mod db;
pub mod error;
pub mod migrate;
pub mod model;
pub mod preview;
pub mod purge;
pub mod sqlstore;
pub mod store;

#[cfg(test)]
mod tests;

pub use clock::{Clock, ManualClock, SystemClock};
pub use db::BackendKind;
pub use error::{Error, Result};
pub use migrate::{MigrateReport, latest_version};
pub use model::*;
pub use preview::{PngKind, Preview, PreviewError};
pub use purge::{FileOps, PurgeReport, Purger, RetentionSettings, StdFileOps};
pub use sqlstore::SqlStore;
pub use store::FaxStore;

use std::sync::Arc;

/// Connection options.
#[derive(Clone)]
pub struct ConnectOptions {
    /// Pool size (SQLite in-memory always uses 1).
    pub max_connections: u32,
    /// Apply pending migrations on connect (`[database] auto_migrate`). When false, an outdated
    /// schema is an error; a newer schema is always an error.
    pub auto_migrate: bool,
    /// Time source (inject a [`ManualClock`] in tests).
    pub clock: Arc<dyn Clock>,
}

impl Default for ConnectOptions {
    fn default() -> Self {
        Self {
            max_connections: 5,
            auto_migrate: true,
            clock: Arc::new(SystemClock),
        }
    }
}

/// Which backend a URL selects.
pub fn backend_for_url(url: &str) -> Result<BackendKind> {
    let scheme = url
        .split_once(':')
        .map(|(s, _)| s.to_ascii_lowercase())
        .unwrap_or_default();
    match scheme.as_str() {
        "sqlite" => Ok(BackendKind::Sqlite),
        "postgres" | "postgresql" => Ok(BackendKind::Postgres),
        "mysql" | "mariadb" => Ok(BackendKind::Mysql),
        "mssql" | "sqlserver" => Ok(BackendKind::Mssql),
        _ => Err(Error::BadUrl(format!(
            "unsupported scheme in database url (expected sqlite:, postgres:, mysql:, mariadb: or mssql:): {:?}",
            scheme
        ))),
    }
}

/// Connect with default options (auto-migrate on).
pub async fn connect(url: &str) -> Result<Arc<dyn FaxStore>> {
    connect_with(url, ConnectOptions::default()).await
}

/// Connect, pick the backend by URL scheme and (optionally) migrate.
pub async fn connect_with(url: &str, opts: ConnectOptions) -> Result<Arc<dyn FaxStore>> {
    Ok(Arc::new(connect_sql(url, opts).await?))
}

pub(crate) async fn connect_sql(url: &str, opts: ConnectOptions) -> Result<SqlStore> {
    let kind = backend_for_url(url)?;
    let be: Box<dyn db::Backend> = match kind {
        BackendKind::Sqlite => {
            prepare_sqlite_dir(url);
            Box::new(backend::sqlite::Sqlite::connect(url, opts.max_connections).await?)
        }
        BackendKind::Postgres => {
            Box::new(backend::postgres::Postgres::connect(url, opts.max_connections).await?)
        }
        BackendKind::Mysql => {
            let url = url.replacen("mariadb:", "mysql:", 1);
            Box::new(backend::mysql::Mysql::connect(&url, opts.max_connections).await?)
        }
        BackendKind::Mssql => {
            Box::new(backend::mssql::Mssql::connect(url, opts.max_connections).await?)
        }
    };
    let store = SqlStore::new(be, opts.clock);
    if opts.auto_migrate {
        store.migrate().await?;
    } else {
        let have = store.schema_version().await?;
        let want = latest_version(kind);
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
    }
    Ok(store)
}

/// Create the parent directory of a SQLite database file so `sqlite://data/x.db` just works.
fn prepare_sqlite_dir(url: &str) {
    let rest = url.trim_start_matches("sqlite:").trim_start_matches("//");
    let path = rest.split('?').next().unwrap_or("");
    if path.is_empty() || path.contains(":memory:") || path.starts_with("file:") {
        return;
    }
    if let Some(parent) = std::path::Path::new(path).parent()
        && !parent.as_os_str().is_empty()
    {
        let _ = std::fs::create_dir_all(parent);
    }
}
