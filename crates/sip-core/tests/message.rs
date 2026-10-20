//! P03 - P11: message parser and serializer behaviour.

use bytes::Bytes;
use proptest::prelude::*;
use sip_core::message::*;
use sip_core::transport::framing::take_frame;

/// A valid request skeleton; `extra` is inserted before the blank line.
fn req_bytes(extra: &str, body: &str) -> Vec<u8> {
    format!(
        "OPTIONS sip:bob@example.com SIP/2.0\r\n\
         Via: SIP/2.0/UDP pc.example.com;branch=z9hG4bK1\r\n\
         To: <sip:bob@example.com>\r\n\
         From: <sip:alice@example.com>;tag=1\r\n\
         Call-ID: c1@pc\r\n\
         CSeq: 1 OPTIONS\r\n\
         Max-Forwards: 70\r\n\
         {extra}\r\n{body}"
    )
    .into_bytes()
}

fn dg(b: &[u8]) -> Result<SipMessage, ParseError> {
    parse_datagram(b)
}

fn request(b: &[u8]) -> Request {
    match parse_datagram(b).unwrap_or_else(|e| panic!("{e:?}\n{}", String::from_utf8_lossy(b))) {
        SipMessage::Request(r) => r,
        other => panic!("not a request: {other:?}"),
    }
}

// ---------------------------------------------------------------- P03

#[test]
fn p03_folding_unfolded() {
    // fold after the colon, in the middle of a value, with TAB, several continuation lines
    let r = request(&req_bytes(
        "Subject:\r\n  a folded\r\n\tsubject   value\r\n   here\r\nX-After: x\r\nContent-Length: 0\r\n",
        "",
    ));
    assert_eq!(
        r.headers.get("Subject"),
        Some("a folded subject   value here"),
        "each CRLF+LWS run becomes one SP; interior spacing inside a physical line is untouched"
    );
    assert_eq!(
        r.headers.get("X-After"),
        Some("x"),
        "the header after the folded one is intact"
    );

    // fold directly after the header name colon and before it: `To :\r\n sip:...`
    let m = parse_datagram(
        b"INVITE sip:a@b SIP/2.0\r\nTo :\r\n <sip:x@y>\r\n ;tag=9\r\nFrom: <sip:f@g>;tag=1\r\nCall-ID: 1\r\nCSeq: 1 INVITE\r\nMax-Forwards: 1\r\nVia: SIP/2.0/UDP h\r\n\r\n",
    )
    .unwrap();
    assert_eq!(m.headers().get("To"), Some("<sip:x@y> ;tag=9"));
    assert_eq!(m.headers().to().unwrap().tag(), Some("9"));

    // folding inside a quoted string keeps one space, and a folded Via parameter works
    let r = request(&req_bytes(
        "Subject: \"quoted\r\n   part\"\r\nContact: <sip:a@b>;\r\n  q=0.5\r\n",
        "",
    ));
    assert_eq!(r.headers.get("Subject"), Some("\"quoted part\""));
    assert_eq!(
        r.headers.contacts().unwrap()[0].param("q"),
        Some(Some("0.5"))
    );

    // empty value + continuation
    let r = request(&req_bytes("X-Empty:\r\n \r\n", ""));
    assert_eq!(r.headers.get("X-Empty"), Some(""));
}

#[test]
fn p03_continuation_without_a_header_is_rejected() {
    let e = parse_datagram(b"OPTIONS sip:a@b SIP/2.0\r\n folded-first\r\nTo: <sip:a@b>\r\n\r\n")
        .unwrap_err();
    assert!(matches!(e, ParseError::InvalidHeader { .. }), "{e:?}");
}

#[test]
fn p03_whitespace_before_colon_and_header_without_colon() {
    let r = request(&req_bytes("X-Sp  \t :   spaced   \r\n", ""));
    assert_eq!(r.headers.get("X-Sp"), Some("spaced"));
    for bad in [
        "no colon here\r\n",
        ": empty name\r\n",
        "Bad Name: v\r\n",
        "B(ad: v\r\n",
    ] {
        assert!(
            matches!(
                dg(&req_bytes(bad, "")),
                Err(ParseError::InvalidHeader { .. })
            ),
            "{bad:?}"
        );
    }
}

// ---------------------------------------------------------------- P04

const COMPACT: [(char, &str); 16] = [
    ('i', "Call-ID"),
    ('f', "From"),
    ('t', "To"),
    ('v', "Via"),
    ('m', "Contact"),
    ('l', "Content-Length"),
    ('c', "Content-Type"),
    ('k', "Supported"),
    ('s', "Subject"),
    ('e', "Content-Encoding"),
    ('o', "Event"),
    ('u', "Allow-Events"),
    ('r', "Refer-To"),
    ('b', "Referred-By"),
    ('x', "Session-Expires"),
    ('y', "Identity"),
];

