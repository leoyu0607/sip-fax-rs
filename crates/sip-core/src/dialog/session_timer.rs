//! Session timers (RFC 4028): header parsing and the UAS-side negotiation. Pure functions; the
//! timers themselves run in the dialog task.

use std::time::Duration;

use crate::message::Request;

/// Which side refreshes the session.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Refresher {
    /// The UAC of the (initial) INVITE.
    Uac,
    /// The UAS of the (initial) INVITE.
    Uas,
}

impl Refresher {
    /// Token used in the `refresher` parameter.
    pub fn as_str(self) -> &'static str {
        match self {
            Refresher::Uac => "uac",
            Refresher::Uas => "uas",
        }
    }
}

/// A parsed `Session-Expires` header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SessionExpires {
    /// Session interval in seconds.
    pub delta: u32,
    /// The `refresher` parameter, when present.
    pub refresher: Option<Refresher>,
}

/// Parses `1800;refresher=uac` (also the compact header `x`, which the message layer
/// normalizes to `Session-Expires`).
pub fn parse_session_expires(v: &str) -> Option<SessionExpires> {
    let mut parts = v.split(';');
    let delta = parts.next()?.trim().parse::<u32>().ok()?;
    let mut refresher = None;
    for p in parts {
        if let Some((n, val)) = p.split_once('=') {
            if n.trim().eq_ignore_ascii_case("refresher") {
                refresher = match val.trim().to_ascii_lowercase().as_str() {
                    "uac" => Some(Refresher::Uac),
                    "uas" => Some(Refresher::Uas),
                    _ => None,
                };
            }
        }
    }
    Some(SessionExpires { delta, refresher })
}

/// Settings of the session-timer machinery of an endpoint.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionTimerConfig {
    /// Honour `Session-Expires` in incoming INVITEs (and advertise `Supported: timer`).
    pub enabled: bool,
    /// Smallest session interval we accept (seconds); smaller requests are answered with 422.
    pub min_se: u32,
    /// Refresher we pick when the request leaves it open.
    pub default_refresher: Refresher,
}

impl Default for SessionTimerConfig {
    fn default() -> Self {
        SessionTimerConfig {
            enabled: true,
            min_se: 90,
            default_refresher: Refresher::Uac,
        }
    }
}

/// Outcome of evaluating the session-timer headers of an incoming INVITE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Negotiation {
    /// No session timer for this dialog.
    None,
    /// The requested interval is below our minimum: answer 422 with `Min-SE`.
    TooSmall {
        /// Our minimum, for the `Min-SE` header.
        min_se: u32,
    },
    /// The session is refreshed every `interval` seconds by `refresher`.
    Active {
        /// Interval in seconds.
        interval: u32,
        /// Who refreshes (from the point of view of the INVITE: uac = the caller).
        refresher: Refresher,
    },
}

fn lists_token(req: &Request, header: &str, token: &str) -> bool {
    req.headers
        .get_all(header)
        .flat_map(|v| v.split(','))
        .any(|t| t.trim().eq_ignore_ascii_case(token))
}

/// UAS-side negotiation (RFC 4028 section 9): honours `Session-Expires` and `refresher` of the
/// request; if the caller does not support timers we must refresh ourselves.
pub fn negotiate_uas(req: &Request, cfg: &SessionTimerConfig) -> Negotiation {
    if !cfg.enabled {
        return Negotiation::None;
    }
    let Some(se) = req
        .headers
        .get("Session-Expires")
        .and_then(parse_session_expires)
    else {
        return Negotiation::None;
    };
    if se.delta < cfg.min_se {
        return Negotiation::TooSmall { min_se: cfg.min_se };
    }
    let peer_supports =
        lists_token(req, "Supported", "timer") || lists_token(req, "Require", "timer");
    let refresher = if peer_supports {
        se.refresher.unwrap_or(cfg.default_refresher)
    } else {
        Refresher::Uas
    };
    Negotiation::Active {
        interval: se.delta,
        refresher,
    }
}

/// When the refresher sends the refresh: half of the interval.
pub fn refresh_after(interval: u32) -> Duration {
    Duration::from_secs(u64::from(interval)) / 2
}

/// When the refreshee gives up (RFC 4028 section 10): `interval - min(32, interval / 3)`.
pub fn expire_after(interval: u32) -> Duration {
    let margin = (interval / 3).min(32);
    Duration::from_secs(u64::from(interval - margin))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_session_expires() {
        let se = parse_session_expires("1800 ; refresher=UAS").unwrap();
        assert_eq!(se.delta, 1800);
        assert_eq!(se.refresher, Some(Refresher::Uas));
        assert_eq!(parse_session_expires("90").unwrap().refresher, None);
        assert_eq!(
            parse_session_expires("90;refresher=bogus")
                .unwrap()
                .refresher,
            None
        );
        assert!(parse_session_expires("abc").is_none());
    }

    #[test]
    fn schedule_arithmetic() {
        assert_eq!(refresh_after(90), Duration::from_secs(45));
        assert_eq!(expire_after(90), Duration::from_secs(60)); // 90 - min(32, 30)
        assert_eq!(expire_after(1800), Duration::from_secs(1768)); // 1800 - 32
    }
}
