//! M6a (ADR-070) development check: an API key made by an administrator reads the faxes,
//! follows the change feed and stops working once revoked. The full cases (K, C) are in the
//! M6a test plan.

mod common;

use accounts::Role;
use common::*;
use reqwest::Method;
use serde_json::{Value, json};

#[tokio::test]
async fn a_key_reads_the_faxes_follows_the_changes_and_stops_when_revoked() {
    let e = env().await;
    add_user(&e, "boss", Role::Admin, false).await;
    let (c, csrf) = login(&e, "boss").await;
    let a = fax(&e, "call-a", "0911", 30).await;
    let b = fax(&e, "call-b", "0922", 20).await;
    // the change feed leaves out the last 2 s
    e.clock.advance(chrono::Duration::seconds(3));

    let r = change(&c, Method::POST, e.url("/api/v1/api-keys"), &csrf)
        .json(&json!({"name": "ERP", "write": false}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 201);
    let v: Value = r.json().await.unwrap();
    let key = v["key"].as_str().unwrap().to_string();
    assert!(key.starts_with("sfx_") && key.len() == 47, "{key}");
    assert_eq!(v["api_key"]["name"], "erp");
    assert_eq!(v["api_key"]["scopes"], json!(["fax:read"]));
    let id = v["api_key"]["id"].as_i64().unwrap();

    let api = reqwest::Client::new();
    let get = |path: &str| {
        api.get(e.url(path))
            .header("Authorization", format!("Bearer {key}"))
    };
    // the list, one fax (without deliveries), its PDF and a thumbnail
    let v: Value = get("/api/v1/faxes")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["items"].as_array().unwrap().len(), 2);
    let v: Value = get(&format!("/api/v1/faxes/{a}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["call_id"], "call-a");
    assert!(v.get("deliveries").is_none(), "{v}");
    let pdf = get(&format!("/api/v1/faxes/{a}/file.pdf"))
        .send()
        .await
        .unwrap();
    assert_eq!(pdf.status(), 200);
    assert_eq!(&pdf.bytes().await.unwrap()[..], b"%PDF-1.4 fax call-a");
    let png = get(&format!("/api/v1/faxes/{b}/pages/1.png?kind=thumb"))
        .send()
        .await
        .unwrap();
    assert_eq!(png.status(), 200);
    assert_eq!(&png.bytes().await.unwrap()[..8], b"\x89PNG\r\n\x1a\n");

    // the change feed: both, oldest change first; then nothing, with the same position
    let v: Value = get("/api/v1/faxes/changes?limit=1")
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["items"][0]["id"], a);
    let next = v["next"].as_str().unwrap().to_string();
    let v: Value = get(&format!("/api/v1/faxes/changes?after={next}"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(v["items"].as_array().unwrap().len(), 1);
    assert_eq!(v["items"][0]["id"], b);
    let last = v["next"].as_str().unwrap().to_string();
    // a change made with a session comes again after the settling time
    let r = change(
        &c,
        Method::PUT,
        e.url(&format!("/api/v1/faxes/{a}/read")),
        &csrf,
    )
    .json(&json!({"read": true}))
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    let feed = |after: String| get(&format!("/api/v1/faxes/changes?after={after}")).send();
    let v: Value = feed(last.clone()).await.unwrap().json().await.unwrap();
    assert!(v["items"].as_array().unwrap().is_empty(), "not settled yet");
    e.clock.advance(chrono::Duration::seconds(3));
    let v: Value = feed(last).await.unwrap().json().await.unwrap();
    assert_eq!(v["items"][0]["id"], a);
    assert_eq!(v["items"][0]["is_read"], true);

    // read only: no read flag; and nothing outside the faxes
    let r = api
        .put(e.url(&format!("/api/v1/faxes/{b}/read")))
        .header("Authorization", format!("Bearer {key}"))
        .json(&json!({"read": true}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    for path in [
        "/api/v1/users",
        "/api/v1/config",
        "/api/v1/auth/me",
        "/api/v1/api-keys",
    ] {
        assert_eq!(get(path).send().await.unwrap().status(), 401, "{path}");
    }

    // revoked: refused at once, like a key that never existed
    let r = change(
        &c,
        Method::POST,
        e.url(&format!("/api/v1/api-keys/{id}/revoke")),
        &csrf,
    )
    .json(&json!({}))
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    let r = get("/api/v1/faxes").send().await.unwrap();
    assert_eq!(r.status(), 401);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["error"]["code"], "invalid_api_key");
    e.stop().await;
}
