//! C02 / C03: calling and called number extraction (docs/design/m3-store.md section 2).

use fax_in::config::NumbersSection;
use fax_in::numbers::{self, Numbers, extract, normalize};
use sip_core::message::{Request, SipMessage, parse_datagram};

/// An INVITE with the given request line URI, From, To and extra header lines.
fn invite(ruri: &str, from: &str, to: &str, extra: &[&str]) -> Request {
    let mut t = format!(
        "INVITE {ruri} SIP/2.0\r\nVia: SIP/2.0/UDP 192.0.2.10:5060;branch=z9hG4bKc02\r\nMax-Forwards: 70\r\nFrom: {from};tag=a1\r\nTo: {to}\r\nCall-ID: c02@test\r\nCSeq: 1 INVITE\r\nContact: <sip:gw@192.0.2.10:5060>\r\n"
    );
    for e in extra {
        t.push_str(e);
        t.push_str("\r\n");
    }
    t.push_str("Content-Length: 0\r\n\r\n");
    match parse_datagram(t.as_bytes()) {
        Ok(SipMessage::Request(r)) => r,
        other => panic!("test INVITE does not parse: {other:?}\n{t}"),
    }
}

fn cfg() -> NumbersSection {
    NumbersSection::default()
}

fn nums(
    ani: Option<&str>,
    a_src: Option<&str>,
    dnis: Option<&str>,
    d_src: Option<&str>,
) -> Numbers {
    Numbers {
        ani: ani.map(Into::into),
        ani_source: a_src.map(Into::into),
        dnis: dnis.map(Into::into),
        dnis_source: d_src.map(Into::into),
    }
}

struct Case {
    name: &'static str,
    req: Request,
    cfg: NumbersSection,
    want: Numbers,
}

