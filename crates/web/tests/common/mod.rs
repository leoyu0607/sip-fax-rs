//! A real server on 127.0.0.1 with a real SQLite store (no mocks, test plan section 0).
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use accounts::{AuthConfig, AuthService, NewAccount, Role};
use runtime_config::{ConfigService, RuntimeConfig, SecretBox};
use store::{Clock, ConnectOptions, FaxStore, ManualClock, StoreSource};
use web::events::EventBus;
use web::server::WebServer;
use web::{AppState, ServiceStatus, StatusSource, TrunkState, app};

pub const T0: i64 = 1_792_540_800_000;
pub const PW: &str = "correct horse battery";

pub struct Env {
    pub srv: WebServer,
    pub state: AppState,
    pub data_dir: std::path::PathBuf,
    pub base: String,
    pub store: Arc<dyn FaxStore>,
    pub auth: Arc<AuthService>,
    pub clock: Arc<ManualClock>,
    pub config: Arc<ConfigService>,
    pub status: Arc<FixedStatus>,
    pub events: Arc<EventBus>,
    pub _dir: tempfile::TempDir,
}

impl Env {
    pub async fn stop(self) {
        self.srv.shutdown(Duration::from_secs(1)).await;
    }

    pub fn url(&self, path: &str) -> String {
        format!("{}{path}", self.base)
    }
}

/// What `/status` reports in the tests; change it with [`FixedStatus::set`].
pub struct FixedStatus(pub std::sync::Mutex<ServiceStatus>);

impl FixedStatus {
    pub fn set(&self, f: impl FnOnce(&mut ServiceStatus)) {
        f(&mut self.0.lock().unwrap());
    }
}

impl StatusSource for FixedStatus {
    fn status(&self) -> ServiceStatus {
        self.0.lock().unwrap().clone()
    }
}

pub fn service_status() -> ServiceStatus {
    ServiceStatus {
        version: "0.0.0-test".into(),
        state: "running".into(),
        started_at: "2026-10-22T00:00:00Z".into(),
        uptime_secs: 42,
        listen: vec!["udp 127.0.0.1:5060".into()],
        active_calls: 0,
        trunks: vec![TrunkState {
            name: "gw".into(),
            registration: "none".into(),
        }],
    }
}

pub async fn env() -> Env {
    env_opts(web::REQUEST_TIMEOUT, web::api::events::PING_EVERY).await
}

/// [`env`] with another request deadline.
pub async fn env_with(request_timeout: Duration) -> Env {
    env_opts(request_timeout, web::api::events::PING_EVERY).await
}

/// [`env`] with another request deadline and WebSocket ping interval.
pub async fn env_opts(request_timeout: Duration, ws_ping: Duration) -> Env {
    let events = Arc::new(EventBus::new());
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::at_millis(T0));
    let url = format!("sqlite://{}/w.db", dir.path().display());
    let store = store::connect_with(
        &url,
        ConnectOptions {
            max_connections: 4,
            auto_migrate: true,
            clock: clock.clone(),
        },
    )
    .await
    .unwrap();
    let src: Arc<dyn StoreSource> = Arc::new(store.clone());
    let auth = Arc::new(AuthService::new(
        src.clone(),
        clock.clone(),
        AuthConfig::default(),
    ));
    let config = Arc::new(ConfigService::new(
        src.clone(),
        Arc::new(SecretBox::load_or_create(&dir.path().join("secret.key"))),
        RuntimeConfig::default(),
        Vec::new(),
    ));
    config.bootstrap().await;
    let status = Arc::new(FixedStatus(std::sync::Mutex::new(service_status())));
    let data_dir = dir.path().join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let state = AppState {
        auth: auth.clone(),
        config: config.clone(),
        status: status.clone(),
        events: events.clone(),
        ws_ping,
        store: Arc::new(store.clone()),
        data_dir: data_dir.clone(),
        preview_cache_mb: 64,
        preview: Arc::default(),
        clock: clock.clone(),
        https: false,
        request_timeout,
    };
    let srv = WebServer::start("127.0.0.1:0".parse().unwrap(), app(state.clone()), None)
        .await
        .unwrap();
    let base = format!("http://{}", srv.local_addr());
    Env {
        srv,
        state,
        data_dir,
        base,
        store,
        auth,
        clock,
        config,
        status,
        events,
        _dir: dir,
    }
}

