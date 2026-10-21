//! K01-K06, K08, K10-K12 (docs/test-plans/M4.md section 1) on a real SQLite store. The
//! "applier" is a recorder (what `sip-faxd` implements with FaxIn and the delivery service);
//! the store is never replaced by a double.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};

use async_trait::async_trait;
use base64::Engine;
use ring::aead::{AES_256_GCM, Aad, LessSafeKey, Nonce, UnboundKey};
use runtime_config::{
    ApplyReport, Boot, ConfigApplier, ConfigService, ImportError, KeyStatus, RuntimeConfig,
    RuntimeFile, SecretBox, Source, StoreSource, UpdateError, to_toml,
};
use serde_json::{Value, json};
use store::{ConnectOptions, FaxStore, ManualClock};

const FILE: &str = r#"
[sip]
bind = "127.0.0.1:5060"
advertised = "192.0.2.10"
[media]
port_range = "30000-30099"
[call]
station_id = "0212345678"
max_duration_secs = 600
[capture]
enabled = false
[delivery]
max_attempts = 5
[retention]
files_days = 30
records_days = 400
run_at = "02:30"

[[trunks]]
name = "zeta"
preset = "cht-cs210"
peer = "203.0.113.10:5060"
[trunks.register]
enabled = true
username = "fax"
password = "reg-secret-zeta"

[[trunks]]
name = "alpha"
allowed_sources = ["198.51.100.0/24"]
[trunks.numbers]
strip_plus = true

[[routes]]
name = "sales"
dnis_prefix = "2557"
[[routes.deliveries]]
kind = "email"
host = "smtp.example.com"
from = "fax@example.com"
to = ["a@example.com"]
username = "fax"
password = "mail-secret"
[[routes.deliveries]]
kind = "webhook"
url = "https://example.com/hook"
secret = "hmac-secret"

[[routes]]
default = true
[[routes.deliveries]]
kind = "folder"
dir = "/srv/other"
"#;

const SECRETS: [&str; 3] = ["reg-secret-zeta", "mail-secret", "hmac-secret"];

struct Flaky {
    inner: Arc<dyn FaxStore>,
    down: AtomicBool,
}
#[async_trait]
impl StoreSource for Flaky {
    async fn store(&self) -> store::Result<Arc<dyn FaxStore>> {
        if self.down.load(Ordering::SeqCst) {
            Err(store::Error::Unavailable("connection refused".into()))
        } else {
            Ok(self.inner.clone())
        }
    }
}

#[derive(Default)]
struct Recorder(Mutex<Vec<(Vec<String>, Vec<String>)>>);
#[async_trait]
impl ConfigApplier for Recorder {
    async fn apply(&self, old: &RuntimeConfig, new: &RuntimeConfig) -> ApplyReport {
        let names = |c: &RuntimeConfig| c.trunks.iter().map(|t| t.name.clone()).collect::<Vec<_>>();
        self.0.lock().unwrap().push((names(old), names(new)));
        ApplyReport::default()
    }
}

struct Fx {
    dir: tempfile::TempDir,
    store: Arc<dyn FaxStore>,
    flaky: Arc<Flaky>,
    svc: ConfigService,
    rec: Arc<Recorder>,
}

fn file_cfg(text: &str) -> (RuntimeConfig, Vec<String>) {
    let f = RuntimeFile::parse(text).expect("the test file is valid");
    (f.runtime, f.present)
}

async fn open_store(dir: &std::path::Path) -> Arc<dyn FaxStore> {
    store::connect_with(
        &format!("sqlite://{}/c.db", dir.display()),
        ConnectOptions {
            max_connections: 4,
            auto_migrate: true,
            clock: Arc::new(ManualClock::at_millis(1_792_540_800_000)),
        },
    )
    .await
    .unwrap()
}

fn service(flaky: &Arc<Flaky>, key: &std::path::Path, text: &str) -> (ConfigService, Arc<Recorder>) {
    let (file, present) = file_cfg(text);
    let svc = ConfigService::new(
        flaky.clone(),
        Arc::new(SecretBox::load_or_create(key)),
        file,
        present,
    );
    let rec = Arc::new(Recorder::default());
    svc.set_applier(rec.clone());
    (svc, rec)
}

async fn fx() -> Fx {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let flaky = Arc::new(Flaky {
        inner: store.clone(),
        down: AtomicBool::new(false),
    });
    let (svc, rec) = service(&flaky, &dir.path().join("secret.key"), FILE);
    Fx { dir, store, flaky, svc, rec }
}

