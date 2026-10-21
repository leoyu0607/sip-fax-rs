//! A01-A08, A11, A12 and the password / session details of M4a (docs/test-plans/M4.md section 2).
//! Real SQLite store, injected clock; nothing waits.

use std::sync::{Arc, Mutex};

use accounts::{
    AccountError, AuthConfig, AuthService, Capability, LoginError, NewAccount, Role, SessionError,
    csrf_for, password,
};
use chrono::Duration;
use store::{ConnectOptions, FaxStore, ManualClock, StoreSource};

const T0: i64 = 1_792_540_800_000;
const PW: &str = "correct horse battery";

struct Env {
    auth: AuthService,
    store: Arc<dyn FaxStore>,
    clock: Arc<ManualClock>,
    _dir: tempfile::TempDir,
}

async fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let clock = Arc::new(ManualClock::at_millis(T0));
    let url = format!("sqlite://{}/a.db", dir.path().display());
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
    Env {
        auth: AuthService::new(src, clock.clone(), AuthConfig::default()),
        store,
        clock,
        _dir: dir,
    }
}

fn acct<'a>(name: &'a str, role: Role) -> NewAccount<'a> {
    NewAccount {
        username: name,
        role,
        password: PW,
        display_name: None,
        locale: None,
        must_change: false,
    }
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

// ---------------------------------------------------------------- A01
#[tokio::test(flavor = "multi_thread")]
async fn a01_first_start_creates_admin_with_a_private_password_file_and_forces_a_change() {
    let e = env().await;
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("conf").join("initial-admin-password");
    let logs = Capture::default();
    let sub = tracing_subscriber::fmt()
        .with_writer(logs.clone())
        .with_ansi(false)
        .finish();
    let _g = tracing::subscriber::set_default(sub);

    let made = e.auth.ensure_initial_admin(file.clone()).await.unwrap();
    assert_eq!(made.as_deref(), Some(file.as_path()));
    let pw = std::fs::read_to_string(&file).unwrap();
    let pw = pw.trim();
    assert_eq!(pw.chars().count(), 16);
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the file is private");
    }
    let admin = e.auth.get_user("admin").await.unwrap();
    assert_eq!(admin.role, Role::Admin);
    assert!(admin.must_change_password);
    // the stored hash is not the password
    let row = e.store.user_get_by_name("admin").await.unwrap().unwrap();
    assert!(!row.password_hash.contains(pw));
    assert!(row.password_hash.starts_with("$argon2id$"));
    // a second call does nothing (an account exists)
    assert!(e.auth.ensure_initial_admin(file.clone()).await.unwrap().is_none());
    assert_eq!(e.store.user_count().await.unwrap(), 1);

    // the first login works and asks for a new password
    let ok = e.auth.login("admin", pw, "10.0.0.1", None).await.unwrap();
    assert!(ok.user.must_change_password);
    // a wrong old password changes nothing and keeps the file
    let err = e
        .auth
        .change_password(ok.user.id, "wrong wrong wrong", "a brand new passphrase", "10.0.0.1", None)
        .await
        .unwrap_err();
    assert!(matches!(err, AccountError::WrongPassword));
    assert!(file.exists(), "the file stays until the password has been changed");
    // a weak new password is refused and keeps the file
    assert!(e.auth.change_password(ok.user.id, pw, "short", "10.0.0.1", None).await.is_err());
    assert!(file.exists());
    // the change succeeds: the file is deleted, the old session is gone, a new one is returned
    let again = e
        .auth
        .change_password(ok.user.id, pw, "a brand new passphrase", "10.0.0.1", None)
        .await
        .unwrap();
    assert!(!again.user.must_change_password);
    assert!(!file.exists(), "initial-admin-password is removed after the first change");
    assert!(matches!(e.auth.authenticate(&ok.token).await, Err(SessionError::Missing)));
    assert!(e.auth.authenticate(&again.token).await.is_ok());
    assert!(
        e.auth.login("admin", pw, "10.0.0.2", None).await.is_err(),
        "the generated password no longer works"
    );
    assert!(e.auth.login("admin", "a brand new passphrase", "10.0.0.2", None).await.is_ok());

    // the password never reached the log
    let text = String::from_utf8(logs.0.lock().unwrap().clone()).unwrap();
    assert!(!text.contains(pw), "log: {text}");
    assert!(!text.contains("a brand new passphrase"));
}