pub async fn add_user(e: &Env, name: &str, role: Role, must_change: bool) {
    e.auth
        .create_user(
            &NewAccount {
                username: name,
                role,
                password: PW,
                display_name: None,
                locale: None,
                must_change,
            },
            "test",
        )
        .await
        .unwrap();
}

/// A client that keeps cookies, logged in as `name`; returns it and the CSRF token.
pub async fn login(e: &Env, name: &str) -> (reqwest::Client, String) {
    let c = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .unwrap();
    let r = c
        .post(e.url("/api/v1/auth/login"))
        .json(&serde_json::json!({"username": name, "password": PW}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200, "{}", r.text().await.unwrap());
    let v: serde_json::Value = r.json().await.unwrap();
    (c, v["csrf"].as_str().unwrap().to_string())
}

/// A one-page TIFF (a black bar on white), written with our own T.4 encoder.
pub fn tiff_bytes() -> Vec<u8> {
    let (w, h) = (1728u32, 40u32);
    let mut bm = vec![0u8; (w as usize / 8) * h as usize];
    for row in 10..20 {
        for b in 0..20 {
            bm[row * (w as usize / 8) + b] = 0xFF;
        }
    }
    let g4 = t4::encode_mmr(&bm, w).unwrap();
    t4::write_tiff(
        &[t4::Page {
            width: w,
            height: h,
            x_dpi: 204,
            y_dpi: 196,
            g4: &g4,
            bad_lines: 0,
            longest_bad_run: 0,
        }],
        &t4::TiffMeta {
            software: "web test",
            datetime: "2026:10:22 08:00:00",
            image_description: None,
        },
    )
    .unwrap()
}

/// A finished fax `call` from `ani` that started `minutes` before the test clock, with a TIFF
/// and a PDF on disk. Returns its id.
pub async fn fax(e: &Env, call: &str, ani: &str, minutes: i64) -> i64 {
    use store::{FaxFinal, FaxStatus, HangupDirection, NewFax};
    let start = e.clock.now() - chrono::Duration::minutes(minutes);
    let dir = e.data_dir.join("faxes/2026/10/22");
    std::fs::create_dir_all(&dir).unwrap();
    let tif = tiff_bytes();
    let pdf = format!("%PDF-1.4 fax {call}").into_bytes();
    std::fs::write(dir.join(format!("{call}.tif")), &tif).unwrap();
    std::fs::write(dir.join(format!("{call}.pdf")), &pdf).unwrap();
    let new = NewFax {
        call_id: call.into(),
        trunk: "gw".into(),
        ani: Some(ani.into()),
        ani_source: Some("from".into()),
        dnis: Some("2557".into()),
        dnis_source: Some("ruri".into()),
        started_at: start,
        answered_at: Some(start),
    };
    let fin = FaxFinal {
        ended_at: start + chrono::Duration::seconds(40),
        answered_at: Some(start),
        duration_sec: Some(40),
        hangup_direction: HangupDirection::Remote,
        hangup_code: "completed".into(),
        hangup_text: None,
        sip_status: Some(200),
        t30_code: Some(0),
        fax_status: FaxStatus::Received,
        pages: 1,
        remote_ident: Some("0000000001".into()),
        resolution: Some("fine".into()),
        bit_rate: Some(14400),
        compression: Some("mmr".into()),
        ecm: false,
        tiff_path: Some(format!("faxes/2026/10/22/{call}.tif")),
        tiff_size: Some(tif.len() as i64),
        pdf_path: Some(format!("faxes/2026/10/22/{call}.pdf")),
        pdf_size: Some(pdf.len() as i64),
    };
    e.store.upsert_finished(&new, &fin).await.unwrap().0
}

/// A request that changes state, with the JSON content type and the CSRF header.
pub fn change(
    c: &reqwest::Client,
    m: reqwest::Method,
    url: String,
    csrf: &str,
) -> reqwest::RequestBuilder {
    c.request(m, url).header("X-Sipfax-Csrf", csrf)
}
