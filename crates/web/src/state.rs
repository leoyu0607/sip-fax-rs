//! What the handlers share.

use std::sync::Arc;

use accounts::AuthService;
use store::Clock;

/// The application state.
#[derive(Clone)]
pub struct AppState {
    /// Accounts and sessions.
    pub auth: Arc<AuthService>,
    /// The time (sessions, cookies); injectable for tests.
    pub clock: Arc<dyn Clock>,
    /// The server speaks HTTPS (the cookie gets `Secure`, responses HSTS).
    pub https: bool,
}
