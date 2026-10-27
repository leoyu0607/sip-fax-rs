//! `/api/v1/api-keys` (M6a, ADR-070): the administrator's list of API keys, a new key (shown
//! once), revoking one. Session only: a key cannot manage keys.

use accounts::{ApiKeyAdminError, ApiKeyInfo, Capability};
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use chrono::{DateTime, Utc};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::session::Session;
use crate::state::AppState;

/// The routes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/api-keys", get(list).post(create))
        .route("/api-keys/{id}/revoke", post(revoke))
}

/// A key as the API lists it (never the key itself).
pub fn key_json(k: &ApiKeyInfo, now: DateTime<Utc>) -> Value {
    json!({
        "id": k.id,
        "name": k.name,
        "prefix": k.prefix,
        "scopes": k.scopes,
        "created_by": k.created_by,
        "created_at": k.created_at,
        "expires_at": k.expires_at,
        "revoked_at": k.revoked_at,
        "revoked_by": k.revoked_by,
        "last_used_at": k.last_used_at,
        "last_used_ip": k.last_used_ip,
        "active": k.active(now),
    })
}

/// The API error of a refused key operation.
pub fn key_error(e: &ApiKeyAdminError) -> ApiError {
    let status = match e {
        ApiKeyAdminError::BadName | ApiKeyAdminError::BadExpiry => StatusCode::UNPROCESSABLE_ENTITY,
        ApiKeyAdminError::Duplicate | ApiKeyAdminError::AlreadyRevoked => StatusCode::CONFLICT,
        ApiKeyAdminError::NotFound => StatusCode::NOT_FOUND,
        ApiKeyAdminError::Store(_) => return ApiError::unavailable(),
    };
    ApiError::new(status, e.code(), e.to_string())
}

async fn list(State(st): State<AppState>, s: Session) -> Result<Json<Value>, ApiError> {
    s.need(Capability::ManageApiKeys)?;
    let keys = st.auth.list_api_keys().await.map_err(|e| key_error(&e))?;
    let now = st.clock.now();
    Ok(Json(json!({
        "keys": keys.iter().map(|k| key_json(k, now)).collect::<Vec<_>>()
    })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateBody {
    name: String,
    /// Also `fax:write` (the read flag and the note).
    #[serde(default)]
    write: bool,
    expires_at: Option<DateTime<Utc>>,
}

async fn create(
    State(st): State<AppState>,
    s: Session,
    ApiJson(b): ApiJson<CreateBody>,
) -> Result<Response, ApiError> {
    s.need(Capability::ManageApiKeys)?;
    let c = st
        .auth
        .create_api_key(&b.name, b.write, b.expires_at, s.username())
        .await
        .map_err(|e| key_error(&e))?;
    tracing::info!(
        key = %c.info.name,
        prefix = %c.info.prefix,
        write = b.write,
        by = s.username(),
        "web: API key created"
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({ "key": c.key, "api_key": key_json(&c.info, st.clock.now()) })),
    )
        .into_response())
}

async fn revoke(
    State(st): State<AppState>,
    s: Session,
    UrlPath(id): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    s.need(Capability::ManageApiKeys)?;
    let id: i64 = id.parse().map_err(|_| ApiError::not_found("API key"))?;
    let keys = st.auth.list_api_keys().await.map_err(|e| key_error(&e))?;
    let name = keys
        .iter()
        .find(|k| k.id == id)
        .map(|k| k.name.clone())
        .ok_or_else(|| ApiError::not_found("API key"))?;
    let k = st
        .auth
        .revoke_api_key(&name, s.username())
        .await
        .map_err(|e| key_error(&e))?;
    tracing::info!(key = %k.name, by = s.username(), "web: API key revoked");
    Ok(Json(json!({ "api_key": key_json(&k, st.clock.now()) })))
}
