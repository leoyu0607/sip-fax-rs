//! `/api/v1/auth/*`: login, logout, the current account, the password change.

use accounts::{AccountError, LoginError, LoginOk, UserInfo};
use std::net::SocketAddr;

use axum::extract::State;
use axum::extract::connect_info::ConnectInfo;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::session::{self, Session};
use crate::state::AppState;

/// The routes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/auth/login", post(login))
        .route("/auth/logout", post(logout))
        .route("/auth/me", get(me).put(update_me))
        .route("/auth/password", post(password))
}

/// The languages of the interface (`users.locale`; `null`: the browser's).
pub const LOCALES: &[&str] = &["zh-TW", "en"];

/// 422 `bad_locale` unless `l` is one of [`LOCALES`] (or absent).
pub fn check_locale(l: Option<&str>) -> Result<(), ApiError> {
    match l {
        Some(v) if !LOCALES.contains(&v) => Err(ApiError::new(
            StatusCode::UNPROCESSABLE_ENTITY,
            "bad_locale",
            "locale must be zh-TW, en or null",
        )),
        _ => Ok(()),
    }
}

/// An account in API responses.
pub fn user_json(u: &UserInfo) -> Value {
    json!({
        "id": u.id,
        "username": u.username,
        "display_name": u.display_name,
        "role": u.role.as_str(),
        "locale": u.locale,
        "disabled": u.disabled,
        "must_change_password": u.must_change_password,
        "last_login_at": u.last_login_at,
        "created_at": u.created_at,
    })
}

/// 429 / 423 with `Retry-After`, 401, 503: the answer to a failed login.
fn login_error(e: &LoginError) -> Response {
    let (status, retry) = match e {
        LoginError::InvalidCredentials => (StatusCode::UNAUTHORIZED, None),
        LoginError::Locked { retry_after_secs } => (StatusCode::LOCKED, Some(*retry_after_secs)),
        LoginError::RateLimited { retry_after_secs } => {
            (StatusCode::TOO_MANY_REQUESTS, Some(*retry_after_secs))
        }
        LoginError::Unavailable(_) => (StatusCode::SERVICE_UNAVAILABLE, None),
    };
    let message = match e {
        LoginError::Unavailable(_) => "the database is not reachable, try again later".to_string(),
        other => other.to_string(),
    };
    let mut r = ApiError::new(status, e.code(), message).into_response();
    if let Some(s) = retry
        && let Ok(v) = HeaderValue::from_str(&s.max(1).to_string())
    {
        r.headers_mut().insert(header::RETRY_AFTER, v);
    }
    r
}

/// The error of an account operation.
pub fn account_error(e: &AccountError) -> ApiError {
    let status = match e {
        AccountError::Password(_) | AccountError::BadUsername | AccountError::BadRole => {
            StatusCode::UNPROCESSABLE_ENTITY
        }
        AccountError::WrongPassword => StatusCode::FORBIDDEN,
        AccountError::Duplicate | AccountError::LastAdmin => StatusCode::CONFLICT,
        AccountError::NotFound => StatusCode::NOT_FOUND,
        AccountError::Store(_) => return ApiError::unavailable(),
    };
    ApiError::new(status, e.code(), e.to_string())
}

fn logged_in(st: &AppState, ok: &LoginOk) -> Response {
    let cookie = session::session_cookie(&ok.token, ok.expires_at, st.clock.now(), st.https);
    let mut r = Json(json!({
        "user": user_json(&ok.user),
        "csrf": ok.csrf,
        "expires_at": ok.expires_at,
    }))
    .into_response();
    if let Ok(v) = HeaderValue::from_str(&cookie) {
        r.headers_mut().insert(header::SET_COOKIE, v);
    }
    r
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct LoginBody {
    username: String,
    password: String,
}

fn user_agent(h: &HeaderMap) -> Option<String> {
    h.get(header::USER_AGENT)
        .and_then(|v| v.to_str().ok())
        .map(str::to_string)
}

async fn login(
    State(st): State<AppState>,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ApiJson(b): ApiJson<LoginBody>,
) -> Response {
    let ip = peer.ip().to_string();
    match st
        .auth
        .login(
            &b.username,
            &b.password,
            &ip,
            user_agent(&headers).as_deref(),
        )
        .await
    {
        Ok(ok) => logged_in(&st, &ok),
        Err(e) => login_error(&e),
    }
}

async fn logout(State(st): State<AppState>, s: Session, headers: HeaderMap) -> Response {
    if let Some(t) = session::cookie_token(&headers)
        && let Err(e) = st.auth.logout(&t).await
    {
        return account_error(&e).into_response();
    }
    // (sessions carry no id of their own: every connection of the account closes; another
    // browser of it reconnects with its own session)
    st.events.revoke_user(s.0.user.id);
    let mut r = StatusCode::NO_CONTENT.into_response();
    if let Ok(v) = HeaderValue::from_str(&session::clear_cookie(st.https)) {
        r.headers_mut().insert(header::SET_COOKIE, v);
    }
    r
}

async fn me(s: Session) -> Json<Value> {
    Json(json!({ "user": user_json(&s.0.user), "csrf": s.0.csrf }))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct MeBody {
    /// `null`: follow the browser.
    locale: Option<String>,
}

/// The account's own preferences (the language; everything else is the administrator's).
async fn update_me(
    State(st): State<AppState>,
    s: Session,
    ApiJson(b): ApiJson<MeBody>,
) -> Result<Json<Value>, ApiError> {
    check_locale(b.locale.as_deref())?;
    let u = st
        .auth
        .update_user(s.username(), None, None, None, Some(b.locale), s.username())
        .await
        .map_err(|e| account_error(&e))?;
    Ok(Json(json!({ "user": user_json(&u) })))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PasswordBody {
    current: String,
    new: String,
}

async fn password(
    State(st): State<AppState>,
    s: Session,
    ConnectInfo(peer): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    ApiJson(b): ApiJson<PasswordBody>,
) -> Response {
    let ip = peer.ip().to_string();
    match st
        .auth
        .change_password(
            s.0.user.id,
            &b.current,
            &b.new,
            &ip,
            user_agent(&headers).as_deref(),
        )
        .await
    {
        // every session of the account ends; this request gets a new one
        Ok(ok) => {
            st.events.revoke_user(s.0.user.id);
            logged_in(&st, &ok)
        }
        Err(e) => account_error(&e).into_response(),
    }
}
