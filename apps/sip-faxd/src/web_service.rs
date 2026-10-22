//! The web interface inside `serve` (M4b): `[web]`, the certificate, the listener.

use std::path::Path;
use std::sync::Arc;

use accounts::AuthService;
use chrono::Utc;
use store::{StoreSource, SystemClock};
use web::config::Scheme;
use web::server::WebServer;
use web::tls;

use crate::settings::AppConfig;

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
    let state = web::AppState {
        auth,
        store,
        data_dir: cfg.data_dir(base),
        preview_cache_mb: cfg.fax_in.storage.preview_cache_mb,
        preview: Arc::default(),
        clock: Arc::new(SystemClock),
        https,
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
