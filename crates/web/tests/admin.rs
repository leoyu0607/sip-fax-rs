//! M4b: `/config`, `/users`, `/status` (W09), and the request deadline (W11).

mod common;

use std::time::Duration;

use accounts::Role;
use common::*;
use reqwest::Method;
use serde_json::{Value, json};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

async fn admin_env() -> (Env, reqwest::Client, String) {
    let e = env().await;
    add_user(&e, "root", Role::Admin, false).await;
    let (c, csrf) = login(&e, "root").await;
    (e, c, csrf)
}

async fn body(r: reqwest::Response) -> (u16, Value) {
    let s = r.status().as_u16();
    let t = r.text().await.unwrap();
    (s, serde_json::from_str(&t).unwrap_or(Value::String(t)))
}

// ------------------------------------------------------------------------------ config

#[tokio::test(flavor = "multi_thread")]
async fn config_list_and_one_section() {
    let (e, c, _) = admin_env().await;
    let (s, v) = body(c.get(e.url("/api/v1/config")).send().await.unwrap()).await;
    assert_eq!(s, 200, "{v}");
    let names: Vec<&str> = v["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["name"].as_str().unwrap())
        .collect();
    for n in [
        "sip",
        "media",
        "call",
        "capture",
        "delivery",
        "retention",
        "routes",
    ] {
        assert!(names.contains(&n), "{n} in {names:?}");
    }
    assert_eq!(v["status"]["source"], "database");
    assert_eq!(v["status"]["pending_restart"], json!([]));

    let (s, v) = body(c.get(e.url("/api/v1/config/media")).send().await.unwrap()).await;
    assert_eq!(s, 200);
    assert_eq!(v["name"], "media");
    let pr = v["body"]["port_range"].as_str().expect("a string");
    let (lo, hi) = pr.split_once('-').expect("low-high");
    assert!(
        lo.parse::<u16>().unwrap() < hi.parse::<u16>().unwrap(),
        "{pr}"
    );

    let (s, v) = body(
        c.get(e.url("/api/v1/config/nonsense"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!((s, v["error"]["code"].as_str()), (404, Some("not_found")));
    let (s, _) = body(
        c.get(e.url("/api/v1/config/trunk:absent"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, 404);
    e.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn config_update_validation_conflict_and_restart_flag() {
    let (e, c, csrf) = admin_env().await;
    let put = |section: &str, b: Value| {
        change(
            &c,
            Method::PUT,
            e.url(&format!("/api/v1/config/{section}")),
            &csrf,
        )
        .json(&b)
    };
    let cur: Value = c
        .get(e.url("/api/v1/config/media"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let v0 = cur["version"].as_i64().unwrap();

    // K05: 422, field by field, nothing stored
    let (s, v) = body(
        put(
            "media",
            json!({"version": v0, "body": {"port_range": "31000-30000"}}),
        )
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(s, 422, "{v}");
    assert_eq!(v["error"]["code"], "invalid");
    assert_eq!(
        v["error"]["fields"]["port_range"]["code"], "reversed",
        "{v}"
    );
    let again: Value = c
        .get(e.url("/api/v1/config/media"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(again["version"], cur["version"]);

    // K08: a change that needs a restart says so, and /status lists it
    let (s, v) = body(
        put(
            "media",
            json!({"version": v0, "body": {"port_range": "32000-32999"}}),
        )
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    let v1 = v["version"].as_i64().unwrap();
    assert!(v1 > v0);
    assert!(
        v["restart_required"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x.as_str().unwrap().contains("port_range")),
        "{v}"
    );
    let (_, st) = body(c.get(e.url("/api/v1/status")).send().await.unwrap()).await;
    assert!(
        st["config"]["pending_restart"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x.as_str().unwrap().contains("port_range")),
        "{st}"
    );

    // K06: the stale version loses with 409 and the current version
    let (s, v) = body(
        put(
            "media",
            json!({"version": v0, "body": {"port_range": "33000-33999"}}),
        )
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(s, 409, "{v}");
    assert_eq!(v["error"]["code"], "conflict");
    assert_eq!(v["error"]["fields"]["current_version"], v1);

    // the audit trail names the account
    let (s, v) = body(c.get(e.url("/api/v1/config/audit")).send().await.unwrap()).await;
    assert_eq!(s, 200);
    let first = &v["entries"][0];
    assert_eq!(first["section"], "media");
    assert_eq!(first["by"], "root");
    assert_eq!(first["after"]["port_range"], "32000-32999", "{first}");

    // a body that is not the expected shape
    let (s, v) = body(put("media", json!({"body": {}})).send().await.unwrap()).await;
    assert_eq!((s, v["error"]["code"].as_str()), (400, Some("bad_request")));
    e.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn config_audit_pages_with_next() {
    let (e, c, csrf) = admin_env().await;
    // (retention is stored from the start: migration 0002 moved the M3 values into it)
    let cur: Value = c
        .get(e.url("/api/v1/config/retention"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut version = cur["version"].as_i64().unwrap();
    for d in [10, 11, 12] {
        let r: Value = change(&c, Method::PUT, e.url("/api/v1/config/retention"), &csrf)
            .json(&json!({"version": version, "body": {"files_days": d, "records_days": 400}}))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        version = r["version"].as_i64().unwrap_or_else(|| panic!("{r}"));
    }
    let (_, p1) = body(
        c.get(e.url("/api/v1/config/audit?limit=2"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(p1["entries"].as_array().unwrap().len(), 2);
    let next = p1["next"].as_i64().unwrap();
    let (_, p2) = body(
        c.get(e.url(&format!("/api/v1/config/audit?limit=2&before={next}")))
            .send()
            .await
            .unwrap(),
    )
    .await;
    let ids: Vec<i64> = p2["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x["id"].as_i64().unwrap())
        .collect();
    assert!(ids.iter().all(|i| *i < next), "{ids:?} < {next}");
    let (s, _) = body(
        c.get(e.url("/api/v1/config/audit?limit=x"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, 400);
    e.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn delete_needs_a_trunk_and_a_version() {
    let (e, c, csrf) = admin_env().await;
    let (s, v) = body(
        change(
            &c,
            Method::DELETE,
            e.url("/api/v1/config/sip?version=0"),
            &csrf,
        )
        .header("content-type", "application/json")
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (405, Some("method_not_allowed"))
    );
    let (s, _) = body(
        change(&c, Method::DELETE, e.url("/api/v1/config/trunk:gw"), &csrf)
            .header("content-type", "application/json")
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, 400);
    let (s, _) = body(
        change(
            &c,
            Method::DELETE,
            e.url("/api/v1/config/trunk:gw?version=1"),
            &csrf,
        )
        .header("content-type", "application/json")
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(s, 404);
    // reorder with names that are not the trunks
    let (s, v) = body(
        change(&c, Method::PUT, e.url("/api/v1/config/trunks/order"), &csrf)
            .json(&json!({"order": ["nope"]}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, 422, "{v}");
    e.stop().await;
}

// ------------------------------------------------------------------------------ users

#[tokio::test(flavor = "multi_thread")]
async fn users_create_change_reset_delete() {
    let (e, c, csrf) = admin_env().await;
    let (s, v) = body(
        change(&c, Method::POST, e.url("/api/v1/users"), &csrf)
            .json(&json!({"username": "amy", "role": "agent", "password": PW}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, 201, "{v}");
    let id = v["user"]["id"].as_i64().unwrap();
    assert_eq!(v["user"]["must_change_password"], true);
    assert!(v["user"].get("password_hash").is_none());

    // duplicate, bad role, weak password
    for (b, status, code) in [
        (
            json!({"username": "amy", "role": "agent", "password": PW}),
            409,
            "username_taken",
        ),
        (
            json!({"username": "bob", "role": "boss", "password": PW}),
            422,
            "bad_role",
        ),
        (
            json!({"username": "bob", "role": "agent", "password": "short"}),
            422,
            "",
        ),
    ] {
        let (s, v) = body(
            change(&c, Method::POST, e.url("/api/v1/users"), &csrf)
                .json(&b)
                .send()
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(s, status, "{b} -> {v}");
        if !code.is_empty() {
            assert_eq!(v["error"]["code"], code, "{v}");
        }
    }

    // change: display name set, then cleared with null
    let (s, v) = body(
        change(
            &c,
            Method::PUT,
            e.url(&format!("/api/v1/users/{id}")),
            &csrf,
        )
        .json(&json!({"display_name": "Amy", "locale": "en"}))
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["user"]["display_name"], "Amy");
    let (_, v) = body(
        change(
            &c,
            Method::PUT,
            e.url(&format!("/api/v1/users/{id}")),
            &csrf,
        )
        .json(&json!({"display_name": null}))
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert!(v["user"]["display_name"].is_null(), "{v}");
    assert_eq!(v["user"]["locale"], "en");

    // a password reset by the administrator: the new one logs in, the account must change it
    let (s, v) = body(
        change(
            &c,
            Method::PUT,
            e.url(&format!("/api/v1/users/{id}")),
            &csrf,
        )
        .json(&json!({"password": "another fine password"}))
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["user"]["must_change_password"], true);
    let r = reqwest::Client::new()
        .post(e.url("/api/v1/auth/login"))
        .json(&json!({"username": "amy", "password": "another fine password"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    let (s, _) = body(
        change(
            &c,
            Method::PUT,
            e.url(&format!("/api/v1/users/{id}")),
            &csrf,
        )
        .json(&json!({"must_change_password": false}))
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(s, 400);

    // list, then delete
    let (_, v) = body(c.get(e.url("/api/v1/users")).send().await.unwrap()).await;
    assert_eq!(v["users"].as_array().unwrap().len(), 2);
    let r = change(
        &c,
        Method::DELETE,
        e.url(&format!("/api/v1/users/{id}")),
        &csrf,
    )
    .header("content-type", "application/json")
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 204);
    for bad in [
        id.to_string(),
        "0".into(),
        "x".into(),
        "99999999999999999999".into(),
    ] {
        let (s, _) = body(
            change(
                &c,
                Method::PUT,
                e.url(&format!("/api/v1/users/{bad}")),
                &csrf,
            )
            .json(&json!({"disabled": true}))
            .send()
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(s, 404, "{bad}");
    }
    e.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a12_the_last_admin_stays() {
    let (e, c, csrf) = admin_env().await;
    let me: Value = c
        .get(e.url("/api/v1/auth/me"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = me["user"]["id"].as_i64().unwrap();
    for b in [json!({"disabled": true}), json!({"role": "agent"})] {
        let (s, v) = body(
            change(
                &c,
                Method::PUT,
                e.url(&format!("/api/v1/users/{id}")),
                &csrf,
            )
            .json(&b)
            .send()
            .await
            .unwrap(),
        )
        .await;
        assert_eq!(
            (s, v["error"]["code"].as_str()),
            (409, Some("last_admin")),
            "{b}"
        );
    }
    let (s, _) = body(
        change(
            &c,
            Method::DELETE,
            e.url(&format!("/api/v1/users/{id}")),
            &csrf,
        )
        .header("content-type", "application/json")
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(s, 409);
    // still an administrator, still logged in
    let r = c.get(e.url("/api/v1/users")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    e.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn disabling_an_account_ends_its_sessions() {
    let (e, c, csrf) = admin_env().await;
    add_user(&e, "amy", Role::Agent, false).await;
    let (amy, _) = login(&e, "amy").await;
    assert_eq!(
        amy.get(e.url("/api/v1/faxes"))
            .send()
            .await
            .unwrap()
            .status(),
        200
    );
    let users: Value = c
        .get(e.url("/api/v1/users"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let id = users["users"]
        .as_array()
        .unwrap()
        .iter()
        .find(|u| u["username"] == "amy")
        .unwrap()["id"]
        .as_i64()
        .unwrap();
    let r = change(
        &c,
        Method::PUT,
        e.url(&format!("/api/v1/users/{id}")),
        &csrf,
    )
    .json(&json!({"disabled": true}))
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    assert_eq!(
        amy.get(e.url("/api/v1/faxes"))
            .send()
            .await
            .unwrap()
            .status(),
        401
    );
    e.stop().await;
}

// ------------------------------------------------------------------------------ permissions

#[tokio::test(flavor = "multi_thread")]
async fn agents_and_strangers_are_refused_and_nothing_changes() {
    let e = env().await;
    add_user(&e, "amy", Role::Agent, false).await;
    let (amy, csrf) = login(&e, "amy").await;
    let anon = reqwest::Client::new();
    let calls: Vec<(Method, &str, Option<Value>)> = vec![
        (Method::GET, "/api/v1/config", None),
        (Method::GET, "/api/v1/config/sip", None),
        (Method::GET, "/api/v1/config/audit", None),
        (
            Method::PUT,
            "/api/v1/config/call",
            Some(json!({"version": 0, "body": {"max_duration_secs": 99}})),
        ),
        (Method::DELETE, "/api/v1/config/trunk:gw?version=1", None),
        (
            Method::PUT,
            "/api/v1/config/trunks/order",
            Some(json!({"order": []})),
        ),
        (Method::GET, "/api/v1/users", None),
        (
            Method::POST,
            "/api/v1/users",
            Some(json!({"username": "eve", "role": "admin", "password": PW})),
        ),
        (
            Method::PUT,
            "/api/v1/users/1",
            Some(json!({"role": "admin"})),
        ),
        (Method::DELETE, "/api/v1/users/1", None),
    ];
    for (m, path, b) in &calls {
        for (who, client, expect) in [("agent", &amy, 403), ("nobody", &anon, 401)] {
            let mut rq = change(client, m.clone(), e.url(path), &csrf)
                .header("content-type", "application/json");
            if let Some(b) = b {
                rq = rq.json(b);
            }
            let (s, v) = body(rq.send().await.unwrap()).await;
            assert_eq!(s, expect, "{who} {m} {path}: {v}");
            let code = if who == "agent" {
                "forbidden"
            } else {
                "unauthenticated"
            };
            assert_eq!(v["error"]["code"], code, "{who} {m} {path}: {v}");
        }
    }
    // the database is as it was
    let users = e.auth.list_users().await.unwrap();
    assert_eq!(users.len(), 1);
    assert_eq!(users[0].role, Role::Agent);
    assert_eq!(e.config.current().call.max_duration_secs, {
        runtime_config::RuntimeConfig::default()
            .call
            .max_duration_secs
    });
    // an agent may read the status, without the key details
    let (s, v) = body(amy.get(e.url("/api/v1/status")).send().await.unwrap()).await;
    assert_eq!(s, 200, "{v}");
    assert!(v["config"].get("key").is_none(), "{v}");
    let (s, _) = body(anon.get(e.url("/api/v1/status")).send().await.unwrap()).await;
    assert_eq!(s, 401);
    e.stop().await;
}

// ------------------------------------------------------------------------------ W09

#[tokio::test(flavor = "multi_thread")]
async fn w09_status_reports_service_database_and_config() {
    let (e, c, _) = admin_env().await;
    e.status.set(|s| {
        s.active_calls = 2;
        s.trunks[0].registration = "registered".into();
    });
    let (s, v) = body(c.get(e.url("/api/v1/status")).send().await.unwrap()).await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["service"]["version"], "0.0.0-test");
    assert_eq!(v["service"]["active_calls"], 2);
    assert_eq!(v["service"]["trunks"][0]["registration"], "registered");
    assert_eq!(v["database"]["state"], "ok");
    assert!(v["database"]["schema"].as_i64().unwrap() >= 2);
    assert_eq!(v["config"]["source"], "database");
    assert_eq!(v["config"]["key"], json!(e.config.status().key));
    assert_eq!(v["config"]["pending_restart"], json!([]));
    e.stop().await;
}

// ------------------------------------------------------------------------------ W11

#[tokio::test(flavor = "multi_thread")]
async fn w11_a_slow_body_gets_408() {
    let e = env_with(Duration::from_millis(400)).await;
    let addr = e.base.trim_start_matches("http://").to_string();
    let mut s = tokio::net::TcpStream::connect(&addr).await.unwrap();
    // a body that never completes
    s.write_all(
        b"POST /api/v1/auth/login HTTP/1.1\r\nHost: x\r\nContent-Type: application/json\r\n\
          Content-Length: 100\r\n\r\n{\"user",
    )
    .await
    .unwrap();
    let mut buf = Vec::new();
    let read = tokio::time::timeout(Duration::from_secs(5), async {
        let mut chunk = [0u8; 1024];
        loop {
            let n = s.read(&mut chunk).await.unwrap();
            if n == 0 {
                break;
            }
            buf.extend_from_slice(&chunk[..n]);
            if buf.windows(4).any(|w| w == b"\r\n\r\n") && buf.ends_with(b"}}") {
                break;
            }
        }
    })
    .await;
    assert!(read.is_ok(), "the server answered");
    let text = String::from_utf8_lossy(&buf);
    assert!(text.starts_with("HTTP/1.1 408"), "{text}");
    assert!(text.contains("\"timeout\""), "{text}");
    e.stop().await;
}
