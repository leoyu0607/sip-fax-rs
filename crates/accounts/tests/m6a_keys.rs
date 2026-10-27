//! M6a (ADR-070) API keys at the level of the account service: K01 (what is stored), K04 (every
//! kind of bad key), K07 (the refusals of the management calls), K08 (the audit), K09 (the last
//! use, at most once a minute) and K12 (the same on the server databases).
//!
//! The database is a SQLite file, or, with `SIP_FAXD_TEST_DB=<url>` (the db-matrix job), that
//! server: the cases use key names of their own and leave nothing the next run trips over.

use std::sync::{Arc, Mutex};

use accounts::{
    ApiKeyAdminError, ApiKeyError, AuthConfig, AuthService, Capability, KEY_PREFIX,
    apikey::PREFIX_LEN, apikey::TOUCH_EVERY_SECS,
};
use chrono::{DateTime, Duration, Utc};
use store::{ConnectOptions, FaxStore, ManualClock, StoreSource};

const T0: i64 = 1_792_540_800_000;

struct Env {
    auth: AuthService,
    store: Arc<dyn FaxStore>,
    clock: Arc<ManualClock>,
    /// A suffix that makes the key names of this run and case unique on a shared server.
    tag: String,
    sqlite_file: Option<std::path::PathBuf>,
    _dir: tempfile::TempDir,
}

static SEQ: std::sync::atomic::AtomicU32 = std::sync::atomic::AtomicU32::new(0);

async fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::at_millis(T0));
    let (url, file) = match std::env::var("SIP_FAXD_TEST_DB") {
        Ok(u) if !u.trim().is_empty() && !u.starts_with("sqlite") => (u, None),
        _ => {
            let f = dir.path().join("k.db");
            (format!("sqlite://{}", f.display()), Some(f))
        }
    };
    let store = store::connect_with(
        &url,
        ConnectOptions {
            max_connections: 4,
            auto_migrate: true,
            clock: clock.clone(),
        },
    )
    .await
    .unwrap();
    let src: Arc<dyn StoreSource> = Arc::new(store.clone());
    let tag = format!(
        "{}x{}",
        std::process::id(),
        SEQ.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
    );
    Env {
        auth: AuthService::new(src, clock.clone(), AuthConfig::default()),
        store,
        clock,
        tag,
        sqlite_file: file,
        _dir: dir,
    }
}

impl Env {
    fn name(&self, n: &str) -> String {
        format!("{n}-{}", self.tag)
    }
    fn now(&self) -> DateTime<Utc> {
        use store::Clock;
        self.clock.now()
    }
}

fn sha256_hex(s: &str) -> String {
    ring::digest::digest(&ring::digest::SHA256, s.as_bytes())
        .as_ref()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Captures what `tracing` writes.
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);
impl std::io::Write for Capture {
    fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(b);
        Ok(b.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = Capture;
    fn make_writer(&'a self) -> Capture {
        self.clone()
    }
}

// ---------------------------------------------------------------- K01
#[tokio::test(flavor = "multi_thread")]
async fn k01_a_key_is_shown_once_and_only_its_hash_is_stored() {
    let e = env().await;
    let logs = Capture::default();
    let _g = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .finish(),
    );
    let exp = e.now() + Duration::days(30) + Duration::milliseconds(123);
    let (ro, rw) = (e.name("ro"), e.name("rw"));
    let a = e
        .auth
        .create_api_key(&ro, false, None, "boss")
        .await
        .unwrap();
    let b = e
        .auth
        .create_api_key(&rw, true, Some(exp), "boss")
        .await
        .unwrap();
    for c in [&a, &b] {
        assert!(c.key.starts_with(KEY_PREFIX));
        assert_eq!(c.key.len(), 47, "sfx_ and 43 characters: {}", c.key);
        assert!(
            c.key[4..]
                .bytes()
                .all(|x| x.is_ascii_alphanumeric() || x == b'-' || x == b'_'),
            "base64url"
        );
    }
    assert_ne!(a.key, b.key);
    assert_eq!(a.info.scopes, ["fax:read"]);
    assert_eq!(b.info.scopes, ["fax:read", "fax:write"]);
    assert_eq!(
        b.info.expires_at.map(|t| t.timestamp_millis()),
        Some(exp.timestamp_millis())
    );
    assert_eq!(a.info.expires_at, None);
    assert_eq!(a.info.created_by, "boss");
    assert_eq!(a.info.prefix, &a.key[..PREFIX_LEN]);

