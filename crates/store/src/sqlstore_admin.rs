//! [`AdminStore`] on top of the four drivers: configuration sections with an optimistic lock and
//! audit trail, users, sessions, and the manual re-delivery.

use crate::admin::*;
use crate::db::*;
use crate::error::{Error, Result};
use crate::model::{DeliveryStatus, trunc_ms};
use crate::sqlstore::{SqlStore, check_len, check_opt, otv, tv};
use crate::store::FaxStore;
use async_trait::async_trait;
use chrono::{DateTime, Utc};

use Kind::{Bool as B, Int as I, Str as S, Time as T};

const CONFIG_COLS: &str = "name, body, version, updated_by, updated_at";
const CONFIG_KINDS: &[Kind] = &[S, S, I, S, T];
const AUDIT_COLS: &str = "id, section, action, before_json, after_json, changed_by, at";
const AUDIT_KINDS: &[Kind] = &[I, S, S, S, S, S, T];
const USER_COLS: &str = "id, username, display_name, role, password_hash, locale, disabled, must_change_password, created_at, updated_at, last_login_at";
const USER_KINDS: &[Kind] = &[I, S, S, S, S, S, B, B, T, T, T];
const SESSION_COLS: &str = "id, token_hash, user_id, created_at, last_seen_at, expires_at, ip, user_agent";
const SESSION_KINDS: &[Kind] = &[I, S, I, T, T, T, S, S];

fn read_config(v: Vec<Val>) -> ConfigRow {
    let mut r = RowReader::new(v);
    ConfigRow {
        name: r.str(),
        body: r.str(),
        version: r.i32(),
        updated_by: r.opt_str(),
        updated_at: r.time(),
    }
}

fn read_audit(v: Vec<Val>) -> ConfigAuditRow {
    let mut r = RowReader::new(v);
    ConfigAuditRow {
        id: r.i64(),
        section: r.str(),
        action: r.str(),
        before_json: r.opt_str(),
        after_json: r.opt_str(),
        by: r.str(),
        at: r.time(),
    }
}

fn read_user(v: Vec<Val>) -> UserRow {
    let mut r = RowReader::new(v);
    UserRow {
        id: r.i64(),
        username: r.str(),
        display_name: r.opt_str(),
        role: r.str(),
        password_hash: r.str(),
        locale: r.opt_str(),
        disabled: r.bool(),
        must_change_password: r.bool(),
        created_at: r.time(),
        updated_at: r.time(),
        last_login_at: r.opt_time(),
    }
}

fn read_session(v: Vec<Val>) -> SessionRow {
    let mut r = RowReader::new(v);
    SessionRow {
        id: r.i64(),
        token_hash: r.str(),
        user_id: r.i64(),
        created_at: r.time(),
        last_seen_at: r.time(),
        expires_at: r.time(),
        ip: r.opt_str(),
        user_agent: r.opt_str(),
    }
}

impl SqlStore {
    async fn audit(&self, section: &str, by: &str, a: &ConfigAudit) -> Result<()> {
        check_len("action", &a.action, 16)?;
        self.raw()
            .execute(
                "INSERT INTO config_audit (section, action, before_json, after_json, changed_by, at) VALUES (?, ?, ?, ?, ?, ?)",
                &[
                    section.into(),
                    a.action.as_str().into(),
                    a.before.clone().into(),
                    a.after.clone().into(),
                    by.into(),
                    tv(self.now()),
                ],
            )
            .await?;
        Ok(())
    }

    async fn one_user(&self, where_sql: &str, params: &[Val]) -> Result<Option<UserRow>> {
        let sql = format!("SELECT {USER_COLS} FROM users WHERE {where_sql}");
        let rows = self.raw().query(&sql, params, USER_KINDS).await?;
        Ok(rows.into_iter().next().map(read_user))
    }

