//! M4b ⑤: `/api/v1/events` (V01-V06, A08) with a real WebSocket client.

mod common;

use std::time::Duration;

use accounts::Role;
use common::*;
use futures_util::{SinkExt, StreamExt};
use reqwest::Method;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::protocol::frame::coding::CloseCode;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

/// Logs `name` in; returns the session token (the cookie's value) and the CSRF token.
async fn token(e: &Env, name: &str) -> (String, String) {
    let r = reqwest::Client::new()
        .post(e.url("/api/v1/auth/login"))
        .json(&json!({"username": name, "password": PW}))
        .send()
        .await
        .unwrap();
    let cookie = r
        .headers()
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
        .to_string();
    let v: Value = r.json().await.unwrap();
    (cookie, v["csrf"].as_str().unwrap().to_string())
}

fn ws_url(e: &Env, query: &str) -> String {
    format!(
        "{}/api/v1/events{query}",
        e.base.replacen("http://", "ws://", 1)
    )
}

async fn try_connect(
    e: &Env,
    token: Option<&str>,
    query: &str,
    origin: Option<&str>,
) -> Result<Ws, WsError> {
    let mut rq = ws_url(e, query).into_client_request().unwrap();
    if let Some(t) = token {
        rq.headers_mut()
            .insert("cookie", format!("sipfax_session={t}").parse().unwrap());
    }
    if let Some(o) = origin {
        rq.headers_mut().insert("origin", o.parse().unwrap());
    }
    tokio_tungstenite::connect_async(rq).await.map(|(s, _)| s)
}

async fn connect(e: &Env, token: &str, query: &str) -> Ws {
    try_connect(e, Some(token), query, None).await.unwrap()
}

/// The next text frame as JSON (pings are skipped; the library answers them).
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

/// Reads until the server closes; returns the close code.
async fn close_code(ws: &mut Ws) -> Option<u16> {
    loop {
        match tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("closed within 5 s")
        {
            Some(Ok(Message::Close(f))) => return f.map(|f| u16::from(f.code)),
            Some(Ok(_)) => {}
            Some(Err(_)) | None => return None,
        }
    }
}

