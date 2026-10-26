//! Accounts, roles, login protection and sessions of the web interface and the CLI (M4a,
//! `docs/design/m4-web.md` section 2).
//!
//! * [`password`]: argon2id hashing and the password rules;
//! * [`role`]: [`Role`] and the [`Capability`] each route needs (the permission matrix);
//! * [`throttle`]: account lock-out and per-address rate limit, with an injected clock;
//! * [`auth`]: [`AuthService`], the API of the REST layer;
//! * [`apikey`]: API keys for other systems (M6a).

#![deny(missing_docs)]

pub mod apikey;
pub mod auth;
pub mod password;
pub mod role;
pub mod throttle;

pub use apikey::{
    ApiKeyAdminError, ApiKeyAuth, ApiKeyError, ApiKeyInfo, CreatedKey, KEY_PREFIX, looks_like_key,
    valid_key_name,
};
pub use auth::{
    AccountError, AuthConfig, AuthService, Authenticated, COOKIE_NAME, CSRF_HEADER, Forbidden,
    LoginError, LoginOk, NewAccount, SessionError, UserInfo, csrf_for, valid_username,
};
pub use password::PasswordError;
pub use role::{Capability, Role};
pub use throttle::{Block, GuardConfig, LoginGuard};
