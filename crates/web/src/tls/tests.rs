use super::*;

fn day(y: i32, m: u32, d: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(y, m, d).unwrap()
}

fn cfg() -> WebConfig {
    WebConfig {
        enabled: true,
        ..WebConfig::default()
    }
}

#[test]
fn h01_names_cover_host_localhost_loopback_bind_and_extras() {
    let ips: Vec<IpAddr> = vec![
        "127.0.0.1".parse().unwrap(),
        "192.0.2.10".parse().unwrap(),
        "::1".parse().unwrap(),
        "2001:db8::7".parse().unwrap(),
    ];
    let n = subject_names(
        Some("faxsrv"),
        "0.0.0.0".parse().unwrap(),
        &ips,
        &[
            "fax.example.test".into(),
            "LOCALHOST".into(),
            "198.51.100.4".into(),
        ],
    );
    assert_eq!(
        n,
        [
            "faxsrv",
            "localhost",
            "127.0.0.1",
            "192.0.2.10",
            "2001:db8::7",
            "fax.example.test",
            "198.51.100.4"
        ]
    );
    // a concrete bind address is used as it is, the interfaces are not listed
    let n = subject_names(None, "192.0.2.10".parse().unwrap(), &ips, &[]);
    assert_eq!(n, ["localhost", "127.0.0.1", "192.0.2.10"]);
}

#[test]
fn h01_h02_a_self_signed_certificate_is_made_once_and_reused() {
    let dir = tempfile::tempdir().unwrap();
    let names = vec!["faxsrv".to_string(), "localhost".into(), "127.0.0.1".into()];
    let a = load(&cfg(), dir.path(), dir.path(), &names, day(2026, 10, 22)).unwrap();
    assert_eq!(a.generated, names);
    let (crt, key) = self_signed_paths(dir.path());
    assert_eq!(a.cert_path, crt);
    assert!(crt.is_file() && key.is_file());
    assert_eq!(a.not_after.date_naive(), day(2029, 1, 24), "825 days");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&key).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "the private key is the owner's only");
    }
    let before = fs::read(&crt).unwrap();
    // the second start: the same certificate (a later day must not make a new one)
    let b = load(&cfg(), dir.path(), dir.path(), &names, day(2027, 1, 1)).unwrap();
    assert!(b.generated.is_empty());
    assert_eq!(b.fingerprint, a.fingerprint);
    assert_eq!(fs::read(&crt).unwrap(), before);
    // the fingerprint is SHA-256 over the DER: 32 bytes, upper case, colons
    assert_eq!(a.fingerprint.len(), 32 * 3 - 1);
    assert!(
        a.fingerprint
            .chars()
            .all(|c| c == ':' || c.is_ascii_hexdigit() && !c.is_ascii_lowercase())
    );
}

#[test]
fn h02_a_half_written_pair_is_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let (crt, key) = self_signed_paths(dir.path());
    fs::create_dir_all(crt.parent().unwrap()).unwrap();
    fs::write(&key, b"left over").unwrap();
    let m = load(
        &cfg(),
        dir.path(),
        dir.path(),
        &["localhost".into()],
        day(2026, 1, 1),
    )
    .unwrap();
    assert_eq!(m.generated, ["localhost"]);
    assert!(crt.is_file());
    assert_ne!(fs::read(&key).unwrap(), b"left over");
}

