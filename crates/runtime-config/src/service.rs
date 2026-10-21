//! [`ConfigService`]: the runtime configuration in the database, the in-memory snapshot, and the
//! one place where a change is validated, stored, audited and applied.
//!
//! API for the REST layer (M4b) and the control socket (M4d):
//!
//! * [`ConfigService::current`] / [`ConfigService::subscribe`]: the snapshot every new call uses;
//! * [`ConfigService::get_section`] / [`ConfigService::list_sections`]: what the interface shows
//!   (secrets as `{"set": true}`);
//! * [`ConfigService::update`] / [`ConfigService::delete_trunk`] /
//!   [`ConfigService::reorder_trunks`]: a change;
//! * [`ConfigService::reload`]: re-read the database and apply what differs (`reload` command);
//! * [`ConfigService::pending_restart`], [`ConfigService::status`]: `/status`.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use arc_swap::ArcSwap;
use async_trait::async_trait;
use fax_in::config::StorageConfig;
use serde::Serialize;
use serde_json::Value;
use store::{ConfigAudit, FaxStore};
use tokio::sync::{Mutex as AsyncMutex, watch};

use crate::codec::{self, SecretIssue};
use crate::model::{FIXED_SECTIONS, RuntimeConfig, Section, TRUNK_ORDER};
use crate::secret::{KeyStatus, SecretBox};
use crate::validate::{self, FieldError, SectionValue};

/// Where the service gets its store from (the daemon's [`Db`] reconnects lazily; tests pass a
/// store directly).
#[async_trait]
pub trait StoreSource: Send + Sync {
    /// The store, connecting first when necessary.
    async fn store(&self) -> store::Result<Arc<dyn FaxStore>>;
}

#[async_trait]
impl StoreSource for Arc<dyn FaxStore> {
    async fn store(&self) -> store::Result<Arc<dyn FaxStore>> {
        Ok(self.clone())
    }
}

/// Applies a new configuration to the running server (the daemon implements this with
/// `FaxIn::apply_config` and the delivery service).
#[async_trait]
pub trait ConfigApplier: Send + Sync {
    /// Called after the change is stored, with the old and the new snapshot.
    async fn apply(&self, old: &RuntimeConfig, new: &RuntimeConfig) -> ApplyReport;
}

/// What an apply did.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct ApplyReport {
    /// Trunks whose registration was restarted (un-REGISTER, then REGISTER again).
    pub registrations_restarted: Vec<String>,
    /// Trunks that were added, changed or removed.
    pub trunks_changed: Vec<String>,
    /// Anything worth telling the operator.
    pub notes: Vec<String>,
}

/// Where the snapshot came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub enum Source {
    /// Read from the database.
    Database,
    /// The database was not reachable at start: the configuration file is used until it is.
    FileFallback,
}

struct State {
    config: Arc<RuntimeConfig>,
    versions: BTreeMap<String, i32>,
    issues: Vec<SecretIssue>,
    source: Source,
}

/// Result of [`ConfigService::update`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct UpdateOutcome {
    /// The section.
    pub section: String,
    /// Its new version.
    pub version: i32,
    /// Settings of this section that only take effect after a restart (empty: effective now).
    pub restart_required: Vec<String>,
    /// All settings that wait for a restart.
    pub pending_restart: Vec<String>,
    /// Non-fatal observations about the new configuration.
    pub warnings: Vec<String>,
    /// What the running server did with it.
    pub applied: ApplyReport,
}

/// Result of [`ConfigService::reload`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ReloadOutcome {
    /// Whether the database differed from the snapshot (and was applied).
    pub changed: bool,
    /// All settings that wait for a restart.
    pub pending_restart: Vec<String>,
    /// What the running server did.
    pub applied: ApplyReport,
}

/// Why a change was refused.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    /// The body does not validate; one entry per field.
    #[error("the configuration is not valid ({} problem(s))", .0.len())]
    Invalid(Vec<FieldError>),
    /// Not one of the section names.
    #[error("unknown section {0:?}")]
    UnknownSection(String),
    /// Somebody else changed the section first.
    #[error("the section was changed by someone else (now at version {current})")]
    Conflict {
        /// The version in the database now.
        current: i32,
    },
    /// The section does not exist (delete, or an update with a version).
    #[error("no such section")]
    NotFound,
    /// The database cannot be reached: nothing was changed, the running configuration stays.
    #[error("the database is not available: {0}")]
    Unavailable(String),
    /// Anything else.
    #[error("{0}")]
    Internal(String),
}

