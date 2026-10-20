//! SDP (RFC 4566) model, parser and serializer, with T.38 ([`t38`]) and offer/answer
//! ([`negotiate`]) support.
//!
//! The model covers `v= o= s= c= t= m= a=`; every other line (`i= u= e= p= b= r= z= k=` ...) is
//! kept verbatim in [`SessionDescription::extra`] / [`MediaDescription::extra`]. Serialization
//! always emits the RFC 4566 line order, so well-ordered SDP round-trips byte for byte.

pub mod negotiate;
pub mod t38;

use std::fmt::{self, Write as _};

pub use negotiate::{
    AnswerPolicy, AudioCodec, MediaPreference, NegotiateError, OriginTracker, answer_audio,
    answer_offer, answer_t38, reject_media,
};
pub use t38::{RateManagement, T38Capabilities, T38Params, UdpEc};

/// Maximum accepted SDP size in bytes.
pub const MAX_SDP_SIZE: usize = 65_536;
/// Maximum number of `m=` sections.
pub const MAX_MEDIA: usize = 64;

/// SDP syntax error.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SdpError {
    /// No lines.
    #[error("empty SDP")]
    Empty,
    /// Larger than [`MAX_SDP_SIZE`].
    #[error("SDP larger than {MAX_SDP_SIZE} bytes")]
    TooLarge,
    /// More than [`MAX_MEDIA`] media sections.
    #[error("more than {MAX_MEDIA} media sections")]
    TooManyMedia,
    /// First line is not `v=`.
    #[error("SDP must start with v=")]
    MissingVersion,
    /// No `o=` line.
    #[error("SDP has no o= line")]
    MissingOrigin,
    /// A line is not of the form `<letter>=<value>`.
    #[error("malformed SDP line {line}")]
    MalformedLine {
        /// 1-based line number.
        line: usize,
    },
    /// A field has an invalid value.
    #[error("invalid {field}: {reason}")]
    Invalid {
        /// Which line/attribute.
        field: &'static str,
        /// What is wrong.
        reason: String,
    },
}

fn invalid(field: &'static str, reason: impl Into<String>) -> SdpError {
    SdpError::Invalid {
        field,
        reason: reason.into(),
    }
}

/// `o=` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Origin {
    /// User name (`-` when unused).
    pub username: String,
    /// Session id.
    pub sess_id: u64,
    /// Session version; incremented when the description changes.
    pub sess_version: u64,
    /// Network type, normally `IN`.
    pub nettype: String,
    /// Address type, `IP4` or `IP6`.
    pub addrtype: String,
    /// Unicast address.
    pub address: String,
}

/// `c=` line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    /// Network type, normally `IN`.
    pub nettype: String,
    /// `IP4` or `IP6`.
    pub addrtype: String,
    /// Address (may carry `/ttl` multicast suffixes, kept as written).
    pub address: String,
}

impl Connection {
    /// `IN IP4|IP6 <address>` for a textual IP address.
    pub fn for_address(address: &str) -> Self {
        Connection {
            nettype: "IN".into(),
            addrtype: if address.contains(':') { "IP6" } else { "IP4" }.into(),
            address: address.into(),
        }
    }
}

/// `t=` line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Timing {
    /// Start time (NTP seconds, 0 = unbounded).
    pub start: u64,
    /// Stop time (0 = unbounded).
    pub stop: u64,
}

/// `a=` line: `a=name` or `a=name:value`. The name's case is preserved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Attribute {
    /// Attribute name.
    pub name: String,
    /// Value after the first colon.
    pub value: Option<String>,
}

impl Attribute {
    /// `a=name:value`
    pub fn value(name: impl Into<String>, value: impl Into<String>) -> Self {
        Attribute {
            name: name.into(),
            value: Some(value.into()),
        }
    }

    /// `a=name`
    pub fn flag(name: impl Into<String>) -> Self {
        Attribute {
            name: name.into(),
            value: None,
        }
    }
}

