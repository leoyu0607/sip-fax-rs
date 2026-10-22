//! H05–H07 against a real listener: plain HTTP and HTTPS with the self-signed certificate, the
//! security headers and the error shape.

use std::time::Duration;

mod common;

use chrono::NaiveDate;
use web::config::WebConfig;
use web::server::{CSP, HSTS, WebServer};
use web::tls;

fn state_of(e: &common::Env) -> web::AppState {
    e.state.clone()
}

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 22).unwrap()
}

#[tokio::test]
async fn h07_plain_http_has_the_security_headers_and_no_hsts() {
    let e = common::env().await;
    let url = e.url("/api/v1/nope");
    let r = reqwest::get(&url).await.unwrap();
    assert_eq!(r.status(), 404);
    let h = r.headers().clone();
    assert_eq!(h["content-security-policy"], CSP);
    assert_eq!(h["x-content-type-options"], "nosniff");
    assert_eq!(h["referrer-policy"], "same-origin");
    assert!(
        h.get("strict-transport-security").is_none(),
        "no HSTS over HTTP"
    );
    // W10: the error shape
    let body: serde_json::Value = r.json().await.unwrap();
    assert_eq!(body["error"]["code"], "not_found");
    assert_eq!(
        body["error"]["message"], "resource not found",
        "the message is the fixed English text"
    );
    e.stop().await;
}

#[tokio::test]
async fn h01_h07_https_with_the_self_signed_certificate_verifies_for_localhost_and_sends_hsts() {
    let dir = tempfile::tempdir().unwrap();
    let cfg = WebConfig {
        enabled: true,
        ..WebConfig::default()
    };
    let names = tls::subject_names(Some("faxsrv"), "127.0.0.1".parse().unwrap(), &[], &[]);
    let m = tls::load(&cfg, dir.path(), dir.path(), &names, today()).unwrap();
    let e = common::env().await;
    let mut state = state_of(&e);
    state.https = true;
    let srv = WebServer::start(
        "127.0.0.1:0".parse().unwrap(),
        web::app(state),
        Some(m.config.clone()),
    )
    .await
    .unwrap();
    let pem = std::fs::read(&m.cert_path).unwrap();
    let client = reqwest::Client::builder()
        .add_root_certificate(reqwest::Certificate::from_pem(&pem).unwrap())
        .build()
        .unwrap();
    // the certificate names localhost: the client verifies it fully
    let url = format!("https://localhost:{}/x", srv.local_addr().port());
    let r = client.get(&url).send().await.unwrap();
    assert_eq!(r.status(), 404);
    assert_eq!(r.headers()["strict-transport-security"], HSTS);
    assert_eq!(r.headers()["content-security-policy"], CSP);
    // reverse: a client that does not trust the certificate is refused
    let plain = reqwest::Client::new();
    assert!(
        plain.get(&url).send().await.is_err(),
        "an unknown certificate must not verify"
    );
    // and plain HTTP to the HTTPS port gets no answer
    let http = format!("http://127.0.0.1:{}/x", srv.local_addr().port());
    assert!(
        reqwest::get(&http).await.is_err(),
        "plain HTTP to the HTTPS port gets no answer"
    );
    srv.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn h00_shutdown_stops_accepting() {
    let e = common::env().await;
    let addr = e.srv.local_addr();
    e.stop().await;
    assert!(
        tokio::net::TcpStream::connect(addr).await.is_err(),
        "the port is closed after the shutdown"
    );
}

/// An HTTPS server (self-signed certificate, `https = true`) over the state of `e`; returns it
/// with a client that trusts that certificate and the base URL (`localhost`, so that the name is
/// verified).
async fn https_server(e: &common::Env) -> (WebServer, reqwest::Client, String, Vec<u8>) {
    let dir = tempfile::tempdir().unwrap();
    let cfg = WebConfig {
        enabled: true,
        ..WebConfig::default()
    };
    let names = tls::subject_names(Some("faxsrv"), "127.0.0.1".parse().unwrap(), &[], &[]);
    let m = tls::load(&cfg, dir.path(), dir.path(), &names, today()).unwrap();
    let mut state = state_of(e);
    state.https = true;
    let srv = WebServer::start(
        "127.0.0.1:0".parse().unwrap(),
        web::app(state),
        Some(m.config.clone()),
    )
    .await
    .unwrap();
    let pem = std::fs::read(&m.cert_path).unwrap();
    let client = reqwest::Client::builder()
        .add_root_certificate(reqwest::Certificate::from_pem(&pem).unwrap())
        .cookie_store(true)
        .build()
        .unwrap();
    let base = format!("https://localhost:{}", srv.local_addr().port());
    (srv, client, base, pem)
}

#[tokio::test(flavor = "multi_thread")]
async fn a09_over_real_https_the_cookie_is_secure_httponly_samesite_strict_and_works() {
    let e = common::env().await;
    common::add_user(&e, "alice", accounts::Role::Agent, false).await;
    let (srv, c, base, pem) = https_server(&e).await;
    let r = c
        .post(format!("{base}/api/v1/auth/login"))
        .json(&serde_json::json!({"username": "alice", "password": common::PW}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let cookie = r.headers()["set-cookie"].to_str().unwrap().to_string();
    for want in ["HttpOnly", "SameSite=Strict", "Secure", "Path=/"] {
        assert!(cookie.contains(want), "{want} missing: {cookie}");
    }
    let v: serde_json::Value = r.json().await.unwrap();
    let csrf = v["csrf"].as_str().unwrap().to_string();
    // the cookie jar of a client sends a Secure cookie over HTTPS: the session works
    let me = c
        .get(format!("{base}/api/v1/auth/me"))
        .send()
        .await
        .unwrap();
    assert_eq!(me.status(), 200);
    assert_eq!(me.headers()["strict-transport-security"], HSTS);
    // reverse: a client without the cookie is a stranger
    let stranger = reqwest::Client::builder()
        .add_root_certificate(reqwest::Certificate::from_pem(&pem).unwrap())
        .build()
        .unwrap();
    assert_eq!(
        stranger
            .get(format!("{base}/api/v1/auth/me"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    // the logout clears it with the same attributes, so that the browser really drops it
    let out = c
        .post(format!("{base}/api/v1/auth/logout"))
        .header("X-Sipfax-Csrf", csrf)
        .json(&serde_json::json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(out.status(), 204);
    let cleared = out.headers()["set-cookie"].to_str().unwrap().to_string();
    assert!(
        cleared.contains("Max-Age=0") && cleared.contains("Secure") && cleared.contains("HttpOnly"),
        "{cleared}"
    );
    srv.shutdown(Duration::from_secs(1)).await;
    e.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn h07_https_sends_hsts_with_every_status_and_the_other_headers_too() {
    let e = common::env().await;
    let (srv, c, base, _pem) = https_server(&e).await;
    for (what, path, status) in [
        ("401", "/api/v1/faxes", 401),
        ("404", "/api/v1/none", 404),
        ("405", "/api/v1/auth/login", 405),
        ("fallback", "/x/y", 404),
    ] {
        let r = c.get(format!("{base}{path}")).send().await.unwrap();
        assert_eq!(r.status().as_u16(), status, "{what}");
        let h = r.headers();
        assert_eq!(h["strict-transport-security"], HSTS, "{what}");
        assert_eq!(h["content-security-policy"], CSP, "{what}");
        assert_eq!(h["x-content-type-options"], "nosniff", "{what}");
        assert_eq!(h["referrer-policy"], "same-origin", "{what}");
    }
    srv.shutdown(Duration::from_secs(1)).await;
    e.stop().await;
}
