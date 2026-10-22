//! A03, A04, A05, A07 and A08 over HTTP: the password rules, the lock-out, the timing of a
//! failed login, the session lifetimes (injected clock) and the end of the sessions after a
//! password reset. (`accounts` tests the same rules without HTTP.)

mod common;

use accounts::Role;
use common::{PW, add_user, env, login};
use serde_json::{Value, json};

async fn status_of(r: reqwest::Response) -> (u16, Value) {
    let s = r.status().as_u16();
    (s, r.json().await.unwrap_or(Value::Null))
}

// ------------------------------------------------------------------------------ A03

#[tokio::test(flavor = "multi_thread")]
async fn a03_the_password_rules_are_422_with_fixed_codes_over_http_and_nothing_changes() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    add_user(&e, "root", Role::Admin, false).await;
    let (c, csrf) = login(&e, "alice").await;
    let (admin, acsrf) = login(&e, "root").await;
    let users_before = e.auth.list_users().await.unwrap();
    for (new, code) in [
        ("short", "password_too_short"),
        ("123456789", "password_too_short"),
        (&"x".repeat(129), "password_too_long"),
    ] {
        // a password change by the account
        let r = c
            .post(e.url("/api/v1/auth/password"))
            .header("X-Sipfax-Csrf", &csrf)
            .json(&json!({"current": PW, "new": new}))
            .send()
            .await
            .unwrap();
        let (s, b) = status_of(r).await;
        assert_eq!((s, b["error"]["code"].as_str()), (422, Some(code)), "{new}");
        // a new account made by an administrator
        let r = admin
            .post(e.url("/api/v1/users"))
            .header("X-Sipfax-Csrf", &acsrf)
            .json(&json!({"username": "carol", "role": "agent", "password": new}))
            .send()
            .await
            .unwrap();
        let (s, b) = status_of(r).await;
        assert_eq!((s, b["error"]["code"].as_str()), (422, Some(code)), "{new}");
    }
    // the same as the account name (ignoring case): needs ten characters to get that far
    let r = c
        .post(e.url("/api/v1/auth/password"))
        .header("X-Sipfax-Csrf", &csrf)
        .json(&json!({"current": PW, "new": "ALICE"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 422);
    add_user(&e, "alicealice", Role::Agent, false).await;
    let (c2, csrf2) = login(&e, "alicealice").await;
    let r = c2
        .post(e.url("/api/v1/auth/password"))
        .header("X-Sipfax-Csrf", &csrf2)
        .json(&json!({"current": PW, "new": "ALICEALICE"}))
        .send()
        .await
        .unwrap();
    let (s, b) = status_of(r).await;
    assert_eq!(
        (s, b["error"]["code"].as_str()),
        (422, Some("password_same_as_username"))
    );
    // the old password still works and nobody was created
    login(&e, "alice").await;
    assert_eq!(
        e.auth.list_users().await.unwrap().len(),
        users_before.len() + 1,
        "only alicealice, added by the test"
    );
    // ten characters are enough
    let r = c
        .post(e.url("/api/v1/auth/password"))
        .header("X-Sipfax-Csrf", &csrf)
        .json(&json!({"current": PW, "new": "0123456789"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    e.stop().await;
}

// ------------------------------------------------------------------------------ A04

#[tokio::test(flavor = "multi_thread")]
async fn a04_five_failures_lock_the_account_for_fifteen_minutes_over_http() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    let c = reqwest::Client::new();
    let attempt = |pw: &'static str| {
        c.post(e.url("/api/v1/auth/login"))
            .json(&json!({"username": "alice", "password": pw}))
            .send()
    };
    for i in 0..5 {
        let (s, b) = status_of(attempt("not the password").await.unwrap()).await;
        assert_eq!(
            (s, b["error"]["code"].as_str()),
            (401, Some("invalid_credentials")),
            "{i}"
        );
    }
    // the sixth, with the right password
    let r = attempt(PW).await.unwrap();
    assert_eq!(r.status(), 423);
    let retry: i64 = r.headers()["retry-after"]
        .to_str()
        .unwrap()
        .parse()
        .unwrap();
    assert!((800..=900).contains(&retry), "{retry}");
    assert!(
        r.headers().get("set-cookie").is_none(),
        "no session for a locked account"
    );
    let (_, b) = status_of(r).await;
    assert_eq!(b["error"]["code"], "account_locked");
    // 15 minutes less a second later: still locked (one request a minute stays under the
    // limit of the address)
    for _ in 0..14 {
        e.clock.advance(chrono::Duration::seconds(60));
        assert_eq!(attempt(PW).await.unwrap().status(), 423);
    }
    e.clock.advance(chrono::Duration::seconds(59));
    assert_eq!(
        attempt(PW).await.unwrap().status(),
        423,
        "14:59 after the lock"
    );
    e.clock.advance(chrono::Duration::seconds(2));
    let r = attempt(PW).await.unwrap();
    assert_eq!(r.status(), 200, "the lock ran out");
    assert!(r.headers().get("set-cookie").is_some());
    e.stop().await;
}

// ------------------------------------------------------------------------------ A05

#[tokio::test(flavor = "multi_thread")]
async fn a05_over_http_unknown_and_wrong_password_take_the_same_time_within_20_percent() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    let c = reqwest::Client::new();
    let mut known = Vec::new();
    let mut unknown = Vec::new();
    for i in 0..50 {
        // 100 requests from one address: the clock moves on so that the limit never applies,
        // and the failure count of alice is reset so that she is never locked
        if i % 8 == 0 {
            e.clock.advance(chrono::Duration::seconds(61));
        }
        let t = std::time::Instant::now();
        let r = c
            .post(e.url("/api/v1/auth/login"))
            .json(&json!({"username": "alice", "password": "wrong password!"}))
            .send()
            .await
            .unwrap();
        known.push(t.elapsed());
        assert_eq!(r.status(), 401);
        e.auth.set_password("alice", PW, false, "t").await.unwrap();
        let t = std::time::Instant::now();
        let r = c
            .post(e.url("/api/v1/auth/login"))
            .json(&json!({"username": format!("nobody{i}"), "password": "wrong password!"}))
            .send()
            .await
            .unwrap();
        unknown.push(t.elapsed());
        assert_eq!(r.status(), 401);
    }
    let med = |mut v: Vec<std::time::Duration>| {
        v.sort();
        v[v.len() / 2].as_secs_f64()
    };
    let (k, u) = (med(known), med(unknown));
    let ratio = (k - u).abs() / k.max(u);
    assert!(
        ratio < 0.20,
        "median known {k:.4}s, unknown {u:.4}s, ratio {ratio:.2}"
    );
    assert!(
        k > 0.001 && u > 0.001,
        "both paths hash a password: {k} {u}"
    );
    e.stop().await;
}

// ------------------------------------------------------------------------------ A07

#[tokio::test(flavor = "multi_thread")]
async fn a07_idle_expiry_is_8_hours_from_the_last_request_with_the_edge_one_second_either_side() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    let (c, _) = login(&e, "alice").await;
    let hour = |h: i64, s: i64| chrono::Duration::hours(h) + chrono::Duration::seconds(s);
    let me = || c.get(e.url("/api/v1/auth/me")).send();
    // one second before the end of the idle time: fine, and the request renews it
    e.clock.advance(hour(8, -1));
    assert_eq!(me().await.unwrap().status(), 200);
    e.clock.advance(hour(8, -1));
    assert_eq!(
        me().await.unwrap().status(),
        200,
        "measured from the last use"
    );
    // one second after: gone
    e.clock.advance(hour(8, 1));
    let (s, b) = status_of(me().await.unwrap()).await;
    assert_eq!(
        (s, b["error"]["code"].as_str()),
        (401, Some("session_expired"))
    );
    // and it stays gone, for every route
    for p in ["/api/v1/faxes", "/api/v1/status", "/api/v1/auth/me"] {
        let r = c.get(e.url(p)).send().await.unwrap();
        assert_eq!(r.status(), 401, "{p}");
    }
    // a new login works
    login(&e, "alice").await;
    e.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a07_the_absolute_limit_is_7_days_however_busy_the_session_is() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    let (c, _) = login(&e, "alice").await;
    let me = || c.get(e.url("/api/v1/auth/me")).send();
    // a request every 7 hours keeps the idle time away for 161 hours
    for _ in 0..23 {
        e.clock.advance(chrono::Duration::hours(7));
        assert_eq!(me().await.unwrap().status(), 200);
    }
    // 167 h 59 min 59 s after the login: one second left
    e.clock
        .advance(chrono::Duration::hours(6) + chrono::Duration::seconds(59 * 60 + 59));
    assert_eq!(
        me().await.unwrap().status(),
        200,
        "one second before 7 days"
    );
    e.clock.advance(chrono::Duration::seconds(2));
    let (s, b) = status_of(me().await.unwrap()).await;
    assert_eq!(
        (s, b["error"]["code"].as_str()),
        (401, Some("session_expired")),
        "one second after"
    );
    e.stop().await;
}

