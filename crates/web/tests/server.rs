//! H05–H07 against a real listener: plain HTTP and HTTPS with the self-signed certificate, the
//! security headers and the error shape.

use std::time::Duration;

use chrono::NaiveDate;
use web::config::WebConfig;
use web::server::{CSP, HSTS, WebServer};
use web::tls;

fn today() -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, 10, 22).unwrap()
}

#[tokio::test]
async fn h07_plain_http_has_the_security_headers_and_no_hsts() {
    let srv = WebServer::start("127.0.0.1:0".parse().unwrap(), web::app(false), None)
        .await
        .unwrap();
    let url = format!("http://{}/api/v1/nope", srv.local_addr());
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
    assert!(body["error"]["message"].is_string());
    srv.shutdown(Duration::from_secs(1)).await;
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
    let srv = WebServer::start(
        "127.0.0.1:0".parse().unwrap(),
        web::app(true),
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
        !reqwest::get(&http)
            .await
            .is_ok_and(|r| r.status().is_success()),
        "plain HTTP to the HTTPS port gets no answer"
    );
    srv.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test]
async fn h00_shutdown_stops_accepting() {
    let srv = WebServer::start("127.0.0.1:0".parse().unwrap(), web::app(false), None)
        .await
        .unwrap();
    let addr = srv.local_addr();
    srv.shutdown(Duration::from_secs(1)).await;
    assert!(
        tokio::net::TcpStream::connect(addr).await.is_err(),
        "the port is closed after the shutdown"
    );
}
