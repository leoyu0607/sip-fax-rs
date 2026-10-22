//! A10 and A11 over HTTP, table driven: every route of the API (spec 2.1 and 3) as an agent, as
//! an administrator and without a session. A request that is refused must be refused with the
//! right status and must leave the database exactly as it was (test plan section 0).

mod common;

use accounts::Role;
use common::*;
use reqwest::Method;
use serde_json::{Value, json};
use store::{DeliveryStatus, NewDelivery};

const SEED: &str = r#"
[[trunks]]
name = "alpha"
peer = "192.0.2.10:5060"

[[trunks]]
name = "beta"
peer = "192.0.2.11:5060"
"#;

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Who {
    Anon,
    Agent,
    Admin,
}

/// What the matrix needs to build a request.
struct Ctx {
    /// A fax of its own for each caller (the allowed requests change it).
    faxes: [i64; 3],
    victims: [i64; 3],
    call_version: i64,
    call_body: Value,
    trunk_version: i64,
}

impl Ctx {
    fn idx(w: Who) -> usize {
        match w {
            Who::Anon => 0,
            Who::Agent => 1,
            Who::Admin => 2,
        }
    }

    fn path(&self, p: &str, w: Who) -> String {
        let i = Ctx::idx(w);
        p.replace("{fax}", &self.faxes[i].to_string())
            .replace("{victim}", &self.victims[i].to_string())
            .replace("{trunk_version}", &self.trunk_version.to_string())
    }

    fn body(&self, w: Who, b: &Body) -> Option<Value> {
        match b {
            Body::None => None,
            Body::Json(v) => Some(v.clone()),
            Body::Call => {
                let mut v = self.call_body.clone();
                v["max_duration_secs"] = json!(901);
                Some(json!({"version": self.call_version, "body": v}))
            }
            Body::NewUser => Some(
                json!({"username": format!("new-{}", Ctx::idx(w)), "role": "agent", "password": PW}),
            ),
        }
    }
}

enum Body {
    None,
    Json(Value),
    Call,
    NewUser,
}

/// One line of spec 2.1 / 3: the route and who may use it.
struct Route {
    /// The capability of the spec (for the failure message).
    cap: &'static str,
    method: Method,
    path: &'static str,
    body: Body,
    agent: bool,
    admin: bool,
    /// The status a caller who is allowed gets when it is not a 2xx (the WebSocket route answers
    /// 400 to a plain GET: authorised, but not an upgrade).
    allowed_status: Option<u16>,
}

fn r(cap: &'static str, method: Method, path: &'static str, body: Body, agent: bool) -> Route {
    Route {
        cap,
        method,
        path,
        body,
        agent,
        admin: true,
        allowed_status: None,
    }
}