fn cases() -> Vec<Case> {
    let f = "<sip:0200000001@192.0.2.10>";
    let t = "<sip:0200000002@198.51.100.20>";
    let ru = "sip:0200000002@198.51.100.20";
    let mut v = Vec::new();
    let mut add = |name, req, cfg, want| {
        v.push(Case {
            name,
            req,
            cfg,
            want,
        })
    };

    // ---- ANI source priority: pai, rpid, from
    add(
        "From only (the AudioCodes case)",
        invite(ru, f, t, &[]),
        cfg(),
        nums(
            Some("0200000001"),
            Some("from"),
            Some("0200000002"),
            Some("ruri"),
        ),
    );
    add(
        "PAI wins over RPID and From",
        invite(
            ru,
            f,
            t,
            &[
                "P-Asserted-Identity: <sip:0311111111@carrier.example>",
                "Remote-Party-ID: <sip:0322222222@carrier.example>;party=calling",
            ],
        ),
        cfg(),
        nums(
            Some("0311111111"),
            Some("pai"),
            Some("0200000002"),
            Some("ruri"),
        ),
    );
    add(
        "RPID wins over From when there is no PAI",
        invite(
            ru,
            f,
            t,
            &[
                "Remote-Party-ID: \"Alice\" <sip:0322222222@carrier.example>;party=calling;screen=yes",
            ],
        ),
        cfg(),
        nums(
            Some("0322222222"),
            Some("rpid"),
            Some("0200000002"),
            Some("ruri"),
        ),
    );
    add(
        "PAI with a sip and a tel identity: the first usable one",
        invite(
            ru,
            f,
            t,
            &["P-Asserted-Identity: <tel:+886-2-2345-6789>, <sip:0311111111@carrier.example>"],
        ),
        cfg(),
        nums(
            Some("+886223456789"),
            Some("pai"),
            Some("0200000002"),
            Some("ruri"),
        ),
    );
    add(
        "PAI in compact-less multiple header lines",
        invite(
            ru,
            f,
            t,
            &[
                "P-Asserted-Identity: <sip:0311111111@carrier.example>",
                "P-Asserted-Identity: <tel:+886222222222>",
            ],
        ),
        cfg(),
        nums(
            Some("0311111111"),
            Some("pai"),
            Some("0200000002"),
            Some("ruri"),
        ),
    );
    // ---- configured order
    let mut from_first = cfg();
    from_first.ani_sources = vec!["from".into(), "pai".into()];
    add(
        "ani_sources = [from, pai]",
        invite(
            ru,
            f,
            t,
            &["P-Asserted-Identity: <sip:0311111111@carrier.example>"],
        ),
        from_first,
        nums(
            Some("0200000001"),
            Some("from"),
            Some("0200000002"),
            Some("ruri"),
        ),
    );
    let mut pai_only = cfg();
    pai_only.ani_sources = vec!["pai".into()];
    add(
        "ani_sources = [pai] and no PAI: no ANI at all",
        invite(ru, f, t, &[]),
        pai_only,
        nums(None, None, Some("0200000002"), Some("ruri")),
    );
    // ---- URI forms
    add(
        "tel: URI in From",
        invite(ru, "<tel:0212345678>", t, &[]),
        cfg(),
        nums(
            Some("0212345678"),
            Some("from"),
            Some("0200000002"),
            Some("ruri"),
        ),
    );
    add(
        "user parameters are cut off",
        invite(
            ru,
            "<sip:0212345678;phone-context=+886@192.0.2.10;user=phone>",
            t,
            &[],
        ),
        cfg(),
        nums(
            Some("0212345678"),
            Some("from"),
            Some("0200000002"),
            Some("ruri"),
        ),
    );
    add(
        "percent-escaped user",
        invite(ru, "<sip:%2B886223456789@192.0.2.10>", t, &[]),
        cfg(),
        nums(
            Some("+886223456789"),
            Some("from"),
            Some("0200000002"),
            Some("ruri"),
        ),
    );
    add(
        "visual separators are removed",
        invite(ru, "<sip:+886-(2)2345.6789@192.0.2.10>", t, &[]),
        cfg(),
        nums(
            Some("+886223456789"),
            Some("from"),
            Some("0200000002"),
            Some("ruri"),
        ),
    );
    // ---- strip_plus and prefixes
    let mut strip = cfg();
    strip.strip_plus = true;
    add(
        "strip_plus",
        invite(
            ru,
            "<sip:+886223456789@192.0.2.10>",
            "<sip:+886255501234@x>",
            &[],
        ),
        strip,
        nums(
            Some("886223456789"),
            Some("from"),
            Some("0200000002"),
            Some("ruri"),
        ),
    );
    let mut prefix = cfg();
    prefix.ani_strip_prefix = Some("+886".into());
    prefix.dnis_strip_prefix = Some("886".into());
    add(
        "ani_strip_prefix and dnis_strip_prefix",
        invite(
            "sip:+886255501234@198.51.100.20",
            "<sip:+886223456789@192.0.2.10>",
            t,
            &[],
        ),
        prefix,
        nums(
            Some("223456789"),
            Some("from"),
            Some("255501234"),
            Some("ruri"),
        ),
    );
    let mut both = cfg();
    both.strip_plus = true;
    both.ani_strip_prefix = Some("886".into());
    add(
        "a prefix without + also matches a number with +, then strip_plus",
        invite(ru, "<sip:+886223456789@192.0.2.10>", t, &[]),
        both,
        nums(
            Some("223456789"),
            Some("from"),
            Some("0200000002"),
            Some("ruri"),
        ),
    );
    // ---- anonymous
    add(
        "anonymous From",
        invite(
            ru,
            "\"Anonymous\" <sip:anonymous@anonymous.invalid>",
            t,
            &[],
        ),
        cfg(),
        nums(None, Some("anonymous"), Some("0200000002"), Some("ruri")),
    );
    add(
        "Privacy: id hides a PAI number",
        invite(
            ru,
            f,
            t,
            &[
                "P-Asserted-Identity: <sip:0311111111@carrier.example>",
                "Privacy: id",
            ],
        ),
        cfg(),
        nums(None, Some("anonymous"), Some("0200000002"), Some("ruri")),
    );
    add(
        "Privacy: id;critical hides the From number",
        invite(ru, f, t, &["Privacy: id;critical"]),
        cfg(),
        nums(None, Some("anonymous"), Some("0200000002"), Some("ruri")),
    );
    add(
        "Privacy: none does not hide anything",
        invite(ru, f, t, &["Privacy: none"]),
        cfg(),
        nums(
            Some("0200000001"),
            Some("from"),
            Some("0200000002"),
            Some("ruri"),
        ),
    );
    add(
        "Remote-Party-ID privacy=full",
        invite(
            ru,
            f,
            t,
            &["Remote-Party-ID: <sip:0322222222@carrier.example>;party=calling;privacy=full"],
        ),
        cfg(),
        nums(None, Some("anonymous"), Some("0200000002"), Some("ruri")),
    );
    // ---- DNIS sources
    add(
        "To is used when the Request-URI has no user",
        invite("sip:198.51.100.20", f, t, &[]),
        cfg(),
        nums(
            Some("0200000001"),
            Some("from"),
            Some("0200000002"),
            Some("to"),
        ),
    );
    let mut to_first = cfg();
    to_first.dnis_sources = vec!["to".into(), "ruri".into()];
    add(
        "dnis_sources = [to, ruri]",
        invite("sip:0299999999@198.51.100.20", f, t, &[]),
        to_first,
        nums(
            Some("0200000001"),
            Some("from"),
            Some("0200000002"),
            Some("to"),
        ),
    );
    let mut div = cfg();
    div.dnis_sources = vec!["diversion".into(), "ruri".into()];
    add(
        "diversion: the original called number is the last entry",
        invite(
            "sip:0288888888@198.51.100.20",
            f,
            t,
            &[
                "Diversion: <sip:0277777777@pbx.example>;reason=unconditional, <sip:0266666666@pbx.example>;reason=user-busy",
            ],
        ),
        div.clone(),
        nums(
            Some("0200000001"),
            Some("from"),
            Some("0266666666"),
            Some("diversion"),
        ),
    );
    add(
        "diversion listed but absent falls back to ruri",
        invite(ru, f, t, &[]),
        div,
        nums(
            Some("0200000001"),
            Some("from"),
            Some("0200000002"),
            Some("ruri"),
        ),
    );
    // ---- nothing at all
    add(
        "no user parts anywhere",
        invite(
            "sip:198.51.100.20",
            "<sip:192.0.2.10>",
            "<sip:198.51.100.20>",
            &[],
        ),
        cfg(),
        nums(None, None, None, None),
    );
    v
}

