//! Small lexical helpers shared by the header parsers (token rules, quoted strings, parameter
//! lists, top-level list splitting). All functions are total: they never panic on any input.

use std::fmt;

use super::parse::ParseError;

/// One `;name[=value]` parameter. The value is kept exactly as written (a quoted string keeps
/// its quotes) so that serialization reproduces it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Param {
    /// Parameter name (case preserved; compare with `eq_ignore_ascii_case`).
    pub name: String,
    /// Parameter value as written, `None` for a bare flag such as `lr` or `rport`.
    pub value: Option<String>,
}

impl Param {
    /// Creates a parameter.
    pub fn new(name: impl Into<String>, value: Option<impl Into<String>>) -> Self {
        Param {
            name: name.into(),
            value: value.map(Into::into),
        }
    }
}

impl fmt::Display for Param {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.value {
            Some(v) => write!(f, "{}={}", self.name, v),
            None => f.write_str(&self.name),
        }
    }
}

/// Finds a parameter by case-insensitive name.
pub fn find_param<'a>(params: &'a [Param], name: &str) -> Option<&'a Param> {
    params.iter().find(|p| p.name.eq_ignore_ascii_case(name))
}

/// RFC 3261 `token` character.
pub fn is_token_char(c: char) -> bool {
    c.is_ascii_alphanumeric()
        || matches!(
            c,
            '-' | '.' | '!' | '%' | '*' | '_' | '+' | '`' | '\'' | '~'
        )
}

/// `true` when `s` is a non-empty RFC 3261 token.
pub fn is_token(s: &str) -> bool {
    !s.is_empty() && s.chars().all(is_token_char)
}

/// Returns the byte index just after the closing quote of the quoted string starting at
/// `s[start]` (which must be `"`), honouring `\` quoted-pairs. `None` when unterminated.
pub fn scan_quoted(s: &str, start: usize) -> Option<usize> {
    let bytes = s.as_bytes();
    if bytes.get(start) != Some(&b'"') {
        return None;
    }
    let mut i = start + 1;
    while i < bytes.len() {
        match bytes[i] {
            b'\\' => i += 2,
            b'"' => return Some(i + 1),
            _ => i += 1,
        }
    }
    None
}

/// Removes the quotes of a quoted string and resolves quoted-pairs. `s` must be the complete
/// quoted string including both quotes.
pub fn unquote(s: &str) -> String {
    let inner = s
        .strip_prefix('"')
        .and_then(|r| r.strip_suffix('"'))
        .unwrap_or(s);
    let mut out = String::with_capacity(inner.len());
    let mut chars = inner.chars();
    while let Some(c) = chars.next() {
        if c == '\\' {
            if let Some(n) = chars.next() {
                out.push(n);
            }
        } else {
            out.push(c);
        }
    }
    out
}

/// Wraps `s` into a quoted string, escaping `"` and `\`.
pub fn quote(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        if c == '"' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('"');
    out
}

/// Splits `s` at every top-level `sep`, i.e. one that is neither inside a quoted string nor
/// inside `<...>`. Segments are trimmed and empty segments are kept (callers decide).
pub fn split_top_level(s: &str, sep: char) -> Vec<&str> {
    let mut out = Vec::new();
    let mut start = 0;
    let mut angle = false;
    let mut iter = s.char_indices().peekable();
    while let Some((i, c)) = iter.next() {
        match c {
            '"' => {
                // skip the quoted string
                if let Some(end) = scan_quoted(s, i) {
                    while let Some(&(j, _)) = iter.peek() {
                        if j < end {
                            iter.next();
                        } else {
                            break;
                        }
                    }
                } else {
                    // unterminated quote: the rest of the value is one segment
                    break;
                }
            }
            '<' => angle = true,
            '>' => angle = false,
            c if c == sep && !angle => {
                out.push(s[start..i].trim());
                start = i + c.len_utf8();
            }
            _ => {}
        }
    }
    out.push(s[start..].trim());
    out
}

/// Parses `;a=b;c;d="x;y"` (the text must start at the first `;` or be empty/whitespace).
///
/// Whitespace around `;` and `=` is allowed. Empty parameter names or values, and unterminated
/// quoted values are errors.
pub fn parse_params(s: &str, what: &'static str) -> Result<Vec<Param>, ParseError> {
    let s = s.trim();
    if s.is_empty() {
        return Ok(Vec::new());
    }
    let Some(rest) = s.strip_prefix(';') else {
        return Err(ParseError::invalid_header(
            what,
            "garbage where ';' expected",
        ));
    };
    let mut params = Vec::new();
    for seg in split_top_level(rest, ';') {
        if seg.is_empty() {
            return Err(ParseError::invalid_header(what, "empty parameter"));
        }
        let (name, value) = match seg.split_once('=') {
            Some((n, v)) => (n.trim_end(), Some(v.trim_start())),
            None => (seg, None),
        };
        if !is_token(name) {
            return Err(ParseError::invalid_header(what, "invalid parameter name"));
        }
        if let Some(v) = value {
            if v.is_empty() {
                return Err(ParseError::invalid_header(what, "empty parameter value"));
            }
            if v.starts_with('"') {
                if scan_quoted(v, 0) != Some(v.len()) {
                    return Err(ParseError::invalid_header(
                        what,
                        "bad quoted parameter value",
                    ));
                }
            } else if v
                .chars()
                .any(|c| c.is_ascii_whitespace() || c.is_ascii_control())
            {
                return Err(ParseError::invalid_header(
                    what,
                    "whitespace in parameter value",
                ));
            }
        }
        params.push(Param {
            name: name.to_string(),
            value: value.map(str::to_string),
        });
    }
    Ok(params)
}

/// Writes `;name[=value]` for every parameter.
pub fn write_params(f: &mut fmt::Formatter<'_>, params: &[Param]) -> fmt::Result {
    for p in params {
        write!(f, ";{p}")?;
    }
    Ok(())
}