// ------------------------------------------------------------------------------ A08

#[tokio::test(flavor = "multi_thread")]
async fn a08_an_administrator_resetting_a_password_ends_all_sessions_of_the_account() {
    let e = env().await;
    add_user(&e, "root", Role::Admin, false).await;
    add_user(&e, "amy", Role::Agent, false).await;
    let (admin, acsrf) = login(&e, "root").await;
    let (amy1, _) = login(&e, "amy").await;
    let (amy2, _) = login(&e, "amy").await;
    let id = e.auth.get_user("amy").await.unwrap().id;
    // a reset with a password that breaks the rules changes nothing
    let r = admin
        .put(e.url(&format!("/api/v1/users/{id}")))
        .header("X-Sipfax-Csrf", &acsrf)
        .json(&json!({"password": "short"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 422);
    for c in [&amy1, &amy2] {
        assert_eq!(
            c.get(e.url("/api/v1/auth/me"))
                .send()
                .await
                .unwrap()
                .status(),
            200
        );
    }
    let r = admin
        .put(e.url(&format!("/api/v1/users/{id}")))
        .header("X-Sipfax-Csrf", &acsrf)
        .json(&json!({"password": "a brand new password"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    for c in [&amy1, &amy2] {
        let (s, b) = status_of(c.get(e.url("/api/v1/auth/me")).send().await.unwrap()).await;
        assert_eq!(
            (s, b["error"]["code"].as_str()),
            (401, Some("unauthenticated"))
        );
    }
    // the old password is dead, the new one logs in but has to be changed
    let r = reqwest::Client::new()
        .post(e.url("/api/v1/auth/login"))
        .json(&json!({"username": "amy", "password": PW}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let r = reqwest::Client::new()
        .post(e.url("/api/v1/auth/login"))
        .json(&json!({"username": "amy", "password": "a brand new password"}))
        .send()
        .await
        .unwrap();
    let (s, b) = status_of(r).await;
    assert_eq!(s, 200);
    assert_eq!(b["user"]["must_change_password"], true);
    // the administrator's own session is untouched
    assert_eq!(
        admin
            .get(e.url("/api/v1/auth/me"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    e.stop().await;
}

// ------------------------------------------------------------------------------ A01

#[tokio::test(flavor = "multi_thread")]
async fn a01_the_first_admin_logs_in_with_the_file_must_change_it_and_the_file_goes_away() {
    let e = env().await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("conf").join("initial-admin-password");
    assert!(
        e.auth
            .ensure_initial_admin(file.clone())
            .await
            .unwrap()
            .is_some()
    );
    let pw = std::fs::read_to_string(&file).unwrap().trim().to_string();
    let c = reqwest::Client::builder()
        .cookie_store(true)
        .build()
        .unwrap();
    // the wrong password is the same 401 as any other
    let r = c
        .post(e.url("/api/v1/auth/login"))
        .json(&json!({"username": "admin", "password": "not the password"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let r = c
        .post(e.url("/api/v1/auth/login"))
        .json(&json!({"username": "admin", "password": pw}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["user"]["must_change_password"], true);
    let csrf = v["csrf"].as_str().unwrap().to_string();
    // nothing but the password change works yet, and the file is still there
    for p in [
        "/api/v1/faxes",
        "/api/v1/users",
        "/api/v1/status",
        "/api/v1/config",
    ] {
        let (s, b) = status_of(c.get(e.url(p)).send().await.unwrap()).await;
        assert_eq!(
            (s, b["error"]["code"].as_str()),
            (403, Some("password_change_required")),
            "{p}"
        );
    }
    assert!(file.is_file());
    // a refused change (a rule broken) keeps both
    let r = c
        .post(e.url("/api/v1/auth/password"))
        .header("X-Sipfax-Csrf", &csrf)
        .json(&json!({"current": pw, "new": "short"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 422);
    assert!(file.is_file(), "a refused change keeps the file");
    // the change: the file is deleted, the session works for everything
    let r = c
        .post(e.url("/api/v1/auth/password"))
        .header("X-Sipfax-Csrf", &csrf)
        .json(&json!({"current": pw, "new": "my own new passphrase"}))
        .send()
        .await
        .unwrap();
    let (s, b) = status_of(r).await;
    assert_eq!(s, 200);
    assert_eq!(b["user"]["must_change_password"], false);
    assert!(!file.exists(), "the initial password file is deleted");
    for p in ["/api/v1/faxes", "/api/v1/users", "/api/v1/status"] {
        assert_eq!(c.get(e.url(p)).send().await.unwrap().status(), 200, "{p}");
    }
    // the old password (and so the file's content) no longer logs in
    let r = reqwest::Client::new()
        .post(e.url("/api/v1/auth/login"))
        .json(&json!({"username": "admin", "password": pw}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    e.stop().await;
}
