use super::*;

// ---- tests carried over from Sipress ----

#[test]
fn parse_digest_challenge() {
    let raw = "SIP/2.0 401 Unauthorized\r\n\
                   Via: SIP/2.0/UDP 1.2.3.4:5060\r\n\
                   WWW-Authenticate: Digest realm=\"sip.example.com\", \
                   nonce=\"abc123\", algorithm=MD5, qop=\"auth\"\r\n\
                   \r\n";
    let c = DigestChallenge::parse(raw).expect("should parse");
    assert_eq!(c.realm, "sip.example.com");
    assert_eq!(c.nonce, "abc123");
    assert_eq!(c.algorithm, "MD5");
    assert_eq!(c.qop.as_deref(), Some("auth"));
}

#[test]
fn www_authenticate_uses_authorization_header() {
    let raw = "SIP/2.0 401 Unauthorized\r\n\
                   WWW-Authenticate: Digest realm=\"r\", nonce=\"n\"\r\n\r\n";
    let c = DigestChallenge::parse(raw).unwrap();
    assert!(!c.proxy);
    assert!(
        c.authorization_header("u", "p", "REGISTER", "sip:x")
            .starts_with("Authorization: Digest ")
    );
}

#[test]
fn proxy_authenticate_uses_proxy_authorization_header() {
    let raw = "SIP/2.0 407 Proxy Authentication Required\r\n\
                   Proxy-Authenticate: Digest realm=\"r\", nonce=\"n\"\r\n\r\n";
    let c = DigestChallenge::parse(raw).unwrap();
    assert!(c.proxy);
    assert!(
        c.authorization_header("u", "p", "REGISTER", "sip:x")
            .starts_with("Proxy-Authorization: Digest ")
    );
}

#[test]
fn build_auth_response() {
    let c = DigestChallenge {
        realm: "sip.example.com".into(),
        nonce: "abc123".into(),
        algorithm: "MD5".into(),
        qop: None,
        opaque: None,
        proxy: false,
    };
    let h = c.build_authorization("alice", "secret", "REGISTER", "sip:1.2.3.4:5060");
    assert!(h.contains("username=\"alice\""));
    assert!(h.contains("response=\""));
    // Independent check of the RFC 2069 response: MD5(HA1:nonce:HA2) computed by hand here.
    let ha1 = md5_hex("alice:sip.example.com:secret");
    let ha2 = md5_hex("REGISTER:sip:1.2.3.4:5060");
    let expected = md5_hex(&format!("{ha1}:abc123:{ha2}"));
    assert!(h.contains(&format!("response=\"{expected}\"")), "{h}");
}

// ---- additional tests ----

/// RFC 2617 section 3.5 worked example (MD5, qop=auth) as an independent known answer.
#[test]
fn rfc2617_known_answer() {
    let c = DigestChallenge::from_header_value(
        "Digest realm=\"testrealm@host.com\", qop=\"auth,auth-int\", \
             nonce=\"dcd98b7102dd2f0e8b11d0f600bfb0c093\", \
             opaque=\"5ccc069c403ebaf9f0171e9517f40e41\"",
        false,
    )
    .unwrap();
    let v = c.build_authorization_with_cnonce(
        "Mufasa",
        "Circle Of Life",
        "GET",
        "/dir/index.html",
        "0a4f113b",
    );
    assert!(
        v.contains("response=\"6629fae49393a05397450978507c4ef1\""),
        "{v}"
    );
    assert!(v.contains("qop=auth, nc=00000001, cnonce=\"0a4f113b\""));
    assert!(v.contains("opaque=\"5ccc069c403ebaf9f0171e9517f40e41\""));
}

#[test]
fn random_cnonce_is_used_and_varies() {
    let c =
        DigestChallenge::from_header_value("Digest realm=\"r\", nonce=\"n\", qop=\"auth\"", false)
            .unwrap();
    let a = c.build_authorization("u", "p", "INVITE", "sip:x");
    let b = c.build_authorization("u", "p", "INVITE", "sip:x");
    assert!(a.contains("cnonce=\""));
    assert_ne!(a, b, "two 32-bit random cnonces collided");
}

#[test]
fn rejects_non_digest_and_incomplete_challenges() {
    assert!(DigestChallenge::from_header_value("Basic realm=\"x\"", false).is_none());
    assert!(DigestChallenge::from_header_value("Digest realm=\"x\"", false).is_none());
    assert!(DigestChallenge::from_header_value("Digest", false).is_none());
    assert!(DigestChallenge::from_header_value("", false).is_none());
    assert!(DigestChallenge::parse("SIP/2.0 401 Unauthorized\r\n\r\n").is_none());
}

#[test]
fn folded_challenge_and_quoted_comma() {
    let raw = "SIP/2.0 401 Unauthorized\r\n\
                   WWW-Authenticate: Digest realm=\"a,b\",\r\n nonce=\"n1\"\r\n\
                   Content-Length: 0\r\n\r\n";
    let c = DigestChallenge::parse(raw).unwrap();
    assert_eq!(c.realm, "a,b");
    assert_eq!(c.nonce, "n1");
    assert_eq!(c.algorithm, "MD5", "default algorithm");
}
