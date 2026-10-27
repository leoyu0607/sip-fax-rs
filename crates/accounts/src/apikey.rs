//! API keys (M6a, ADR-070): programs of other systems read the faxes with
//! `Authorization: Bearer <key>` instead of a browser session.
//!
//! A key is `sfx_` and 43 base64url characters (32 random bytes). Only its SHA-256 is stored,
//! with its first [`PREFIX_LEN`] characters to recognise it in a list. Every key may read the
//! faxes ([`Capability::ViewFaxes`]); a key created with `write` may also set the read flag and
//! the note ([`Capability::EditFaxMeta`]). Nothing else: the REST layer accepts keys on the fax
//! routes only. Keys are revoked, never deleted, and their names are never reused.

use chrono::{DateTime, Duration, Utc};
use store::{ApiKeyRow, ConfigAudit, FaxStore, NewApiKey};

use crate::auth::{AuthService, new_token, sha256_hex};
use crate::role::Capability;

/// What every key starts with.
pub const KEY_PREFIX: &str = "sfx_";
/// Characters of the key kept to recognise it (`sfx_` and 8 more).
pub const PREFIX_LEN: usize = 12;
/// Scope of every key.
pub const SCOPE_READ: &str = "fax:read";
/// Scope of a key that may change the read flag and the note.
pub const SCOPE_WRITE: &str = "fax:write";
/// The last use is written at most this often per key.
pub const TOUCH_EVERY_SECS: i64 = 60;

/// A key as the list shows it (never the key itself).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyInfo {
    /// Row id.
    pub id: i64,
    /// Unique name.
    pub name: String,
    /// The first characters of the key.
    pub prefix: String,
    /// `["fax:read"]` or `["fax:read", "fax:write"]`.
    pub scopes: Vec<String>,
    /// Who created it.
    pub created_by: String,
    /// When.
    pub created_at: DateTime<Utc>,
    /// End of validity.
    pub expires_at: Option<DateTime<Utc>>,
    /// When it was revoked.
    pub revoked_at: Option<DateTime<Utc>>,
    /// By whom.
    pub revoked_by: Option<String>,
    /// Last use (to the minute).
    pub last_used_at: Option<DateTime<Utc>>,
    /// Client address of the last use.
    pub last_used_ip: Option<String>,
}

impl ApiKeyInfo {
    fn from_row(r: &ApiKeyRow) -> ApiKeyInfo {
        ApiKeyInfo {
            id: r.id,
            name: r.name.clone(),
            prefix: r.prefix.clone(),
            scopes: r.scopes.split(',').map(str::to_string).collect(),
            created_by: r.created_by.clone(),
            created_at: r.created_at,
            expires_at: r.expires_at,
            revoked_at: r.revoked_at,
            revoked_by: r.revoked_by.clone(),
            last_used_at: r.last_used_at,
            last_used_ip: r.last_used_ip.clone(),
        }
    }

    /// Whether the key can be used at `now`.
    pub fn active(&self, now: DateTime<Utc>) -> bool {
        self.revoked_at.is_none() && self.expires_at.is_none_or(|e| now < e)
    }
}

/// A request made with a valid key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiKeyAuth {
    /// Row id.
    pub id: i64,
    /// Name of the key.
    pub name: String,
    /// It may change the read flag and the note.
    pub write: bool,
}

impl ApiKeyAuth {
    /// Whether the key may do `cap`.
    pub fn allows(&self, cap: Capability) -> bool {
        match cap {
            Capability::ViewFaxes => true,
            Capability::EditFaxMeta => self.write,
            _ => false,
        }
    }

    /// How a change made with the key is recorded (`read_changed_by`, `note_updated_by`).
    pub fn actor(&self) -> String {
        format!("api:{}", self.name)
    }
}

/// Why a key was not accepted. Wrong, unknown, revoked and expired keys are one error on
/// purpose: a caller learns nothing about which keys exist.
#[derive(Debug, thiserror::Error)]
pub enum ApiKeyError {
    /// Not a valid key (HTTP 401, code `invalid_api_key`).
    #[error("the API key is not valid")]
    Invalid,
    /// The database cannot be reached (HTTP 503).
    #[error("database: {0}")]
    Unavailable(String),
}

