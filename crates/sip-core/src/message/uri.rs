//! SIP / SIPS / tel URIs (RFC 3261 section 19.1, RFC 3966 subset) and opaque other-scheme URIs.

use std::fmt;
use std::net::Ipv6Addr;

use super::lex::Param;
use super::parse::ParseError;

/// Scheme of a [`SipUri`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UriScheme {
    /// `sip:`
    Sip,
    /// `sips:`
    Sips,
    /// `tel:` (the number is stored in [`SipUri::user`], [`SipUri::host`] is empty).
    Tel,
}

impl UriScheme {
    /// Lower-case scheme text.
    pub fn as_str(self) -> &'static str {
        match self {
            UriScheme::Sip => "sip",
            UriScheme::Sips => "sips",
            UriScheme::Tel => "tel",
        }
    }
}

/// A `sip:` / `sips:` / `tel:` URI. Components are kept exactly as written (still
/// percent-escaped); use [`unescape`] / [`escape_user`] to convert.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SipUri {
    /// URI scheme.
    pub scheme: UriScheme,
    /// User part (escaped form). For `tel:` the subscriber number.
    pub user: Option<String>,
    /// Password (deprecated, escaped form).
    pub password: Option<String>,
    /// Host name or address; IPv6 literals keep their brackets. Empty for `tel:`.
    pub host: String,
    /// Port, if given.
    pub port: Option<u16>,
    /// URI parameters (`;lr`, `;transport=tcp`, ...).
    pub params: Vec<Param>,
    /// URI headers (`?name=value&...`), still escaped.
    pub headers: Vec<(String, String)>,
}

fn bad(reason: &str) -> ParseError {
    ParseError::InvalidUri(reason.to_string())
}

fn has_forbidden(s: &str) -> bool {
    s.chars()
        .any(|c| c.is_ascii_whitespace() || c.is_ascii_control() || matches!(c, '<' | '>' | '"'))
}

fn valid_scheme(s: &str) -> bool {
    let mut chars = s.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '-' | '.'))
}

impl SipUri {
    /// Builds `sip:host` with no user, port or parameters.
    pub fn new(host: impl Into<String>) -> Self {
        SipUri {
            scheme: UriScheme::Sip,
            user: None,
            password: None,
            host: host.into(),
            port: None,
            params: Vec::new(),
            headers: Vec::new(),
        }
    }

    /// Parses a `sip:`, `sips:` or `tel:` URI. Any other scheme is rejected (see
    /// [`Uri::parse`] for the tolerant variant used in message start lines).
    pub fn parse(s: &str) -> Result<Self, ParseError> {
        if has_forbidden(s) {
            return Err(bad(
                "whitespace, control or angle-bracket characters in URI",
            ));
        }
        let Some((scheme, rest)) = s.split_once(':') else {
            return Err(bad("missing scheme"));
        };
        let scheme = if scheme.eq_ignore_ascii_case("sip") {
            UriScheme::Sip
        } else if scheme.eq_ignore_ascii_case("sips") {
            UriScheme::Sips
        } else if scheme.eq_ignore_ascii_case("tel") {
            UriScheme::Tel
        } else {
            return Err(bad("unsupported URI scheme"));
        };
        if scheme == UriScheme::Tel {
            let (number, params) = match rest.split_once(';') {
                Some((n, p)) => (n, parse_uri_params(p)?),
                None => (rest, Vec::new()),
            };
            if number.is_empty() {
                return Err(bad("empty tel number"));
            }
            return Ok(SipUri {
                scheme,
                user: Some(number.to_string()),
                password: None,
                host: String::new(),
                port: None,
                params,
                headers: Vec::new(),
            });
        }

        // The user part may contain ';' '?' etc., but never '@', so the first '@' ends it.
        let (userinfo, remainder) = match rest.find('@') {
            Some(at) => (Some(&rest[..at]), &rest[at + 1..]),
            None => (None, rest),
        };
        let (user, password) = match userinfo {
            None => (None, None),
            Some(ui) => {
                let (u, p) = match ui.split_once(':') {
                    Some((u, p)) => (u, Some(p)),
                    None => (ui, None),
                };
                if u.is_empty() {
                    return Err(bad("empty user part"));
                }
                (Some(u.to_string()), p.map(str::to_string))
            }
        };
        let (before_headers, headers_str) = match remainder.split_once('?') {
            Some((b, h)) => (b, Some(h)),
            None => (remainder, None),
        };
        let (hostport, params_str) = match before_headers.split_once(';') {
            Some((h, p)) => (h, Some(p)),
            None => (before_headers, None),
        };
        let (host, port) = parse_hostport(hostport)?;
        let params = match params_str {
            Some(p) => parse_uri_params(p)?,
            None => Vec::new(),
        };
        let mut headers = Vec::new();
        if let Some(h) = headers_str {
            for pair in h.split('&') {
                let Some((n, v)) = pair.split_once('=') else {
                    return Err(bad("URI header without '='"));
                };
                if n.is_empty() {
                    return Err(bad("empty URI header name"));
                }
                headers.push((n.to_string(), v.to_string()));
            }
        }
        Ok(SipUri {
            scheme,
            user,
            password,
            host,
            port,
            params,
            headers,
        })
    }