fn map_store(e: store::Error) -> UpdateError {
    match e {
        store::Error::Unavailable(m) => UpdateError::Unavailable(m),
        store::Error::NotFound(_) => UpdateError::NotFound,
        other => UpdateError::Internal(other.to_string()),
    }
}

/// What a section looks like to a client.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SectionView {
    /// Section name.
    pub name: String,
    /// Version (0: never stored, the body shows the defaults).
    pub version: i32,
    /// The body; secrets as `{"set": true, "ref": ...}`.
    pub body: Value,
}

/// Why `config import` refused.
#[derive(Debug, thiserror::Error)]
pub enum ImportError {
    /// The database already has runtime configuration and `--force` was not given.
    #[error("the database already holds runtime configuration ({0} section(s)); use --force to replace it")]
    AlreadyConfigured(usize),
    /// The file's configuration is not valid.
    #[error("{0}")]
    Invalid(String),
    /// The database failed.
    #[error("database: {0}")]
    Store(String),
}

/// What an import wrote.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ImportReport {
    /// Sections written.
    pub written: Vec<String>,
    /// Sections removed (only with force).
    pub removed: Vec<String>,
}

/// Result of [`ConfigService::bootstrap`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Boot {
    /// The database had no runtime configuration: the file's was imported.
    Imported(ImportReport),
    /// The database decides. `ignored` lists the sections the file still defines (they have no
    /// effect any more).
    FromDatabase {
        /// Sections written in the file that the database overrides.
        ignored: Vec<String>,
    },
    /// The database cannot be reached: the file's configuration is in use for now.
    Unreachable(String),
}

/// The runtime configuration service.
pub struct ConfigService {
    src: Arc<dyn StoreSource>,
    secrets: Arc<SecretBox>,
    state: ArcSwap<State>,
    started: Mutex<Option<Arc<RuntimeConfig>>>,
    applier: Mutex<Option<Arc<dyn ConfigApplier>>>,
    write: AsyncMutex<()>,
    tx: watch::Sender<Arc<RuntimeConfig>>,
    file: Mutex<(RuntimeConfig, Vec<String>)>,
}

fn is_runtime_section(name: &str) -> bool {
    name != "retention" && name != TRUNK_ORDER
}

impl ConfigService {
    /// A service over `src`. `file` is the configuration file's runtime part: the import source
    /// and the fallback while the database is unreachable; `file_sections` names the sections
    /// the file actually contains.
    pub fn new(
        src: Arc<dyn StoreSource>,
        secrets: Arc<SecretBox>,
        file: RuntimeConfig,
        file_sections: Vec<String>,
    ) -> ConfigService {
        let initial = Arc::new(file.clone());
        let (tx, _) = watch::channel(initial.clone());
        ConfigService {
            src,
            secrets,
            state: ArcSwap::from_pointee(State {
                config: initial,
                versions: BTreeMap::new(),
                issues: Vec::new(),
                source: Source::FileFallback,
            }),
            started: Mutex::new(None),
            applier: Mutex::new(None),
            write: AsyncMutex::new(()),
            tx,
            file: Mutex::new((file, file_sections)),
        }
    }

    /// Registers what applies a change to the running server.
    pub fn set_applier(&self, a: Arc<dyn ConfigApplier>) {
        *self.applier.lock().unwrap_or_else(|e| e.into_inner()) = Some(a);
    }

    /// The configuration new calls use right now.
    pub fn current(&self) -> Arc<RuntimeConfig> {
        self.state.load().config.clone()
    }

    /// Notified with every new snapshot.
    pub fn subscribe(&self) -> watch::Receiver<Arc<RuntimeConfig>> {
        self.tx.subscribe()
    }

    /// Where the snapshot came from.
    pub fn source(&self) -> Source {
        self.state.load().source
    }

    /// Versions of the stored sections.
    pub fn versions(&self) -> BTreeMap<String, i32> {
        self.state.load().versions.clone()
    }

    /// Secrets that could not be decrypted at the last load.
    pub fn secret_issues(&self) -> Vec<SecretIssue> {
        self.state.load().issues.clone()
    }

    /// The secret key.
    pub fn secrets(&self) -> &Arc<SecretBox> {
        &self.secrets
    }

    /// Marks the configuration the process started with (what the fixed parts were built from);
    /// [`ConfigService::pending_restart`] compares against it. [`ConfigService::bootstrap`]
    /// does this at its end.
    pub fn mark_started(&self) {
        *self.started.lock().unwrap_or_else(|e| e.into_inner()) = Some(self.current());
    }

    /// Settings that differ from what the process started with and need a restart.
    pub fn pending_restart(&self) -> Vec<String> {
        let started = self.started.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let Some(started) = started else {
            return Vec::new();
        };
        let st = StorageConfig::default();
        fax_in::config::restart_required(
            &started.to_fax_in(&st),
            &self.current().to_fax_in(&st),
        )
    }

