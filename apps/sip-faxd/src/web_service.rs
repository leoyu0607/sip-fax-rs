//! The web interface inside `serve` (M4b): `[web]`, the certificate, the listener.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use accounts::AuthService;
use chrono::Utc;
use fax_in::recorder::{RecordWritten, WrittenHook};
use runtime_config::ConfigService;
use store::{StoreSource, SystemClock};
use web::config::Scheme;
use web::events::{self, EventBus};
use web::server::WebServer;
use web::tls;
use web::{ServiceStatus, StatusSource, TrunkState};

use crate::service::hub::Hub;
use crate::settings::AppConfig;

/// How often `status.changed` compares the service's state.
const STATUS_EVERY: Duration = Duration::from_secs(2);

/// The recorder's writes as `fax.created` / `fax.progress` / `fax.finalized`.
pub fn recorder_events(bus: Arc<EventBus>) -> WrittenHook {
    Arc::new(move |w| match w {
        RecordWritten::Created { id, call_id } => bus.fax_created(*id, call_id),
        RecordWritten::Pages { id, call_id, pages } => bus.fax_progress(*id, call_id, *pages),
        RecordWritten::Finalized { id, call_id } => bus.fax_finalized(*id, call_id),
    })
}

/// Starts the web interface when `[web] enabled`. The certificate is loaded (or made) first;
/// its fingerprint, a new or replaced self-signed certificate and a near end are logged.
///
/// # Errors
/// The certificate cannot be used or the address cannot be bound; the message names the file
/// or the address.
pub async fn start(
    cfg: &AppConfig,
    base: &Path,
    auth: Arc<AuthService>,
    config: Arc<ConfigService>,
    hub: Arc<Hub>,
    events: Arc<EventBus>,
    store: Arc<dyn StoreSource>,
) -> Result<Option<WebServer>, String> {
    let w = &cfg.web;
    if !w.enabled {
        return Ok(None);
    }
    if let Some(warning) = w.plaintext_warning() {
        tracing::warn!("{warning}");
    }
    let tls = match w.scheme {
        Scheme::Http => None,
        Scheme::Https => {
            let names = tls::subject_names(
                tls::hostname().as_deref(),
                w.bind.ip(),
                &tls::local_ips(),
                &w.tls_names,
            );
            let now = Utc::now();
            let m = tls::load(w, base, &cfg.conf_dir, &names, now.date_naive())
                .map_err(|e| format!("[web] {e}"))?;
            if !m.generated.is_empty() {
                tracing::warn!(
                    cert = %m.cert_path.display(),
                    names = %m.generated.join(", "),
                    "web: a self-signed certificate was made; browsers warn about it until it is trusted"
                );
            }
            if let Some(old) = &m.replaced {
                tracing::warn!(
                    old_fingerprint = %old,
                    "web: the self-signed certificate was about to end and was replaced: browsers have to accept the new one"
                );
            }
            if let Some(warning) = m.expiry_warning(now) {
                tracing::warn!("web: {warning}");
            }
            tracing::info!(
                "web: certificate SHA-256 fingerprint {} (valid until {})",
                m.fingerprint,
                m.not_after.format("%Y-%m-%d")
            );
            Some(m.config)
        }
    };
    let https = tls.is_some();
    let status: Arc<dyn StatusSource> = Arc::new(HubStatus(hub));
    let pending = config.clone();
    // (lives as long as the process)
    events::watch_status(
        events.clone(),
        status.clone(),
        move || pending.pending_restart(),
        STATUS_EVERY,
    );
    let state = web::AppState {
        auth,
        config,
        status,
        events,
        ws_ping: web::api::events::PING_EVERY,
        store,
        data_dir: cfg.data_dir(base),
        preview_cache_mb: cfg.fax_in.storage.preview_cache_mb,
        preview: Arc::default(),
        clock: Arc::new(SystemClock),
        https,
        request_timeout: web::REQUEST_TIMEOUT,
    };
    let srv = WebServer::start(w.bind, web::app(state), tls)
        .await
        .map_err(|e| format!("[web] cannot listen on {}: {e}", w.bind))?;
    tracing::info!(
        "web interface on {}://{}",
        if https { "https" } else { "http" },
        srv.local_addr()
    );
    Ok(Some(srv))
}

/// `GET /status` reads what the control socket's `status` answers.
struct HubStatus(Arc<Hub>);

impl StatusSource for HubStatus {
    fn status(&self) -> ServiceStatus {
        let s = self.0.status();
        ServiceStatus {
            version: s.version,
            state: s.state,
            started_at: s.started_at,
            uptime_secs: s.uptime_secs,
            listen: s.listen,
            active_calls: s.active_calls,
            trunks: s
                .trunks
                .into_iter()
                .map(|t| TrunkState {
                    name: t.name,
                    registration: t.registration,
                })
                .collect(),
        }
    }
}
