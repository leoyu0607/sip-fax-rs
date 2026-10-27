//! Embedded, forward-only migrations (spec section 1.3). One SQL directory per backend:
//! `crates/store/migrations/<backend>/NNNN_name.sql`, compiled into the binary.

use crate::clock::Clock;
use crate::db::{Backend, BackendKind, Kind, MigrationSession, Val, split_statements};
use crate::error::{Error, Result};

/// One migration.
pub struct Migration {
    /// Version number (the NNNN prefix).
    pub version: i32,
    /// Name.
    pub name: &'static str,
    /// SQL text.
    pub sql: &'static str,
}

// include_str! needs literal file names, so the list is spelled out per backend.
static SQLITE: &[Migration] = &[
    Migration {
        version: 1,
        name: "init",
        sql: include_str!("../migrations/sqlite/0001_init.sql"),
    },
    Migration {
        version: 2,
        name: "config_accounts",
        sql: include_str!("../migrations/sqlite/0002_config_accounts.sql"),
    },
    Migration {
        version: 3,
        name: "fax_modem",
        sql: include_str!("../migrations/sqlite/0003_fax_modem.sql"),
    },
    Migration {
        version: 4,
        name: "integration",
        sql: include_str!("../migrations/sqlite/0004_integration.sql"),
    },
];
static POSTGRES: &[Migration] = &[
    Migration {
        version: 1,
        name: "init",
        sql: include_str!("../migrations/postgres/0001_init.sql"),
    },
    Migration {
        version: 2,
        name: "config_accounts",
        sql: include_str!("../migrations/postgres/0002_config_accounts.sql"),
    },
    Migration {
        version: 3,
        name: "fax_modem",
        sql: include_str!("../migrations/postgres/0003_fax_modem.sql"),
    },
    Migration {
        version: 4,
        name: "integration",
        sql: include_str!("../migrations/postgres/0004_integration.sql"),
    },
];
static MYSQL: &[Migration] = &[
    Migration {
        version: 1,
        name: "init",
        sql: include_str!("../migrations/mysql/0001_init.sql"),
    },
    Migration {
        version: 2,
        name: "config_accounts",
        sql: include_str!("../migrations/mysql/0002_config_accounts.sql"),
    },
    Migration {
        version: 3,
        name: "fax_modem",
        sql: include_str!("../migrations/mysql/0003_fax_modem.sql"),
    },
    Migration {
        version: 4,
        name: "integration",
        sql: include_str!("../migrations/mysql/0004_integration.sql"),
    },
];
static MSSQL: &[Migration] = &[
    Migration {
        version: 1,
        name: "init",
        sql: include_str!("../migrations/mssql/0001_init.sql"),
    },
    Migration {
        version: 2,
        name: "config_accounts",
        sql: include_str!("../migrations/mssql/0002_config_accounts.sql"),
    },
    Migration {
        version: 3,
        name: "fax_modem",
        sql: include_str!("../migrations/mssql/0003_fax_modem.sql"),
    },
    Migration {
        version: 4,
        name: "integration",
        sql: include_str!("../migrations/mssql/0004_integration.sql"),
    },
];

/// Migrations of a backend, ascending by version.
pub fn migrations(kind: BackendKind) -> &'static [Migration] {
    match kind {
        BackendKind::Sqlite => SQLITE,
        BackendKind::Postgres => POSTGRES,
        BackendKind::Mysql => MYSQL,
        BackendKind::Mssql => MSSQL,
    }
}

/// Highest schema version this binary knows.
pub fn latest_version(kind: BackendKind) -> i32 {
    migrations(kind).last().map_or(0, |m| m.version)
}

/// What a migration run did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MigrateReport {
    /// Version before (0 = empty database).
    pub from: i32,
    /// Version after.
    pub to: i32,
    /// Versions applied by this run.
    pub applied: Vec<i32>,
}

fn bootstrap_sql(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::Sqlite => {
            "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER NOT NULL PRIMARY KEY, name TEXT NOT NULL, applied_at TEXT NOT NULL)"
        }
        BackendKind::Postgres => {
            "CREATE TABLE IF NOT EXISTS schema_migrations (version INTEGER NOT NULL PRIMARY KEY, name VARCHAR(64) NOT NULL, applied_at TIMESTAMPTZ NOT NULL)"
        }
        BackendKind::Mysql => {
            "CREATE TABLE IF NOT EXISTS schema_migrations (version INT NOT NULL PRIMARY KEY, name VARCHAR(64) NOT NULL, applied_at DATETIME(3) NOT NULL) ENGINE=InnoDB DEFAULT CHARSET=utf8mb4"
        }
        BackendKind::Mssql => {
            "IF OBJECT_ID(N'schema_migrations', N'U') IS NULL CREATE TABLE schema_migrations (version INT NOT NULL PRIMARY KEY, name NVARCHAR(64) NOT NULL, applied_at DATETIME2(3) NOT NULL)"
        }
    }
}

pub(crate) fn exists_sql(kind: BackendKind) -> &'static str {
    match kind {
        BackendKind::Sqlite => {
            "SELECT name FROM sqlite_master WHERE type = 'table' AND name = 'schema_migrations'"
        }
        BackendKind::Postgres => {
            "SELECT table_name FROM information_schema.tables WHERE table_name = 'schema_migrations' AND table_schema = current_schema()"
        }
        BackendKind::Mysql => {
            "SELECT table_name FROM information_schema.tables WHERE table_name = 'schema_migrations' AND table_schema = DATABASE()"
        }
        BackendKind::Mssql => "SELECT name FROM sys.tables WHERE name = 'schema_migrations'",
    }
}

