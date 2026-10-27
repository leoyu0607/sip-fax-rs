//! M6a (ADR-070) D01: `docs/openapi.yaml` is a valid OpenAPI 3.1 document, says what the fax
//! routes do (routes, methods, statuses, the fields of a fax) and is served at
//! `/api/v1/openapi.yaml` without a login.

mod common;

use std::collections::{BTreeMap, BTreeSet};

use accounts::Role;
use chrono::Duration as Dur;
use common::*;
use reqwest::Method;
use serde_json::{Value, json};
use store::Clock;

const FAXES_RS: &str = include_str!("../src/api/faxes.rs");

fn yaml_text() -> String {
    std::fs::read_to_string(concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/../../docs/openapi.yaml"
    ))
    .expect("docs/openapi.yaml")
}

fn doc() -> Value {
    serde_norway::from_str(&yaml_text()).expect("the document is YAML")
}

/// `(path, METHOD)` of every operation of the document, the path below `/api/v1`.
fn documented(doc: &Value) -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    for (path, item) in doc["paths"].as_object().unwrap() {
        for (m, op) in item.as_object().unwrap() {
            if ["get", "put", "post", "delete", "patch", "head", "options"].contains(&m.as_str()) {
                assert!(op.is_object(), "{path} {m}");
                out.insert((path.clone(), m.to_ascii_uppercase()));
            }
        }
    }
    out
}

/// `(path, METHOD)` of the routes of `api/faxes.rs` (read from its source), under `/api/v1`.
fn router_routes() -> BTreeSet<(String, String)> {
    let mut out = BTreeSet::new();
    let src = FAXES_RS.replace("\r\n", "\n");
    let start = src.find("pub fn routes()").unwrap();
    let body = &src[start..src[start..].find("\n}\n").unwrap() + start];
    for line in body
        .lines()
        .filter(|l| l.trim_start().starts_with(".route("))
    {
        let path = line.split('"').nth(1).unwrap().to_string();
        let handlers = line.split('"').nth(2).unwrap();
        for m in ["get", "put", "post", "delete"] {
            if handlers.contains(&format!("{m}(")) {
                out.insert((path.clone(), m.to_ascii_uppercase()));
            }
        }
    }
    assert!(out.len() >= 8, "the routes were found: {out:?}");
    out
}

