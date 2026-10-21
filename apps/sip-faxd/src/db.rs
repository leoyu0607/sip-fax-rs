//! The database handle of the daemon: connects at start (a refused connection is not fatal: the
//! calls are received and recorded later, docs/design/m3-store.md section 0) and reconnects on
//! demand.

use std::path::Path;
use std::sync::Arc;

use async_trait::async_trait;
use chrono::{DateTime, Utc};
use fax_in::recorder::RecordSink;
use store::{ConnectOptions, FaxFinal, FaxStore, NewFax, RetentionSettings};
use tokio::sync::Mutex;

use crate::settings::AppConfig;

/// A lazily (re)connecting store.
pub struct Db {
    url: String,
    opts: ConnectOptions,
    retention: RetentionSettings,
    inner: Mutex<Option<Arc<dyn FaxStore>>>,
}

/// How the daemon's first connection attempt went.
#[derive(Debug, PartialEq, Eq)]
pub enum Startup {
    /// Connected, schema current, retention settings seeded, stale `receiving` rows repaired.
    Ready {
        /// Rows that were still `receiving` from a previous run.
        stale_fixed: u64,
    },
    /// The database is not reachable yet; the daemon runs anyway and keeps trying.
    Unreachable(String),
}

impl Db {
    /// The handle for a configuration (nothing is opened yet).
    pub fn new(cfg: &AppConfig, base: &Path) -> Db {
        Db {
            url: cfg.database_url(base),
            opts: ConnectOptions {
                max_connections: cfg.database.max_connections,
                auto_migrate: cfg.database.auto_migrate,
                ..ConnectOptions::default()
            },
            retention: cfg.retention.settings(),
            inner: Mutex::new(None),
        }
    }

    /// Connects now. An error that retrying cannot fix (schema newer than this binary, schema
    /// outdated with `auto_migrate = false`, bad URL, wrong password) is returned: the daemon
    /// must not start. A database that is merely unreachable is reported as
    /// [`Startup::Unreachable`].
    pub async fn start(&self) -> Result<Startup, store::Error> {
        match store::connect_with(&self.url, self.opts.clone()).await {
            Ok(st) => {
                store::purge::seed_retention(&*st, &self.retention).await?;
                // Only now, before the first call: a row that is still `receiving` belongs to a
                // process that is gone.
                let stale_fixed = st.fix_stale_receiving().await?;
                *self.inner.lock().await = Some(st);
                Ok(Startup::Ready { stale_fixed })
            }
            Err(e) if e.is_transient() => Ok(Startup::Unreachable(e.to_string())),
            Err(e) => Err(e),
        }
    }

    /// The store, connecting first if the database was not reachable before.
    pub async fn store(&self) -> store::Result<Arc<dyn FaxStore>> {
        let mut g = self.inner.lock().await;
        if let Some(s) = g.as_ref() {
            return Ok(s.clone());
        }
        let st = store::connect_with(&self.url, self.opts.clone()).await?;
        // (the stale-row repair is start-up only: calls may be in progress by now)
        store::purge::seed_retention(&*st, &self.retention).await?;
        *g = Some(st.clone());
        Ok(st)
    }

    /// Closes the pool.
    pub async fn close(&self) {
        if let Some(s) = self.inner.lock().await.take() {
            s.close().await;
        }
    }
}

#[async_trait]
impl RecordSink for Db {
    async fn insert_receiving(&self, new: &NewFax) -> store::Result<i64> {
        self.store().await?.insert_receiving(new).await
    }
    async fn update_pages(&self, id: i64, pages: i32, at: DateTime<Utc>) -> store::Result<bool> {
        self.store().await?.update_pages(id, pages, at).await
    }
    async fn upsert_finished(&self, new: &NewFax, fin: &FaxFinal) -> store::Result<(i64, bool)> {
        self.store().await?.upsert_finished(new, fin).await
    }
}