    // ------------------------------------------------------------------ loading

    async fn read_all(&self, store: &dyn FaxStore) -> store::Result<State> {
        let rows = store.config_list().await?;
        let mut cfg = RuntimeConfig::default();
        let mut versions = BTreeMap::new();
        let mut issues = Vec::new();
        let mut trunks: BTreeMap<String, fax_in::config::TrunkConfig> = BTreeMap::new();
        let mut order: Vec<String> = Vec::new();
        for row in &rows {
            versions.insert(row.name.clone(), row.version);
            if row.name == TRUNK_ORDER {
                order = serde_json::from_str(&row.body).unwrap_or_default();
                continue;
            }
            let Some(section) = Section::parse(&row.name) else {
                tracing::warn!(section = %row.name, "unknown configuration section ignored");
                continue;
            };
            let stored: Value = match serde_json::from_str(&row.body) {
                Ok(v) => v,
                Err(e) => {
                    tracing::error!(section = %row.name, "stored section is not JSON, ignored: {e}");
                    continue;
                }
            };
            let (plain, mut iss) = codec::decrypt(&section, &stored, &self.secrets);
            issues.append(&mut iss);
            match validate::parse_section(&section, &plain) {
                Ok(SectionValue::Sip(c)) => cfg.sip = c,
                Ok(SectionValue::Media(c)) => cfg.media = c,
                Ok(SectionValue::Call(c)) => cfg.call = c,
                Ok(SectionValue::Capture(c)) => cfg.capture = c,
                Ok(SectionValue::Delivery(c)) => cfg.delivery = c,
                Ok(SectionValue::Retention(c)) => cfg.retention = c,
                Ok(SectionValue::Routes(c)) => cfg.routes = c,
                Ok(SectionValue::Trunk(t)) => {
                    trunks.insert(t.name.clone(), *t);
                }
                Err(errs) => {
                    let list: Vec<String> = errs
                        .iter()
                        .map(|e| format!("{}: {}", e.field, e.message))
                        .collect();
                    tracing::error!(section = %row.name, "stored section is not valid, ignored: {}", list.join("; "));
                }
            }
        }
        for n in order {
            if let Some(t) = trunks.remove(&n) {
                cfg.trunks.push(t);
            }
        }
        cfg.trunks.extend(trunks.into_values());
        for i in &issues {
            tracing::error!(
                section = %i.section, field = %i.field,
                "secret cannot be decrypted ({}): enter it again in the web interface",
                i.error
            );
        }
        Ok(State {
            config: Arc::new(cfg),
            versions,
            issues,
            source: Source::Database,
        })
    }

    /// Start-up: the database decides; when it has no runtime configuration yet, the file's is
    /// imported (audit `by = config-file`). When the database cannot be reached, the file's
    /// configuration is the snapshot until [`ConfigService::retry_bootstrap`] succeeds.
    pub async fn bootstrap(&self) -> Boot {
        let boot = self.bootstrap_inner().await;
        self.mark_started();
        boot
    }

    /// Like [`ConfigService::bootstrap`] once the database is reachable after an unreachable
    /// start; applies the loaded configuration to the running server.
    pub async fn retry_bootstrap(&self) -> Boot {
        let old = self.current();
        let boot = self.bootstrap_inner().await;
        if !matches!(boot, Boot::Unreachable(_)) {
            let new = self.current();
            if !old.same_as(&new) {
                self.apply(&old, &new).await;
            }
            let _ = self.tx.send(new);
        }
        boot
    }

    async fn bootstrap_inner(&self) -> Boot {
        let store = match self.src.store().await {
            Ok(s) => s,
            Err(e) => return Boot::Unreachable(e.to_string()),
        };
        let (file, file_sections) = self.file.lock().unwrap_or_else(|e| e.into_inner()).clone();
        let rows = match store.config_list().await {
            Ok(r) => r,
            Err(e) => return Boot::Unreachable(e.to_string()),
        };
        let have = rows.iter().filter(|r| is_runtime_section(&r.name)).count();
        let boot;
        if have == 0 {
            match self.import_missing(&*store, &file, "config-file").await {
                Ok(rep) => boot = Boot::Imported(rep),
                Err(ImportError::Store(m)) => return Boot::Unreachable(m),
                Err(e) => {
                    tracing::error!("importing the configuration file failed: {e}");
                    return Boot::Unreachable(e.to_string());
                }
            }
        } else {
            let ignored: Vec<String> = file_sections
                .into_iter()
                .filter(|s| s != "retention" || rows.iter().any(|r| r.name == "retention"))
                .collect();
            boot = Boot::FromDatabase { ignored };
        }
        match self.read_all(&*store).await {
            Ok(st) => {
                let cfg = st.config.clone();
                self.state.store(Arc::new(st));
                let _ = self.tx.send(cfg);
                boot
            }
            Err(e) => Boot::Unreachable(e.to_string()),
        }
    }

