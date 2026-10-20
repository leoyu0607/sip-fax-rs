//! `CSeq` header (RFC 3261 section 20.16).

use std::fmt;

use super::Method;
use super::lex::is_token;
use super::parse::ParseError;

/// A parsed CSeq header value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CSeq {
    /// Sequence number, `0 ..= 2^32 - 1`.
    pub seq: u32,
    /// Method.
    pub method: Method,
}

impl CSeq {
    /// Parses `"<digits> <method>"`. Leading zeros are accepted; values above `2^32-1`,
    /// signs, and non-digits are rejected (RFC 4475 3.1.2.4).
    pub fn parse(s: &str) -> Result<Self, ParseError> {
        let bad = |r: &str| ParseError::invalid_header("CSeq", r);
        let mut it = s.split_ascii_whitespace();
        let (Some(num), Some(method), None) = (it.next(), it.next(), it.next()) else {
            return Err(bad("expected '<number> <method>'"));
        };
        if !num.bytes().all(|b| b.is_ascii_digit()) {
            return Err(bad("sequence number is not a decimal number"));
        }
        let seq = num
            .parse::<u32>()
            .map_err(|_| bad("sequence number out of range"))?;
        if !is_token(method) {
            return Err(bad("invalid method token"));
        }
        Ok(CSeq {
            seq,
            method: Method::from_token(method),
        })
    }
}

impl fmt::Display for CSeq {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.seq, self.method)
    }
}
