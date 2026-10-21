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
    /// administrator when there is none, and starts the housekeeping of sessions. The database
    /// being down is not an error: the configuration file is used until it is back (a task keeps
    /// trying).
    pub async fn start(cfg: &AppConfig, db: Arc<Db>) -> Runtime {
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
            AuthConfig::default(),
        ));
        let boot = config.bootstrap().await;
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
                rt.tasks.push(tokio::spawn(async move {
                    loop {
                        tokio::time::sleep(DB_RETRY).await;
                        match config.retry_bootstrap().await {
                            Boot::Unreachable(_) => continue,
                            other => {
                                log_boot(&other);
                                make_first_admin(&auth, initial).await;
                                return;
                            }
                        }
                    }
                }));
            }
            other => {
                log_boot(&other);
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

fn log_boot(boot: &Boot) {
    match boot {
        Boot::Imported(rep) => tracing::info!(
            sections = rep.written.len(),
            "the runtime configuration of the configuration file was imported into the database"
        ),
        Boot::FromDatabase { ignored } if !ignored.is_empty() => tracing::warn!(
            "these sections of the configuration file are managed by the database now, the values in the file have no effect (change them in the web interface, or with `sip-faxd config import --force`): {}",
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
mod tests {
    //! K07 at the daemon level: a change made through the [`ConfigService`] reaches the running
    //! fax-in controller and the delivery service, on a real SQLite store and real sockets.
    use super::*;
    use crate::delivery_service::DeliveryService;
    use fax_in::StartOptions;
    use serde_json::json;

    const TEXT: &str = "[sip]\nbind = \"127.0.0.1:0\"\n[capture]\nenabled = false\n\
        [call]\nmax_duration_secs = 600\n\
        [[trunks]]\nname = \"local\"\nallowed_sources = [\"127.0.0.0/8\"]\n";

    #[tokio::test(flavor = "multi_thread")]
    async fn k07_changes_through_the_service_reach_fax_in_and_delivery() {
        let dir = tempfile::tempdir().unwrap();
        let slash = |p: std::path::PathBuf| p.display().to_string().replace('\\', "/");
        let text = format!(
            "[storage]\ndata_dir = {:?}\n[database]\nurl = {:?}\n{TEXT}",
            slash(dir.path().join("data")),
            format!("sqlite://{}", slash(dir.path().join("fax.db"))),
        );
        let cfg = AppConfig::parse(&text, &dir.path().join("sip-faxd.toml")).unwrap();
        let db = Arc::new(Db::new(&cfg, dir.path()));
        assert!(matches!(
            db.start().await.unwrap(),
            crate::db::Startup::Ready { .. }
        ));
        let rt = Runtime::start(&cfg, db.clone()).await;
        // the first administrator exists, with the password in a private file
        let users = rt.auth.list_users().await.unwrap();
        assert_eq!(users.len(), 1);
        assert!(users[0].must_change_password);
        assert!(dir.path().join("initial-admin-password").exists());
        assert!(dir.path().join("secret.key").exists());

        let live = rt.current();
        let delivery = DeliveryService::start_default(
            db.clone(),
            live.routes.clone(),
            dir.path().join("data"),
            live.delivery.clone(),
        );
        let (fax, _events) =
            FaxIn::start(live.to_fax_in(&cfg.fax_in.storage), StartOptions::default())
                .await
                .unwrap();
        let fax = Arc::new(fax);
        rt.attach(fax.clone(), delivery.reconfigurer());

        // a call setting: applied to the next call, nothing needs a restart
        let call = rt.config.get_section("call").await.unwrap();
        let mut body = call.body.clone();
        body["max_duration_secs"] = json!(77);
        let out = rt
            .config
            .update("call", body, call.version, "admin")
            .await
            .unwrap();
        assert!(out.restart_required.is_empty());
        assert_eq!(fax.snapshot().cfg.call.max_duration_secs, 77);
        assert!(out.applied.trunks_changed.is_empty());

        // a trunk: the controller's trunk table follows
        let t = rt.config.get_section("trunk:local").await.unwrap();
        let mut body = t.body.clone();
        body["numbers"]["strip_plus"] = json!(true);
        let out = rt
            .config
            .update("trunk:local", body, t.version, "admin")
            .await
            .unwrap();
        assert_eq!(out.applied.trunks_changed, ["local"]);
        assert!(fax.snapshot().table.trunks()[0].cfg.numbers.strip_plus);

        // a setting that needs a restart is stored, reported, and not applied to the sockets
        let sip = rt.config.get_section("sip").await.unwrap();
        let mut body = sip.body.clone();
        body["bind"] = json!("127.0.0.1:5999");
        let out = rt
            .config
            .update("sip", body, sip.version, "admin")
            .await
            .unwrap();
        assert_eq!(out.restart_required, ["sip.bind"]);
        assert_eq!(fax.pending_restart(), ["sip.bind"]);
        assert_ne!(fax.udp_addr().unwrap().port(), 5999);

        // routes: the delivery service takes them (a broken set never gets this far)
        let routes = rt.config.get_section("routes").await.unwrap();
        let body = json!({"routes": [{"dnis": "1", "deliveries": [{"kind": "folder", "dir": slash(dir.path().join("out"))}]}]});
        let out = rt
            .config
            .update("routes", body, routes.version, "admin")
            .await
            .unwrap();
        assert!(out.applied.notes.is_empty(), "{:?}", out.applied.notes);

        rt.detach();
        let fax = Arc::try_unwrap(fax)
            .ok()
            .expect("nothing else holds the controller");
        fax.shutdown().await;
        delivery.shutdown().await;
        db.close().await;
    }

    #[test]
    fn m14_the_operator_is_cli_and_the_os_user() {
        let who = cli_operator();
        assert!(who.starts_with("cli:") && who.len() > 4, "{who}");
    }
}