    async fn count_where(&self, sql: &str, params: &[Val]) -> Result<u64> {
        let rows = self.raw().query(sql, params, &[I]).await?;
        Ok(match rows.first().and_then(|r| r.first()) {
            Some(Val::I(Some(n))) => *n as u64,
            _ => 0,
        })
    }
}

#[async_trait]
impl AdminStore for SqlStore {
    async fn config_get(&self, name: &str) -> Result<Option<ConfigRow>> {
        let sql = format!("SELECT {CONFIG_COLS} FROM config_sections WHERE name = ?");
        let rows = self
            .raw()
            .query(&sql, &[name.into()], CONFIG_KINDS)
            .await?;
        Ok(rows.into_iter().next().map(read_config))
    }

    async fn config_list(&self) -> Result<Vec<ConfigRow>> {
        let sql = format!("SELECT {CONFIG_COLS} FROM config_sections ORDER BY name");
        let rows = self.raw().query(&sql, &[], CONFIG_KINDS).await?;
        Ok(rows.into_iter().map(read_config).collect())
    }

    async fn config_put(
        &self,
        name: &str,
        body: &str,
        expected_version: Option<i32>,
        by: &str,
        audit: &ConfigAudit,
    ) -> Result<ConfigRow> {
        check_len("section name", name, 64)?;
        check_len("by", by, 64)?;
        let now = tv(self.now());
        match expected_version {
            None => {
                let r = self
                    .raw()
                    .execute(
                        "INSERT INTO config_sections (name, body, version, updated_by, updated_at) VALUES (?, ?, 1, ?, ?)",
                        &[name.into(), body.into(), by.into(), now],
                    )
                    .await;
                match r {
                    Ok(_) => {}
                    Err(Error::Duplicate(_)) => {
                        return Err(Error::Conflict(format!("section {name} already exists")));
                    }
                    Err(e) => return Err(e),
                }
            }
            Some(v) => {
                let n = self
                    .raw()
                    .execute(
                        "UPDATE config_sections SET body = ?, version = version + 1, updated_by = ?, updated_at = ? WHERE name = ? AND version = ?",
                        &[body.into(), by.into(), now, name.into(), v.into()],
                    )
                    .await?;
                if n == 0 {
                    return Err(match self.config_get(name).await? {
                        None => Error::NotFound(format!("section {name}")),
                        Some(cur) => Error::Conflict(format!(
                            "section {name} is at version {} (expected {v})",
                            cur.version
                        )),
                    });
                }
            }
        }
        self.audit(name, by, audit).await?;
        self.config_get(name)
            .await?
            .ok_or_else(|| Error::NotFound(format!("section {name}")))
    }

    async fn config_delete(
        &self,
        name: &str,
        expected_version: i32,
        by: &str,
        audit: &ConfigAudit,
    ) -> Result<()> {
        check_len("by", by, 64)?;
        let n = self
            .raw()
            .execute(
                "DELETE FROM config_sections WHERE name = ? AND version = ?",
                &[name.into(), expected_version.into()],
            )
            .await?;
        if n == 0 {
            return Err(match self.config_get(name).await? {
                None => Error::NotFound(format!("section {name}")),
                Some(cur) => Error::Conflict(format!(
                    "section {name} is at version {} (expected {expected_version})",
                    cur.version
                )),
            });
        }
        self.audit(name, by, audit).await
    }

    async fn config_audit_list(
        &self,
        limit: u32,
        before_id: Option<i64>,
    ) -> Result<Vec<ConfigAuditRow>> {
        let (top, lim) = self.limit(limit.clamp(1, 500));
        let (cond, params): (&str, Vec<Val>) = match before_id {
            Some(id) => ("WHERE id < ?", vec![id.into()]),
            None => ("", vec![]),
        };
        let sql = format!(
            "SELECT {top} {AUDIT_COLS} FROM config_audit {cond} ORDER BY id DESC {lim}"
        );
        let rows = self.raw().query(&sql, &params, AUDIT_KINDS).await?;
        Ok(rows.into_iter().map(read_audit).collect())
    }