#[test]
fn p04_all_sixteen_compact_forms_normalise_and_serialise_back_compact() {
    assert_eq!(COMPACT.len(), 16);
    assert_eq!(COMPACT_FORMS.len(), 16);
    for (letter, full) in COMPACT {
        for l in [letter, letter.to_ascii_uppercase()] {
            let value = match full {
                "Content-Length" => "4".to_string(),
                "Call-ID" => "cid@h".to_string(),
                "From" => "<sip:f@x>;tag=a".to_string(),
                "To" => "<sip:t@x>".to_string(),
                "Via" => "SIP/2.0/UDP h;branch=z9hG4bKx".to_string(),
                "Contact" => "<sip:m@x>".to_string(),
                "Supported" => "timer".to_string(),
                _ => "some-value".to_string(),
            };
            // Build a message in which `l` is the ONLY spelling of this header.
            let mut text = String::from("INVITE sip:a@b SIP/2.0\r\n");
            let mut put = |n: &str, v: &str| text.push_str(&format!("{n}: {v}\r\n"));
            for (needed, v) in [
                ("Via", "SIP/2.0/UDP h;branch=z9hG4bKx"),
                ("To", "<sip:t@x>"),
                ("From", "<sip:f@x>;tag=a"),
                ("Call-ID", "cid@h"),
                ("CSeq", "1 INVITE"),
                ("Max-Forwards", "70"),
            ] {
                if needed == full {
                    put(&l.to_string(), &value);
                } else {
                    put(needed, v);
                }
            }
            if !["Via", "To", "From", "Call-ID"].contains(&full) {
                put(&l.to_string(), &value);
            }
            if full != "Content-Length" {
                put("Content-Length", "4");
            }
            text.push_str("\r\nbody");
            let m = parse_datagram(text.as_bytes()).unwrap_or_else(|e| panic!("{l}/{full}: {e:?}"));
            let h = m.headers();
            let found = h
                .iter()
                .find(|x| x.name == full)
                .unwrap_or_else(|| panic!("{l}: no {full}"));
            assert!(found.compact, "{l} -> {full}: compact flag");
            assert_eq!(found.value, value);
            assert_eq!(h.get(full), Some(value.as_str()), "lookup by full name");
            assert_eq!(
                h.get(&l.to_string()),
                Some(value.as_str()),
                "lookup by compact name"
            );
            assert_eq!(
                h.get(&full.to_ascii_uppercase()),
                Some(value.as_str()),
                "lookup is case-insensitive"
            );
            // serialisation keeps the compact spelling, in lower case
            let out = String::from_utf8(m.to_bytes().to_vec()).unwrap();
            let want = format!(
                "\r\n{}: {}\r\n",
                letter,
                if full == "Content-Length" {
                    "4"
                } else {
                    &value
                }
            );
            assert!(out.contains(&want), "{l}/{full}: {out}");
        }
    }
}

#[test]
fn p04_long_names_stay_long_and_unknown_names_keep_their_spelling() {
    let r = request(&req_bytes(
        "Call-Info: <http://x>\r\nx-MiXeD-Case: v\r\nCONTENT-type: text/plain\r\n",
        "",
    ));
    let out = String::from_utf8(r.to_bytes().to_vec()).unwrap();
    assert!(
        out.contains("\r\nx-MiXeD-Case: v\r\n"),
        "unknown header spelling preserved: {out}"
    );
    assert!(
        out.contains("\r\nContent-Type: text/plain\r\n"),
        "known header canonicalised: {out}"
    );
    assert!(out.contains("\r\nCall-ID: c1@pc\r\n"));
    assert_eq!(canonical_name("m"), ("Contact".to_string(), true));
    assert_eq!(
        canonical_name("MAX-forwards"),
        ("Max-Forwards".to_string(), false)
    );
    assert_eq!(
        canonical_name("Z"),
        ("Z".to_string(), false),
        "'z' is not a compact form"
    );
    assert_eq!(compact_letter("Call-ID"), Some('i'));
    assert_eq!(compact_letter("Max-Forwards"), None);
}

// ---------------------------------------------------------------- P05