/// Current schema version; 0 when the database has no `schema_migrations` table yet.
pub async fn current_version(be: &dyn Backend) -> Result<i32> {
    if be
        .query(exists_sql(be.kind()), &[], &[Kind::Str])
        .await?
        .is_empty()
    {
        return Ok(0);
    }
    let rows = be
        .query(
            "SELECT MAX(version) FROM schema_migrations",
            &[],
            &[Kind::Int],
        )
        .await?;
    match rows.first().and_then(|r| r.first()) {
        Some(Val::I(Some(v))) => Ok(*v as i32),
        _ => Ok(0),
    }
}

/// Refuse a database that is newer than this binary.
pub async fn check_not_newer(be: &dyn Backend) -> Result<i32> {
    let db = current_version(be).await?;
    let supported = latest_version(be.kind());
    if db > supported {
        return Err(Error::SchemaTooNew { db, supported });
    }
    Ok(db)
}

/// Apply all pending migrations. Idempotent, and safe to run from several processes at once: the
/// run holds a database lock (`pg_advisory_lock`, MySQL `GET_LOCK`, MSSQL `sp_getapplock`, a
/// SQLite exclusive transaction) and reads the version again once it has the lock, so the
/// second instance finds nothing left to do. Where DDL is transactional (SQLite, PostgreSQL,
/// MSSQL) a failed migration is rolled back completely; MySQL/MariaDB cannot roll DDL back, so
/// their migration files only contain statements that are safe to repeat (`CREATE TABLE IF NOT
/// EXISTS` with the indexes inside the table definition; an `ADD COLUMN` that finds the column
/// there already, error 1060 "Duplicate column name", and an `ADD INDEX` that finds the index, 1061
/// "Duplicate key name", count as done) and a failed run is simply
/// run again.
pub async fn migrate(be: &dyn Backend, clock: &dyn Clock) -> Result<MigrateReport> {
    apply(be, clock, migrations(be.kind())).await
}

/// [`migrate`] with an explicit list (the tests inject a failing migration).
pub(crate) async fn apply(
    be: &dyn Backend,
    clock: &dyn Clock,
    list: &[Migration],
) -> Result<MigrateReport> {
    let kind = be.kind();
    // Refuse a database that is newer than this binary before taking any lock.
    let db = current_version(be).await?;
    let supported = list.last().map_or(0, |m| m.version);
    if db > supported {
        return Err(Error::SchemaTooNew { db, supported });
    }
    let mut session = be.migration_session().await?;
    let result = run(&mut *session, kind, clock, list).await;
    let released = session.finish(result.is_ok()).await;
    let report = result?;
    released?;
    Ok(report)
}

async fn version_in(s: &mut dyn MigrationSession, kind: BackendKind) -> Result<i32> {
    if s.query(exists_sql(kind), &[], &[Kind::Str])
        .await?
        .is_empty()
    {
        return Ok(0);
    }
    let rows = s
        .query(
            "SELECT MAX(version) FROM schema_migrations",
            &[],
            &[Kind::Int],
        )
        .await?;
    match rows.first().and_then(|r| r.first()) {
        Some(Val::I(Some(v))) => Ok(*v as i32),
        _ => Ok(0),
    }
}

async fn run(
    s: &mut dyn MigrationSession,
    kind: BackendKind,
    clock: &dyn Clock,
    list: &[Migration],
) -> Result<MigrateReport> {
    s.execute(bootstrap_sql(kind), &[]).await?;
    // read again under the lock: another instance may have migrated while we waited for it
    let from = version_in(s, kind).await?;
    let supported = list.last().map_or(0, |m| m.version);
    if from > supported {
        return Err(Error::SchemaTooNew {
            db: from,
            supported,
        });
    }
    let mut applied = Vec::new();
    for m in list.iter().filter(|m| m.version > from) {
        s.begin().await?;
        let step = async {
            for stmt in split_statements(m.sql) {
                match s.execute(&stmt, &[]).await {
                    Ok(_) => {}
                    // MySQL cannot roll an ADD COLUMN or ADD INDEX back: done by an earlier run
                    // that failed before recording its version
                    Err(e)
                        if kind == BackendKind::Mysql
                            && (e.to_string().contains("Duplicate column name")
                                || e.to_string().contains("Duplicate key name")) => {}
                    Err(e) => {
                        return Err(Error::Backend(format!(
                            "migration {:04}_{}: {e}",
                            m.version, m.name
                        )));
                    }
                }
            }
            s.execute(
                "INSERT INTO schema_migrations (version, name, applied_at) VALUES (?, ?, ?)",
                &[
                    Val::from(m.version),
                    Val::from(m.name),
                    Val::from(crate::model::trunc_ms(clock.now())),
                ],
            )
            .await
        }
        .await;
        match step {
            Ok(_) => s.commit().await?,
            Err(e) => {
                let _ = s.rollback().await;
                return Err(e);
            }
        }
        applied.push(m.version);
    }
    Ok(MigrateReport {
        from,
        to: from.max(applied.last().copied().unwrap_or(from)),
        applied,
    })
}