    /// Re-reads the database and applies what differs (the `reload` command).
    ///
    /// # Errors
    /// The database cannot be read; the running configuration stays.
    pub async fn reload(&self) -> Result<ReloadOutcome, UpdateError> {
        let _w = self.write.lock().await;
        let store = self.src.store().await.map_err(map_store)?;
        let st = self.read_all(&*store).await.map_err(map_store)?;
        let old = self.current();
        let changed = !old.same_as(&st.config);
        let new = st.config.clone();
        self.state.store(Arc::new(st));
        let applied = if changed {
            let r = self.apply(&old, &new).await;
            let _ = self.tx.send(new);
            r
        } else {
            ApplyReport::default()
        };
        Ok(ReloadOutcome {
            changed,
            pending_restart: self.pending_restart(),
            applied,
        })
    }

    async fn apply(&self, old: &RuntimeConfig, new: &RuntimeConfig) -> ApplyReport {
        let a = self.applier.lock().unwrap_or_else(|e| e.into_inner()).clone();
        match a {
            Some(a) => a.apply(old, new).await,
            None => ApplyReport::default(),
        }
    }

    // ------------------------------------------------------------------ reading

    /// One section as a client sees it.
    ///
    /// # Errors
    /// Unknown name, or the database cannot be read.
    pub async fn get_section(&self, name: &str) -> Result<SectionView, UpdateError> {
        let section =
            Section::parse(name).ok_or_else(|| UpdateError::UnknownSection(name.into()))?;
        let store = self.src.store().await.map_err(map_store)?;
        match store.config_get(name).await.map_err(map_store)? {
            Some(row) => {
                let stored: Value = serde_json::from_str(&row.body)
                    .map_err(|e| UpdateError::Internal(e.to_string()))?;
                Ok(SectionView {
                    name: row.name,
                    version: row.version,
                    body: codec::redact(&section, &stored),
                })
            }
            None => match section {
                Section::Trunk(_) => Err(UpdateError::NotFound),
                _ => Ok(SectionView {
                    name: name.into(),
                    version: 0,
                    body: self.default_body(&section),
                }),
            },
        }
    }

    fn default_body(&self, section: &Section) -> Value {
        let d = RuntimeConfig::default();
        let v = match section {
            Section::Sip => serde_json::to_value(&d.sip),
            Section::Media => serde_json::to_value(&d.media),
            Section::Call => serde_json::to_value(&d.call),
            Section::Capture => serde_json::to_value(&d.capture),
            Section::Delivery => serde_json::to_value(&d.delivery),
            Section::Retention => serde_json::to_value(&d.retention),
            Section::Routes => serde_json::to_value(&d.routes),
            Section::Trunk(_) => Ok(Value::Null),
        };
        v.unwrap_or(Value::Null)
    }

    /// Every stored section (and the fixed ones that were never stored, at version 0), trunks in
    /// match order.
    ///
    /// # Errors
    /// The database cannot be read.
    pub async fn list_sections(&self) -> Result<Vec<SectionView>, UpdateError> {
        let mut out = Vec::new();
        for n in FIXED_SECTIONS {
            out.push(self.get_section(n).await?);
        }
        let store = self.src.store().await.map_err(map_store)?;
        let rows = store.config_list().await.map_err(map_store)?;
        let order: Vec<String> = rows
            .iter()
            .find(|r| r.name == TRUNK_ORDER)
            .and_then(|r| serde_json::from_str(&r.body).ok())
            .unwrap_or_default();
        let mut trunks: Vec<&store::ConfigRow> =
            rows.iter().filter(|r| r.name.starts_with("trunk:")).collect();
        trunks.sort_by_key(|r| {
            let n = r.name.trim_start_matches("trunk:");
            (
                order.iter().position(|o| o == n).unwrap_or(usize::MAX),
                r.name.clone(),
            )
        });
        for r in trunks {
            out.push(self.get_section(&r.name).await?);
        }
        Ok(out)
    }

    /// The audit trail, newest first.
    ///
    /// # Errors
    /// The database cannot be read.
    pub async fn audit(
        &self,
        limit: u32,
        before_id: Option<i64>,
    ) -> Result<Vec<store::ConfigAuditRow>, UpdateError> {
        let store = self.src.store().await.map_err(map_store)?;
        store
            .config_audit_list(limit, before_id)
            .await
            .map_err(map_store)
    }