    // the database has the SHA-256 and the prefix, nothing else of the key
    let rows = e.store.api_key_list().await.unwrap();
    let mine: Vec<_> = rows.iter().filter(|r| r.name.ends_with(&e.tag)).collect();
    assert_eq!(mine.len(), 2);
    for (c, name) in [(&a, &ro), (&b, &rw)] {
        let row = mine.iter().find(|r| &r.name == name).unwrap();
        assert_eq!(row.token_hash, sha256_hex(&c.key));
        assert_eq!(row.prefix, c.key[..12]);
        assert!(
            !format!("{row:?}").contains(&c.key[12..]),
            "no more than the prefix"
        );
    }
    // the list (what the interface shows) has no key either
    let listed = e.auth.list_api_keys().await.unwrap();
    for c in [&a, &b] {
        assert!(!format!("{listed:?}").contains(&c.key[12..]));
    }
    // the audit rows, the log and (SQLite) the file: not the key
    let audit = e.store.config_audit_list(200, None).await.unwrap();
    for c in [&a, &b] {
        assert!(!format!("{audit:?}").contains(&c.key[12..]), "audit");
        let log = String::from_utf8_lossy(&logs.0.lock().unwrap()).into_owned();
        assert!(!log.contains(&c.key[12..]), "log: {log}");
        if let Some(f) = &e.sqlite_file {
            let bytes = std::fs::read(f).unwrap();
            let needle = c.key.as_bytes();
            assert!(
                !bytes.windows(needle.len()).any(|w| w == needle),
                "the database file does not contain the key"
            );
        }
    }
    // both keys work, with the rights they were given
    let ra = e
        .auth
        .authenticate_api_key(&a.key, Some("10.0.0.1"))
        .await
        .unwrap();
    let rb = e.auth.authenticate_api_key(&b.key, None).await.unwrap();
    assert_eq!(ra.name, ro);
    assert!(ra.allows(Capability::ViewFaxes) && !ra.allows(Capability::EditFaxMeta));
    assert!(rb.allows(Capability::EditFaxMeta));
    for cap in Capability::ALL {
        if !matches!(cap, Capability::ViewFaxes | Capability::EditFaxMeta) {
            assert!(!rb.allows(cap), "{cap:?}");
        }
    }
    assert_eq!(rb.actor(), format!("api:{rw}"));
}

// ---------------------------------------------------------------- K04
#[tokio::test(flavor = "multi_thread")]
async fn k04_every_bad_key_is_the_same_error_and_revoking_works_at_once() {
    let e = env().await;
    let live = e
        .auth
        .create_api_key(&e.name("live"), false, None, "boss")
        .await
        .unwrap();
    let doomed = e
        .auth
        .create_api_key(&e.name("doomed"), false, None, "boss")
        .await
        .unwrap();
    let short = e
        .auth
        .create_api_key(
            &e.name("short"),
            false,
            Some(e.now() + Duration::minutes(10)),
            "boss",
        )
        .await
        .unwrap();
    e.auth.authenticate_api_key(&live.key, None).await.unwrap();
    // unknown, malformed, a key of the right shape that never existed
    let flipped = {
        let mut k = live.key.clone().into_bytes();
        let last = k.len() - 1;
        k[last] = if k[last] == b'A' { b'B' } else { b'A' };
        String::from_utf8(k).unwrap()
    };
    let bad = [
        String::new(),
        "sfx_".to_string(),
        "garbage".to_string(),
        live.key[1..].to_string(),
        format!("{}x", live.key),
        live.key.to_uppercase(),
        live.key.replace("sfx_", "SFX_"),
        flipped,
        format!("sfx_{}", "A".repeat(43)),
        format!("sfx_{}", "+".repeat(43)),
    ];
    let mut messages = Vec::new();
    for k in &bad {
        match e.auth.authenticate_api_key(k, None).await {
            Err(ApiKeyError::Invalid) => messages.push(ApiKeyError::Invalid.to_string()),
            other => panic!("{k:?}: {other:?}"),
        }
    }
    // revoked: from the next request on
    e.auth
        .authenticate_api_key(&doomed.key, None)
        .await
        .unwrap();
    e.auth
        .revoke_api_key(&e.name("doomed"), "boss")
        .await
        .unwrap();
    assert!(matches!(
        e.auth.authenticate_api_key(&doomed.key, None).await,
        Err(ApiKeyError::Invalid)
    ));
    // expired: valid until the second before the end, refused at the end
    e.clock
        .advance(Duration::minutes(10) - Duration::milliseconds(1));
    e.auth.authenticate_api_key(&short.key, None).await.unwrap();
    e.clock.advance(Duration::milliseconds(1));
    assert!(matches!(
        e.auth.authenticate_api_key(&short.key, None).await,
        Err(ApiKeyError::Invalid)
    ));
    // one message for all of them: nothing tells which keys exist
    messages.push(ApiKeyError::Invalid.to_string());
    messages.dedup();
    assert_eq!(messages.len(), 1);
    // the live key is not affected by any of it
    e.auth.authenticate_api_key(&live.key, None).await.unwrap();
}

