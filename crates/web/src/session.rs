//! The session of a request (spec 2.3): the `sipfax_session` cookie, the CSRF rule for requests
//! that change something, the forced password change, and the capability a route needs.

use std::net::SocketAddr;

use accounts::{AuthService, Authenticated, COOKIE_NAME, CSRF_HEADER, Capability, SessionError};
use axum::extract::connect_info::ConnectInfo;
use axum::extract::{FromRef, FromRequestParts, OriginalUri};
use axum::http::request::Parts;
use axum::http::{HeaderMap, Method, StatusCode, header};
use chrono::{DateTime, Utc};

use crate::error::ApiError;
use crate::state::AppState;

/// The routes an account that has to change its password may still use.
const ALLOWED_BEFORE_PASSWORD_CHANGE: &[&str] = &[
    "/api/v1/auth/me",
    "/api/v1/auth/password",
    "/api/v1/auth/logout",
];

/// A request with a valid session (and, when it changes something, a valid CSRF token).
#[derive(Debug, Clone)]
pub struct Session(pub Authenticated);

impl Session {
    /// The account name, as it is recorded (`read_by`, audit).
    pub fn username(&self) -> &str {
        &self.0.user.username
    }

    /// Refuses with 403 `forbidden` when the role lacks `cap`.
    ///
    /// # Errors
    /// [`ApiError`] 403.
    pub fn need(&self, cap: Capability) -> Result<(), ApiError> {
        self.0
            .require(cap)
            .map_err(|f| ApiError::new(StatusCode::FORBIDDEN, "forbidden", f.to_string()))
    }
}

/// Whether the method changes state (and so needs JSON and the CSRF header).
pub fn changes_state(m: &Method) -> bool {
    !matches!(*m, Method::GET | Method::HEAD | Method::OPTIONS)
}

/// The JSON rule of spec 2.3 for requests that change state.
///
/// # Errors
/// 403 `csrf` when the content type is not `application/json`.
pub fn require_json(headers: &HeaderMap) -> Result<(), ApiError> {
    let json = headers
        .get(header::CONTENT_TYPE)
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.split(';').next())
        .is_some_and(|v| v.trim().eq_ignore_ascii_case("application/json"));
    if json {
        Ok(())
    } else {
        Err(csrf_error(
            "a request that changes something must be application/json",
        ))
    }
}

fn csrf_error(msg: &str) -> ApiError {
    ApiError::new(StatusCode::FORBIDDEN, "csrf", msg)
}

/// The session token of the cookie header, if any.
pub fn cookie_token(headers: &HeaderMap) -> Option<String> {
    headers
        .get_all(header::COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .flat_map(|v| v.split(';'))
        .filter_map(|kv| kv.trim().split_once('='))
        .find(|(k, _)| *k == COOKIE_NAME)
        .map(|(_, v)| v.trim().to_string())
        .filter(|v| !v.is_empty())
}

/// The `Set-Cookie` value of a session that ends at `expires_at` (`Max-Age` is what is left of
/// it, so that the browser keeps it across restarts as long as the server does).
pub fn session_cookie(
    token: &str,
    expires_at: DateTime<Utc>,
    now: DateTime<Utc>,
    https: bool,
) -> String {
    let max_age = (expires_at - now).num_seconds().max(0);
    format!(
        "{COOKIE_NAME}={token}; Path=/; HttpOnly; SameSite=Strict; Max-Age={max_age}{}",
        if https { "; Secure" } else { "" }
    )
}

/// The `Set-Cookie` value that removes the session cookie.
pub fn clear_cookie(https: bool) -> String {
    format!(
        "{COOKIE_NAME}=; Path=/; HttpOnly; SameSite=Strict; Max-Age=0{}",
        if https { "; Secure" } else { "" }
    )
}

/// The client's address (for the login throttle).
pub fn client_ip(parts: &Parts) -> String {
    parts
        .extensions
        .get::<ConnectInfo<SocketAddr>>()
        .map_or_else(|| "unknown".to_string(), |c| c.0.ip().to_string())
}

/// The fixed API error of a session problem (401, or 503 when the database is down).
pub fn session_error(e: &SessionError) -> ApiError {
    let status = match e {
        SessionError::Unavailable(_) => StatusCode::SERVICE_UNAVAILABLE,
        _ => StatusCode::UNAUTHORIZED,
    };
    let message = match e {
        SessionError::Unavailable(_) => {
            "the database is not reachable, try again later".to_string()
        }
        other => other.to_string(),
    };
    ApiError::new(status, e.code(), message)
}

impl<S> FromRequestParts<S> for Session
where
    S: Send + Sync,
    AppState: FromRef<S>,
{
    type Rejection = ApiError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Session, ApiError> {
        let st = AppState::from_ref(state);
        let token =
            cookie_token(&parts.headers).ok_or_else(|| session_error(&SessionError::Missing))?;
        let a = st
            .auth
            .authenticate(&token)
            .await
            .map_err(|e| session_error(&e))?;
        if changes_state(&parts.method) {
            require_json(&parts.headers)?;
            let given = parts.headers.get(CSRF_HEADER).and_then(|v| v.to_str().ok());
            if !AuthService::csrf_ok(&token, given) {
                return Err(csrf_error("the X-Sipfax-Csrf header is missing or wrong"));
            }
        }
        // a nested router sees its own part of the path; the original one is what counts
        let path = parts
            .extensions
            .get::<OriginalUri>()
            .map_or_else(|| parts.uri.path().to_string(), |u| u.0.path().to_string());
        if a.user.must_change_password && !ALLOWED_BEFORE_PASSWORD_CHANGE.contains(&path.as_str()) {
            return Err(ApiError::new(
                StatusCode::FORBIDDEN,
                "password_change_required",
                "choose a new password first",
            ));
        }
        Ok(Session(a))
    }
}