    // ------------------------------------------------------------------ changing

    /// Validates and stores a section, applies it to the running server and returns what
    /// happened. `expected_version` is the version the client read (0 for a section that has
    /// never been stored, and for a new trunk). `by` is the operator for the audit trail
    /// (a username, `cli:<os user>`).
    ///
    /// Calls in progress are not affected; the next call uses the new snapshot. With the
    /// database unreachable nothing changes and [`UpdateError::Unavailable`] is returned.
    ///
    /// # Errors
    /// See [`UpdateError`].
    pub async fn update(
        &self,
        section: &str,
        body: Value,
        expected_version: i32,
        by: &str,
    ) -> Result<UpdateOutcome, UpdateError> {
        let sec = Section::parse(section)
            .ok_or_else(|| UpdateError::UnknownSection(section.to_string()))?;
        let _w = self.write.lock().await;
        let store = self.src.store().await.map_err(map_store)?;
        let old_row = store.config_get(section).await.map_err(map_store)?;
        let old_version = old_row.as_ref().map_or(0, |r| r.version);
        if old_version != expected_version {
            return Err(UpdateError::Conflict {
                current: old_version,
            });
        }
        let old_stored: Option<Value> = old_row
            .as_ref()
            .and_then(|r| serde_json::from_str(&r.body).ok());
        let (stored, for_validation) =
            codec::from_api(&sec, &body, old_stored.as_ref(), &self.secrets)
                .map_err(UpdateError::Invalid)?;
        let value = validate::parse_section(&sec, &for_validation).map_err(UpdateError::Invalid)?;
        // consistency with the other sections: only what this change newly breaks is reported
        let before = self.current();
        let mut candidate = (*before).clone();
        place(&mut candidate, value);
        let had: Vec<String> = validate::cross_check(&before);
        let new_problems: Vec<FieldError> = validate::cross_check(&candidate)
            .into_iter()
            .filter(|m| !had.contains(m))
            .map(|m| FieldError::new("", "invalid", m))
            .collect();
        if !new_problems.is_empty() {
            return Err(UpdateError::Invalid(new_problems));
        }
        let body_text = serde_json::to_string(&stored).map_err(|e| UpdateError::Internal(e.to_string()))?;
        let audit = ConfigAudit {
            action: if old_row.is_some() { "update" } else { "create" }.into(),
            before: old_stored
                .as_ref()
                .map(|o| codec::mask_for_audit(&sec, o).to_string()),
            after: Some(codec::mask_for_audit(&sec, &stored).to_string()),
        };
        let row = store
            .config_put(
                section,
                &body_text,
                old_row.as_ref().map(|r| r.version),
                by,
                &audit,
            )
            .await
            .map_err(|e| match e {
                store::Error::Conflict(m) => UpdateError::Conflict {
                    current: version_in(&m).unwrap_or(old_version + 1),
                },
                other => map_store(other),
            })?;
        if let Section::Trunk(name) = &sec
            && old_row.is_none()
        {
            self.order_add(&*store, name, by).await?;
        }
        let applied = self.refresh(&*store).await?;
        Ok(UpdateOutcome {
            section: section.to_string(),
            version: row.version,
            restart_required: self.restart_fields_of(&sec),
            pending_restart: self.pending_restart(),
            warnings: validate::warnings(&self.current()),
            applied,
        })
    }

    fn restart_fields_of(&self, sec: &Section) -> Vec<String> {
        let prefix = match sec {
            Section::Sip => "sip.",
            Section::Media => "media.",
            Section::Call => "call.",
            Section::Capture => "capture",
            _ => return Vec::new(),
        };
        self.pending_restart()
            .into_iter()
            .filter(|f| f.starts_with(prefix))
            .collect()
    }

    /// Re-reads everything, swaps the snapshot, applies the difference.
    async fn refresh(&self, store: &dyn FaxStore) -> Result<ApplyReport, UpdateError> {
        let st = self.read_all(store).await.map_err(map_store)?;
        let old = self.current();
        let new = st.config.clone();
        self.state.store(Arc::new(st));
        let applied = self.apply(&old, &new).await;
        let _ = self.tx.send(new);
        Ok(applied)
    }

    async fn order_add(
        &self,
        store: &dyn FaxStore,
        name: &str,
        by: &str,
    ) -> Result<(), UpdateError> {
        let row = store.config_get(TRUNK_ORDER).await.map_err(map_store)?;
        let mut order: Vec<String> = row
            .as_ref()
            .and_then(|r| serde_json::from_str(&r.body).ok())
            .unwrap_or_default();
        if !order.iter().any(|n| n == name) {
            order.push(name.to_string());
        }
        self.put_order(store, &order, row.map(|r| r.version), by).await
    }

