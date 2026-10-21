//! Behaviour suite. Every `s*`/`r*` case runs against each target: by default SQLite (file and
//! in-memory); with `SIP_FAXD_TEST_DB=<url>` against that server (a fresh database is created per
//! case and dropped afterwards). Optional `SIP_FAXD_TEST_DB_CONTAINER=<docker container>` enables
//! the connection-loss half of S11.

mod purge_tests;
mod store_tests;
mod unit_tests;

use crate::*;
use chrono::{DateTime, TimeZone, Utc};
use sqlx::Connection;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};

static SEQ: AtomicU32 = AtomicU32::new(0);
/// Ordinary cases hold a read lock; the outage case (stops the database container) takes the write lock.
pub static DB_LOCK: tokio::sync::RwLock<()> = tokio::sync::RwLock::const_new(());

/// UTC time from unix milliseconds.
pub fn t(ms: i64) -> DateTime<Utc> {
    Utc.timestamp_millis_opt(ms).single().unwrap()
}

/// 2026-10-21T00:00:00Z
pub const T0: i64 = 1_792_540_800_000;

#[derive(Clone, Debug)]
pub enum Target {
    SqliteFile,
    SqliteMemory,
    Server(String),
}

pub fn targets() -> Vec<Target> {
    match std::env::var("SIP_FAXD_TEST_DB") {
        Ok(url) if !url.trim().is_empty() => {
            if url.starts_with("sqlite") {
                vec![Target::SqliteFile, Target::SqliteMemory]
            } else {
                vec![Target::Server(url)]
            }
        }
        _ => vec![Target::SqliteFile, Target::SqliteMemory],
    }
}

pub struct Env {
    pub label: String,
    pub url: String,
    pub store: Arc<SqlStore>,
    pub clock: Arc<ManualClock>,
    pub dir: tempfile::TempDir,
    pub target: Target,
    admin: Option<(String, String)>, // (admin url, database name)
}

async fn admin_exec(url: &str, sql: &str) {
    match backend_for_url(url).unwrap() {
        BackendKind::Postgres => {
            let mut c = sqlx::PgConnection::connect(url)
                .await
                .expect("admin connect");
            sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_string()))
                .execute(&mut c)
                .await
                .unwrap_or_else(|e| panic!("admin `{sql}`: {e}"));
        }
        BackendKind::Mysql => {
            let url = url.replacen("mariadb:", "mysql:", 1);
            let mut c = sqlx::MySqlConnection::connect(&url)
                .await
                .expect("admin connect");
            sqlx::raw_sql(sqlx::AssertSqlSafe(sql.to_string()))
                .execute(&mut c)
                .await
                .unwrap_or_else(|e| panic!("admin `{sql}`: {e}"));
        }
        BackendKind::Mssql => {
            let be = crate::backend::mssql::Mssql::connect(url, 1)
                .await
                .expect("admin connect");
            for stmt in sql.split(";;") {
                db::Backend::execute(&be, stmt, &[])
                    .await
                    .unwrap_or_else(|e| panic!("admin `{stmt}`: {e}"));
            }
        }
        BackendKind::Sqlite => unreachable!(),
    }
}

fn with_db(url: &str, db: &str) -> String {
    let mut u = url::Url::parse(&url.replacen("mariadb:", "mysql:", 1)).unwrap();
    u.set_path(&format!("/{db}"));
    u.to_string()
}

