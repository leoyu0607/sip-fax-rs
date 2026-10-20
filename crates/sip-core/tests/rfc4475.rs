//! P01 / P02: RFC 4475 torture-test vectors (bit-exact copies from the RFC's appendix A).
//!
//! `tests/vectors/rfc4475/{valid,invalid}/*.sip` hold the raw bytes. The directory name follows
//! the RFC's own classification; the *outcome* the parser must produce for each file is asserted
//! below, with the RFC's reasoning, because several RFC "invalid" messages are only semantically
//! wrong and are expected to parse (the layer above must answer 400/501/...).

use sip_core::message::*;

fn load(dir: &str, name: &str) -> Vec<u8> {
    let p = format!(
        "{}/tests/vectors/rfc4475/{dir}/{name}.sip",
        env!("CARGO_MANIFEST_DIR")
    );
    std::fs::read(&p).unwrap_or_else(|e| panic!("read {p}: {e}"))
}

fn req(m: SipMessage) -> Request {
    match m {
        SipMessage::Request(r) => r,
        SipMessage::Response(r) => panic!("expected request, got response {}", r.status),
    }
}

fn resp(m: SipMessage) -> Response {
    match m {
        SipMessage::Response(r) => r,
        SipMessage::Request(r) => panic!("expected response, got request {}", r.method),
    }
}

fn sip_uri(u: &Uri) -> &SipUri {
    u.as_sip().expect("a sip/sips/tel URI")
}

fn valid(name: &str) -> SipMessage {
    parse_datagram(&load("valid", name)).unwrap_or_else(|e| panic!("{name}: {e:?}"))
}

const VALID: &[&str] = &[
    "wsinv",
    "intmeth",
    "esc01",
    "escnull",
    "esc02",
    "lwsdisp",
    "longreq",
    "dblreq",
    "semiuri",
    "transports",
    "mpart01",
    "unreason",
    "noreason", // RFC 3.1.1.x
    "unkscm",
    "novelsc",
    "unksm2",
    "bext01",
    "invut",
    "regaut01",
    "bcast",
    "zeromf",
    "cparam01",
    "cparam02",
    "regescrt",
    "sdp01", // RFC 3.3.x: well-formed, rejected (if at all) by upper layers
];

