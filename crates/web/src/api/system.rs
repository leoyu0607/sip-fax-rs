//! `/api/v1/status` (spec 3, W09): the service, the database, the configuration waiting for a
//! restart.

use std::time::Duration;

use accounts::{Capability, Role};
use axum::extract::State;
use axum::routing::get;
use axum::{Json, Router};
use serde_json::{Value, json};

use crate::api::config::source_str;
use crate::error::ApiError;
use crate::session::Session;
use crate::state::AppState;

/// The database gets this long to answer the status check.
pub const DB_CHECK_TIMEOUT: Duration = Duration::from_secs(3);

/// The routes.
pub fn routes() -> Router<AppState> {
    Router::new().route("/status", get(status))
}

/// `{"state": "ok", "schema": N}` or `{"state": "unavailable"}`; never the cause (it may name
/// hosts), which goes to the log.
async fn database(st: &AppState) -> Value {
    let check = async {
        let s = st.store.store().await?;
        s.schema_version().await
    };
    match tokio::time::timeout(DB_CHECK_TIMEOUT, check).await {
        Ok(Ok(v)) => json!({"state": "ok", "schema": v}),
        Ok(Err(e)) => {
            tracing::warn!("web: status: database: {e}");
            json!({"state": "unavailable"})
        }
        Err(_) => {
            tracing::warn!("web: status: the database did not answer within {DB_CHECK_TIMEOUT:?}");
            json!({"state": "unavailable"})
        }
    }
}

async fn status(State(st): State<AppState>, s: Session) -> Result<Json<Value>, ApiError> {
    s.need(Capability::ViewStatus)?;
    let svc = st.status.status();
    let c = st.config.status();
    let mut config = json!({
        "source": source_str(c.source),
        "pending_restart": c.pending_restart,
    });
    // the key file and the secrets that need to be entered again are for whoever can fix them
    if s.0.user.role == Role::Admin {
        config["key"] = json!(c.key);
        config["undecryptable"] = json!(c.undecryptable);
    }
    Ok(Json(json!({
        "service": svc,
        "database": database(&st).await,
        "config": config,
        "storage": storage(&st.data_dir),
    })))
}

/// The space of the file system that holds `[storage] data_dir`: `{"free_bytes", "total_bytes"}`
/// (free for an unprivileged writer), or `null` when it cannot be read (and off unix).
fn storage(dir: &std::path::Path) -> Value {
    #[cfg(unix)]
    {
        match rustix::fs::statvfs(dir) {
            Ok(v) => {
                let unit = v.f_frsize.max(1);
                json!({
                    "free_bytes": v.f_bavail.saturating_mul(unit),
                    "total_bytes": v.f_blocks.saturating_mul(unit),
                })
            }
            Err(e) => {
                tracing::warn!("web: status: the free space of {}: {e}", dir.display());
                Value::Null
            }
        }
    }
    #[cfg(not(unix))]
    {
        let _ = dir;
        Value::Null
    }
}
