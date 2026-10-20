//! Ordered, case-insensitive header collection with compact-form normalization.

use super::cseq::CSeq;
use super::name_addr::NameAddr;
use super::parse::ParseError;
use super::via::Via;

/// Canonical header names known to the stack (RFC 3261 plus common extensions). Parsing maps
/// any capitalisation of these onto the spelling below; unknown names are kept as written.
const KNOWN: &[&str] = &[
    "Accept",
    "Accept-Encoding",
    "Accept-Language",
    "Alert-Info",
    "Allow",
    "Allow-Events",
    "Authentication-Info",
    "Authorization",
    "Call-ID",
    "Call-Info",
    "Contact",
    "Content-Disposition",
    "Content-Encoding",
    "Content-Language",
    "Content-Length",
    "Content-Type",
    "CSeq",
    "Date",
    "Error-Info",
    "Event",
    "Expires",
    "From",
    "Identity",
    "In-Reply-To",
    "Max-Forwards",
    "Min-Expires",
    "Min-SE",
    "MIME-Version",
    "Organization",
    "Priority",
    "Proxy-Authenticate",
    "Proxy-Authorization",
    "Proxy-Require",
    "RAck",
    "Record-Route",
    "Refer-To",
    "Referred-By",
    "Reply-To",
    "Require",
    "Retry-After",
    "Route",
    "RSeq",
    "Server",
    "Session-Expires",
    "Subject",
    "Supported",
    "Timestamp",
    "To",
    "Unsupported",
    "User-Agent",
    "Via",
    "Warning",
    "WWW-Authenticate",
];

/// Compact form letter and the header it stands for.
pub const COMPACT_FORMS: [(char, &str); 16] = [
    ('i', "Call-ID"),
    ('m', "Contact"),
    ('e', "Content-Encoding"),
    ('l', "Content-Length"),
    ('c', "Content-Type"),
    ('f', "From"),
    ('s', "Subject"),
    ('k', "Supported"),
    ('t', "To"),
    ('v', "Via"),
    ('o', "Event"),
    ('u', "Allow-Events"),
    ('r', "Refer-To"),
    ('b', "Referred-By"),
    ('x', "Session-Expires"),
    ('y', "Identity"),
];

/// Headers whose value is a comma-separated list that may be split into several header lines
/// (RFC 3261 section 7.3.1). Other headers (Date, Subject, WWW-Authenticate, unknown ...) can
/// legitimately contain commas and are never split.
pub(crate) const LIST_HEADERS: &[&str] = &[
    "Accept",
    "Accept-Encoding",
    "Accept-Language",
    "Alert-Info",
    "Allow",
    "Allow-Events",
    "Call-Info",
    "Contact",
    "Content-Encoding",
    "Content-Language",
    "Error-Info",
    "In-Reply-To",
    "Proxy-Require",
    "Record-Route",
    "Require",
    "Route",
    "Supported",
    "Unsupported",
    "Via",
    "Warning",
];

/// Resolves `name` (any case, long or compact form) to `(canonical name, was_compact)`.
/// Unknown names are returned unchanged.
pub fn canonical_name(name: &str) -> (String, bool) {
    if name.len() == 1 {
        let c = name.chars().next().unwrap_or(' ').to_ascii_lowercase();
        if let Some((_, full)) = COMPACT_FORMS.iter().find(|(l, _)| *l == c) {
            return ((*full).to_string(), true);
        }
    }
    match KNOWN.iter().find(|k| k.eq_ignore_ascii_case(name)) {
        Some(k) => ((*k).to_string(), false),
        None => (name.to_string(), false),
    }
}

/// Compact letter for a canonical header name, if it has one.
pub fn compact_letter(canonical: &str) -> Option<char> {
    COMPACT_FORMS
        .iter()
        .find(|(_, full)| full.eq_ignore_ascii_case(canonical))
        .map(|(l, _)| *l)
}

/// One header line.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Header {
    /// Canonical name for known headers, the original spelling for unknown ones.
    pub name: String,
    /// `true` if the header should be written in compact form (it was read that way).
    pub compact: bool,
    /// Value with leading/trailing whitespace removed and folding resolved.
    pub value: String,
}

impl Header {
    /// Creates a header; `name` may be a compact letter or any capitalisation of a known name.
    pub fn new(name: &str, value: impl Into<String>) -> Self {
        let (name, compact) = canonical_name(name);
        Header {
            name,
            compact,
            value: value.into(),
        }
    }

    /// The name as it is written on the wire (compact letter when `compact`).
    pub fn wire_name(&self) -> String {
        if self.compact {
            if let Some(l) = compact_letter(&self.name) {
                return l.to_string();
            }
        }
        self.name.clone()
    }
}

/// Ordered header list. Lookup is case-insensitive and understands compact names.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Headers {
    items: Vec<Header>,
}

impl Headers {
    /// Empty list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Number of header lines.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// `true` when there are no headers.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Iterates headers in wire order.
    pub fn iter(&self) -> impl Iterator<Item = &Header> {
        self.items.iter()
    }

    /// Appends a header.
    pub fn push(&mut self, name: &str, value: impl Into<String>) {
        self.items.push(Header::new(name, value));
    }

    /// Appends an already built header.
    pub fn push_header(&mut self, h: Header) {
        self.items.push(h);
    }

