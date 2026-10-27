//! Inbound source access policy (docs/design/sip-core.md section 9, "source filtering").
//!
//! Pure, I/O-free: CIDR matching ([`Cidr`]) and the decision for one source address
//! ([`SourcePolicy::decide`]). The transport applies the decision where inbound requests
//! enter the stack.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

/// An IP network in CIDR notation (a bare address is a host route: /32 or /128).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Cidr {
    addr: IpAddr,
    prefix: u8,
}

/// A CIDR string could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid CIDR {0:?}")]
pub struct CidrParseError(pub String);

/// Maps IPv4-mapped IPv6 addresses (`::ffff:a.b.c.d`) to plain IPv4 so that dual-stack sockets
/// match IPv4 rules.
pub fn normalize_ip(ip: IpAddr) -> IpAddr {
    match ip {
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => IpAddr::V4(v4),
            None => IpAddr::V6(v6),
        },
        v4 => v4,
    }
}

impl Cidr {
    /// Parses `a.b.c.d`, `a.b.c.d/len`, `v6addr` or `v6addr/len`. Host bits below the prefix
    /// are ignored when matching.
    pub fn parse(s: &str) -> Result<Self, CidrParseError> {
        let err = || CidrParseError(s.to_string());
        let s = s.trim();
        let (ip_text, len_text) = match s.split_once('/') {
            Some((a, l)) => (a, Some(l)),
            None => (s, None),
        };
        let parsed = ip_text.parse::<IpAddr>().map_err(|_| err())?;
        let addr = normalize_ip(parsed);
        let raw_max = if parsed.is_ipv4() { 32 } else { 128 };
        let raw = match len_text {
            None => raw_max,
            Some(l) => {
                if l.is_empty() || !l.bytes().all(|b| b.is_ascii_digit()) {
                    return Err(err());
                }
                l.parse::<u8>().map_err(|_| err())?
            }
        };
        if raw > raw_max {
            return Err(err());
        }
        // A mapped IPv6 network "::ffff:1.2.3.0/120" becomes 1.2.3.0/24.
        let prefix = if parsed.is_ipv6() && addr.is_ipv4() {
            raw.checked_sub(96).ok_or_else(err)?
        } else {
            raw
        };
        Ok(Cidr { addr, prefix })
    }

    /// `true` when `ip` lies inside the network (address families must match; IPv4-mapped
    /// IPv6 addresses count as IPv4).
    pub fn contains(&self, ip: IpAddr) -> bool {
        match (self.addr, normalize_ip(ip)) {
            (IpAddr::V4(net), IpAddr::V4(ip)) => {
                prefix_eq(&net.octets(), &ip.octets(), self.prefix)
            }
            (IpAddr::V6(net), IpAddr::V6(ip)) => {
                prefix_eq(&net.octets(), &ip.octets(), self.prefix)
            }
            _ => false,
        }
    }
}

fn prefix_eq(a: &[u8], b: &[u8], prefix: u8) -> bool {
    let full = usize::from(prefix / 8);
    let rem = prefix % 8;
    if a[..full] != b[..full] {
        return false;
    }
    if rem == 0 {
        return true;
    }
    let mask = 0xffu8 << (8 - rem);
    (a[full] & mask) == (b[full] & mask)
}

impl fmt::Display for Cidr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.addr, self.prefix)
    }
}

impl From<Ipv4Addr> for Cidr {
    fn from(a: Ipv4Addr) -> Self {
        Cidr {
            addr: IpAddr::V4(a),
            prefix: 32,
        }
    }
}

impl From<Ipv6Addr> for Cidr {
    fn from(a: Ipv6Addr) -> Self {
        Cidr {
            addr: IpAddr::V6(a),
            prefix: 128,
        }
    }
}

/// What to do with a request whose source is not on the allow-list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum PolicyMode {
    /// Silently discard (default; reveals nothing to scanners).
    #[default]
    Drop,
    /// Answer `403 Forbidden` and discard.
    Reject,
    /// Process normally but log a warning.
    LogOnly,
}

/// Outcome of [`SourcePolicy::decide`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Source is allowed (or filtering is off): process.
    Allow,
    /// Source is not allowed but the mode is log-only: process and log a warning.
    AllowAndWarn,
    /// Discard without any reply.
    Drop,
    /// Discard and answer 403.
    Reject403,
}

impl Decision {
    /// `true` when the request is processed (allowed, or log-only mode).
    pub fn admits(self) -> bool {
        matches!(self, Decision::Allow | Decision::AllowAndWarn)
    }
}

/// Allow-list of source networks plus the action for everything else.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourcePolicy {
    allow: Option<Vec<Cidr>>,
    mode: PolicyMode,
}

impl Default for SourcePolicy {
    fn default() -> Self {
        Self::allow_all()
    }
}

/// A [`SourcePolicy`] that can be replaced while the transport runs (a configuration change
/// that touches the allowed networks). Clones share the same policy; readers take a snapshot
/// per datagram, so a replacement never affects a request that is already being handled.
#[derive(Clone, Debug)]
pub struct PolicyCell(std::sync::Arc<std::sync::RwLock<std::sync::Arc<SourcePolicy>>>);

impl PolicyCell {
    /// A cell holding `policy`.
    pub fn new(policy: SourcePolicy) -> Self {
        PolicyCell(std::sync::Arc::new(std::sync::RwLock::new(
            std::sync::Arc::new(policy),
        )))
    }

    /// The policy in force right now.
    pub fn get(&self) -> std::sync::Arc<SourcePolicy> {
        self.0.read().unwrap_or_else(|e| e.into_inner()).clone()
    }

    /// Replaces the policy for every request that arrives from now on.
    pub fn set(&self, policy: SourcePolicy) {
        *self.0.write().unwrap_or_else(|e| e.into_inner()) = std::sync::Arc::new(policy);
    }
}

impl SourcePolicy {
    /// No filtering: every source is allowed.
    pub fn allow_all() -> Self {
        SourcePolicy {
            allow: None,
            mode: PolicyMode::Drop,
        }
    }

    /// Only sources inside `allow` are accepted; others are handled according to `mode`. An
    /// empty list therefore allows nobody.
    pub fn new(allow: Vec<Cidr>, mode: PolicyMode) -> Self {
        SourcePolicy {
            allow: Some(allow),
            mode,
        }
    }

    /// Convenience: parses every entry with [`Cidr::parse`].
    pub fn parse<S: AsRef<str>>(allow: &[S], mode: PolicyMode) -> Result<Self, CidrParseError> {
        let list = allow
            .iter()
            .map(|s| Cidr::parse(s.as_ref()))
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self::new(list, mode))
    }

    /// The configured mode.
    pub fn mode(&self) -> PolicyMode {
        self.mode
    }

    /// Decides what to do with a request that came from `source`.
    pub fn decide(&self, source: IpAddr) -> Decision {
        let Some(list) = &self.allow else {
            return Decision::Allow;
        };
        if list.iter().any(|c| c.contains(source)) {
            return Decision::Allow;
        }
        match self.mode {
            PolicyMode::Drop => Decision::Drop,
            PolicyMode::Reject => Decision::Reject403,
            PolicyMode::LogOnly => Decision::AllowAndWarn,
        }
    }
}

#[cfg(test)]
mod tests;