/// An SDP line that the model does not interpret, kept verbatim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawLine {
    /// Line type letter (`i`, `b`, `k`, ...).
    pub kind: char,
    /// Everything after `=`.
    pub value: String,
}

/// Media direction attribute.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Direction {
    /// `a=sendrecv` (also the default when absent)
    SendRecv,
    /// `a=sendonly`
    SendOnly,
    /// `a=recvonly`
    RecvOnly,
    /// `a=inactive`
    Inactive,
}

impl Direction {
    /// The direction the answerer must use.
    pub fn answered(self) -> Direction {
        match self {
            Direction::SendOnly => Direction::RecvOnly,
            Direction::RecvOnly => Direction::SendOnly,
            other => other,
        }
    }

    /// Attribute name.
    pub fn as_str(self) -> &'static str {
        match self {
            Direction::SendRecv => "sendrecv",
            Direction::SendOnly => "sendonly",
            Direction::RecvOnly => "recvonly",
            Direction::Inactive => "inactive",
        }
    }
}

/// One `a=rtpmap:` entry.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtpMap {
    /// Payload type.
    pub payload_type: u8,
    /// Encoding name, e.g. `PCMU`.
    pub encoding: String,
    /// Clock rate.
    pub clock_rate: u32,
    /// Encoding parameters (channels).
    pub params: Option<String>,
}

/// An `m=` section.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MediaDescription {
    /// Media type: `audio`, `image`, `video`, ...
    pub media: String,
    /// Transport port; 0 means the stream is rejected/disabled.
    pub port: u16,
    /// Optional port count (`m=audio 5004/2 ...`).
    pub num_ports: Option<u32>,
    /// Transport protocol, e.g. `RTP/AVP` or `udptl`.
    pub proto: String,
    /// Format list (payload types, or `t38` for udptl).
    pub formats: Vec<String>,
    /// Media-level `c=`.
    pub connection: Option<Connection>,
    /// Media-level attributes in order.
    pub attributes: Vec<Attribute>,
    /// Uninterpreted lines (`i=`, `b=`, `k=`, ...).
    pub extra: Vec<RawLine>,
}

impl MediaDescription {
    /// Creates a media section without connection or attributes.
    pub fn new(media: &str, port: u16, proto: &str, formats: &[&str]) -> Self {
        MediaDescription {
            media: media.into(),
            port,
            num_ports: None,
            proto: proto.into(),
            formats: formats.iter().map(|s| (*s).to_string()).collect(),
            connection: None,
            attributes: Vec::new(),
            extra: Vec::new(),
        }
    }

    /// First attribute with this (case-insensitive) name.
    pub fn attribute(&self, name: &str) -> Option<&Attribute> {
        self.attributes
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(name))
    }

    /// All attributes with this (case-insensitive) name.
    pub fn attributes_named<'a>(&'a self, name: &'a str) -> impl Iterator<Item = &'a Attribute> {
        self.attributes
            .iter()
            .filter(move |a| a.name.eq_ignore_ascii_case(name))
    }

    /// `true` for `m=image <port> udptl t38`.
    pub fn is_t38(&self) -> bool {
        self.media.eq_ignore_ascii_case("image")
            && self.proto.eq_ignore_ascii_case("udptl")
            && self.formats.iter().any(|f| f.eq_ignore_ascii_case("t38"))
    }

    /// Direction attribute (default `sendrecv`).
    pub fn direction(&self) -> Direction {
        for a in &self.attributes {
            for d in [
                Direction::SendOnly,
                Direction::RecvOnly,
                Direction::Inactive,
                Direction::SendRecv,
            ] {
                if a.value.is_none() && a.name.eq_ignore_ascii_case(d.as_str()) {
                    return d;
                }
            }
        }
        Direction::SendRecv
    }

    /// Parsed `a=rtpmap:` entries (malformed ones are skipped).
    pub fn rtpmaps(&self) -> Vec<RtpMap> {
        self.attributes_named("rtpmap")
            .filter_map(|a| parse_rtpmap(a.value.as_deref()?))
            .collect()
    }

    /// `a=fmtp:` parameters for a payload type.
    pub fn fmtp(&self, payload_type: &str) -> Option<&str> {
        self.attributes_named("fmtp").find_map(|a| {
            let v = a.value.as_deref()?;
            let (pt, rest) = v.split_once(' ')?;
            (pt == payload_type).then_some(rest.trim())
        })
    }
}

