//! The runtime configuration of the daemon (M4a): the database-backed [`ConfigService`], what
//! applies a change to the running server, the accounts service and the first administrator.
//!
//! `main.rs` only calls [`Runtime::start`], [`Runtime::attach`] and [`Runtime::detach`]; the rest
//! of the daemon keeps working with the plain `FaxInConfig` it is given.

use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use accounts::{AuthConfig, AuthService};
use async_trait::async_trait;
use fax_in::FaxIn;
use fax_in::config::StorageConfig;
use runtime_config::{
    ApplyReport, Boot, ConfigApplier, ConfigService, RuntimeConfig, SecretBox, StoreSource,
};
use store::SystemClock;
use tokio::task::JoinHandle;

use crate::db::Db;
use crate::delivery_service::Reconfigurer;
use crate::settings::{AppConfig, mask_url};

/// How often the daemon looks again for a database that was not reachable at start.
const DB_RETRY: Duration = Duration::from_secs(10);

/// The operator recorded for changes made on the command line: `cli:<operating system user>`.
pub fn cli_operator() -> String {
    let user = std::env::var("SUDO_USER")
        .or_else(|_| std::env::var("USER"))
        .or_else(|_| std::env::var("USERNAME"))
        .unwrap_or_else(|_| "unknown".into());
    format!("cli:{user}")
}

/// Applies a new snapshot to the running fax-in controller and the delivery service.
pub struct LiveApplier {
    fax: Mutex<Option<Arc<FaxIn>>>,
    delivery: Mutex<Option<Reconfigurer>>,
    storage: StorageConfig,
}

impl LiveApplier {
    fn new(storage: StorageConfig) -> LiveApplier {
        LiveApplier {
            fax: Mutex::new(None),
            delivery: Mutex::new(None),
            storage,
        }
    }
}

#[async_trait]
impl ConfigApplier for LiveApplier {
    async fn apply(&self, old: &RuntimeConfig, new: &RuntimeConfig) -> ApplyReport {
        let mut rep = ApplyReport::default();
        let fax = self.fax.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(fax) = fax {
            match fax.apply_config(new.to_fax_in(&self.storage)).await {
                Ok(o) => {
                    rep.registrations_restarted = o.registrations_restarted;
                    rep.trunks_changed = o.trunks_changed;
                    if !o.restart_required.is_empty() {
                        tracing::warn!(
                            "saved, but these settings only take effect after a restart: {}",
                            o.restart_required.join(", ")
                        );
                    }
                }
                Err(e) => {
                    tracing::error!("the new configuration was saved but fax-in refused it: {e}");
                    rep.notes
                        .push(format!("fax-in refused the configuration: {e}"));
                }
            }
        }
        let delivery_changed = serde_json::to_value(&old.routes).ok()
            != serde_json::to_value(&new.routes).ok()
            || old.delivery != new.delivery;
        if delivery_changed
            && let Some(d) = self
                .delivery
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
            && let Err(errs) = d.reconfigure(new.routes.clone(), new.delivery.clone())
        {
            tracing::error!("delivery refused the new routes: {}", errs.join("; "));
            rep.notes
                .push(format!("delivery refused the routes: {}", errs.join("; ")));
        }
        rep
    }
}

/// What `serve` keeps of the runtime configuration and the accounts.
pub struct Runtime {
    /// Configuration service (for M4b's REST layer and M4d's control socket).
    pub config: Arc<ConfigService>,
    /// Accounts and sessions.
    pub auth: Arc<AuthService>,
    applier: Arc<LiveApplier>,
    tasks: Vec<JoinHandle<()>>,
}

