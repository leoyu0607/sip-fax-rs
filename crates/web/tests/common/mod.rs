//! A real server on 127.0.0.1 with a real SQLite store (no mocks, test plan section 0).
#![allow(dead_code)]

use std::sync::Arc;
use std::time::Duration;

use accounts::{AuthConfig, AuthService, NewAccount, Role};
use store::{ConnectOptions, FaxStore, ManualClock, StoreSource};
use web::server::WebServer;
use web::{AppState, app};

pub const T0: i64 = 1_792_540_800_000;
pub const PW: &str = "correct horse battery";

pub struct Env {
    pub srv: WebServer,
    pub base: String,
    pub store: Arc<dyn FaxStore>,
    pub auth: Arc<AuthService>,
    pub clock: Arc<ManualClock>,
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

pub async fn env() -> Env {
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
    let auth = Arc::new(AuthService::new(src, clock.clone(), AuthConfig::default()));
    let state = AppState {
        auth: auth.clone(),
        clock: clock.clone(),
        https: false,
    };
    let srv = WebServer::start("127.0.0.1:0".parse().unwrap(), app(state), None)
        .await
        .unwrap();
    let base = format!("http://{}", srv.local_addr());
    Env {
        srv,
        base,
        store,
        auth,
        clock,
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