// ---------------------------------------------------------------- K07 (service level)
#[tokio::test(flavor = "multi_thread")]
async fn k07_names_expiry_and_repeats_are_refused() {
    let e = env().await;
    for bad in [
        "",
        " ",
        "has space",
        "slash/name",
        "ümlaut",
        "名稱",
        "semi;colon",
        &"a".repeat(49),
    ] {
        match e.auth.create_api_key(bad, false, None, "boss").await {
            Err(ApiKeyAdminError::BadName) => {}
            other => panic!("{bad:?}: {other:?}"),
        }
    }
    let ok = e.name("Mixed-Case");
    let c = e
        .auth
        .create_api_key(&format!("  {ok}  "), false, None, "boss")
        .await
        .unwrap();
    assert_eq!(c.info.name, ok.to_ascii_lowercase(), "trimmed, lower case");
    // 48 characters are fine
    let long = format!("{:a<48}", format!("l{}", e.tag));
    e.auth
        .create_api_key(&long, false, None, "boss")
        .await
        .unwrap();
    // the past and the present are no expiry
    for bad in [e.now() - Duration::days(1), e.now()] {
        assert!(matches!(
            e.auth
                .create_api_key(&e.name("late"), false, Some(bad), "boss")
                .await,
            Err(ApiKeyAdminError::BadExpiry)
        ));
    }
    // the name is taken, in any case, and also after the key was revoked
    assert!(matches!(
        e.auth
            .create_api_key(&ok.to_ascii_uppercase(), true, None, "boss")
            .await,
        Err(ApiKeyAdminError::Duplicate)
    ));
    e.auth.revoke_api_key(&ok, "boss").await.unwrap();
    assert!(matches!(
        e.auth.create_api_key(&ok, false, None, "boss").await,
        Err(ApiKeyAdminError::Duplicate)
    ));
    assert!(matches!(
        e.auth.revoke_api_key(&ok, "boss").await,
        Err(ApiKeyAdminError::AlreadyRevoked)
    ));
    assert!(matches!(
        e.auth.revoke_api_key(&e.name("nobody"), "boss").await,
        Err(ApiKeyAdminError::NotFound)
    ));
    // the refusals changed nothing: the two keys, one of them revoked
    let mine: Vec<_> = e
        .auth
        .list_api_keys()
        .await
        .unwrap()
        .into_iter()
        .filter(|k| k.name.ends_with(&e.tag) || k.name == long)
        .collect();
    assert_eq!(mine.len(), 2);
    assert_eq!(mine.iter().filter(|k| k.revoked_at.is_some()).count(), 1);
    let codes: Vec<_> = [
        ApiKeyAdminError::BadName,
        ApiKeyAdminError::BadExpiry,
        ApiKeyAdminError::Duplicate,
        ApiKeyAdminError::NotFound,
        ApiKeyAdminError::AlreadyRevoked,
    ]
    .iter()
    .map(ApiKeyAdminError::code)
    .collect();
    assert_eq!(
        codes,
        [
            "bad_name",
            "bad_expiry",
            "name_taken",
            "not_found",
            "already_revoked"
        ]
    );
}

