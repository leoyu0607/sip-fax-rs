//! [`AuthService`]: login, sessions and account management for the web interface and the CLI.
//!
//! Everything here takes the clock from [`store::Clock`], so lock-outs and session lifetimes are
//! tested without waiting. Passwords are hashed on the blocking pool.
//!
//! API for the REST layer (M4b):
//!
//! * [`AuthService::login`] -> [`LoginOk`] (cookie `sipfax_session` = `token`, header
//!   `X-Sipfax-Csrf` = `csrf`), errors with fixed codes ([`LoginError::code`]);
//! * [`AuthService::authenticate`] on every request -> [`Authenticated`], then
//!   [`Authenticated::require`] with the route's [`Capability`]; for requests that change state,
//!   [`AuthService::csrf_ok`];
//! * [`AuthService::logout`], [`AuthService::change_password`];
//! * account management: [`AuthService::create_user`], [`AuthService::update_user`],
//!   [`AuthService::set_password`], [`AuthService::delete_user`], [`AuthService::list_users`]
//!   (each takes the acting operator for the audit trail and refuses to remove the last active
//!   administrator).

use std::path::PathBuf;
use std::sync::Arc;

use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64URL;
use chrono::{DateTime, Duration, Utc};
use ring::digest;
use ring::rand::{SecureRandom, SystemRandom};
use store::{
    Clock, ConfigAudit, FaxStore, NewSession, NewUser, StoreSource, UserRow, UserUpdate,
};

use crate::password::{self, PasswordError};
use crate::role::{Capability, Role};
use crate::throttle::{Block, GuardConfig, LoginGuard};

/// Name of the session cookie.
pub const COOKIE_NAME: &str = "sipfax_session";
/// Name of the CSRF header.
pub const CSRF_HEADER: &str = "X-Sipfax-Csrf";

/// Lifetimes.
#[derive(Debug, Clone, Copy)]
pub struct AuthConfig {
    /// A session that was not used for this long ends (default 8 hours, `[web] session_idle_hours`).
    pub idle: Duration,
    /// A session ends this long after the login, however busy (default 7 days).
    pub absolute: Duration,
    /// Login protection.
    pub guard: GuardConfig,
}

impl Default for AuthConfig {
    fn default() -> Self {
        AuthConfig {
            idle: Duration::hours(8),
            absolute: Duration::days(7),
            guard: GuardConfig::default(),
        }
    }
}

/// An account as the interface sees it (never the hash).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserInfo {
    /// Row id.
    pub id: i64,
    /// Login name.
    pub username: String,
    /// Display name.
    pub display_name: Option<String>,
    /// Role.
    pub role: Role,
    /// UI language.
    pub locale: Option<String>,
    /// May not log in.
    pub disabled: bool,
    /// Has to choose a new password first.
    pub must_change_password: bool,
    /// Last successful login.
    pub last_login_at: Option<DateTime<Utc>>,
    /// Created.
    pub created_at: DateTime<Utc>,
}

impl UserInfo {
    fn from_row(r: &UserRow) -> UserInfo {
        UserInfo {
            id: r.id,
            username: r.username.clone(),
            display_name: r.display_name.clone(),
            // an unknown role in the database gets the least privilege
            role: Role::parse(&r.role).unwrap_or(Role::Agent),
            locale: r.locale.clone(),
            disabled: r.disabled,
            must_change_password: r.must_change_password,
            last_login_at: r.last_login_at,
            created_at: r.created_at,
        }
    }
}

/// An account to create.
#[derive(Debug, Clone, Copy)]
pub struct NewAccount<'a> {
    /// Login name (`a-z 0-9 . _ -`).
    pub username: &'a str,
    /// Role.
    pub role: Role,
    /// Initial password (checked against the rules).
    pub password: &'a str,
    /// Display name.
    pub display_name: Option<&'a str>,
    /// UI language.
    pub locale: Option<&'a str>,
    /// The first login must choose another password.
    pub must_change: bool,
}

/// A successful login.
#[derive(Debug, Clone)]
pub struct LoginOk {
    /// The session token (cookie value). Shown once; only its hash is stored.
    pub token: String,
    /// The CSRF token for the `X-Sipfax-Csrf` header.
    pub csrf: String,
    /// The account.
    pub user: UserInfo,
    /// When the session ends at the latest.
    pub expires_at: DateTime<Utc>,
}