#[test]
fn c02_every_row_of_the_extraction_table() {
    let cases = cases();
    assert!(cases.len() >= 20);
    for c in cases {
        assert_eq!(extract(&c.req, &c.cfg), c.want, "case: {}", c.name);
    }
}

#[test]
fn c02_normalize_corner_cases() {
    assert_eq!(normalize("0 2-2345.6789", false, None), "0223456789");
    assert_eq!(normalize("(02)2345-6789", false, None), "0223456789");
    // a prefix that is the whole number is not stripped (never an empty number)
    assert_eq!(normalize("886", false, Some("886")), "886");
    // a prefix that does not match changes nothing
    assert_eq!(normalize("0223456789", false, Some("886")), "0223456789");
    // the prefix can carry separators too
    assert_eq!(
        normalize("+886223456789", false, Some("+886-")),
        "223456789"
    );
    // a plus-prefix does not match a number without plus
    assert_eq!(
        normalize("886223456789", false, Some("+886")),
        "886223456789"
    );
    assert_eq!(normalize("+886223456789", true, None), "886223456789");
    assert_eq!(normalize("", true, Some("886")), "");
}

#[test]
fn c02_the_config_checks_the_source_names() {
    assert!(NumbersSection::default().check().is_ok());
    let mut c = NumbersSection {
        ani_sources: vec!["pai".into(), "contact".into()],
        ..NumbersSection::default()
    };
    assert!(c.check().unwrap_err().contains("contact"));
    c.ani_sources = vec![];
    assert!(c.check().unwrap_err().contains("empty"));
    let mut c = NumbersSection {
        dnis_sources: vec!["ruri".into(), "pai".into()],
        ..NumbersSection::default()
    };
    assert!(c.check().unwrap_err().contains("dnis_sources"));
    c.dnis_sources = vec!["diversion".into()];
    assert!(c.check().is_ok());
}

