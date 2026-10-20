//! Extra (non-plan) tests of the typed message building blocks: URIs, name-addr, Via, CSeq,
//! methods, status codes and the header list. IDs `U..` are this crate's own.

use sip_core::message::*;

#[test]
fn u01_sip_uri_parse_and_display() {
    let cases = [
        ("sip:alice@example.com", "sip:alice@example.com"),
        (
            "SIP:alice:pw@example.com:5070;transport=tcp;lr?subject=hi&x=1",
            "sip:alice:pw@example.com:5070;transport=tcp;lr?subject=hi&x=1",
        ),
        ("sips:[2001:db8::1]:5061", "sips:[2001:db8::1]:5061"),
        ("sip:host.example.com;lr", "sip:host.example.com;lr"),
        (
            "tel:+1-201-555-0123;phone-context=example.com",
            "tel:+1-201-555-0123;phone-context=example.com",
        ),
    ];
    for (input, canonical) in cases {
        let u = SipUri::parse(input).unwrap_or_else(|e| panic!("{input}: {e:?}"));
        assert_eq!(u.to_string(), canonical, "{input}");
        assert_eq!(SipUri::parse(&u.to_string()).unwrap(), u);
    }
    let u = SipUri::parse("sip:alice:pw@example.com:5070;transport=tcp;lr?subject=hi").unwrap();
    assert_eq!(u.scheme, UriScheme::Sip);
    assert_eq!(
        (u.user.as_deref(), u.password.as_deref()),
        (Some("alice"), Some("pw"))
    );
    assert_eq!((u.host.as_str(), u.port), ("example.com", Some(5070)));
    assert_eq!(
        u.param("TRANSPORT"),
        Some(Some("tcp")),
        "param names are case-insensitive"
    );
    assert_eq!(u.param("lr"), Some(None));
    assert!(u.is_loose_router());
    assert_eq!(u.param("nope"), None);
    assert_eq!(u.headers, vec![("subject".to_string(), "hi".to_string())]);
    let t = SipUri::parse("tel:+15550100;ext=7").unwrap();
    assert_eq!(
        (t.scheme, t.user.as_deref(), t.host.as_str()),
        (UriScheme::Tel, Some("+15550100"), "")
    );
    assert_eq!(UriScheme::Sips.as_str(), "sips");
    let v6 = SipUri::parse("sip:[::1]").unwrap();
    assert_eq!(v6.host, "[::1]");
    assert_eq!(SipUri::new("h").to_string(), "sip:h");
}

#[test]
fn u02_sip_uri_rejections() {
    for bad in [
        "",
        "sip:",
        "sip:@host",
        "sip:host:",
        "sip:host:0x50",
        "sip:host:65536",
        "sip:host:123456",
        "sip:[::1",
        "sip:[zzz]",
        "sip:[::1]x",
        "sip:ho st",
        "sip:host;;lr",
        "sip:host;=v",
        "sip:host;a=",
        "sip:host?",
        "sip:host?a",
        "sip:host?=v",
        "sip:ho<st",
        "http://example.com",
        "nocolon",
        "tel:",
        "tel:123;;x",
        "sip:bad_host!@",
        "sip:h\u{0}",
        "sip:ho$t",
    ] {
        assert!(SipUri::parse(bad).is_err(), "{bad:?} must be rejected");
    }
}