// ---------------------------------------------------------------- K08
#[tokio::test(flavor = "multi_thread")]
async fn k08_creating_and_revoking_are_audited_without_the_key() {
    let e = env().await;
    let name = e.name("audit");
    let exp = e.now() + Duration::days(2);
    let c = e
        .auth
        .create_api_key(&name, true, Some(exp), "boss")
        .await
        .unwrap();
    e.clock.advance(Duration::minutes(3));
    e.auth.revoke_api_key(&name, "second-admin").await.unwrap();
    // failures write nothing
    let _ = e.auth.create_api_key(&name, false, None, "boss").await;
    let _ = e.auth.revoke_api_key(&name, "boss").await;
    let section = format!("api_key:{name}");
    let rows: Vec<_> = e
        .store
        .config_audit_list(200, None)
        .await
        .unwrap()
        .into_iter()
        .filter(|r| r.section == section)
        .collect();
    assert_eq!(rows.len(), 2, "{rows:?}");
    let mut by_action: Vec<_> = rows
        .iter()
        .map(|r| (r.action.as_str(), r.by.as_str()))
        .collect();
    by_action.sort();
    assert_eq!(
        by_action,
        [("key_create", "boss"), ("key_revoke", "second-admin")]
    );
    let created = rows.iter().find(|r| r.action == "key_create").unwrap();
    let after = created.after_json.as_deref().unwrap();
    assert!(after.contains("fax:read,fax:write"), "{after}");
    assert!(
        after.contains(&c.key[..12]),
        "the prefix tells which key: {after}"
    );
    assert!(
        serde_json_ok(after),
        "the audit text is JSON as the list of the interface expects: {after}"
    );
    for r in &rows {
        let all = format!("{r:?}");
        assert!(!all.contains(&c.key[12..]));
    }
}

/// A rough JSON check without a JSON dependency: balanced braces and quotes.
fn serde_json_ok(s: &str) -> bool {
    s.starts_with('{')
        && s.ends_with('}')
        && s.matches('"').count().is_multiple_of(2)
        && s.matches('{').count() == s.matches('}').count()
}

// ---------------------------------------------------------------- K09
#[tokio::test(flavor = "multi_thread")]
async fn k09_the_last_use_is_written_at_most_once_a_minute() {
    let e = env().await;
    let name = e.name("used");
    let c = e
        .auth
        .create_api_key(&name, false, None, "boss")
        .await
        .unwrap();
    let row = |e: &Env| {
        let name = name.clone();
        let st = e.store.clone();
        async move {
            st.api_key_list()
                .await
                .unwrap()
                .into_iter()
                .find(|k| k.name == name)
                .unwrap()
        }
    };
    let r = row(&e).await;
    assert_eq!(
        (r.last_used_at, r.last_used_ip),
        (None, None),
        "not used yet"
    );
    // the first use is recorded with the time and the address
    e.clock.advance(Duration::seconds(5));
    let first = e.now();
    e.auth
        .authenticate_api_key(&c.key, Some("198.51.100.7"))
        .await
        .unwrap();
    let r = row(&e).await;
    assert_eq!(
        r.last_used_at.map(|t| t.timestamp_millis()),
        Some(first.timestamp_millis())
    );
    assert_eq!(r.last_used_ip.as_deref(), Some("198.51.100.7"));
    // within the minute: nothing is written (neither the time nor a new address)
    for secs in [1, 20, 59] {
        e.clock.set(first + Duration::seconds(secs));
        e.auth
            .authenticate_api_key(&c.key, Some("203.0.113.1"))
            .await
            .unwrap();
        let r = row(&e).await;
        assert_eq!(
            r.last_used_at.map(|t| t.timestamp_millis()),
            Some(first.timestamp_millis()),
            "after {secs} s"
        );
        assert_eq!(r.last_used_ip.as_deref(), Some("198.51.100.7"));
    }
    // a minute later: written again
    e.clock.set(first + Duration::seconds(TOUCH_EVERY_SECS));
    e.auth
        .authenticate_api_key(&c.key, Some("203.0.113.1"))
        .await
        .unwrap();
    let r = row(&e).await;
    assert_eq!(
        r.last_used_at.map(|t| t.timestamp_millis()),
        Some((first + Duration::seconds(TOUCH_EVERY_SECS)).timestamp_millis())
    );
    assert_eq!(r.last_used_ip.as_deref(), Some("203.0.113.1"));
    // a refused key does not count as a use
    let before = r.last_used_at;
    e.clock.advance(Duration::minutes(5));
    let mut wrong = c.key.clone();
    wrong.replace_range(5..6, if &c.key[5..6] == "A" { "B" } else { "A" });
    assert!(
        e.auth
            .authenticate_api_key(&wrong, Some("192.0.2.1"))
            .await
            .is_err()
    );
    assert_eq!(row(&e).await.last_used_at, before);
}
