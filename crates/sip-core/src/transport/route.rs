//! Pure message-stamping and response-routing rules (RFC 3261 18.1.1, 18.2.1, 18.2.2 and
//! RFC 3581). No I/O, so every rule is unit-testable.

use std::net::{IpAddr, SocketAddr};

use super::TransportKind;
use super::policy::normalize_ip;
use crate::message::{Request, Via};
use crate::net;

/// Default SIP port.
pub const DEFAULT_PORT: u16 = 5060;

/// Where to send a response: a host (IP literal without brackets, or a name that needs DNS)
/// and a port.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    /// IP literal (no brackets) or host name.
    pub host: String,
    /// Port.
    pub port: u16,
}

impl Target {
    /// The target as a socket address when `host` is an IP literal.
    pub fn socket_addr(&self) -> Option<SocketAddr> {
        self.host
            .parse::<IpAddr>()
            .ok()
            .map(|ip| SocketAddr::new(ip, self.port))
    }
}

fn unbracket(host: &str) -> &str {
    host.trim_start_matches('[').trim_end_matches(']')
}

/// Server-side stamping of the topmost Via of a received request (RFC 3261 18.2.1, RFC 3581):
///
/// * `received=<source ip>` is set when the sent-by host differs from the source address (or
///   is a host name), or when the Via carries `rport`;
/// * `rport` (present, with or without a value) is set to the source port.
///
/// A `received` parameter that arrived on the wire is discarded first: the topmost Via of a
/// request is written by the previous hop and must not be able to steer our responses.
/// Returns `true` when the Via was modified.
pub fn stamp_inbound_via(req: &mut Request, source: SocketAddr) -> bool {
    let Ok(mut via) = req.headers.top_via() else {
        return false;
    };
    let before = via.clone();
    via.remove_param("received");
    let src_ip = normalize_ip(source.ip());
    let sent_by_ip = unbracket(&via.host)
        .parse::<IpAddr>()
        .ok()
        .map(normalize_ip);
    let rport_requested = via.rport().is_some();
    if sent_by_ip != Some(src_ip) || rport_requested {
        via.set_param("received", Some(src_ip.to_string()));
    }
    if rport_requested {
        via.set_param("rport", Some(source.port().to_string()));
    }
    if via == before {
        return false;
    }
    req.headers.set_first("Via", via.to_string())
}

/// Client-side stamping of the topmost Via of an outgoing request (RFC 3261 18.1.1): sets the
/// transport token and the sent-by to the configured advertised address, keeping `branch` and
/// every other parameter. A request without any Via gets a new one (without branch).
pub fn stamp_outbound_via(req: &mut Request, kind: TransportKind, host: &str, port: u16) {
    let sent_by_host = net::uri_host(host);
    match req.headers.top_via() {
        Ok(mut via) => {
            via.transport = kind.as_str().to_string();
            via.host = sent_by_host;
            via.port = Some(port);
            req.headers.set_first("Via", via.to_string());
        }
        Err(_) => {
            let via = Via::new(kind.as_str(), sent_by_host, Some(port));
            req.headers.push_front("Via", via.to_string());
        }
    }
}

/// Destination of a response, derived from the topmost Via of the response
/// (RFC 3261 18.2.2 + RFC 3581 section 4).
///
/// * Unreliable (UDP): `rport` with value -> `received` (or sent-by host) + rport;
///   else `received` + sent-by port; else sent-by.
/// * Reliable (used only when the original connection is gone): `received` (or sent-by host)
///   + sent-by port; `rport` names the client's ephemeral source port and is not used.
pub fn response_target(via: &Via, kind: TransportKind) -> Target {
    let sent_by_host = unbracket(&via.host);
    let host = unbracket(via.received().unwrap_or(sent_by_host)).to_string();
    let sent_by_port = via.port.unwrap_or(DEFAULT_PORT);
    let port = match (kind, via.rport()) {
        (TransportKind::Udp, Some(Some(p))) => p,
        _ => sent_by_port,
    };
    Target { host, port }
}

#[cfg(test)]
mod tests {
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
}
