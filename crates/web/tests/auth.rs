//! Login, session cookie, CSRF and the forced password change over HTTP (A01, A05, A06, A08,
//! A09, A10 at the HTTP level; the logic itself is tested in `accounts`).

mod common;

use accounts::Role;
use common::{PW, add_user, env, login};
use serde_json::{Value, json};

async fn status_of(r: reqwest::Response) -> (u16, Value) {
    let s = r.status().as_u16();
    (s, r.json().await.unwrap_or(Value::Null))
}

#[tokio::test]
async fn a09_the_session_cookie_is_httponly_samesite_strict_and_not_secure_over_http() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    let r = reqwest::Client::new()
        .post(e.url("/api/v1/auth/login"))
        .json(&json!({"username": "alice", "password": PW}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let cookie = r.headers()["set-cookie"].to_str().unwrap().to_string();
    assert!(cookie.starts_with("sipfax_session="), "{cookie}");
    for want in ["HttpOnly", "SameSite=Strict", "Path=/", "Max-Age=604800"] {
        assert!(cookie.contains(want), "{want} missing: {cookie}");
    }
    assert!(
        !cookie.contains("Secure"),
        "no Secure over plain HTTP: {cookie}"
    );
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["user"]["username"], "alice");
    assert_eq!(v["user"]["role"], "agent");
    assert!(v["user"].get("password_hash").is_none());
    assert_eq!(v["csrf"].as_str().unwrap().len(), 64);
    e.stop().await;
}

#[test]
fn a09_over_https_the_cookie_is_secure() {
    let now = chrono::Utc::now();
    let c = web::session::session_cookie("t", now + chrono::Duration::hours(1), now, true);
    assert!(c.contains("; Secure") && c.contains("Max-Age=3600"), "{c}");
    assert!(web::session::clear_cookie(true).contains("Max-Age=0; Secure"));
    assert!(!web::session::clear_cookie(false).contains("Secure"));
}

#[tokio::test]
async fn a05_unknown_account_and_wrong_password_get_the_same_401() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    let c = reqwest::Client::new();
    let try_login = |u: &'static str, p: &'static str| {
        c.post(e.url("/api/v1/auth/login"))
            .json(&json!({"username": u, "password": p}))
            .send()
    };
    let (s1, b1) = status_of(try_login("alice", "wrong password!").await.unwrap()).await;
    let (s2, b2) = status_of(try_login("nobody", "wrong password!").await.unwrap()).await;
    assert_eq!((s1, s2), (401, 401));
    assert_eq!(b1, b2, "the answers must not tell the two apart");
    assert_eq!(b1["error"]["code"], "invalid_credentials");
    e.stop().await;
}

