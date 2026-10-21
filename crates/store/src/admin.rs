//! Runtime configuration, accounts and sessions (M4a, migration 0002). Everything here is stored
//! once, in plain SQL, for all four backends (see `sqlstore_admin.rs`); the rules (validation,
//! encryption of secrets, password hashing, lock-out, session lifetime) live in the
//! `runtime-config` and `accounts` crates.

use crate::error::Result;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

/// One row of `config_sections`: the JSON body of a configuration section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigRow {
    /// Section name (`sip`, `media`, `call`, `capture`, `delivery`, `retention`, `routes`,
    /// `trunk:<name>`).
    pub name: String,
    /// The section as JSON text.
    pub body: String,
    /// Optimistic-lock version, starts at 1 and grows by one per change.
    pub version: i32,
    /// Who changed it last.
    pub updated_by: Option<String>,
    /// When.
    pub updated_at: DateTime<Utc>,
}

/// What a change looked like, written to `config_audit` together with the change. The caller has
/// already replaced secrets by `"***"`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigAudit {
    /// `create`, `update`, `delete`, `import`.
    pub action: String,
    /// The body before (None for a new section).
    pub before: Option<String>,
    /// The body after (None for a deleted section).
    pub after: Option<String>,
}

/// One row of `config_audit`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigAuditRow {
    /// Row id (ascending in time).
    pub id: i64,
    /// Section name.
    pub section: String,
    /// `create`, `update`, `delete`, `import`.
    pub action: String,
    /// Body before, secrets masked.
    pub before_json: Option<String>,
    /// Body after, secrets masked.
    pub after_json: Option<String>,
    /// Who: a username, `cli:<os user>` or `config-file`.
    pub by: String,
    /// When.
    pub at: DateTime<Utc>,
}

/// One row of `users`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UserRow {
    /// Row id.
    pub id: i64,
    /// Login name (lower case).
    pub username: String,
    /// Name shown in the UI.
    pub display_name: Option<String>,
    /// `admin` or `agent`.
    pub role: String,
    /// argon2id PHC string.
    pub password_hash: String,
    /// UI language (`zh-TW`, `en`), None = follow the browser.
    pub locale: Option<String>,
    /// The account may not log in.
    pub disabled: bool,
    /// The next login must change the password first.
    pub must_change_password: bool,
    /// Creation time.
    pub created_at: DateTime<Utc>,
    /// Last change.
    pub updated_at: DateTime<Utc>,
    /// Last successful login.
    pub last_login_at: Option<DateTime<Utc>>,
}

/// A user to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewUser {
    /// Login name.
    pub username: String,
    /// Display name.
    pub display_name: Option<String>,
    /// `admin` or `agent`.
    pub role: String,
    /// argon2id PHC string.
    pub password_hash: String,
    /// UI language.
    pub locale: Option<String>,
    /// Force a password change at the first login.
    pub must_change_password: bool,
}

/// Changes to a user; `None` leaves the column alone.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct UserUpdate {
    /// New display name (`Some(None)` clears it).
    pub display_name: Option<Option<String>>,
    /// New role.
    pub role: Option<String>,
    /// New password hash.
    pub password_hash: Option<String>,
    /// New UI language (`Some(None)` clears it).
    pub locale: Option<Option<String>>,
    /// Disable / enable.
    pub disabled: Option<bool>,
    /// Force / release the password change.
    pub must_change_password: Option<bool>,
}

/// One row of `sessions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionRow {
    /// Row id.
    pub id: i64,
    /// SHA-256 (hex) of the token in the cookie.
    pub token_hash: String,
    /// The owner.
    pub user_id: i64,
    /// Login time.
    pub created_at: DateTime<Utc>,
    /// Last use (idle timeout is measured from here).
    pub last_seen_at: DateTime<Utc>,
    /// Absolute end of life.
    pub expires_at: DateTime<Utc>,
    /// Client address at login.
    pub ip: Option<String>,
    /// Client `User-Agent` at login (cut to 255 characters).
    pub user_agent: Option<String>,
}