    async fn put_order(
        &self,
        store: &dyn FaxStore,
        order: &[String],
        version: Option<i32>,
        by: &str,
    ) -> Result<(), UpdateError> {
        let body = serde_json::to_string(order).map_err(|e| UpdateError::Internal(e.to_string()))?;
        let audit = ConfigAudit {
            action: if version.is_some() { "update" } else { "create" }.into(),
            before: None,
            after: Some(body.clone()),
        };
        store
            .config_put(TRUNK_ORDER, &body, version, by, &audit)
            .await
            .map(|_| ())
            .map_err(map_store)
    }

    /// Deletes a trunk (its registration is stopped by the apply).
    ///
    /// # Errors
    /// See [`UpdateError`].
    pub async fn delete_trunk(
        &self,
        name: &str,
        expected_version: i32,
        by: &str,
    ) -> Result<UpdateOutcome, UpdateError> {
        let section = format!("trunk:{name}");
        let sec = Section::parse(&section).ok_or(UpdateError::UnknownSection(section.clone()))?;
        let _w = self.write.lock().await;
        let store = self.src.store().await.map_err(map_store)?;
        let row = store
            .config_get(&section)
            .await
            .map_err(map_store)?
            .ok_or(UpdateError::NotFound)?;
        let stored: Value = serde_json::from_str(&row.body).unwrap_or(Value::Null);
        let audit = ConfigAudit {
            action: "delete".into(),
            before: Some(codec::mask_for_audit(&sec, &stored).to_string()),
            after: None,
        };
        store
            .config_delete(&section, expected_version, by, &audit)
            .await
            .map_err(|e| match e {
                store::Error::Conflict(_) => UpdateError::Conflict { current: row.version },
                other => map_store(other),
            })?;
        let ord = store.config_get(TRUNK_ORDER).await.map_err(map_store)?;
        if let Some(o) = ord {
            let mut order: Vec<String> = serde_json::from_str(&o.body).unwrap_or_default();
            order.retain(|n| n != name);
            self.put_order(&*store, &order, Some(o.version), by).await?;
        }
        let applied = self.refresh(&*store).await?;
        Ok(UpdateOutcome {
            section,
            version: 0,
            restart_required: Vec::new(),
            pending_restart: self.pending_restart(),
            warnings: validate::warnings(&self.current()),
            applied,
        })
    }

    /// Sets the match order of the trunks (a permutation of the existing trunk names).
    ///
    /// # Errors
    /// See [`UpdateError`]; `Invalid` when `order` is not a permutation of the trunks.
    pub async fn reorder_trunks(
        &self,
        order: Vec<String>,
        by: &str,
    ) -> Result<UpdateOutcome, UpdateError> {
        let _w = self.write.lock().await;
        let store = self.src.store().await.map_err(map_store)?;
        let rows = store.config_list().await.map_err(map_store)?;
        let mut have: Vec<String> = rows
            .iter()
            .filter_map(|r| r.name.strip_prefix("trunk:").map(str::to_string))
            .collect();
        let mut want = order.clone();
        have.sort();
        want.sort();
        if have != want {
            return Err(UpdateError::Invalid(vec![FieldError::new(
                "order",
                "invalid",
                "order must list every trunk exactly once",
            )]));
        }
        let cur = rows.iter().find(|r| r.name == TRUNK_ORDER);
        self.put_order(&*store, &order, cur.map(|r| r.version), by).await?;
        let applied = self.refresh(&*store).await?;
        Ok(UpdateOutcome {
            section: TRUNK_ORDER.into(),
            version: 0,
            restart_required: Vec::new(),
            pending_restart: self.pending_restart(),
            warnings: Vec::new(),
            applied,
        })
    }

    // ------------------------------------------------------------------ import / export

    async fn put_plain(
        &self,
        store: &dyn FaxStore,
        section: &Section,
        plain: &Value,
        by: &str,
        action: &str,
    ) -> Result<(), ImportError> {
        let stored = codec::encrypt_plain(section, plain, &self.secrets)
            .map_err(|e| ImportError::Invalid(e.to_string()))?;
        let name = section.name();
        let cur = store
            .config_get(&name)
            .await
            .map_err(|e| ImportError::Store(e.to_string()))?;
        let audit = ConfigAudit {
            action: action.into(),
            before: cur
                .as_ref()
                .and_then(|r| serde_json::from_str::<Value>(&r.body).ok())
                .map(|v| codec::mask_for_audit(section, &v).to_string()),
            after: Some(codec::mask_for_audit(section, &stored).to_string()),
        };
        store
            .config_put(
                &name,
                &stored.to_string(),
                cur.map(|r| r.version),
                by,
                &audit,
            )
            .await
            .map(|_| ())
            .map_err(|e| ImportError::Store(e.to_string()))
    }