impl Env {
    pub async fn create(target: Target) -> Env {
        let dir = tempfile::tempdir().unwrap();
        let clock = Arc::new(ManualClock::at_millis(T0));
        let (label, url, admin) = match &target {
            Target::SqliteFile => (
                "sqlite-file".to_string(),
                format!("sqlite://{}/test.db", dir.path().display()),
                None,
            ),
            Target::SqliteMemory => (
                "sqlite-memory".to_string(),
                "sqlite::memory:".to_string(),
                None,
            ),
            Target::Server(base) => {
                let kind = backend_for_url(base).unwrap();
                let name = format!(
                    "sipfax_{}_{}",
                    std::process::id(),
                    SEQ.fetch_add(1, Ordering::SeqCst)
                );
                let create = match kind {
                    BackendKind::Mysql => format!("CREATE DATABASE {name} CHARACTER SET utf8mb4"),
                    _ => format!("CREATE DATABASE {name}"),
                };
                admin_exec(base, &create).await;
                (
                    kind.name().to_string(),
                    with_db(base, &name),
                    Some((base.clone(), name)),
                )
            }
        };
        let opts = ConnectOptions {
            max_connections: 8,
            auto_migrate: true,
            clock: clock.clone(),
        };
        let store = Arc::new(
            connect_sql(&url, opts)
                .await
                .unwrap_or_else(|e| panic!("[{label}] connect: {e}")),
        );
        Env {
            label,
            url,
            store,
            clock,
            dir,
            target,
            admin,
        }
    }

    pub fn opts(&self) -> ConnectOptions {
        ConnectOptions {
            max_connections: 8,
            auto_migrate: true,
            clock: self.clock.clone(),
        }
    }

    pub fn is_memory(&self) -> bool {
        matches!(self.target, Target::SqliteMemory)
    }

    pub fn dyn_store(&self) -> Arc<dyn FaxStore> {
        self.store.clone()
    }

    pub async fn cleanup(self) {
        self.store.close().await;
        if let Some((admin, name)) = &self.admin {
            let sql = match backend_for_url(admin).unwrap() {
                BackendKind::Postgres => format!("DROP DATABASE IF EXISTS {name} WITH (FORCE)"),
                BackendKind::Mysql => format!("DROP DATABASE IF EXISTS {name}"),
                BackendKind::Mssql => format!(
                    "ALTER DATABASE {name} SET SINGLE_USER WITH ROLLBACK IMMEDIATE;;DROP DATABASE {name}"
                ),
                BackendKind::Sqlite => return,
            };
            admin_exec(admin, &sql).await;
        }
    }
}

/// Run an async case body against every target.
macro_rules! db_case {
    ($name:ident, $body:ident) => {
        #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
        async fn $name() {
            for target in $crate::tests::targets() {
                let _guard = $crate::tests::DB_LOCK.read().await;
                let env = $crate::tests::Env::create(target).await;
                eprintln!("[{}] {}", env.label, stringify!($name));
                tokio::time::timeout(std::time::Duration::from_secs(300), $body(&env))
                    .await
                    .unwrap_or_else(|_| panic!("[{}] {} timed out", env.label, stringify!($name)));
                env.cleanup().await;
            }
        }
    };
}
pub(crate) use db_case;

pub fn nf(call: &str, trunk: &str, started_ms: i64) -> NewFax {
    NewFax {
        call_id: call.to_string(),
        trunk: trunk.to_string(),
        ani: Some("0227001234".into()),
        ani_source: Some("from".into()),
        dnis: Some("025570001".into()),
        dnis_source: Some("ruri".into()),
        started_at: t(started_ms),
        answered_at: None,
    }
}

pub fn ff(ended_ms: i64, status: FaxStatus, pages: i32) -> FaxFinal {
    FaxFinal {
        ended_at: t(ended_ms),
        answered_at: None,
        duration_sec: None,
        hangup_direction: HangupDirection::Local,
        hangup_code: "completed".into(),
        hangup_text: Some("正常完成".into()),
        sip_status: None,
        t30_code: Some(0),
        fax_status: status,
        pages,
        remote_ident: Some("+886 2 2700 1234".into()),
        resolution: Some("fine".into()),
        bit_rate: Some(14400),
        compression: Some("MMR".into()),
        ecm: true,
        tiff_path: Some(format!("faxes/{ended_ms}.tif")),
        tiff_size: Some(123_456),
        pdf_path: Some(format!("faxes/{ended_ms}.pdf")),
        pdf_size: Some(78_901),
    }
}