fn trunk_names(c: &RuntimeConfig) -> Vec<String> {
    c.trunks.iter().map(|t| t.name.clone()).collect()
}

// ---------------------------------------------------------------- K01
#[tokio::test(flavor = "multi_thread")]
async fn k01_first_start_imports_the_whole_file_and_reads_it_back_field_for_field() {
    let f = fx().await;
    let (file, _) = file_cfg(FILE);
    assert_eq!(f.svc.source(), Source::FileFallback, "before bootstrap only the file is known");
    let Boot::Imported(rep) = f.svc.bootstrap().await else {
        panic!("expected an import")
    };
    assert!(rep.written.contains(&"trunk:zeta".to_string()) && rep.written.contains(&"routes".to_string()));
    assert_eq!(f.svc.source(), Source::Database);
    let got = f.svc.current();
    assert!(got.same_as(&file), "the runtime configuration equals the file, field for field");
    assert_eq!(trunk_names(&got), ["zeta", "alpha"], "trunk order is kept");
    assert_eq!(
        got.routes.routes.iter().map(|r| r.default).collect::<Vec<_>>(),
        [false, true],
        "route order is kept"
    );
    // a fresh service that only reads the database sees the same thing
    let (svc2, _) = service(&f.flaky, &f.dir.path().join("secret.key"), "");
    let Boot::FromDatabase { .. } = svc2.bootstrap().await else {
        panic!("the second start reads the database")
    };
    assert!(svc2.current().same_as(&file));
    // every imported section has an audit row by `config-file`
    let audit = f.store.config_audit_list(100, None).await.unwrap();
    let sections: Vec<_> = audit.iter().filter(|r| r.action == "import").collect();
    assert!(sections.len() >= 9, "{sections:?}");
    assert!(audit.iter().all(|r| r.by == "config-file"), "{audit:?}");
}

// ---------------------------------------------------------------- K02
#[tokio::test(flavor = "multi_thread")]
async fn k02_the_second_start_keeps_the_database_and_names_the_ignored_sections() {
    let f = fx().await;
    f.svc.bootstrap().await;
    let before = f.svc.current();
    let versions = f.svc.versions();
    let changed = FILE.replace("max_duration_secs = 600", "max_duration_secs = 123").replace("files_days = 30", "files_days = 31");
    let (svc2, _) = service(&f.flaky, &f.dir.path().join("secret.key"), &changed);
    match svc2.bootstrap().await {
        Boot::FromDatabase { ignored } => {
            for s in ["sip", "media", "call", "capture", "trunks", "routes", "delivery", "retention"] {
                assert!(ignored.contains(&s.to_string()), "{s} in {ignored:?}");
            }
        }
        other => panic!("{other:?}"),
    }
    assert!(svc2.current().same_as(&before), "the database wins");
    assert_eq!(svc2.current().call.max_duration_secs, 600);
    assert_eq!(f.svc.versions(), versions);
    assert_eq!(svc2.versions(), versions, "nothing was written");
}

// ---------------------------------------------------------------- K03
#[tokio::test(flavor = "multi_thread")]
async fn k03_import_without_force_refuses_and_with_force_replaces() {
    let f = fx().await;
    f.svc.bootstrap().await;
    let versions = f.svc.versions();
    let audit_n = f.store.config_audit_list(1000, None).await.unwrap().len();
    let (other, _) = file_cfg(&FILE.replace("max_duration_secs = 600", "max_duration_secs = 77"));
    let err = f.svc.import(&other, false, "cli:root").await.unwrap_err();
    assert!(matches!(err, ImportError::AlreadyConfigured(n) if n >= 8), "{err}");
    assert_eq!(f.svc.versions(), versions, "the database is untouched");
    assert_eq!(f.store.config_audit_list(1000, None).await.unwrap().len(), audit_n);
    assert_eq!(f.svc.current().call.max_duration_secs, 600);
    // force: replaced, extra trunks removed, audited with the operator
    let only_alpha = FILE.split("[[trunks]]\nname = \"zeta\"").next().unwrap().to_string()
        + "[[trunks]]\nname = \"alpha\"\nallowed_sources = [\"198.51.100.0/24\"]\n";
    let (small, _) = file_cfg(&only_alpha.replace("max_duration_secs = 600", "max_duration_secs = 77"));
    let rep = f.svc.import(&small, true, "cli:root").await.unwrap();
    assert_eq!(rep.removed, ["trunk:zeta"]);
    f.svc.reload().await.unwrap();
    assert_eq!(f.svc.current().call.max_duration_secs, 77);
    assert_eq!(trunk_names(&f.svc.current()), ["alpha"]);
    let audit = f.store.config_audit_list(1000, None).await.unwrap();
    assert!(audit.iter().any(|r| r.by == "cli:root" && r.action == "import" && r.section == "call"));
    assert!(audit.iter().any(|r| r.by == "cli:root" && r.action == "delete" && r.section == "trunk:zeta"));
}

