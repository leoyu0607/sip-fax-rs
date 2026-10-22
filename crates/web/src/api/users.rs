//! `/api/v1/users/*` (spec 3): account management for administrators. The rules (password,
//! username, the last enabled administrator) are `accounts::AuthService`'s.

use accounts::{AccountError, Capability, NewAccount, Role, UserInfo};
use axum::extract::{Path as UrlPath, State};
use axum::http::StatusCode;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, put};
use axum::{Json, Router};
use serde::{Deserialize, Deserializer};
use serde_json::{Value, json};

use crate::api::auth::{account_error, check_locale, user_json};
use crate::error::ApiError;
use crate::extract::ApiJson;
use crate::session::Session;
use crate::state::AppState;

/// The routes.
pub fn routes() -> Router<AppState> {
    Router::new()
        .route("/users", get(list).post(create))
        .route("/users/{id}", put(update).delete(delete))
}

fn role(s: &str) -> Result<Role, ApiError> {
    Role::parse(s).ok_or_else(|| account_error(&AccountError::BadRole))
}

/// The account with row id `id` (the path segment), or 404.
async fn by_id(st: &AppState, id: &str) -> Result<UserInfo, ApiError> {
    let id: i64 = id
        .parse()
        .ok()
        .filter(|i| *i > 0)
        .ok_or_else(|| ApiError::not_found("user"))?;
    st.auth
        .list_users()
        .await
        .map_err(|e| account_error(&e))?
        .into_iter()
        .find(|u| u.id == id)
        .ok_or_else(|| ApiError::not_found("user"))
}

async fn list(State(st): State<AppState>, s: Session) -> Result<Json<Value>, ApiError> {
    s.need(Capability::ManageUsers)?;
    let users = st.auth.list_users().await.map_err(|e| account_error(&e))?;
    Ok(Json(
        json!({ "users": users.iter().map(user_json).collect::<Vec<_>>() }),
    ))
}

fn yes() -> bool {
    true
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CreateBody {
    username: String,
    role: String,
    password: String,
    display_name: Option<String>,
    locale: Option<String>,
    /// The first login has to choose another password (default: yes).
    #[serde(default = "yes")]
    must_change_password: bool,
}

async fn create(
    State(st): State<AppState>,
    s: Session,
    ApiJson(b): ApiJson<CreateBody>,
) -> Result<Response, ApiError> {
    s.need(Capability::ManageUsers)?;
    check_locale(b.locale.as_deref())?;
    let account = NewAccount {
        username: &b.username,
        role: role(&b.role)?,
        password: &b.password,
        display_name: b.display_name.as_deref(),
        locale: b.locale.as_deref(),
        must_change: b.must_change_password,
    };
    let u = st
        .auth
        .create_user(&account, s.username())
        .await
        .map_err(|e| account_error(&e))?;
    tracing::info!(user = %u.username, role = %u.role, by = s.username(), "web: account created");
    Ok((StatusCode::CREATED, Json(json!({ "user": user_json(&u) }))).into_response())
}

/// `absent` → `None`, `null` → `Some(None)`, a value → `Some(Some(v))`.
pub(crate) fn nullable<'de, D: Deserializer<'de>>(
    d: D,
) -> Result<Option<Option<String>>, D::Error> {
    Option::<String>::deserialize(d).map(Some)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UpdateBody {
    role: Option<String>,
    disabled: Option<bool>,
    #[serde(default, deserialize_with = "nullable")]
    display_name: Option<Option<String>>,
    #[serde(default, deserialize_with = "nullable")]
    locale: Option<Option<String>>,
    /// A new password (every session of the account ends).
    password: Option<String>,
    /// With `password`: the next login has to choose another one (default: yes).
    must_change_password: Option<bool>,
}

async fn update(
    State(st): State<AppState>,
    s: Session,
    UrlPath(id): UrlPath<String>,
    ApiJson(b): ApiJson<UpdateBody>,
) -> Result<Json<Value>, ApiError> {
    s.need(Capability::ManageUsers)?;
    let u = by_id(&st, &id).await?;
    let new_role = b.role.as_deref().map(role).transpose()?;
    if let Some(l) = &b.locale {
        check_locale(l.as_deref())?;
    }
    if b.must_change_password.is_some() && b.password.is_none() {
        return Err(ApiError::bad_request(
            "must_change_password goes with a new password",
        ));
    }
    // A request is applied whole or not at all as far as the checks can tell: the rules of
    // the new password are checked first, then the account changes (which can be refused as
    // the last administrator), and only then the password is reset. A refused request has
    // changed nothing (and ended no session).
    if let Some(pw) = &b.password {
        accounts::password::check_rules(&u.username, pw)
            .map_err(|e| account_error(&AccountError::Password(e)))?;
    }
    let changes = new_role.is_some()
        || b.disabled.is_some()
        || b.display_name.is_some()
        || b.locale.is_some();
    let before = u.clone();
    let mut u = u;
    if changes {
        u = st
            .auth
            .update_user(
                &u.username,
                new_role,
                b.disabled,
                b.display_name,
                b.locale,
                s.username(),
            )
            .await
            .map_err(|e| account_error(&e))?;
        tracing::info!(user = %u.username, role = %u.role, disabled = u.disabled, by = s.username(), "web: account changed");
        // the sessions of a disabled or demoted account are gone (AuthService): so are its
        // connections
        if (u.disabled && !before.disabled) || u.role != before.role {
            st.events.revoke_user(u.id);
        }
    }
    if let Some(pw) = &b.password {
        st.auth
            .set_password(
                &u.username,
                pw,
                b.must_change_password.unwrap_or(true),
                s.username(),
            )
            .await
            .map_err(|e| account_error(&e))?;
        tracing::info!(user = %u.username, by = s.username(), "web: password reset");
        st.events.revoke_user(u.id);
        // (read it again for must_change_password)
        u = by_id(&st, &id).await?;
    }
    Ok(Json(json!({ "user": user_json(&u) })))
}

async fn delete(
    State(st): State<AppState>,
    s: Session,
    UrlPath(id): UrlPath<String>,
) -> Result<StatusCode, ApiError> {
    s.need(Capability::ManageUsers)?;
    let u = by_id(&st, &id).await?;
    st.auth
        .delete_user(&u.username, s.username())
        .await
        .map_err(|e| account_error(&e))?;
    st.events.revoke_user(u.id);
    tracing::info!(user = %u.username, by = s.username(), "web: account deleted");
    Ok(StatusCode::NO_CONTENT)
}