// ---------------------------------------------------------------- A02
#[test]
fn a02_the_hash_is_argon2id_with_the_specified_parameters() {
    let h = password::hash("another long passphrase");
    assert!(h.starts_with("$argon2id$v=19$m=19456,t=2,p=1$"), "{h}");
    assert!(password::verify(&h, "another long passphrase"));
    assert!(!password::verify(&h, "another long passphrasE"));
    assert!(!password::verify("not a hash", "x"));
    assert_ne!(h, password::hash("another long passphrase"), "every hash has its own salt");
}

#[test]
fn a02_an_independent_implementation_accepts_our_hash_and_we_accept_its_hash() {
    // rust-argon2 is a different code base than the argon2 crate behind `password`
    let ours = password::hash("interop passphrase 1");
    assert!(rust_argon2::verify_encoded(&ours, b"interop passphrase 1").unwrap());
    assert!(!rust_argon2::verify_encoded(&ours, b"interop passphrase 2").unwrap());
    let cfg = rust_argon2::Config {
        variant: rust_argon2::Variant::Argon2id,
        version: rust_argon2::Version::Version13,
        mem_cost: 19456,
        time_cost: 2,
        lanes: 1,
        ..rust_argon2::Config::default()
    };
    let theirs = rust_argon2::hash_encoded(b"interop passphrase 3", b"0123456789abcdef", &cfg).unwrap();
    assert!(password::verify(&theirs, "interop passphrase 3"));
    assert!(!password::verify(&theirs, "interop passphrase 4"));
}

/// The reference implementation of OpenSSL (3.2+) in the test image, when Docker is around:
/// `SIPFAX_ORACLE_DOCKER=1 cargo test -p accounts a02_openssl`.
#[test]
fn a02_openssl_argon2id_agrees_with_our_hash() {
    if std::env::var("SIPFAX_ORACLE_DOCKER").is_err() {
        eprintln!("a02_openssl: skipped (set SIPFAX_ORACLE_DOCKER=1; needs docker and the sip-fax-rs-test:rocky9 image)");
        return;
    }
    use base64::Engine;
    let phc = password::hash("openssl oracle passphrase");
    let parts: Vec<&str> = phc.split('$').collect();
    // ["", "argon2id", "v=19", "m=19456,t=2,p=1", salt, hash]
    let salt_b64 = parts[4];
    let hash_b64 = parts[5];
    let nopad = base64::engine::general_purpose::STANDARD_NO_PAD;
    let salt = nopad.decode(salt_b64).unwrap();
    let want = nopad.decode(hash_b64).unwrap();
    let hex = |b: &[u8]| b.iter().map(|x| format!("{x:02x}")).collect::<String>();
    let out = std::process::Command::new("docker")
        .args(["run", "--rm", "sip-fax-rs-test:rocky9", "openssl", "kdf", "-binary", "-keylen"])
        .arg(want.len().to_string())
        .args(["-kdfopt", "pass:openssl oracle passphrase"])
        .arg("-kdfopt")
        .arg(format!("hexsalt:{}", hex(&salt)))
        .args(["-kdfopt", "iter:2", "-kdfopt", "lanes:1", "-kdfopt", "memcost:19456", "ARGON2ID"])
        .output()
        .expect("docker runs");
    assert!(out.status.success(), "{}", String::from_utf8_lossy(&out.stderr));
    assert_eq!(out.stdout, want, "OpenSSL derives the same key");
    // and the negative control: another password gives another key
    let other = std::process::Command::new("docker")
        .args(["run", "--rm", "sip-fax-rs-test:rocky9", "openssl", "kdf", "-binary", "-keylen"])
        .arg(want.len().to_string())
        .args(["-kdfopt", "pass:something else entirely"])
        .arg("-kdfopt")
        .arg(format!("hexsalt:{}", hex(&salt)))
        .args(["-kdfopt", "iter:2", "-kdfopt", "lanes:1", "-kdfopt", "memcost:19456", "ARGON2ID"])
        .output()
        .unwrap();
    assert_ne!(other.stdout, want);
}