#[test]
fn p01_every_valid_vector_is_in_the_table_and_parses_in_both_modes() {
    let mut on_disk: Vec<String> = std::fs::read_dir(format!(
        "{}/tests/vectors/rfc4475/valid",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
    .map(|e| {
        e.unwrap()
            .path()
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    })
    .collect();
    on_disk.sort();
    let mut table: Vec<String> = VALID.iter().map(|s| s.to_string()).collect();
    table.sort();
    assert_eq!(on_disk, table, "vector directory and VALID table differ");

    for name in VALID {
        let bytes = load("valid", name);
        let dgram = parse_datagram(&bytes).unwrap_or_else(|e| panic!("{name} datagram: {e:?}"));
        let (stream, used) =
            parse_message(&bytes).unwrap_or_else(|e| panic!("{name} stream: {e:?}"));
        assert_eq!(dgram, stream, "{name}: datagram and stream parse differ");
        if *name == "dblreq" {
            assert!(
                used < bytes.len(),
                "dblreq must leave the second request unconsumed"
            );
        } else {
            assert_eq!(
                used,
                bytes.len(),
                "{name}: stream mode must consume the whole message"
            );
        }
    }
}

#[test]
fn p01_wsinv_whitespace_folding_and_compact_forms() {
    // RFC 3.1.1.1: LWS everywhere, compact forms, mixed case names, folded values.
    let r = req(valid("wsinv"));
    assert_eq!(r.method, Method::Invite);
    let u = sip_uri(&r.uri);
    assert_eq!(u.user.as_deref(), Some("vivekg"));
    assert_eq!(u.host, "chair-dnrc.example.com");
    assert_eq!(u.params, vec![Param::new("unknownparam", None::<String>)]);

    assert_eq!(r.headers.call_id(), Some("wsinv.ndaksdj@192.0.2.1"));
    assert_eq!(r.headers.max_forwards().unwrap(), 68, "leading zeros: 0068");
    let cseq = r.headers.cseq().unwrap();
    assert_eq!(
        (cseq.seq, cseq.method),
        (9, Method::Invite),
        "cseq: 0009 folded INVITE"
    );

    let to = r.headers.to().unwrap();
    assert_eq!(to.tag(), Some("1918181833n"), "tag with spaces around '='");
    assert_eq!(sip_uri(&to.uri).host, "chair-dnrc.example.com");
    let from = r.headers.from().unwrap();
    assert_eq!(from.display_name.as_deref(), Some("J Rosenberg \\\""));
    assert_eq!(from.tag(), Some("98asjd8"));

    let vias = r.headers.vias().unwrap();
    assert_eq!(
        vias.len(),
        3,
        "1 Via line + 1 compact line holding 2 elements"
    );
    assert_eq!(
        (
            vias[0].transport.as_str(),
            vias[0].host.as_str(),
            vias[0].branch()
        ),
        ("UDP", "192.0.2.2", Some("390skdjuw"))
    );
    assert_eq!(
        (
            vias[1].transport.as_str(),
            vias[1].host.as_str(),
            vias[1].branch()
        ),
        ("TCP", "spindle.example.com", Some("z9hG4bK9ikj8"))
    );
    assert_eq!(
        (
            vias[2].transport.as_str(),
            vias[2].host.as_str(),
            vias[2].branch()
        ),
        ("UDP", "192.168.255.111", Some("z9hG4bK30239")),
        "branch value folded onto the next line"
    );

    let contacts = r.headers.contacts().unwrap();
    assert_eq!(contacts.len(), 1);
    assert_eq!(
        contacts[0].display_name.as_deref(),
        Some("Quoted string \"\"")
    );
    assert_eq!(contacts[0].param("newparam"), Some(Some("newvalue")));
    assert_eq!(contacts[0].param("secondparam"), Some(None));
    assert_eq!(contacts[0].param("q"), Some(Some("0.33")));

    let routes = r.headers.routes().unwrap();
    assert_eq!(routes.len(), 1);
    let ru = sip_uri(&routes[0].uri);
    assert!(ru.is_loose_router());
    assert_eq!(ru.param("unknownwith"), Some(Some("value")));
    assert_eq!(ru.param("unknown-no-value"), Some(None));

    assert_eq!(
        r.headers.get("NewFangledHeader"),
        Some("newfangled value continued newfangled value"),
        "folding replaced by a single space"
    );
    assert_eq!(
        r.headers.get("UnknownHeaderWithUnusualValue"),
        Some(";;,,;;,;")
    );
    assert_eq!(
        r.headers.get("Subject"),
        Some(""),
        "s :  (compact, empty value)"
    );
    assert_eq!(r.headers.get("Content-Type"), Some("application/sdp"));
    assert_eq!(
        r.body.len(),
        150,
        "Content-Length   : 150 (space before colon)"
    );
    assert!(r.body.starts_with(b"v=0\r\n"));
}

#[test]
fn p01_intmeth_unusual_but_legal_characters() {
    // RFC 3.1.1.2
    let r = req(valid("intmeth"));
    let m = "!interesting-Method0123456789_*+`.%indeed'~";
    assert_eq!(r.method, Method::Other(m.into()));
    assert_eq!(r.headers.cseq().unwrap().method, Method::Other(m.into()));
    assert!(r.cseq_method_matches());
    let u = sip_uri(&r.uri);
    assert_eq!(
        u.user.as_deref(),
        Some("1_unusual.URI~(to-be!sure)&isn't+it$/crazy?,/;;*")
    );
    assert_eq!(
        u.password.as_deref(),
        Some("&it+has=1,weird!*pas$wo~d_too.(doesn't-it)")
    );
    assert_eq!(u.host, "example.com");
    assert_eq!(
        r.headers.top_via().unwrap().branch(),
        Some("z9hG4bK-.!%66*_+`'~")
    );
    let to = r.headers.to().unwrap();
    assert_eq!(
        to.display_name.as_deref(),
        Some("BEL:\u{7} NUL:\u{0} DEL:\u{7f}"),
        "quoted-pairs incl. NUL"
    );
    let from = r.headers.from().unwrap();
    assert_eq!(
        from.display_name.as_deref(),
        Some("token1~` token2'+_ token3*%!.-")
    );
    assert_eq!(
        from.param("fromParam''~+*_!.-%"),
        Some(Some("\"раб\u{43e}тающий\""))
    );
    assert_eq!(from.tag(), Some("_token~1'+`*%!-."));
    assert_eq!(
        r.headers.call_id(),
        Some("intmeth.word%ZK-!.*_+'@word`~)(><:\\/\"][?}{")
    );
    assert_eq!(
        r.headers.get("extensionHeader-!.%*+_`'~"),
        Some("\u{feff}大停電")
    );
    assert_eq!(r.headers.max_forwards().unwrap(), 255);
}

#[test]
fn p01_esc01_escapes_are_kept_raw_and_unescape_correctly() {
    // RFC 3.1.1.3
    let r = req(valid("esc01"));
    let u = sip_uri(&r.uri);
    assert_eq!(u.user.as_deref(), Some("sips%3Auser%40example.com"));
    assert_eq!(
        unescape(u.user.as_deref().unwrap()),
        b"sips:user@example.com"
    );
    assert_eq!(u.host, "example.net");
    let to = r.headers.to().unwrap();
    assert_eq!(unescape(sip_uri(&to.uri).user.as_deref().unwrap()), b"user");
    let c = &r.headers.contacts().unwrap()[0];
    let cu = sip_uri(&c.uri);
    assert_eq!(unescape(cu.user.as_deref().unwrap()), b"caller");
    assert_eq!(cu.params[0].name, "%6C%72");
    assert_eq!(
        unescape(&cu.params[0].name),
        b"lr",
        "%6C%72 is an escaped lr"
    );
    assert!(
        !cu.is_loose_router(),
        "the raw name '%6C%72' is not the token 'lr'"
    );
    assert_eq!(
        unescape(cu.params[1].value.as_deref().unwrap()),
        b"value%41"
    );
    // compact forms in this vector
    assert_eq!(
        r.headers.get("Call-ID"),
        Some("esc01.239409asdfakjkn23onasd0-3234")
    );
    assert_eq!(r.headers.get("Content-Type"), Some("application/sdp"));
    assert_eq!(r.body.len(), 150);
}

#[test]
fn p01_escnull_nul_in_uri_user() {
    // RFC 3.1.1.4
    let r = req(valid("escnull"));
    assert_eq!(r.method, Method::Register);
    let to = r.headers.to().unwrap();
    let user = sip_uri(&to.uri).user.clone().unwrap();
    assert_eq!(user, "null-%00-null");
    assert_eq!(
        unescape(&user),
        b"null-\0-null",
        "escaped NUL decodes to a real NUL byte"
    );
    let contacts = r.headers.contacts().unwrap();
    assert_eq!(contacts.len(), 2);
    assert_eq!(
        unescape(sip_uri(&contacts[0].uri).user.as_deref().unwrap()),
        b"\0"
    );
    assert_eq!(
        unescape(sip_uri(&contacts[1].uri).user.as_deref().unwrap()),
        b"\0\0"
    );
    assert_eq!(
        r.headers.get("Content-Length"),
        Some("0"),
        "'L:0' (upper-case compact)"
    );
}

#[test]
fn p01_esc02_percent_is_not_an_escape_outside_escaped_productions() {
    // RFC 3.1.1.5
    let r = req(valid("esc02"));
    assert_eq!(
        r.method,
        Method::Other("RE%47IST%45R".into()),
        "% in a method is literal"
    );
    assert_ne!(r.method, Method::Register);
    assert_eq!(
        r.headers.cseq().unwrap().method,
        Method::Other("RE%47IST%45R".into())
    );
    assert_eq!(
        r.headers.top_via().unwrap().branch(),
        Some("z9hG4bK209%fzsnel234")
    );
    assert_eq!(
        r.headers.to().unwrap().display_name.as_deref(),
        Some("%Z%45")
    );
    // 'C%6Fntact' is NOT Contact: 2 real Contacts + 1 unknown header with a literal % name
    assert_eq!(r.headers.get_all("Contact").count(), 2);
    assert_eq!(
        r.headers.get("C%6Fntact"),
        Some("<sip:alias2@host2.example.com>")
    );
    // invalid %-sequences stay literal when unescaping
    assert_eq!(unescape("%Z%45"), b"%ZE");
}

#[test]
fn p01_lwsdisp_no_space_between_display_name_and_bracket() {
    // RFC 3.1.1.6
    let r = req(valid("lwsdisp"));
    let from = r.headers.from().unwrap();
    assert_eq!(from.display_name.as_deref(), Some("caller"));
    assert_eq!(sip_uri(&from.uri).user.as_deref(), Some("caller"));
    assert_eq!(from.tag(), Some("323"));
}

#[test]
fn p01_longreq_long_values_and_many_vias() {
    // RFC 3.1.1.7
    let bytes = load("valid", "longreq");
    let r = req(parse_datagram(&bytes).unwrap());
    // Independent expectation derived from the raw bytes: number of header lines and of Via
    // lines (v / V / Via in any case, optional spaces before ':').
    let head_end = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap();
    let head = String::from_utf8_lossy(&bytes[..head_end]).into_owned();
    let lines: Vec<&str> = head.split("\r\n").skip(1).collect();
    let via_lines = lines
        .iter()
        .filter(|l| {
            let n = l.split(':').next().unwrap().trim().to_ascii_lowercase();
            n == "v" || n == "via"
        })
        .count();
    assert_eq!(via_lines, 34, "sanity check of the independent count");
    assert_eq!(r.headers.vias().unwrap().len(), via_lines);
    assert_eq!(
        r.headers.len(),
        lines.len(),
        "one entry per header line (no list splitting here)"
    );
    assert_eq!(r.headers.top_via().unwrap().host, "sip33.example.com");
    let last = r.headers.vias().unwrap().pop().unwrap();
    assert_eq!(last.host, "host.example.com", "last Via line of the file");
    assert_eq!(last.received(), Some("192.0.2.5"));
    assert!(last.branch().unwrap().starts_with("verylonglong"));
    // all four ways to spell the Via name end up as one canonical header
    assert!(r.headers.iter().filter(|h| h.name == "Via").count() == via_lines);
    let to = r.headers.to().unwrap();
    assert!(to.display_name.unwrap().len() > 100);
    let raw_call_id = lines.iter().find(|l| l.starts_with("Call-ID:")).unwrap();
    assert_eq!(
        r.headers.call_id().unwrap(),
        raw_call_id["Call-ID:".len()..].trim()
    );
    assert!(r.headers.call_id().unwrap().len() > 130);
    assert_eq!(r.body.len(), 150);
    assert!(bytes.len() > 3000);
}

#[test]
fn p01_dblreq_trailing_octets_after_the_message() {
    // RFC 3.1.1.8: Content-Length: 0 followed by a second, complete request in the datagram.
    let bytes = load("valid", "dblreq");
    // datagram: the extra octets are ignored (truncated, RFC 3261 18.3)
    let r = req(parse_datagram(&bytes).unwrap());
    assert_eq!(r.method, Method::Register);
    assert_eq!(
        r.headers.call_id(),
        Some("dblreq.0ha0isndaksdj99sdfafnl3lk233412"),
        "compact 'I:'"
    );
    assert!(r.body.is_empty());
    // stream: the first request is consumed alone, the second parses from the remainder
    let (first, used) = parse_message(&bytes).unwrap();
    assert_eq!(req(first).method, Method::Register);
    assert!(used < bytes.len());
    let (second, used2) = parse_message(&bytes[used..]).unwrap();
    let second = req(second);
    assert_eq!(second.method, Method::Invite);
    assert_eq!(
        second.headers.call_id(),
        Some("dblreq.0ha0isnda977644900765@192.0.2.15")
    );
    assert_eq!(second.headers.to().unwrap().uri, second.uri, "compact 't:'");
    assert_eq!(second.body.len(), 150);
    // The RFC's own Content-Length (150) undercounts the SDP of the second request by 5 octets;
    // those remain as unconsumed stream bytes (a real stream would now be desynchronised, which
    // is why Content-Length must be right). Documented here instead of papered over.
    let rest = &bytes[used + used2..];
    assert_eq!(rest.len(), 5);
    assert!(rest.ends_with(b"\r\n\r\n"), "{rest:?}");
}

#[test]
fn p01_semiuri_semicolon_params_in_user_part() {
    // RFC 3.1.1.9
    let r = req(valid("semiuri"));
    let u = sip_uri(&r.uri);
    assert_eq!(u.user.as_deref(), Some("user;par=u%40example.net"));
    assert_eq!(u.host, "example.com");
    assert!(
        u.params.is_empty(),
        "';par=...' belongs to the user part, not to URI params"
    );
    // folded + comma separated Accept header: 6 elements
    let accepts: Vec<&str> = r.headers.get_all("Accept").collect();
    assert_eq!(
        accepts,
        [
            "application/sdp",
            "application/pkcs7-mime",
            "multipart/mixed",
            "multipart/signed",
            "message/sip",
            "message/sipfrag"
        ]
    );
}

#[test]
fn p01_transports_varied_and_unknown() {
    // RFC 3.1.1.10
    let r = req(valid("transports"));
    let t: Vec<String> = r
        .headers
        .vias()
        .unwrap()
        .into_iter()
        .map(|v| v.transport)
        .collect();
    assert_eq!(t, ["UDP", "SCTP", "TLS", "UNKNOWN", "TCP"]);
    assert_eq!(
        r.headers.get("Call-ID"),
        Some("transports.kijh4akdnaqjkwendsasfdj")
    );
}

#[test]
fn p01_mpart01_binary_multipart_body_is_kept_byte_exact() {
    // RFC 3.1.1.11
    let bytes = load("valid", "mpart01");
    let r = req(parse_datagram(&bytes).unwrap());
    assert_eq!(r.method, Method::Message);
    assert_eq!(r.body.len(), 553);
    let head_end = bytes.windows(4).position(|w| w == b"\r\n\r\n").unwrap() + 4;
    assert_eq!(
        &r.body[..],
        &bytes[head_end..],
        "body bytes identical, incl. non-UTF-8 octets"
    );
    assert!(r.body.contains(&0x82), "binary DER content is present");
    assert!(
        std::str::from_utf8(&r.body).is_err(),
        "body is genuinely not UTF-8"
    );
    assert_eq!(
        r.headers.get("Content-Type"),
        Some("multipart/mixed;boundary=7a9cbec02ceef655")
    );
    assert_eq!(
        r.headers.top_via().unwrap().rport(),
        Some(None),
        "bare rport"
    );
    // and it serializes back to exactly the same bytes
    assert_eq!(&r.to_bytes()[..], &bytes[..]);
}

#[test]
fn p01_unreason_and_noreason() {
    // RFC 3.1.1.12 / 3.1.1.13
    let r = resp(valid("unreason"));
    assert_eq!(r.status, StatusCode::OK);
    assert_eq!(r.reason, "= 2**3 * 5**2 но сто девяносто девять - простое");
    assert_eq!(r.body.len(), 154);
    let n = resp(valid("noreason"));
    assert_eq!(n.status, StatusCode::TRYING);
    assert_eq!(n.reason, "", "'SIP/2.0 100 ' has an empty reason phrase");
}

#[test]
fn p01_application_layer_vectors_parse_as_well_formed() {
    // RFC 3.3.x: the *parser* must accept these; rejecting them is the job of upper layers.
    let r = req(valid("unkscm"));
    assert_eq!(
        r.uri,
        Uri::Other {
            scheme: "nobodyKnowsThisScheme".into(),
            rest: "totallyopaquecontent".into()
        },
        "unknown Request-URI scheme (answer 416 at the UAS)"
    );
    let r = req(valid("novelsc"));
    assert_eq!(
        r.uri,
        Uri::Other {
            scheme: "soap.beep".into(),
            rest: "//192.0.2.103:3002".into()
        }
    );
    let r = req(valid("unksm2"));
    assert_eq!(
        r.headers.to().unwrap().uri,
        Uri::Other {
            scheme: "isbn".into(),
            rest: "2983792873".into()
        }
    );
    assert_eq!(
        r.headers.from().unwrap().uri,
        Uri::Other {
            scheme: "http".into(),
            rest: "//www.example.com".into()
        }
    );
    assert_eq!(
        r.headers.contacts().unwrap()[0].uri,
        Uri::Other {
            scheme: "name".into(),
            rest: "John_Smith".into()
        }
    );
    let r = req(valid("bext01"));
    assert_eq!(
        r.headers.get_all("Require").collect::<Vec<_>>(),
        ["nothingSupportsThis", "nothingSupportsThisEither"]
    );
    assert_eq!(
        r.headers.get_all("Proxy-Require").collect::<Vec<_>>(),
        ["noProxiesSupportThis", "norDoAnyProxiesSupportThis"]
    );
    assert_eq!(r.headers.top_via().unwrap().transport, "TLS");
    let r = req(valid("invut"));
    assert_eq!(
        r.headers.get("Content-Type"),
        Some("application/unknownformat")
    );
    assert_eq!(r.body.len(), 40);
    let r = req(valid("regaut01"));
    assert_eq!(
        r.headers.get("Authorization"),
        Some("NoOneKnowsThisScheme opaque-data=here")
    );
    assert_eq!(
        r.headers.get("Content-Length"),
        Some("0"),
        "'Content-Length:0' without space"
    );
    let b = resp(valid("bcast"));
    let vias = b.headers.vias().unwrap();
    assert_eq!(
        vias[1].host, "255.255.255.255",
        "parser keeps it; proxies must drop such responses"
    );
    let z = req(valid("zeromf"));
    assert_eq!(z.headers.max_forwards().unwrap(), 0);
    let c1 = req(valid("cparam01"));
    let c = &c1.headers.contacts().unwrap()[0];
    assert!(
        c.params
            .contains(&Param::new("unknownparam", None::<String>)),
        "contact-param"
    );
    assert!(sip_uri(&c.uri).params.is_empty(), "NOT a url-param");
    let c2 = req(valid("cparam02"));
    let c = &c2.headers.contacts().unwrap()[0];
    assert!(c.params.is_empty());
    assert_eq!(
        sip_uri(&c.uri).param("unknownparam"),
        Some(None),
        "url-param inside <>"
    );
    let e = req(valid("regescrt"));
    let c = &e.headers.contacts().unwrap()[0];
    assert_eq!(
        c.uri.as_sip().unwrap().headers,
        vec![("Route".to_string(), "%3Csip:sip.example.com%3E".to_string())]
    );
    assert_eq!(
        e.headers.get("Contact"),
        Some("<sip:user@example.com?Route=%3Csip:sip.example.com%3E>"),
        "compact 'M:'"
    );
    let s = req(valid("sdp01"));
    assert_eq!(s.headers.get("Accept"), Some("text/nobodyKnowsThis"));
    assert_eq!(s.body.len(), 150);
}

// ------------------------------------------------------------------------------------------
// P02: the "invalid" directory. Every vector has an explicit expected outcome.
// ------------------------------------------------------------------------------------------

enum Outcome {
    /// parse must fail with an error accepted by the predicate (in datagram mode)
    Reject(fn(&ParseError) -> bool),
    /// parse must succeed; the layer named in the string has to produce the error response
    Tolerate(&'static str),
}

struct Case {
    name: &'static str,
    outcome: Outcome,
    why: &'static str,
}

macro_rules! rej {
    ($p:pat) => {
        Outcome::Reject(|e| matches!(e, $p))
    };
}

fn cases() -> Vec<Case> {
    vec![
        Case {
            name: "badinv01",
            outcome: rej!(ParseError::InvalidHeader { .. }),
            why: "3.1.2.1 Via `;;,;,,` has empty parameters/list elements: syntactically invalid, answer 400",
        },
        Case {
            name: "clerr",
            outcome: rej!(ParseError::ContentLengthMismatch {
                declared: 9999,
                available: 154
            }),
            why: "3.1.2.2 UDP: Content-Length larger than body, answer 400. (Stream mode: Incomplete, see test below)",
        },
        Case {
            name: "ncl",
            outcome: rej!(ParseError::InvalidContentLength),
            why: "3.1.2.3 Content-Length: -999 is not a number; framing error, answer 400 / close TCP",
        },
        Case {
            name: "scalar02",
            outcome: rej!(ParseError::InvalidHeader { name: "CSeq", .. }),
            why: "3.1.2.4 CSeq 36893488147419103232 > 2^32-1 -> 400. (Max-Forwards 300 and the huge Expires would only be tolerated; CSeq decides)",
        },
        Case {
            name: "scalarlg",
            outcome: rej!(ParseError::InvalidHeader { name: "CSeq", .. }),
            why: "3.1.2.5 response with CSeq > 2^32-1: receiver 'will simply discard it' = parse failure",
        },
        Case {
            name: "quotbal",
            outcome: rej!(ParseError::InvalidHeader { name: "To", .. }),
            why: "3.1.2.6 unterminated quoted display name in To; 400 (inferring the quote is allowed but we are strict)",
        },
        Case {
            name: "ltgtruri",
            outcome: rej!(ParseError::InvalidUri(_)),
            why: "3.1.2.7 Request-URI enclosed in <>: 'reasonable always to reject' with 400",
        },
        Case {
            name: "lwsruri",
            outcome: rej!(ParseError::InvalidStartLine(_)),
            why: "3.1.2.8 embedded LWS in Request-URI splits the request line into 4 parts: 400",
        },
        Case {
            name: "lwsstart",
            outcome: rej!(ParseError::InvalidStartLine(_)),
            why: "3.1.2.9 multiple SP between request-line elements: RFC says rejecting is acceptable; we reject",
        },
        Case {
            name: "trws",
            outcome: rej!(ParseError::InvalidStartLine(_)),
            why: "3.1.2.10 trailing SPs after SIP/2.0: RFC says rejecting is acceptable; we reject",
        },
        Case {
            name: "escruri",
            outcome: Outcome::Tolerate(
                "UAS may answer 400; the URI model keeps the escaped header",
            ),
            why: "3.1.2.11 URI headers in Request-URI: RFC allows either rejecting or ignoring; the SipUri grammar legitimately has headers, so we tolerate and expose them",
        },
        Case {
            name: "baddate",
            outcome: Outcome::Tolerate("only a layer that interprets Date; we never do"),
            why: "3.1.2.12 'EST' time zone in Date: 'an element should not reject unless Date matters'",
        },
        Case {
            name: "regbadct",
            outcome: rej!(ParseError::InvalidHeader {
                name: "Contact",
                ..
            }),
            why: "3.1.2.13 URI with '?' in Contact must be in <>; we reject (400)",
        },
        Case {
            name: "badaspec",
            outcome: rej!(ParseError::InvalidHeader { name: "To", .. }),
            why: "3.1.2.14 spaces inside <addr-spec> in To; RFC: reasonable to reject with 400",
        },
        Case {
            name: "baddn",
            outcome: rej!(ParseError::InvalidHeader { name: "To", .. }),
            why: "3.1.2.15 unquoted display name with non-token chars (comma); 'reasonable always to reject'",
        },
        Case {
            name: "badvers",
            outcome: rej!(ParseError::UnsupportedVersion),
            why: "3.1.2.16 SIP/7.0: well-formed but unsupported; the transport layer should answer 505",
        },
        Case {
            name: "mismatch01",
            outcome: Outcome::Tolerate(
                "UAS/transaction layer must answer 400 (checked via cseq_method_matches)",
            ),
            why: "3.1.2.17 request-line OPTIONS vs CSeq INVITE: syntactically fine, semantic error",
        },
        Case {
            name: "mismatch02",
            outcome: Outcome::Tolerate("UAS answers 501 (unknown method) or 400"),
            why: "3.1.2.18 unknown method NEWMETHOD vs CSeq INVITE: syntactically fine, semantic error",
        },
        Case {
            name: "bigcode",
            outcome: rej!(ParseError::InvalidStatusCode),
            why: "3.1.2.19 status code 4294967301 is not 100..=699: response 'should simply be dropped'",
        },
        Case {
            name: "badbranch",
            outcome: Outcome::Tolerate(
                "transaction layer (400, or RFC 2543 fallback which we do not support)",
            ),
            why: "3.2.1 branch=z9hG4bK with empty id: 'a parser must not break'; the Via is syntactically valid",
        },
        Case {
            name: "insuf",
            outcome: rej!(ParseError::MissingHeader("To")),
            why: "3.3.1 no To/From/Call-ID: mandatory headers are checked by the parser (first missing in spec order: To)",
        },
        Case {
            name: "multi01",
            outcome: rej!(ParseError::MultipleHeader("To")),
            why: "3.3.8 duplicate Call-ID/To/From/Max-Forwards/CSeq: 400",
        },
        Case {
            name: "mcl01",
            outcome: rej!(ParseError::InvalidContentLength),
            why: "3.3.9 conflicting Content-Length values: equivalent to an invalid length",
        },
        Case {
            name: "inv2543",
            outcome: rej!(ParseError::MissingHeader("Max-Forwards")),
            why: "3.4.1 RFC 2543 style INVITE has no Max-Forwards; docs/design/sip-core.md section 2 makes Max-Forwards mandatory for requests, so the SPEC-MANDATED behaviour is reject (the RFC says 'should be accepted by elements that want backward compatibility'; we deliberately do not)",
        },
    ]
}

#[test]
fn p02_every_invalid_vector_has_an_expectation_and_meets_it() {
    let mut on_disk: Vec<String> = std::fs::read_dir(format!(
        "{}/tests/vectors/rfc4475/invalid",
        env!("CARGO_MANIFEST_DIR")
    ))
    .unwrap()
    .map(|e| {
        e.unwrap()
            .path()
            .file_stem()
            .unwrap()
            .to_string_lossy()
            .into_owned()
    })
    .collect();
    on_disk.sort();
    let mut table: Vec<String> = cases().iter().map(|c| c.name.to_string()).collect();
    table.sort();
    assert_eq!(
        on_disk, table,
        "vector directory and expectation table differ"
    );

    for c in cases() {
        let bytes = load("invalid", c.name);
        let got = parse_datagram(&bytes);
        match (&c.outcome, got) {
            (Outcome::Reject(pred), Err(e)) => {
                assert!(
                    pred(&e),
                    "{}: rejected with unexpected error {e:?} ({})",
                    c.name,
                    c.why
                )
            }
            (Outcome::Reject(_), Ok(m)) => {
                panic!(
                    "{}: expected rejection, parsed as {m:?} ({})",
                    c.name, c.why
                )
            }
            (Outcome::Tolerate(_), Ok(_)) => {}
            (Outcome::Tolerate(layer), Err(e)) => {
                panic!("{}: expected tolerated ({layer}), got {e:?}", c.name)
            }
        }
    }
}

#[test]
fn p02_rejected_vectors_are_also_rejected_in_stream_mode() {
    // With the exception of clerr / baddn (see below) the verdict does not depend on framing.
    for c in cases() {
        if matches!(c.outcome, Outcome::Tolerate(_)) || c.name == "clerr" || c.name == "baddn" {
            continue;
        }
        let bytes = load("invalid", c.name);
        let got = parse_message(&bytes);
        match (&c.outcome, got) {
            (Outcome::Reject(pred), Err(e)) => assert!(pred(&e), "{}: {e:?}", c.name),
            (_, other) => panic!("{}: stream mode gave {other:?}", c.name),
        }
    }
}

#[test]
fn p02_clerr_on_a_stream_waits_for_more_data() {
    // 3.1.2.2: "If this message arrived over TCP there's not much the receiving party could do
    // but wait for more data". Incomplete is exactly that; the transport applies the timeout.
    let bytes = load("invalid", "clerr");
    assert_eq!(parse_message(&bytes), Err(ParseError::Incomplete));
    // once the declared 9999 bytes are there it parses
    let mut full = bytes.clone();
    full.resize(bytes.len() - 154 + 9999, b'x');
    let (m, used) = parse_message(&full).unwrap();
    assert_eq!(used, full.len());
    assert_eq!(m.body().len(), 9999);
}

#[test]
fn p02_baddn_is_rejected_for_its_display_name_not_for_missing_blank_line() {
    // The archived file ends without the final empty line. Datagram mode tolerates that (the
    // case table above); in stream mode the message is simply incomplete...
    let bytes = load("invalid", "baddn");
    assert_eq!(parse_message(&bytes), Err(ParseError::Incomplete));
    // ...and with the terminator added it is rejected for the display name.
    let mut done = bytes.clone();
    done.extend_from_slice(b"\r\n");
    assert!(matches!(
        parse_message(&done),
        Err(ParseError::InvalidHeader { name: "To", .. })
    ));
}

#[test]
fn p02_tolerated_vectors_expose_what_the_upper_layers_need() {
    // mismatch01 / mismatch02: both parse; the caller can detect the mismatch.
    let m1 = req(parse_datagram(&load("invalid", "mismatch01")).unwrap());
    assert_eq!(m1.method, Method::Options);
    assert_eq!(m1.headers.cseq().unwrap().method, Method::Invite);
    assert!(!m1.cseq_method_matches());
    let m2 = req(parse_datagram(&load("invalid", "mismatch02")).unwrap());
    assert_eq!(m2.method, Method::Other("NEWMETHOD".into()));
    assert!(!m2.cseq_method_matches());
    // badbranch: magic cookie without identifier is visible to the transaction layer.
    let b = req(parse_datagram(&load("invalid", "badbranch")).unwrap());
    assert_eq!(b.headers.top_via().unwrap().branch(), Some("z9hG4bK"));
    // escruri: the escaped header is available but is not turned into a real header.
    let e = req(parse_datagram(&load("invalid", "escruri")).unwrap());
    assert_eq!(
        sip_uri(&e.uri).headers,
        vec![("Route".to_string(), "%3Csip:example.com%3E".to_string())]
    );
    assert!(!e.headers.contains("Route"));
    // baddate: kept verbatim.
    let d = req(parse_datagram(&load("invalid", "baddate")).unwrap());
    assert_eq!(d.headers.get("Date"), Some("Fri, 01 Jan 2010 16:00:00 EST"));
}

#[test]
fn p02_matching_requests_have_matching_cseq() {
    // control for the mismatch tests: a well-formed request reports a match
    let ok = req(valid("wsinv"));
    assert!(ok.cseq_method_matches());
}

#[test]
fn p02_error_for_mandatory_header_names_the_header() {
    // insuf lacks To, From and Call-ID (and Max-Forwards): the first one in spec order is named
    let e = parse_datagram(&load("invalid", "insuf")).unwrap_err();
    assert_eq!(e, ParseError::MissingHeader("To"));
    assert_eq!(e.to_string(), "missing mandatory header To");
}
