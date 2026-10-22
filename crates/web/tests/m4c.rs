//! M4c backend cases over HTTP: W12 (`q` search), W13 (`restart_fields`), A14 (the account's
//! language) and U05 (the embedded interface).

mod common;

use accounts::Role;
use common::{PW, add_user, change, env, fax, login};
use reqwest::Method;
use serde_json::{Value, json};

async fn body(r: reqwest::Response) -> (u16, Value) {
    let s = r.status().as_u16();
    let t = r.text().await.unwrap();
    (s, serde_json::from_str(&t).unwrap_or(Value::String(t)))
}

// ------------------------------------------------------------------------------ W12

fn ids(v: &Value) -> Vec<i64> {
    v["items"]
        .as_array()
        .unwrap_or_else(|| panic!("no items: {v}"))
        .iter()
        .map(|f| f["id"].as_i64().unwrap())
        .collect()
}

#[tokio::test]
async fn w12_q_matches_anywhere_ignoring_case_takes_wildcards_literally_and_ands_with_filters() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    let (c, csrf) = login(&e, "alice").await;
    // the number goes into the ANI; every fax has DNIS 2557 and TSI 0000000001
    let under = fax(&e, "a@gw", "0911_11", 50).await; // an underscore
    let plain = fax(&e, "b@gw", "0911x11", 40).await; // would match `_` as a wildcard
    let pct = fax(&e, "c@gw", "09%22", 30).await; // a percent sign
    let bracket = fax(&e, "d@gw", "[abc]33", 20).await; // a bracket
    let named = fax(&e, "e@gw", "Bob-Smith", 10).await; // letters, upper case
    let q = |text: &str| e.url(&format!("/api/v1/faxes?q={}", text.replace('%', "%25")));
    let list = |url: String| {
        let c = c.clone();
        async move { body(c.get(url).send().await.unwrap()).await }
    };

    // anywhere in the ANI, ignoring case
    for (text, want) in [
        ("bob", vec![named]),
        ("BOB-SMITH", vec![named]),
        ("ob-sm", vec![named]),
        ("0911", vec![plain, under]),
    ] {
        let (s, v) = list(q(text)).await;
        assert_eq!((s, ids(&v)), (200, want), "q={text}");
    }
    // the DNIS and the remote ident (TSI), in the middle of the value
    let (_, v) = list(q("55")).await;
    assert_eq!(ids(&v), [named, bracket, pct, plain, under], "DNIS 2557");
    let (_, v) = list(q("00000000")).await;
    assert_eq!(ids(&v).len(), 5, "TSI 0000000001");

    // `_`, `%` and `[` are only themselves
    for (text, want) in [
        ("_", vec![under]),
        ("1_1", vec![under]),
        ("%", vec![pct]),
        ("[", vec![bracket]),
        ("[abc]", vec![bracket]),
    ] {
        let (s, v) = list(q(text)).await;
        assert_eq!((s, ids(&v)), (200, want), "q={text}");
    }
    // nothing matches
    let (s, v) = list(q("zzz")).await;
    assert_eq!((s, ids(&v)), (200, vec![]));
    assert!(v["next_cursor"].is_null());

    // the note counts too
    let r = change(
        &c,
        Method::PUT,
        e.url(&format!("/api/v1/faxes/{named}/note")),
        &csrf,
    )
    .json(&json!({"text": "Call Back URGENT", "expected_updated_at": null}))
    .send()
    .await
    .unwrap();
    assert_eq!(r.status(), 200);
    let (_, v) = list(q("urgent")).await;
    assert_eq!(ids(&v), [named]);
    let (_, v) = list(q("k urg")).await;
    assert_eq!(ids(&v), [named], "across words of the note");

    // AND with the other filters
    change(
        &c,
        Method::PUT,
        e.url(&format!("/api/v1/faxes/{under}/read")),
        &csrf,
    )
    .json(&json!({"read": true}))
    .send()
    .await
    .unwrap();
    let (_, v) = list(e.url("/api/v1/faxes?q=11&ani=0911&is_read=false")).await;
    assert_eq!(ids(&v), [plain]);
    let (_, v) = list(e.url("/api/v1/faxes?q=11&ani=0911")).await;
    assert_eq!(ids(&v), [plain, under]);
    let (_, v) = list(e.url("/api/v1/faxes?q=11&ani=09%25")).await;
    assert_eq!(ids(&v), [] as [i64; 0], "no ANI starts with 09% (literal)");
    let (_, v) = list(e.url("/api/v1/faxes?q=0911&status=failed")).await;
    assert_eq!(ids(&v), [] as [i64; 0]);

    // the same answer as the store gives, and paging keeps the filter
    let direct = e
        .store
        .list(
            &store::ListFilter {
                q: Some("0911".into()),
                ..Default::default()
            },
            store::PageRequest::default(),
        )
        .await
        .unwrap();
    let (_, v) = list(q("0911")).await;
    assert_eq!(
        ids(&v),
        direct.items.iter().map(|r| r.id).collect::<Vec<_>>()
    );
    let (_, p1) = list(e.url("/api/v1/faxes?q=0911&limit=1")).await;
    assert_eq!(ids(&p1), [plain]);
    let cur = p1["next_cursor"].as_str().unwrap().to_string();
    let (_, p2) = list(e.url(&format!("/api/v1/faxes?q=0911&limit=1&cursor={cur}"))).await;
    assert_eq!(ids(&p2), [under]);
    assert!(p2["next_cursor"].is_null());
    e.stop().await;
}

