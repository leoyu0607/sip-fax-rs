//! Message parser: `&[u8]` to [`SipMessage`], with stream (TCP) and datagram (UDP) framing.
//!
//! The parser is a pure function with no I/O and no panics on any input. Header lines are
//! unfolded, compact names normalised and comma-separated lists of list-type headers split
//! (see `Headers`). The mandatory headers (To, From, CSeq, Call-ID, Via; Max-Forwards is optional but
//! validated when present) and the syntax of From/To/Via/CSeq/Contact/Route/Record-Route are validated; a
//! malformed one makes the whole message a [`ParseError`].
//!
//! Non-UTF-8 octets in the start line or header values are replaced by U+FFFD (the body is
//! always kept as raw bytes).

use bytes::Bytes;

use super::headers::{Header, Headers, LIST_HEADERS, canonical_name, parse_max_forwards};
use super::lex::{is_token, split_top_level};
use super::name_addr::NameAddr;
use super::uri::Uri;
use super::via::Via;
use super::{CSeq, Method, Request, Response, SipMessage, StatusCode};

/// Maximum size of one message (headers plus body): 64 KiB.
pub const MAX_MESSAGE_SIZE: usize = 65_536;
/// Maximum number of header entries (after list splitting).
pub const MAX_HEADERS: usize = 256;

/// Why a message could not be parsed.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// Stream mode only: more bytes are needed to complete the message.
    #[error("incomplete message, more data needed")]
    Incomplete,
    /// No data.
    #[error("empty message")]
    Empty,
    /// Message exceeds [`MAX_MESSAGE_SIZE`].
    #[error("message larger than {MAX_MESSAGE_SIZE} bytes")]
    TooLarge,
    /// More than [`MAX_HEADERS`] header entries.
    #[error("more than {MAX_HEADERS} headers")]
    TooManyHeaders,
    /// The request line or status line is malformed.
    #[error("invalid start line: {0}")]
    InvalidStartLine(String),
    /// SIP version other than 2.0 (the caller should answer 505).
    #[error("unsupported SIP version")]
    UnsupportedVersion,
    /// The request method is not a valid token.
    #[error("invalid method")]
    InvalidMethod,
    /// The status code is not three digits in 100..=699.
    #[error("invalid status code")]
    InvalidStatusCode,
    /// A URI is malformed.
    #[error("invalid URI: {0}")]
    InvalidUri(String),
    /// A header line or value is malformed.
    #[error("invalid header {name}: {reason}")]
    InvalidHeader {
        /// Header (or element) name.
        name: &'static str,
        /// What is wrong.
        reason: String,
    },
    /// A mandatory header is absent.
    #[error("missing mandatory header {0}")]
    MissingHeader(&'static str),
    /// A single-valued mandatory header appears more than once.
    #[error("header {0} appears more than once")]
    MultipleHeader(&'static str),
    /// `Content-Length` is not a non-negative decimal number, or several conflicting values.
    #[error("invalid Content-Length")]
    InvalidContentLength,
    /// Stream mode: `Content-Length` is required on stream transports.
    #[error("missing Content-Length on a stream transport")]
    MissingContentLength,
    /// Datagram mode: `Content-Length` is larger than the data that arrived.
    #[error("Content-Length {declared} exceeds the {available} body bytes received")]
    ContentLengthMismatch {
        /// Declared length.
        declared: usize,
        /// Bytes actually present.
        available: usize,
    },
}

impl ParseError {
    pub(crate) fn invalid_header(name: &'static str, reason: impl Into<String>) -> Self {
        ParseError::InvalidHeader {
            name,
            reason: reason.into(),
        }
    }
}

/// How message boundaries are determined.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Framing {
    /// Byte stream (TCP/TLS): `Content-Length` is mandatory, trailing bytes belong to the next
    /// message and are not consumed.
    Stream,
    /// One message per datagram (UDP): a missing `Content-Length` means "rest of datagram", a
    /// smaller value truncates, a larger value is an error (RFC 3261 section 18.3).
    Datagram,
}

/// Parses one message from a byte stream (RFC 3261 section 18.3). Returns the message and the
/// number of bytes consumed (leading keep-alive CRLFs included). [`ParseError::Incomplete`]
/// means "feed more bytes".
pub fn parse_message(buf: &[u8]) -> Result<(SipMessage, usize), ParseError> {
    parse_with_framing(buf, Framing::Stream)
}

/// Parses one message that arrived as a complete UDP datagram.
pub fn parse_datagram(buf: &[u8]) -> Result<SipMessage, ParseError> {
    parse_with_framing(buf, Framing::Datagram).map(|(m, _)| m)
}