// ---------------------------------------------------------------- K04
#[tokio::test(flavor = "multi_thread")]
async fn k04_export_then_import_into_an_empty_database_gives_the_same_configuration() {
    let f = fx().await;
    f.svc.bootstrap().await;
    let original = f.svc.current();
    let full = f.svc.export(true).await.unwrap();
    let plain = f.svc.export(false).await.unwrap();
    for s in SECRETS {
        assert!(full.contains(s), "--include-secrets has {s}");
        assert!(!plain.contains(s), "the default export has no {s}");
    }
    assert!(plain.contains("WITHOUT secrets"));
    // into an empty database (another directory, another key)
    let dir2 = tempfile::tempdir().unwrap();
    let store2 = open_store(dir2.path()).await;
    let src2 = Arc::new(Flaky { inner: store2.clone(), down: AtomicBool::new(false) });
    let (svc2, _) = service(&src2, &dir2.path().join("secret.key"), "");
    let parsed = RuntimeFile::parse(&full).unwrap();
    svc2.import(&parsed.runtime, true, "cli:root").await.unwrap();
    svc2.reload().await.unwrap();
    assert!(svc2.current().same_as(&original), "the round trip changes nothing, secrets included");
    assert_eq!(trunk_names(&svc2.current()), ["zeta", "alpha"]);
    // the file without secrets re-imports, but the trunk that registers needs its password again
    let err = RuntimeFile::parse(&plain).unwrap_err();
    assert!(err.contains("register.password"), "the trunk that registers needs its password again: {err}");
    // the exported text is what to_toml writes for the in-memory configuration
    assert_eq!(full, to_toml(&original, true));
}