/// Why a login failed. [`LoginError::code`] is the fixed API code.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LoginError {
    /// Unknown account, wrong password or disabled account (deliberately the same answer).
    #[error("invalid user name or password")]
    InvalidCredentials,
    /// Too many failures for this account.
    #[error("the account is locked for {retry_after_secs} seconds")]
    Locked {
        /// Seconds until it unlocks.
        retry_after_secs: i64,
    },
    /// Too many attempts from this address (HTTP 429).
    #[error("too many attempts, try again in {retry_after_secs} seconds")]
    RateLimited {
        /// Seconds until the next attempt is looked at.
        retry_after_secs: i64,
    },
    /// The database cannot be reached.
    #[error("the database is not available: {0}")]
    Unavailable(String),
}

impl LoginError {
    /// The fixed error code.
    pub fn code(&self) -> &'static str {
        match self {
            LoginError::InvalidCredentials => "invalid_credentials",
            LoginError::Locked { .. } => "account_locked",
            LoginError::RateLimited { .. } => "rate_limited",
            LoginError::Unavailable(_) => "unavailable",
        }
    }
}

/// Why a session was not accepted.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SessionError {
    /// No session cookie, or an unknown token.
    #[error("not logged in")]
    Missing,
    /// Idle or absolute lifetime exceeded.
    #[error("the session has expired")]
    Expired,
    /// The account was disabled or deleted.
    #[error("the account is disabled")]
    Disabled,
    /// The database cannot be reached.
    #[error("the database is not available: {0}")]
    Unavailable(String),
}

impl SessionError {
    /// The fixed error code.
    pub fn code(&self) -> &'static str {
        match self {
            SessionError::Missing => "unauthenticated",
            SessionError::Expired => "session_expired",
            SessionError::Disabled => "account_disabled",
            SessionError::Unavailable(_) => "unavailable",
        }
    }
}

/// A request that carries a valid session.
#[derive(Debug, Clone)]
pub struct Authenticated {
    /// The account.
    pub user: UserInfo,
    /// The session row id (WebSocket connections are tied to it).
    pub session_id: i64,
    /// The CSRF token that belongs to this session.
    pub csrf: String,
}

/// The account is not allowed to do this (HTTP 403, code `forbidden`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("the {role} role is not allowed to do this")]
pub struct Forbidden {
    /// The role that was refused.
    pub role: Role,
}

impl Authenticated {
    /// Checks the role against the capability a route needs.
    ///
    /// # Errors
    /// [`Forbidden`].
    pub fn require(&self, cap: Capability) -> Result<(), Forbidden> {
        if self.user.role.can(cap) {
            Ok(())
        } else {
            Err(Forbidden {
                role: self.user.role,
            })
        }
    }
}

/// Why an account operation was refused. [`AccountError::code`] is the fixed API code.
#[derive(Debug, thiserror::Error)]
pub enum AccountError {
    /// The password breaks a rule.
    #[error(transparent)]
    Password(#[from] PasswordError),
    /// The old password given to a password change is wrong.
    #[error("the current password is wrong")]
    WrongPassword,
    /// The account name is not 1-64 characters of `a-z 0-9 . _ -`.
    #[error("the account name must be 1-64 characters of a-z, 0-9, `.`, `_` or `-`")]
    BadUsername,
    /// The role is not `admin` or `agent`.
    #[error("the role must be admin or agent")]
    BadRole,
    /// The name is taken.
    #[error("the account name is already in use")]
    Duplicate,
    /// No such account.
    #[error("no such account")]
    NotFound,
    /// The change would leave no enabled administrator (HTTP 409).
    #[error("this would leave no enabled administrator")]
    LastAdmin,
    /// The database cannot be reached or failed.
    #[error("database: {0}")]
    Store(String),
}

impl AccountError {
    /// The fixed error code.
    pub fn code(&self) -> &'static str {
        match self {
            AccountError::Password(p) => p.code(),
            AccountError::WrongPassword => "wrong_password",
            AccountError::BadUsername => "bad_username",
            AccountError::BadRole => "bad_role",
            AccountError::Duplicate => "username_taken",
            AccountError::NotFound => "not_found",
            AccountError::LastAdmin => "last_admin",
            AccountError::Store(_) => "unavailable",
        }
    }
}

