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
    })))
}