// ---------------------------------------------------------------- K05
fn field_errors(e: UpdateError) -> Vec<(String, String)> {
    match e {
        UpdateError::Invalid(v) => v.into_iter().map(|f| (f.field, f.code)).collect(),
        other => panic!("{other:?}"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn k05_invalid_sections_are_refused_field_by_field_and_nothing_changes() {
    let f = fx().await;
    f.svc.bootstrap().await;
    let versions = f.svc.versions();
    let before = f.svc.current();
    let audit_n = f.store.config_audit_list(1000, None).await.unwrap().len();
    let applied_n = f.rec.0.lock().unwrap().len();
    let zeta = f.svc.get_section("trunk:zeta").await.unwrap();
    let routes = f.svc.get_section("routes").await.unwrap();

    let mut t = zeta.body.clone();
    t["fax"]["resolutions"] = json!([]);
    let mut preset = zeta.body.clone();
    preset["preset"] = json!("no-such-preset");
    let mut unknown = zeta.body.clone();
    unknown["surprise"] = json!(1);
    let mut wrong = zeta.body.clone();
    wrong["t38"]["max_datagram"] = json!("big");
    let mut r = routes.body.clone();
    r["routes"].as_array_mut().unwrap().swap(0, 1); // the default route is first now

    let cases: Vec<(&str, Value, i32, &str, &str)> = vec![
        ("trunk:zeta", t, zeta.version, "fax.resolutions", "empty"),
        ("trunk:zeta", preset, zeta.version, "preset", "unknown_preset"),
        ("trunk:zeta", unknown, zeta.version, "surprise", "unknown_field"),
        ("trunk:zeta", wrong, zeta.version, "t38.max_datagram", "wrong_type"),
        ("routes", r, routes.version, "routes[0]", "not_last"),
        ("media", json!({"port_range": "31000-30000"}), 1, "port_range", "reversed"),
        ("media", json!({"port_range": "banana"}), 1, "port_range", "invalid"),
        ("call", json!({"max_duration_secs": 0, "station_id": "x".repeat(21)}), 1, "max_duration_secs", "out_of_range"),
        ("retention", json!({"files_days": 90, "records_days": 30}), 1, "records_days", "out_of_range"),
        ("retention", json!({"run_at": "25:61"}), 1, "run_at", "invalid"),
        ("delivery", json!({"max_attempts": 0}), 1, "max_attempts", "out_of_range"),
        ("sip", json!({"user_agent": "  "}), 1, "user_agent", "empty"),
    ];
    for (section, body, version, field, code) in cases {
        let v = if section == "media" || section == "call" || section == "retention" || section == "delivery" || section == "sip" {
            f.svc.versions().get(section).copied().unwrap_or(version)
        } else {
            version
        };
        let errs = field_errors(f.svc.update(section, body, v, "admin").await.unwrap_err());
        assert!(errs.contains(&(field.to_string(), code.to_string())), "{section}: {errs:?}");
    }
    // the second problem of a body is reported too (more than one field at once)
    let errs = field_errors(
        f.svc
            .update("call", json!({"max_duration_secs": 0, "station_id": "x".repeat(21)}), f.svc.versions()["call"], "admin")
            .await
            .unwrap_err(),
    );
    assert!(errs.contains(&("max_duration_secs".into(), "out_of_range".into())) && errs.contains(&("station_id".into(), "too_long".into())), "{errs:?}");
    // a trunk whose name does not match its section
    let mut renamed = zeta.body.clone();
    renamed["name"] = json!("other");
    let errs = field_errors(f.svc.update("trunk:zeta", renamed, zeta.version, "admin").await.unwrap_err());
    assert!(errs.contains(&("name".into(), "mismatch".into())), "{errs:?}");
    // unknown section names
    assert!(matches!(f.svc.update("nonsense", json!({}), 0, "a").await, Err(UpdateError::UnknownSection(_))));
    assert!(matches!(f.svc.update("trunk:", json!({}), 0, "a").await, Err(UpdateError::UnknownSection(_))));
    // nothing moved: versions, memory, audit, applier
    assert_eq!(f.svc.versions(), versions);
    assert!(f.svc.current().same_as(&before));
    assert_eq!(f.store.config_audit_list(1000, None).await.unwrap().len(), audit_n);
    assert_eq!(f.rec.0.lock().unwrap().len(), applied_n);
}

#[tokio::test(flavor = "multi_thread")]
async fn k05_a_change_that_breaks_another_section_is_refused() {
    let f = fx().await;
    f.svc.bootstrap().await;
    // zeta registers: a port-0 bind cannot be combined with registration (checked across sections)
    let sip = f.svc.get_section("sip").await.unwrap();
    let mut body = sip.body.clone();
    body["bind"] = json!("127.0.0.1:0");
    let errs = field_errors(f.svc.update("sip", body, sip.version, "admin").await.unwrap_err());
    assert_eq!(errs.len(), 1);
    assert_eq!(errs[0].0, "", "a problem of the whole section");
    assert_eq!(f.svc.current().sip.bind.port(), 5060);
}

// ---------------------------------------------------------------- K06
#[tokio::test(flavor = "multi_thread")]
async fn k06_two_administrators_editing_the_same_section_only_one_wins() {
    let f = fx().await;
    f.svc.bootstrap().await;
    let call = f.svc.get_section("call").await.unwrap();
    let mk = |secs: u64| {
        let mut b = call.body.clone();
        b["max_duration_secs"] = json!(secs);
        b
    };
    let (a, b) = tokio::join!(
        f.svc.update("call", mk(111), call.version, "alice"),
        f.svc.update("call", mk(222), call.version, "bob"),
    );
    let (ok, err) = match (&a, &b) {
        (Ok(_), Err(_)) => (a, b),
        (Err(_), Ok(_)) => (b, a),
        other => panic!("exactly one must win: {other:?}"),
    };
    let ok = ok.unwrap();
    assert_eq!(ok.version, call.version + 1);
    match err.unwrap_err() {
        UpdateError::Conflict { current } => assert_eq!(current, call.version + 1, "the loser learns the current version"),
        other => panic!("{other:?}"),
    }
    let now = f.svc.current().call.max_duration_secs;
    assert!(now == 111 || now == 222);
    // a stale version later is a conflict as well, and does not change anything
    let stale = f.svc.update("call", mk(333), call.version, "carol").await.unwrap_err();
    assert!(matches!(stale, UpdateError::Conflict { current } if current == call.version + 1));
    assert_eq!(f.svc.current().call.max_duration_secs, now);
    // the current version works
    f.svc.update("call", mk(333), call.version + 1, "carol").await.unwrap();
    assert_eq!(f.svc.current().call.max_duration_secs, 333);
}

// ---------------------------------------------------------------- K08
#[tokio::test(flavor = "multi_thread")]
async fn k08_settings_that_need_a_restart_are_marked_and_listed_until_the_restart() {
    let f = fx().await;
    f.svc.bootstrap().await;
    assert!(f.svc.pending_restart().is_empty());
    let sip = f.svc.get_section("sip").await.unwrap();
    let mut body = sip.body.clone();
    body["bind"] = json!("127.0.0.1:5070");
    let out = f.svc.update("sip", body, sip.version, "admin").await.unwrap();
    assert_eq!(out.restart_required, ["sip.bind"]);
    assert_eq!(out.pending_restart, ["sip.bind"]);
    let media = f.svc.get_section("media").await.unwrap();
    let mut body = media.body.clone();
    body["port_range"] = json!("31000-31099");
    let out = f.svc.update("media", body, media.version, "admin").await.unwrap();
    assert_eq!(out.restart_required, ["media.port_range"], "only this section's fields");
    assert_eq!(out.pending_restart, ["sip.bind", "media.port_range"]);
    assert_eq!(f.svc.status().pending_restart, ["sip.bind", "media.port_range"]);
    // the new values are stored and in the snapshot (they apply at the next start)
    assert_eq!(f.svc.current().sip.bind.port(), 5070);
    // a change that applies at once is not marked
    let call = f.svc.get_section("call").await.unwrap();
    let mut body = call.body.clone();
    body["send_ringing"] = json!(false);
    let out = f.svc.update("call", body, call.version, "admin").await.unwrap();
    assert!(out.restart_required.is_empty());
    // "restart": a new service on the same database starts with these values; nothing pending
    let (svc2, _) = service(&f.flaky, &f.dir.path().join("secret.key"), "");
    svc2.bootstrap().await;
    assert!(svc2.pending_restart().is_empty());
    assert_eq!(svc2.current().sip.bind.port(), 5070);
}

// ---------------------------------------------------------------- K10
fn independent_decrypt(key: &[u8], aad: &str, stored: &str) -> String {
    // deliberately not using SecretBox: ring directly, from the documented format
    let b64 = stored.strip_prefix("enc:v1:").expect("enc:v1: prefix");
    let data = base64::engine::general_purpose::STANDARD.decode(b64).unwrap();
    let (nonce, ct) = data.split_at(12);
    let key = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).unwrap());
    let mut buf = ct.to_vec();
    let plain = key
        .open_in_place(Nonce::try_assume_unique_for_key(nonce).unwrap(), Aad::from(aad.as_bytes()), &mut buf)
        .expect("the ciphertext authenticates");
    String::from_utf8(plain.to_vec()).unwrap()
}

#[tokio::test(flavor = "multi_thread")]
async fn k10_secrets_are_encrypted_in_the_database_and_never_shown() {
    let f = fx().await;
    f.svc.bootstrap().await;
    let key = std::fs::read(f.dir.path().join("secret.key")).unwrap();
    assert_eq!(key.len(), 32);
    // the stored trunk
    let row = f.store.config_get("trunk:zeta").await.unwrap().unwrap();
    let body: Value = serde_json::from_str(&row.body).unwrap();
    let stored = body["register"]["password"].as_str().unwrap();
    assert!(stored.starts_with("enc:v1:"));
    assert!(!row.body.contains("reg-secret-zeta"));
    assert_eq!(independent_decrypt(&key, "trunk:zeta/register.password", stored), "reg-secret-zeta");
    // the stored routes
    let row = f.store.config_get("routes").await.unwrap().unwrap();
    let body: Value = serde_json::from_str(&row.body).unwrap();
    let mail = body["routes"][0]["deliveries"][0]["password"].as_str().unwrap();
    let hook = body["routes"][0]["deliveries"][1]["secret"].as_str().unwrap();
    assert_eq!(independent_decrypt(&key, "routes/routes[0].deliveries[0].password", mail), "mail-secret");
    assert_eq!(independent_decrypt(&key, "routes/routes[0].deliveries[1].secret", hook), "hmac-secret");
    // a ciphertext does not authenticate for another field (the context is authenticated data)
    let moved = std::panic::catch_unwind(|| independent_decrypt(&key, "trunk:alpha/register.password", stored));
    assert!(moved.is_err(), "moving a ciphertext to another field must fail");
    // the same plain text encrypts differently every time
    assert_ne!(mail, hook);
    // the API view: set / not set, never the value
    let v = f.svc.get_section("trunk:zeta").await.unwrap();
    assert_eq!(v.body["register"]["password"]["set"], json!(true));
    let all = serde_json::to_string(&f.svc.list_sections().await.unwrap()).unwrap();
    for s in SECRETS {
        assert!(!all.contains(s), "the section list shows {s}");
    }
    let alpha = f.svc.get_section("trunk:alpha").await.unwrap();
    assert_eq!(alpha.body["register"]["password"], json!({"set": false}));
    // changing the secret: audit rows have *** and nowhere does the plain text appear
    let mut b = v.body.clone();
    b["register"]["password"] = json!("brand-new-secret");
    f.svc.update("trunk:zeta", b, v.version, "admin").await.unwrap();
    assert_eq!(f.svc.current().trunks[0].register.password.as_deref(), Some("brand-new-secret"));
    let audit = f.store.config_audit_list(100, None).await.unwrap();
    let upd = audit.iter().find(|r| r.action == "update" && r.section == "trunk:zeta").unwrap();
    assert!(upd.before_json.as_deref().unwrap().contains("\"password\":\"***\""));
    assert!(upd.after_json.as_deref().unwrap().contains("\"password\":\"***\""));
    for row in &audit {
        for text in [&row.before_json, &row.after_json].into_iter().flatten() {
            for s in SECRETS.iter().chain(["brand-new-secret"].iter()) {
                assert!(!text.contains(s), "audit row {} leaks {s}", row.id);
            }
        }
    }
    for row in f.store.config_list().await.unwrap() {
        for s in SECRETS.iter().chain(["brand-new-secret"].iter()) {
            assert!(!row.body.contains(s), "section {} stores {s} in clear", row.name);
        }
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn k10_keeping_a_secret_with_the_set_marker_survives_reordering() {
    let f = fx().await;
    f.svc.bootstrap().await;
    let routes = f.svc.get_section("routes").await.unwrap();
    // swap the two deliveries of the first route: the markers carry their own reference
    let mut b = routes.body.clone();
    b["routes"][0]["deliveries"].as_array_mut().unwrap().reverse();
    f.svc.update("routes", b, routes.version, "admin").await.unwrap();
    let cfg = f.svc.current();
    let ds = &cfg.routes.routes[0].deliveries;
    match (&ds[0], &ds[1]) {
        (delivery::DeliveryConfig::Webhook(w), delivery::DeliveryConfig::Email(m)) => {
            assert_eq!(w.secret.as_deref(), Some("hmac-secret"), "the secret moved with its delivery");
            assert_eq!(m.password.as_deref(), Some("mail-secret"));
        }
        other => panic!("{other:?}"),
    }
    // a marker without anything stored behind it is refused
    let alpha = f.svc.get_section("trunk:alpha").await.unwrap();
    let mut a = alpha.body.clone();
    a["register"]["enabled"] = json!(true);
    a["register"]["username"] = json!("u");
    a["register"]["registrar"] = json!("198.51.100.1");
    a["register"]["password"] = json!({"set": true});
    let errs = field_errors(f.svc.update("trunk:alpha", a, alpha.version, "admin").await.unwrap_err());
    assert!(errs.contains(&("register.password".into(), "required".into())), "{errs:?}");
    // clearing it with null / set:false
    let zeta = f.svc.get_section("trunk:zeta").await.unwrap();
    let mut z = zeta.body.clone();
    z["register"]["enabled"] = json!(false);
    z["register"]["password"] = Value::Null;
    f.svc.update("trunk:zeta", z, zeta.version, "admin").await.unwrap();
    assert!(f.svc.current().trunks[0].register.password.is_none());
}

// ---------------------------------------------------------------- K11
#[tokio::test(flavor = "multi_thread")]
async fn k11_a_lost_key_costs_the_secrets_and_nothing_else() {
    let f = fx().await;
    f.svc.bootstrap().await;
    drop(f.svc);
    // the key file is lost and a new start creates another
    std::fs::remove_file(f.dir.path().join("secret.key")).unwrap();
    let (svc2, _) = service(&f.flaky, &f.dir.path().join("secret.key"), "");
    assert_eq!(svc2.secrets().status(), &KeyStatus::Created);
    svc2.bootstrap().await;
    let cfg = svc2.current();
    // everything else is intact
    assert_eq!(trunk_names(&cfg), ["zeta", "alpha"]);
    assert_eq!(cfg.call.max_duration_secs, 600);
    assert_eq!(cfg.routes.routes.len(), 2);
    // the three secrets are reported and replaced by a placeholder that no peer accepts
    let st = svc2.status();
    assert_eq!(st.undecryptable.len(), 3, "{:?}", st.undecryptable);
    assert!(st.undecryptable.contains(&"trunk:zeta: register.password".to_string()));
    assert_eq!(cfg.trunks[0].register.password.as_deref(), Some(runtime_config::UNDECRYPTABLE));
    // the placeholder is not a value: an export with secrets leaves it out
    assert!(!svc2.export(true).await.unwrap().contains("undecryptable"));
    // the trunk list and the views still work and the secret shows as set (a value is stored)
    let zeta = svc2.get_section("trunk:zeta").await.unwrap();
    // keeping the unreadable secret is refused with a clear reason, entering it again works
    let mut kept = zeta.body.clone();
    kept["call_dummy"] = Value::Null;
    kept.as_object_mut().unwrap().remove("call_dummy");
    let errs = field_errors(svc2.update("trunk:zeta", kept, zeta.version, "admin").await.unwrap_err());
    assert!(errs.contains(&("register.password".into(), "secret_key".into())), "{errs:?}");
    let mut again = zeta.body.clone();
    again["register"]["password"] = json!("entered-again");
    svc2.update("trunk:zeta", again, zeta.version, "admin").await.unwrap();
    assert_eq!(svc2.current().trunks[0].register.password.as_deref(), Some("entered-again"));
    assert_eq!(svc2.status().undecryptable.len(), 2, "the two others still need to be entered");
    // a key file of the wrong size is moved aside, not overwritten
    let path = f.dir.path().join("secret.key");
    std::fs::write(&path, b"too short").unwrap();
    let boxed = SecretBox::load_or_create(&path);
    match boxed.status() {
        KeyStatus::Replaced(old) => assert_eq!(std::fs::read(old).unwrap(), b"too short"),
        other => panic!("{other:?}"),
    }
    assert_eq!(std::fs::read(&path).unwrap().len(), 32);
    // without any key: reading works, changing a secret does not, changing anything else does
    let (svc3, _) = {
        let (file, present) = file_cfg("");
        let s = ConfigService::new(f.flaky.clone(), Arc::new(SecretBox::unavailable("read-only disk")), file, present);
        (s, ())
    };
    svc3.bootstrap().await;
    assert!(svc3.status().key.starts_with("unavailable"));
    let zeta = svc3.get_section("trunk:zeta").await.unwrap();
    let mut b = zeta.body.clone();
    b["register"]["password"] = json!("x-new-secret");
    let errs = field_errors(svc3.update("trunk:zeta", b, zeta.version, "admin").await.unwrap_err());
    assert!(errs.iter().any(|e| e.0 == "register.password" && e.1 == "secret_key"));
    let call = svc3.get_section("call").await.unwrap();
    let mut c = call.body.clone();
    c["send_ringing"] = json!(false);
    svc3.update("call", c, call.version, "admin").await.unwrap();
}

// ---------------------------------------------------------------- K12
#[tokio::test(flavor = "multi_thread")]
async fn k12_without_the_database_the_snapshot_stays_and_changes_are_refused() {
    let f = fx().await;
    f.svc.bootstrap().await;
    let before = f.svc.current();
    f.flaky.down.store(true, Ordering::SeqCst);
    let call = f.svc.get_section("call").await;
    assert!(matches!(call, Err(UpdateError::Unavailable(_))));
    let mut body = serde_json::to_value(&before.call).unwrap();
    body["send_ringing"] = json!(false);
    let err = f.svc.update("call", body.clone(), 1, "admin").await.unwrap_err();
    assert!(matches!(err, UpdateError::Unavailable(_)), "{err}");
    assert!(f.svc.current().same_as(&before), "calls keep using the snapshot");
    assert!(f.svc.current().call.send_ringing);
    assert!(matches!(f.svc.reload().await, Err(UpdateError::Unavailable(_))));
    // the database comes back: changes work again
    f.flaky.down.store(false, Ordering::SeqCst);
    f.svc.update("call", body, f.svc.versions()["call"], "admin").await.unwrap();
    assert!(!f.svc.current().call.send_ringing);
}

#[tokio::test(flavor = "multi_thread")]
async fn k12_a_start_without_the_database_runs_on_the_file_until_it_is_back() {
    let dir = tempfile::tempdir().unwrap();
    let store = open_store(dir.path()).await;
    let flaky = Arc::new(Flaky { inner: store.clone(), down: AtomicBool::new(true) });
    let (svc, rec) = service(&flaky, &dir.path().join("secret.key"), FILE);
    let (file, _) = file_cfg(FILE);
    assert!(matches!(svc.bootstrap().await, Boot::Unreachable(_)));
    assert_eq!(svc.source(), Source::FileFallback);
    assert!(svc.current().same_as(&file), "the file's configuration is in use");
    flaky.down.store(false, Ordering::SeqCst);
    assert!(matches!(svc.retry_bootstrap().await, Boot::Imported(_)));
    assert_eq!(svc.source(), Source::Database);
    assert!(svc.current().same_as(&file));
    assert!(store.config_get("sip").await.unwrap().is_some(), "imported once the database was there");
    let _ = rec;
}

// ---------------------------------------------------------------- trunks and reload
#[tokio::test(flavor = "multi_thread")]
async fn k01_trunks_can_be_added_reordered_and_deleted_and_each_change_is_applied() {
    let f = fx().await;
    f.svc.bootstrap().await;
    let body = json!({"name": "beta", "allowed_sources": ["192.0.2.0/24"], "preset": "generic"});
    let out = f.svc.update("trunk:beta", body.clone(), 0, "admin").await.unwrap();
    assert_eq!(out.version, 1);
    assert_eq!(trunk_names(&f.svc.current()), ["zeta", "alpha", "beta"], "a new trunk goes last");
    // creating again is a conflict
    assert!(matches!(f.svc.update("trunk:beta", body, 0, "admin").await, Err(UpdateError::Conflict { .. })));
    f.svc.reorder_trunks(vec!["beta".into(), "zeta".into(), "alpha".into()], "admin").await.unwrap();
    assert_eq!(trunk_names(&f.svc.current()), ["beta", "zeta", "alpha"]);
    assert!(matches!(
        f.svc.reorder_trunks(vec!["beta".into()], "admin").await,
        Err(UpdateError::Invalid(_))
    ));
    f.svc.delete_trunk("zeta", f.svc.versions()["trunk:zeta"], "admin").await.unwrap();
    assert_eq!(trunk_names(&f.svc.current()), ["beta", "alpha"]);
    assert!(matches!(f.svc.delete_trunk("zeta", 1, "admin").await, Err(UpdateError::NotFound)));
    assert!(matches!(
        f.svc.delete_trunk("alpha", 99, "admin").await,
        Err(UpdateError::Conflict { .. })
    ));
    assert_eq!(trunk_names(&f.svc.current()), ["beta", "alpha"]);
    // the listing has the fixed sections first and then the trunks in match order
    let names: Vec<String> = f.svc.list_sections().await.unwrap().into_iter().map(|s| s.name).collect();
    assert_eq!(&names[7..], ["trunk:beta", "trunk:alpha"]);
    // every successful change was handed to the applier with the old and the new trunk list
    let calls = f.rec.0.lock().unwrap().clone();
    assert!(calls.contains(&(vec!["zeta".into(), "alpha".into()], vec!["zeta".into(), "alpha".into(), "beta".into()])));
    assert!(calls.contains(&(vec!["beta".into(), "zeta".into(), "alpha".into()], vec!["beta".into(), "alpha".into()])));
    // the audit trail has the delete
    let audit = f.store.config_audit_list(100, None).await.unwrap();
    assert!(audit.iter().any(|r| r.action == "delete" && r.section == "trunk:zeta" && r.by == "admin"));
}

#[tokio::test(flavor = "multi_thread")]
async fn k07_reload_picks_up_what_another_process_wrote_and_applies_only_differences() {
    let f = fx().await;
    f.svc.bootstrap().await;
    let r = f.svc.reload().await.unwrap();
    assert!(!r.changed, "nothing differs");
    let n = f.rec.0.lock().unwrap().len();
    // the CLI (another service object on the same database) changes a section
    let (cli, _) = service(&f.flaky, &f.dir.path().join("secret.key"), "");
    cli.bootstrap().await;
    let call = cli.get_section("call").await.unwrap();
    let mut b = call.body.clone();
    b["answer_delay_ms"] = json!(250);
    cli.update("call", b, call.version, "cli:root").await.unwrap();
    assert_eq!(f.svc.current().call.answer_delay_ms, 0, "the running service has not noticed");
    let r = f.svc.reload().await.unwrap();
    assert!(r.changed);
    assert_eq!(f.svc.current().call.answer_delay_ms, 250);
    assert_eq!(f.rec.0.lock().unwrap().len(), n + 1);
    // subscribers see the new snapshot
    let rx = f.svc.subscribe();
    assert_eq!(rx.borrow().call.answer_delay_ms, 250);
}