#[test]
fn u03_generic_uri_keeps_other_schemes_opaque() {
    let u = Uri::parse("mailto:a@b.example").unwrap();
    assert_eq!(
        u,
        Uri::Other {
            scheme: "mailto".into(),
            rest: "a@b.example".into()
        }
    );
    assert_eq!(u.to_string(), "mailto:a@b.example");
    assert!(u.as_sip().is_none());
    let s = Uri::parse("sip:a@b").unwrap();
    assert!(s.as_sip().is_some());
    assert_eq!(Uri::from(SipUri::new("x")).to_string(), "sip:x");
    for bad in [
        "", "9bad:x", "x", "http:", "http:a b", "http:<x>", ":x", "ht tp:x",
    ] {
        assert!(Uri::parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn u04_escape_and_unescape() {
    assert_eq!(unescape("a%20b%2Fc"), b"a b/c");
    assert_eq!(unescape("100%"), b"100%", "dangling % stays literal");
    assert_eq!(unescape("%4"), b"%4");
    assert_eq!(unescape("%zz%41"), b"%zzA");
    assert_eq!(unescape("%e4%B8%ad"), "中".as_bytes());
    assert_eq!(escape_user(b"a b@c"), "a%20b%40c");
    assert_eq!(escape_user("中".as_bytes()), "%E4%B8%AD");
    assert_eq!(escape_user(b"+1(555)-01.00;x=y"), "+1(555)-01.00;x=y");
    let raw = b"user \"quoted\" \x00\xff".to_vec();
    assert_eq!(
        unescape(&escape_user(&raw)),
        raw,
        "escape/unescape round trip"
    );
}

#[test]
fn u05_name_addr_forms() {
    let n =
        NameAddr::parse("\"Bob \\\"B\\\" Smith\" <sip:bob@example.com;lr>;tag=abc;q=0.7").unwrap();
    assert_eq!(n.display_name.as_deref(), Some("Bob \"B\" Smith"));
    assert_eq!(n.tag(), Some("abc"));
    assert_eq!(n.param("q"), Some(Some("0.7")));
    assert!(
        n.uri.as_sip().unwrap().is_loose_router(),
        "lr inside <> is a URI param"
    );
    assert_eq!(
        n.to_string(),
        "\"Bob \\\"B\\\" Smith\" <sip:bob@example.com;lr>;tag=abc;q=0.7"
    );

    let n = NameAddr::parse("Bob Smith <sip:bob@example.com>").unwrap();
    assert_eq!(n.display_name.as_deref(), Some("Bob Smith"));
    let n = NameAddr::parse("sip:bob@example.com;lr;tag=1").unwrap();
    assert!(
        !n.uri.as_sip().unwrap().is_loose_router(),
        "addr-spec: ;lr is a header param"
    );
    assert_eq!(n.param("lr"), Some(None));
    assert_eq!(n.tag(), Some("1"));
    assert_eq!(n.to_string(), "<sip:bob@example.com>;lr;tag=1");
    assert_eq!(
        NameAddr::parse("\"\" <sip:x@y>")
            .unwrap()
            .display_name
            .as_deref(),
        Some("")
    );

    let mut n = NameAddr::new(SipUri::new("example.com"));
    assert_eq!(n.tag(), None);
    n.set_tag("t1");
    n.set_tag("t2");
    assert_eq!(n.params.len(), 1, "set_tag replaces");
    assert_eq!(n.tag(), Some("t2"));
    assert_eq!(n.to_string(), "<sip:example.com>;tag=t2");

    for bad in [
        "",
        "   ",
        "\"unterminated <sip:a@b>",
        "\"x\" sip:a@b",
        "<sip:a@b",
        "Bell, Alex <sip:a@b>",
        "sip:a@b?h=v",
        "<sip:a@b>garbage",
        "<sip:a@b>;;x",
        "<sip:a@b>;x=",
        "<>",
        "< sip:a@b >",
        "<sip:a@b>;t=\"unterminated",
    ] {
        assert!(NameAddr::parse(bad).is_err(), "{bad:?} must be rejected");
    }
}

#[test]
fn u06_via_parse_and_display() {
    let v = Via::parse("SIP / 2.0 / udp  [2001:db8::5]:5070 ; branch = z9hG4bKab ;rport; received=10.0.0.1;ttl=16;maddr=224.0.1.1").unwrap();
    assert_eq!(v.transport, "udp");
    assert_eq!((v.host.as_str(), v.port), ("[2001:db8::5]", Some(5070)));
    assert_eq!(v.branch(), Some("z9hG4bKab"));
    assert_eq!(v.rport(), Some(None));
    assert_eq!(v.received(), Some("10.0.0.1"));
    assert_eq!(v.ttl(), Some(16));
    assert_eq!(v.maddr(), Some("224.0.1.1"));
    assert_eq!(
        v.to_string(),
        "SIP/2.0/udp [2001:db8::5]:5070;branch=z9hG4bKab;rport;received=10.0.0.1;ttl=16;maddr=224.0.1.1"
    );
    assert_eq!(Via::parse(&v.to_string()).unwrap(), v);

    let mut v = Via::new("UDP", "h.example.com", None);
    assert_eq!(v.rport(), None);
    v.set_param("rport", Some("5060".into()));
    v.set_param("rport", Some("5061".into()));
    v.set_param("branch", Some("z9hG4bK1".into()));
    assert_eq!(v.rport(), Some(Some(5061)));
    assert_eq!(
        v.to_string(),
        "SIP/2.0/UDP h.example.com;rport=5061;branch=z9hG4bK1"
    );
    v.set_param("rport", Some("x".into()));
    assert_eq!(
        v.rport(),
        Some(None),
        "unparsable rport value reads as 'present, unfilled'"
    );

    for bad in [
        "",
        "UDP host",
        "SIP/2.0 host",
        "SIP/2.0/UDP",
        "SIP/2.0/UDP ",
        "SIP/2.0/UD(P host",
        "XYZ/2.0/UDP host",
        "SIP/2.0/UDP ho st;a",
        "SIP/2.0/UDP host:99999",
        "SIP/2.0/UDP host:",
        "SIP/2.0/UDP [::1",
        "SIP/2.0/UDP [bad]",
        "SIP/2.0/UDP [::1]x",
        "SIP/2.0/UDP host;;",
        "SIP/2.0/UDP host;a=",
        "SIP/2.0/UDP ho!st",
    ] {
        assert!(Via::parse(bad).is_err(), "{bad:?}");
    }
    assert_eq!(
        Via::parse("SIP/3.0/UDP host"),
        Err(ParseError::UnsupportedVersion)
    );
}

#[test]
fn u07_cseq() {
    let c = CSeq::parse(" 0009   INVITE ").unwrap();
    assert_eq!((c.seq, c.method.clone()), (9, Method::Invite));
    assert_eq!(c.to_string(), "9 INVITE");
    assert_eq!(CSeq::parse("4294967295 BYE").unwrap().seq, u32::MAX);
    for bad in [
        "4294967296 BYE",
        "-1 BYE",
        "+1 BYE",
        "1",
        "",
        "1 B(Y",
        "1 BYE X",
        "x BYE",
    ] {
        assert!(CSeq::parse(bad).is_err(), "{bad:?}");
    }
}

#[test]
fn u08_method_and_status_code() {
    let all = [
        "INVITE",
        "ACK",
        "BYE",
        "CANCEL",
        "OPTIONS",
        "REGISTER",
        "INFO",
        "UPDATE",
        "PRACK",
        "SUBSCRIBE",
        "NOTIFY",
        "REFER",
        "MESSAGE",
        "PUBLISH",
    ];
    for m in all {
        let parsed = Method::from_token(m);
        assert!(!matches!(parsed, Method::Other(_)), "{m}");
        assert_eq!(parsed.as_str(), m);
        assert_eq!(parsed.to_string(), m);
    }
    assert_eq!(
        Method::from_token("invite"),
        Method::Other("invite".into()),
        "case sensitive"
    );
    assert_eq!(Method::from_token("FOO").as_str(), "FOO");

    assert!(StatusCode::new(99).is_none() && StatusCode::new(700).is_none());
    assert_eq!(StatusCode::new(486), Some(StatusCode::BUSY_HERE));
    assert_eq!(StatusCode::BUSY_HERE.code(), 486);
    assert_eq!(StatusCode::BUSY_HERE.to_string(), "486");
    assert!(StatusCode::TRYING.is_provisional() && !StatusCode::TRYING.is_final());
    assert!(StatusCode::OK.is_success() && StatusCode::OK.is_final());
    assert!(!StatusCode::BUSY_HERE.is_success() && StatusCode::BUSY_HERE.is_final());
    assert_eq!(
        StatusCode::REQUEST_PENDING.reason_phrase(),
        "Request Pending"
    );
    assert_eq!(StatusCode::new(199).unwrap().reason_phrase(), "Provisional");
    assert_eq!(StatusCode::new(299).unwrap().reason_phrase(), "Success");
    assert_eq!(StatusCode::new(399).unwrap().reason_phrase(), "Redirection");
    assert_eq!(
        StatusCode::new(499).unwrap().reason_phrase(),
        "Client Error"
    );
    assert_eq!(
        StatusCode::new(599).unwrap().reason_phrase(),
        "Server Error"
    );
    assert_eq!(
        StatusCode::new(699).unwrap().reason_phrase(),
        "Global Failure"
    );
    for (c, t) in [
        (StatusCode::RINGING, "Ringing"),
        (StatusCode::SESSION_PROGRESS, "Session Progress"),
        (StatusCode::BAD_REQUEST, "Bad Request"),
        (StatusCode::UNAUTHORIZED, "Unauthorized"),
        (StatusCode::NOT_FOUND, "Not Found"),
        (StatusCode::METHOD_NOT_ALLOWED, "Method Not Allowed"),
        (StatusCode::REQUEST_TIMEOUT, "Request Timeout"),
        (StatusCode::UNSUPPORTED_MEDIA_TYPE, "Unsupported Media Type"),
        (StatusCode::BAD_EXTENSION, "Bad Extension"),
        (
            StatusCode::SESSION_INTERVAL_TOO_SMALL,
            "Session Interval Too Small",
        ),
        (
            StatusCode::CALL_DOES_NOT_EXIST,
            "Call/Transaction Does Not Exist",
        ),
        (StatusCode::LOOP_DETECTED, "Loop Detected"),
        (StatusCode::TOO_MANY_HOPS, "Too Many Hops"),
        (StatusCode::REQUEST_TERMINATED, "Request Terminated"),
        (StatusCode::NOT_ACCEPTABLE_HERE, "Not Acceptable Here"),
        (StatusCode::SERVER_INTERNAL_ERROR, "Server Internal Error"),
        (StatusCode::NOT_IMPLEMENTED, "Not Implemented"),
        (StatusCode::SERVICE_UNAVAILABLE, "Service Unavailable"),
        (StatusCode::VERSION_NOT_SUPPORTED, "Version Not Supported"),
        (StatusCode::DECLINE, "Decline"),
    ] {
        assert_eq!(c.reason_phrase(), t);
    }
}

#[test]
fn u09_headers_collection_operations() {
    let mut h = Headers::new();
    assert!(h.is_empty());
    h.push("Via", "SIP/2.0/UDP a");
    h.push("v", "SIP/2.0/UDP b");
    h.push("X-Custom", "1");
    h.push("X-Custom", "2");
    assert_eq!(h.len(), 4);
    assert!(h.contains("VIA") && h.contains("v") && !h.contains("Route"));
    assert_eq!(
        h.get_all("via").collect::<Vec<_>>(),
        ["SIP/2.0/UDP a", "SIP/2.0/UDP b"]
    );
    h.push_front("Via", "SIP/2.0/UDP top");
    assert_eq!(h.top_via().unwrap().host, "top");
    assert_eq!(h.vias().unwrap().len(), 3);

    h.set("x-custom", "only");
    assert_eq!(
        h.get_all("X-Custom").collect::<Vec<_>>(),
        ["only"],
        "set keeps the first, drops the rest"
    );
    h.set("New-Header", "n");
    assert_eq!(h.iter().last().unwrap().name, "New-Header");
    h.set_content_length(42);
    assert_eq!(h.get("l"), Some("42"));
    h.set_content_length(7);
    assert_eq!(h.get_all("Content-Length").collect::<Vec<_>>(), ["7"]);

    assert_eq!(h.remove("via"), 3);
    assert_eq!(h.remove("via"), 0);
    assert!(h.top_via().is_err(), "no Via left");
    assert_eq!(h.top_via(), Err(ParseError::MissingHeader("Via")));
    assert_eq!(h.from(), Err(ParseError::MissingHeader("From")));
    assert_eq!(h.to(), Err(ParseError::MissingHeader("To")));
    assert_eq!(h.cseq(), Err(ParseError::MissingHeader("CSeq")));
    assert_eq!(
        h.max_forwards(),
        Ok(None),
        "absent Max-Forwards is tolerated"
    );
    assert_eq!(h.call_id(), None);

    let hd = Header::new("i", "abc");
    assert_eq!(
        (hd.name.as_str(), hd.compact, hd.wire_name().as_str()),
        ("Call-ID", true, "i")
    );
    let hd = Header::new("Call-ID", "abc");
    assert_eq!(hd.wire_name(), "Call-ID");
    let hd = Header {
        name: "Max-Forwards".into(),
        compact: true,
        value: "1".into(),
    };
    assert_eq!(
        hd.wire_name(),
        "Max-Forwards",
        "compact flag without a compact form is ignored"
    );
}

#[test]
fn u10_serialisation_details() {
    let mut r = Request::new(Method::Options, Uri::parse("sip:a@b").unwrap());
    r.headers.push("Via", "SIP/2.0/UDP h;branch=z9hG4bK1");
    r.headers.push("l", "999"); // wrong value, compact spelling
    r.headers.push("Content-Length", "5"); // duplicate
    r.headers.push("X-Evil", "a\r\nInjected: yes");
    r.body = bytes::Bytes::from_static(b"hello");
    let out = r.to_bytes();
    let text = String::from_utf8(out.to_vec()).unwrap();
    assert_eq!(
        text,
        "OPTIONS sip:a@b SIP/2.0\r\nVia: SIP/2.0/UDP h;branch=z9hG4bK1\r\nl: 5\r\nX-Evil: a  Injected: yes\r\n\r\nhello",
        "Content-Length recomputed in place (compact spelling kept), duplicate dropped, CR/LF neutralised"
    );
    // appended when absent
    let mut resp = Response::new(StatusCode::RINGING);
    assert_eq!(resp.reason, "Ringing");
    assert_eq!(
        &resp.to_bytes()[..],
        b"SIP/2.0 180 Ringing\r\nContent-Length: 0\r\n\r\n"
    );
    resp.reason = "Custom\r\nreason".into();
    assert_eq!(
        &resp.to_bytes()[..],
        b"SIP/2.0 180 Custom  reason\r\nContent-Length: 0\r\n\r\n"
    );
    let m = SipMessage::Response(resp);
    assert!(m.headers().is_empty());
    assert!(m.body().is_empty());
    let m = SipMessage::Request(r);
    assert_eq!(m.headers().len(), 4);
    assert_eq!(&m.body()[..], b"hello");
    assert_eq!(&m.to_bytes()[..], &m.to_bytes()[..]);
}