/// Parses with an explicit [`Framing`].
pub fn parse_with_framing(buf: &[u8], framing: Framing) -> Result<(SipMessage, usize), ParseError> {
    if framing == Framing::Datagram && buf.len() > MAX_MESSAGE_SIZE {
        return Err(ParseError::TooLarge);
    }
    let skip = buf
        .iter()
        .take_while(|&&b| b == b'\r' || b == b'\n')
        .count();
    let data = &buf[skip..];
    if data.is_empty() {
        return Err(match framing {
            Framing::Stream => ParseError::Incomplete,
            Framing::Datagram => ParseError::Empty,
        });
    }
    let (head_bytes, body_start) = match data.windows(4).position(|w| w == b"\r\n\r\n") {
        Some(i) => (&data[..i], i + 4),
        None => match framing {
            Framing::Stream => {
                return Err(if data.len() > MAX_MESSAGE_SIZE {
                    ParseError::TooLarge
                } else {
                    ParseError::Incomplete
                });
            }
            // Liberal in datagram mode: a missing final blank line ends the headers.
            Framing::Datagram => {
                let mut end = data.len();
                while end > 0 && (data[end - 1] == b'\r' || data[end - 1] == b'\n') {
                    end -= 1;
                }
                (&data[..end], data.len())
            }
        },
    };
    if framing == Framing::Stream && body_start > MAX_MESSAGE_SIZE {
        return Err(ParseError::TooLarge);
    }

    let head = String::from_utf8_lossy(head_bytes);
    let mut lines = head.split("\r\n");
    let start_line = lines.next().unwrap_or("");
    if start_line.is_empty() {
        return Err(ParseError::InvalidStartLine("empty start line".into()));
    }

    // Unfold continuation lines (LWS), then parse "name: value".
    let mut logical: Vec<String> = Vec::new();
    for line in lines {
        if line.starts_with(' ') || line.starts_with('\t') {
            match logical.last_mut() {
                Some(prev) => {
                    prev.push(' ');
                    prev.push_str(line.trim());
                }
                None => {
                    return Err(ParseError::invalid_header(
                        "header",
                        "continuation line before first header",
                    ));
                }
            }
        } else {
            if logical.len() >= MAX_HEADERS {
                return Err(ParseError::TooManyHeaders);
            }
            logical.push(line.to_string());
        }
    }

    let mut headers = Headers::new();
    for line in &logical {
        let Some((raw_name, raw_value)) = line.split_once(':') else {
            return Err(ParseError::invalid_header("header", "line without ':'"));
        };
        let raw_name = raw_name.trim_end();
        if !is_token(raw_name) {
            return Err(ParseError::invalid_header("header", "invalid header name"));
        }
        push_header(&mut headers, raw_name, raw_value.trim())?;
    }

    // Content-Length (all occurrences must agree).
    let mut content_length: Option<usize> = None;
    for v in headers.get_all("Content-Length") {
        let v = v.trim();
        if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ParseError::InvalidContentLength);
        }
        let n = v
            .parse::<usize>()
            .map_err(|_| ParseError::InvalidContentLength)?;
        match content_length {
            Some(prev) if prev != n => return Err(ParseError::InvalidContentLength),
            _ => content_length = Some(n),
        }
    }

    let first = parse_start_line(start_line)?;
    validate_headers(&headers, matches!(first, StartLine::Request { .. }))?;

    // Body framing.
    let available = data.len().saturating_sub(body_start).min(data.len());
    let (body, consumed) = match framing {
        Framing::Stream => {
            let cl = content_length.ok_or(ParseError::MissingContentLength)?;
            let total = body_start.saturating_add(cl);
            if total > MAX_MESSAGE_SIZE {
                return Err(ParseError::TooLarge);
            }
            if data.len() < total {
                return Err(ParseError::Incomplete);
            }
            (&data[body_start..total], skip + total)
        }
        Framing::Datagram => {
            let rest = &data[body_start.min(data.len())..];
            match content_length {
                None => (rest, buf.len()),
                Some(cl) if cl > rest.len() => {
                    return Err(ParseError::ContentLengthMismatch {
                        declared: cl,
                        available,
                    });
                }
                Some(cl) => (&rest[..cl], buf.len()),
            }
        }
    };
    let body = Bytes::copy_from_slice(body);

    let msg = match first {
        StartLine::Request { method, uri } => SipMessage::Request(Request {
            method,
            uri,
            headers,
            body,
        }),
        StartLine::Response { status, reason } => SipMessage::Response(Response {
            status,
            reason,
            headers,
            body,
        }),
    };
    Ok((msg, consumed))
}

/// Normalises the name, splits list values and appends the resulting entries.
fn push_header(headers: &mut Headers, raw_name: &str, value: &str) -> Result<(), ParseError> {
    let (name, compact) = canonical_name(raw_name);
    let is_list = LIST_HEADERS.iter().any(|h| h.eq_ignore_ascii_case(&name));
    let strict_list = matches!(name.as_str(), "Via" | "Route" | "Record-Route" | "Contact");
    let mut push = |value: &str| -> Result<(), ParseError> {
        if headers.len() >= MAX_HEADERS {
            return Err(ParseError::TooManyHeaders);
        }
        headers.push_header(Header {
            name: name.clone(),
            compact,
            value: value.to_string(),
        });
        Ok(())
    };
    if !is_list || value.is_empty() {
        return push(value);
    }
    let parts = split_top_level(value, ',');
    for part in parts {
        if part.is_empty() {
            if strict_list {
                return Err(ParseError::invalid_header("header", "empty list element"));
            }
            continue;
        }
        push(part)?;
    }
    Ok(())
}

