//! Calling / called number (ANI / DNIS) extraction from the INVITE (docs/design/m3-store.md
//! section 2).
//!
//! * ANI: the first of `ani_sources` (`pai` = P-Asserted-Identity, `rpid` = Remote-Party-ID,
//!   `from`) that carries a non-empty user part.
//! * DNIS: the first of `dnis_sources` (`ruri` = Request-URI, `to`, `diversion`). For
//!   `diversion` the *last* entry counts: RFC 5806 puts the most recent diversion first, so
//!   the last one is the number that was originally dialled.
//! * A `tel:` URI yields its number, a `sip:` / `sips:` URI its user part without the
//!   `;parameters`.
//! * Normalisation: visual separators (`-`, `.`, blanks, parentheses) are removed, then
//!   `*_strip_prefix` is removed (a prefix without `+` also matches a number written with
//!   `+`), then `strip_plus`.
//! * A caller who hides the number (`anonymous` user, `Privacy: id`, or `privacy=full` on the
//!   Remote-Party-ID) gets `ani = None`, `ani_source = "anonymous"`.

use serde::{Deserialize, Serialize};
use sip_core::message::{Request, SipUri, Uri, UriScheme, unescape};

use crate::config::NumbersSection;

/// Calling and called number of a call.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Numbers {
    /// Calling number; `None` when absent or hidden.
    pub ani: Option<String>,
    /// Where the ANI came from: `pai`, `rpid`, `from`, or `anonymous`.
    pub ani_source: Option<String>,
    /// Called number.
    pub dnis: Option<String>,
    /// Where the DNIS came from: `ruri`, `to`, or `diversion`.
    pub dnis_source: Option<String>,
}

/// Source label of a hidden calling number.
pub const ANONYMOUS: &str = "anonymous";

/// Splits a header value at the commas outside `<...>` and quotes.
fn split_values(v: &str) -> Vec<&str> {
    let mut out = Vec::new();
    let (mut depth, mut quoted, mut start) = (0u32, false, 0usize);
    let mut prev = '\0';
    for (i, c) in v.char_indices() {
        match c {
            '"' if prev != '\\' => quoted = !quoted,
            '<' if !quoted => depth += 1,
            '>' if !quoted => depth = depth.saturating_sub(1),
            ',' if !quoted && depth == 0 => {
                out.push(v[start..i].trim());
                start = i + 1;
            }
            _ => {}
        }
        prev = c;
    }
    out.push(v[start..].trim());
    out.retain(|s| !s.is_empty());
    out
}

/// The URI inside one name-addr / addr-spec header element, plus its header parameters.
/// Lower-cased header parameter name and its value.
type HeaderParams = Vec<(String, Option<String>)>;

fn parse_element(text: &str) -> Option<(Uri, HeaderParams)> {
    let na = sip_core::message::NameAddr::parse(text).ok()?;
    let params = na
        .params
        .iter()
        .map(|p| (p.name.to_ascii_lowercase(), p.value.clone()))
        .collect();
    Some((na.uri, params))
}

/// The raw (unnormalised) number a URI carries, or `None` for an empty / non-SIP URI.
fn user_of(uri: &Uri) -> Option<String> {
    let Uri::Sip(u) = uri else { return None };
    user_of_sip(u)
}

fn user_of_sip(u: &SipUri) -> Option<String> {
    let raw = u.user.as_deref()?;
    let decoded = String::from_utf8_lossy(&unescape(raw)).into_owned();
    // `sip:` users may carry `;parameters` (e.g. `;phone-context=`); a `tel:` URI never does
    // (its parser already split them off).
    let user = if u.scheme == UriScheme::Tel {
        decoded.as_str()
    } else {
        decoded.split(';').next().unwrap_or("")
    };
    let user = user.trim();
    (!user.is_empty()).then(|| user.to_string())
}

/// Removes the visual separators of a number.
fn strip_separators(s: &str) -> String {
    s.chars()
        .filter(|c| !matches!(c, '-' | '.' | ' ' | '(' | ')'))
        .collect()
}