impl Runtime {
    /// Opens `conf/secret.key`, imports or loads the configuration, makes the first
    /// administrator when there is none, and starts the housekeeping of sessions (`db_ready`: the
    /// database answered at start). The database
    /// being down is not an error: the configuration file is used until it is back (a task keeps
    /// trying).
    pub async fn start(cfg: &AppConfig, db: Arc<Db>, db_ready: bool) -> Runtime {
        let secrets = Arc::new(SecretBox::load_or_create(&cfg.conf_dir.join("secret.key")));
        match secrets.status() {
            runtime_config::KeyStatus::Created => tracing::warn!(
                "conf/secret.key did not exist: a new key was created. Secrets stored earlier (if any) cannot be decrypted and have to be entered again"
            ),
            runtime_config::KeyStatus::Replaced(old) => tracing::error!(
                "conf/secret.key has the wrong size: it was moved to {old} and a new key was created; secrets stored earlier have to be entered again"
            ),
            runtime_config::KeyStatus::Unavailable(why) => tracing::error!(
                "conf/secret.key is not usable ({why}): secrets cannot be stored or read until this is fixed"
            ),
            runtime_config::KeyStatus::Loaded => {}
        }
        let src: Arc<dyn StoreSource> = db.clone();
        let config = Arc::new(ConfigService::new(
            src.clone(),
            secrets,
            cfg.runtime.clone(),
            cfg.file_sections.clone(),
        ));
        let applier = Arc::new(LiveApplier::new(cfg.fax_in.storage.clone()));
        config.set_applier(applier.clone());
        let auth = Arc::new(AuthService::new(
            src,
            Arc::new(SystemClock),
            AuthConfig {
                idle: chrono::Duration::hours(i64::from(cfg.web.session_idle_hours)),
                ..AuthConfig::default()
            },
        ));
        // A database that did not answer a moment ago is not asked again here (a connection
        // attempt can take half a minute): the file's configuration serves until the retry
        // task below finds the database.
        let boot = if db_ready {
            tokio::time::timeout(Duration::from_secs(20), config.bootstrap())
                .await
                .unwrap_or_else(|_| Boot::Unreachable("timed out".into()))
        } else {
            Boot::Unreachable("the database was not reachable at start".into())
        };
        let mut rt = Runtime {
            config,
            auth,
            applier,
            tasks: Vec::new(),
        };
        let initial = cfg.conf_dir.join("initial-admin-password");
        rt.auth.watch_initial_password_file(initial.clone());
        match boot {
            Boot::Unreachable(why) => {
                tracing::error!(
                    "the runtime configuration cannot be read from the database ({}): the configuration file is used until it is back: {why}",
                    mask_url(&cfg.database.url)
                );
                let (config, auth) = (rt.config.clone(), rt.auth.clone());
                let from_file = !cfg.file_sections.is_empty();
                rt.tasks.push(tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(DB_RETRY).await;
                        match config.retry_bootstrap().await {
                            Boot::Unreachable(_) => continue,
                            other => {
                                log_boot(&other, from_file);
                                make_first_admin(&auth, initial).await;
                                return;
                            }
                        }
                    }
                }));
            }
            other => {
                log_boot(&other, !cfg.file_sections.is_empty());
                make_first_admin(&rt.auth, initial).await;
            }
        }
        let auth = rt.auth.clone();
        rt.tasks.push(tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                if let Err(e) = auth.purge_expired_sessions().await {
                    tracing::warn!("expired sessions not purged: {e}");
                }
            }
        }));
        rt
    }

    /// The configuration `serve` starts with (the database's, or the file's when the database
    /// is down).
    pub fn current(&self) -> Arc<RuntimeConfig> {
        self.config.current()
    }

    /// From now on configuration changes are applied to `fax` and `delivery`.
    pub fn attach(&self, fax: Arc<FaxIn>, delivery: Reconfigurer) {
        *self.applier.fax.lock().unwrap_or_else(|e| e.into_inner()) = Some(fax);
        *self
            .applier
            .delivery
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(delivery);
    }

    /// Stops applying changes (shutdown) and the housekeeping tasks.
    pub fn detach(&self) {
        self.config.clear_applier();
        *self.applier.fax.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self
            .applier
            .delivery
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = None;
        for t in &self.tasks {
            t.abort();
        }
    }
}

/// `reload` on the control socket: the database's runtime configuration, applied to the running
/// server (the same path as a change made in the web interface).
pub struct ConfigReloader(pub Arc<ConfigService>);

#[async_trait]
impl crate::service::hub::Reloader for ConfigReloader {
    async fn reload(&self) -> Result<String, String> {
        let out = self.0.reload().await.map_err(|e| e.to_string())?;
        Ok(reload_message(&out))
    }
}

/// One line for the operator about a [`ConfigService::reload`].
pub fn reload_message(out: &runtime_config::ReloadOutcome) -> String {
    let mut msg = if out.changed {
        "the runtime configuration was reloaded from the database and applied".to_string()
    } else {
        "the runtime configuration in the database is unchanged".to_string()
    };
    if !out.applied.trunks_changed.is_empty() {
        msg.push_str(&format!(
            "; trunks changed: {}",
            out.applied.trunks_changed.join(", ")
        ));
    }
    if !out.applied.registrations_restarted.is_empty() {
        msg.push_str(&format!(
            "; registrations restarted: {}",
            out.applied.registrations_restarted.join(", ")
        ));
    }
    if !out.pending_restart.is_empty() {
        msg.push_str(&format!(
            "; waiting for a restart: {}",
            out.pending_restart.join(", ")
        ));
    }
    msg
}

/// `from_file`: the configuration file has runtime sections (a file of 0.3.0 or older); without
/// them an import writes the defaults.
fn log_boot(boot: &Boot, from_file: bool) {
    match boot {
        Boot::Imported(rep) if !from_file => tracing::info!(
            sections = rep.written.len(),
            "the database had no runtime configuration: the defaults were written to it"
        ),
        Boot::Imported(rep) => tracing::info!(
            sections = rep.written.len(),
            "the runtime configuration of the configuration file was imported into the database"
        ),
        Boot::FromDatabase { ignored } if !ignored.is_empty() => tracing::warn!(
            "these sections of the configuration file are not used (the settings live in the database: change them in the web interface) and can be removed from the file: {}",
            ignored.join(", ")
        ),
        _ => {}
    }
}

async fn make_first_admin(auth: &AuthService, file: PathBuf) {
    match auth.ensure_initial_admin(file).await {
        Ok(Some(path)) => tracing::warn!(
            "created the administrator `admin`; its initial password is in {} and has to be changed at the first login",
            path.display()
        ),
        Ok(None) => {}
        Err(e) => tracing::error!("cannot create the first administrator: {e}"),
    }
}

#[cfg(test)]
mod tests;