#[test]
fn p05_multi_value_split() {
    // 1. Via order is preserved across lines and commas, including odd spacing
    let r = request(&req_bytes(
        "Via: SIP/2.0/UDP a.example.com;branch=z9hG4bKa, SIP/2.0/TCP b.example.com;branch=z9hG4bKb\r\nVia: SIP/2.0/TLS c.example.com;branch=z9hG4bKc ,SIP/2.0/UDP d.example.com\r\n",
        "",
    ));
    let hosts: Vec<_> = r
        .headers
        .vias()
        .unwrap()
        .into_iter()
        .map(|v| v.host)
        .collect();
    assert_eq!(
        hosts,
        [
            "pc.example.com",
            "a.example.com",
            "b.example.com",
            "c.example.com",
            "d.example.com"
        ]
    );
    assert_eq!(r.headers.get_all("Via").count(), 5, "one entry per element");

    // 2. commas inside a quoted display name do not split
    let r = request(&req_bytes(
        "Contact: \"Doe, John\" <sip:john@example.com>, <sip:jane@example.com>\r\n",
        "",
    ));
    let c = r.headers.contacts().unwrap();
    assert_eq!(c.len(), 2);
    assert_eq!(c[0].display_name.as_deref(), Some("Doe, John"));

    // 3. commas inside <...> do not split (URI parameter and URI header values)
    let r = request(&req_bytes(
        "Route: <sip:p1.example.com;lr;x=a,b>, <sip:p2.example.com;lr>\r\nRecord-Route: <sip:r1;a=1,2,3>,<sip:r2>\r\n",
        "",
    ));
    let routes: Vec<_> = r.headers.get_all("Route").collect();
    assert_eq!(
        routes,
        ["<sip:p1.example.com;lr;x=a,b>", "<sip:p2.example.com;lr>"]
    );
    assert_eq!(
        r.headers.get_all("Record-Route").collect::<Vec<_>>(),
        ["<sip:r1;a=1,2,3>", "<sip:r2>"]
    );

    // 4. escaped quote inside quoted string followed by a comma
    let r = request(&req_bytes(
        "Contact: \"a \\\" , b\" <sip:x@y>, <sip:z@y>\r\n",
        "",
    ));
    let c = r.headers.contacts().unwrap();
    assert_eq!(c.len(), 2);
    assert_eq!(c[0].display_name.as_deref(), Some("a \" , b"));

    // 5. list headers that are not validated: split, empty elements dropped
    let r = request(&req_bytes(
        "Allow: INVITE, ACK,, BYE ,\r\nSupported: timer,100rel\r\n",
        "",
    ));
    assert_eq!(
        r.headers.get_all("Allow").collect::<Vec<_>>(),
        ["INVITE", "ACK", "BYE"]
    );
    assert_eq!(
        r.headers.get_all("k").collect::<Vec<_>>(),
        ["timer", "100rel"]
    );

    // 6. headers that may legitimately contain commas are never split
    let r = request(&req_bytes(
        "Subject: hello, world, again\r\nDate: Sat, 15 Oct 2005 04:44:56 GMT\r\nWWW-Authenticate: Digest realm=\"x\", nonce=\"y\"\r\nX-Unknown: a, b, c\r\n",
        "",
    ));
    assert_eq!(r.headers.get("Subject"), Some("hello, world, again"));
    assert_eq!(r.headers.get("Date"), Some("Sat, 15 Oct 2005 04:44:56 GMT"));
    assert_eq!(
        r.headers.get("WWW-Authenticate"),
        Some("Digest realm=\"x\", nonce=\"y\"")
    );
    assert_eq!(r.headers.get("X-Unknown"), Some("a, b, c"));

    // 7. an empty element in a strictly validated list is an error
    for bad in [
        "Via: SIP/2.0/UDP a,,SIP/2.0/UDP b\r\n",
        "Route: <sip:a>,\r\n",
        "Contact: ,<sip:a>\r\n",
    ] {
        assert!(
            matches!(
                dg(&req_bytes(bad, "")),
                Err(ParseError::InvalidHeader { .. })
            ),
            "{bad:?}"
        );
    }
    // ... while an entirely empty Contact/Route value is tolerated
    let r = request(&req_bytes("Contact:\r\nRoute:\r\n", ""));
    assert_eq!(r.headers.get("Contact"), Some(""));
    assert!(
        r.headers.contacts().is_err(),
        "but the typed accessor reports it"
    );

    // 8. Contact: * wildcard
    let r = request(&req_bytes("Contact: *\r\nExpires: 0\r\n", ""));
    assert!(r.headers.contact_is_wildcard());
    assert!(r.headers.contacts().unwrap().is_empty());

    // 9. split entries serialise as one line each and re-parse identically
    let r = request(&req_bytes(
        "Via: SIP/2.0/UDP a;branch=z9hG4bKa, SIP/2.0/UDP b\r\n",
        "",
    ));
    let again = request(&r.to_bytes());
    assert_eq!(again.headers.vias().unwrap(), r.headers.vias().unwrap());
}

// ---------------------------------------------------------------- P06

#[test]
fn p06_stream_requires_content_length() {
    assert_eq!(
        parse_message(&req_bytes("", "")),
        Err(ParseError::MissingContentLength)
    );
    // the same message with Content-Length: 0 parses
    let (m, used) = parse_message(&req_bytes("Content-Length: 0\r\n", "")).unwrap();
    assert_eq!(used, req_bytes("Content-Length: 0\r\n", "").len());
    assert!(m.body().is_empty());
}

#[test]
fn p06_two_messages_in_one_buffer_report_exact_consumed_length() {
    let a = req_bytes("Content-Length: 5\r\n", "hello");
    let b = req_bytes("l: 3\r\n", "abc");
    let mut buf = a.clone();
    buf.extend_from_slice(&b);
    let (m1, n1) = parse_message(&buf).unwrap();
    assert_eq!(n1, a.len());
    assert_eq!(&m1.body()[..], b"hello");
    let (m2, n2) = parse_message(&buf[n1..]).unwrap();
    assert_eq!(n2, b.len());
    assert_eq!(&m2.body()[..], b"abc");
    assert_eq!(n1 + n2, buf.len());

    // leading keep-alive CRLFs are consumed together with the message
    let mut ka = b"\r\n\r\n".to_vec();
    ka.extend_from_slice(&a);
    ka.extend_from_slice(b"\r\n");
    let (_, n) = parse_message(&ka).unwrap();
    assert_eq!(
        n,
        4 + a.len(),
        "trailing CRLF is NOT consumed, leading ones are"
    );
}

