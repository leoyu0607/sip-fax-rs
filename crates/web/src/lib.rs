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
pub mod extract;
pub mod server;
pub mod session;
pub mod state;
pub mod tls;

use axum::Router;

pub use extract::BODY_LIMIT;
pub use state::AppState;

/// The application: the API under `/api/v1` and the security headers (HSTS with HTTPS).
pub fn app(state: AppState) -> Router {
    let https = state.https;
    let r = Router::new()
        .nest("/api/v1", api::routes())
        .fallback(error::not_found)
        .with_state(state);
    server::with_security_headers(r, https)
}