    /// Looks up a URI parameter by case-insensitive name (outer `Option`: present?).
    pub fn param(&self, name: &str) -> Option<Option<&str>> {
        self.params
            .iter()
            .find(|p| p.name.eq_ignore_ascii_case(name))
            .map(|p| p.value.as_deref())
    }

    /// `true` when the URI carries the `lr` (loose routing) parameter.
    pub fn is_loose_router(&self) -> bool {
        self.param("lr").is_some()
    }
}

fn parse_uri_params(s: &str) -> Result<Vec<Param>, ParseError> {
    let mut out = Vec::new();
    for seg in s.split(';') {
        if seg.is_empty() {
            return Err(bad("empty URI parameter"));
        }
        match seg.split_once('=') {
            Some((n, v)) => {
                if n.is_empty() || v.is_empty() {
                    return Err(bad("empty URI parameter name or value"));
                }
                out.push(Param {
                    name: n.to_string(),
                    value: Some(v.to_string()),
                });
            }
            None => out.push(Param {
                name: seg.to_string(),
                value: None,
            }),
        }
    }
    Ok(out)
}

fn parse_hostport(s: &str) -> Result<(String, Option<u16>), ParseError> {
    if s.is_empty() {
        return Err(bad("empty host"));
    }
    let (host, port_str) = if let Some(rest) = s.strip_prefix('[') {
        let Some(end) = rest.find(']') else {
            return Err(bad("unterminated IPv6 literal"));
        };
        if rest[..end].parse::<Ipv6Addr>().is_err() {
            return Err(bad("invalid IPv6 literal"));
        }
        let after = &rest[end + 1..];
        let port = match after {
            "" => None,
            _ => Some(
                after
                    .strip_prefix(':')
                    .ok_or_else(|| bad("garbage after IPv6 literal"))?,
            ),
        };
        (s[..end + 2].to_string(), port)
    } else {
        match s.split_once(':') {
            Some((h, p)) => (h.to_string(), Some(p)),
            None => (s.to_string(), None),
        }
    };
    if !s.starts_with('[')
        && (host.is_empty()
            || !host
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_')))
    {
        return Err(bad("invalid host"));
    }
    let port = match port_str {
        None => None,
        Some(p) => {
            if p.is_empty() || p.len() > 5 || !p.bytes().all(|b| b.is_ascii_digit()) {
                return Err(bad("invalid port"));
            }
            Some(p.parse::<u16>().map_err(|_| bad("port out of range"))?)
        }
    };
    Ok((host, port))
}

impl fmt::Display for SipUri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}:", self.scheme.as_str())?;
        if self.scheme == UriScheme::Tel {
            if let Some(u) = &self.user {
                f.write_str(u)?;
            }
        } else {
            if let Some(u) = &self.user {
                f.write_str(u)?;
                if let Some(p) = &self.password {
                    write!(f, ":{p}")?;
                }
                f.write_str("@")?;
            }
            f.write_str(&self.host)?;
            if let Some(p) = self.port {
                write!(f, ":{p}")?;
            }
        }
        for p in &self.params {
            write!(f, ";{p}")?;
        }
        for (i, (n, v)) in self.headers.iter().enumerate() {
            f.write_str(if i == 0 { "?" } else { "&" })?;
            write!(f, "{n}={v}")?;
        }
        Ok(())
    }
}