#[test]
fn p06_every_proper_prefix_is_incomplete() {
    let msg = req_bytes("Content-Length: 11\r\n", "hello world");
    for cut in 0..msg.len() {
        assert_eq!(
            parse_message(&msg[..cut]),
            Err(ParseError::Incomplete),
            "prefix of {cut} bytes must be Incomplete (header cut or body cut)"
        );
    }
    let (m, n) = parse_message(&msg).unwrap();
    assert_eq!((n, &m.body()[..]), (msg.len(), &b"hello world"[..]));
    // body only half there
    assert_eq!(
        parse_message(&msg[..msg.len() - 5]),
        Err(ParseError::Incomplete)
    );
}

#[test]
fn p06_stream_rejects_bad_content_length_values() {
    for v in [
        "-1",
        "five",
        "",
        "1.5",
        "+3",
        "0x10",
        "99999999999999999999999",
    ] {
        assert_eq!(
            parse_message(&req_bytes(&format!("Content-Length: {v}\r\n"), "")),
            Err(ParseError::InvalidContentLength),
            "value {v:?}"
        );
    }
    // agreeing duplicates are fine, conflicting ones are not
    assert!(parse_message(&req_bytes("Content-Length: 0\r\nl: 0\r\n", "")).is_ok());
    assert_eq!(
        parse_message(&req_bytes("Content-Length: 0\r\nl: 1\r\n", "x")),
        Err(ParseError::InvalidContentLength)
    );
}

// ---------------------------------------------------------------- P07

#[test]
fn p07_datagram_content_length_rules() {
    // missing -> rest of datagram
    let r = request(&req_bytes("", "the rest of the datagram"));
    assert_eq!(&r.body[..], b"the rest of the datagram");
    // equal
    let r = request(&req_bytes("Content-Length: 5\r\n", "12345"));
    assert_eq!(&r.body[..], b"12345");
    // smaller -> truncated (RFC 3261 18.3)
    let r = request(&req_bytes("Content-Length: 3\r\n", "12345"));
    assert_eq!(&r.body[..], b"123");
    // zero with trailing junk
    let r = request(&req_bytes("Content-Length: 0\r\n", "junk"));
    assert!(r.body.is_empty());
    // larger -> error carrying both numbers
    assert_eq!(
        dg(&req_bytes("Content-Length: 10\r\n", "12345")),
        Err(ParseError::ContentLengthMismatch {
            declared: 10,
            available: 5
        })
    );
    assert_eq!(
        dg(&req_bytes("Content-Length: 1\r\n", "")),
        Err(ParseError::ContentLengthMismatch {
            declared: 1,
            available: 0
        })
    );
    // missing blank line at the end of a datagram is tolerated (headers only)
    let mut no_blank = req_bytes("Content-Length: 0\r\n", "");
    no_blank.truncate(no_blank.len() - 2);
    assert!(dg(&no_blank).is_ok());
    // binary body bytes are untouched
    let body: Vec<u8> = (0u8..=255).collect();
    let mut raw = req_bytes("Content-Length: 256\r\n", "");
    raw.extend_from_slice(&body);
    assert_eq!(&request(&raw).body[..], &body[..]);
    // empty input
    assert_eq!(dg(b""), Err(ParseError::Empty));
    assert_eq!(dg(b"\r\n\r\n"), Err(ParseError::Empty));
    assert_eq!(parse_message(b""), Err(ParseError::Incomplete));
}

// ---------------------------------------------------------------- P08

#[test]
fn p08_each_missing_mandatory_header_reports_its_name() {
    let full: Vec<(&str, &str)> = vec![
        ("To", "<sip:bob@example.com>"),
        ("From", "<sip:alice@example.com>;tag=1"),
        ("CSeq", "1 OPTIONS"),
        ("Call-ID", "c1@pc"),
        ("Max-Forwards", "70"),
        ("Via", "SIP/2.0/UDP pc.example.com;branch=z9hG4bK1"),
    ];
    for skip in ["To", "From", "CSeq", "Call-ID", "Via"] {
        let mut text = String::from("OPTIONS sip:bob@example.com SIP/2.0\r\n");
        for (n, v) in &full {
            if *n != skip {
                text.push_str(&format!("{n}: {v}\r\n"));
            }
        }
        text.push_str("Content-Length: 0\r\n\r\n");
        let want = ParseError::MissingHeader(match skip {
            "To" => "To",
            "From" => "From",
            "CSeq" => "CSeq",
            "Call-ID" => "Call-ID",
            _ => "Via",
        });
        assert_eq!(
            dg(text.as_bytes()),
            Err(want.clone()),
            "request without {skip}"
        );
        assert_eq!(
            parse_message(text.as_bytes()),
            Err(want),
            "request without {skip} (stream)"
        );
    }
    // M1a review fix: a request without Max-Forwards is tolerated (spec section 9); the
    // typed accessor reports None, and a malformed value is still rejected (below).
    {
        let mut text = String::from("OPTIONS sip:bob@example.com SIP/2.0\r\n");
        for (n, v) in &full {
            if *n != "Max-Forwards" {
                text.push_str(&format!("{n}: {v}\r\n"));
            }
        }
        text.push_str("Content-Length: 0\r\n\r\n");
        let SipMessage::Request(r) = dg(text.as_bytes()).expect("tolerated") else {
            panic!("request expected");
        };
        assert_eq!(r.headers.max_forwards(), Ok(None));
        assert_eq!(
            parse_message(text.as_bytes()).map(|(_, n)| n),
            Ok(text.len())
        );
    }
    // responses: Max-Forwards is NOT required, the other five are
    for skip in ["To", "From", "CSeq", "Call-ID", "Via", "Max-Forwards"] {
        let mut text = String::from("SIP/2.0 200 OK\r\n");
        for (n, v) in &full {
            if *n != skip {
                text.push_str(&format!("{n}: {v}\r\n"));
            }
        }
        text.push_str("Content-Length: 0\r\n\r\n");
        let got = dg(text.as_bytes());
        if skip == "Max-Forwards" {
            assert!(
                got.is_ok(),
                "response without Max-Forwards must parse: {got:?}"
            );
        } else {
            assert!(
                matches!(got, Err(ParseError::MissingHeader(n)) if n == skip),
                "response without {skip}: {got:?}"
            );
        }
    }
    // a compact spelling satisfies the requirement
    let r = req_bytes("", "");
    let s = String::from_utf8(r)
        .unwrap()
        .replace("\r\nTo: ", "\r\nt: ")
        .replace("\r\nVia: ", "\r\nv: ");
    assert!(dg(s.as_bytes()).is_ok());
}

