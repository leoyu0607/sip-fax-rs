//! `/api/v1` (spec 3).

pub mod apikeys;
pub mod auth;
pub mod config;
pub mod events;
pub mod faxes;
pub mod system;
pub mod users;

use axum::Router;
use axum::http::header;
use axum::response::IntoResponse;
use axum::routing::get;

use crate::state::AppState;

/// The OpenAPI description of the fax routes for API keys (M6a, ADR-070), the same file as
/// `docs/openapi.yaml`.
pub const OPENAPI: &str = include_str!("../../../../docs/openapi.yaml");

/// `GET /openapi.yaml`, without a session: it says nothing about the data.
async fn openapi() -> impl IntoResponse {
    (
        [(header::CONTENT_TYPE, "application/yaml; charset=utf-8")],
        OPENAPI,
    )
}

/// Every route of the API, relative to `/api/v1`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/openapi.yaml", get(openapi))
        .merge(auth::routes())
        .merge(faxes::routes())
        .merge(config::routes())
        .merge(users::routes())
        .merge(system::routes())
        .merge(events::routes())
        .merge(apikeys::routes())
}
