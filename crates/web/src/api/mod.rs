//! `/api/v1` (spec 3).

pub mod apikeys;
pub mod auth;
pub mod config;
pub mod events;
pub mod faxes;
pub mod system;
pub mod users;

use axum::Router;

use crate::state::AppState;

/// Every route of the API, relative to `/api/v1`.
pub fn routes() -> Router<AppState> {
    Router::new()
        .merge(auth::routes())
        .merge(faxes::routes())
        .merge(config::routes())
        .merge(users::routes())
        .merge(system::routes())
        .merge(events::routes())
        .merge(apikeys::routes())
}
