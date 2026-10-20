//! `Via` header (RFC 3261 section 20.42, RFC 3581 `rport`).

use std::fmt;

use super::lex::{Param, find_param, is_token, parse_params, write_params};
use super::parse::ParseError;

/// One Via header field value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Via {
    /// Transport token as written, e.g. `UDP`, `TCP`, `TLS`, `SCTP` or an unknown one.
    pub transport: String,
    /// sent-by host (IPv6 literals keep brackets).
    pub host: String,
    /// sent-by port.
    pub port: Option<u16>,
    /// Via parameters: `branch`, `received`, `rport`, `maddr`, `ttl`, ...
    pub params: Vec<Param>,
}

fn bad(reason: &str) -> ParseError {
    ParseError::invalid_header("Via", reason)
}

impl Via {
    /// Creates a Via with the given transport and sent-by and no parameters.
    pub fn new(transport: impl Into<String>, host: impl Into<String>, port: Option<u16>) -> Self {
        Via {
            transport: transport.into(),
            host: host.into(),
            port,
            params: Vec::new(),
        }
    }

    /// Parses one Via element (whitespace around `/`, `;` and `=` is tolerated).
    pub fn parse(s: &str) -> Result<Self, ParseError> {
        let (head, params_text) = match s.find(';') {
            Some(i) => (&s[..i], &s[i..]),
            None => (s, ""),
        };
        let mut parts = head.splitn(3, '/');
        let (Some(proto), Some(version), Some(third)) = (parts.next(), parts.next(), parts.next())
        else {
            return Err(bad("expected SIP/2.0/<transport>"));
        };
        if !proto.trim().eq_ignore_ascii_case("SIP") {
            return Err(bad("protocol name is not SIP"));
        }
        if version.trim() != "2.0" {
            return Err(ParseError::UnsupportedVersion);
        }
        let third = third.trim();
        let (transport, sent_by) = match third.find(|c: char| c.is_ascii_whitespace()) {
            Some(i) => (&third[..i], third[i..].trim()),
            None => return Err(bad("missing sent-by")),
        };
        if !is_token(transport) {
            return Err(bad("invalid transport token"));
        }
        let (host, port) = parse_sent_by(sent_by)?;
        Ok(Via {
            transport: transport.to_string(),
            host,
            port,
            params: parse_params(params_text, "Via")?,
        })
    }

    fn value(&self, name: &str) -> Option<&str> {
        find_param(&self.params, name).and_then(|p| p.value.as_deref())
    }

    /// The `branch` parameter.
    pub fn branch(&self) -> Option<&str> {
        self.value("branch")
    }

    /// The `received` parameter.
    pub fn received(&self) -> Option<&str> {
        self.value("received")
    }

    /// The `maddr` parameter.
    pub fn maddr(&self) -> Option<&str> {
        self.value("maddr")
    }

    /// The `ttl` parameter parsed as a number.
    pub fn ttl(&self) -> Option<u8> {
        self.value("ttl").and_then(|v| v.parse().ok())
    }

    /// The `rport` parameter: `None` = absent, `Some(None)` = present without a value (the
    /// request is asking for it to be filled in), `Some(Some(p))` = filled in.
    pub fn rport(&self) -> Option<Option<u16>> {
        find_param(&self.params, "rport").map(|p| p.value.as_deref().and_then(|v| v.parse().ok()))
    }

    /// Removes a parameter (case-insensitive); returns `true` when it was present.
    pub fn remove_param(&mut self, name: &str) -> bool {
        let before = self.params.len();
        self.params.retain(|p| !p.name.eq_ignore_ascii_case(name));
        before != self.params.len()
    }

    /// Sets (replacing) a parameter.
    pub fn set_param(&mut self, name: &str, value: Option<String>) {
        match self
            .params
            .iter_mut()
            .find(|p| p.name.eq_ignore_ascii_case(name))
        {
            Some(p) => p.value = value,
            None => self.params.push(Param {
                name: name.to_string(),
                value,
            }),
        }
    }
}

fn parse_sent_by(s: &str) -> Result<(String, Option<u16>), ParseError> {
    if s.is_empty() {
        return Err(bad("empty sent-by"));
    }
    let (host, port_text) = if let Some(rest) = s.strip_prefix('[') {
        let Some(end) = rest.find(']') else {
            return Err(bad("unterminated IPv6 literal"));
        };
        if rest[..end].parse::<std::net::Ipv6Addr>().is_err() {
            return Err(bad("invalid IPv6 literal"));
        }
        let after = &rest[end + 1..];
        let port = if after.is_empty() {
            None
        } else {
            Some(
                after
                    .strip_prefix(':')
                    .ok_or_else(|| bad("garbage after IPv6 literal"))?,
            )
        };
        (s[..end + 2].to_string(), port)
    } else {
        let (h, p) = match s.split_once(':') {
            Some((h, p)) => (h, Some(p)),
            None => (s, None),
        };
        if h.is_empty()
            || !h
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'))
        {
            return Err(bad("invalid sent-by host"));
        }
        (h.to_string(), p)
    };
    let port = match port_text {
        None => None,
        Some(p) => {
            let p = p.trim();
            if p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) {
                return Err(bad("invalid sent-by port"));
            }
            Some(
                p.parse::<u16>()
                    .map_err(|_| bad("sent-by port out of range"))?,
            )
        }
    };
    Ok((host, port))
}

impl fmt::Display for Via {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SIP/2.0/{} {}", self.transport, self.host)?;
        if let Some(p) = self.port {
            write!(f, ":{p}")?;
        }
        write_params(f, &self.params)
    }
}