/// Spec 2.1 row by row (and every route of spec 3 under its capability).
fn matrix() -> Vec<Route> {
    let read = || Body::Json(json!({"read": true}));
    let note = || Body::Json(json!({"text": "a note", "expected_updated_at": null}));
    let g = || Method::GET;
    let mut v = vec![
        // 看傳真列表、預覽、下載
        r("view faxes", g(), "/api/v1/faxes", Body::None, true),
        r("view faxes", g(), "/api/v1/faxes/counts", Body::None, true),
        r("view faxes", g(), "/api/v1/faxes/{fax}", Body::None, true),
        r(
            "view faxes",
            g(),
            "/api/v1/faxes/{fax}/pages/1.png",
            Body::None,
            true,
        ),
        r(
            "view faxes",
            g(),
            "/api/v1/faxes/{fax}/file.pdf",
            Body::None,
            true,
        ),
        r(
            "view faxes",
            g(),
            "/api/v1/faxes/{fax}/file.tif",
            Body::None,
            true,
        ),
        // 切換已讀／未讀、編輯備註
        r(
            "read flag",
            Method::PUT,
            "/api/v1/faxes/{fax}/read",
            read(),
            true,
        ),
        r(
            "note",
            Method::PUT,
            "/api/v1/faxes/{fax}/note",
            note(),
            true,
        ),
        // 手動重新投遞
        r(
            "redeliver",
            Method::POST,
            "/api/v1/faxes/{fax}/redeliver",
            Body::Json(json!({})),
            false,
        ),
        // 修改任何設定、看設定
        r("view config", g(), "/api/v1/config", Body::None, false),
        r("view config", g(), "/api/v1/config/sip", Body::None, false),
        r(
            "view config",
            g(),
            "/api/v1/config/trunk:alpha",
            Body::None,
            false,
        ),
        r(
            "edit config",
            Method::PUT,
            "/api/v1/config/call",
            Body::Call,
            false,
        ),
        r(
            "edit config",
            Method::PUT,
            "/api/v1/config/trunks/order",
            Body::Json(json!({"order": ["beta", "alpha"]})),
            false,
        ),
        r(
            "edit config",
            Method::DELETE,
            "/api/v1/config/trunk:beta?version={trunk_version}",
            Body::None,
            false,
        ),
        // 帳號管理、看稽核紀錄
        r(
            "audit",
            g(),
            "/api/v1/config/audit?limit=5",
            Body::None,
            false,
        ),
        r("users", g(), "/api/v1/users", Body::None, false),
        r("users", Method::POST, "/api/v1/users", Body::NewUser, false),
        r(
            "users",
            Method::PUT,
            "/api/v1/users/{victim}",
            Body::Json(json!({"display_name": "Renamed"})),
            false,
        ),
        r(
            "users",
            Method::DELETE,
            "/api/v1/users/{victim}",
            Body::None,
            false,
        ),
        // 看系統狀態
        r("status", g(), "/api/v1/status", Body::None, true),
        // 登入的人都可以看自己
        r("session", g(), "/api/v1/auth/me", Body::None, true),
    ];
    // the WebSocket route: authorised callers get past the session and are told to upgrade
    let mut ev = r("events", g(), "/api/v1/events", Body::None, true);
    ev.allowed_status = Some(400);
    v.push(ev);
    v
}

/// Everything the refused requests must not touch.
async fn snapshot(e: &Env, ctx: &Ctx) -> String {
    let users = e.auth.list_users().await.unwrap();
    let mut out = format!("{users:?}\n");
    out += &format!("{:?}\n", e.config.list_sections().await.unwrap());
    out += &format!("{:?}\n", e.config.audit(500, None).await.unwrap());
    for id in ctx.faxes {
        out += &format!("{:?}\n", e.store.get(id).await.unwrap());
        out += &format!("{:?}\n", e.store.list_deliveries(id).await.unwrap());
    }
    out
}

async fn fax_with_failed_delivery(e: &Env, call: &str) -> i64 {
    let id = fax(e, call, "0212345678", 5).await;
    e.store
        .add_delivery(&NewDelivery {
            fax_id: id,
            target: "folder:/out".into(),
            kind: "folder".into(),
            attempt: 1,
            status: DeliveryStatus::Failed,
            next_try_at: None,
        })
        .await
        .unwrap();
    id
}

struct World {
    e: Env,
    anon: reqwest::Client,
    agent: reqwest::Client,
    agent_csrf: String,
    admin: reqwest::Client,
    admin_csrf: String,
    ctx: Ctx,
}