    fn plain_sections(cfg: &RuntimeConfig) -> Vec<(Section, Value)> {
        fn j<T: Serialize>(v: &T) -> Value {
            serde_json::to_value(v).unwrap_or(Value::Null)
        }
        let mut out = vec![
            (Section::Sip, j(&cfg.sip)),
            (Section::Media, j(&cfg.media)),
            (Section::Call, j(&cfg.call)),
            (Section::Capture, j(&cfg.capture)),
            (Section::Delivery, j(&cfg.delivery)),
            (Section::Retention, j(&cfg.retention)),
            (Section::Routes, j(&cfg.routes)),
        ];
        for t in &cfg.trunks {
            out.push((Section::Trunk(t.name.clone()), j(t)));
        }
        out
    }

    async fn import_missing(
        &self,
        store: &dyn FaxStore,
        file: &RuntimeConfig,
        by: &str,
    ) -> Result<ImportReport, ImportError> {
        let mut rep = ImportReport::default();
        let rows = store
            .config_list()
            .await
            .map_err(|e| ImportError::Store(e.to_string()))?;
        for (sec, plain) in Self::plain_sections(file) {
            let name = sec.name();
            if rows.iter().any(|r| r.name == name) {
                continue;
            }
            self.put_plain(store, &sec, &plain, by, "import").await?;
            rep.written.push(name);
        }
        let order: Vec<String> = file.trunks.iter().map(|t| t.name.clone()).collect();
        if !order.is_empty() && !rows.iter().any(|r| r.name == TRUNK_ORDER) {
            self.put_order(store, &order, None, by)
                .await
                .map_err(|e| ImportError::Store(e.to_string()))?;
            rep.written.push(TRUNK_ORDER.into());
        }
        Ok(rep)
    }

    /// `config import`: writes the runtime configuration of `file` to the database. Without
    /// `force` it refuses when the database already holds runtime configuration; with `force`
    /// the database ends up exactly as the file (extra sections are deleted). The running
    /// server picks it up with `reload`.
    ///
    /// # Errors
    /// See [`ImportError`].
    pub async fn import(
        &self,
        file: &RuntimeConfig,
        force: bool,
        by: &str,
    ) -> Result<ImportReport, ImportError> {
        let _w = self.write.lock().await;
        let store = self
            .src
            .store()
            .await
            .map_err(|e| ImportError::Store(e.to_string()))?;
        let rows = store
            .config_list()
            .await
            .map_err(|e| ImportError::Store(e.to_string()))?;
        let have = rows.iter().filter(|r| is_runtime_section(&r.name)).count();
        if have > 0 && !force {
            return Err(ImportError::AlreadyConfigured(have));
        }
        let errs = validate::cross_check(file);
        if let Some(e) = errs.first() {
            return Err(ImportError::Invalid(e.clone()));
        }
        let mut rep = ImportReport::default();
        let wanted = Self::plain_sections(file);
        for r in &rows {
            if r.name == TRUNK_ORDER {
                continue;
            }
            let keep = wanted.iter().any(|(s, _)| s.name() == r.name);
            if !keep && r.name.starts_with("trunk:") {
                let audit = ConfigAudit {
                    action: "delete".into(),
                    before: None,
                    after: None,
                };
                store
                    .config_delete(&r.name, r.version, by, &audit)
                    .await
                    .map_err(|e| ImportError::Store(e.to_string()))?;
                rep.removed.push(r.name.clone());
            }
        }
        for (sec, plain) in &wanted {
            self.put_plain(&*store, sec, plain, by, "import").await?;
            rep.written.push(sec.name());
        }
        let order: Vec<String> = file.trunks.iter().map(|t| t.name.clone()).collect();
        let cur = store
            .config_get(TRUNK_ORDER)
            .await
            .map_err(|e| ImportError::Store(e.to_string()))?;
        self.put_order(&*store, &order, cur.map(|r| r.version), by)
            .await
            .map_err(|e| ImportError::Store(e.to_string()))?;
        Ok(rep)
    }

    /// `config export`: the database's runtime configuration as TOML (what `serve` would run
    /// with). Secrets are left out unless `include_secrets`.
    ///
    /// # Errors
    /// The database cannot be read.
    pub async fn export(&self, include_secrets: bool) -> Result<String, UpdateError> {
        let store = self.src.store().await.map_err(map_store)?;
        let st = self.read_all(&*store).await.map_err(map_store)?;
        Ok(to_toml(&st.config, include_secrets))
    }