/// Why managing a key was refused. [`ApiKeyAdminError::code`] is the fixed API code.
#[derive(Debug, thiserror::Error)]
pub enum ApiKeyAdminError {
    /// The name is not 1-48 characters of `a-z 0-9 . _ -`.
    #[error("the key name must be 1-48 characters of a-z, 0-9, `.`, `_` or `-`")]
    BadName,
    /// The name is taken (also by a revoked key).
    #[error("a key with this name exists already (names of revoked keys are not reused)")]
    Duplicate,
    /// The end of validity is not in the future.
    #[error("the expiry must be in the future")]
    BadExpiry,
    /// No such key.
    #[error("no such key")]
    NotFound,
    /// The key is revoked already.
    #[error("the key is revoked already")]
    AlreadyRevoked,
    /// The database cannot be reached or failed.
    #[error("database: {0}")]
    Store(String),
}

impl ApiKeyAdminError {
    /// The fixed error code.
    pub fn code(&self) -> &'static str {
        match self {
            ApiKeyAdminError::BadName => "bad_name",
            ApiKeyAdminError::Duplicate => "name_taken",
            ApiKeyAdminError::BadExpiry => "bad_expiry",
            ApiKeyAdminError::NotFound => "not_found",
            ApiKeyAdminError::AlreadyRevoked => "already_revoked",
            ApiKeyAdminError::Store(_) => "unavailable",
        }
    }
}

fn admin_err(e: store::Error) -> ApiKeyAdminError {
    match e {
        store::Error::Duplicate(_) => ApiKeyAdminError::Duplicate,
        store::Error::NotFound(_) => ApiKeyAdminError::NotFound,
        other => ApiKeyAdminError::Store(other.to_string()),
    }
}

/// A valid key name (the same characters as an account name, at most 48).
pub fn valid_key_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 48
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '.' | '_' | '-'))
}