#[test]
fn h03_configured_files_are_used_and_problems_are_named() {
    let dir = tempfile::tempdir().unwrap();
    // make two different pairs to have real files
    let one = dir.path().join("one");
    let two = dir.path().join("two");
    load(&cfg(), &one, &one, &["one.test".into()], day(2026, 1, 1)).unwrap();
    load(&cfg(), &two, &two, &["two.test".into()], day(2026, 1, 1)).unwrap();
    let (c1, k1) = self_signed_paths(&one);
    let (_, k2) = self_signed_paths(&two);
    let mut c = cfg();
    c.cert = c1.display().to_string();
    c.key = k1.display().to_string();
    let m = load(&c, dir.path(), dir.path(), &[], day(2026, 1, 1)).unwrap();
    assert_eq!(m.cert_path, c1);
    assert!(m.generated.is_empty());
    assert!(
        !self_signed_paths(dir.path()).0.exists(),
        "no self-signed one is made"
    );
    // a key that belongs to another certificate
    c.key = k2.display().to_string();
    let e = load(&c, dir.path(), dir.path(), &[], day(2026, 1, 1)).unwrap_err();
    assert!(matches!(e, TlsError::Unusable { .. }), "{e}");
    // a missing file
    c.key = dir.path().join("nope.key").display().to_string();
    let e = load(&c, dir.path(), dir.path(), &[], day(2026, 1, 1))
        .unwrap_err()
        .to_string();
    assert!(
        e.contains("nope.key") && e.contains("does not exist"),
        "{e}"
    );
    // not PEM
    let junk = dir.path().join("junk.crt");
    fs::write(&junk, b"hello").unwrap();
    c.cert = junk.display().to_string();
    c.key = k1.display().to_string();
    let e = load(&c, dir.path(), dir.path(), &[], day(2026, 1, 1))
        .unwrap_err()
        .to_string();
    assert!(e.contains("junk.crt"), "{e}");
}

#[test]
fn h04_the_end_of_validity_is_warned_about_30_days_ahead() {
    let p = Path::new("conf/tls/self-signed.crt");
    let end = DateTime::parse_from_rfc3339("2036-10-22T00:00:00Z")
        .unwrap()
        .to_utc();
    let at = |s: &str| DateTime::parse_from_rfc3339(s).unwrap().to_utc();
    assert!(expiry_warning(p, end, at("2036-09-21T23:59:59Z")).is_none());
    let w = expiry_warning(p, end, at("2036-09-23T00:00:00Z")).unwrap();
    assert!(
        w.contains("2036-10-22") && w.contains("29 day(s) left"),
        "{w}"
    );
    let w = expiry_warning(p, end, at("2036-10-22T00:00:01Z")).unwrap();
    assert!(w.contains("expired"), "{w}");
}

#[test]
fn h04_a_self_signed_certificate_close_to_its_end_is_replaced_and_a_configured_one_is_not() {
    let dir = tempfile::tempdir().unwrap();
    let names = vec!["localhost".to_string()];
    let first = load(&cfg(), dir.path(), dir.path(), &names, day(2026, 10, 22)).unwrap();
    // 31 days before the end: still used
    let keep = load(&cfg(), dir.path(), dir.path(), &names, day(2028, 12, 24)).unwrap();
    assert_eq!(keep.fingerprint, first.fingerprint);
    assert!(keep.replaced.is_none() && keep.generated.is_empty());
    // 29 days before the end: replaced, the old fingerprint is reported
    let new = load(&cfg(), dir.path(), dir.path(), &names, day(2028, 12, 26)).unwrap();
    assert_ne!(new.fingerprint, first.fingerprint);
    assert_eq!(new.replaced.as_deref(), Some(first.fingerprint.as_str()));
    assert_eq!(
        new.not_after.date_naive(),
        day(2028, 12, 26) + Duration::days(825)
    );
    // a configured certificate is used as it is, however close to its end
    let (c, k) = self_signed_paths(dir.path());
    let mut own = cfg();
    own.cert = c.display().to_string();
    own.key = k.display().to_string();
    let later = new.not_after.date_naive() + Duration::days(5);
    let m = load(&own, dir.path(), dir.path(), &names, later).unwrap();
    assert_eq!(m.fingerprint, new.fingerprint);
    assert!(m.replaced.is_none());
    let now = DateTime::<Utc>::from_naive_utc_and_offset(later.and_hms_opt(0, 0, 0).unwrap(), Utc);
    assert!(m.expiry_warning(now).unwrap().contains("expired"));
}

#[test]
fn h02_an_unreadable_self_signed_pair_is_made_again() {
    let dir = tempfile::tempdir().unwrap();
    let (crt, key) = self_signed_paths(dir.path());
    fs::create_dir_all(crt.parent().unwrap()).unwrap();
    fs::write(&crt, b"garbage").unwrap();
    fs::write(&key, b"garbage").unwrap();
    let m = load(
        &cfg(),
        dir.path(),
        dir.path(),
        &["localhost".into()],
        day(2026, 1, 1),
    )
    .unwrap();
    assert_eq!(m.generated, ["localhost"]);
    assert!(m.replaced.is_none());
}
