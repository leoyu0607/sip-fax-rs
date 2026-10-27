use super::*;

fn parse(text: &str) -> Result<WebConfig, String> {
    let c: WebConfig = toml::from_str(text).map_err(|e| e.to_string())?;
    c.validate()?;
    Ok(c)
}

#[test]
fn h00_defaults_are_on_https_on_8443() {
    let c = WebConfig::default();
    assert!(c.enabled);
    assert_eq!(c.scheme, Scheme::Https);
    assert_eq!(c.bind.port(), 8443);
    assert_eq!(c.session_idle_hours, 8);
    assert_eq!(parse("").unwrap(), c);
}

#[test]
fn h00_cert_and_key_go_together_and_need_https() {
    assert!(parse("cert = \"a.crt\"").unwrap_err().contains("together"));
    assert!(parse("key = \"a.key\"").unwrap_err().contains("together"));
    let e = parse("scheme = \"http\"\ncert = \"a\"\nkey = \"b\"").unwrap_err();
    assert!(e.contains("https"), "{e}");
    let c = parse("cert = \"conf/tls/web.crt\"\nkey = \"/etc/web.key\"").unwrap();
    let (crt, key) = c.cert_files(Path::new("/opt/sip-faxd")).unwrap();
    assert_eq!(crt, Path::new("/opt/sip-faxd/conf/tls/web.crt"));
    assert_eq!(key, Path::new("/etc/web.key"));
    assert!(WebConfig::default().cert_files(Path::new("/x")).is_none());
}

#[test]
fn h00_bad_values_are_refused() {
    assert!(parse("session_idle_hours = 0").is_err());
    assert!(parse("tls_names = [\"a b\"]").is_err());
    assert!(parse("tls_names = [\"\"]").is_err());
    assert!(parse("bogus = 1").is_err());
    assert!(parse("scheme = \"ftp\"").is_err());
    assert!(parse("bind = \"not an address\"").is_err());
}

#[test]
fn h06_plain_http_off_loopback_is_warned_about() {
    let mut c = parse("scheme = \"http\"\nbind = \"0.0.0.0:8080\"").unwrap();
    assert!(c.plaintext_warning().unwrap().contains("unencrypted"));
    c.bind = "127.0.0.1:8080".parse().unwrap();
    assert!(c.plaintext_warning().is_none());
    c.bind = "[::1]:8080".parse().unwrap();
    assert!(c.plaintext_warning().is_none());
    c.scheme = Scheme::Https;
    c.bind = "0.0.0.0:8443".parse().unwrap();
    assert!(c.plaintext_warning().is_none());
}