async fn world() -> World {
    let e = env_seeded(SEED).await;
    add_user(&e, "root", Role::Admin, false).await;
    add_user(&e, "amy", Role::Agent, false).await;
    // accounts the matrix may edit and delete, one per caller
    for n in ["victim-anon", "victim-agent", "victim-admin"] {
        add_user(&e, n, Role::Agent, false).await;
    }
    let (admin, admin_csrf) = login(&e, "root").await;
    let (agent, agent_csrf) = login(&e, "amy").await;
    let mut victims = [0i64; 3];
    for (i, n) in ["victim-anon", "victim-agent", "victim-admin"]
        .iter()
        .enumerate()
    {
        victims[i] = e.auth.get_user(n).await.unwrap().id;
    }
    let mut faxes = [0i64; 3];
    for (i, n) in ["f-anon", "f-agent", "f-admin"].iter().enumerate() {
        faxes[i] = fax_with_failed_delivery(&e, n).await;
    }
    let call: Value = admin
        .get(e.url("/api/v1/config/call"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let trunk: Value = admin
        .get(e.url("/api/v1/config/trunk:beta"))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let ctx = Ctx {
        faxes,
        victims,
        call_version: call["version"].as_i64().unwrap(),
        call_body: call["body"].clone(),
        trunk_version: trunk["version"].as_i64().unwrap(),
    };
    World {
        e,
        anon: reqwest::Client::new(),
        agent,
        agent_csrf,
        admin,
        admin_csrf,
        ctx,
    }
}

impl World {
    fn client(&self, w: Who) -> (&reqwest::Client, &str) {
        match w {
            Who::Anon => (&self.anon, ""),
            Who::Agent => (&self.agent, &self.agent_csrf),
            Who::Admin => (&self.admin, &self.admin_csrf),
        }
    }

    fn request(&self, w: Who, route: &Route) -> reqwest::RequestBuilder {
        let (c, csrf) = self.client(w);
        let url = self.e.url(&self.ctx.path(route.path, w));
        let mut rq = c.request(route.method.clone(), url);
        if route.method != Method::GET {
            rq = rq
                .header("X-Sipfax-Csrf", csrf)
                .header("content-type", "application/json");
        }
        match self.ctx.body(w, &route.body) {
            Some(b) => rq.body(b.to_string()),
            None => rq,
        }
    }
}

async fn answer(r: reqwest::Response) -> (u16, Value) {
    let s = r.status().as_u16();
    let t = r.bytes().await.unwrap();
    (s, serde_json::from_slice(&t).unwrap_or(Value::Null))
}

#[tokio::test(flavor = "multi_thread")]
async fn a11_the_permission_matrix_of_the_spec_holds_for_every_route_and_role() {
    let w = world().await;
    let routes = matrix();
    let mut refused = 0;
    let mut allowed = 0;
    for route in &routes {
        for who in [Who::Anon, Who::Agent, Who::Admin] {
            let may = match who {
                Who::Anon => false,
                Who::Agent => route.agent,
                Who::Admin => route.admin,
            };
            let label = format!("{who:?} {} {} ({})", route.method, route.path, route.cap);
            let before = snapshot(&w.e, &w.ctx).await;
            let (status, v) = answer(w.request(who, route).send().await.unwrap()).await;
            if may {
                allowed += 1;
                match route.allowed_status {
                    Some(s) => assert_eq!(status, s, "{label}: {v}"),
                    None => assert!(
                        (200..300).contains(&status),
                        "{label} must be allowed: {status} {v}"
                    ),
                }
            } else {
                refused += 1;
                let (want, code) = if who == Who::Anon {
                    (401, "unauthenticated")
                } else {
                    (403, "forbidden")
                };
                assert_eq!(status, want, "{label}: {v}");
                assert_eq!(v["error"]["code"], code, "{label}: {v}");
                assert_eq!(
                    snapshot(&w.e, &w.ctx).await,
                    before,
                    "{label}: a refused request changed the database"
                );
            }
        }
    }
    // the matrix is not accidentally empty on either side
    assert!(refused >= 30 && allowed >= 30, "{refused} / {allowed}");
    // the allowed requests of the administrator did what they say
    let cfg = w.e.config.current();
    assert_eq!(cfg.call.max_duration_secs, 901);
    assert_eq!(
        cfg.trunks
            .iter()
            .map(|t| t.name.as_str())
            .collect::<Vec<_>>(),
        ["alpha"],
        "beta was deleted"
    );
    let names: Vec<String> =
        w.e.auth
            .list_users()
            .await
            .unwrap()
            .into_iter()
            .map(|u| u.username)
            .collect();
    assert!(names.contains(&"new-2".to_string()), "{names:?}");
    assert!(
        !names.contains(&"victim-admin".to_string()),
        "the admin deleted its victim: {names:?}"
    );
    assert!(
        names.contains(&"victim-agent".to_string()) && names.contains(&"victim-anon".to_string()),
        "the refused callers deleted nobody: {names:?}"
    );
    // ... and the agent's allowed requests: its own fax is read and has a note
    let f = w.e.store.get(w.ctx.faxes[1]).await.unwrap().unwrap();
    assert!(f.is_read && f.note.as_deref() == Some("a note"));
    assert_eq!(f.read_changed_by.as_deref(), Some("amy"));
    // the refused caller's fax was not touched; the agent could not redeliver theirs
    let untouched = w.e.store.get(w.ctx.faxes[0]).await.unwrap().unwrap();
    assert!(!untouched.is_read && untouched.note.is_none());
    assert_eq!(
        w.e.store
            .list_deliveries(w.ctx.faxes[1])
            .await
            .unwrap()
            .len(),
        1
    );
    assert_eq!(
        w.e.store
            .list_deliveries(w.ctx.faxes[2])
            .await
            .unwrap()
            .len(),
        2,
        "only the administrator's redelivery queued a new attempt"
    );
    w.e.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a11_the_matrix_covers_every_route_of_the_router() {
    // a route that is added without a line in the matrix would escape the permission test: list
    // what the source declares and compare it with the table
    let declared = [
        "GET /faxes",
        "GET /faxes/{id}",
        "PUT /faxes/{id}/read",
        "PUT /faxes/{id}/note",
        "GET /faxes/{id}/pages/{page}",
        "GET /faxes/{id}/{file}",
        "POST /faxes/{id}/redeliver",
        "GET /config",
        "GET /config/audit",
        "PUT /config/trunks/order",
        "GET /config/{section}",
        "PUT /config/{section}",
        "DELETE /config/{section}",
        "GET /users",
        "POST /users",
        "PUT /users/{id}",
        "DELETE /users/{id}",
        "GET /status",
        "GET /events",
        "POST /auth/login",
        "POST /auth/logout",
        "GET /auth/me",
        "POST /auth/password",
    ];
    let shape = |p: &str| -> String {
        // /api/v1/faxes/{fax}/pages/1.png -> /faxes/{id}/pages/{page}
        let p = p.trim_start_matches("/api/v1");
        let p = p.split('?').next().unwrap();
        let parts: Vec<String> = p
            .split('/')
            .map(|s| match s {
                "{fax}" | "{victim}" => "{id}".to_string(),
                "1.png" => "{page}".to_string(),
                "file.pdf" | "file.tif" => "{file}".to_string(),
                s if s.starts_with("trunk:") || s == "sip" || s == "call" => {
                    "{section}".to_string()
                }
                s => s.to_string(),
            })
            .collect();
        parts.join("/")
    };
    let covered: Vec<String> = matrix()
        .iter()
        .map(|m| format!("{} {}", m.method, shape(m.path)))
        .collect();
    // login / logout / password are the session's own (tested in auth.rs); nothing in them
    // depends on the role
    let own_session = [
        "POST /auth/login",
        "POST /auth/logout",
        "POST /auth/password",
    ];
    for d in declared {
        assert!(
            own_session.contains(&d) || covered.iter().any(|c| c == d),
            "{d} has no line in the permission matrix"
        );
    }
    // and what the router really knows: an undeclared path is a 404, not a hidden route
    let e = env().await;
    add_user(&e, "root", Role::Admin, false).await;
    let (c, _) = login(&e, "root").await;
    for p in [
        "/api/v1/debug",
        "/api/v1/faxes/1/extra/x",
        "/metrics",
        "/api/v2/faxes",
    ] {
        let (s, v) = answer(c.get(e.url(p)).send().await.unwrap()).await;
        assert_eq!(s, 404, "{p}");
        assert_eq!(v["error"]["code"], "not_found", "{p}");
    }
    e.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a10_every_route_that_changes_something_refuses_a_missing_or_wrong_csrf_header_or_a_wrong_content_type()
 {
    let w = world().await;
    let routes: Vec<Route> = matrix()
        .into_iter()
        .filter(|m| m.method != Method::GET)
        .collect();
    assert!(routes.len() >= 8, "{}", routes.len());
    // the administrator may do all of these: so anything refused here is refused for the
    // header or the content type alone
    for route in &routes {
        let url = w.e.url(&w.ctx.path(route.path, Who::Admin));
        let body = w.ctx.body(Who::Admin, &route.body);
        let bad = [
            ("no header", None, Some("application/json")),
            (
                "wrong token",
                Some("0".repeat(64)),
                Some("application/json"),
            ),
            ("empty token", Some(String::new()), Some("application/json")),
            (
                "form content type",
                Some(w.admin_csrf.clone()),
                Some("application/x-www-form-urlencoded"),
            ),
            (
                "text content type",
                Some(w.admin_csrf.clone()),
                Some("text/plain"),
            ),
            ("no content type", Some(w.admin_csrf.clone()), None),
        ];
        for (what, token, ctype) in bad {
            let before = snapshot(&w.e, &w.ctx).await;
            let mut rq = w.admin.request(route.method.clone(), &url);
            if let Some(t) = token {
                rq = rq.header("X-Sipfax-Csrf", t);
            }
            if let Some(c) = ctype {
                rq = rq.header("content-type", c);
            }
            if let Some(b) = &body {
                rq = rq.body(b.to_string());
            }
            let (s, v) = answer(rq.send().await.unwrap()).await;
            assert_eq!(s, 403, "{what}: {} {}: {v}", route.method, route.path);
            assert_eq!(
                v["error"]["code"], "csrf",
                "{what}: {} {}",
                route.method, route.path
            );
            assert_eq!(
                snapshot(&w.e, &w.ctx).await,
                before,
                "{what}: {} {} changed the database",
                route.method,
                route.path
            );
        }
        // another account's token does not do either
        let before = snapshot(&w.e, &w.ctx).await;
        let mut rq = w
            .admin
            .request(route.method.clone(), &url)
            .header("X-Sipfax-Csrf", &w.agent_csrf)
            .header("content-type", "application/json");
        if let Some(b) = &body {
            rq = rq.body(b.to_string());
        }
        let (s, _) = answer(rq.send().await.unwrap()).await;
        assert_eq!(s, 403, "token of another session: {}", route.path);
        assert_eq!(snapshot(&w.e, &w.ctx).await, before);
    }
    w.e.stop().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a11_a_session_that_must_change_its_password_is_refused_everywhere_else() {
    let e = env_seeded(SEED).await;
    add_user(&e, "newbie", Role::Admin, true).await;
    let id = fax(&e, "x@gw", "02", 5).await;
    let (c, csrf) = login(&e, "newbie").await;
    let ctx = Ctx {
        faxes: [id; 3],
        victims: [1; 3],
        call_version: 1,
        call_body: json!({}),
        trunk_version: 1,
    };
    for route in matrix() {
        if route.path.ends_with("/auth/me") {
            continue;
        }
        let url = e.url(&ctx.path(route.path, Who::Admin));
        let mut rq = c.request(route.method.clone(), url);
        if route.method != Method::GET {
            rq = rq
                .header("X-Sipfax-Csrf", &csrf)
                .header("content-type", "application/json");
        }
        if let Some(b) = ctx.body(Who::Admin, &route.body) {
            rq = rq.body(b.to_string());
        }
        let (s, v) = answer(rq.send().await.unwrap()).await;
        assert_eq!(
            (s, v["error"]["code"].as_str()),
            (403, Some("password_change_required")),
            "{} {}",
            route.method,
            route.path
        );
    }
    assert_eq!(e.auth.list_users().await.unwrap().len(), 1);
    e.stop().await;
}