#[test]
fn p08_malformed_mandatory_headers_are_invalid_not_missing() {
    for (hdr, bad, name) in [
        ("CSeq", "abc OPTIONS", "CSeq"),
        ("CSeq", "1", "CSeq"),
        ("CSeq", "1 OPTIONS extra", "CSeq"),
        ("CSeq", "-1 OPTIONS", "CSeq"),
        ("CSeq", "4294967296 OPTIONS", "CSeq"),
        ("Max-Forwards", "256", "Max-Forwards"),
        ("Max-Forwards", "-1", "Max-Forwards"),
        ("Max-Forwards", "many", "Max-Forwards"),
        ("Max-Forwards", "", "Max-Forwards"),
        ("Call-ID", "", "Call-ID"),
        ("To", "", "To"),
        ("From", "sip:a@b;tag", "From"), // valid, control (below)
    ] {
        let text = String::from_utf8(req_bytes("", "")).unwrap();
        let line = text
            .lines()
            .find(|l| l.starts_with(&format!("{hdr}:")))
            .unwrap()
            .to_string();
        let replaced = text.replace(&line, &format!("{hdr}: {bad}"));
        let got = dg(replaced.as_bytes());
        if hdr == "From" {
            assert!(got.is_ok(), "control case must parse: {got:?}");
        } else {
            assert!(
                matches!(&got, Err(ParseError::InvalidHeader { name: n, .. }) if *n == name),
                "{hdr}: {bad:?} -> {got:?}"
            );
        }
    }
    // duplicates of single-valued headers
    for h in [
        "To: <sip:x@y>",
        "From: <sip:x@y>",
        "CSeq: 2 OPTIONS",
        "Call-ID: other",
        "Max-Forwards: 3",
    ] {
        let name = h.split(':').next().unwrap();
        let got = dg(&req_bytes(&format!("{h}\r\n"), ""));
        assert!(
            matches!(got, Err(ParseError::MultipleHeader(n)) if n == name),
            "{h}: {got:?}"
        );
    }
}

#[test]
fn p08_start_line_errors() {
    for (line, pred) in [
        ("OPTIONS sip:a@b SIP/2.0 extra", "start"),
        ("OPTIONS sip:a@b", "start"),
        ("OPTIONS", "start"),
        ("OPTIONS sip:a@b HTTP/1.1", "start"),
        ("OPTIONS sip:a@b SIP/2", "start"),
        ("OPT(IONS sip:a@b SIP/2.0", "method"),
        ("OPTIONS sip:a@b SIP/3.0", "version"),
        ("OPTIONS sip:a@b SIP/2.1", "version"),
        ("OPTIONS sip:@b SIP/2.0", "uri"),
        ("OPTIONS sip:a@b:99999 SIP/2.0", "uri"),
        ("OPTIONS sip:[::1 SIP/2.0", "uri"),
        ("OPTIONS nocolon SIP/2.0", "uri"),
        ("SIP/2.0 99 Low", "status"),
        ("SIP/2.0 700 High", "status"),
        ("SIP/2.0 2000 Long", "status"),
        ("SIP/2.0 abc Text", "status"),
        ("SIP/2.0", "status"),
        ("SIP/1.0 200 OK", "version"),
    ] {
        let text = String::from_utf8(req_bytes("", "")).unwrap();
        let first = text.lines().next().unwrap().to_string();
        let replaced = text.replacen(&first, line, 1);
        let got = dg(replaced.as_bytes());
        let ok = match pred {
            "start" => matches!(got, Err(ParseError::InvalidStartLine(_))),
            "method" => matches!(got, Err(ParseError::InvalidMethod)),
            "version" => matches!(got, Err(ParseError::UnsupportedVersion)),
            "uri" => matches!(got, Err(ParseError::InvalidUri(_))),
            _ => matches!(got, Err(ParseError::InvalidStatusCode)),
        };
        assert!(ok, "{line:?} expected {pred}, got {got:?}");
    }
    // Method names are case sensitive
    let r = request(
        &String::from_utf8(req_bytes("", ""))
            .unwrap()
            .replacen("OPTIONS sip", "options sip", 1)
            .into_bytes(),
    );
    assert_eq!(r.method, Method::Other("options".into()));
}

