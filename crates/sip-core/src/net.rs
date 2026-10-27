// Derived from Sipress (MIT, Copyright (c) 2025 LeoYu) core/src/net.rs, unmodified apart from this comment (see docs/sipress-extraction.md).
//! 位址字串處理（IPv4 / IPv6 共用）
//!
//! SIP / SDP 會用到三種形式：`ip:port`（IPv6 為 `[ip]:port`）、單獨的 IP、或 `host:port`。
//! 不可以用 `split(':')` 取 IP：IPv6 位址本身就含冒號。

use std::net::{IpAddr, SocketAddr};

/// 取出位址中的主機部分（IPv6 不含中括號）：
/// `10.0.0.1:5060` → `10.0.0.1`、`[2001:db8::1]:5060` → `2001:db8::1`、
/// `2001:db8::1` → `2001:db8::1`、`pbx.example:5060` → `pbx.example`
pub fn host(addr: &str) -> String {
    let addr = addr.trim();
    if let Ok(sa) = addr.parse::<SocketAddr>() {
        return sa.ip().to_string();
    }
    if let Ok(ip) = addr.parse::<IpAddr>() {
        return ip.to_string();
    }
    if let Some(rest) = addr.strip_prefix('[') {
        return rest.split(']').next().unwrap_or(rest).to_string();
    }
    match addr.rsplit_once(':') {
        Some((h, port)) if port.chars().all(|c| c.is_ascii_digit()) => h.to_string(),
        _ => addr.to_string(),
    }
}

/// SIP URI 用的主機（IPv6 加中括號）：`[2001:db8::1]:5060` → `[2001:db8::1]`、`10.0.0.1:5060` → `10.0.0.1`
pub fn uri_host(addr: &str) -> String {
    let h = host(addr);
    if is_ipv6(&h) { format!("[{}]", h) } else { h }
}

/// 組成 `ip:port`（IPv6 加中括號）
pub fn join(ip: &str, port: u16) -> String {
    let ip = ip.trim_start_matches('[').trim_end_matches(']');
    if ip.contains(':') {
        format!("[{}]:{}", ip, port)
    } else {
        format!("{}:{}", ip, port)
    }
}

/// 是否為 IPv6 位址（可含中括號）
pub fn is_ipv6(ip: &str) -> bool {
    ip.trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<std::net::Ipv6Addr>()
        .is_ok()
}

/// 連往 `target` 時使用的萬用綁定位址（系統分配 port）
pub fn unspecified_for(target: &SocketAddr) -> &'static str {
    if target.is_ipv6() {
        "[::]:0"
    } else {
        "0.0.0.0:0"
    }
}

/// The local address the system would use to reach `peer` (a connected UDP socket, nothing is
/// sent); `None` when there is no route. Added for sip-fax-rs (not from Sipress): the address to
/// advertise when the socket is bound to a wildcard address.
pub fn local_ip_towards(peer: SocketAddr) -> Option<IpAddr> {
    let s = std::net::UdpSocket::bind(unspecified_for(&peer)).ok()?;
    s.connect(peer).ok()?;
    let ip = s.local_addr().ok()?.ip();
    (!ip.is_unspecified()).then_some(ip)
}

/// `host` as it may be written into a message for `peer`: a wildcard IP (`0.0.0.0`, `::`)
/// becomes the local address towards `peer`; anything else, or no route, stays as it is.
pub fn concrete_host(host: &str, peer: SocketAddr) -> String {
    match host
        .trim_start_matches('[')
        .trim_end_matches(']')
        .parse::<IpAddr>()
    {
        Ok(ip) if ip.is_unspecified() => {
            local_ip_towards(peer).map_or_else(|| host.to_string(), |l| l.to_string())
        }
        _ => host.to_string(),
    }
}

#[cfg(test)]
mod tests;