    /// Everything `config check` and `/status` want to know about the stored configuration.
    pub fn status(&self) -> ConfigStatus {
        let st = self.state.load();
        ConfigStatus {
            source: st.source,
            key: match self.secrets.status() {
                KeyStatus::Loaded => "loaded".into(),
                KeyStatus::Created => "created".into(),
                KeyStatus::Replaced(p) => format!("replaced (old file moved to {p})"),
                KeyStatus::Unavailable(r) => format!("unavailable: {r}"),
            },
            undecryptable: st
                .issues
                .iter()
                .map(|i| format!("{}: {}", i.section, i.field))
                .collect(),
            versions: st.versions.clone(),
            pending_restart: self.pending_restart(),
        }
    }
}

/// See [`ConfigService::status`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ConfigStatus {
    /// Database or file fallback.
    pub source: Source,
    /// State of `secret.key`.
    pub key: String,
    /// Secrets that need to be entered again (`section: field`).
    pub undecryptable: Vec<String>,
    /// Section versions.
    pub versions: BTreeMap<String, i32>,
    /// Settings waiting for a restart.
    pub pending_restart: Vec<String>,
}

/// The version named in a store conflict message (`... is at version N (expected M)`).
fn version_in(msg: &str) -> Option<i32> {
    let rest = msg.split("at version ").nth(1)?;
    rest.split(|c: char| !c.is_ascii_digit()).next()?.parse().ok()
}

fn place(cfg: &mut RuntimeConfig, v: SectionValue) {
    match v {
        SectionValue::Sip(c) => cfg.sip = c,
        SectionValue::Media(c) => cfg.media = c,
        SectionValue::Call(c) => cfg.call = c,
        SectionValue::Capture(c) => cfg.capture = c,
        SectionValue::Delivery(c) => cfg.delivery = c,
        SectionValue::Retention(c) => cfg.retention = c,
        SectionValue::Routes(c) => cfg.routes = c,
        SectionValue::Trunk(t) => match cfg.trunks.iter_mut().find(|x| x.name == t.name) {
            Some(slot) => *slot = *t,
            None => cfg.trunks.push(*t),
        },
    }
}

fn json_to_toml(v: &Value) -> Option<toml::Value> {
    Some(match v {
        Value::Null => return None,
        Value::Bool(b) => toml::Value::Boolean(*b),
        Value::Number(n) => match n.as_i64() {
            Some(i) => toml::Value::Integer(i),
            None => toml::Value::Float(n.as_f64()?),
        },
        Value::String(s) => toml::Value::String(s.clone()),
        Value::Array(a) => toml::Value::Array(a.iter().filter_map(json_to_toml).collect()),
        Value::Object(o) => toml::Value::Table(
            o.iter()
                .filter_map(|(k, v)| json_to_toml(v).map(|t| (k.clone(), t)))
                .collect(),
        ),
    })
}

/// A configuration as TOML in the layout of the configuration file (`[sip]`, `[[trunks]]`, ...).
/// Secrets are omitted unless `include_secrets`.
pub fn to_toml(cfg: &RuntimeConfig, include_secrets: bool) -> String {
    let mut root = toml::Table::new();
    let mut trunks = Vec::new();
    let mut routes = Vec::new();
    for (sec, v) in ConfigService::plain_sections(cfg) {
        let v = if include_secrets { v } else { codec::strip(&sec, &v) };
        let Some(t) = json_to_toml(&v) else { continue };
        match sec {
            Section::Trunk(_) => trunks.push(t),
            Section::Routes => {
                if let toml::Value::Table(mut tb) = t
                    && let Some(toml::Value::Array(a)) = tb.remove("routes")
                {
                    routes = a;
                }
            }
            other => {
                root.insert(other.name(), t);
            }
        }
    }
    if !routes.is_empty() {
        root.insert("routes".into(), toml::Value::Array(routes));
    }
    if !trunks.is_empty() {
        root.insert("trunks".into(), toml::Value::Array(trunks));
    }
    let header = if include_secrets {
        "# sip-faxd runtime configuration, exported WITH secrets: keep this file private.\n"
    } else {
        "# sip-faxd runtime configuration, exported WITHOUT secrets (passwords and HMAC keys are\n# left out and must be entered again after an import).\n"
    };
    format!(
        "{header}{}",
        toml::to_string(&root).unwrap_or_else(|e| format!("# export failed: {e}\n"))
    )
}
