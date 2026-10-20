//! `name-addr` / `addr-spec` header values (From, To, Contact, Route, Record-Route, ...).

use std::fmt;

use super::lex::{self, Param, find_param, is_token, parse_params, write_params};
use super::parse::ParseError;
use super::uri::Uri;

/// A parsed name-addr or addr-spec with its header parameters (RFC 3261 section 25.1).
///
/// In the addr-spec form (no angle brackets) everything after the first `;` is a header
/// parameter, never a URI parameter. Serialization always uses the bracketed form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NameAddr {
    /// Display name with quotes removed and quoted-pairs resolved.
    pub display_name: Option<String>,
    /// The URI.
    pub uri: Uri,
    /// Header parameters such as `tag`, `q`, `expires`.
    pub params: Vec<Param>,
}

impl NameAddr {
    /// Creates a name-addr without display name or parameters.
    pub fn new(uri: impl Into<Uri>) -> Self {
        NameAddr {
            display_name: None,
            uri: uri.into(),
            params: Vec::new(),
        }
    }

    /// Parses one header value (a single element, no top-level commas).
    pub fn parse(s: &str) -> Result<Self, ParseError> {
        let err = |r: &str| ParseError::InvalidHeader {
            name: "name-addr",
            reason: r.to_string(),
        };
        let s = s.trim();
        if s.is_empty() {
            return Err(err("empty value"));
        }
        let (display_name, rest) = if s.starts_with('"') {
            let Some(end) = lex::scan_quoted(s, 0) else {
                return Err(err("unterminated quoted display name"));
            };
            let rest = s[end..].trim_start();
            if !rest.starts_with('<') {
                return Err(err("expected '<' after quoted display name"));
            }
            (Some(lex::unquote(&s[..end])), rest)
        } else if let Some(lt) = s.find('<') {
            let before = s[..lt].trim();
            let display = if before.is_empty() {
                None
            } else {
                if !before.split_ascii_whitespace().all(is_token) {
                    return Err(err("non-token characters in unquoted display name"));
                }
                Some(before.to_string())
            };
            (display, &s[lt..])
        } else {
            // addr-spec form
            let (uri_text, params_text) = match s.find(';') {
                Some(i) => (s[..i].trim_end(), &s[i..]),
                None => (s, ""),
            };
            if uri_text.contains('?') || uri_text.contains(',') {
                return Err(err("URI with '?' or ',' must be enclosed in <>"));
            }
            return Ok(NameAddr {
                display_name: None,
                uri: Uri::parse(uri_text)?,
                params: parse_params(params_text, "name-addr")?,
            });
        };
        // rest starts with '<'
        let Some(gt) = rest.find('>') else {
            return Err(err("missing '>'"));
        };
        let uri = Uri::parse(&rest[1..gt])?;
        let params = parse_params(&rest[gt + 1..], "name-addr")?;
        Ok(NameAddr {
            display_name,
            uri,
            params,
        })
    }

    /// The `tag` header parameter.
    pub fn tag(&self) -> Option<&str> {
        find_param(&self.params, "tag").and_then(|p| p.value.as_deref())
    }

    /// Sets (or replaces) the `tag` parameter.
    pub fn set_tag(&mut self, tag: impl Into<String>) {
        let tag = tag.into();
        match self
            .params
            .iter_mut()
            .find(|p| p.name.eq_ignore_ascii_case("tag"))
        {
            Some(p) => p.value = Some(tag),
            None => self.params.push(Param {
                name: "tag".into(),
                value: Some(tag),
            }),
        }
    }

    /// Looks up a header parameter by case-insensitive name.
    pub fn param(&self, name: &str) -> Option<Option<&str>> {
        find_param(&self.params, name).map(|p| p.value.as_deref())
    }
}

impl fmt::Display for NameAddr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        if let Some(d) = &self.display_name {
            write!(f, "{} ", lex::quote(d))?;
        }
        write!(f, "<{}>", self.uri)?;
        write_params(f, &self.params)
    }
}