fn store_err(e: store::Error) -> AccountError {
    match e {
        store::Error::Duplicate(_) => AccountError::Duplicate,
        store::Error::NotFound(_) => AccountError::NotFound,
        other => AccountError::Store(other.to_string()),
    }
}

/// A valid account name.
pub fn valid_username(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 64
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

fn sha256_hex(data: &[u8]) -> String {
    let d = digest::digest(&digest::SHA256, data);
    d.as_ref().iter().map(|b| format!("{b:02x}")).collect()
}

/// The CSRF token of a session token.
pub fn csrf_for(token: &str) -> String {
    sha256_hex(format!("sipfax-csrf:{token}").as_bytes())
}

fn new_token() -> String {
    let mut b = [0u8; 32];
    SystemRandom::new()
        .fill(&mut b)
        .expect("the system random generator works");
    B64URL.encode(b)
}

/// Accounts, login and sessions.
pub struct AuthService {
    src: Arc<dyn StoreSource>,
    clock: Arc<dyn Clock>,
    cfg: AuthConfig,
    guard: LoginGuard,
    /// A hash to verify against when the account does not exist (same cost, same time).
    dummy_hash: String,
    initial_file: std::sync::Mutex<Option<PathBuf>>,
}

impl AuthService {
    /// A service over `src` using `clock` for every time decision.
    pub fn new(src: Arc<dyn StoreSource>, clock: Arc<dyn Clock>, cfg: AuthConfig) -> AuthService {
        AuthService {
            src,
            guard: LoginGuard::new(cfg.guard, clock.clone()),
            clock,
            cfg,
            dummy_hash: password::hash("not the password of anybody"),
            initial_file: std::sync::Mutex::new(None),
        }
    }

    async fn store(&self) -> Result<Arc<dyn FaxStore>, store::Error> {
        self.src.store().await
    }

    async fn hash(pw: &str) -> String {
        let pw = pw.to_string();
        tokio::task::spawn_blocking(move || password::hash(&pw))
            .await
            .expect("hashing does not panic")
    }

    async fn verify(phc: String, pw: String) -> bool {
        tokio::task::spawn_blocking(move || password::verify(&phc, &pw))
            .await
            .unwrap_or(false)
    }

    // ------------------------------------------------------------------ login and sessions

    /// Checks the credentials and opens a session.
    ///
    /// The same [`LoginError::InvalidCredentials`] answers an unknown account, a wrong password
    /// and a disabled account, and all three take the time of one password hash.
    ///
    /// # Errors
    /// See [`LoginError`].
    pub async fn login(
        &self,
        username: &str,
        pw: &str,
        ip: &str,
        user_agent: Option<&str>,
    ) -> Result<LoginOk, LoginError> {
        let name = username.trim().to_ascii_lowercase();
        self.guard.check(&name, ip).map_err(|b| match b {
            Block::RateLimited { retry_after_secs } => LoginError::RateLimited { retry_after_secs },
            Block::Locked { retry_after_secs } => LoginError::Locked { retry_after_secs },
        })?;
        let st = self
            .store()
            .await
            .map_err(|e| LoginError::Unavailable(e.to_string()))?;
        let row = st
            .user_get_by_name(&name)
            .await
            .map_err(|e| LoginError::Unavailable(e.to_string()))?;
        let (phc, usable) = match &row {
            Some(r) => (r.password_hash.clone(), !r.disabled),
            None => (self.dummy_hash.clone(), false),
        };
        let matches = Self::verify(phc, pw.to_string()).await;
        let Some(row) = row.filter(|_| usable && matches) else {
            self.guard.record_failure(&name);
            return Err(LoginError::InvalidCredentials);
        };
        self.guard.record_success(&name);
        let now = self.clock.now();
        let token = new_token();
        let expires_at = now + self.cfg.absolute;
        st.session_create(&NewSession {
            token_hash: sha256_hex(token.as_bytes()),
            user_id: row.id,
            created_at: now,
            expires_at,
            ip: Some(ip.to_string()),
            user_agent: user_agent.map(str::to_string),
        })
        .await
        .map_err(|e| LoginError::Unavailable(e.to_string()))?;
        st.user_touch_login(row.id, now)
            .await
            .map_err(|e| LoginError::Unavailable(e.to_string()))?;
        let mut user = UserInfo::from_row(&row);
        user.last_login_at = Some(now);
        Ok(LoginOk {
            csrf: csrf_for(&token),
            token,
            user,
            expires_at,
        })
    }

    /// Validates a session token (the cookie value) and records its use.
    ///
    /// # Errors
    /// See [`SessionError`]; an expired session is deleted on the way.
    pub async fn authenticate(&self, token: &str) -> Result<Authenticated, SessionError> {
        let st = self
            .store()
            .await
            .map_err(|e| SessionError::Unavailable(e.to_string()))?;
        let hash = sha256_hex(token.as_bytes());
        let unavailable = |e: store::Error| SessionError::Unavailable(e.to_string());
        let s = st
            .session_get(&hash)
            .await
            .map_err(unavailable)?
            .ok_or(SessionError::Missing)?;
        let now = self.clock.now();
        if now >= s.expires_at || now - s.last_seen_at >= self.cfg.idle {
            let _ = st.session_delete(&hash).await;
            return Err(SessionError::Expired);
        }
        let user = st.user_get(s.user_id).await.map_err(unavailable)?;
        let Some(user) = user.filter(|u| !u.disabled) else {
            let _ = st.sessions_delete_for_user(s.user_id).await;
            return Err(SessionError::Disabled);
        };
        st.session_touch(s.id, now).await.map_err(unavailable)?;
        Ok(Authenticated {
            user: UserInfo::from_row(&user),
            session_id: s.id,
            csrf: csrf_for(token),
        })
    }

    /// Whether the `X-Sipfax-Csrf` header value belongs to this session (constant time).
    pub fn csrf_ok(token: &str, header: Option<&str>) -> bool {
        let want = csrf_for(token);
        match header {
            Some(h) => {
                let (a, b) = (want.as_bytes(), h.as_bytes());
                // both are hex digests of the same length: fold the differences, no early exit
                a.len() == b.len() && a.iter().zip(b).fold(0u8, |acc, (x, y)| acc | (x ^ y)) == 0
            }
            None => false,
        }
    }

    /// Ends one session.
    pub async fn logout(&self, token: &str) -> Result<(), AccountError> {
        let st = self.store().await.map_err(store_err)?;
        st.session_delete(&sha256_hex(token.as_bytes()))
            .await
            .map(|_| ())
            .map_err(store_err)
    }

    /// Deletes sessions that passed their absolute lifetime (call now and then).
    pub async fn purge_expired_sessions(&self) -> Result<u64, AccountError> {
        let st = self.store().await.map_err(store_err)?;
        st.sessions_delete_expired(self.clock.now())
            .await
            .map_err(store_err)
    }

    // ------------------------------------------------------------------ passwords

    /// The account changes its own password. Every session of the account ends and a new one is
    /// opened (the caller sets the new cookie); `must_change_password` is cleared, and the file
    /// `conf/initial-admin-password` is removed once the first administrator has chosen their
    /// own password.
    ///
    /// # Errors
    /// See [`AccountError`].
    pub async fn change_password(
        &self,
        user_id: i64,
        old: &str,
        new: &str,
        ip: &str,
        user_agent: Option<&str>,
    ) -> Result<LoginOk, AccountError> {
        let st = self.store().await.map_err(store_err)?;
        let row = st
            .user_get(user_id)
            .await
            .map_err(store_err)?
            .ok_or(AccountError::NotFound)?;
        if !Self::verify(row.password_hash.clone(), old.to_string()).await {
            return Err(AccountError::WrongPassword);
        }
        password::check_rules(&row.username, new)?;
        let hash = Self::hash(new).await;
        st.user_update(
            user_id,
            &UserUpdate {
                password_hash: Some(hash),
                must_change_password: Some(false),
                ..UserUpdate::default()
            },
        )
        .await
        .map_err(store_err)?;
        st.sessions_delete_for_user(user_id).await.map_err(store_err)?;
        self.audit(&*st, &row.username, "password_change", &row.username).await;
        self.remove_initial_file_for(&row.username);
        let now = self.clock.now();
        let token = new_token();
        let expires_at = now + self.cfg.absolute;
        st.session_create(&NewSession {
            token_hash: sha256_hex(token.as_bytes()),
            user_id,
            created_at: now,
            expires_at,
            ip: Some(ip.to_string()),
            user_agent: user_agent.map(str::to_string),
        })
        .await
        .map_err(store_err)?;
        let mut user = UserInfo::from_row(&row);
        user.must_change_password = false;
        Ok(LoginOk {
            csrf: csrf_for(&token),
            token,
            user,
            expires_at,
        })
    }

    /// An administrator (or the CLI) sets a password. All sessions of the account end; with
    /// `must_change` the next login has to choose another one.
    ///
    /// # Errors
    /// See [`AccountError`].
    pub async fn set_password(
        &self,
        username: &str,
        new: &str,
        must_change: bool,
        by: &str,
    ) -> Result<(), AccountError> {
        let st = self.store().await.map_err(store_err)?;
        let row = st
            .user_get_by_name(username)
            .await
            .map_err(store_err)?
            .ok_or(AccountError::NotFound)?;
        password::check_rules(&row.username, new)?;
        let hash = Self::hash(new).await;
        st.user_update(
            row.id,
            &UserUpdate {
                password_hash: Some(hash),
                must_change_password: Some(must_change),
                ..UserUpdate::default()
            },
        )
        .await
        .map_err(store_err)?;
        st.sessions_delete_for_user(row.id).await.map_err(store_err)?;
        self.guard.reset(&row.username);
        self.audit(&*st, &row.username, "password_reset", by).await;
        Ok(())
    }

    // ------------------------------------------------------------------ account management

    async fn audit(&self, st: &dyn FaxStore, user: &str, action: &str, by: &str) {
        let audit = ConfigAudit {
            action: action.chars().take(16).collect(),
            before: None,
            after: None,
        };
        if let Err(e) = st.config_audit_add(&format!("user:{user}"), by, &audit).await {
            tracing::warn!("audit row for {action} of {user} not written: {e}");
        }
    }

    /// All accounts.
    pub async fn list_users(&self) -> Result<Vec<UserInfo>, AccountError> {
        let st = self.store().await.map_err(store_err)?;
        Ok(st
            .user_list()
            .await
            .map_err(store_err)?
            .iter()
            .map(UserInfo::from_row)
            .collect())
    }

    /// One account.
    pub async fn get_user(&self, username: &str) -> Result<UserInfo, AccountError> {
        let st = self.store().await.map_err(store_err)?;
        st.user_get_by_name(username)
            .await
            .map_err(store_err)?
            .map(|r| UserInfo::from_row(&r))
            .ok_or(AccountError::NotFound)
    }

    /// Creates an account. New accounts made by an administrator normally have
    /// `must_change = true`.
    ///
    /// # Errors
    /// See [`AccountError`].
    pub async fn create_user(&self, a: &NewAccount<'_>, by: &str) -> Result<UserInfo, AccountError> {
        if !valid_username(a.username) {
            return Err(AccountError::BadUsername);
        }
        password::check_rules(a.username, a.password)?;
        let hash = Self::hash(a.password).await;
        let st = self.store().await.map_err(store_err)?;
        let row = st
            .user_create(&NewUser {
                username: a.username.to_string(),
                display_name: a.display_name.map(str::to_string),
                role: a.role.as_str().to_string(),
                password_hash: hash,
                locale: a.locale.map(str::to_string),
                must_change_password: a.must_change,
            })
            .await
            .map_err(store_err)?;
        self.audit(&*st, a.username, "user_add", by).await;
        Ok(UserInfo::from_row(&row))
    }

    async fn would_remove_last_admin(
        st: &dyn FaxStore,
        row: &UserRow,
        becomes_admin: bool,
        becomes_enabled: bool,
    ) -> Result<bool, AccountError> {
        let is_active_admin = row.role == "admin" && !row.disabled;
        let stays_active_admin = becomes_admin && becomes_enabled;
        if !is_active_admin || stays_active_admin {
            return Ok(false);
        }
        Ok(st.user_count_active_admins().await.map_err(store_err)? <= 1)
    }

    /// Changes the role, the disabled flag, the display name or the language of an account.
    /// Disabling ends its sessions. The last enabled administrator cannot be disabled or
    /// demoted.
    ///
    /// # Errors
    /// See [`AccountError`].
    pub async fn update_user(
        &self,
        username: &str,
        role: Option<Role>,
        disabled: Option<bool>,
        display_name: Option<Option<String>>,
        locale: Option<Option<String>>,
        by: &str,
    ) -> Result<UserInfo, AccountError> {
        let st = self.store().await.map_err(store_err)?;
        let row = st
            .user_get_by_name(username)
            .await
            .map_err(store_err)?
            .ok_or(AccountError::NotFound)?;
        let becomes_admin = role.map_or(row.role == "admin", |r| r == Role::Admin);
        let becomes_enabled = disabled.map_or(!row.disabled, |d| !d);
        if Self::would_remove_last_admin(&*st, &row, becomes_admin, becomes_enabled).await? {
            return Err(AccountError::LastAdmin);
        }
        let upd = st
            .user_update(
                row.id,
                &UserUpdate {
                    role: role.map(|r| r.as_str().to_string()),
                    disabled,
                    display_name,
                    locale,
                    ..UserUpdate::default()
                },
            )
            .await
            .map_err(store_err)?;
        if disabled == Some(true) || (role.is_some() && role != Role::parse(&row.role)) {
            // sessions carry no role, but a disabled or demoted account must not keep any
            st.sessions_delete_for_user(row.id).await.map_err(store_err)?;
        }
        let action = match disabled {
            Some(true) => "user_disable",
            Some(false) => "user_enable",
            None => "user_update",
        };
        self.audit(&*st, username, action, by).await;
        Ok(UserInfo::from_row(&upd))
    }

    /// Deletes an account and its sessions (not the last enabled administrator).
    ///
    /// # Errors
    /// See [`AccountError`].
    pub async fn delete_user(&self, username: &str, by: &str) -> Result<(), AccountError> {
        let st = self.store().await.map_err(store_err)?;
        let row = st
            .user_get_by_name(username)
            .await
            .map_err(store_err)?
            .ok_or(AccountError::NotFound)?;
        if Self::would_remove_last_admin(&*st, &row, false, false).await? {
            return Err(AccountError::LastAdmin);
        }
        st.user_delete(row.id).await.map_err(store_err)?;
        self.audit(&*st, username, "user_delete", by).await;
        Ok(())
    }

    // ------------------------------------------------------------------ first administrator

    /// When there is no account at all, creates `admin` with a random password that has to be
    /// changed at the first login, and writes the password to `file` (mode 0600). The password
    /// never goes to the log. Returns the path when an account was created.
    pub async fn ensure_initial_admin(&self, file: PathBuf) -> Result<Option<PathBuf>, AccountError> {
        let st = self.store().await.map_err(store_err)?;
        if st.user_count().await.map_err(store_err)? > 0 {
            return Ok(None);
        }
        let pw = password::random_password(16);
        let account = NewAccount {
            username: "admin",
            role: Role::Admin,
            password: &pw,
            display_name: Some("Administrator"),
            locale: None,
            must_change: true,
        };
        match self.create_user(&account, "system").await {
            Ok(_) => {}
            // another instance created it first
            Err(AccountError::Duplicate) => return Ok(None),
            Err(e) => return Err(e),
        }
        let text = format!("{pw}\n");
        if let Err(e) = crate_write_private(&file, text.as_bytes()) {
            tracing::error!(
                "the initial administrator password could not be written to {}: {e}; \
                 set one with `sip-faxd user passwd admin`",
                file.display()
            );
            return Ok(None);
        }
        *self.initial_file.lock().unwrap_or_else(|e| e.into_inner()) = Some(file.clone());
        Ok(Some(file))
    }

    /// Tells the service where `initial-admin-password` is, so that the first password change
    /// of `admin` deletes it even after a restart.
    pub fn watch_initial_password_file(&self, file: PathBuf) {
        *self.initial_file.lock().unwrap_or_else(|e| e.into_inner()) = Some(file);
    }

    fn remove_initial_file_for(&self, username: &str) {
        if username != "admin" {
            return;
        }
        let f = self.initial_file.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(f) = f {
            let _ = std::fs::remove_file(f);
        }
    }
}

/// Mode 0600 file write (the same as the secret key's).
fn crate_write_private(path: &std::path::Path, data: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let _ = std::fs::remove_file(path);
    #[cfg(unix)]
    {
        use std::io::Write;
        use std::os::unix::fs::OpenOptionsExt;
        let mut f = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(path)?;
        f.write_all(data)
    }
    #[cfg(not(unix))]
    {
        std::fs::write(path, data)
    }
}
