//! What the handlers share.

use std::path::PathBuf;
use std::sync::Arc;

use accounts::AuthService;
use store::{Clock, FaxStore, Preview, StoreSource};
use tokio::sync::OnceCell;

use crate::error::ApiError;

/// The application state.
#[derive(Clone)]
pub struct AppState {
    /// Accounts and sessions.
    pub auth: Arc<AuthService>,
    /// The fax records (connected lazily: the database may be down at start).
    pub store: Arc<dyn StoreSource>,
    /// `[storage] data_dir`: the TIFF/PDF files and the PNG cache live under it.
    pub data_dir: PathBuf,
    /// `[storage] preview_cache_mb`.
    pub preview_cache_mb: u64,
    /// The PNG preview service, made with the first store.
    pub preview: Arc<OnceCell<Arc<Preview>>>,
    /// The time (sessions, cookies, `redeliver`); injectable for tests.
    pub clock: Arc<dyn Clock>,
    /// The server speaks HTTPS (the cookie gets `Secure`, responses HSTS).
    pub https: bool,
}

impl AppState {
    /// The store, or 503 when the database does not answer.
    ///
    /// # Errors
    /// [`ApiError::unavailable`].
    pub async fn store(&self) -> Result<Arc<dyn FaxStore>, ApiError> {
        self.store.store().await.map_err(|e| {
            tracing::warn!("web: database: {e}");
            ApiError::unavailable()
        })
    }

    /// The preview service (made on first use, with the cache limit and the clock).
    ///
    /// # Errors
    /// 503 when the database does not answer.
    pub async fn preview(&self) -> Result<Arc<Preview>, ApiError> {
        let st = self.store().await?;
        let p = self
            .preview
            .get_or_init(|| async {
                Arc::new(
                    Preview::new(st, self.data_dir.clone())
                        .with_clock(self.clock.clone())
                        .with_cache_limit_mb(self.preview_cache_mb),
                )
            })
            .await;
        Ok(p.clone())
    }
}