fn parse_rtpmap(v: &str) -> Option<RtpMap> {
    let (pt, rest) = v.trim().split_once(' ')?;
    let payload_type = pt.parse().ok()?;
    let mut it = rest.trim().splitn(3, '/');
    let encoding = it.next()?.to_string();
    let clock_rate = it.next()?.parse().ok()?;
    if encoding.is_empty() {
        return None;
    }
    Some(RtpMap {
        payload_type,
        encoding,
        clock_rate,
        params: it.next().map(str::to_string),
    })
}

/// A complete session description.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionDescription {
    /// `v=` (always 0 in practice).
    pub version: u32,
    /// `o=`
    pub origin: Origin,
    /// `s=`; `None` if the line was missing (tolerated when parsing).
    pub session_name: Option<String>,
    /// Session-level `c=`.
    pub connection: Option<Connection>,
    /// `t=` lines.
    pub times: Vec<Timing>,
    /// Session-level attributes.
    pub attributes: Vec<Attribute>,
    /// Media sections in order.
    pub media: Vec<MediaDescription>,
    /// Uninterpreted session-level lines.
    pub extra: Vec<RawLine>,
}

/// Parses SDP from bytes (lossy UTF-8; CRLF or bare LF accepted).
pub fn parse_sdp(data: &[u8]) -> Result<SessionDescription, SdpError> {
    if data.len() > MAX_SDP_SIZE {
        return Err(SdpError::TooLarge);
    }
    SessionDescription::parse(&String::from_utf8_lossy(data))
}

fn split3(s: &str, field: &'static str) -> Result<(String, String, String), SdpError> {
    let mut it = s.split_ascii_whitespace();
    match (it.next(), it.next(), it.next(), it.next()) {
        (Some(a), Some(b), Some(c), None) => Ok((a.into(), b.into(), c.into())),
        _ => Err(invalid(field, "expected three space separated fields")),
    }
}

fn parse_connection(v: &str) -> Result<Connection, SdpError> {
    let (nettype, addrtype, address) = split3(v, "c=")?;
    Ok(Connection {
        nettype,
        addrtype,
        address,
    })
}

fn parse_origin(v: &str) -> Result<Origin, SdpError> {
    let f: Vec<&str> = v.split_ascii_whitespace().collect();
    let [username, id, ver, nettype, addrtype, address] = f.as_slice() else {
        return Err(invalid("o=", "expected six fields"));
    };
    Ok(Origin {
        username: (*username).into(),
        sess_id: id
            .parse()
            .map_err(|_| invalid("o=", "session id is not a number"))?,
        sess_version: ver
            .parse()
            .map_err(|_| invalid("o=", "session version is not a number"))?,
        nettype: (*nettype).into(),
        addrtype: (*addrtype).into(),
        address: (*address).into(),
    })
}

fn parse_media(v: &str) -> Result<MediaDescription, SdpError> {
    let mut it = v.split_ascii_whitespace();
    let (Some(media), Some(port), Some(proto)) = (it.next(), it.next(), it.next()) else {
        return Err(invalid("m=", "expected media, port and protocol"));
    };
    let (port, num_ports) = match port.split_once('/') {
        Some((p, n)) => (
            p,
            Some(
                n.parse::<u32>()
                    .map_err(|_| invalid("m=", "bad port count"))?,
            ),
        ),
        None => (port, None),
    };
    let port = port.parse::<u16>().map_err(|_| invalid("m=", "bad port"))?;
    let formats: Vec<String> = it.map(str::to_string).collect();
    if formats.is_empty() {
        return Err(invalid("m=", "no formats"));
    }
    Ok(MediaDescription {
        media: media.into(),
        port,
        num_ports,
        proto: proto.into(),
        formats,
        connection: None,
        attributes: Vec::new(),
        extra: Vec::new(),
    })
}