/// Any URI that can appear in a request line or a name-addr: a [`SipUri`] or an opaque URI with
/// another scheme (RFC 4475 3.3.2 / 3.3.4 require such messages to parse).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Uri {
    /// `sip:` / `sips:` / `tel:`
    Sip(SipUri),
    /// Any other scheme, e.g. `http://...`, kept verbatim.
    Other {
        /// Scheme as written.
        scheme: String,
        /// Everything after the first colon.
        rest: String,
    },
}

impl Uri {
    /// Parses a URI; known schemes are fully parsed, other well-formed schemes are kept opaque.
    pub fn parse(s: &str) -> Result<Self, ParseError> {
        match s.split_once(':') {
            Some((scheme, rest)) if valid_scheme(scheme) => {
                let l = scheme.to_ascii_lowercase();
                if matches!(l.as_str(), "sip" | "sips" | "tel") {
                    SipUri::parse(s).map(Uri::Sip)
                } else if rest.is_empty() || has_forbidden(rest) {
                    Err(bad("malformed opaque URI"))
                } else {
                    Ok(Uri::Other {
                        scheme: scheme.to_string(),
                        rest: rest.to_string(),
                    })
                }
            }
            _ => Err(bad("missing or invalid scheme")),
        }
    }

    /// The SIP URI, if this is one.
    pub fn as_sip(&self) -> Option<&SipUri> {
        match self {
            Uri::Sip(u) => Some(u),
            Uri::Other { .. } => None,
        }
    }
}

impl fmt::Display for Uri {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Uri::Sip(u) => u.fmt(f),
            Uri::Other { scheme, rest } => write!(f, "{scheme}:{rest}"),
        }
    }
}

impl From<SipUri> for Uri {
    fn from(u: SipUri) -> Self {
        Uri::Sip(u)
    }
}

/// Resolves `%XX` escapes. Invalid escapes (`%` not followed by two hex digits) are kept
/// literally, as RFC 4475 3.1.1.5 requires.
pub fn unescape(s: &str) -> Vec<u8> {
    let b = s.as_bytes();
    let mut out = Vec::with_capacity(b.len());
    let mut i = 0;
    while i < b.len() {
        if b[i] == b'%'
            && i + 2 < b.len()
            && let (Some(h), Some(l)) = (hex(b[i + 1]), hex(b[i + 2]))
        {
            out.push(h << 4 | l);
            i += 3;
            continue;
        }
        out.push(b[i]);
        i += 1;
    }
    out
}

fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Escapes bytes that are not allowed unescaped in a SIP URI user part.
pub fn escape_user(s: &[u8]) -> String {
    let mut out = String::with_capacity(s.len());
    for &b in s {
        let ok = b.is_ascii_alphanumeric()
            || matches!(
                b,
                b'-' | b'_'
                    | b'.'
                    | b'!'
                    | b'~'
                    | b'*'
                    | b'\''
                    | b'('
                    | b')'
                    | b'&'
                    | b'='
                    | b'+'
                    | b'$'
                    | b','
                    | b';'
                    | b'?'
                    | b'/'
            );
        if ok {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}