    async fn user_create(&self, u: &NewUser) -> Result<UserRow> {
        check_len("username", &u.username, 64)?;
        check_opt("display_name", &u.display_name, 128)?;
        check_len("role", &u.role, 8)?;
        check_len("password_hash", &u.password_hash, 255)?;
        check_opt("locale", &u.locale, 8)?;
        let now = tv(self.now());
        let id = self
            .raw()
            .insert_id(
                "INSERT INTO users (username, display_name, role, password_hash, locale, disabled, must_change_password, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                &[
                    u.username.as_str().into(),
                    u.display_name.clone().into(),
                    u.role.as_str().into(),
                    u.password_hash.as_str().into(),
                    u.locale.clone().into(),
                    false.into(),
                    u.must_change_password.into(),
                    now.clone(),
                    now,
                ],
            )
            .await?;
        self.user_get(id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("user {id}")))
    }

    async fn user_get(&self, id: i64) -> Result<Option<UserRow>> {
        self.one_user("id = ?", &[id.into()]).await
    }

    async fn user_get_by_name(&self, username: &str) -> Result<Option<UserRow>> {
        self.one_user("username = ?", &[username.into()]).await
    }

    async fn user_list(&self) -> Result<Vec<UserRow>> {
        let sql = format!("SELECT {USER_COLS} FROM users ORDER BY username");
        let rows = self.raw().query(&sql, &[], USER_KINDS).await?;
        Ok(rows.into_iter().map(read_user).collect())
    }

    async fn user_update(&self, id: i64, u: &UserUpdate) -> Result<UserRow> {
        let mut sets: Vec<&str> = Vec::new();
        let mut params: Vec<Val> = Vec::new();
        if let Some(v) = &u.display_name {
            check_opt("display_name", v, 128)?;
            sets.push("display_name = ?");
            params.push(v.clone().into());
        }
        if let Some(v) = &u.role {
            check_len("role", v, 8)?;
            sets.push("role = ?");
            params.push(v.as_str().into());
        }
        if let Some(v) = &u.password_hash {
            check_len("password_hash", v, 255)?;
            sets.push("password_hash = ?");
            params.push(v.as_str().into());
        }
        if let Some(v) = &u.locale {
            check_opt("locale", v, 8)?;
            sets.push("locale = ?");
            params.push(v.clone().into());
        }
        if let Some(v) = u.disabled {
            sets.push("disabled = ?");
            params.push(v.into());
        }
        if let Some(v) = u.must_change_password {
            sets.push("must_change_password = ?");
            params.push(v.into());
        }
        sets.push("updated_at = ?");
        params.push(tv(self.now()));
        params.push(id.into());
        let sql = format!("UPDATE users SET {} WHERE id = ?", sets.join(", "));
        if self.raw().execute(&sql, &params).await? == 0 {
            return Err(Error::NotFound(format!("user {id}")));
        }
        self.user_get(id)
            .await?
            .ok_or_else(|| Error::NotFound(format!("user {id}")))
    }

    async fn user_touch_login(&self, id: i64, at: DateTime<Utc>) -> Result<()> {
        self.raw()
            .execute(
                "UPDATE users SET last_login_at = ? WHERE id = ?",
                &[tv(at), id.into()],
            )
            .await?;
        Ok(())
    }

    async fn user_delete(&self, id: i64) -> Result<bool> {
        self.raw()
            .execute("DELETE FROM sessions WHERE user_id = ?", &[id.into()])
            .await?;
        Ok(self
            .raw()
            .execute("DELETE FROM users WHERE id = ?", &[id.into()])
            .await?
            > 0)
    }

    async fn user_count(&self) -> Result<u64> {
        self.count_where("SELECT COUNT(*) FROM users", &[]).await
    }

    async fn user_count_active_admins(&self) -> Result<u64> {
        self.count_where(
            "SELECT COUNT(*) FROM users WHERE role = ? AND disabled = ?",
            &["admin".into(), false.into()],
        )
        .await
    }

    async fn session_create(&self, s: &NewSession) -> Result<i64> {
        check_len("token_hash", &s.token_hash, 64)?;
        check_opt("ip", &s.ip, 64)?;
        // the user agent is cut, not refused: a long one is no reason to fail a login
        let ua: Option<String> = s.user_agent.as_ref().map(|u| u.chars().take(120).collect());
        self.raw()
            .insert_id(
                "INSERT INTO sessions (token_hash, user_id, created_at, last_seen_at, expires_at, ip, user_agent) VALUES (?, ?, ?, ?, ?, ?, ?)",
                &[
                    s.token_hash.as_str().into(),
                    s.user_id.into(),
                    tv(s.created_at),
                    tv(s.created_at),
                    tv(s.expires_at),
                    s.ip.clone().into(),
                    ua.into(),
                ],
            )
            .await
    }

    async fn session_get(&self, token_hash: &str) -> Result<Option<SessionRow>> {
        let sql = format!("SELECT {SESSION_COLS} FROM sessions WHERE token_hash = ?");
        let rows = self
            .raw()
            .query(&sql, &[token_hash.into()], SESSION_KINDS)
            .await?;
        Ok(rows.into_iter().next().map(read_session))
    }

    async fn session_touch(&self, id: i64, at: DateTime<Utc>) -> Result<()> {
        self.raw()
            .execute(
                "UPDATE sessions SET last_seen_at = ? WHERE id = ?",
                &[tv(trunc_ms(at)), id.into()],
            )
            .await?;
        Ok(())
    }

    async fn session_delete(&self, token_hash: &str) -> Result<bool> {
        Ok(self
            .raw()
            .execute(
                "DELETE FROM sessions WHERE token_hash = ?",
                &[token_hash.into()],
            )
            .await?
            > 0)
    }

    async fn sessions_delete_for_user(&self, user_id: i64) -> Result<u64> {
        self.raw()
            .execute("DELETE FROM sessions WHERE user_id = ?", &[user_id.into()])
            .await
    }

    async fn sessions_delete_expired(&self, now: DateTime<Utc>) -> Result<u64> {
        self.raw()
            .execute("DELETE FROM sessions WHERE expires_at < ?", &[tv(now)])
            .await
    }

    async fn session_count_for_user(&self, user_id: i64) -> Result<u64> {
        self.count_where(
            "SELECT COUNT(*) FROM sessions WHERE user_id = ?",
            &[user_id.into()],
        )
        .await
    }

    async fn redeliver(&self, fax_id: i64, at: DateTime<Utc>) -> Result<Vec<i64>> {
        let all = self.list_deliveries(fax_id).await?;
        let mut latest: Vec<&crate::model::DeliveryAttempt> = Vec::new();
        for d in &all {
            match latest.iter_mut().find(|l| l.target == d.target) {
                Some(slot) => {
                    if d.id > slot.id {
                        *slot = d;
                    }
                }
                None => latest.push(d),
            }
        }
        let mut created = Vec::new();
        for d in latest {
            if matches!(d.status, DeliveryStatus::Failed | DeliveryStatus::GivingUp) {
                let id = self
                    .raw()
                    .insert_id(
                        "INSERT INTO delivery_attempts (fax_id, target, kind, attempt, status, last_error, next_try_at, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?)",
                        &[
                            fax_id.into(),
                            d.target.as_str().into(),
                            d.kind.as_str().into(),
                            (d.attempt + 1).into(),
                            DeliveryStatus::Pending.as_str().into(),
                            Val::S(None),
                            otv(Some(at)),
                            tv(self.now()),
                            tv(self.now()),
                        ],
                    )
                    .await?;
                created.push(id);
            }
        }
        Ok(created)
    }
}