// ---------------------------------------------------------------- the document itself
#[test]
fn d01_the_document_is_a_valid_openapi_3_1_document() {
    let text = yaml_text();
    // a parser that knows OpenAPI 3.1
    let as_json = serde_json::to_string(&serde_norway::from_str::<Value>(&text).unwrap()).unwrap();
    let spec = oas3::from_json(&as_json).expect("oas3 parses the document as OpenAPI 3.1");
    assert!(spec.openapi.starts_with("3.1"), "{}", spec.openapi);
    // (the parser is not a rubber stamp)
    assert!(oas3::from_json(r#"{"openapi":"3.1.0"}"#).is_err());
    assert!(
        oas3::from_json(r#"{"openapi":"3.1.0","info":{"title":1,"version":"1"},"paths":{}}"#)
            .is_err()
    );
    assert!(!spec.info.title.is_empty() && !spec.info.version.is_empty());
    assert!(spec.paths.as_ref().is_some_and(|p| !p.is_empty()));
    assert!(spec.components.is_some());

    // our own checks on top (a parser that is lenient lets mistakes through)
    let d = doc();
    assert_eq!(d["openapi"], "3.1.0");
    let mut ids = BTreeSet::new();
    for (path, item) in d["paths"].as_object().unwrap() {
        assert!(path.starts_with('/'));
        let declared: BTreeSet<String> = path
            .split('/')
            .filter_map(|s| s.strip_prefix('{'))
            .map(|s| s.split('}').next().unwrap().to_string())
            .collect();
        for (m, op) in item.as_object().unwrap() {
            let op_id = op["operationId"]
                .as_str()
                .unwrap_or_else(|| panic!("operationId of {m} {path}"));
            assert!(ids.insert(op_id.to_string()), "operationId {op_id} twice");
            assert!(
                op["responses"].as_object().is_some_and(|r| !r.is_empty()),
                "{m} {path} has responses"
            );
            assert!(
                op["summary"].as_str().is_some_and(|s| !s.is_empty()),
                "{m} {path} has a summary"
            );
            // every `{name}` of the path is a declared path parameter and the other way round
            let mut params = BTreeSet::new();
            for p in op["parameters"].as_array().into_iter().flatten() {
                let p = resolve(&d, p);
                if p["in"] == "path" {
                    assert_eq!(p["required"], true, "{path}: a path parameter is required");
                    params.insert(p["name"].as_str().unwrap().to_string());
                }
            }
            assert_eq!(params, declared, "{m} {path}: path parameters");
            if m == "put" || m == "post" {
                assert!(op["requestBody"].is_object(), "{m} {path} has a body");
            }
            for (code, r) in op["responses"].as_object().unwrap() {
                assert!(code.len() == 3 && code.parse::<u16>().is_ok(), "{code}");
                let r = resolve(&d, r);
                assert!(
                    r["description"].as_str().is_some_and(|s| !s.is_empty()),
                    "{m} {path} {code} description"
                );
            }
        }
    }
    // every $ref points at something
    let mut refs = 0;
    walk(&d, &mut |v| {
        if let Some(r) = v.get("$ref").and_then(Value::as_str) {
            refs += 1;
            let target = r
                .strip_prefix("#/")
                .unwrap_or_else(|| panic!("local ref {r}"));
            let mut cur = &d;
            for part in target.split('/') {
                cur = cur
                    .get(part)
                    .unwrap_or_else(|| panic!("{r} does not exist"));
            }
        }
    });
    assert!(refs > 10, "{refs} refs");
    // the security scheme is the bearer key and every operation needs it
    assert_eq!(
        d["components"]["securitySchemes"]["apiKey"]["scheme"],
        "bearer"
    );
    assert!(d["security"].as_array().is_some_and(|s| !s.is_empty()));
    // the status enum of the document is the one of the interface
    let doc_status: BTreeSet<&str> = d["components"]["schemas"]["FaxStatus"]["enum"]
        .as_array()
        .unwrap()
        .iter()
        .map(|v| v.as_str().unwrap())
        .collect();
    assert_eq!(
        doc_status,
        ["failed", "no_fax", "partial", "received", "receiving"]
            .into_iter()
            .collect()
    );
}

fn resolve<'a>(d: &'a Value, v: &'a Value) -> &'a Value {
    match v.get("$ref").and_then(Value::as_str) {
        Some(r) => {
            let mut cur = d;
            for part in r.strip_prefix("#/").unwrap().split('/') {
                cur = &cur[part];
            }
            cur
        }
        None => v,
    }
}

fn walk(v: &Value, f: &mut dyn FnMut(&Value)) {
    f(v);
    match v {
        Value::Object(m) => m.values().for_each(|x| walk(x, f)),
        Value::Array(a) => a.iter().for_each(|x| walk(x, f)),
        _ => {}
    }
}

// ---------------------------------------------------------------- the routes
#[test]
fn d01_every_fax_route_a_key_may_use_is_documented_and_nothing_else() {
    let d = doc();
    let doc_ops = documented(&d);
    let router = router_routes();
    // the document names the two files and the png of the generic routes of the router
    let mut expect = BTreeSet::new();
    for (path, m) in &router {
        match path.as_str() {
            // a session's action: no key may use it, so it is not in the description for keys
            "/faxes/{id}/redeliver" => {}
            "/faxes/{id}/{file}" => {
                expect.insert(("/faxes/{id}/file.pdf".to_string(), m.clone()));
                expect.insert(("/faxes/{id}/file.tif".to_string(), m.clone()));
            }
            "/faxes/{id}/pages/{page}" => {
                expect.insert(("/faxes/{id}/pages/{page}.png".to_string(), m.clone()));
            }
            _ => {
                expect.insert((path.clone(), m.clone()));
            }
        }
    }
    assert_eq!(
        doc_ops,
        expect,
        "documented only: {:?}; undocumented: {:?}",
        doc_ops.difference(&expect).collect::<Vec<_>>(),
        expect.difference(&doc_ops).collect::<Vec<_>>()
    );
    assert!(router.contains(&("/faxes/{id}/redeliver".to_string(), "POST".to_string())));
    assert!(
        doc_ops.iter().all(|(p, _)| p.starts_with("/faxes")),
        "only fax routes"
    );
}

/// Substitutes the placeholders of a documented path.
fn fill(path: &str, id: i64) -> String {
    path.replace("{id}", &id.to_string()).replace("{page}", "1")
}

/// A value of a JSON type of the schema (`type` is a name or a list of names).
fn type_ok(v: &Value, ty: &Value) -> bool {
    let names: Vec<&str> = match ty {
        Value::String(s) => vec![s.as_str()],
        Value::Array(a) => a.iter().filter_map(Value::as_str).collect(),
        _ => return true,
    };
    names.iter().any(|n| match *n {
        "string" => v.is_string(),
        "integer" => v.is_i64() || v.is_u64(),
        "number" => v.is_number(),
        "boolean" => v.is_boolean(),
        "null" => v.is_null(),
        "object" => v.is_object(),
        "array" => v.is_array(),
        _ => false,
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn d01_the_routes_answer_as_documented_and_a_fax_has_the_documented_fields() {
    let d = doc();
    let e = env().await;
    add_user(&e, "boss", Role::Admin, false).await;
    let (c, csrf) = login(&e, "boss").await;
    let id = fax(&e, "doc-1", "0911", 10).await;
    let purged = fax(&e, "doc-2", "0922", 9).await;
    e.store
        .mark_files_purged(&[purged], e.clock.now())
        .await
        .unwrap();
    e.clock.advance(Dur::seconds(3));
    let key = {
        let r = change(&c, Method::POST, e.url("/api/v1/api-keys"), &csrf)
            .json(&json!({"name": "doc", "write": true}))
            .send()
            .await
            .unwrap();
        r.json::<Value>().await.unwrap()["key"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let ro = {
        let r = change(&c, Method::POST, e.url("/api/v1/api-keys"), &csrf)
            .json(&json!({"name": "doc-ro", "write": false}))
            .send()
            .await
            .unwrap();
        r.json::<Value>().await.unwrap()["key"]
            .as_str()
            .unwrap()
            .to_string()
    };
    let api = reqwest::Client::new();
    let call = |m: Method, path: String, key: Option<String>, body: Option<Value>| {
        let (api, e) = (&api, &e);
        async move {
            let mut rq = api.request(m, e.url(&format!("/api/v1{path}")));
            if let Some(k) = key {
                rq = rq.header("Authorization", format!("Bearer {k}"));
            }
            if let Some(b) = body {
                rq = rq.json(&b);
            }
            let r = rq.send().await.unwrap();
            let s = r.status().as_u16();
            let ct = r
                .headers()
                .get("content-type")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("")
                .to_string();
            let bytes = r.bytes().await.unwrap();
            (s, ct, bytes)
        }
    };
    let documented_statuses = |path: &str, method: &str| -> BTreeSet<u16> {
        d["paths"][path][method.to_ascii_lowercase()]["responses"]
            .as_object()
            .unwrap_or_else(|| panic!("{method} {path} in the document"))
            .keys()
            .map(|k| k.parse().unwrap())
            .collect()
    };

    // 1. every documented operation, called with valid input, answers 200 with the documented
    //    media type; every status seen is one the document lists
    let mut seen: BTreeMap<(String, String), BTreeSet<u16>> = BTreeMap::new();
    for (path, m) in documented(&d) {
        let method: Method = m.parse().unwrap();
        let body = match path.as_str() {
            "/faxes/{id}/read" => Some(json!({"read": true})),
            "/faxes/{id}/note" => Some(json!({"text": "documented", "expected_updated_at": null})),
            _ => None,
        };
        let (s, ct, bytes) = call(method, fill(&path, id), Some(key.clone()), body).await;
        assert_eq!(s, 200, "{m} {path}");
        let media: BTreeSet<&str> = d["paths"][path.as_str()][m.to_ascii_lowercase()]["responses"]
            ["200"]["content"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert!(
            media.iter().any(|x| ct.starts_with(x)),
            "{m} {path}: {ct} is not one of {media:?}"
        );
        assert!(!bytes.is_empty());
        seen.entry((path.clone(), m.clone())).or_default().insert(s);
    }

    // 2. the errors the document promises
    #[allow(clippy::type_complexity)]
    let errors: Vec<(&str, &str, String, Option<String>, Option<Value>, u16, &str)> = vec![
        // (path template, method, concrete path, key, body, status, code)
        (
            "/faxes",
            "GET",
            "/faxes?limit=0".into(),
            Some(key.clone()),
            None,
            400,
            "bad_request",
        ),
        (
            "/faxes",
            "GET",
            "/faxes".into(),
            Some("sfx_".into()),
            None,
            401,
            "invalid_api_key",
        ),
        (
            "/faxes/changes",
            "GET",
            "/faxes/changes?after=zzz".into(),
            Some(key.clone()),
            None,
            400,
            "bad_request",
        ),
        (
            "/faxes/changes",
            "GET",
            "/faxes/changes?limit=201".into(),
            Some(key.clone()),
            None,
            400,
            "bad_request",
        ),
        (
            "/faxes/changes",
            "GET",
            "/faxes/changes".into(),
            Some("sfx_".into()),
            None,
            401,
            "invalid_api_key",
        ),
        (
            "/faxes/counts",
            "GET",
            "/faxes/counts?since=x".into(),
            Some(key.clone()),
            None,
            400,
            "bad_request",
        ),
        (
            "/faxes/{id}",
            "GET",
            "/faxes/999999".into(),
            Some(key.clone()),
            None,
            404,
            "not_found",
        ),
        (
            "/faxes/{id}",
            "GET",
            format!("/faxes/{id}"),
            Some("sfx_".into()),
            None,
            401,
            "invalid_api_key",
        ),
        (
            "/faxes/{id}/file.pdf",
            "GET",
            "/faxes/999999/file.pdf".into(),
            Some(key.clone()),
            None,
            404,
            "not_found",
        ),
        (
            "/faxes/{id}/file.pdf",
            "GET",
            format!("/faxes/{purged}/file.pdf"),
            Some(key.clone()),
            None,
            410,
            "purged",
        ),
        (
            "/faxes/{id}/file.tif",
            "GET",
            format!("/faxes/{purged}/file.tif"),
            Some(key.clone()),
            None,
            410,
            "purged",
        ),
        (
            "/faxes/{id}/file.tif",
            "GET",
            "/faxes/999999/file.tif".into(),
            Some(key.clone()),
            None,
            404,
            "not_found",
        ),
        (
            "/faxes/{id}/pages/{page}.png",
            "GET",
            format!("/faxes/{id}/pages/1.png?kind=huge"),
            Some(key.clone()),
            None,
            400,
            "bad_request",
        ),
        (
            "/faxes/{id}/pages/{page}.png",
            "GET",
            format!("/faxes/{id}/pages/7.png"),
            Some(key.clone()),
            None,
            404,
            "not_found",
        ),
        (
            "/faxes/{id}/pages/{page}.png",
            "GET",
            format!("/faxes/{purged}/pages/1.png"),
            Some(key.clone()),
            None,
            410,
            "purged",
        ),
        (
            "/faxes/{id}/read",
            "PUT",
            format!("/faxes/{id}/read"),
            Some(ro.clone()),
            Some(json!({"read": true})),
            403,
            "forbidden",
        ),
        (
            "/faxes/{id}/read",
            "PUT",
            "/faxes/999999/read".into(),
            Some(key.clone()),
            Some(json!({"read": true})),
            404,
            "not_found",
        ),
        (
            "/faxes/{id}/read",
            "PUT",
            format!("/faxes/{id}/read"),
            Some("sfx_".into()),
            Some(json!({"read": true})),
            401,
            "invalid_api_key",
        ),
        (
            "/faxes/{id}/note",
            "PUT",
            format!("/faxes/{id}/note"),
            Some(ro.clone()),
            Some(json!({"text": "x", "expected_updated_at": null})),
            403,
            "forbidden",
        ),
        (
            "/faxes/{id}/note",
            "PUT",
            format!("/faxes/{id}/note"),
            Some(key.clone()),
            Some(json!({"text": "stale", "expected_updated_at": "2001-01-01T00:00:00Z"})),
            409,
            "conflict",
        ),
        (
            "/faxes/{id}/note",
            "PUT",
            format!("/faxes/{id}/note"),
            Some(key.clone()),
            Some(json!({"text": "x".repeat(4001), "expected_updated_at": null})),
            422,
            "too_long",
        ),
        (
            "/faxes/{id}/note",
            "PUT",
            "/faxes/999999/note".into(),
            Some(key.clone()),
            Some(json!({"text": "x", "expected_updated_at": null})),
            404,
            "not_found",
        ),
    ];
    for (template, m, path, k, body, status, code) in errors {
        let (s, ct, bytes) = call(m.parse().unwrap(), path.clone(), k, body).await;
        assert_eq!(s, status, "{m} {path}: {}", String::from_utf8_lossy(&bytes));
        assert!(ct.starts_with("application/json"), "{m} {path}: {ct}");
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(v["error"]["code"], code, "{m} {path}");
        assert!(
            v["error"]["message"]
                .as_str()
                .is_some_and(|x| !x.is_empty())
        );
        assert!(
            documented_statuses(template, m).contains(&status),
            "{m} {template}: {status} is answered but not documented"
        );
        // and the description of that response names the code
        let resp = &d["paths"][template][m.to_ascii_lowercase()]["responses"][status.to_string()];
        let resp = resolve(&d, resp);
        assert!(
            resp["description"].as_str().unwrap().contains(code) || code == "too_long",
            "{m} {template} {status}: the description names `{code}`: {resp}"
        );
    }
    // 304 of the page
    let r = api
        .get(e.url(&format!("/api/v1/faxes/{id}/pages/1.png")))
        .header("Authorization", format!("Bearer {key}"))
        .send()
        .await
        .unwrap();
    let etag = r.headers().get("etag").unwrap().clone();
    let r = api
        .get(e.url(&format!("/api/v1/faxes/{id}/pages/1.png")))
        .header("Authorization", format!("Bearer {key}"))
        .header("If-None-Match", etag)
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 304);
    assert!(documented_statuses("/faxes/{id}/pages/{page}.png", "GET").contains(&304));

    // 3. the fields of a fax: the document's schema and the interface say the same
    let schema = &d["components"]["schemas"]["Fax"];
    let props: BTreeSet<&str> = schema["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    let (_, _, bytes) = call(Method::GET, format!("/faxes/{id}"), Some(key.clone()), None).await;
    let fax_v: Value = serde_json::from_slice(&bytes).unwrap();
    let real: BTreeSet<&str> = fax_v
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        real,
        props,
        "fields of a fax: the interface has {:?} more, the document {:?} more",
        real.difference(&props).collect::<Vec<_>>(),
        props.difference(&real).collect::<Vec<_>>()
    );
    for r in schema["required"].as_array().unwrap() {
        assert!(real.contains(r.as_str().unwrap()), "required {r}");
    }
    // every field has the documented type, for a finished fax and one that is still receiving
    let live = {
        use store::NewFax;
        e.store
            .insert_receiving(&NewFax {
                call_id: "doc-live".into(),
                trunk: "gw".into(),
                ani: None,
                ani_source: None,
                dnis: None,
                dnis_source: None,
                started_at: e.clock.now(),
                answered_at: None,
            })
            .await
            .unwrap()
    };
    e.clock.advance(Dur::seconds(3));
    for fid in [id, purged, live] {
        let (_, _, bytes) = call(
            Method::GET,
            format!("/faxes/{fid}"),
            Some(key.clone()),
            None,
        )
        .await;
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        for (name, def) in schema["properties"].as_object().unwrap() {
            let def = resolve(&d, def);
            if let Some(ty) = def.get("type") {
                assert!(
                    type_ok(&v[name], ty),
                    "fax {fid}: {name} = {} is not {ty}",
                    v[name]
                );
            }
            if let Some(en) = def.get("enum").and_then(Value::as_array) {
                assert!(
                    en.contains(&v[name]),
                    "fax {fid}: {name} = {} is not in {en:?}",
                    v[name]
                );
            }
        }
        for t in ["started_at", "created_at", "updated_at"] {
            assert!(chrono::DateTime::parse_from_rfc3339(v[t].as_str().unwrap()).is_ok());
        }
    }
    // the lists carry the same Fax objects
    for (path, field) in [("/faxes", "items"), ("/faxes/changes", "items")] {
        let (_, _, bytes) = call(Method::GET, path.to_string(), Some(key.clone()), None).await;
        let v: Value = serde_json::from_slice(&bytes).unwrap();
        let items = v[field].as_array().unwrap();
        assert_eq!(items.len(), 3, "{path}");
        for it in items {
            let keys: BTreeSet<&str> = it.as_object().unwrap().keys().map(String::as_str).collect();
            assert_eq!(keys, props, "{path}");
        }
        let top: BTreeSet<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
        let want: BTreeSet<&str> = if path == "/faxes" {
            ["items", "next_cursor"].into()
        } else {
            ["items", "next"].into()
        };
        assert_eq!(top, want, "{path}");
        let schema_top: BTreeSet<&str> = d["paths"][path]["get"]["responses"]["200"]["content"]["application/json"]["schema"]["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect();
        assert_eq!(schema_top, want, "{path}: documented");
    }
    // the counts
    let (_, _, bytes) = call(
        Method::GET,
        "/faxes/counts?since=2026-01-01T00:00:00Z".into(),
        Some(key.clone()),
        None,
    )
    .await;
    let v: Value = serde_json::from_slice(&bytes).unwrap();
    let top: BTreeSet<&str> = v.as_object().unwrap().keys().map(String::as_str).collect();
    let doc_top: BTreeSet<&str> = d["paths"]["/faxes/counts"]["get"]["responses"]["200"]["content"]
        ["application/json"]["schema"]["properties"]
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(top, doc_top);
    // the parameters that the document gives are the ones the routes accept (nothing is "unknown")
    for (path, query) in [
        (
            "/faxes",
            "is_read=false&status=received&from=2026-01-01T00:00:00Z&to=2030-01-01T00:00:00Z&ani=09&dnis=25&trunk=gw&q=x&limit=5",
        ),
        ("/faxes/changes", "since=2026-01-01T00:00:00Z&limit=5"),
        ("/faxes/changes", "after=0.0&limit=5"),
        ("/faxes/counts", "since=2026-01-01T00:00:00Z"),
    ] {
        let (s, _, bytes) = call(
            Method::GET,
            format!("{path}?{query}"),
            Some(key.clone()),
            None,
        )
        .await;
        assert_eq!(
            s,
            200,
            "{path}?{query}: {}",
            String::from_utf8_lossy(&bytes)
        );
        let documented: BTreeSet<&str> = d["paths"][path]["get"]["parameters"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| resolve(&d, p)["name"].as_str().unwrap())
            .collect();
        for name in query.split('&').map(|kv| kv.split('=').next().unwrap()) {
            assert!(
                documented.contains(name),
                "{path}: {name} is accepted but not documented"
            );
        }
    }
    // a parameter that is not documented is refused (that is what the document says too)
    let (s, _, _) = call(
        Method::GET,
        "/faxes/changes?bogus=1".into(),
        Some(key.clone()),
        None,
    )
    .await;
    assert_eq!(s, 400);
    e.stop().await;
}

// ---------------------------------------------------------------- served by the interface
#[tokio::test(flavor = "multi_thread")]
async fn d01_the_interface_serves_the_same_document_without_a_login() {
    let e = env().await;
    let on_disk = yaml_text();
    for path in ["/api/v1/openapi.yaml"] {
        let r = reqwest::get(e.url(path)).await.unwrap();
        assert_eq!(r.status(), 200, "no login needed");
        assert!(
            r.headers()
                .get("content-type")
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("application/yaml"),
            "{:?}",
            r.headers().get("content-type")
        );
        let body = r.text().await.unwrap();
        assert_eq!(body, on_disk, "the served text is docs/openapi.yaml");
        assert_eq!(body, web::api::OPENAPI);
        // and what is served parses to the same document
        let served: Value = serde_norway::from_str(&body).unwrap();
        assert_eq!(served, doc());
        assert!(oas3::from_json(serde_json::to_string(&served).unwrap()).is_ok());
    }
    // a key or a bad key in the request changes nothing: no authentication is looked at
    let r = reqwest::Client::new()
        .get(e.url("/api/v1/openapi.yaml"))
        .header("Authorization", "Bearer garbage")
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);
    // the document only describes; other methods are not answered
    let r = reqwest::Client::new()
        .post(e.url("/api/v1/openapi.yaml"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 405);
    e.stop().await;
}

// ---------------------------------------------------------------- D04 (the commands of the guide)
const INTEGRATION_MD: &str = include_str!("../../../docs/integration.md");

/// Splits a command line the way a shell does for the simple cases of the guide (double quotes).
fn split_words(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut any = false;
    for ch in line.chars() {
        match ch {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            c if c.is_whitespace() && !quoted => {
                if any || !cur.is_empty() {
                    out.push(std::mem::take(&mut cur));
                    any = false;
                }
            }
            c => cur.push(c),
        }
    }
    if any || !cur.is_empty() {
        out.push(cur);
    }
    out
}

#[tokio::test(flavor = "multi_thread")]
async fn d04_the_curl_commands_and_the_request_of_the_guide_work() {
    let text = INTEGRATION_MD.replace("\r\n", "\n");
    // the sh block of section 3: the variables and the curl lines
    let sh = text
        .split("```sh\n")
        .find(|b| b.contains("curl "))
        .and_then(|b| b.split("\n```").next())
        .expect("the curl block");
    let curls: Vec<&str> = sh.lines().filter(|l| l.starts_with("curl ")).collect();
    assert_eq!(curls.len(), 3, "{sh}");
    assert!(sh.contains("KEY=sfx_") && sh.contains("BASE=https://"));
    // the HTTP request of the guide
    let http = text
        .split("```http\n")
        .nth(1)
        .and_then(|b| b.split("\n```").next())
        .unwrap();
    let request_line = http.lines().next().unwrap();
    assert_eq!(request_line, "GET /api/v1/faxes/changes HTTP/1.1");
    assert!(http.contains("Authorization: Bearer sfx_"));

    let e = env().await;
    add_user(&e, "boss", Role::Admin, false).await;
    let (c, csrf) = login(&e, "boss").await;
    // the guide's fax number 42: make the server have a fax with that id
    let mut id = 0;
    while id < 42 {
        id = fax(&e, &format!("curl-{id}"), "0911", 10).await;
    }
    assert_eq!(id, 42);
    e.clock.advance(Dur::seconds(3));
    let key = change(&c, Method::POST, e.url("/api/v1/api-keys"), &csrf)
        .json(&json!({"name": "guide"}))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap()["key"]
        .as_str()
        .unwrap()
        .to_string();

    // the request line, as written, with the key of the guide's header
    let r = reqwest::Client::new()
        .get(e.url(request_line.split(' ').nth(1).unwrap()))
        .header("Authorization", format!("Bearer {key}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 200);

    if std::process::Command::new("curl")
        .arg("--version")
        .output()
        .is_err()
    {
        panic!("curl is needed for D04 (every runner has it)");
    }
    let dir = tempfile::tempdir().unwrap();
    let base = e.url("/api/v1");
    let mut outputs = Vec::new();
    for line in curls {
        // (the server of the test speaks plain HTTP: the certificate option is D02/D03's)
        let mut words = split_words(line);
        let at = words
            .iter()
            .position(|w| w == "--cacert")
            .expect("--cacert in the guide");
        words.drain(at..at + 2);
        let words: Vec<String> = words
            .into_iter()
            .map(|w| w.replace("$KEY", &key).replace("$BASE", &base))
            .collect();
        assert_eq!(words[0], "curl");
        let out = std::process::Command::new("curl")
            .current_dir(dir.path())
            .args(["-sS", "-f"])
            .args(&words[1..])
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{line}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        outputs.push(out.stdout);
    }
    // 1. the list on stdout
    let v: Value = serde_json::from_slice(&outputs[0]).unwrap();
    assert!(v["items"].as_array().unwrap().iter().any(|f| f["id"] == 42));
    // 2. 42.pdf, 3. 42-1.png
    let pdf = std::fs::read(dir.path().join("42.pdf")).unwrap();
    assert_eq!(pdf, b"%PDF-1.4 fax curl-41");
    let png = std::fs::read(dir.path().join("42-1.png")).unwrap();
    assert_eq!(&png[..8], b"\x89PNG\r\n\x1a\n");
    assert!(
        outputs[1].is_empty() && outputs[2].is_empty(),
        "-o: nothing on stdout"
    );

    // the table of the guide: every method and path in it is an operation of the document
    let doc_ops = documented(&doc());
    let table = text.split("| 方法與路徑 | 說明 |").nth(1).unwrap();
    let mut rows = 0;
    for l in table.lines().skip(2).take_while(|l| l.starts_with('|')) {
        let cell = l.split('|').nth(1).unwrap().trim();
        let first = cell.split('`').nth(1).unwrap();
        let (m, path) = first.split_once(' ').unwrap();
        let path = path.split('?').next().unwrap().to_string();
        let mut paths = vec![path.clone()];
        // "`GET /faxes/{id}/file.pdf`、`/file.tif`"
        if cell.contains("、`/file.tif`") {
            paths.push(path.replace("file.pdf", "file.tif"));
        }
        let path_fixed = |p: String| p.replace("pages/{n}.png", "pages/{page}.png");
        for p in paths {
            let p = path_fixed(p);
            assert!(
                doc_ops.contains(&(p.clone(), m.to_string())),
                "{m} {p} of the guide is not in the OpenAPI document"
            );
        }
        rows += 1;
    }
    assert_eq!(rows, 8, "the eight operations of the table");
    // the other direction: every operation of the document is in the guide's table
    let table_text = table.split("\n\n").next().unwrap();
    for (p, m) in &doc_ops {
        let short = p.replace("{page}", "{n}");
        assert!(
            table_text.contains(&format!("{m} {short}"))
                || (p.ends_with("file.tif") && table_text.contains("`/file.tif`")),
            "{m} {p} is not in the table of the guide"
        );
    }
    // the error table: the codes of the guide are the codes the server uses
    for code in [
        "bad_request",
        "invalid_api_key",
        "forbidden",
        "not_found",
        "conflict",
        "purged",
        "unavailable",
    ] {
        assert!(text.contains(&format!("`{code}`")), "{code}");
    }
    e.stop().await;
}