#[test]
fn c02_numbers_of_old_result_files_come_from_the_from_and_to_uris() {
    let n = numbers::from_uris(
        "sip:0200000001@192.0.2.10",
        "sip:0200000002@198.51.100.20;user=phone",
    );
    assert_eq!(
        n,
        nums(
            Some("0200000001"),
            Some("from"),
            Some("0200000002"),
            Some("to")
        )
    );
    let n = numbers::from_uris("sip:anonymous@anonymous.invalid", "sip:198.51.100.20");
    assert_eq!(n, nums(None, Some("anonymous"), None, None));
    assert_eq!(numbers::from_uris("garbage", "garbage"), Numbers::default());
}

/// The first request in a `sip.log` capture (`[time] RECV from ...` followed by the message).
fn first_request_of_log(log: &str) -> String {
    let log = log.replace("\r\n", "\n");
    let mut it = log.splitn(2, '\n');
    let header = it.next().unwrap();
    assert!(
        header.contains("RECV"),
        "first entry is the received INVITE: {header}"
    );
    let rest = it.next().unwrap();
    // entries are separated by a blank line followed by the next `[timestamp]`; the message's own
    // header/body separator is also a blank line, so cut at the next entry marker
    let end = rest.find("\n\n[").map_or(rest.len(), |i| i + 1);
    // The capture trims the body, so its Content-Length no longer matches; the numbers do not
    // depend on the SDP, so keep the headers only.
    let head = &rest[..rest.find("\n\n").unwrap_or(end)];
    let head: Vec<String> = head
        .lines()
        .map(|l| {
            if l.starts_with("Content-Length:") {
                "Content-Length: 0".to_string()
            } else {
                l.to_string()
            }
        })
        .collect();
    format!("{}\r\n\r\n", head.join("\r\n"))
}

/// C03: the de-identified AudioCodes Mediant 1000 trace of the F1 field problem.
#[test]
fn c03_the_audiocodes_field_invite() {
    const LOG: &str = include_str!(
        "../../../tools/fax-sim/tests/traces/audiocodes-m1000-6.80A-8page-fail.sip.log"
    );
    let text = first_request_of_log(LOG);
    let req = match parse_datagram(text.as_bytes()) {
        Ok(SipMessage::Request(r)) => r,
        other => panic!("the field INVITE does not parse: {other:?}"),
    };
    assert_eq!(req.method.as_str(), "INVITE");
    // the trace has no PAI / RPID / Privacy: the number is in From and in the Request-URI
    assert_eq!(
        extract(&req, &NumbersSection::default()),
        nums(
            Some("0200000001"),
            Some("from"),
            Some("0200000002"),
            Some("ruri")
        )
    );
    // and with the `to` source first the called number is the same one, from the To header
    let c = NumbersSection {
        dnis_sources: vec!["to".into()],
        ..NumbersSection::default()
    };
    assert_eq!(extract(&req, &c).dnis_source.as_deref(), Some("to"));
    assert_eq!(extract(&req, &c).dnis.as_deref(), Some("0200000002"));
}