fn parse_attribute(v: &str) -> Result<Attribute, SdpError> {
    let (name, value) = match v.split_once(':') {
        Some((n, val)) => (n, Some(val.to_string())),
        None => (v, None),
    };
    if name.is_empty() || name.chars().any(|c| c.is_ascii_whitespace()) {
        return Err(invalid("a=", "bad attribute name"));
    }
    Ok(Attribute {
        name: name.into(),
        value,
    })
}

impl SessionDescription {
    /// Parses SDP text.
    pub fn parse(text: &str) -> Result<Self, SdpError> {
        if text.len() > MAX_SDP_SIZE {
            return Err(SdpError::TooLarge);
        }
        let mut version: Option<u32> = None;
        let mut origin: Option<Origin> = None;
        let mut session_name: Option<String> = None;
        let mut connection: Option<Connection> = None;
        let mut times = Vec::new();
        let mut attributes = Vec::new();
        let mut extra = Vec::new();
        let mut media: Vec<MediaDescription> = Vec::new();
        let mut seen_any = false;

        for (idx, raw) in text.split('\n').enumerate() {
            let line = raw.strip_suffix('\r').unwrap_or(raw);
            if line.is_empty() {
                continue;
            }
            let n = idx + 1;
            let mut chars = line.chars();
            let (Some(kind), Some('=')) = (chars.next(), chars.next()) else {
                return Err(SdpError::MalformedLine { line: n });
            };
            if !kind.is_ascii_lowercase() {
                return Err(SdpError::MalformedLine { line: n });
            }
            let value = &line[2..];
            if !seen_any {
                if kind != 'v' {
                    return Err(SdpError::MissingVersion);
                }
                seen_any = true;
            }
            if kind == 'm' {
                if media.len() >= MAX_MEDIA {
                    return Err(SdpError::TooManyMedia);
                }
                media.push(parse_media(value)?);
                continue;
            }
            if let Some(m) = media.last_mut() {
                match kind {
                    'c' if m.connection.is_none() => m.connection = Some(parse_connection(value)?),
                    'a' => m.attributes.push(parse_attribute(value)?),
                    _ => m.extra.push(RawLine {
                        kind,
                        value: value.into(),
                    }),
                }
                continue;
            }
            match kind {
                'v' => {
                    if version.is_some() {
                        return Err(invalid("v=", "duplicate"));
                    }
                    version = Some(
                        value
                            .trim()
                            .parse()
                            .map_err(|_| invalid("v=", "not a number"))?,
                    );
                }
                'o' => {
                    if origin.is_some() {
                        return Err(invalid("o=", "duplicate"));
                    }
                    origin = Some(parse_origin(value)?);
                }
                's' => {
                    if session_name.is_some() {
                        return Err(invalid("s=", "duplicate"));
                    }
                    session_name = Some(value.to_string());
                }
                'c' if connection.is_none() => connection = Some(parse_connection(value)?),
                't' => {
                    let mut it = value.split_ascii_whitespace();
                    let (Some(a), Some(b), None) = (it.next(), it.next(), it.next()) else {
                        return Err(invalid("t=", "expected start and stop"));
                    };
                    times.push(Timing {
                        start: a
                            .parse()
                            .map_err(|_| invalid("t=", "start is not a number"))?,
                        stop: b
                            .parse()
                            .map_err(|_| invalid("t=", "stop is not a number"))?,
                    });
                }
                'a' => attributes.push(parse_attribute(value)?),
                _ => extra.push(RawLine {
                    kind,
                    value: value.into(),
                }),
            }
        }
        if !seen_any {
            return Err(SdpError::Empty);
        }
        Ok(SessionDescription {
            version: version.ok_or(SdpError::MissingVersion)?,
            origin: origin.ok_or(SdpError::MissingOrigin)?,
            session_name,
            connection,
            times,
            attributes,
            media,
            extra,
        })
    }