/// Normalises a number (see the module docs).
pub fn normalize(raw: &str, strip_plus: bool, strip_prefix: Option<&str>) -> String {
    let mut n = strip_separators(raw);
    if let Some(p) = strip_prefix.map(strip_separators).filter(|p| !p.is_empty()) {
        let rest = n
            .strip_prefix(p.as_str())
            .or_else(|| {
                (!p.starts_with('+'))
                    .then(|| n.strip_prefix('+').and_then(|t| t.strip_prefix(p.as_str())))
                    .flatten()
            })
            .map(str::to_string);
        // never strip a number down to nothing
        if let Some(rest) = rest.filter(|r| !r.is_empty()) {
            n = rest;
        }
    }
    if strip_plus && let Some(t) = n.strip_prefix('+') {
        n = t.to_string();
    }
    n
}

fn is_anonymous_user(user: &str) -> bool {
    user.eq_ignore_ascii_case(ANONYMOUS)
}

fn privacy_hides_identity(req: &Request) -> bool {
    req.headers
        .get_all("Privacy")
        .flat_map(|v| v.split([';', ',']))
        .any(|t| t.trim().eq_ignore_ascii_case("id"))
}

/// Extracts both numbers of an INVITE according to the trunk's `[trunks.numbers]`.
pub fn extract(req: &Request, cfg: &NumbersSection) -> Numbers {
    let (ani, ani_source) = extract_ani(req, cfg);
    let (dnis, dnis_source) = extract_dnis(req, cfg);
    Numbers {
        ani,
        ani_source,
        dnis,
        dnis_source,
    }
}

fn extract_ani(req: &Request, cfg: &NumbersSection) -> (Option<String>, Option<String>) {
    let hidden = privacy_hides_identity(req);
    for src in &cfg.ani_sources {
        let candidate: Option<(String, bool)> = match src.as_str() {
            "pai" => req
                .headers
                .get_all("P-Asserted-Identity")
                .flat_map(split_values)
                .filter_map(parse_element)
                // a request may carry a sip: and a tel: identity; the first usable one counts
                .find_map(|(u, _)| user_of(&u).map(|n| (n, false))),
            "rpid" => req
                .headers
                .get_all("Remote-Party-ID")
                .flat_map(split_values)
                .filter_map(parse_element)
                .find_map(|(u, params)| {
                    let full = params.iter().any(|(k, v)| {
                        k == "privacy"
                            && matches!(v.as_deref(), Some(p) if p.eq_ignore_ascii_case("full") || p.eq_ignore_ascii_case("uri"))
                    });
                    user_of(&u).map(|n| (n, full))
                }),
            "from" => req
                .headers
                .from()
                .ok()
                .and_then(|na| user_of(&na.uri))
                .map(|n| (n, false)),
            _ => None,
        };
        let Some((raw, hide_flag)) = candidate else {
            continue;
        };
        if hidden || hide_flag || is_anonymous_user(&raw) {
            return (None, Some(ANONYMOUS.to_string()));
        }
        let n = normalize(&raw, cfg.strip_plus, cfg.ani_strip_prefix.as_deref());
        return (Some(n), Some(src.clone()));
    }
    if hidden {
        // nothing usable, but the caller asked for privacy anyway
        return (None, Some(ANONYMOUS.to_string()));
    }
    (None, None)
}

fn extract_dnis(req: &Request, cfg: &NumbersSection) -> (Option<String>, Option<String>) {
    for src in &cfg.dnis_sources {
        let raw = match src.as_str() {
            "ruri" => user_of(&req.uri),
            "to" => req.headers.to().ok().and_then(|na| user_of(&na.uri)),
            "diversion" => req
                .headers
                .get_all("Diversion")
                .flat_map(split_values)
                .filter_map(parse_element)
                .filter_map(|(u, _)| user_of(&u))
                .last(),
            _ => None,
        };
        if let Some(raw) = raw {
            let n = normalize(&raw, cfg.strip_plus, cfg.dnis_strip_prefix.as_deref());
            return (Some(n), Some(src.clone()));
        }
    }
    (None, None)
}

/// Best effort for result files written before the numbers were recorded: the user parts of
/// the `from` and `to` URIs as `sip-faxd` logged them.
pub fn from_uris(from: &str, to: &str) -> Numbers {
    let user = |s: &str| Uri::parse(s).ok().as_ref().and_then(user_of);
    let mut n = Numbers::default();
    if let Some(u) = user(from) {
        if is_anonymous_user(&u) {
            n.ani_source = Some(ANONYMOUS.into());
        } else {
            n.ani = Some(normalize(&u, false, None));
            n.ani_source = Some("from".into());
        }
    }
    if let Some(u) = user(to) {
        n.dnis = Some(normalize(&u, false, None));
        n.dnis_source = Some("to".into());
    }
    n
}