// ---------------------------------------------------------------- P09

#[test]
fn p09_size_limit_64kib() {
    assert_eq!(MAX_MESSAGE_SIZE, 65536);
    // datagram of exactly 64 KiB is accepted, one byte more is not
    let head = req_bytes("", "").len() - 2 + "Content-Length: 00000\r\n\r\n".len();
    let body_len = MAX_MESSAGE_SIZE - head;
    let mut ok = req_bytes(&format!("Content-Length: {body_len:05}\r\n"), "");
    ok.extend(std::iter::repeat_n(b'x', body_len));
    assert_eq!(ok.len(), MAX_MESSAGE_SIZE);
    let r = request(&ok);
    assert_eq!(r.body.len(), body_len);
    let (_, used) = parse_message(&ok).unwrap();
    assert_eq!(used, MAX_MESSAGE_SIZE);
    ok.push(b'x');
    assert_eq!(dg(&ok), Err(ParseError::TooLarge));
    // stream: Content-Length claims more than the limit -> rejected before waiting for the data
    assert_eq!(
        parse_message(&req_bytes(
            &format!("Content-Length: {}\r\n", MAX_MESSAGE_SIZE),
            ""
        )),
        Err(ParseError::TooLarge)
    );
    // stream: endless header block without terminator
    let mut endless = b"OPTIONS sip:a@b SIP/2.0\r\n".to_vec();
    while endless.len() <= MAX_MESSAGE_SIZE {
        endless.extend_from_slice(b"X-Pad: 0123456789012345678901234567890123456789\r\n");
    }
    assert_eq!(parse_message(&endless), Err(ParseError::TooLarge));
    // ... but below the limit it is merely incomplete
    assert_eq!(parse_message(&endless[..1000]), Err(ParseError::Incomplete));
}

fn with_n_headers(n_total: usize) -> Vec<u8> {
    // 6 mandatory headers + Content-Length + padding headers = n_total entries
    let pad = n_total - 7;
    let mut extra = String::from("Content-Length: 0\r\n");
    for i in 0..pad {
        extra.push_str(&format!("X-H{i}: v\r\n"));
    }
    req_bytes(&extra, "")
}