/// A session to create.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewSession {
    /// SHA-256 (hex) of the token.
    pub token_hash: String,
    /// The owner.
    pub user_id: i64,
    /// Login time.
    pub created_at: DateTime<Utc>,
    /// Absolute end of life.
    pub expires_at: DateTime<Utc>,
    /// Client address.
    pub ip: Option<String>,
    /// Client user agent.
    pub user_agent: Option<String>,
}

/// The M4a part of the store API.
#[async_trait]
pub trait AdminStore: Send + Sync {
    /// One configuration section.
    async fn config_get(&self, name: &str) -> Result<Option<ConfigRow>>;
    /// All sections, ordered by name.
    async fn config_list(&self) -> Result<Vec<ConfigRow>>;
    /// Create (`expected_version = None`, fails with `Conflict` when it exists) or replace
    /// (`Some(v)`, fails with `Conflict` when the stored version is not `v` and `NotFound` when
    /// there is no such section) a section, bump its version and write the audit row.
    /// The change and its audit row are two statements: a crash in between loses the audit row,
    /// never the change.
    async fn config_put(
        &self,
        name: &str,
        body: &str,
        expected_version: Option<i32>,
        by: &str,
        audit: &ConfigAudit,
    ) -> Result<ConfigRow>;
    /// Delete a section (optimistic lock as above) and write the audit row.
    async fn config_delete(
        &self,
        name: &str,
        expected_version: i32,
        by: &str,
        audit: &ConfigAudit,
    ) -> Result<()>;
    /// Writes an audit row that is not tied to a section change (user management, for example
    /// `section = "user:alice"`, `action = "user_add"`).
    async fn config_audit_add(&self, section: &str, by: &str, audit: &ConfigAudit) -> Result<()>;
    /// The audit trail, newest first; `before_id` pages (exclusive).
    async fn config_audit_list(
        &self,
        limit: u32,
        before_id: Option<i64>,
    ) -> Result<Vec<ConfigAuditRow>>;

    /// Create a user. `Duplicate` when the name is taken.
    async fn user_create(&self, u: &NewUser) -> Result<UserRow>;
    /// By id.
    async fn user_get(&self, id: i64) -> Result<Option<UserRow>>;
    /// By login name (exact match; names are stored lower case).
    async fn user_get_by_name(&self, username: &str) -> Result<Option<UserRow>>;
    /// All users ordered by name.
    async fn user_list(&self) -> Result<Vec<UserRow>>;
    /// Apply changes; `NotFound` for an unknown id.
    async fn user_update(&self, id: i64, u: &UserUpdate) -> Result<UserRow>;
    /// Record a successful login.
    async fn user_touch_login(&self, id: i64, at: DateTime<Utc>) -> Result<()>;
    /// Delete the user and all their sessions. Returns whether a user was deleted.
    async fn user_delete(&self, id: i64) -> Result<bool>;
    /// Number of users (enabled or not).
    async fn user_count(&self) -> Result<u64>;
    /// Number of enabled administrators.
    async fn user_count_active_admins(&self) -> Result<u64>;

    /// Create a session; returns its id.
    async fn session_create(&self, s: &NewSession) -> Result<i64>;
    /// By token hash.
    async fn session_get(&self, token_hash: &str) -> Result<Option<SessionRow>>;
    /// Record use of the session.
    async fn session_touch(&self, id: i64, at: DateTime<Utc>) -> Result<()>;
    /// Delete one session. Returns whether it existed.
    async fn session_delete(&self, token_hash: &str) -> Result<bool>;
    /// Delete every session of a user (logout everywhere, password reset, disable).
    async fn sessions_delete_for_user(&self, user_id: i64) -> Result<u64>;
    /// Delete sessions whose absolute life ended before `now`.
    async fn sessions_delete_expired(&self, now: DateTime<Utc>) -> Result<u64>;
    /// Sessions of a user.
    async fn session_count_for_user(&self, user_id: i64) -> Result<u64>;

    /// Queue a new `pending` attempt (attempt + 1, due at `at`) for every delivery target of the
    /// fax whose latest attempt is `failed` or `giving_up`. Returns the new attempt ids (empty
    /// when nothing qualifies).
    async fn redeliver(&self, fax_id: i64, at: DateTime<Utc>) -> Result<Vec<i64>>;
}