    /// Serializes with CRLF line endings.
    pub fn to_sdp_string(&self) -> String {
        self.to_string()
    }

    /// Serializes to bytes.
    pub fn to_bytes(&self) -> Vec<u8> {
        self.to_string().into_bytes()
    }

    /// Session-level attribute lookup (case-insensitive).
    pub fn attribute(&self, name: &str) -> Option<&Attribute> {
        self.attributes
            .iter()
            .find(|a| a.name.eq_ignore_ascii_case(name))
    }

    /// Connection address in effect for a media section (media level wins over session level).
    pub fn effective_connection<'a>(&'a self, m: &'a MediaDescription) -> Option<&'a Connection> {
        m.connection.as_ref().or(self.connection.as_ref())
    }
}

fn write_extras(out: &mut String, extra: &[RawLine], kinds: &[char]) {
    for e in extra.iter().filter(|e| kinds.contains(&e.kind)) {
        let _ = write!(out, "{}={}\r\n", e.kind, e.value);
    }
}

fn write_attr(out: &mut String, a: &Attribute) {
    match &a.value {
        Some(v) => {
            let _ = write!(out, "a={}:{}\r\n", a.name, v);
        }
        None => {
            let _ = write!(out, "a={}\r\n", a.name);
        }
    }
}

fn write_conn(out: &mut String, c: &Connection) {
    let _ = write!(out, "c={} {} {}\r\n", c.nettype, c.addrtype, c.address);
}

const SESSION_KNOWN_EXTRA: [char; 8] = ['i', 'u', 'e', 'p', 'b', 'r', 'z', 'k'];
const MEDIA_KNOWN_EXTRA: [char; 3] = ['i', 'b', 'k'];

impl fmt::Display for SessionDescription {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let mut out = String::new();
        let _ = write!(out, "v={}\r\n", self.version);
        let o = &self.origin;
        let _ = write!(
            out,
            "o={} {} {} {} {} {}\r\n",
            o.username, o.sess_id, o.sess_version, o.nettype, o.addrtype, o.address
        );
        if let Some(s) = &self.session_name {
            let _ = write!(out, "s={s}\r\n");
        }
        write_extras(&mut out, &self.extra, &['i', 'u', 'e', 'p']);
        if let Some(c) = &self.connection {
            write_conn(&mut out, c);
        }
        write_extras(&mut out, &self.extra, &['b']);
        for t in &self.times {
            let _ = write!(out, "t={} {}\r\n", t.start, t.stop);
        }
        write_extras(&mut out, &self.extra, &['r', 'z', 'k']);
        for e in self
            .extra
            .iter()
            .filter(|e| !SESSION_KNOWN_EXTRA.contains(&e.kind))
        {
            let _ = write!(out, "{}={}\r\n", e.kind, e.value);
        }
        for a in &self.attributes {
            write_attr(&mut out, a);
        }
        for m in &self.media {
            let _ = write!(out, "m={} {}", m.media, m.port);
            if let Some(n) = m.num_ports {
                let _ = write!(out, "/{n}");
            }
            let _ = write!(out, " {}", m.proto);
            for fmt_ in &m.formats {
                let _ = write!(out, " {fmt_}");
            }
            out.push_str("\r\n");
            write_extras(&mut out, &m.extra, &['i']);
            if let Some(c) = &m.connection {
                write_conn(&mut out, c);
            }
            write_extras(&mut out, &m.extra, &['b', 'k']);
            for e in m
                .extra
                .iter()
                .filter(|e| !MEDIA_KNOWN_EXTRA.contains(&e.kind))
            {
                let _ = write!(out, "{}={}\r\n", e.kind, e.value);
            }
            for a in &m.attributes {
                write_attr(&mut out, a);
            }
        }
        f.write_str(&out)
    }
}
