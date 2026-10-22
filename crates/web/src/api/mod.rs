//! `/api/v1` (spec 3).

pub mod auth;
pub mod faxes;

use axum::Router;

use crate::state::AppState;

/// Every route of the API, relative to `/api/v1`.
pub fn routes() -> Router<AppState> {
    Router::new().merge(auth::routes()).merge(faxes::routes())
}
