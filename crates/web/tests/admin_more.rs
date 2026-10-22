//! More of W09 and the config routes over HTTP: the life of a trunk (create, order, delete) with
//! the audit trail, and what `/status` and the fax routes say while the database is down (the
//! outage is injected in front of the real store, as `runtime-config` does for K12).

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use accounts::Role;
use async_trait::async_trait;
use common::*;
use reqwest::Method;
use serde_json::{Value, json};
use store::{FaxStore, StoreSource};

const SEED: &str = r#"
[[trunks]]
name = "alpha"
peer = "192.0.2.10:5060"

[[trunks]]
name = "beta"
peer = "192.0.2.11:5060"
"#;

async fn body(r: reqwest::Response) -> (u16, Value) {
    let s = r.status().as_u16();
    (s, r.json().await.unwrap_or(Value::Null))
}

#[tokio::test(flavor = "multi_thread")]
async fn config_a_trunk_is_created_ordered_and_deleted_and_the_audit_trail_shows_who() {
    let e = env_seeded(SEED).await;
    add_user(&e, "root", Role::Admin, false).await;
    let (c, csrf) = login(&e, "root").await;
    let names = |e: &Env| -> Vec<String> {
        e.config
            .current()
            .trunks
            .iter()
            .map(|t| t.name.clone())
            .collect()
    };
    assert_eq!(names(&e), ["alpha", "beta"]);
    // a new trunk: the body of an existing one under another name, version 0
    let alpha: Value = c
        .get(e.url("/api/v1/config/trunk:alpha"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut b = alpha["body"].clone();
    b["name"] = json!("gamma");
    b["peer"] = json!("192.0.2.12:5060");
    let (s, v) = body(
        change(&c, Method::PUT, e.url("/api/v1/config/trunk:gamma"), &csrf)
            .json(&json!({"version": 0, "body": b}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(names(&e), ["alpha", "beta", "gamma"]);
    // the same name again with version 0 is a conflict, not a second trunk
    let (s, v) = body(
        change(&c, Method::PUT, e.url("/api/v1/config/trunk:gamma"), &csrf)
            .json(&json!({"version": 0, "body": b}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (409, Some("conflict")),
        "{v}"
    );
    assert_eq!(names(&e).len(), 3);
    // the order decides which trunk matches first
    let (s, v) = body(
        change(&c, Method::PUT, e.url("/api/v1/config/trunks/order"), &csrf)
            .json(&json!({"order": ["gamma", "alpha", "beta"]}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(names(&e), ["gamma", "alpha", "beta"]);
    // a list that leaves one out, or repeats one, is refused and the order is kept
    for bad in [
        json!(["gamma", "alpha"]),
        json!(["gamma", "gamma", "alpha", "beta"]),
    ] {
        let (s, _) = body(
            change(&c, Method::PUT, e.url("/api/v1/config/trunks/order"), &csrf)
                .json(&json!({"order": bad}))
                .send()
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(s, 422, "{bad}");
    }
    assert_eq!(names(&e), ["gamma", "alpha", "beta"]);
    // delete with the version that was read
    let gamma: Value = c
        .get(e.url("/api/v1/config/trunk:gamma"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ver = gamma["version"].as_i64().unwrap();
    let (s, v) = body(
        change(
            &c,
            Method::DELETE,
            e.url(&format!("/api/v1/config/trunk:gamma?version={}", ver + 5)),
            &csrf,
        )
        .header("content-type", "application/json")
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(s, 409, "{v}");
    assert_eq!(names(&e).len(), 3, "a stale delete deletes nothing");
    let (s, v) = body(
        change(
            &c,
            Method::DELETE,
            e.url(&format!("/api/v1/config/trunk:gamma?version={ver}")),
            &csrf,
        )
        .header("content-type", "application/json")
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(names(&e), ["alpha", "beta"]);
    let (s, _) = body(
        c.get(e.url("/api/v1/config/trunk:gamma"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, 404);
    // the audit trail has all of it, newest first, each by root
    let (s, v) = body(
        c.get(e.url("/api/v1/config/audit?limit=20"))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, 200);
    let rows: Vec<(String, String, String)> = v["entries"]
        .as_array()
        .unwrap()
        .iter()
        .map(|r| {
            (
                r["section"].as_str().unwrap().to_string(),
                r["action"].as_str().unwrap().to_string(),
                r["by"].as_str().unwrap().to_string(),
            )
        })
        .collect();
    // (a delete also rewrites the order list: that row comes after the delete's own)
    assert!(
        rows[..2]
            .iter()
            .any(|r| r.0 == "trunk:gamma" && r.1 == "delete"),
        "{rows:?}"
    );
    assert!(
        rows.iter().filter(|r| r.2 == "root").count() >= 3,
        "{rows:?}"
    );
    assert!(
        rows.iter().any(|r| r.0 == "trunk:gamma" && r.1 != "delete"),
        "the creation is there: {rows:?}"
    );
    e.stop().await;
}

/// A store source that can be told to be unreachable; when it is up it is the real store.
struct Outage {
    inner: Arc<dyn FaxStore>,
    down: AtomicBool,
}

#[async_trait]
impl StoreSource for Outage {
    async fn store(&self) -> store::Result<Arc<dyn FaxStore>> {
        if self.down.load(Ordering::SeqCst) {
            Err(store::Error::Unavailable(
                "tcp connect to 10.9.8.7:5432 refused".into(),
            ))
        } else {
            Ok(self.inner.clone())
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn w09_status_says_the_database_is_unavailable_and_the_fax_routes_are_503_without_details() {
    let e = env().await;
    add_user(&e, "root", Role::Admin, false).await;
    let (c, csrf) = login(&e, "root").await;
    let id = fax(&e, "dn@gw", "02", 5).await;
    let outage = Arc::new(Outage {
        inner: e.store.clone(),
        down: AtomicBool::new(false),
    });
    // a second server over the same state, whose fax store can go away (sessions are the
    // accounts' own connection: the administrator stays logged in)
    let mut state = e.state.clone();
    state.store = outage.clone();
    let srv = web::server::WebServer::start("127.0.0.1:0".parse().unwrap(), web::app(state), None)
        .await
        .unwrap();
    let base = format!("http://{}", srv.local_addr());
    // the cookie of the first server is valid here too (same sessions); the host differs by
    // port only, and cookies are not port specific
    let get = |p: &str| c.get(format!("{base}{p}")).send();
    let (s, v) = body(get("/api/v1/status").await.unwrap()).await;
    assert_eq!(
        (s, v["database"]["state"].as_str()),
        (200, Some("ok")),
        "{v}"
    );
    let (s, _) = body(get(&format!("/api/v1/faxes/{id}")).await.unwrap()).await;
    assert_eq!(s, 200);
    outage.down.store(true, Ordering::SeqCst);
    let (s, v) = body(get("/api/v1/status").await.unwrap()).await;
    assert_eq!(s, 200, "the status answers while the database is down: {v}");
    assert_eq!(v["database"]["state"], "unavailable");
    assert!(v["database"].get("schema").is_none());
    assert_eq!(
        v["service"]["state"], "running",
        "the service itself is fine"
    );
    let text = v.to_string();
    assert!(
        !text.contains("10.9.8.7") && !text.contains("5432"),
        "{text}"
    );
    for p in [
        "/api/v1/faxes".to_string(),
        format!("/api/v1/faxes/{id}"),
        format!("/api/v1/faxes/{id}/pages/1.png"),
        format!("/api/v1/faxes/{id}/file.pdf"),
    ] {
        let r = get(&p).await.unwrap();
        let status = r.status().as_u16();
        let t = r.text().await.unwrap();
        assert_eq!(status, 503, "{p}: {t}");
        assert!(
            t.contains("\"unavailable\"") && !t.contains("10.9.8.7"),
            "{p}: {t}"
        );
    }
    let r = c
        .put(format!("{base}/api/v1/faxes/{id}/read"))
        .header("X-Sipfax-Csrf", &csrf)
        .json(&json!({"read": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 503);
    // the outage over: everything is back, and nothing was changed by the refused write
    outage.down.store(false, Ordering::SeqCst);
    assert!(!e.store.get(id).await.unwrap().unwrap().is_read);
    let (s, v) = body(get("/api/v1/status").await.unwrap()).await;
    assert_eq!((s, v["database"]["state"].as_str()), (200, Some("ok")));
    let (s, _) = body(get(&format!("/api/v1/faxes/{id}")).await.unwrap()).await;
    assert_eq!(s, 200);
    srv.shutdown(std::time::Duration::from_secs(1)).await;
    e.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn w09_the_status_follows_the_service_and_lists_what_waits_for_a_restart() {
    let e = env().await;
    add_user(&e, "root", Role::Admin, false).await;
    add_user(&e, "amy", Role::Agent, false).await;
    let (c, csrf) = login(&e, "root").await;
    let (agent, _) = login(&e, "amy").await;
    let (_, v) = body(c.get(e.url("/api/v1/status")).send().await.unwrap()).await;
    assert_eq!(v["config"]["pending_restart"], json!([]));
    assert_eq!(v["service"]["state"], "running");
    assert_eq!(v["service"]["listen"][0], "udp 127.0.0.1:5060");
    // the service starts to stop and a trunk fails to register
    e.status.set(|s| {
        s.state = "stopping".into();
        s.active_calls = 3;
        s.trunks[0].registration = "failed: 403 Forbidden".into();
    });
    // a setting that needs a restart is saved: it shows for both roles
    let cur: Value = c
        .get(e.url("/api/v1/config/sip"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let mut b = cur["body"].clone();
    b["bind"] = json!("127.0.0.1:5099");
    let (s, v) = body(
        change(&c, Method::PUT, e.url("/api/v1/config/sip"), &csrf)
            .json(&json!({"version": cur["version"], "body": b}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert!(
        v["restart_required"].to_string().contains("bind"),
        "the answer says which field: {v}"
    );
    for client in [&c, &agent] {
        let (s, v) = body(client.get(e.url("/api/v1/status")).send().await.unwrap()).await;
        assert_eq!(s, 200);
        assert_eq!(v["service"]["state"], "stopping");
        assert_eq!(v["service"]["active_calls"], 3);
        assert_eq!(
            v["service"]["trunks"][0]["registration"],
            "failed: 403 Forbidden"
        );
        assert!(
            v["config"]["pending_restart"].to_string().contains("bind"),
            "{v}"
        );
    }
    // the agent does not see the key file or the undecryptable secrets
    let (_, v) = body(agent.get(e.url("/api/v1/status")).send().await.unwrap()).await;
    assert!(v["config"].get("key").is_none() && v["config"].get("undecryptable").is_none());
    let (_, v) = body(c.get(e.url("/api/v1/status")).send().await.unwrap()).await;
    assert!(v["config"].get("key").is_some() && v["config"]["undecryptable"].is_array());
    e.stop().await;
}

// ------------------------------------------------------------------------------ A12

#[tokio::test(flavor = "multi_thread")]
async fn a12_two_admins_may_step_down_one_at_a_time_and_a_refused_request_changes_nothing() {
    let e = env().await;
    add_user(&e, "root", Role::Admin, false).await;
    add_user(&e, "boss", Role::Admin, false).await;
    let (root, rcsrf) = login(&e, "root").await;
    let (boss, _) = login(&e, "boss").await;
    let id_of = |n: &'static str| {
        let a = e.auth.clone();
        async move { a.get_user(n).await.unwrap().id }
    };
    let (root_id, boss_id) = (id_of("root").await, id_of("boss").await);
    // with two administrators, one may be demoted ...
    let (s, v) = body(
        change(
            &root,
            Method::PUT,
            e.url(&format!("/api/v1/users/{boss_id}")),
            &rcsrf,
        )
        .json(&json!({"role": "agent"}))
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(s, 200, "{v}");
    assert_eq!(v["user"]["role"], "agent");
    // ... and now root is the last one: disabling, demoting and deleting are 409, also when the
    // same request carries a password reset or other changes, and nothing of it is applied
    let before = format!("{:?}", e.auth.list_users().await.unwrap());
    for b in [
        json!({"disabled": true}),
        json!({"role": "agent"}),
        json!({"disabled": true, "password": "a brand new password"}),
        json!({"role": "agent", "password": "a brand new password", "display_name": "Nobody"}),
    ] {
        let (s, v) = body(
            change(
                &root,
                Method::PUT,
                e.url(&format!("/api/v1/users/{root_id}")),
                &rcsrf,
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
        assert_eq!(
            format!("{:?}", e.auth.list_users().await.unwrap()),
            before,
            "{b}: the account was changed by a refused request"
        );
        // the session and the old password are as they were
        assert_eq!(
            root.get(e.url("/api/v1/auth/me"))
                .send()
                .await
                .unwrap()
                .status(),
            200,
            "{b}: the sessions of the last admin ended"
        );
        let r = reqwest::Client::new()
            .post(e.url("/api/v1/auth/login"))
            .json(&json!({"username": "root", "password": PW}))
            .send()
            .await
            .unwrap();
        assert_eq!(r.status(), 200, "{b}: the password was reset");
    }
    // the agent that was demoted can no longer do what an administrator does
    let (s, _) = body(boss.get(e.url("/api/v1/users")).send().await.unwrap()).await;
    assert_eq!(s, 401, "the sessions of a demoted account ended");
    let (boss, _) = login(&e, "boss").await;
    assert_eq!(
        boss.get(e.url("/api/v1/users"))
            .send()
            .await
            .unwrap()
            .status(),
        403
    );
    // promoting the agent again makes the first one free to go
    let (s, _) = body(
        change(
            &root,
            Method::PUT,
            e.url(&format!("/api/v1/users/{boss_id}")),
            &rcsrf,
        )
        .json(&json!({"role": "admin"}))
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(s, 200);
    let (s, _) = body(
        change(
            &root,
            Method::DELETE,
            e.url(&format!("/api/v1/users/{root_id}")),
            &rcsrf,
        )
        .header("content-type", "application/json")
        .send()
        .await
        .unwrap(),
    )
    .await;
    assert_eq!(s, 204, "deleted");
    e.stop().await;
}
