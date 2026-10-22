//! W10, W11 and H07 across the whole API: every error has the one JSON shape and tells nothing
//! about the machine, a body that is too large or too slow is refused, and the security headers
//! are on every answer, whatever its status.

mod common;

use std::time::Duration;

use accounts::Role;
use common::*;
use reqwest::Method;
use serde_json::Value;
use tokio::io::{AsyncReadExt, AsyncWriteExt};

/// Words that would tell a client about the server: paths, SQL, drivers, the host.
const LEAKS: &[&str] = &[
    "select ",
    "insert ",
    "update ",
    "sqlite",
    "sqlx",
    "postgres",
    "rusqlite",
    "panicked",
    "stack backtrace",
    "src/",
    "src\\",
    ".rs:",
    "/tmp",
    "\\temp",
    "w.db",
    "secret.key",
    "data_dir",
    "c:\\",
    "/home/",
    "/var/",
    "/srv/",
];

fn assert_shape(label: &str, status: u16, text: &str, data_dir: &str) {
    let v: Value = serde_json::from_str(text)
        .unwrap_or_else(|_| panic!("{label}: {status} is not JSON: {text:?}"));
    let e = &v["error"];
    assert!(
        e["code"].as_str().is_some_and(|c| !c.is_empty()) && e["message"].as_str().is_some(),
        "{label}: {status} has no code and message: {text}"
    );
    let lower = text.to_lowercase();
    for w in LEAKS {
        assert!(!lower.contains(w), "{label}: {status} leaks {w:?}: {text}");
    }
    assert!(
        !text.contains(data_dir),
        "{label}: leaks the data directory"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn w10_every_kind_of_error_has_the_shape_and_leaks_nothing_about_the_server() {
    let e = env_seeded("[[trunks]]\nname = \"alpha\"\npeer = \"192.0.2.10:5060\"\n").await;
    add_user(&e, "root", Role::Admin, false).await;
    add_user(&e, "amy", Role::Agent, false).await;
    let (admin, acsrf) = login(&e, "root").await;
    let (agent, gcsrf) = login(&e, "amy").await;
    let anon = reqwest::Client::new();
    let id = fax(&e, "w10@gw", "02", 5).await;
    let dd = e.data_dir.display().to_string();
    // just over the limit: with much more, the server's 413 and close can come while the client
    // is still writing, and the client then sees a reset instead of the answer
    let big = format!("{{\"x\":\"{}\"}}", "a".repeat(web::BODY_LIMIT + 16));

    // (who, method, path, content type, body, expected status)
    /// (who, method, path, content type, body, expected status)
    type Case<'a> = (&'a str, Method, String, Option<&'a str>, String, u16);
    let cases: Vec<Case> = vec![
        (
            "anon",
            Method::GET,
            "/api/v1/faxes".into(),
            None,
            "".into(),
            401,
        ),
        (
            "anon",
            Method::GET,
            "/api/v1/nothing".into(),
            None,
            "".into(),
            404,
        ),
        // "/" is the web interface; another path outside /api is not a file of it
        (
            "anon",
            Method::GET,
            "/no-such-page".into(),
            None,
            "".into(),
            404,
        ),
        (
            "anon",
            Method::GET,
            "/api/v1/auth/login".into(),
            None,
            "".into(),
            405,
        ),
        (
            "anon",
            Method::DELETE,
            "/api/v1/faxes".into(),
            None,
            "".into(),
            405,
        ),
        (
            "anon",
            Method::POST,
            "/api/v1/auth/login".into(),
            Some("application/json"),
            "{bad".into(),
            400,
        ),
        (
            "anon",
            Method::POST,
            "/api/v1/auth/login".into(),
            Some("application/json"),
            "[]".into(),
            400,
        ),
        (
            "anon",
            Method::POST,
            "/api/v1/auth/login".into(),
            Some("application/json"),
            "{\"username\":1}".into(),
            400,
        ),
        (
            "anon",
            Method::POST,
            "/api/v1/auth/login".into(),
            Some("application/json"),
            "{\"username\":\"a\",\"password\":\"b\",\"extra\":1}".into(),
            400,
        ),
        (
            "anon",
            Method::POST,
            "/api/v1/auth/login".into(),
            Some("application/json"),
            big.clone(),
            413,
        ),
        (
            "anon",
            Method::POST,
            "/api/v1/auth/login".into(),
            Some("text/plain"),
            "x".into(),
            403,
        ),
        (
            "anon",
            Method::POST,
            "/api/v1/auth/login".into(),
            None,
            "".into(),
            403,
        ),
        (
            "agent",
            Method::GET,
            "/api/v1/faxes/999999".into(),
            None,
            "".into(),
            404,
        ),
        (
            "agent",
            Method::GET,
            "/api/v1/faxes/abc".into(),
            None,
            "".into(),
            404,
        ),
        (
            "agent",
            Method::GET,
            format!("/api/v1/faxes/{id}/pages/9.png"),
            None,
            "".into(),
            404,
        ),
        (
            "agent",
            Method::GET,
            format!("/api/v1/faxes/{id}/pages/1.png?kind=huge"),
            None,
            "".into(),
            400,
        ),
        (
            "agent",
            Method::GET,
            format!("/api/v1/faxes/{id}/nothing.bin"),
            None,
            "".into(),
            404,
        ),
        (
            "agent",
            Method::GET,
            "/api/v1/faxes?limit=0".into(),
            None,
            "".into(),
            400,
        ),
        (
            "agent",
            Method::GET,
            "/api/v1/faxes?from=yesterday".into(),
            None,
            "".into(),
            400,
        ),
        (
            "agent",
            Method::GET,
            "/api/v1/faxes/%FF/file.pdf".into(),
            None,
            "".into(),
            400,
        ),
        (
            "agent",
            Method::GET,
            "/api/v1/faxes/%FF".into(),
            None,
            "".into(),
            400,
        ),
        (
            "agent",
            Method::GET,
            "/api/v1/config".into(),
            None,
            "".into(),
            403,
        ),
        (
            "agent",
            Method::POST,
            format!("/api/v1/faxes/{id}/redeliver"),
            Some("application/json"),
            "{}".into(),
            403,
        ),
        (
            "agent",
            Method::PUT,
            format!("/api/v1/faxes/{id}/read"),
            Some("application/json"),
            "{\"read\":1}".into(),
            400,
        ),
        (
            "agent",
            Method::PUT,
            format!("/api/v1/faxes/{id}/note"),
            Some("application/json"),
            "{\"expected_updated_at\":null}".into(),
            400,
        ),
        (
            "agent",
            Method::PUT,
            format!("/api/v1/faxes/{id}/note"),
            Some("application/json"),
            format!(
                "{{\"text\":\"{}\",\"expected_updated_at\":null}}",
                "n".repeat(70_000)
            ),
            422,
        ),
        (
            "agent",
            Method::PUT,
            format!("/api/v1/faxes/{id}/note"),
            Some("application/json"),
            big.clone(),
            413,
        ),
        (
            "admin",
            Method::GET,
            "/api/v1/config/nonsense".into(),
            None,
            "".into(),
            404,
        ),
        (
            "admin",
            Method::GET,
            "/api/v1/config/audit?before=x".into(),
            None,
            "".into(),
            400,
        ),
        (
            "admin",
            Method::PUT,
            "/api/v1/config/call".into(),
            Some("application/json"),
            "{\"version\":1,\"body\":{\"max_duration_secs\":\"x\"}}".into(),
            422,
        ),
        (
            "admin",
            Method::PUT,
            "/api/v1/config/call".into(),
            Some("application/json"),
            "{\"version\":99,\"body\":{}}".into(),
            409,
        ),
        (
            "admin",
            Method::PUT,
            "/api/v1/config/trunk:alpha".into(),
            Some("application/json"),
            "{\"version\":1,\"body\":{\"peer\":\"no port\"}}".into(),
            422,
        ),
        (
            "admin",
            Method::PUT,
            "/api/v1/config/call".into(),
            Some("application/json"),
            big.clone(),
            413,
        ),
        (
            "admin",
            Method::DELETE,
            "/api/v1/config/trunk:alpha?version=99".into(),
            Some("application/json"),
            "".into(),
            409,
        ),
        (
            "admin",
            Method::DELETE,
            "/api/v1/config/trunk:alpha".into(),
            Some("application/json"),
            "".into(),
            400,
        ),
        (
            "admin",
            Method::PUT,
            "/api/v1/users/99999".into(),
            Some("application/json"),
            "{\"disabled\":true}".into(),
            404,
        ),
        (
            "admin",
            Method::PUT,
            "/api/v1/users/x".into(),
            Some("application/json"),
            "{\"disabled\":true}".into(),
            404,
        ),
        (
            "admin",
            Method::POST,
            "/api/v1/users".into(),
            Some("application/json"),
            "{\"username\":\"ZZ\",\"role\":\"agent\",\"password\":\"long enough pw\"}".into(),
            422,
        ),
        (
            "admin",
            Method::POST,
            "/api/v1/users".into(),
            Some("application/json"),
            "{\"username\":\"zz\",\"role\":\"boss\",\"password\":\"long enough pw\"}".into(),
            422,
        ),
        (
            "admin",
            Method::POST,
            "/api/v1/users".into(),
            Some("application/json"),
            "{\"username\":\"amy\",\"role\":\"agent\",\"password\":\"long enough pw\"}".into(),
            409,
        ),
        (
            "admin",
            Method::PUT,
            "/api/v1/users/1".into(),
            Some("application/json"),
            "{\"must_change_password\":true}".into(),
            400,
        ),
    ];
    for (who, m, path, ctype, body, want) in cases {
        let (c, csrf) = match who {
            "anon" => (&anon, ""),
            "agent" => (&agent, gcsrf.as_str()),
            _ => (&admin, acsrf.as_str()),
        };
        let mut rq = c.request(m.clone(), e.url(&path));
        if who != "anon" && m != Method::GET {
            rq = rq.header("X-Sipfax-Csrf", csrf);
        }
        if let Some(ct) = ctype {
            rq = rq.header("content-type", ct);
        }
        if !body.is_empty() {
            rq = rq.body(body);
        }
        let r = rq.send().await.unwrap();
        let status = r.status().as_u16();
        let ct = r
            .headers()
            .get("content-type")
            .map(|v| v.to_str().unwrap().to_string());
        let text = r.text().await.unwrap();
        let label = format!("{who} {m} {}", path.chars().take(60).collect::<String>());
        assert_eq!(status, want, "{label}: {text}");
        assert!(
            ct.as_deref()
                .is_some_and(|c| c.starts_with("application/json")),
            "{label}: content type {ct:?}"
        );
        assert_shape(&label, status, &text, &dd);
    }
    e.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn w11_a_body_over_1_mib_is_413_and_changes_nothing() {
    let e = env().await;
    add_user(&e, "amy", Role::Agent, false).await;
    add_user(&e, "root", Role::Admin, false).await;
    let (c, csrf) = login(&e, "amy").await;
    let (admin, acsrf) = login(&e, "root").await;
    let id = fax(&e, "big@gw", "02", 5).await;
    let before = format!("{:?}", e.store.get(id).await.unwrap());
    let users = e.auth.list_users().await.unwrap().len();
    let audit = e.config.audit(100, None).await.unwrap().len();
    // exactly at the limit minus the envelope is fine, a little over is not
    let note = |n: usize| {
        format!(
            "{{\"text\":\"{}\",\"expected_updated_at\":null}}",
            "n".repeat(n)
        )
    };
    let r = change(
        &c,
        Method::PUT,
        e.url(&format!("/api/v1/faxes/{id}/note")),
        &csrf,
    )
    .header("content-type", "application/json")
    .body(note(web::BODY_LIMIT + 10))
    .send()
    .await
    .unwrap();
    let s = r.status().as_u16();
    let v: Value = r.json().await.unwrap();
    assert_eq!((s, v["error"]["code"].as_str()), (413, Some("too_large")));
    assert_eq!(format!("{:?}", e.store.get(id).await.unwrap()), before);
    let r = change(&admin, Method::POST, e.url("/api/v1/users"), &acsrf)
        .header("content-type", "application/json")
        .body(format!(
            "{{\"username\":\"zz\",\"role\":\"agent\",\"password\":\"{}\"}}",
            "p".repeat(web::BODY_LIMIT)
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 413);
    assert_eq!(e.auth.list_users().await.unwrap().len(), users);
    let r = change(&admin, Method::PUT, e.url("/api/v1/config/call"), &acsrf)
        .header("content-type", "application/json")
        .body(format!(
            "{{\"version\":1,\"body\":{{\"x\":\"{}\"}}}}",
            "c".repeat(web::BODY_LIMIT)
        ))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 413);
    assert_eq!(e.config.audit(100, None).await.unwrap().len(), audit);
    // a body that is only a little smaller than the limit still gets through
    let r = change(
        &c,
        Method::PUT,
        e.url(&format!("/api/v1/faxes/{id}/note")),
        &csrf,
    )
    .header("content-type", "application/json")
    .body(note(100))
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    e.stop().await;
}

/// The whole answer as text (the server closes the connection when it is done).
async fn raw_exchange(addr: &str, request: &[u8], limit: Duration) -> String {
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    s.write_all(request).await.unwrap();
    let mut buf = Vec::new();
    let _ = tokio::time::timeout(limit, async {
        let mut chunk = [0u8; 2048];
        loop {
            match s.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => buf.extend_from_slice(&chunk[..n]),
            }
        }
    })
    .await;
    String::from_utf8_lossy(&buf).into_owned()
}

#[tokio::test(flavor = "multi_thread")]
async fn w11_the_408_of_a_slow_body_carries_the_error_shape_and_the_security_headers() {
    let e = env_with(Duration::from_millis(300)).await;
    let addr = e.base.trim_start_matches("http://").to_string();
    let t = std::time::Instant::now();
    let text = raw_exchange(
        &addr,
        b"POST /api/v1/auth/login HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\
          Content-Type: application/json\r\nContent-Length: 100\r\n\r\n{\"user",
        Duration::from_secs(5),
    )
    .await;
    assert!(
        t.elapsed() < Duration::from_secs(4),
        "answered by the deadline"
    );
    assert!(text.starts_with("HTTP/1.1 408"), "{text}");
    let lower = text.to_lowercase();
    assert!(lower.contains("content-security-policy:"), "{text}");
    assert!(lower.contains("x-content-type-options: nosniff"), "{text}");
    let body = text.split("\r\n\r\n").nth(1).unwrap_or("");
    let v: Value = serde_json::from_str(body.trim()).unwrap_or_else(|_| panic!("{text}"));
    assert_eq!(v["error"]["code"], "timeout");
    // a request that finishes in time is not touched by the deadline
    let r = reqwest::get(e.url("/api/v1/nothing")).await.unwrap();
    assert_eq!(r.status(), 404);
    e.stop().await;
}

/// Virtual time: a client that never finishes its headers is dropped after `HEADER_TIMEOUT`
/// (30 s) without the test waiting for it.
#[tokio::test(start_paused = true)]
async fn w11_a_client_that_does_not_finish_its_headers_is_dropped_after_30_seconds() {
    let app = axum::Router::new().route("/ok", axum::routing::get(|| async { "ok" }));
    let srv = web::server::WebServer::start("127.0.0.1:0".parse().unwrap(), app, None)
        .await
        .unwrap();
    let addr = srv.local_addr();
    let mut s = tokio::net::TcpStream::connect(addr).await.unwrap();
    // half a request line, then silence
    s.write_all(b"GET /ok HTTP/1.1\r\nHost: x\r\nX-Slow: ")
        .await
        .unwrap();
    let mut buf = Vec::new();
    let started = tokio::time::Instant::now();
    // the connection ends by itself: hyper answers 408 or just closes
    let read = tokio::time::timeout(Duration::from_secs(120), s.read_to_end(&mut buf)).await;
    assert!(read.is_ok(), "the server closed the connection");
    let waited = started.elapsed();
    assert!(
        waited >= web::server::HEADER_TIMEOUT && waited < web::server::HEADER_TIMEOUT * 2,
        "dropped after {waited:?}"
    );
    // a well-behaved request on a new connection is still served
    let mut ok = tokio::net::TcpStream::connect(addr).await.unwrap();
    ok.write_all(b"GET /ok HTTP/1.1\r\nHost: x\r\nConnection: close\r\n\r\n")
        .await
        .unwrap();
    let mut out = Vec::new();
    ok.read_to_end(&mut out).await.unwrap();
    assert!(String::from_utf8_lossy(&out).starts_with("HTTP/1.1 200"));
    srv.shutdown(Duration::from_secs(1)).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn h07_the_security_headers_are_on_every_answer_whatever_its_status() {
    let e = env().await;
    add_user(&e, "amy", Role::Agent, false).await;
    let (c, csrf) = login(&e, "amy").await;
    let id = fax(&e, "h7@gw", "02", 5).await;
    let anon = reqwest::Client::new();
    let checks: Vec<(&str, reqwest::RequestBuilder, u16)> = vec![
        ("200 json", c.get(e.url("/api/v1/faxes")), 200),
        (
            "200 png",
            c.get(e.url(&format!("/api/v1/faxes/{id}/pages/1.png"))),
            200,
        ),
        (
            "200 download",
            c.get(e.url(&format!("/api/v1/faxes/{id}/file.pdf"))),
            200,
        ),
        ("401", anon.get(e.url("/api/v1/faxes")), 401),
        ("404", c.get(e.url("/api/v1/faxes/99999")), 404),
        ("404 fallback", anon.get(e.url("/anything")), 404),
        ("405", anon.get(e.url("/api/v1/auth/login")), 405),
        (
            "403",
            c.put(e.url(&format!("/api/v1/faxes/{id}/read")))
                .header("content-type", "application/json")
                .body("{\"read\":true}"),
            403,
        ),
        (
            "400",
            c.put(e.url(&format!("/api/v1/faxes/{id}/read")))
                .header("X-Sipfax-Csrf", &csrf)
                .header("content-type", "application/json")
                .body("nope"),
            400,
        ),
        (
            "413",
            anon.post(e.url("/api/v1/auth/login"))
                .header("content-type", "application/json")
                .body("x".repeat(web::BODY_LIMIT + 1)),
            413,
        ),
        ("websocket refusal", anon.get(e.url("/api/v1/events")), 401),
    ];
    for (what, rq, want) in checks {
        let r = rq.send().await.unwrap();
        assert_eq!(r.status().as_u16(), want, "{what}");
        let h = r.headers();
        assert_eq!(h["content-security-policy"], web::server::CSP, "{what}");
        assert_eq!(h["x-content-type-options"], "nosniff", "{what}");
        assert_eq!(h["referrer-policy"], "same-origin", "{what}");
        assert!(
            h.get("strict-transport-security").is_none(),
            "{what}: HSTS over plain HTTP"
        );
    }
    // the CSP is exactly the one of spec 5.3
    assert_eq!(
        web::server::CSP,
        "default-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'"
    );
    assert_eq!(web::server::HSTS, "max-age=31536000");
    // the JSON of the API is not sniffed into something else
    let r = anon.get(e.url("/api/v1/faxes")).send().await.unwrap();
    assert!(
        r.headers()["content-type"]
            .to_str()
            .unwrap()
            .starts_with("application/json")
    );
    e.stop().await;
}
