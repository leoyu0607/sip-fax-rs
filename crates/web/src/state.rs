//! What the handlers share.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use accounts::AuthService;
use runtime_config::ConfigService;
use serde::Serialize;
use store::{Clock, FaxStore, Preview, StoreSource};
use tokio::sync::OnceCell;

use crate::error::ApiError;
use crate::events::EventBus;

/// A request (headers, body, handler) that takes longer than this is answered with 408.
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// One trunk in [`ServiceStatus`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TrunkState {
    /// Trunk name.
    pub name: String,
    /// `none` (the trunk does not register), `registering`, `registered`, `failed: why`, ...
    pub registration: String,
}

/// What the running service says about itself (`GET /status`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ServiceStatus {
    /// `sip-faxd` version.
    pub version: String,
    /// `running`, or `stopping` while the calls are drained.
    pub state: String,
    /// Start time, RFC 3339 (UTC).
    pub started_at: String,
    /// Seconds since the start.
    pub uptime_secs: u64,
    /// The SIP listeners (`udp 0.0.0.0:5060`).
    pub listen: Vec<String>,
    /// Calls in progress.
    pub active_calls: usize,
    /// The trunks and their registration.
    pub trunks: Vec<TrunkState>,
}

/// Where `GET /status` gets the service's state from (`sip-faxd` connects its control hub).
pub trait StatusSource: Send + Sync {
    /// The state now.
    fn status(&self) -> ServiceStatus;
}

/// The application state.
#[derive(Clone)]
pub struct AppState {
    /// Accounts and sessions.
    pub auth: Arc<AuthService>,
    /// The runtime configuration (`/config`).
    pub config: Arc<ConfigService>,
    /// The service's state (`/status`).
    pub status: Arc<dyn StatusSource>,
    /// The events of `/events`.
    pub events: Arc<EventBus>,
    /// Ping interval of `/events` ([`crate::api::events::PING_EVERY`]; shorter in tests).
    pub ws_ping: Duration,
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
    /// [`REQUEST_TIMEOUT`]; shorter in tests.
    pub request_timeout: Duration,
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