/// Whether `s` has the shape of a key (before it is looked up).
pub fn looks_like_key(s: &str) -> bool {
    s.len() == KEY_PREFIX.len() + 43
        && s.starts_with(KEY_PREFIX)
        && s[KEY_PREFIX.len()..]
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// A new key and how it is listed. The key is in `key` only, this once.
#[derive(Debug, Clone)]
pub struct CreatedKey {
    /// The key: shown to the administrator once, never stored.
    pub key: String,
    /// The key as listed.
    pub info: ApiKeyInfo,
}

impl AuthService {
    async fn key_audit(
        &self,
        st: &dyn FaxStore,
        name: &str,
        action: &str,
        by: &str,
        after: String,
    ) {
        let audit = ConfigAudit {
            action: action.to_string(),
            before: None,
            after: Some(after),
        };
        if let Err(e) = st
            .config_audit_add(&format!("api_key:{name}"), by, &audit)
            .await
        {
            tracing::warn!("audit row for {action} of API key {name} not written: {e}");
        }
    }

    /// Creates a key. `name` is taken lower case.
    ///
    /// # Errors
    /// See [`ApiKeyAdminError`].
    pub async fn create_api_key(
        &self,
        name: &str,
        write: bool,
        expires_at: Option<DateTime<Utc>>,
        by: &str,
    ) -> Result<CreatedKey, ApiKeyAdminError> {
        let name = name.trim().to_ascii_lowercase();
        if !valid_key_name(&name) {
            return Err(ApiKeyAdminError::BadName);
        }
        if expires_at.is_some_and(|e| e <= self.clock.now()) {
            return Err(ApiKeyAdminError::BadExpiry);
        }
        let key = format!("{KEY_PREFIX}{}", new_token());
        let scopes = if write {
            format!("{SCOPE_READ},{SCOPE_WRITE}")
        } else {
            SCOPE_READ.to_string()
        };
        let st = self
            .store()
            .await
            .map_err(|e| ApiKeyAdminError::Store(e.to_string()))?;
        let row = st
            .api_key_create(&NewApiKey {
                name: name.clone(),
                token_hash: sha256_hex(key.as_bytes()),
                prefix: key[..PREFIX_LEN].to_string(),
                scopes: scopes.clone(),
                created_by: by.to_string(),
                expires_at: expires_at.map(store::trunc_ms),
            })
            .await
            .map_err(admin_err)?;
        // only characters that need no JSON escaping: scopes, an RFC 3339 time, a base64url prefix
        let expires = row
            .expires_at
            .map_or("null".to_string(), |e| format!("\"{}\"", e.to_rfc3339()));
        let after = format!(
            "{{\"scopes\":\"{scopes}\",\"expires_at\":{expires},\"prefix\":\"{}\"}}",
            row.prefix
        );
        self.key_audit(&*st, &name, "key_create", by, after).await;
        Ok(CreatedKey {
            key,
            info: ApiKeyInfo::from_row(&row),
        })
    }

    /// Every key, revoked ones included.
    ///
    /// # Errors
    /// [`ApiKeyAdminError::Store`].
    pub async fn list_api_keys(&self) -> Result<Vec<ApiKeyInfo>, ApiKeyAdminError> {
        let st = self
            .store()
            .await
            .map_err(|e| ApiKeyAdminError::Store(e.to_string()))?;
        Ok(st
            .api_key_list()
            .await
            .map_err(admin_err)?
            .iter()
            .map(ApiKeyInfo::from_row)
            .collect())
    }

    /// Revokes the key named `name`. It stops working at once.
    ///
    /// # Errors
    /// [`ApiKeyAdminError::NotFound`], [`ApiKeyAdminError::AlreadyRevoked`] or a store error.
    pub async fn revoke_api_key(
        &self,
        name: &str,
        by: &str,
    ) -> Result<ApiKeyInfo, ApiKeyAdminError> {
        let name = name.trim().to_ascii_lowercase();
        let st = self
            .store()
            .await
            .map_err(|e| ApiKeyAdminError::Store(e.to_string()))?;
        let all = st.api_key_list().await.map_err(admin_err)?;
        let key = all
            .iter()
            .find(|k| k.name == name)
            .ok_or(ApiKeyAdminError::NotFound)?;
        if key.revoked_at.is_some() {
            return Err(ApiKeyAdminError::AlreadyRevoked);
        }
        let now = self.clock.now();
        if !st
            .api_key_revoke(key.id, by, now)
            .await
            .map_err(admin_err)?
        {
            return Err(ApiKeyAdminError::AlreadyRevoked);
        }
        self.key_audit(&*st, &name, "key_revoke", by, "{}".to_string())
            .await;
        let mut info = ApiKeyInfo::from_row(key);
        info.revoked_at = Some(store::trunc_ms(now));
        info.revoked_by = Some(by.to_string());
        Ok(info)
    }

    /// Checks the key of a request (the value after `Bearer `) and records its use (at most once
    /// a minute, with the client address `ip`).
    ///
    /// # Errors
    /// [`ApiKeyError::Invalid`] for any key that cannot be used; [`ApiKeyError::Unavailable`]
    /// when the database does not answer.
    pub async fn authenticate_api_key(
        &self,
        key: &str,
        ip: Option<&str>,
    ) -> Result<ApiKeyAuth, ApiKeyError> {
        if !looks_like_key(key) {
            return Err(ApiKeyError::Invalid);
        }
        let st = self
            .store()
            .await
            .map_err(|e| ApiKeyError::Unavailable(e.to_string()))?;
        let row = st
            .api_key_get_by_hash(&sha256_hex(key.as_bytes()))
            .await
            .map_err(|e| ApiKeyError::Unavailable(e.to_string()))?
            .ok_or(ApiKeyError::Invalid)?;
        let now = self.clock.now();
        let info = ApiKeyInfo::from_row(&row);
        if !info.active(now) {
            return Err(ApiKeyError::Invalid);
        }
        let due = row
            .last_used_at
            .is_none_or(|t| now - t >= Duration::seconds(TOUCH_EVERY_SECS));
        if due && let Err(e) = st.api_key_touch(row.id, now, ip).await {
            // the request itself is fine: a lost "last used" is no reason to refuse it
            tracing::warn!("last use of API key {} not recorded: {e}", row.name);
        }
        Ok(ApiKeyAuth {
            id: row.id,
            write: info.scopes.iter().any(|s| s == SCOPE_WRITE),
            name: row.name,
        })
    }
}

#[cfg(test)]
mod tests;
