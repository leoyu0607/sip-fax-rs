//! `/api/v1/config/*` (spec 3): the runtime configuration sections, their audit trail, the
//! trunk order. Everything goes through `runtime_config::ConfigService`, the one validator.

use accounts::Capability;
use axum::extract::{Path as UrlPath, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, put};
use axum::{Json, Router};
use runtime_config::{ConfigStatus, SectionView, Source, UpdateError, UpdateOutcome};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::session::Session;
use crate::state::AppState;

/// The default and the largest page of the audit trail.
pub const AUDIT_LIMIT: u32 = 50;
/// See [`AUDIT_LIMIT`].
pub const AUDIT_MAX: u32 = 200;

/// The routes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/config", get(list))
        .route("/config/audit", get(audit))
        .route("/config/trunks/order", put(reorder))
        .route(
            "/config/{section}",
            get(show).put(update).delete(delete_trunk),
        )
}

/// The API error of a refused change (mapping of `docs/test-reports/M4a.md` 8.1).
pub fn update_error(e: UpdateError) -> ApiError {
    match e {
        UpdateError::Invalid(errs) => {
            // one entry per field: the first problem is the one to fix first
            let mut fields = Map::new();
            for f in errs {
                fields
                    .entry(f.field)
                    .or_insert_with(|| json!({"code": f.code, "message": f.message}));
            }
            ApiError::new(
                StatusCode::UNPROCESSABLE_ENTITY,
                "invalid",
                "the configuration is not valid",
            )
            .with_fields(fields)
        }
        UpdateError::Conflict { current } => {
            let mut fields = Map::new();
            fields.insert("current_version".into(), json!(current));
            ApiError::new(
                StatusCode::CONFLICT,
                "conflict",
                "the section was changed by someone else; read it again",
            )
            .with_fields(fields)
        }
        UpdateError::NotFound | UpdateError::UnknownSection(_) => ApiError::not_found("section"),
        UpdateError::Unavailable(m) => {
            tracing::warn!("web: config: database: {m}");
            ApiError::unavailable()
        }
        UpdateError::Internal(m) => ApiError::internal(m),
    }
}

fn section_json(v: &SectionView) -> Value {
    json!({"name": v.name, "version": v.version, "body": v.body})
}

/// `database` or `file_fallback`.
pub fn source_str(s: Source) -> &'static str {
    match s {
        Source::Database => "database",
        Source::FileFallback => "file_fallback",
    }
}

fn status_json(s: &ConfigStatus) -> Value {
    json!({
        "source": source_str(s.source),
        "key": s.key,
        "undecryptable": s.undecryptable,
        "pending_restart": s.pending_restart,
    })
}

fn outcome_json(o: &UpdateOutcome) -> Value {
    json!({
        "section": o.section,
        "version": o.version,
        "restart_required": o.restart_required,
        "pending_restart": o.pending_restart,
        "warnings": o.warnings,
        "applied": o.applied,
    })
}

async fn list(State(st): State<AppState>, s: Session) -> Result<Json<Value>, ApiError> {
    s.need(Capability::ViewConfig)?;
    let sections = st.config.list_sections().await.map_err(update_error)?;
    Ok(Json(json!({
        "sections": sections.iter().map(section_json).collect::<Vec<_>>(),
        "status": status_json(&st.config.status()),
        // what waits for a restart when it is changed (marked on the forms beforehand)
        "restart_fields": runtime_config::RESTART_FIELDS,
    })))
}

async fn show(
    State(st): State<AppState>,
    s: Session,
    UrlPath(section): UrlPath<String>,
) -> Result<Json<Value>, ApiError> {
    s.need(Capability::ViewConfig)?;
    let v = st
        .config
        .get_section(&section)
        .await
        .map_err(update_error)?;
    Ok(Json(section_json(&v)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateBody {
    /// The version that was read (0: never stored, or a new trunk).
    version: i32,
    body: Value,
}

async fn update(
    State(st): State<AppState>,
    s: Session,
    UrlPath(section): UrlPath<String>,
    ApiJson(b): ApiJson<UpdateBody>,
) -> Result<Json<Value>, ApiError> {
    s.need(Capability::EditConfig)?;
    let o = st
        .config
        .update(&section, b.body, b.version, s.username())
        .await
        .map_err(update_error)?;
    tracing::info!(section = %o.section, version = o.version, by = s.username(), "web: configuration changed");
    Ok(Json(outcome_json(&o)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct VersionQuery {
    version: i32,
}

async fn delete_trunk(
    State(st): State<AppState>,
    s: Session,
    UrlPath(section): UrlPath<String>,
    q: Result<Query<VersionQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    s.need(Capability::EditConfig)?;
    let Some(name) = section.strip_prefix("trunk:") else {
        return Err(ApiError::new(
            StatusCode::METHOD_NOT_ALLOWED,
            "method_not_allowed",
            "only a trunk can be deleted",
        ));
    };
    let Query(q) =
        q.map_err(|_| ApiError::bad_request("`version` (the version that was read) is required"))?;
    let o = st
        .config
        .delete_trunk(name, q.version, s.username())
        .await
        .map_err(update_error)?;
    tracing::info!(trunk = name, by = s.username(), "web: trunk deleted");
    Ok(Json(outcome_json(&o)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OrderBody {
    order: Vec<String>,
}

async fn reorder(
    State(st): State<AppState>,
    s: Session,
    ApiJson(b): ApiJson<OrderBody>,
) -> Result<Json<Value>, ApiError> {
    s.need(Capability::EditConfig)?;
    let o = st
        .config
        .reorder_trunks(b.order, s.username())
        .await
        .map_err(update_error)?;
    Ok(Json(outcome_json(&o)))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AuditQuery {
    limit: Option<u32>,
    /// Rows older than this id (the `next` of the previous page).
    before: Option<i64>,
}

/// A stored body (masked JSON text) as JSON again; text that does not parse stays text.
fn body_value(s: Option<&str>) -> Value {
    s.map_or(Value::Null, |t| {
        serde_json::from_str(t).unwrap_or_else(|_| Value::String(t.to_string()))
    })
}

async fn audit(
    State(st): State<AppState>,
    s: Session,
    q: Result<Query<AuditQuery>, axum::extract::rejection::QueryRejection>,
) -> Result<Json<Value>, ApiError> {
    s.need(Capability::ViewAudit)?;
    let Query(q) = q.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let limit = q.limit.unwrap_or(AUDIT_LIMIT).clamp(1, AUDIT_MAX);
    let rows = st
        .config
        .audit(limit, q.before)
        .await
        .map_err(update_error)?;
    let next = (rows.len() == limit as usize)
        .then(|| rows.last().map(|r| r.id))
        .flatten();
    Ok(Json(json!({
        "entries": rows.iter().map(|r| json!({
            "id": r.id,
            "section": r.section,
            "action": r.action,
            "before": body_value(r.before_json.as_deref()),
            "after": body_value(r.after_json.as_deref()),
            "by": r.by,
            "at": r.at,
        })).collect::<Vec<_>>(),
        "next": next,
    })))
}

#[cfg(test)]
mod tests;