// ---------------------------------------------------------------- A03
#[tokio::test(flavor = "multi_thread")]
async fn a03_password_rules_are_refused_with_fixed_codes_and_nothing_is_created() {
    let e = env().await;
    for (user, pw, code) in [
        ("carol", "short", "password_too_short"),
        ("carol", "123456789", "password_too_short"),
        ("carolcarol", "carolcarol", "password_same_as_username"),
        ("carolcarol", "CAROLCAROL", "password_same_as_username"),
        ("carol", &"x".repeat(129), "password_too_long"),
    ] {
        let err = e
            .auth
            .create_user(&NewAccount { password: pw, ..acct(user, Role::Agent) }, "test")
            .await
            .unwrap_err();
        assert_eq!(err.code(), code, "{user}/{pw}");
    }
    assert_eq!(e.store.user_count().await.unwrap(), 0, "no account was created");
    // exactly ten characters pass; so do multi-byte characters counted as characters
    e.auth.create_user(&NewAccount { password: "0123456789", ..acct("carol", Role::Agent) }, "t").await.unwrap();
    e.auth.create_user(&NewAccount { password: "密碼密碼密碼密碼密碼", ..acct("dave", Role::Agent) }, "t").await.unwrap();
    // bad names and roles
    for bad in ["", "Alice", "a b", "a/b", &"a".repeat(65)] {
        let err = e.auth.create_user(&acct(bad, Role::Agent), "t").await.unwrap_err();
        assert_eq!(err.code(), "bad_username", "{bad:?}");
    }
    // a duplicate
    let err = e.auth.create_user(&acct("carol", Role::Agent), "t").await.unwrap_err();
    assert_eq!(err.code(), "username_taken");
}

// ---------------------------------------------------------------- A04
#[tokio::test(flavor = "multi_thread")]
async fn a04_five_failures_lock_the_account_for_fifteen_minutes() {
    let e = env().await;
    let u = e.auth.create_user(&acct("alice", Role::Agent), "t").await.unwrap();
    for i in 0..5 {
        let r = e.auth.login("alice", "wrong password!", &format!("10.0.0.{i}"), None).await;
        assert_eq!(r.unwrap_err(), LoginError::InvalidCredentials);
    }
    // the sixth attempt has the right password and is still refused
    let r = e.auth.login("alice", PW, "10.0.0.9", None).await.unwrap_err();
    assert!(matches!(r, LoginError::Locked { retry_after_secs } if (890..=900).contains(&retry_after_secs)), "{r:?}");
    assert_eq!(r.code(), "account_locked");
    assert_eq!(e.store.session_count_for_user(u.id).await.unwrap(), 0, "no session was opened");
    // still locked one second before the end, free one second after it
    e.clock.advance(Duration::minutes(15) - Duration::seconds(1));
    assert!(matches!(e.auth.login("alice", PW, "10.0.0.9", None).await, Err(LoginError::Locked { .. })));
    e.clock.advance(Duration::seconds(2));
    let ok = e.auth.login("alice", PW, "10.0.0.9", None).await.unwrap();
    assert_eq!(ok.user.username, "alice");
    // a success starts the count again: four more failures do not lock
    for _ in 0..4 {
        let _ = e.auth.login("alice", "wrong password!", "10.0.0.10", None).await;
    }
    assert!(e.auth.login("alice", PW, "10.0.0.10", None).await.is_ok());
}

