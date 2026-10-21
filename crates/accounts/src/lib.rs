//! Accounts, roles, login protection and sessions of the web interface and the CLI (M4a,
//! `docs/design/m4-web.md` section 2).
//!
//! * [`password`]: argon2id hashing and the password rules;
//! * [`role`]: [`Role`] and the [`Capability`] each route needs (the permission matrix);
//! * [`throttle`]: account lock-out and per-address rate limit, with an injected clock;
//! * [`auth`]: [`AuthService`], the API of the REST layer.

#![deny(missing_docs)]

pub mod auth;
pub mod password;
pub mod role;
pub mod throttle;

pub use auth::{
    AccountError, AuthConfig, AuthService, Authenticated, COOKIE_NAME, CSRF_HEADER, Forbidden,
    LoginError, LoginOk, NewAccount, SessionError, UserInfo, csrf_for, valid_username,
};
pub use password::PasswordError;
pub use role::{Capability, Role};
pub use throttle::{Block, GuardConfig, LoginGuard};