#[tokio::test]
async fn w12_q_needs_a_session() {
    let e = env().await;
    let r = reqwest::get(e.url("/api/v1/faxes?q=1")).await.unwrap();
    let (s, v) = body(r).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (401, Some("unauthenticated"))
    );
    e.stop().await;
}

// ------------------------------------------------------------------------------ W13

#[tokio::test(flavor = "multi_thread")]
async fn w13_restart_fields_are_the_backends_list_and_a_restart_change_is_a_subset() {
    let e = env().await;
    add_user(&e, "root", Role::Admin, false).await;
    add_user(&e, "amy", Role::Agent, false).await;
    let (c, csrf) = login(&e, "root").await;
    let (agent, _) = login(&e, "amy").await;

    let (s, v) = body(c.get(e.url("/api/v1/config")).send().await.unwrap()).await;
    assert_eq!(s, 200, "{v}");
    let fields: Vec<&str> = v["restart_fields"]
        .as_array()
        .expect("restart_fields")
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert_eq!(fields, fax_in::config::RESTART_FIELDS);
    assert!(fields.contains(&"sip.bind") && fields.contains(&"media.port_range"));

    // changing sip.bind reports names out of that list, and sip.bind is among them
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
    let required: Vec<&str> = v["restart_required"]
        .as_array()
        .unwrap()
        .iter()
        .map(|x| x.as_str().unwrap())
        .collect();
    assert!(required.contains(&"sip.bind"), "{required:?}");
    for name in &required {
        assert!(fields.contains(name), "{name} is not in restart_fields");
    }

    // an agent does not get the configuration at all
    let r = agent.get(e.url("/api/v1/config")).send().await.unwrap();
    assert_eq!(r.status(), 403);
    e.stop().await;
}

// ------------------------------------------------------------------------------ A14

async fn locale_of(e: &common::Env, name: &str) -> Option<String> {
    e.auth
        .list_users()
        .await
        .unwrap()
        .into_iter()
        .find(|u| u.username == name)
        .unwrap()
        .locale
}

