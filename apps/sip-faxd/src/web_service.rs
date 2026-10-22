//! The web interface inside `serve` (M4b): `[web]`, the certificate, the listener.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use accounts::AuthService;
use chrono::Utc;
use fax_in::recorder::{RecordWritten, WrittenHook};
use runtime_config::ConfigService;
use store::{StoreSource, SystemClock};
use web::config::Scheme;
use web::events::{self, EventBus};
use web::server::WebServer;
use web::tls;
use web::{ServiceStatus, StatusSource, TrunkState};

use crate::service::hub::Hub;
use crate::settings::AppConfig;

/// How often `status.changed` compares the service's state.
const STATUS_EVERY: Duration = Duration::from_secs(2);

/// The recorder's writes as `fax.created` / `fax.progress` / `fax.finalized`.
pub fn recorder_events(bus: Arc<EventBus>) -> WrittenHook {
    Arc::new(move |w| match w {
        RecordWritten::Created { id, call_id } => bus.fax_created(*id, call_id),
        RecordWritten::Pages { id, call_id, pages } => bus.fax_progress(*id, call_id, *pages),
        RecordWritten::Finalized { id, call_id } => bus.fax_finalized(*id, call_id),
    })
}

/// Starts the web interface when `[web] enabled`. The certificate is loaded (or made) first;
/// its fingerprint, a new or replaced self-signed certificate and a near end are logged.
///
/// # Errors
/// The certificate cannot be used or the address cannot be bound; the message names the file
/// or the address.
pub async fn start(
    cfg: &AppConfig,
    base: &Path,
    auth: Arc<AuthService>,
    config: Arc<ConfigService>,
    hub: Arc<Hub>,
    events: Arc<EventBus>,
    store: Arc<dyn StoreSource>,
) -> Result<Option<WebServer>, String> {
    let w = &cfg.web;
    if !w.enabled {
        return Ok(None);
    }
    if let Some(warning) = w.plaintext_warning() {
        tracing::warn!("{warning}");
    }
    let tls = match w.scheme {
        Scheme::Http => None,
        Scheme::Https => {
            let names = tls::subject_names(
                tls::hostname().as_deref(),
                w.bind.ip(),
                &tls::local_ips(),
                &w.tls_names,
            );
            let now = Utc::now();
            let m = tls::load(w, base, &cfg.conf_dir, &names, now.date_naive())
                .map_err(|e| format!("[web] {e}"))?;
            if !m.generated.is_empty() {
                tracing::warn!(
                    cert = %m.cert_path.display(),
                    names = %m.generated.join(", "),
                    "web: a self-signed certificate was made; browsers warn about it until it is trusted"
                );
            }
            if let Some(old) = &m.replaced {
                tracing::warn!(
                    old_fingerprint = %old,
                    "web: the self-signed certificate was about to end and was replaced: browsers have to accept the new one"
                );
            }
            if let Some(warning) = m.expiry_warning(now) {
                tracing::warn!("web: {warning}");
            }
            tracing::info!(
                "web: certificate SHA-256 fingerprint {} (valid until {})",
                m.fingerprint,
                m.not_after.format("%Y-%m-%d")
            );
            Some(m.config)
        }
    };
    let https = tls.is_some();
    let status: Arc<dyn StatusSource> = Arc::new(HubStatus(hub));
    let pending = config.clone();
    // (lives as long as the process)
    events::watch_status(
        events.clone(),
        status.clone(),
        move || pending.pending_restart(),
        STATUS_EVERY,
    );
    let state = web::AppState {
        auth,
        config,
        status,
        events,
        ws_ping: web::api::events::PING_EVERY,
        store,
        data_dir: cfg.data_dir(base),
        preview: Arc::default(),
        clock: Arc::new(SystemClock),
        https,
        request_timeout: web::REQUEST_TIMEOUT,
    };
    let srv = WebServer::start(w.bind, web::app(state), tls)
        .await
        .map_err(|e| format!("[web] cannot listen on {}: {e}", w.bind))?;
    tracing::info!(
        "web interface on {}://{}",
        if https { "https" } else { "http" },
        srv.local_addr()
    );
    Ok(Some(srv))
}

/// `GET /status` reads what the control socket's `status` answers.
struct HubStatus(Arc<Hub>);