    /// Inserts a header before all others (used for the topmost Via).
    pub fn push_front(&mut self, name: &str, value: impl Into<String>) {
        self.items.insert(0, Header::new(name, value));
    }

    fn matches(h: &Header, canon: &str) -> bool {
        h.name.eq_ignore_ascii_case(canon)
    }

    /// First value of the header.
    pub fn get(&self, name: &str) -> Option<&str> {
        let (canon, _) = canonical_name(name);
        self.items
            .iter()
            .find(|h| Self::matches(h, &canon))
            .map(|h| h.value.as_str())
    }

    /// All values of the header in order.
    pub fn get_all<'a>(&'a self, name: &str) -> impl Iterator<Item = &'a str> + 'a {
        let (canon, _) = canonical_name(name);
        self.items
            .iter()
            .filter(move |h| Self::matches(h, &canon))
            .map(|h| h.value.as_str())
    }

    /// `true` if at least one such header exists.
    pub fn contains(&self, name: &str) -> bool {
        self.get(name).is_some()
    }

    /// Removes all headers with this name, returning how many were removed.
    pub fn remove(&mut self, name: &str) -> usize {
        let (canon, _) = canonical_name(name);
        let before = self.items.len();
        self.items.retain(|h| !Self::matches(h, &canon));
        before - self.items.len()
    }

    /// Replaces the first header of this name in place (keeping its position and compact
    /// spelling) and removes the others; appends when absent.
    pub fn set(&mut self, name: &str, value: impl Into<String>) {
        let (canon, _) = canonical_name(name);
        let value = value.into();
        match self.items.iter().position(|h| Self::matches(h, &canon)) {
            Some(i) => {
                self.items[i].value = value;
                let mut seen = false;
                self.items.retain(|h| {
                    if Self::matches(h, &canon) {
                        let keep = !seen;
                        seen = true;
                        keep
                    } else {
                        true
                    }
                });
            }
            None => self.push(name, value),
        }
    }

    /// Replaces the value of the first header with this name in place (other headers of the
    /// same name are kept). Returns `false` when there is none.
    pub fn set_first(&mut self, name: &str, value: impl Into<String>) -> bool {
        let (canon, _) = canonical_name(name);
        match self.items.iter_mut().find(|h| Self::matches(h, &canon)) {
            Some(h) => {
                h.value = value.into();
                true
            }
            None => false,
        }
    }

    /// Sets the `Content-Length` header (see [`Self::set`]).
    pub fn set_content_length(&mut self, len: usize) {
        self.set("Content-Length", len.to_string());
    }

    // ---- typed accessors -------------------------------------------------------------

    /// Parses every Via header value in order (topmost first).
    pub fn vias(&self) -> Result<Vec<Via>, ParseError> {
        self.get_all("Via").map(Via::parse).collect()
    }

    /// Parses the topmost Via.
    pub fn top_via(&self) -> Result<Via, ParseError> {
        Via::parse(self.get("Via").ok_or(ParseError::MissingHeader("Via"))?)
    }

    /// Parses `From`.
    pub fn from(&self) -> Result<NameAddr, ParseError> {
        NameAddr::parse(self.get("From").ok_or(ParseError::MissingHeader("From"))?)
    }

    /// Parses `To`.
    pub fn to(&self) -> Result<NameAddr, ParseError> {
        NameAddr::parse(self.get("To").ok_or(ParseError::MissingHeader("To"))?)
    }

    /// Parses `CSeq`.
    pub fn cseq(&self) -> Result<CSeq, ParseError> {
        CSeq::parse(self.get("CSeq").ok_or(ParseError::MissingHeader("CSeq"))?)
    }

    /// The `Call-ID` value.
    pub fn call_id(&self) -> Option<&str> {
        self.get("Call-ID")
    }

    /// Parses `Max-Forwards`: `Ok(None)` when absent (tolerated), an error when malformed.
    pub fn max_forwards(&self) -> Result<Option<u8>, ParseError> {
        self.get("Max-Forwards").map(parse_max_forwards).transpose()
    }

    /// Parses all `Contact` values (a lone `*` yields an empty list; see
    /// [`Self::contact_is_wildcard`]).
    pub fn contacts(&self) -> Result<Vec<NameAddr>, ParseError> {
        self.name_addrs("Contact")
    }

    /// `true` for a `Contact: *` header.
    pub fn contact_is_wildcard(&self) -> bool {
        self.get("Contact").map(str::trim) == Some("*")
    }

    /// Parses all `Record-Route` values in order.
    pub fn record_routes(&self) -> Result<Vec<NameAddr>, ParseError> {
        self.name_addrs("Record-Route")
    }

    /// Parses all `Route` values in order.
    pub fn routes(&self) -> Result<Vec<NameAddr>, ParseError> {
        self.name_addrs("Route")
    }

    fn name_addrs(&self, name: &str) -> Result<Vec<NameAddr>, ParseError> {
        self.get_all(name)
            .filter(|v| v.trim() != "*")
            .map(NameAddr::parse)
            .collect()
    }
}

pub(crate) fn parse_max_forwards(v: &str) -> Result<u8, ParseError> {
    let v = v.trim();
    if v.is_empty() || !v.bytes().all(|b| b.is_ascii_digit()) {
        return Err(ParseError::invalid_header(
            "Max-Forwards",
            "not a decimal number",
        ));
    }
    v.parse::<u8>()
        .map_err(|_| ParseError::invalid_header("Max-Forwards", "value above 255"))
}