#[tokio::test]
async fn a06_more_than_twenty_attempts_a_minute_from_one_address_get_429() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    let c = reqwest::Client::new();
    for i in 0..20 {
        let r = c
            .post(e.url("/api/v1/auth/login"))
            .json(&json!({"username": format!("u{i}"), "password": "wrong password!"}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 401, "attempt {i}");
    }
    // the 21st, even with the right password
    let r = c
        .post(e.url("/api/v1/auth/login"))
        .json(&json!({"username": "alice", "password": PW}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 429);
    let retry: i64 = r.headers()["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((1..=60).contains(&retry), "{retry}");
    assert!(r.headers().get("set-cookie").is_none(), "no session");
    let (_, b) = status_of(r).await;
    assert_eq!(b["error"]["code"], "rate_limited");
    // a minute later (injected clock) it is fine again
    e.clock.advance(chrono::Duration::seconds(61));
    let r = c
        .post(e.url("/api/v1/auth/login"))
        .json(&json!({"username": "alice", "password": PW}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    e.stop().await;
}

#[tokio::test]
async fn a10_a_change_without_json_or_without_the_right_csrf_header_is_403_and_changes_nothing() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    let (c, csrf) = login(&e, "alice").await;
    let pw_change = json!({"current": PW, "new": "another long password"});
    // no CSRF header
    let r = c
        .post(e.url("/api/v1/auth/password"))
        .json(&pw_change)
        .send()
        .await
        .unwrap();
    let (s, b) = status_of(r).await;
    assert_eq!((s, b["error"]["code"].as_str()), (403, Some("csrf")));
    // a wrong one
    let r = c
        .post(e.url("/api/v1/auth/password"))
        .header("X-Sipfax-Csrf", "0".repeat(64))
        .json(&pw_change)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    // the right one but a form body
    let r = c
        .post(e.url("/api/v1/auth/password"))
        .header("X-Sipfax-Csrf", &csrf)
        .header("Content-Type", "application/x-www-form-urlencoded")
        .body("current=x&new=y")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    // logout without the header: refused, the session still works
    let r = c
        .post(e.url("/api/v1/auth/logout"))
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let r = c.get(e.url("/api/v1/auth/me")).send().await.unwrap();
    assert_eq!(r.status(), 200, "the refused logout ended nothing");
    // the database is unchanged: the old password still logs in
    login(&e, "alice").await;
    // and with everything right it works
    let r = c
        .post(e.url("/api/v1/auth/password"))
        .header("X-Sipfax-Csrf", &csrf)
        .json(&pw_change)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    e.stop().await;
}

#[tokio::test]
async fn a08_logout_and_a_password_change_end_the_sessions() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    let (c1, csrf1) = login(&e, "alice").await;
    let (c2, csrf2) = login(&e, "alice").await;
    // logout ends this session
    let r = c1
        .post(e.url("/api/v1/auth/logout"))
        .header("X-Sipfax-Csrf", &csrf1)
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 204);
    assert!(
        r.headers()["set-cookie"]
            .to_str()
            .unwrap()
            .contains("Max-Age=0")
    );
    let (s, b) = status_of(c1.get(e.url("/api/v1/auth/me")).send().await.unwrap()).await;
    assert_eq!(
        (s, b["error"]["code"].as_str()),
        (401, Some("unauthenticated"))
    );
    // a password change ends every other session and gives this one a new cookie
    let (c3, _) = login(&e, "alice").await;
    let r = c2
        .post(e.url("/api/v1/auth/password"))
        .header("X-Sipfax-Csrf", &csrf2)
        .json(&json!({"current": PW, "new": "another long password"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(
        c2.get(e.url("/api/v1/auth/me"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    assert_eq!(
        c3.get(e.url("/api/v1/auth/me"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    e.stop().await;
}

#[tokio::test]
async fn a01_an_account_that_must_change_its_password_may_only_do_that() {
    let e = env().await;
    add_user(&e, "admin", Role::Admin, true).await;
    let (c, csrf) = login(&e, "admin").await;
    let (s, b) = status_of(c.get(e.url("/api/v1/auth/me")).send().await.unwrap()).await;
    assert_eq!(s, 200);
    assert_eq!(b["user"]["must_change_password"], true);
    // (the other routes refuse it with 403 password_change_required: tested with the fax routes)
    let r = c
        .post(e.url("/api/v1/auth/password"))
        .header("X-Sipfax-Csrf", &csrf)
        .json(&json!({"current": "wrong!!!!!!!", "new": "another long password"}))
        .send()
        .await
        .unwrap();
    let (s, b) = status_of(r).await;
    assert_eq!(
        (s, b["error"]["code"].as_str()),
        (403, Some("wrong_password"))
    );
    let r = c
        .post(e.url("/api/v1/auth/password"))
        .header("X-Sipfax-Csrf", &csrf)
        .json(&json!({"current": PW, "new": "another long password"}))
        .send()
        .await
        .unwrap();
    let (s, b) = status_of(r).await;
    assert_eq!(s, 200);
    assert_eq!(b["user"]["must_change_password"], false);
    e.stop().await;
}

#[tokio::test]
async fn w10_bad_bodies_get_the_error_shape() {
    let e = env().await;
    let c = reqwest::Client::new();
    let r = c
        .post(e.url("/api/v1/auth/login"))
        .header("Content-Type", "application/json")
        .body("{not json")
        .send()
        .await
        .unwrap();
    let (s, b) = status_of(r).await;
    assert_eq!((s, b["error"]["code"].as_str()), (400, Some("bad_request")));
    let big = format!(
        "{{\"username\":\"{}\",\"password\":\"x\"}}",
        "a".repeat(2 * 1024 * 1024)
    );
    let r = c
        .post(e.url("/api/v1/auth/login"))
        .header("Content-Type", "application/json")
        .body(big)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 413);
    e.stop().await;
}
