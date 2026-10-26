//! M6a (ADR-070) API keys over real HTTP: K01 (web), K02-K09. A real server, a real SQLite file,
//! an injected clock; nothing is mocked. The cases that must hold on every database (K01, K03,
//! K04, K09 at the level of the account service and the store) are in
//! `crates/accounts/tests/m6a_keys.rs` and `crates/store/src/tests/m6a_tests.rs`.

mod common;

use std::sync::{Arc, Mutex, OnceLock};
use std::time::Duration;

use accounts::Role;
use chrono::Duration as Dur;
use common::*;
use futures_util::StreamExt;
use reqwest::Method;
use serde_json::{Value, json};
use store::{Clock, DeliveryStatus, NewDelivery};
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};

// ---------------------------------------------------------------- what the server logs
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for Capture {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Capture {
        self.clone()
    }
}

/// Every test of this file logs into one buffer (the server runs on other threads).
fn logs() -> &'static Capture {
    static LOGS: OnceLock<Capture> = OnceLock::new();
    LOGS.get_or_init(|| {
        let c = Capture::default();
        let sub = tracing_subscriber::fmt()
            .with_writer(c.clone())
            .with_ansi(false)
            .with_max_level(tracing::Level::TRACE)
            .finish();
        let _ = tracing::subscriber::set_global_default(sub);
        c
    })
}

fn assert_not_logged(key: &str) {
    let text = String::from_utf8_lossy(&logs().0.lock().unwrap()).into_owned();
    assert!(!text.contains(&key[12..]), "the key is in the log");
}

// ---------------------------------------------------------------- helpers
struct Admin {
    c: reqwest::Client,
    csrf: String,
}

async fn admin(e: &Env) -> Admin {
    let _ = logs();
    add_user(e, "boss", Role::Admin, false).await;
    let (c, csrf) = login(e, "boss").await;
    Admin { c, csrf }
}

/// A key made through the web API: `(key, the api_key object)`.
async fn make_key(
    e: &Env,
    a: &Admin,
    name: &str,
    write: bool,
    expires: Option<&str>,
) -> (String, Value) {
    let mut body = json!({"name": name, "write": write});
    if let Some(x) = expires {
        body["expires_at"] = json!(x);
    }
    let r = change(&a.c, Method::POST, e.url("/api/v1/api-keys"), &a.csrf)
        .json(&body)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201, "{name}");
    let v: Value = r.json().await.unwrap();
    (v["key"].as_str().unwrap().to_string(), v["api_key"].clone())
}

fn get(e: &Env, key: &str, path: &str) -> reqwest::RequestBuilder {
    reqwest::Client::new()
        .get(e.url(path))
        .header("Authorization", format!("Bearer {key}"))
}

fn put(e: &Env, key: &str, path: &str, body: &Value) -> reqwest::RequestBuilder {
    reqwest::Client::new()
        .put(e.url(path))
        .header("Authorization", format!("Bearer {key}"))
        .json(body)
}

async fn json_of(r: reqwest::Response) -> Value {
    r.json().await.unwrap()
}