enum StartLine {
    Request { method: Method, uri: Uri },
    Response { status: StatusCode, reason: String },
}

fn parse_version(v: &str) -> Result<(), ParseError> {
    if v == "SIP/2.0" {
        return Ok(());
    }
    // "SIP/<digits>.<digits>" is a well-formed but unsupported version.
    let ok_shape = v.strip_prefix("SIP/").is_some_and(|r| {
        r.split_once('.').is_some_and(|(a, b)| {
            !a.is_empty()
                && !b.is_empty()
                && a.bytes().all(|c| c.is_ascii_digit())
                && b.bytes().all(|c| c.is_ascii_digit())
        })
    });
    if ok_shape {
        Err(ParseError::UnsupportedVersion)
    } else {
        Err(ParseError::InvalidStartLine("malformed SIP version".into()))
    }
}

fn parse_start_line(line: &str) -> Result<StartLine, ParseError> {
    if line.len() >= 4 && line.as_bytes()[..4].eq_ignore_ascii_case(b"SIP/") {
        // Status-Line: SIP-Version SP Status-Code SP Reason-Phrase
        let (version, rest) = line.split_once(' ').unwrap_or((line, ""));
        parse_version(version)?;
        let (code, reason) = match rest.split_once(' ') {
            Some((c, r)) => (c, r),
            None => (rest, ""),
        };
        if code.len() != 3 || !code.bytes().all(|b| b.is_ascii_digit()) {
            return Err(ParseError::InvalidStatusCode);
        }
        let n = code
            .parse::<u16>()
            .map_err(|_| ParseError::InvalidStatusCode)?;
        let status = StatusCode::new(n).ok_or(ParseError::InvalidStatusCode)?;
        return Ok(StartLine::Response {
            status,
            reason: reason.to_string(),
        });
    }
    // Request-Line: Method SP Request-URI SP SIP-Version, exactly single spaces.
    let parts: Vec<&str> = line.split(' ').collect();
    let [method, uri, version] = parts.as_slice() else {
        return Err(ParseError::InvalidStartLine(
            "request line must be 'METHOD URI SIP/2.0' with single spaces".into(),
        ));
    };
    if !is_token(method) {
        return Err(ParseError::InvalidMethod);
    }
    parse_version(version)?;
    let uri = Uri::parse(uri)?;
    Ok(StartLine::Request {
        method: Method::from_token(method),
        uri,
    })
}

fn validate_headers(h: &Headers, is_request: bool) -> Result<(), ParseError> {
    // Presence, in the order mandated by the spec (so the first missing one is reported).
    // Max-Forwards is deliberately NOT mandatory (docs/design/sip-core.md section 9,
    // M1a review): peers without a spec (e.g. CHT CS210) may omit it. It is validated below
    // when present; the UAS decides what a missing one means.
    for name in ["To", "From", "CSeq", "Call-ID", "Via"] {
        let count = h.get_all(name).count();
        if count == 0 {
            return Err(ParseError::MissingHeader(name));
        }
        if name != "Via" && count > 1 {
            return Err(ParseError::MultipleHeader(name));
        }
    }
    // Syntax.
    NameAddr::parse(h.get("To").unwrap_or("")).map_err(|e| rename(e, "To"))?;
    NameAddr::parse(h.get("From").unwrap_or("")).map_err(|e| rename(e, "From"))?;
    CSeq::parse(h.get("CSeq").unwrap_or(""))?;
    if h.get("Call-ID").unwrap_or("").is_empty() {
        return Err(ParseError::invalid_header("Call-ID", "empty"));
    }
    if is_request {
        if h.get_all("Max-Forwards").count() > 1 {
            return Err(ParseError::MultipleHeader("Max-Forwards"));
        }
        if let Some(v) = h.get("Max-Forwards") {
            parse_max_forwards(v)?;
        }
    }
    for v in h.get_all("Via") {
        Via::parse(v)?;
    }
    for name in ["Contact", "Route", "Record-Route"] {
        for v in h.get_all(name) {
            if v.is_empty() || v == "*" {
                continue;
            }
            NameAddr::parse(v).map_err(|e| rename(e, static_name(name)))?;
        }
    }
    Ok(())
}

fn static_name(n: &str) -> &'static str {
    match n {
        "Contact" => "Contact",
        "Route" => "Route",
        _ => "Record-Route",
    }
}

fn rename(e: ParseError, name: &'static str) -> ParseError {
    match e {
        ParseError::InvalidHeader { reason, .. } => ParseError::InvalidHeader { name, reason },
        ParseError::InvalidUri(r) => ParseError::InvalidHeader { name, reason: r },
        other => other,
    }
}