impl StatusSource for HubStatus {
    fn status(&self) -> ServiceStatus {
        let s = self.0.status();
        ServiceStatus {
            version: s.version,
            state: s.state,
            started_at: s.started_at,
            uptime_secs: s.uptime_secs,
            listen: s.listen,
            active_calls: s.active_calls,
            trunks: s
                .trunks
                .into_iter()
                .map(|t| TrunkState {
                    name: t.name,
                    registration: t.registration,
                })
                .collect(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    use accounts::AuthConfig;
    use fax_in::recorder::RecordWritten;
    use runtime_config::{RuntimeConfig, SecretBox};
    use store::{ConnectOptions, ManualClock};
    use tracing_subscriber::fmt::MakeWriter;

    use crate::service::hub::HubInfo;

    #[test]
    fn v01_the_recorder_hook_publishes_created_progress_and_finalized_in_order() {
        let bus = Arc::new(EventBus::new());
        let hook = recorder_events(bus.clone());
        let c = "call-1".to_string();
        hook(&RecordWritten::Created {
            id: 7,
            call_id: c.clone(),
        });
        for pages in 1..=3 {
            hook(&RecordWritten::Pages {
                id: 7,
                call_id: c.clone(),
                pages,
            });
        }
        hook(&RecordWritten::Finalized {
            id: 7,
            call_id: c.clone(),
        });
        assert_eq!(bus.last_seq(), 5);
        // what a client that connects now with `since=0` is sent
        let (_rx, backlog, last) = bus.attach(Some(0));
        assert_eq!(last, 5);
        let web::events::Backlog::Events(events) = backlog else {
            panic!("a resync")
        };
        let seen: Vec<serde_json::Value> = events
            .iter()
            .map(|e| serde_json::from_str(&e.json).unwrap())
            .collect();
        let kinds: Vec<&str> = seen.iter().map(|v| v["type"].as_str().unwrap()).collect();
        assert_eq!(
            kinds,
            [
                "fax.created",
                "fax.progress",
                "fax.progress",
                "fax.progress",
                "fax.finalized"
            ]
        );
        let seqs: Vec<u64> = seen.iter().map(|v| v["seq"].as_u64().unwrap()).collect();
        assert_eq!(seqs, [1, 2, 3, 4, 5]);
        let pages: Vec<u64> = seen[1..4]
            .iter()
            .map(|v| v["data"]["pages"].as_u64().unwrap())
            .collect();
        assert_eq!(pages, [1, 2, 3]);
        for v in &seen {
            assert_eq!(v["data"]["id"], 7);
            assert_eq!(v["data"]["call_id"], "call-1");
        }
    }

    /// Collects what `tracing` logs in this thread.
    #[derive(Clone, Default)]
    struct Logs(Arc<Mutex<Vec<u8>>>);

    impl std::io::Write for Logs {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }

    impl<'a> MakeWriter<'a> for Logs {
        type Writer = Logs;
        fn make_writer(&'a self) -> Logs {
            self.clone()
        }
    }

    impl Logs {
        fn text(&self) -> String {
            String::from_utf8_lossy(&self.0.lock().unwrap()).into_owned()
        }

        /// Makes this the subscriber of the current thread until the guard is dropped.
        fn capture(&self) -> tracing::subscriber::DefaultGuard {
            tracing::subscriber::set_default(
                tracing_subscriber::fmt()
                    .with_writer(self.clone())
                    .with_ansi(false)
                    .with_max_level(tracing::Level::DEBUG)
                    .finish(),
            )
        }
    }

    struct Fixture {
        dir: tempfile::TempDir,
        cfg: AppConfig,
        auth: Arc<AuthService>,
        config: Arc<ConfigService>,
        hub: Arc<Hub>,
        src: Arc<dyn StoreSource>,
    }

    /// A configuration with `web_toml` as its `[web]` table, a real SQLite store.
    async fn fixture(web_toml: &str) -> Fixture {
        let dir = tempfile::tempdir().unwrap();
        let conf = dir.path().join("conf");
        std::fs::create_dir_all(&conf).unwrap();
        let text = format!(
            "[web]\n{web_toml}\n[[trunks]]\nname = \"t\"\nallowed_sources = [\"10.0.0.0/8\"]\n"
        );
        let cfg = AppConfig::parse(&text, &conf.join("sip-faxd.toml")).unwrap();
        let clock = Arc::new(ManualClock::at_millis(1_792_540_800_000));
        let store = store::connect_with(
            &format!("sqlite://{}/w.db", dir.path().display()),
            ConnectOptions {
                max_connections: 2,
                auto_migrate: true,
                clock: clock.clone(),
            },
        )
        .await
        .unwrap();
        let src: Arc<dyn StoreSource> = Arc::new(store);
        let auth = Arc::new(AuthService::new(src.clone(), clock, AuthConfig::default()));
        let config = Arc::new(ConfigService::new(
            src.clone(),
            Arc::new(SecretBox::load_or_create(&conf.join("secret.key"))),
            RuntimeConfig::default(),
            Vec::new(),
        ));
        let hub = Hub::new(
            HubInfo {
                pid: 1,
                version: "test".into(),
                listen: vec!["udp 127.0.0.1:5060".into()],
            },
            Duration::from_secs(5),
            Vec::new,
        );
        Fixture {
            dir,
            cfg,
            auth,
            config,
            hub,
            src,
        }
    }

    async fn start_with(f: &Fixture) -> Result<Option<WebServer>, String> {
        start(
            &f.cfg,
            f.dir.path(),
            f.auth.clone(),
            f.config.clone(),
            f.hub.clone(),
            Arc::new(EventBus::new()),
            f.src.clone(),
        )
        .await
    }

    fn https_toml(extra: &str) -> String {
        format!("enabled = true\nscheme = \"https\"\nbind = \"127.0.0.1:0\"\n{extra}")
    }

    #[tokio::test]
    async fn h06_plain_http_on_a_reachable_address_warns_at_start_and_on_loopback_does_not() {
        let logs = Logs::default();
        let g = logs.capture();
        let f = fixture("enabled = true\nscheme = \"http\"\nbind = \"0.0.0.0:0\"").await;
        let srv = start_with(&f).await.unwrap().expect("enabled");
        let t = logs.text();
        assert!(
            t.contains("WARN") && t.contains("unencrypted") && t.contains("0.0.0.0"),
            "{t}"
        );
        assert!(t.contains("web interface on http://"), "{t}");
        srv.shutdown(Duration::from_secs(1)).await;
        drop(g);

        let quiet = Logs::default();
        let _g2 = quiet.capture();
        let f = fixture("enabled = true\nscheme = \"http\"\nbind = \"127.0.0.1:0\"").await;
        let srv = start_with(&f).await.unwrap().unwrap();
        let t = quiet.text();
        assert!(!t.contains("unencrypted") && !t.contains("WARN"), "{t}");
        srv.shutdown(Duration::from_secs(1)).await;
        // disabled: nothing starts and nothing is made
        let f = fixture("enabled = false").await;
        assert!(start_with(&f).await.unwrap().is_none());
    }

    /// The fingerprint of the DER inside a PEM certificate file (the PEM is decoded here, not by
    /// the code under test; the openssl comparison is `crates/web/tests/openssl.rs`).
    fn fingerprint_of_pem(path: &Path) -> String {
        let pem = std::fs::read_to_string(path).unwrap();
        let b64: String = pem.lines().filter(|l| !l.starts_with("-----")).collect();
        tls::fingerprint(&decode_base64(&b64))
    }

    #[tokio::test]
    async fn h08_h01_https_without_a_certificate_makes_one_and_logs_its_fingerprint() {
        let logs = Logs::default();
        let g = logs.capture();
        let f = fixture(&https_toml("tls_names = [\"fax.example.test\"]")).await;
        let srv = start_with(&f).await.unwrap().unwrap();
        let t = logs.text();
        assert!(t.contains("a self-signed certificate was made"), "{t}");
        assert!(t.contains("fax.example.test"), "the names are logged: {t}");
        // the fingerprint in the log is the one of the certificate file that was written
        let (crt, _) = tls::self_signed_paths(&f.cfg.conf_dir);
        let fp = fingerprint_of_pem(&crt);
        assert!(
            t.contains(&format!(
                "certificate SHA-256 fingerprint {fp} (valid until "
            )),
            "the log has {fp}: {t}"
        );
        assert!(t.contains("web interface on https://"), "{t}");
        srv.shutdown(Duration::from_secs(1)).await;
        drop(g);

        // the second start: the same certificate, no "made" warning
        let again = Logs::default();
        let _g2 = again.capture();
        let srv = start_with(&f).await.unwrap().unwrap();
        let t2 = again.text();
        assert!(!t2.contains("was made"), "{t2}");
        assert!(t2.contains(&format!("fingerprint {fp}")), "{t2}");
        srv.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn h04_a_self_signed_certificate_that_ends_soon_is_replaced_with_a_warning_and_the_old_fingerprint()
     {
        let f = fixture(&https_toml("")).await;
        // a certificate made 800 days ago ends in 25 days
        let old = tls::load(
            &f.cfg.web,
            f.dir.path(),
            &f.cfg.conf_dir,
            &["localhost".to_string()],
            (Utc::now() - chrono::Duration::days(800)).date_naive(),
        )
        .unwrap();
        let logs = Logs::default();
        let _g = logs.capture();
        let srv = start_with(&f).await.unwrap().unwrap();
        let t = logs.text();
        assert!(
            t.contains("was about to end and was replaced") && t.contains(&old.fingerprint),
            "{t}"
        );
        let (crt, _) = tls::self_signed_paths(&f.cfg.conf_dir);
        let fresh = fingerprint_of_pem(&crt);
        assert_ne!(fresh, old.fingerprint, "a new certificate");
        assert!(t.contains(&format!("fingerprint {fresh}")), "{t}");
        srv.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn h04_a_configured_certificate_that_ends_soon_is_kept_with_a_warning() {
        // a certificate of our own making, 800 days old, supplied as the user's own
        let other = tempfile::tempdir().unwrap();
        let mine = tls::load(
            &web::config::WebConfig {
                enabled: true,
                ..Default::default()
            },
            other.path(),
            other.path(),
            &["localhost".to_string()],
            (Utc::now() - chrono::Duration::days(800)).date_naive(),
        )
        .unwrap();
        let (c, k) = tls::self_signed_paths(other.path());
        let before = std::fs::read(&c).unwrap();
        let f = fixture(&https_toml(&format!(
            "cert = {:?}\nkey = {:?}",
            c.display().to_string(),
            k.display().to_string()
        )))
        .await;
        let logs = Logs::default();
        let _g = logs.capture();
        let srv = start_with(&f).await.unwrap().unwrap();
        let t = logs.text();
        assert!(t.contains("expires on") && t.contains("day(s) left"), "{t}");
        assert!(!t.contains("replaced") && !t.contains("was made"), "{t}");
        assert!(
            t.contains(&format!("fingerprint {}", mine.fingerprint)),
            "{t}"
        );
        assert_eq!(std::fs::read(&c).unwrap(), before, "the file is untouched");
        srv.shutdown(Duration::from_secs(1)).await;
    }

    #[tokio::test]
    async fn h03_a_missing_or_mismatched_certificate_stops_the_start_and_names_the_file() {
        let other = tempfile::tempdir().unwrap();
        let mk = |name: &str| {
            let d = other.path().join(name);
            tls::load(
                &web::config::WebConfig {
                    enabled: true,
                    ..Default::default()
                },
                &d,
                &d,
                &[name.to_string()],
                Utc::now().date_naive(),
            )
            .unwrap();
            tls::self_signed_paths(&d)
        };
        let (c1, _k1) = mk("one");
        let (_c2, k2) = mk("two");
        // the key of another certificate
        let f = fixture(&https_toml(&format!(
            "cert = {:?}\nkey = {:?}",
            c1.display().to_string(),
            k2.display().to_string()
        )))
        .await;
        let e = start_with(&f).await.err().expect("must not start");
        assert!(
            e.starts_with("[web]") && e.contains("cannot be used"),
            "{e}"
        );
        // a file that does not exist
        let f = fixture(&https_toml(&format!(
            "cert = {:?}\nkey = \"nope.key\"",
            c1.display().to_string()
        )))
        .await;
        let e = start_with(&f).await.err().expect("must not start");
        assert!(
            e.contains("nope.key") && e.contains("does not exist"),
            "{e}"
        );
        // an address that cannot be bound: the message names it
        let taken = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = taken.local_addr().unwrap().to_string();
        let f = fixture(&format!(
            "enabled = true\nscheme = \"http\"\nbind = \"{addr}\""
        ))
        .await;
        let e = start_with(&f).await.err().expect("must not start");
        assert!(e.contains("cannot listen on") && e.contains(&addr), "{e}");
    }

    /// Standard base64 (PEM bodies), without a helper of ours.
    fn decode_base64(s: &str) -> Vec<u8> {
        let val = |c: u8| match c {
            b'A'..=b'Z' => c - b'A',
            b'a'..=b'z' => c - b'a' + 26,
            b'0'..=b'9' => c - b'0' + 52,
            b'+' => 62,
            b'/' => 63,
            _ => 255,
        };
        let mut out = Vec::new();
        let mut acc = 0u32;
        let mut bits = 0;
        for c in s.bytes().filter(|c| *c != b'=') {
            acc = (acc << 6) | u32::from(val(c));
            bits += 6;
            if bits >= 8 {
                bits -= 8;
                out.push((acc >> bits) as u8);
                acc &= (1 << bits) - 1;
            }
        }
        out
    }
}
