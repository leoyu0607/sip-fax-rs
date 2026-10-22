//! The web interface server (M4b, spec `docs/design/m4-web.md` 3-5).
//!
//! * [`config`]: the `[web]` table
//! * [`tls`]: the certificate (configured or self-signed) and the rustls configuration
//! * [`server`]: the listener (HTTP or HTTPS) and the security headers
//! * [`session`]: the session cookie, CSRF, the capability of a route
//! * [`api`]: the routes under `/api/v1`
//! * [`error`], [`extract`]: the error shape and the JSON body of the API

pub mod api;
pub mod config;
pub mod error;
pub mod events;
pub mod extract;
pub mod server;
pub mod session;
pub mod state;
pub mod tls;

use axum::Router;
use axum::extract::{Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};

pub use extract::BODY_LIMIT;
pub use state::{AppState, REQUEST_TIMEOUT, ServiceStatus, StatusSource, TrunkState};

use crate::error::ApiError;

/// The application: the API under `/api/v1`, the request deadline (W11) and the security
/// headers (HSTS with HTTPS).
pub fn app(state: AppState) -> Router {
    let https = state.https;
    let r = Router::new()
        .nest("/api/v1", api::routes())
        .fallback(error::not_found)
        .layer(middleware::from_fn_with_state(state.clone(), deadline))
        .with_state(state);
    server::with_security_headers(r, https)
}

/// 408 `timeout` when a request (its body included: the handlers read it) is not answered
/// within [`AppState::request_timeout`]. A response that streams (a download) is not cut.
async fn deadline(State(st): State<AppState>, req: Request, next: Next) -> Response {
    match tokio::time::timeout(st.request_timeout, next.run(req)).await {
        Ok(r) => r,
        Err(_) => ApiError::new(
            StatusCode::REQUEST_TIMEOUT,
            "timeout",
            "the request was not completed in time",
        )
        .into_response(),
    }
}