#[tokio::test(flavor = "multi_thread")]
async fn a04_lockout_does_not_depend_on_the_account_existing() {
    let e = env().await;
    for i in 0..5 {
        let _ = e.auth.login("ghost", "whatever password", &format!("10.1.0.{i}"), None).await;
    }
    let r = e.auth.login("ghost", "whatever password", "10.1.0.9", None).await.unwrap_err();
    assert!(matches!(r, LoginError::Locked { .. }), "a name that does not exist locks like one that does");
}

// ---------------------------------------------------------------- A05
#[tokio::test(flavor = "multi_thread")]
async fn a05_unknown_account_and_wrong_password_look_the_same() {
    let e = env().await;
    e.auth.create_user(&acct("alice", Role::Agent), "t").await.unwrap();
    e.auth.create_user(&NewAccount { password: PW, ..acct("off", Role::Agent) }, "t").await.unwrap();
    e.auth.update_user("off", None, Some(true), None, None, "t").await.unwrap();
    let a = e.auth.login("alice", "wrong password!", "10.0.0.1", None).await.unwrap_err();
    let b = e.auth.login("nobody", "wrong password!", "10.0.0.2", None).await.unwrap_err();
    let c = e.auth.login("off", PW, "10.0.0.3", None).await.unwrap_err();
    assert_eq!(a, b);
    assert_eq!(a, c, "a disabled account answers like a wrong password");
    assert_eq!((a.code(), a.to_string()), (b.code(), b.to_string()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a05_response_times_of_unknown_and_wrong_password_differ_by_less_than_20_percent() {
    let e = env().await;
    e.auth.create_user(&acct("alice", Role::Agent), "t").await.unwrap();
    // a different address every time keeps the rate limit and the lock-out out of the picture
    let mut known = Vec::new();
    let mut unknown = Vec::new();
    for i in 0..50 {
        let t = std::time::Instant::now();
        let _ = e.auth.login("alice", "wrong password!", &format!("10.2.{}.{}", i / 200, i % 200), None).await;
        known.push(t.elapsed());
        e.auth.set_password("alice", PW, false, "t").await.unwrap(); // resets the failure count
        let t = std::time::Instant::now();
        let _ = e.auth.login(&format!("nobody{i}"), "wrong password!", &format!("10.3.{}.{}", i / 200, i % 200), None).await;
        unknown.push(t.elapsed());
    }
    let med = |mut v: Vec<std::time::Duration>| {
        v.sort();
        v[v.len() / 2].as_secs_f64()
    };
    let (k, u) = (med(known), med(unknown));
    let ratio = (k - u).abs() / k.max(u);
    assert!(ratio < 0.20, "median known {k:.4}s vs unknown {u:.4}s (ratio {ratio:.2})");
    assert!(k > 0.001 && u > 0.001, "both paths really hash a password");
}

// ---------------------------------------------------------------- A06
#[tokio::test(flavor = "multi_thread")]
async fn a06_an_address_may_try_twenty_times_a_minute() {
    let e = env().await;
    e.auth.create_user(&acct("alice", Role::Agent), "t").await.unwrap();
    for i in 0..20 {
        // a different unknown name each time: only the address counts here
        let r = e.auth.login(&format!("n{i}"), "wrong password!", "203.0.113.5", None).await;
        assert_eq!(r.unwrap_err(), LoginError::InvalidCredentials, "attempt {i}");
    }
    let r = e.auth.login("alice", PW, "203.0.113.5", None).await.unwrap_err();
    assert!(matches!(r, LoginError::RateLimited { retry_after_secs } if (1..=60).contains(&retry_after_secs)), "{r:?}");
    assert_eq!(r.code(), "rate_limited");
    // another address is not affected, and the right password from this address would have worked
    assert!(e.auth.login("alice", PW, "203.0.113.6", None).await.is_ok());
    e.clock.advance(Duration::seconds(61));
    assert!(e.auth.login("alice", PW, "203.0.113.5", None).await.is_ok(), "the window moved on");
}

// ---------------------------------------------------------------- A07
#[tokio::test(flavor = "multi_thread")]
async fn a07_idle_timeout_is_eight_hours_measured_from_the_last_use() {
    let e = env().await;
    e.auth.create_user(&acct("alice", Role::Agent), "t").await.unwrap();
    let ok = e.auth.login("alice", PW, "10.0.0.1", None).await.unwrap();
    e.clock.advance(Duration::hours(8) - Duration::seconds(1));
    assert!(e.auth.authenticate(&ok.token).await.is_ok(), "one second before the limit");
    // that use counted: the clock starts again
    e.clock.advance(Duration::hours(8) - Duration::seconds(1));
    assert!(e.auth.authenticate(&ok.token).await.is_ok());
    e.clock.advance(Duration::hours(8) + Duration::seconds(1));
    assert_eq!(e.auth.authenticate(&ok.token).await.unwrap_err(), SessionError::Expired);
    // an expired session is gone for good
    assert_eq!(e.auth.authenticate(&ok.token).await.unwrap_err(), SessionError::Missing);
}

#[tokio::test(flavor = "multi_thread")]
async fn a07_absolute_limit_is_seven_days_however_busy_the_user_is() {
    let e = env().await;
    e.auth.create_user(&acct("alice", Role::Agent), "t").await.unwrap();
    let ok = e.auth.login("alice", PW, "10.0.0.1", None).await.unwrap();
    // used every 6 hours: never idle
    for _ in 0..27 {
        e.clock.advance(Duration::hours(6));
        assert!(e.auth.authenticate(&ok.token).await.is_ok());
    }
    // 27 * 6 h = 162 h; the end is at 168 h
    e.clock.advance(Duration::hours(6) - Duration::seconds(1));
    assert!(e.auth.authenticate(&ok.token).await.is_ok(), "one second before 7 days");
    e.clock.advance(Duration::seconds(2));
    assert_eq!(e.auth.authenticate(&ok.token).await.unwrap_err(), SessionError::Expired);
    assert_eq!(ok.expires_at, chrono::DateTime::from_timestamp_millis(T0).unwrap() + Duration::days(7));
}

#[tokio::test(flavor = "multi_thread")]
async fn a07_tokens_are_random_and_only_their_hash_is_stored() {
    let e = env().await;
    e.auth.create_user(&acct("alice", Role::Agent), "t").await.unwrap();
    let a = e.auth.login("alice", PW, "10.0.0.1", None).await.unwrap();
    let b = e.auth.login("alice", PW, "10.0.0.1", None).await.unwrap();
    assert_ne!(a.token, b.token);
    assert!(a.token.len() >= 43, "256 bits");
    assert!(e.store.session_get(&a.token).await.unwrap().is_none(), "the token itself is not stored");
    assert_ne!(a.csrf, b.csrf);
    assert_eq!(a.csrf, csrf_for(&a.token));
    assert!(AuthService::csrf_ok(&a.token, Some(&a.csrf)));
    assert!(!AuthService::csrf_ok(&a.token, Some(&b.csrf)), "another session's token");
    assert!(!AuthService::csrf_ok(&a.token, Some("")));
    assert!(!AuthService::csrf_ok(&a.token, None));
    // unknown and empty tokens
    assert_eq!(e.auth.authenticate("").await.unwrap_err(), SessionError::Missing);
    assert_eq!(e.auth.authenticate("garbage").await.unwrap_err(), SessionError::Missing);
    // old sessions can be purged
    e.clock.advance(Duration::days(8));
    assert_eq!(e.auth.purge_expired_sessions().await.unwrap(), 2);
}

// ---------------------------------------------------------------- A08
#[tokio::test(flavor = "multi_thread")]
async fn a08_logout_disable_and_password_reset_end_the_sessions() {
    let e = env().await;
    e.auth.create_user(&acct("root", Role::Admin), "t").await.unwrap();
    let u = e.auth.create_user(&acct("alice", Role::Agent), "t").await.unwrap();
    // logout: only that session
    let s1 = e.auth.login("alice", PW, "10.0.0.1", Some("UA")).await.unwrap();
    let s2 = e.auth.login("alice", PW, "10.0.0.1", None).await.unwrap();
    e.auth.logout(&s1.token).await.unwrap();
    assert_eq!(e.auth.authenticate(&s1.token).await.unwrap_err(), SessionError::Missing);
    assert!(e.auth.authenticate(&s2.token).await.is_ok(), "other sessions are not touched");
    // disable: every session, and no new login
    let s3 = e.auth.login("alice", PW, "10.0.0.1", None).await.unwrap();
    e.auth.update_user("alice", None, Some(true), None, None, "root").await.unwrap();
    for t in [&s2.token, &s3.token] {
        assert!(e.auth.authenticate(t).await.is_err());
    }
    assert_eq!(e.store.session_count_for_user(u.id).await.unwrap(), 0);
    assert_eq!(e.auth.login("alice", PW, "10.0.0.2", None).await.unwrap_err(), LoginError::InvalidCredentials);
    // enable again, then reset the password: sessions end, the old password stops working
    e.auth.update_user("alice", None, Some(false), None, None, "root").await.unwrap();
    let s4 = e.auth.login("alice", PW, "10.0.0.3", None).await.unwrap();
    e.auth.set_password("alice", "a completely new one", true, "root").await.unwrap();
    assert!(e.auth.authenticate(&s4.token).await.is_err());
    assert!(e.auth.login("alice", PW, "10.0.0.4", None).await.is_err());
    let again = e.auth.login("alice", "a completely new one", "10.0.0.4", None).await.unwrap();
    assert!(again.user.must_change_password, "a reset password has to be changed");
    // a session whose account is disabled behind its back is refused and removed
    e.store
        .user_update(u.id, &store::UserUpdate { disabled: Some(true), ..Default::default() })
        .await
        .unwrap();
    assert_eq!(e.auth.authenticate(&again.token).await.unwrap_err(), SessionError::Disabled);
    assert_eq!(e.store.session_count_for_user(u.id).await.unwrap(), 0);
    // operations are in the audit trail with the operator
    let rows = e.store.config_audit_list(50, None).await.unwrap();
    let find = |a: &str| rows.iter().any(|r| r.action == a && r.by == "root" && r.section == "user:alice");
    assert!(find("user_disable") && find("user_enable") && find("password_reset"), "{rows:?}");
}

// ---------------------------------------------------------------- A11
#[test]
fn a11_the_permission_matrix_matches_the_specification() {
    // docs/design/m4-web.md 2.1: (capability, agent, admin)
    let matrix = [
        (Capability::ViewFaxes, true, true),
        (Capability::EditFaxMeta, true, true),
        (Capability::ViewDeliveries, true, true),
        (Capability::Redeliver, false, true),
        (Capability::ViewConfig, false, true),
        (Capability::EditConfig, false, true),
        (Capability::ManageUsers, false, true),
        (Capability::ViewAudit, false, true),
        (Capability::ViewStatus, true, true),
    ];
    assert_eq!(matrix.len(), Capability::ALL.len(), "every capability is in the table");
    for cap in Capability::ALL {
        assert!(matrix.iter().any(|m| m.0 == cap), "{cap:?}");
    }
    for (cap, agent, admin) in matrix {
        assert_eq!(Role::Agent.can(cap), agent, "agent {cap:?}");
        assert_eq!(Role::Admin.can(cap), admin, "admin {cap:?}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn a11_require_refuses_and_unauthenticated_requests_never_get_that_far() {
    let e = env().await;
    e.auth.create_user(&acct("alice", Role::Agent), "t").await.unwrap();
    e.auth.create_user(&acct("root", Role::Admin), "t").await.unwrap();
    let a = e.auth.login("alice", PW, "10.0.0.1", None).await.unwrap();
    let r = e.auth.login("root", PW, "10.0.0.1", None).await.unwrap();
    let a = e.auth.authenticate(&a.token).await.unwrap();
    let r = e.auth.authenticate(&r.token).await.unwrap();
    assert!(a.require(Capability::ViewFaxes).is_ok());
    assert_eq!(a.require(Capability::EditConfig).unwrap_err().role, Role::Agent);
    assert!(a.require(Capability::ManageUsers).is_err());
    assert!(r.require(Capability::EditConfig).is_ok());
    assert_eq!(SessionError::Missing.code(), "unauthenticated");
    assert_eq!(e.auth.authenticate("").await.unwrap_err().code(), "unauthenticated");
}

// ---------------------------------------------------------------- A12
#[tokio::test(flavor = "multi_thread")]
async fn a12_the_last_enabled_admin_cannot_be_disabled_demoted_or_deleted() {
    let e = env().await;
    e.auth.create_user(&acct("root", Role::Admin), "t").await.unwrap();
    e.auth.create_user(&acct("alice", Role::Agent), "t").await.unwrap();
    let before = e.store.user_get_by_name("root").await.unwrap().unwrap();
    for r in [
        e.auth.update_user("root", None, Some(true), None, None, "t").await.map(|_| ()),
        e.auth.update_user("root", Some(Role::Agent), None, None, None, "t").await.map(|_| ()),
        e.auth.delete_user("root", "t").await,
    ] {
        let err = r.unwrap_err();
        assert_eq!(err.code(), "last_admin");
    }
    let after = e.store.user_get_by_name("root").await.unwrap().unwrap();
    assert_eq!(before, after, "nothing was changed");
    // with a second admin the first can go
    e.auth.create_user(&acct("boss", Role::Admin), "t").await.unwrap();
    e.auth.update_user("root", Some(Role::Agent), None, None, None, "t").await.unwrap();
    assert_eq!(e.store.user_count_active_admins().await.unwrap(), 1);
    // and now boss is the last one
    assert_eq!(e.auth.delete_user("boss", "t").await.unwrap_err().code(), "last_admin");
    // a disabled admin does not count as one
    e.auth.update_user("root", Some(Role::Admin), Some(true), None, None, "t").await.unwrap();
    assert_eq!(e.store.user_count_active_admins().await.unwrap(), 1);
    // changing display name or locale of the last admin is fine
    let u = e.auth.update_user("boss", None, None, Some(Some("Boss".into())), Some(Some("en".into())), "t").await.unwrap();
    assert_eq!((u.display_name.as_deref(), u.locale.as_deref()), (Some("Boss"), Some("en")));
    assert_eq!(e.auth.get_user("nobody").await.unwrap_err().code(), "not_found");
}

#[tokio::test(flavor = "multi_thread")]
async fn a12_agents_can_be_deleted_with_their_sessions() {
    let e = env().await;
    e.auth.create_user(&acct("root", Role::Admin), "t").await.unwrap();
    e.auth.create_user(&acct("alice", Role::Agent), "t").await.unwrap();
    let s = e.auth.login("alice", PW, "10.0.0.1", None).await.unwrap();
    e.auth.delete_user("alice", "root").await.unwrap();
    assert_eq!(e.auth.authenticate(&s.token).await.unwrap_err(), SessionError::Missing);
    assert_eq!(e.auth.list_users().await.unwrap().len(), 1);
    assert_eq!(e.auth.delete_user("alice", "root").await.unwrap_err().code(), "not_found");
}
