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
mod tests;
