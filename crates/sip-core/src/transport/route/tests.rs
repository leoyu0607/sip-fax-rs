use super::*;
use crate::message::{Method, SipUri};

fn request_with_via(via: &str) -> Request {
    let mut r = Request::new(Method::Options, SipUri::new("example.com"));
    r.headers.push("Via", via);
    r
}

fn top(r: &Request) -> String {
    r.headers.get("Via").unwrap().to_string()
}

#[test]
fn n01_stamp_rport_and_received() {
    let mut r = request_with_via("SIP/2.0/UDP 10.0.0.9:5060;rport;branch=z9hG4bKa");
    assert!(stamp_inbound_via(&mut r, "10.0.0.9:40000".parse().unwrap()));
    assert_eq!(
        top(&r),
        "SIP/2.0/UDP 10.0.0.9:5060;rport=40000;branch=z9hG4bKa;received=10.0.0.9"
    );
}

#[test]
fn n02_stamp_received_when_sent_by_differs() {
    let mut r = request_with_via("SIP/2.0/UDP pbx.example.com:5099;branch=z9hG4bKa");
    assert!(stamp_inbound_via(
        &mut r,
        "192.0.2.5:12345".parse().unwrap()
    ));
    assert_eq!(
        top(&r),
        "SIP/2.0/UDP pbx.example.com:5099;branch=z9hG4bKa;received=192.0.2.5",
        "no rport requested: rport must not be invented"
    );
}

#[test]
fn n02_no_stamp_when_sent_by_equals_source() {
    let mut r = request_with_via("SIP/2.0/UDP 192.0.2.5:5060;branch=z9hG4bKa");
    assert!(!stamp_inbound_via(
        &mut r,
        "192.0.2.5:5060".parse().unwrap()
    ));
    assert_eq!(top(&r), "SIP/2.0/UDP 192.0.2.5:5060;branch=z9hG4bKa");
}

#[test]
fn n02_spoofed_received_is_discarded() {
    let mut r = request_with_via("SIP/2.0/UDP 192.0.2.5:5060;received=6.6.6.6;branch=z9hG4bKa");
    assert!(stamp_inbound_via(&mut r, "192.0.2.5:5060".parse().unwrap()));
    assert_eq!(top(&r), "SIP/2.0/UDP 192.0.2.5:5060;branch=z9hG4bKa");
    let mut r = request_with_via("SIP/2.0/UDP a.example:5060;received=6.6.6.6;branch=z9hG4bKa");
    stamp_inbound_via(&mut r, "192.0.2.5:5060".parse().unwrap());
    assert_eq!(
        r.headers.top_via().unwrap().received(),
        Some("192.0.2.5"),
        "replaced by the real source"
    );
}

#[test]
fn n07_stamp_ipv6_and_mapped() {
    let mut r = request_with_via("SIP/2.0/UDP [::1]:5060;rport;branch=z9hG4bKa");
    stamp_inbound_via(&mut r, "[::1]:40001".parse().unwrap());
    let v = r.headers.top_via().unwrap();
    assert_eq!(v.received(), Some("::1"));
    assert_eq!(v.rport(), Some(Some(40001)));
    // a dual-stack socket reports an IPv4 peer as ::ffff:a.b.c.d
    let mut r = request_with_via("SIP/2.0/UDP 10.1.1.1:5060;branch=z9hG4bKa");
    assert!(!stamp_inbound_via(
        &mut r,
        "[::ffff:10.1.1.1]:5060".parse().unwrap()
    ));
}

#[test]
fn n06_stamp_outbound_replaces_transport_and_sent_by_keeps_params() {
    let mut r = request_with_via("SIP/2.0/UDP placeholder;rport;branch=z9hG4bKxyz");
    r.headers
        .push("Via", "SIP/2.0/UDP lower.example:5060;branch=z9hG4bKlower");
    stamp_outbound_via(&mut r, TransportKind::Tcp, "203.0.113.5", 5070);
    let vias: Vec<_> = r.headers.get_all("Via").collect();
    assert_eq!(
        vias[0],
        "SIP/2.0/TCP 203.0.113.5:5070;rport;branch=z9hG4bKxyz"
    );
    assert_eq!(
        vias[1],
        "SIP/2.0/UDP lower.example:5060;branch=z9hG4bKlower"
    );
    // idempotent
    let before = r.clone();
    stamp_outbound_via(&mut r, TransportKind::Tcp, "203.0.113.5", 5070);
    assert_eq!(r, before);
    // no Via at all: one is created
    let mut bare = Request::new(Method::Options, SipUri::new("example.com"));
    stamp_outbound_via(&mut bare, TransportKind::Udp, "2001:db8::1", 5060);
    assert_eq!(top(&bare), "SIP/2.0/UDP [2001:db8::1]:5060");
}

#[test]
fn n01_n02_response_target_udp_rules() {
    let via = |s: &str| Via::parse(s).unwrap();
    let t = |s: &str| response_target(&via(s), TransportKind::Udp);
    // rport + received -> received:rport
    assert_eq!(
        t("SIP/2.0/UDP 10.0.0.9:5060;received=192.0.2.1;rport=40000"),
        Target {
            host: "192.0.2.1".into(),
            port: 40000
        }
    );
    // rport without received -> sent-by host : rport
    assert_eq!(
        t("SIP/2.0/UDP 10.0.0.9:5060;rport=40000"),
        Target {
            host: "10.0.0.9".into(),
            port: 40000
        }
    );
    // rport present but empty (not filled in) is ignored
    assert_eq!(
        t("SIP/2.0/UDP 10.0.0.9:5099;rport;received=192.0.2.1"),
        Target {
            host: "192.0.2.1".into(),
            port: 5099
        }
    );
    // received without rport -> received : sent-by port
    assert_eq!(
        t("SIP/2.0/UDP pbx.example.com:5099;received=192.0.2.1"),
        Target {
            host: "192.0.2.1".into(),
            port: 5099
        }
    );
    // neither -> sent-by, default port 5060
    assert_eq!(
        t("SIP/2.0/UDP pbx.example.com"),
        Target {
            host: "pbx.example.com".into(),
            port: 5060
        }
    );
    // IPv6
    let v6 = t("SIP/2.0/UDP [2001:db8::7]:5070;rport=4000");
    assert_eq!(
        v6.socket_addr(),
        Some("[2001:db8::7]:4000".parse().unwrap())
    );
    assert_eq!(
        Target {
            host: "pbx.example.com".into(),
            port: 1
        }
        .socket_addr(),
        None
    );
}

#[test]
fn n03_response_target_reliable_ignores_rport() {
    let via = Via::parse("SIP/2.0/TCP 10.0.0.9:5090;received=192.0.2.1;rport=40000").unwrap();
    assert_eq!(
        response_target(&via, TransportKind::Tcp),
        Target {
            host: "192.0.2.1".into(),
            port: 5090
        }
    );
}