fn http_status(r: Result<Ws, WsError>) -> u16 {
    match r {
        Err(WsError::Http(resp)) => resp.status().as_u16(),
        Err(other) => panic!("{other}"),
        Ok(_) => panic!("the upgrade was accepted"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn v01_hello_then_the_events_of_the_api() {
    let e = env().await;
    add_user(&e, "amy", Role::Agent, false).await;
    let id = fax(&e, "c1", "0912", 5).await;
    let (t, _) = token(&e, "amy").await;
    let mut ws = connect(&e, &t, "").await;
    let hello = next_json(&mut ws).await;
    assert_eq!(hello["type"], "hello");
    assert_eq!(hello["seq"], 0);

    let (c, csrf) = login(&e, "amy").await;
    let r = change(
        &c,
        Method::PUT,
        e.url(&format!("/api/v1/faxes/{id}/read")),
        &csrf,
    )
    .json(&json!({"read": true}))
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    let ev = next_json(&mut ws).await;
    assert_eq!(ev["type"], "fax.read_changed");
    assert_eq!(ev["seq"], 1);
    assert_eq!(ev["data"]["id"], id);
    assert_eq!(ev["data"]["is_read"], true);
    assert_eq!(ev["data"]["by"], "amy");

    let r = change(
        &c,
        Method::PUT,
        e.url(&format!("/api/v1/faxes/{id}/note")),
        &csrf,
    )
    .json(&json!({"text": "call back", "expected_updated_at": null}))
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    let ev = next_json(&mut ws).await;
    assert_eq!(ev["type"], "fax.note_changed");
    assert_eq!(ev["seq"], 2);
    assert_eq!(ev["data"]["note"], "call back");

    // what the daemon publishes reaches the connection the same way
    e.events.fax_created(77, "call-77");
    e.events.fax_progress(77, "call-77", 3);
    e.events.delivery_updated(id);
    let kinds: Vec<Value> = vec![
        next_json(&mut ws).await,
        next_json(&mut ws).await,
        next_json(&mut ws).await,
    ];
    assert_eq!(kinds[0]["type"], "fax.created");
    assert_eq!(kinds[1]["data"]["pages"], 3);
    assert_eq!(kinds[2]["data"]["fax_id"], id);
    e.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn v02_v03_since_replays_or_asks_for_a_resync() {
    let e = env().await;
    add_user(&e, "amy", Role::Agent, false).await;
    let (t, _) = token(&e, "amy").await;
    for i in 0..3 {
        e.events.publish("t", json!(i));
    }
    let mut ws = connect(&e, &t, "?since=1").await;
    assert_eq!(next_json(&mut ws).await["seq"], 3, "hello names the last");
    assert_eq!(next_json(&mut ws).await["seq"], 2);
    assert_eq!(next_json(&mut ws).await["seq"], 3);
    e.events.publish("t", json!(3));
    assert_eq!(next_json(&mut ws).await["seq"], 4, "then live, no gap");

    // newer than anything here (a client of the previous process)
    let mut ws = connect(&e, &t, "?since=999").await;
    next_json(&mut ws).await;
    let r = next_json(&mut ws).await;
    assert_eq!(
        (r["type"].as_str(), r["seq"].as_u64()),
        (Some("resync"), Some(4))
    );

    // older than the ring
    for i in 0..web::events::RING {
        e.events.publish("t", json!(i));
    }
    let mut ws = connect(&e, &t, "?since=2").await;
    next_json(&mut ws).await;
    assert_eq!(next_json(&mut ws).await["type"], "resync");

    let r = try_connect(&e, Some(&t), "?since=minus", None).await;
    assert_eq!(http_status(r), 400);
    e.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn v04_a_client_that_does_not_answer_pings_is_closed() {
    let e = env_opts(web::REQUEST_TIMEOUT, Duration::from_millis(150)).await;
    add_user(&e, "amy", Role::Agent, false).await;
    let (t, _) = token(&e, "amy").await;

    // this one reads (and so answers the pings): still open after many intervals
    let mut alive = connect(&e, &t, "").await;
    next_json(&mut alive).await;
    let reader = tokio::spawn(async move {
        let mut pings = 0;
        let end = tokio::time::Instant::now() + Duration::from_millis(1200);
        while tokio::time::Instant::now() < end {
            match tokio::time::timeout(Duration::from_millis(200), alive.next()).await {
                Ok(Some(Ok(Message::Ping(_)))) => pings += 1,
                Ok(Some(Ok(Message::Close(f)))) => panic!("closed: {f:?}"),
                Ok(Some(Ok(_))) | Err(_) => {}
                Ok(Some(Err(e))) => panic!("{e}"),
                Ok(None) => panic!("ended"),
            }
        }
        pings
    });

    // this one never reads, so it never answers
    let mut silent = connect(&e, &t, "").await;
    tokio::time::sleep(Duration::from_millis(900)).await;
    assert_eq!(close_code(&mut silent).await, Some(4002));
    assert!(reader.await.unwrap() >= 4, "pings arrive every interval");
    e.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn v05_a_client_that_falls_behind_is_closed() {
    let e = env().await;
    add_user(&e, "amy", Role::Agent, false).await;
    let (t, _) = token(&e, "amy").await;
    let mut ws = connect(&e, &t, "").await;
    next_json(&mut ws).await;
    // far more than the channel holds, faster than one connection can write them
    let big = "x".repeat(2000);
    for i in 0..20 * web::events::LAG {
        e.events.publish("t", json!({"i": i, "pad": big}));
    }
    assert_eq!(close_code(&mut ws).await, Some(4008));
    e.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a08_logout_password_and_disable_close_the_connections() {
    let e = env().await;
    add_user(&e, "root", Role::Admin, false).await;
    add_user(&e, "amy", Role::Agent, false).await;
    let (admin, admin_csrf) = login(&e, "root").await;
    let amy_id = e.auth.get_user("amy").await.unwrap().id;

    // logout: both connections of the account close, another account's stays
    let (t1, csrf1) = token(&e, "amy").await;
    let (t2, _) = token(&e, "amy").await;
    let (troot, _) = token(&e, "root").await;
    let mut a1 = connect(&e, &t1, "").await;
    let mut a2 = connect(&e, &t2, "").await;
    let mut r = connect(&e, &troot, "").await;
    for ws in [&mut a1, &mut a2, &mut r] {
        next_json(ws).await;
    }
    let out = reqwest::Client::new()
        .post(e.url("/api/v1/auth/logout"))
        .header("cookie", format!("sipfax_session={t1}"))
        .header("x-sipfax-csrf", csrf1)
        .header("content-type", "application/json")
        .send()
        .await
        .unwrap();
    assert_eq!(out.status(), 204);
    assert_eq!(close_code(&mut a1).await, Some(4001));
    assert_eq!(close_code(&mut a2).await, Some(4001));
    e.events.publish("still", json!(null));
    assert_eq!(next_json(&mut r).await["type"], "still");

    // the administrator disables the account
    let (t3, _) = token(&e, "amy").await;
    let mut a3 = connect(&e, &t3, "").await;
    next_json(&mut a3).await;
    let res = change(
        &admin,
        Method::PUT,
        e.url(&format!("/api/v1/users/{amy_id}")),
        &admin_csrf,
    )
    .json(&json!({"disabled": true}))
    .send()
    .await
    .unwrap();
    assert_eq!(res.status(), 200);
    assert_eq!(close_code(&mut a3).await, Some(4001));
    // and the next connection is refused
    assert_eq!(http_status(try_connect(&e, Some(&t3), "", None).await), 401);
    e.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn the_handshake_needs_a_session_and_this_origin() {
    let e = env().await;
    add_user(&e, "amy", Role::Agent, false).await;
    let (t, _) = token(&e, "amy").await;
    assert_eq!(http_status(try_connect(&e, None, "", None).await), 401);
    assert_eq!(
        http_status(try_connect(&e, Some("forged"), "", None).await),
        401
    );
    assert_eq!(
        http_status(try_connect(&e, Some(&t), "", Some("https://evil.example")).await),
        403
    );
    let host = e.base.trim_start_matches("http://");
    let mut ws = try_connect(&e, Some(&t), "", Some(&format!("http://{host}")))
        .await
        .unwrap();
    assert_eq!(next_json(&mut ws).await["type"], "hello");
    // a client that closes is let go
    ws.close(Some(tokio_tungstenite::tungstenite::protocol::CloseFrame {
        code: CloseCode::Normal,
        reason: "bye".into(),
    }))
    .await
    .unwrap();
    // a plain GET is not an upgrade
    let (c, _) = login(&e, "amy").await;
    let r = c.get(e.url("/api/v1/events")).send().await.unwrap();
    assert_eq!(r.status(), 400);
    let _ = ws.send(Message::Text("ignored".into())).await;
    e.stop().await;
}