#[test]
fn p09_header_count_limit_256() {
    assert_eq!(MAX_HEADERS, 256);
    let r = request(&with_n_headers(256));
    assert_eq!(r.headers.len(), 256, "exactly 256 headers are accepted");
    assert_eq!(dg(&with_n_headers(257)), Err(ParseError::TooManyHeaders));
    assert_eq!(
        parse_message(&with_n_headers(257)),
        Err(ParseError::TooManyHeaders)
    );
    assert_eq!(dg(&with_n_headers(1000)), Err(ParseError::TooManyHeaders));
    // list splitting cannot be used to amplify: 300 Via elements in ONE line
    let vias = (0..300)
        .map(|i| format!("SIP/2.0/UDP h{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    assert_eq!(
        dg(&req_bytes(&format!("Via: {vias}\r\n"), "")),
        Err(ParseError::TooManyHeaders)
    );
    // 250 elements fit (6 other headers + ... = within limit)
    let vias = (0..200)
        .map(|i| format!("SIP/2.0/UDP h{i}"))
        .collect::<Vec<_>>()
        .join(", ");
    assert_eq!(
        request(&req_bytes(&format!("Via: {vias}\r\n"), ""))
            .headers
            .vias()
            .unwrap()
            .len(),
        201
    );
}

// ---------------------------------------------------------------- P10

fn token() -> impl Strategy<Value = String> {
    "[a-z][a-z0-9]{0,7}"
}

fn arb_host() -> impl Strategy<Value = String> {
    prop_oneof![
        "[a-z][a-z0-9-]{0,8}(\\.[a-z]{2,5}){0,2}".prop_map(|s| s),
        (any::<u8>(), any::<u8>(), any::<u8>(), any::<u8>())
            .prop_map(|(a, b, c, d)| format!("{a}.{b}.{c}.{d}")),
        Just("[2001:db8::1]".to_string()),
    ]
}

fn arb_param() -> impl Strategy<Value = Param> {
    (token(), proptest::option::of("[a-z0-9]{1,6}")).prop_map(|(n, v)| Param { name: n, value: v })
}

fn arb_sip_uri() -> impl Strategy<Value = SipUri> {
    (
        prop_oneof![Just(UriScheme::Sip), Just(UriScheme::Sips)],
        proptest::option::of("[a-z0-9._%+-]{1,8}"),
        proptest::option::of("[a-z0-9]{1,6}"),
        arb_host(),
        proptest::option::of(1u16..),
        proptest::collection::vec(arb_param(), 0..3),
        proptest::collection::vec(("[a-z]{1,5}", "[a-z0-9%]{1,6}"), 0..2),
    )
        .prop_map(
            |(scheme, user, password, host, port, params, headers)| SipUri {
                scheme,
                password: if user.is_some() { password } else { None },
                user,
                host,
                port,
                params,
                headers,
            },
        )
}

fn arb_uri() -> impl Strategy<Value = Uri> {
    prop_oneof![
        8 => arb_sip_uri().prop_map(Uri::Sip),
        1 => Just(Uri::Other { scheme: "http".into(), rest: "//h.example.com/p?q=1".into() }),
    ]
}

fn arb_name_addr() -> impl Strategy<Value = NameAddr> {
    (
        proptest::option::of("[ -~é大]{0,15}"),
        arb_uri(),
        proptest::collection::vec(arb_param(), 0..3),
    )
        .prop_map(|(display_name, uri, params)| NameAddr {
            display_name,
            uri,
            params,
        })
}

fn arb_via() -> impl Strategy<Value = Via> {
    (
        prop_oneof![Just("UDP"), Just("TCP"), Just("TLS"), Just("SCTP")],
        arb_host(),
        proptest::option::of(1u16..),
        "[a-zA-Z0-9]{1,10}",
        proptest::option::of(proptest::option::of(1u16..)),
    )
        .prop_map(|(t, host, port, branch, rport)| {
            let mut v = Via::new(t, host, port);
            v.params.push(Param {
                name: "branch".into(),
                value: Some(format!("z9hG4bK{branch}")),
            });
            if let Some(rp) = rport {
                v.params.push(Param {
                    name: "rport".into(),
                    value: rp.map(|p| p.to_string()),
                });
            }
            v
        })
}

fn arb_method() -> impl Strategy<Value = Method> {
    "[A-Z]{3,10}".prop_map(|s| Method::from_token(&s))
}

fn arb_unknown_header() -> impl Strategy<Value = Header> {
    ("X-[A-Za-z]{1,6}", "[!-~]([ -~]{0,14}[!-~])?").prop_map(|(n, v)| Header {
        name: n,
        compact: false,
        value: v,
    })
}

/// Builds a header list: the mandatory ones for the given kind, plus optional extras, shuffled.
fn arb_headers(request: bool) -> impl Strategy<Value = Headers> {
    (
        proptest::collection::vec(arb_via(), 1..4),
        arb_name_addr(),
        arb_name_addr(),
        "[a-z0-9.@-]{1,20}",
        any::<u32>(),
        arb_method(),
        any::<u8>(),
        proptest::collection::vec(arb_name_addr(), 0..3),
        proptest::collection::vec(arb_name_addr(), 0..3),
        proptest::collection::vec(arb_unknown_header(), 0..4),
        proptest::collection::vec(any::<bool>(), 8),
    )
        .prop_flat_map(
            move |(
                vias,
                to,
                from,
                call_id,
                seq,
                method,
                mf,
                contacts,
                routes,
                unknown,
                compacts,
            )| {
                let mut hs: Vec<Header> = Vec::new();
                for v in vias {
                    hs.push(Header {
                        name: "Via".into(),
                        compact: compacts[0],
                        value: v.to_string(),
                    });
                }
                hs.push(Header {
                    name: "To".into(),
                    compact: compacts[1],
                    value: to.to_string(),
                });
                hs.push(Header {
                    name: "From".into(),
                    compact: compacts[2],
                    value: from.to_string(),
                });
                hs.push(Header {
                    name: "Call-ID".into(),
                    compact: compacts[3],
                    value: call_id,
                });
                hs.push(Header::new("CSeq", CSeq { seq, method }.to_string()));
                if request {
                    hs.push(Header::new("Max-Forwards", mf.to_string()));
                }
                for c in contacts {
                    hs.push(Header {
                        name: "Contact".into(),
                        compact: compacts[4],
                        value: c.to_string(),
                    });
                }
                for r in routes {
                    hs.push(Header::new("Route", r.to_string()));
                }
                hs.push(Header {
                    name: "Subject".into(),
                    compact: compacts[5],
                    value: "a, b; c".into(),
                });
                hs.extend(unknown);
                Just(hs).prop_shuffle().prop_map(|v| {
                    let mut h = Headers::new();
                    for x in v {
                        h.push_header(x);
                    }
                    h
                })
            },
        )
}

fn arb_request() -> impl Strategy<Value = Request> {
    (
        arb_method(),
        arb_uri(),
        arb_headers(true),
        proptest::collection::vec(any::<u8>(), 0..200),
    )
        .prop_map(|(method, uri, headers, body)| Request {
            method,
            uri,
            headers,
            body: Bytes::from(body),
        })
}

fn arb_response() -> impl Strategy<Value = Response> {
    (
        100u16..=699,
        "[ -~]{0,20}",
        arb_headers(false),
        proptest::collection::vec(any::<u8>(), 0..200),
    )
        .prop_map(|(code, reason, headers, body)| Response {
            status: StatusCode::new(code).unwrap(),
            reason,
            headers,
            body: Bytes::from(body),
        })
}

/// Serialization (re)computes Content-Length, so the expected parsed value carries it too.
fn with_cl(mut h: Headers, len: usize) -> Headers {
    h.set_content_length(len);
    h
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(1500))]

    #[test]
    fn p10_request_round_trip(r in arb_request()) {
        let wire = r.to_bytes();
        let mut expected = r.clone();
        expected.headers = with_cl(expected.headers, expected.body.len());
        let expected = SipMessage::Request(expected);
        prop_assert_eq!(&parse_datagram(&wire).unwrap(), &expected);
        let (stream, used) = parse_message(&wire).unwrap();
        prop_assert_eq!(&stream, &expected);
        prop_assert_eq!(used, wire.len());
        // serialization is a fixed point
        prop_assert_eq!(expected.to_bytes(), wire);
    }

    #[test]
    fn p10_response_round_trip(r in arb_response()) {
        let wire = r.to_bytes();
        let mut expected = r.clone();
        expected.headers = with_cl(expected.headers, expected.body.len());
        let expected = SipMessage::Response(expected);
        prop_assert_eq!(&parse_datagram(&wire).unwrap(), &expected);
        let (stream, used) = parse_message(&wire).unwrap();
        prop_assert_eq!(&stream, &expected);
        prop_assert_eq!(used, wire.len());
    }

    #[test]
    fn p10_arbitrary_bytes_never_panic(data in proptest::collection::vec(any::<u8>(), 0..1500)) {
        let _ = parse_message(&data);
        let _ = parse_datagram(&data);
        let mut buf = data.clone();
        let _ = take_frame(&mut buf);
    }

    #[test]
    fn p10_sip_flavoured_text_never_panics(s in "(INVITE|SIP/2.0|ACK)? ?[a-z:@<>;,=\"\\\\ \r\n0-9/.-]{0,300}") {
        let _ = parse_message(s.as_bytes());
        let _ = parse_datagram(s.as_bytes());
    }

    #[test]
    fn p10_mutated_valid_messages_never_panic_and_survivors_reserialise(
        which in 0usize..4,
        edits in proptest::collection::vec((any::<prop::sample::Index>(), any::<u8>()), 1..8),
        cut in proptest::option::of(any::<prop::sample::Index>()),
    ) {
        let mut bytes = [
            req_bytes("Content-Length: 4\r\nContact: <sip:a@b>\r\n", "abcd"),
            std::fs::read(format!("{}/tests/vectors/rfc4475/valid/wsinv.sip", env!("CARGO_MANIFEST_DIR"))).unwrap(),
            std::fs::read(format!("{}/tests/vectors/rfc4475/valid/intmeth.sip", env!("CARGO_MANIFEST_DIR"))).unwrap(),
            std::fs::read(format!("{}/tests/vectors/rfc4475/valid/transports.sip", env!("CARGO_MANIFEST_DIR"))).unwrap(),
        ][which].clone();
        for (i, b) in edits {
            let at = i.index(bytes.len());
            bytes[at] = b;
        }
        if let Some(c) = cut { bytes.truncate(c.index(bytes.len() + 1)); }
        for mode in [Framing::Stream, Framing::Datagram] {
            if let Ok((m, _)) = parse_with_framing(&bytes, mode) {
                // whatever parses can be serialised and parses again
                let again = parse_datagram(&m.to_bytes());
                prop_assert!(again.is_ok(), "re-parse failed: {:?}\n{}", again, String::from_utf8_lossy(&m.to_bytes()));
            }
        }
    }
}