#[tokio::test]
async fn a14_the_account_sets_its_own_language_and_only_zh_tw_en_or_null() {
    let e = env().await;
    add_user(&e, "alice", Role::Agent, false).await;
    let (c, csrf) = login(&e, "alice").await;
    let put = |v: Value| change(&c, Method::PUT, e.url("/api/v1/auth/me"), &csrf).json(&v);
    let me = || async {
        let (_, v) = body(c.get(e.url("/api/v1/auth/me")).send().await.unwrap()).await;
        v["user"]["locale"].clone()
    };
    assert_eq!(me().await, Value::Null);

    for want in ["en", "zh-TW"] {
        let (s, v) = body(put(json!({"locale": want})).send().await.unwrap()).await;
        assert_eq!((s, v["user"]["locale"].as_str()), (200, Some(want)), "{v}");
        assert_eq!(me().await, json!(want));
        assert_eq!(locale_of(&e, "alice").await.as_deref(), Some(want));
    }
    let (s, v) = body(put(json!({"locale": null})).send().await.unwrap()).await;
    assert_eq!((s, &v["user"]["locale"]), (200, &Value::Null), "{v}");
    assert_eq!(me().await, Value::Null);
    assert_eq!(locale_of(&e, "alice").await, None);

    // refused values leave what was stored
    put(json!({"locale": "en"})).send().await.unwrap();
    for bad in [
        json!("fr"),
        json!("EN"),
        json!("zh-tw"),
        json!(""),
        json!("en "),
        json!("zh_TW"),
    ] {
        let (s, v) = body(put(json!({"locale": bad})).send().await.unwrap()).await;
        assert_eq!(
            (s, v["error"]["code"].as_str()),
            (422, Some("bad_locale")),
            "{bad}"
        );
        assert_eq!(locale_of(&e, "alice").await.as_deref(), Some("en"), "{bad}");
        assert_eq!(me().await, json!("en"));
    }
    // a wrong type or another field is a bad request, nothing changes
    for bad in [
        json!({"locale": 5}),
        json!({"locale": "en", "role": "admin"}),
        json!({}),
    ] {
        let (s, v) = body(put(bad.clone()).send().await.unwrap()).await;
        assert!(s == 400 || s == 422, "{bad}: {s}");
        assert!(v["error"]["code"].is_string(), "{v}");
        assert_eq!(locale_of(&e, "alice").await.as_deref(), Some("en"), "{bad}");
    }
    let alice = e.auth.list_users().await.unwrap();
    assert_eq!(
        alice[0].role,
        Role::Agent,
        "no role change through /auth/me"
    );

    // no CSRF header, a wrong one, a form body: 403 and nothing changes
    let r = c
        .put(e.url("/api/v1/auth/me"))
        .json(&json!({"locale": "zh-TW"}))
        .send()
        .await
        .unwrap();
    let (s, v) = body(r).await;
    assert_eq!((s, v["error"]["code"].as_str()), (403, Some("csrf")));
    let r = c
        .put(e.url("/api/v1/auth/me"))
        .header("X-Sipfax-Csrf", "0".repeat(64))
        .json(&json!({"locale": "zh-TW"}))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    let r = c
        .put(e.url("/api/v1/auth/me"))
        .header("X-Sipfax-Csrf", &csrf)
        .header("Content-Type", "text/plain")
        .body(r#"{"locale":"zh-TW"}"#)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 403);
    assert_eq!(locale_of(&e, "alice").await.as_deref(), Some("en"));

    // without a session: 401
    let r = reqwest::Client::new()
        .put(e.url("/api/v1/auth/me"))
        .header("X-Sipfax-Csrf", &csrf)
        .json(&json!({"locale": "zh-TW"}))
        .send()
        .await
        .unwrap();
    let (s, v) = body(r).await;
    assert_eq!(
        (s, v["error"]["code"].as_str()),
        (401, Some("unauthenticated"))
    );
    assert_eq!(locale_of(&e, "alice").await.as_deref(), Some("en"));
    e.stop().await;
}

#[tokio::test]
async fn a14_the_administrator_may_only_give_the_supported_languages_too() {
    let e = env().await;
    add_user(&e, "root", Role::Admin, false).await;
    add_user(&e, "bob", Role::Agent, false).await;
    let (c, csrf) = login(&e, "root").await;
    let before = e.auth.list_users().await.unwrap();
    let bob = before.iter().find(|u| u.username == "bob").unwrap().id;

    // a new account
    for (name, loc, ok) in [
        ("u_en", json!("en"), true),
        ("u_null", json!(null), true),
        ("u_fr", json!("fr"), false),
    ] {
        let (s, v) = body(
            change(&c, Method::POST, e.url("/api/v1/users"), &csrf)
                .json(&json!({"username": name, "role": "agent", "password": PW, "locale": loc}))
                .send()
                .await
                .unwrap(),
        )
        .await;
        if ok {
            assert!(s == 200 || s == 201, "{s} {v}");
            assert_eq!(locale_of(&e, name).await.as_deref(), loc.as_str());
        } else {
            assert_eq!((s, v["error"]["code"].as_str()), (422, Some("bad_locale")));
            assert!(
                locale_of_opt(&e, name).await.is_none(),
                "the refused account was not created"
            );
        }
    }

    // an existing account: set, clear with null, refuse a bad one
    let url = e.url(&format!("/api/v1/users/{bob}"));
    let (s, v) = body(
        change(&c, Method::PUT, url.clone(), &csrf)
            .json(&json!({"locale": "zh-TW"}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!(
        (s, v["user"]["locale"].as_str()),
        (200, Some("zh-TW")),
        "{v}"
    );
    for bad in ["xx", "zh", "EN"] {
        let (s, v) = body(
            change(&c, Method::PUT, url.clone(), &csrf)
                .json(&json!({"locale": bad, "display_name": "changed"}))
                .send()
                .await
                .unwrap(),
        )
        .await;
        assert_eq!(
            (s, v["error"]["code"].as_str()),
            (422, Some("bad_locale")),
            "{bad}"
        );
        let now = e.auth.list_users().await.unwrap();
        let b = now.iter().find(|u| u.id == bob).unwrap();
        assert_eq!(b.locale.as_deref(), Some("zh-TW"), "{bad}");
        assert_eq!(
            b.display_name, None,
            "the refused request changed nothing else: {bad}"
        );
    }
    let (s, v) = body(
        change(&c, Method::PUT, url, &csrf)
            .json(&json!({"locale": null}))
            .send()
            .await
            .unwrap(),
    )
    .await;
    assert_eq!((s, &v["user"]["locale"]), (200, &Value::Null), "{v}");
    assert_eq!(locale_of(&e, "bob").await, None);
    e.stop().await;
}

async fn locale_of_opt(e: &common::Env, name: &str) -> Option<Option<String>> {
    e.auth
        .list_users()
        .await
        .unwrap()
        .into_iter()
        .find(|u| u.username == name)
        .map(|u| u.locale)
}

// ------------------------------------------------------------------------------ U05

#[tokio::test]
async fn u05_the_root_and_the_assets_over_http_with_type_cache_and_security_headers() {
    let e = env().await;
    let c = reqwest::Client::new();
    let r = c.get(e.url("/")).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let h = r.headers().clone();
    let ct = h["content-type"].to_str().unwrap().to_string();
    assert_eq!(ct, "text/html; charset=utf-8");
    assert_eq!(h["cache-control"], "no-cache");
    assert_eq!(h["content-security-policy"], web::server::CSP);
    assert_eq!(h["x-content-type-options"], "nosniff");
    let page = r.text().await.unwrap();

    if web::ui::embedded() {
        // the built interface: index.html names its script and style under assets/
        assert!(page.contains("<div id=\"app\">"), "{page}");
        let find = |ext: &str| -> String {
            let end = page.find(&format!(".{ext}\"")).expect(ext) + ext.len() + 1;
            let start = page[..end].rfind("/assets/").unwrap();
            page[start..end].to_string()
        };
        for (path, want) in [
            (find("js"), "text/javascript; charset=utf-8"),
            (find("css"), "text/css; charset=utf-8"),
        ] {
            let r = c.get(e.url(&path)).send().await.unwrap();
            assert_eq!(r.status(), 200, "{path}");
            assert_eq!(r.headers()["content-type"], want, "{path}");
            assert_eq!(
                r.headers()["cache-control"],
                "public, max-age=31536000, immutable",
                "{path}"
            );
            assert!(!r.bytes().await.unwrap().is_empty(), "{path}");
        }
        let r = c.get(e.url("/favicon.svg")).send().await.unwrap();
        assert_eq!(r.status(), 200);
        assert_eq!(r.headers()["content-type"], "image/svg+xml");
        assert_eq!(r.headers()["cache-control"], "no-cache");
        // the same bytes at /index.html
        let r = c.get(e.url("/index.html")).send().await.unwrap();
        assert_eq!(r.text().await.unwrap(), page);
    } else {
        // built without web-ui/dist: the explanation (the unit tests in ui.rs cover the
        // built case with a table of their own)
        assert!(page.contains("npm ci"), "{page}");
    }
    e.stop().await;
}

#[tokio::test]
async fn u05_a_path_that_is_not_a_file_and_a_post_get_the_api_404() {
    let e = env().await;
    let c = reqwest::Client::new();
    for (m, path) in [
        (Method::GET, "/no-such-page"),
        (Method::GET, "/assets/nothing-0123.js"),
        (Method::GET, "/assets/"),
        (Method::GET, "/..%2f..%2fetc%2fpasswd"),
        (Method::GET, "/assets/..%2findex.html.bak"),
        (Method::POST, "/"),
        (Method::DELETE, "/index.html"),
        (Method::GET, "/api/v1/no-such-route"),
    ] {
        let r = c.request(m.clone(), e.url(path)).send().await.unwrap();
        let (s, v) = body(r).await;
        assert_eq!(s, 404, "{m} {path}: {v}");
        assert_eq!(v["error"]["code"], "not_found", "{m} {path}");
    }
    e.stop().await;
}
