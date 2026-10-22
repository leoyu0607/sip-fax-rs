//! The web interface server (M4b, spec `docs/design/m4-web.md` 3-5).
//!
//! * [`config`]: the `[web]` table
//! * [`tls`]: the certificate (configured or self-signed) and the rustls configuration
//! * [`server`]: the listener (HTTP or HTTPS) and the security headers
//! * [`error`]: the error shape of the API

pub mod config;
pub mod error;
pub mod server;
pub mod tls;

use axum::Router;

/// The application: the API under `/api/v1` and the security headers. `https` adds HSTS.
pub fn app(https: bool) -> Router {
    server::with_security_headers(Router::new().fallback(error::not_found), https)
}