// ---------------------------------------------------------------- P11 (integration with the extracted framer)

#[test]
fn p11_take_frame_output_feeds_parse_message() {
    // All valid RFC 4475 vectors that carry a Content-Length, glued into one TCP stream and fed
    // in uneven chunks: framing must cut exactly the original messages, and each frame must
    // parse (the extracted sipress framer and the new parser agree on message boundaries).
    let dir = format!("{}/tests/vectors/rfc4475/valid", env!("CARGO_MANIFEST_DIR"));
    let mut names: Vec<_> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    names.sort();
    let mut messages: Vec<Vec<u8>> = Vec::new();
    for p in names {
        let raw = std::fs::read(&p).unwrap();
        let stem = p.file_stem().unwrap().to_string_lossy().into_owned();
        if stem == "dblreq" {
            continue; // deliberately carries extra octets after the message
        }
        if let Ok((_, used)) = parse_message(&raw) {
            assert_eq!(used, raw.len(), "{stem}");
            messages.push(raw);
        }
    }
    assert!(
        messages.len() >= 20,
        "only {} vectors usable as stream messages",
        messages.len()
    );
    let stream: Vec<u8> = messages.iter().flat_map(|m| m.iter().copied()).collect();
    for chunk in [1usize, 7, 64, 1000] {
        let mut buf = Vec::new();
        let mut got = Vec::new();
        for piece in stream.chunks(chunk) {
            buf.extend_from_slice(piece);
            while let Some(frame) = take_frame(&mut buf).expect("framing") {
                got.push(frame);
            }
        }
        assert!(
            buf.is_empty(),
            "chunk {chunk}: leftover {} bytes",
            buf.len()
        );
        assert_eq!(got, messages, "chunk size {chunk}");
        for f in &got {
            assert_eq!(parse_message(f).unwrap().1, f.len());
        }
    }
}