fn sha256_hex(s: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, s.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// What the file of the SQLite database (and its write-ahead log) contains.
fn db_bytes(e: &Env) -> Vec<u8> {
    let mut all = Vec::new();
    for f in std::fs::read_dir(e._dir.path()).unwrap().flatten() {
        let n = f.file_name().to_string_lossy().into_owned();
        if n.starts_with("w.db") {
            all.extend(std::fs::read(f.path()).unwrap());
        }
    }
    all
}

fn contains(hay: &[u8], needle: &str) -> bool {
    hay.windows(needle.len()).any(|w| w == needle.as_bytes())
}

// ---------------------------------------------------------------- K01
#[tokio::test(flavor = "multi_thread")]
async fn k01_web_create_shows_the_key_once_and_stores_only_the_hash() {
    let e = env().await;
    let a = admin(&e).await;
    let exp = (e.clock.now() + Dur::days(90) + Dur::milliseconds(250)).to_rfc3339();
    let (ro, ro_info) = make_key(&e, &a, "reader", false, None).await;
    let (rw, rw_info) = make_key(&e, &a, "Writer", true, Some(&exp)).await;
    for k in [&ro, &rw] {
        assert!(k.starts_with("sfx_") && k.len() == 47, "{k}");
        assert!(
            k[4..]
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
        );
    }
    assert_ne!(ro, rw);
    assert_eq!(ro_info["scopes"], json!(["fax:read"]));
    assert_eq!(rw_info["scopes"], json!(["fax:read", "fax:write"]));
    assert_eq!(rw_info["name"], "writer", "lower case");
    assert_eq!(ro_info["expires_at"], Value::Null);
    assert!(
        rw_info["expires_at"].as_str().unwrap().contains(".250"),
        "{rw_info}"
    );
    assert_eq!(ro_info["created_by"], "boss");
    assert_eq!(ro_info["prefix"], ro[..12]);
    assert_eq!(ro_info["active"], true);
    assert!(ro_info.get("key").is_none() && ro_info.get("token_hash").is_none());

    // the list: prefix, rights, who, when; no key, no hash; the key is in no other answer
    let r = a.c.get(e.url("/api/v1/api-keys")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let text = r.text().await.unwrap();
    for k in [&ro, &rw] {
        assert!(!text.contains(&k[12..]), "the list shows no key");
    }
    let list: Value = serde_json::from_str(&text).unwrap();
    let keys = list["keys"].as_array().unwrap();
    assert_eq!(keys.len(), 2);
    for k in keys {
        for f in [
            "id",
            "name",
            "prefix",
            "scopes",
            "created_by",
            "created_at",
            "expires_at",
            "revoked_at",
            "last_used_at",
            "active",
        ] {
            assert!(k.get(f).is_some(), "{f} in {k}");
        }
        assert!(k.get("token_hash").is_none() && k.get("key").is_none());
    }
    // the database: the hash and the prefix only
    let rows = e.store.api_key_list().await.unwrap();
    assert_eq!(rows.len(), 2);
    for (k, name) in [(&ro, "reader"), (&rw, "writer")] {
        let row = rows.iter().find(|r| r.name == name).unwrap();
        assert_eq!(row.token_hash, sha256_hex(k));
        assert_eq!(row.prefix, k[..12]);
    }
    let bytes = db_bytes(&e);
    for k in [&ro, &rw] {
        assert!(!contains(&bytes, k), "the database file has the key");
        assert!(!contains(&bytes, &k[12..]), "... or its tail");
    }
    // the audit trail and the logs: no key (and the log is really captured: it says what happened)
    let text = String::from_utf8_lossy(&logs().0.lock().unwrap()).into_owned();
    assert!(
        text.contains("API key created") && text.contains(&ro[..12]),
        "{text}"
    );
    let r = a.c.get(e.url("/api/v1/config/audit")).send().await.unwrap();
    let audit = r.text().await.unwrap();
    for k in [&ro, &rw] {
        assert!(!audit.contains(&k[12..]));
        assert_not_logged(k);
    }
    // and the export of the configuration
    let r = a.c.get(e.url("/api/v1/config")).send().await.unwrap();
    let cfg = r.text().await.unwrap();
    for k in [&ro, &rw] {
        assert!(!cfg.contains(&k[12..]), "config");
    }
    e.stop().await;
}

// ---------------------------------------------------------------- K02
#[tokio::test(flavor = "multi_thread")]
async fn k02_a_read_only_key_reads_what_a_session_reads() {
    let e = env().await;
    let a = admin(&e).await;
    let f1 = fax(&e, "call-a", "0911", 30).await;
    let f2 = fax(&e, "call-b", "0922", 20).await;
    let f3 = fax(&e, "call-c", "0933", 10).await;
    // one with a note and the read flag, one with a delivery attempt (only a session may see it)
    e.store.set_read(f1, true, "boss").await.unwrap();
    e.store
        .set_note(f1, "a note 備註", "boss", None)
        .await
        .unwrap();
    e.store
        .add_delivery(&NewDelivery {
            fax_id: f2,
            target: "mail:ops@example.com".into(),
            kind: "email".into(),
            attempt: 1,
            status: DeliveryStatus::Failed,
            next_try_at: None,
        })
        .await
        .unwrap();
    e.clock.advance(Dur::seconds(5));
    let (key, _) = make_key(&e, &a, "reader", false, None).await;

    // both ways of calling, same path
    let same = |path: String| {
        let e = &e;
        let a = &a;
        let key = key.clone();
        async move {
            let s = a.c.get(e.url(&path)).send().await.unwrap();
            let k = get(e, &key, &path).send().await.unwrap();
            assert_eq!(
                (s.status().as_u16(), k.status().as_u16()),
                (200, 200),
                "{path}"
            );
            (s, k)
        }
    };
    // JSON answers: every field the same (the session's extras are the deliveries)
    for path in [
        "/api/v1/faxes".to_string(),
        "/api/v1/faxes?limit=2".to_string(),
        "/api/v1/faxes?is_read=false".to_string(),
        "/api/v1/faxes?q=note".to_string(),
        "/api/v1/faxes/changes".to_string(),
        "/api/v1/faxes/changes?limit=2".to_string(),
        "/api/v1/faxes/counts".to_string(),
        format!("/api/v1/faxes/counts?since={}", "2026-01-01T00:00:00Z"),
        format!("/api/v1/faxes/{f1}"),
        format!("/api/v1/faxes/{f2}"),
        format!("/api/v1/faxes/{f3}"),
    ] {
        let (s, k) = same(path.clone()).await;
        let mut sv = json_of(s).await;
        let kv = json_of(k).await;
        // the parts a key does not get
        if let Some(o) = sv.as_object_mut() {
            o.remove("deliveries");
        }
        assert_eq!(sv, kv, "{path}");
        assert!(
            kv.get("deliveries").is_none(),
            "{path}: no deliveries for a key: {kv}"
        );
    }
    // the session sees the delivery of f2, the key does not
    let sv = json_of(
        a.c.get(e.url(&format!("/api/v1/faxes/{f2}")))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(sv["deliveries"].as_array().unwrap().len(), 1, "{sv}");
    let kv = json_of(
        get(&e, &key, &format!("/api/v1/faxes/{f2}"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert!(kv.get("deliveries").is_none());
    // counts: the delivery numbers are for a session too
    let sc = json_of(a.c.get(e.url("/api/v1/faxes/counts")).send().await.unwrap()).await;
    let kc = json_of(get(&e, &key, "/api/v1/faxes/counts").send().await.unwrap()).await;
    assert!(sc.get("deliveries").is_some());
    assert!(kc.get("deliveries").is_none(), "{kc}");
    assert_eq!(kc["unread"], 2);
    // the changes carry everything the list does
    let ch = json_of(get(&e, &key, "/api/v1/faxes/changes").send().await.unwrap()).await;
    assert_eq!(ch["items"].as_array().unwrap().len(), 3);

    // the files: the same bytes and headers
    for path in [
        format!("/api/v1/faxes/{f1}/file.tif"),
        format!("/api/v1/faxes/{f1}/file.pdf"),
        format!("/api/v1/faxes/{f2}/pages/1.png?kind=full"),
        format!("/api/v1/faxes/{f2}/pages/1.png?kind=thumb"),
        format!("/api/v1/faxes/{f2}/pages/1.png"),
    ] {
        let (s, k) = same(path.clone()).await;
        let (sh, kh) = (s.headers().clone(), k.headers().clone());
        for h in [
            "content-type",
            "content-length",
            "etag",
            "content-disposition",
            "cache-control",
        ] {
            assert_eq!(sh.get(h), kh.get(h), "{path}: header {h}");
        }
        let (sb, kb) = (s.bytes().await.unwrap(), k.bytes().await.unwrap());
        assert!(!sb.is_empty());
        assert_eq!(sb, kb, "{path}");
    }
    let pdf = get(&e, &key, &format!("/api/v1/faxes/{f1}/file.pdf"))
        .send()
        .await
        .unwrap();
    assert_eq!(&pdf.bytes().await.unwrap()[..], b"%PDF-1.4 fax call-a");
    // the thumbnail is smaller than the full page
    let full = get(
        &e,
        &key,
        &format!("/api/v1/faxes/{f2}/pages/1.png?kind=full"),
    )
    .send()
    .await
    .unwrap()
    .bytes()
    .await
    .unwrap();
    let thumb = get(
        &e,
        &key,
        &format!("/api/v1/faxes/{f2}/pages/1.png?kind=thumb"),
    )
    .send()
    .await
    .unwrap()
    .bytes()
    .await
    .unwrap();
    assert_eq!(&thumb[..8], b"\x89PNG\r\n\x1a\n");
    assert!(thumb.len() < full.len());
    // If-None-Match works for a key too
    let r = get(
        &e,
        &key,
        &format!("/api/v1/faxes/{f2}/pages/1.png?kind=thumb"),
    )
    .send()
    .await
    .unwrap();
    let etag = r.headers().get("etag").unwrap().clone();
    let r = get(
        &e,
        &key,
        &format!("/api/v1/faxes/{f2}/pages/1.png?kind=thumb"),
    )
    .header("If-None-Match", etag)
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 304);
    // errors have the same shape as for a session
    let r = get(&e, &key, "/api/v1/faxes/99999").send().await.unwrap();
    assert_eq!(r.status(), 404);
    assert_eq!(json_of(r).await["error"]["code"], "not_found");
    let r = get(&e, &key, &format!("/api/v1/faxes/{f1}/pages/9.png"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 404);
    // nothing was changed by reading
    assert_eq!(e.store.count_unread().await.unwrap(), 2);
    e.stop().await;
}

// ---------------------------------------------------------------- WebSocket
async fn session_token(e: &Env, name: &str) -> String {
    let r = reqwest::Client::new()
        .post(e.url("/api/v1/auth/login"))
        .json(&json!({"username": name, "password": PW}))
        .send()
        .await
        .unwrap();
    r.headers()
        .get("set-cookie")
        .unwrap()
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1
        .to_string()
}

type Ws =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;

fn ws_request(e: &Env) -> tokio_tungstenite::tungstenite::handshake::client::Request {
    format!("{}/api/v1/events", e.base.replacen("http://", "ws://", 1))
        .into_client_request()
        .unwrap()
}

async fn next_json(ws: &mut Ws) -> Value {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("a frame within 5 s")
            .expect("open")
            .expect("no error");
        match m {
            Message::Text(t) => return serde_json::from_str(&t).unwrap(),
            Message::Ping(_) | Message::Pong(_) => {}
            other => panic!("unexpected {other:?}"),
        }
    }
}

// ---------------------------------------------------------------- K03
#[tokio::test(flavor = "multi_thread")]
async fn k03_only_a_key_with_write_changes_a_fax_and_the_event_names_it() {
    let e = env().await;
    let a = admin(&e).await;
    let id = fax(&e, "call-w", "0955", 10).await;
    e.clock.advance(Dur::seconds(3));
    let (ro, _) = make_key(&e, &a, "reader", false, None).await;
    let (rw, _) = make_key(&e, &a, "erp", true, None).await;
    let before = json_of(
        a.c.get(e.url(&format!("/api/v1/faxes/{id}")))
            .send()
            .await
            .unwrap(),
    )
    .await;

    // read only: 403 forbidden, and the fax is as it was
    for (path, body) in [
        (format!("/api/v1/faxes/{id}/read"), json!({"read": true})),
        (
            format!("/api/v1/faxes/{id}/note"),
            json!({"text": "x", "expected_updated_at": null}),
        ),
    ] {
        let r = put(&e, &ro, &path, &body).send().await.unwrap();
        assert_eq!(r.status(), 403, "{path}");
        let v = json_of(r).await;
        assert_eq!(v["error"]["code"], "forbidden");
    }
    let still = json_of(
        a.c.get(e.url(&format!("/api/v1/faxes/{id}")))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(still, before, "nothing changed");
    assert_eq!(e.store.count_unread().await.unwrap(), 1);

    // a key that may write: a body that is not JSON is refused as it is for a session
    let r = reqwest::Client::new()
        .put(e.url(&format!("/api/v1/faxes/{id}/read")))
        .header("Authorization", format!("Bearer {rw}"))
        .header("Content-Type", "text/plain")
        .body(r#"{"read": true}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    assert_eq!(json_of(r).await["error"]["code"], "csrf");
    assert!(!e.store.get(id).await.unwrap().unwrap().is_read);

    // a socket of a session sees the changes
    let token = session_token(&e, "boss").await;
    let mut rq = ws_request(&e);
    rq.headers_mut()
        .insert("cookie", format!("sipfax_session={token}").parse().unwrap());
    let (mut ws, _) = tokio_tungstenite::connect_async(rq).await.unwrap();
    assert_eq!(next_json(&mut ws).await["type"], "hello");

    let r = put(
        &e,
        &rw,
        &format!("/api/v1/faxes/{id}/read"),
        &json!({"read": true}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    let v = json_of(r).await;
    assert_eq!(
        (v["is_read"].clone(), v["read_changed_by"].clone()),
        (json!(true), json!("api:erp"))
    );
    let ev = next_json(&mut ws).await;
    assert_eq!(ev["type"], "fax.read_changed");
    assert_eq!(
        (ev["data"]["id"].clone(), ev["data"]["by"].clone()),
        (json!(id), json!("api:erp"))
    );

    let r = put(
        &e,
        &rw,
        &format!("/api/v1/faxes/{id}/note"),
        &json!({"text": "from the ERP", "expected_updated_at": null}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    let v = json_of(r).await;
    assert_eq!(
        (v["note"].clone(), v["note_updated_by"].clone()),
        (json!("from the ERP"), json!("api:erp"))
    );
    let ev = next_json(&mut ws).await;
    assert_eq!(ev["type"], "fax.note_changed");
    assert_eq!(ev["data"]["by"], "api:erp");
    assert_eq!(ev["data"]["note"], "from the ERP");
    // the optimistic lock holds for a key: a stale expected time is a 409 that changes nothing
    let r = put(
        &e,
        &rw,
        &format!("/api/v1/faxes/{id}/note"),
        &json!({"text": "lost", "expected_updated_at": null}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 409);
    assert_eq!(json_of(r).await["error"]["code"], "conflict");
    assert_eq!(
        e.store.get(id).await.unwrap().unwrap().note.as_deref(),
        Some("from the ERP")
    );
    // unread again, by the key too; an unknown fax is a 404, an unknown field a refusal
    let r = put(
        &e,
        &rw,
        &format!("/api/v1/faxes/{id}/read"),
        &json!({"read": false}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(
        put(&e, &rw, "/api/v1/faxes/99999/read", &json!({"read": true}))
            .send()
            .await
            .unwrap()
            .status(),
        404
    );
    let r = put(
        &e,
        &rw,
        &format!("/api/v1/faxes/{id}/read"),
        &json!({"read": true, "by": "boss"}),
    )
    .send()
    .await
    .unwrap();
    assert!(r.status().is_client_error(), "{}", r.status());
    assert!(!e.store.get(id).await.unwrap().unwrap().is_read);
    assert_not_logged(&ro);
    assert_not_logged(&rw);
    e.stop().await;
}

// ---------------------------------------------------------------- K04
#[tokio::test(flavor = "multi_thread")]
async fn k04_a_key_that_is_not_valid_is_refused_alike_and_at_once() {
    let e = env().await;
    let a = admin(&e).await;
    fax(&e, "call-k4", "0966", 10).await;
    e.clock.advance(Dur::seconds(3));
    let (live, _) = make_key(&e, &a, "live", false, None).await;
    let (doomed, doomed_info) = make_key(&e, &a, "doomed", false, None).await;
    let soon = (e.clock.now() + Dur::minutes(5)).to_rfc3339();
    let (short, _) = make_key(&e, &a, "short", false, Some(&soon)).await;
    let list = "/api/v1/faxes";
    let send = |auth: Option<String>| {
        let e = &e;
        async move {
            let mut rq = reqwest::Client::new().get(e.url(list));
            if let Some(v) = auth {
                rq = rq.header("Authorization", v);
            }
            let r = rq.send().await.unwrap();
            let s = r.status().as_u16();
            (s, json_of(r).await)
        }
    };
    assert_eq!(send(Some(format!("Bearer {live}"))).await.0, 200);
    // the scheme is case-insensitive (RFC 6750) and blanks around the key do not matter
    for ok in [
        format!("bearer {live}"),
        format!("BEARER {live}"),
        format!("Bearer  {live}"),
        format!("Bearer {live} "),
    ] {
        assert_eq!(send(Some(ok.clone())).await.0, 200, "{ok:?}");
    }
    let mut wrong_tail = live.clone();
    wrong_tail.replace_range(46.., if live.ends_with('A') { "B" } else { "A" });
    // all of these: 401 invalid_api_key with one and the same message
    let bad = vec![
        format!("Bearer {wrong_tail}"),
        format!("Bearer sfx_{}", "A".repeat(43)),
        format!("Bearer {}", &live[1..]),
        format!("Bearer {live}x"),
        format!("Bearer {live} extra"),
        format!("Bearer {}", live.to_uppercase()),
        format!("Bearer {}", live.replacen("sfx_", "SFX_", 1)),
        "Bearer".to_string(),
        "Bearer ".to_string(),
        "Bearer sfx_".to_string(),
        "Bearer garbage".to_string(),
        format!("Basic {live}"),
        "Basic dXNlcjpwYXNz".to_string(),
        format!("Token {live}"),
        live.clone(),
        format!("Bearer\t{live}x"),
    ];
    let mut first: Option<Value> = None;
    for auth in &bad {
        let (s, v) = send(Some(auth.clone())).await;
        assert_eq!(s, 401, "{auth:?}: {v}");
        assert_eq!(v["error"]["code"], "invalid_api_key", "{auth:?}");
        match &first {
            None => first = Some(v),
            Some(f) => assert_eq!(&v, f, "the same answer for {auth:?}"),
        }
    }
    // revoked: the very next request (no waiting, no caching)
    assert_eq!(send(Some(format!("Bearer {doomed}"))).await.0, 200);
    let id = doomed_info["id"].as_i64().unwrap();
    let r = change(
        &a.c,
        Method::POST,
        e.url(&format!("/api/v1/api-keys/{id}/revoke")),
        &a.csrf,
    )
    .json(&json!({}))
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    let (s, v) = send(Some(format!("Bearer {doomed}"))).await;
    assert_eq!(s, 401);
    assert_eq!(
        Some(v),
        first,
        "a revoked key looks like a key that never existed"
    );
    // expired: from the moment of the expiry on
    assert_eq!(send(Some(format!("Bearer {short}"))).await.0, 200);
    e.clock.advance(Dur::minutes(5) - Dur::milliseconds(1));
    assert_eq!(
        send(Some(format!("Bearer {short}"))).await.0,
        200,
        "still valid a millisecond before"
    );
    e.clock.advance(Dur::milliseconds(1));
    let (s, v) = send(Some(format!("Bearer {short}"))).await;
    assert_eq!(s, 401);
    assert_eq!(Some(v), first);
    // no key at all: a session is needed (the other error: nothing to judge)
    let (s, v) = send(None).await;
    assert_eq!(
        (s, v["error"]["code"].clone()),
        (401, json!("unauthenticated"))
    );
    // a key in the query string or another header is no key
    let r = reqwest::Client::new()
        .get(e.url(&format!("{list}?api_key={live}")))
        .header("X-Api-Key", &live)
        .send()
        .await
        .unwrap();
    assert_eq!(
        r.status(),
        401,
        "no key was read from the query string or another header"
    );
    // the good key goes on working; nothing above logged it
    assert_eq!(send(Some(format!("Bearer {live}"))).await.0, 200);
    for k in [&live, &doomed, &short] {
        assert_not_logged(k);
    }
    // none of the refusals left a mark on the keys
    let rows = e.store.api_key_list().await.unwrap();
    let live_row = rows.iter().find(|r| r.name == "live").unwrap();
    assert!(live_row.last_used_at.is_some());
    e.stop().await;
}

// ---------------------------------------------------------------- K05
#[tokio::test(flavor = "multi_thread")]
async fn k05_a_key_works_on_the_fax_routes_only() {
    let e = env().await;
    let a = admin(&e).await;
    add_user(&e, "amy", Role::Agent, false).await;
    let id = fax(&e, "call-k5", "0977", 10).await;
    e.clock.advance(Dur::seconds(3));
    // the strongest key there is
    let (key, _) = make_key(&e, &a, "strong", true, None).await;

    let config_before =
        a.c.get(e.url("/api/v1/config"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
    let users_before =
        a.c.get(e.url("/api/v1/users"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap();
    let keys_before = e.store.api_key_list().await.unwrap().len();
    let audit_before = e.store.config_audit_list(500, None).await.unwrap().len();
    let deliveries_before = e.store.list_deliveries(id).await.unwrap().len();

    let attempts: Vec<(Method, String, Option<Value>)> = vec![
        (Method::GET, "/api/v1/config".into(), None),
        (Method::GET, "/api/v1/config/audit".into(), None),
        (Method::GET, "/api/v1/config/trunks".into(), None),
        (
            Method::PUT,
            "/api/v1/config/trunks/order".into(),
            Some(json!({"order": []})),
        ),
        (
            Method::PUT,
            "/api/v1/config/sip".into(),
            Some(json!({"version": 1, "body": {}})),
        ),
        (Method::GET, "/api/v1/users".into(), None),
        (
            Method::POST,
            "/api/v1/users".into(),
            Some(json!({"username": "mallory", "role": "admin", "password": PW})),
        ),
        (
            Method::PUT,
            "/api/v1/users/1".into(),
            Some(json!({"role": "agent"})),
        ),
        (Method::DELETE, "/api/v1/users/2".into(), None),
        (Method::GET, "/api/v1/status".into(), None),
        (Method::GET, "/api/v1/auth/me".into(), None),
        (
            Method::PUT,
            "/api/v1/auth/me".into(),
            Some(json!({"display_name": "pwned"})),
        ),
        (
            Method::POST,
            "/api/v1/auth/password".into(),
            Some(json!({"current_password": PW, "new_password": "A brand new password 1"})),
        ),
        (Method::POST, "/api/v1/auth/logout".into(), Some(json!({}))),
        (Method::GET, "/api/v1/api-keys".into(), None),
        (
            Method::POST,
            "/api/v1/api-keys".into(),
            Some(json!({"name": "child", "write": true})),
        ),
        (
            Method::POST,
            "/api/v1/api-keys/1/revoke".into(),
            Some(json!({})),
        ),
        (
            Method::POST,
            format!("/api/v1/faxes/{id}/redeliver"),
            Some(json!({})),
        ),
        (Method::GET, "/api/v1/events".into(), None),
    ];
    for (m, path, body) in attempts {
        let mut rq = reqwest::Client::new()
            .request(m.clone(), e.url(&path))
            .header("Authorization", format!("Bearer {key}"));
        if let Some(b) = &body {
            rq = rq.json(b);
        }
        let r = rq.send().await.unwrap();
        let s = r.status().as_u16();
        assert!(s == 401 || s == 403, "{m} {path}: {s}");
    }
    // the same with a header that sends the key as a cookie value: not an accepted credential either
    let r = reqwest::Client::new()
        .get(e.url("/api/v1/users"))
        .header("Cookie", format!("sipfax_session={key}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    // the WebSocket upgrade with the key: refused at the handshake
    let mut rq = ws_request(&e);
    rq.headers_mut()
        .insert("authorization", format!("Bearer {key}").parse().unwrap());
    match tokio_tungstenite::connect_async(rq).await {
        Err(WsError::Http(resp)) => assert_eq!(resp.status().as_u16(), 401),
        other => panic!("the upgrade must be refused: {:?}", other.map(|_| ())),
    }

    // nothing happened
    assert_eq!(
        a.c.get(e.url("/api/v1/config"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        config_before
    );
    assert_eq!(
        a.c.get(e.url("/api/v1/users"))
            .send()
            .await
            .unwrap()
            .text()
            .await
            .unwrap(),
        users_before
    );
    assert_eq!(e.store.api_key_list().await.unwrap().len(), keys_before);
    assert_eq!(
        e.store.config_audit_list(500, None).await.unwrap().len(),
        audit_before
    );
    assert_eq!(
        e.store.list_deliveries(id).await.unwrap().len(),
        deliveries_before
    );
    // the passwords are the old ones
    login(&e, "boss").await;
    login(&e, "amy").await;
    let r = reqwest::Client::new()
        .post(e.url("/api/v1/auth/login"))
        .json(&json!({"username": "boss", "password": "A brand new password 1"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    // and the key still does its own job
    assert_eq!(
        get(&e, &key, "/api/v1/faxes")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    e.stop().await;
}

// ---------------------------------------------------------------- K06
#[tokio::test(flavor = "multi_thread")]
async fn k06_with_a_key_the_key_decides_a_cookie_does_not_help() {
    let e = env().await;
    let a = admin(&e).await;
    let id = fax(&e, "call-k6", "0988", 10).await;
    e.clock.advance(Dur::seconds(3));
    let (ro, _) = make_key(&e, &a, "reader", false, None).await;
    let (rw, _) = make_key(&e, &a, "writer", true, None).await;
    let token = session_token(&e, "boss").await;
    let cookie = format!("sipfax_session={token}");

    // an administrator's cookie and CSRF token plus a read-only key: the key's rights count
    let r = reqwest::Client::new()
        .put(e.url(&format!("/api/v1/faxes/{id}/read")))
        .header("Cookie", &cookie)
        .header("X-Sipfax-Csrf", &a.csrf)
        .header("Authorization", format!("Bearer {ro}"))
        .json(&json!({"read": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403, "the cookie must not add rights");
    assert_eq!(json_of(r).await["error"]["code"], "forbidden");
    assert!(!e.store.get(id).await.unwrap().unwrap().is_read);
    // a wrong key plus a good cookie: refused, not "falls back to the session"
    let wrong = format!("sfx_{}", "B".repeat(43));
    let r = reqwest::Client::new()
        .get(e.url("/api/v1/faxes"))
        .header("Cookie", &cookie)
        .header("Authorization", format!("Bearer {wrong}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    assert_eq!(json_of(r).await["error"]["code"], "invalid_api_key");
    // a key that may write plus the cookie: the change is the key's (api:writer), not boss's
    let r = reqwest::Client::new()
        .put(e.url(&format!("/api/v1/faxes/{id}/read")))
        .header("Cookie", &cookie)
        .header("X-Sipfax-Csrf", &a.csrf)
        .header("Authorization", format!("Bearer {rw}"))
        .json(&json!({"read": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(json_of(r).await["read_changed_by"], "api:writer");
    // a key needs no CSRF header (it is not sent by a browser on its own)
    let r = put(
        &e,
        &rw,
        &format!("/api/v1/faxes/{id}/read"),
        &json!({"read": false}),
    )
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    // outside the fax routes the Authorization header is not looked at: the cookie is judged
    // alone, a valid one works as before, with a bad key next to it as well
    for auth in [ro.clone(), wrong.clone()] {
        let r = reqwest::Client::new()
            .get(e.url("/api/v1/users"))
            .header("Cookie", &cookie)
            .header("Authorization", format!("Bearer {auth}"))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200);
    }
    // a session alone is what it always was: the CSRF rule still holds on the fax routes
    let c = reqwest::Client::new();
    let r = c
        .put(e.url(&format!("/api/v1/faxes/{id}/read")))
        .header("Cookie", &cookie)
        .json(&json!({"read": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    assert_eq!(json_of(r).await["error"]["code"], "csrf");
    assert!(!e.store.get(id).await.unwrap().unwrap().is_read);
    let r = change(
        &a.c,
        Method::PUT,
        e.url(&format!("/api/v1/faxes/{id}/read")),
        &a.csrf,
    )
    .json(&json!({"read": true}))
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(json_of(r).await["read_changed_by"], "boss");
    e.stop().await;
}

// ---------------------------------------------------------------- K07
#[tokio::test(flavor = "multi_thread")]
async fn k07_only_an_administrator_manages_keys_and_bad_requests_change_nothing() {
    let e = env().await;
    let a = admin(&e).await;
    add_user(&e, "amy", Role::Agent, false).await;
    let (amy, amy_csrf) = login(&e, "amy").await;
    let (_, info) = make_key(&e, &a, "taken", false, None).await;
    let id = info["id"].as_i64().unwrap();

    // an agent: 403 for all three, and nothing happens
    let r = amy.get(e.url("/api/v1/api-keys")).send().await.unwrap();
    assert_eq!(r.status(), 403);
    assert_eq!(json_of(r).await["error"]["code"], "forbidden");
    let r = change(&amy, Method::POST, e.url("/api/v1/api-keys"), &amy_csrf)
        .json(&json!({"name": "agentkey"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let r = change(
        &amy,
        Method::POST,
        e.url(&format!("/api/v1/api-keys/{id}/revoke")),
        &amy_csrf,
    )
    .json(&json!({}))
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 403);
    assert_eq!(e.store.api_key_list().await.unwrap().len(), 1);
    assert!(
        e.store.api_key_list().await.unwrap()[0]
            .revoked_at
            .is_none(),
        "not revoked"
    );
    // nobody: 401
    let none = reqwest::Client::new();
    assert_eq!(
        none.get(e.url("/api/v1/api-keys"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );

    // the administrator's refusals
    let post = |body: Value| {
        let (a, e) = (&a, &e);
        async move {
            let r = change(&a.c, Method::POST, e.url("/api/v1/api-keys"), &a.csrf)
                .json(&body)
                .send()
                .await
                .unwrap();
            (r.status().as_u16(), json_of(r).await)
        }
    };
    let (s, v) = post(json!({"name": "taken"})).await;
    assert_eq!((s, v["error"]["code"].clone()), (409, json!("name_taken")));
    let (s, _) = post(json!({"name": "TAKEN"})).await;
    assert_eq!(s, 409, "the name is not case sensitive");
    for bad in ["", "has space", "a/b", "名稱", &"n".repeat(49)] {
        let (s, v) = post(json!({"name": bad})).await;
        assert_eq!(
            (s, v["error"]["code"].clone()),
            (422, json!("bad_name")),
            "{bad:?}"
        );
    }
    let past = (e.clock.now() - Dur::seconds(1)).to_rfc3339();
    let (s, v) = post(json!({"name": "old", "expires_at": past})).await;
    assert_eq!((s, v["error"]["code"].clone()), (422, json!("bad_expiry")));
    let (s, _) = post(json!({"name": "now", "expires_at": e.clock.now().to_rfc3339()})).await;
    assert_eq!(s, 422);
    let (s, _) = post(json!({"name": "x", "expires_at": "tomorrow"})).await;
    assert!(s == 400 || s == 422, "{s}");
    let (s, _) = post(json!({"name": "x", "admin": true})).await;
    assert!(s == 400 || s == 422, "unknown field: {s}");
    let (s, _) = post(json!({"write": true})).await;
    assert!(s == 400 || s == 422, "no name: {s}");
    // without the CSRF header or without JSON: 403, nothing created
    let r =
        a.c.post(e.url("/api/v1/api-keys"))
            .json(&json!({"name": "nocsrf"}))
            .send()
            .await
            .unwrap();
    assert_eq!(r.status(), 403);
    let r =
        a.c.post(e.url("/api/v1/api-keys"))
            .header("X-Sipfax-Csrf", &a.csrf)
            .header("Content-Type", "text/plain")
            .body(r#"{"name":"plain"}"#)
            .send()
            .await
            .unwrap();
    assert_eq!(r.status(), 403);
    // nothing of this exists
    let names: Vec<_> = e
        .store
        .api_key_list()
        .await
        .unwrap()
        .into_iter()
        .map(|k| k.name)
        .collect();
    assert_eq!(names, ["taken"]);

    // revoking: once; an unknown or a malformed id; the name stays taken afterwards
    let rev = |id: &str| {
        let (a, e) = (&a, &e);
        let url = e.url(&format!("/api/v1/api-keys/{id}/revoke"));
        async move {
            let r = change(&a.c, Method::POST, url, &a.csrf)
                .json(&json!({}))
                .send()
                .await
                .unwrap();
            (r.status().as_u16(), json_of(r).await)
        }
    };
    let (s, v) = rev(&id.to_string()).await;
    assert_eq!(s, 200);
    assert_eq!(v["api_key"]["active"], false);
    assert_eq!(v["api_key"]["revoked_by"], "boss");
    let (s, v) = rev(&id.to_string()).await;
    assert_eq!(
        (s, v["error"]["code"].clone()),
        (409, json!("already_revoked"))
    );
    assert_eq!(rev("99999").await.0, 404);
    assert_eq!(rev("abc").await.0, 404);
    assert_eq!(rev("-1").await.0, 404);
    let (s, v) = post(json!({"name": "taken"})).await;
    assert_eq!(
        (s, v["error"]["code"].clone()),
        (409, json!("name_taken")),
        "also after revoking"
    );
    // a valid one still works after all the refusals
    let (s, _) = post(json!({"name": "fine"})).await;
    assert_eq!(s, 201);
    e.stop().await;
}

// ---------------------------------------------------------------- K08
#[tokio::test(flavor = "multi_thread")]
async fn k08_the_audit_has_one_row_for_creating_and_one_for_revoking() {
    let e = env().await;
    let a = admin(&e).await;
    add_user(&e, "second", Role::Admin, false).await;
    let (key, info) = make_key(&e, &a, "audited", true, None).await;
    let (c2, csrf2) = login(&e, "second").await;
    let r = change(
        &c2,
        Method::POST,
        e.url(&format!("/api/v1/api-keys/{}/revoke", info["id"])),
        &csrf2,
    )
    .json(&json!({}))
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    // refusals write no row
    let _ = change(&a.c, Method::POST, e.url("/api/v1/api-keys"), &a.csrf)
        .json(&json!({"name": "audited"}))
        .send()
        .await
        .unwrap();
    let v = json_of(a.c.get(e.url("/api/v1/config/audit")).send().await.unwrap()).await;
    let rows: Vec<&Value> = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|r| r["section"] == "api_key:audited")
        .collect();
    assert_eq!(rows.len(), 2, "{v}");
    let create = rows.iter().find(|r| r["action"] == "key_create").unwrap();
    let revoke = rows.iter().find(|r| r["action"] == "key_revoke").unwrap();
    assert_eq!(create["by"], "boss");
    assert_eq!(revoke["by"], "second");
    assert!(create["at"].as_str().is_some() && revoke["at"].as_str().is_some());
    assert_eq!(create["after"]["scopes"], "fax:read,fax:write", "{create}");
    assert_eq!(create["after"]["prefix"], key[..12]);
    assert!(!v.to_string().contains(&key[12..]), "no key in the audit");
    e.stop().await;
}

// ---------------------------------------------------------------- K09
#[tokio::test(flavor = "multi_thread")]
async fn k09_the_last_use_is_recorded_at_most_once_a_minute() {
    let e = env().await;
    let a = admin(&e).await;
    let (key, info) = make_key(&e, &a, "watched", false, None).await;
    let last = |e: &Env| {
        let c = a.c.clone();
        let url = e.url("/api/v1/api-keys");
        async move {
            let v = json_of(c.get(url).send().await.unwrap()).await;
            let k = v["keys"][0].clone();
            (k["last_used_at"].clone(), k["last_used_ip"].clone())
        }
    };
    assert_eq!(last(&e).await, (Value::Null, Value::Null), "unused");
    assert_eq!(info["last_used_at"], Value::Null);
    // the first use: time and address
    e.clock.advance(Dur::seconds(10));
    let first = e.clock.now();
    assert_eq!(
        get(&e, &key, "/api/v1/faxes/counts")
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let (at, ip) = last(&e).await;
    assert_eq!(ip, "127.0.0.1");
    let first_at = at.as_str().unwrap().to_string();
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(&first_at)
            .unwrap()
            .timestamp_millis(),
        first.timestamp_millis()
    );
    // again within the minute (many times): nothing is written
    for secs in [1, 5, 30, 59] {
        e.clock.set(first + Dur::seconds(secs));
        for _ in 0..3 {
            get(&e, &key, "/api/v1/faxes/counts").send().await.unwrap();
        }
        assert_eq!(last(&e).await.0, json!(first_at), "after {secs} s");
    }
    // a refused request is no use
    e.clock.set(first + Dur::seconds(100));
    get(
        &e,
        &format!("sfx_{}", "C".repeat(43)),
        "/api/v1/faxes/counts",
    )
    .send()
    .await
    .unwrap();
    assert_eq!(last(&e).await.0, json!(first_at));
    // a minute after the written one: written again
    e.clock.set(first + Dur::seconds(120));
    get(&e, &key, "/api/v1/faxes/counts").send().await.unwrap();
    let (at, _) = last(&e).await;
    assert_eq!(
        chrono::DateTime::parse_from_rfc3339(at.as_str().unwrap())
            .unwrap()
            .timestamp_millis(),
        (first + Dur::seconds(120)).timestamp_millis()
    );
    e.stop().await;
}
