//! Tiny RFC 3339 (UTC, millisecond) formatter so that no date/time crate is needed.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

/// `2026-10-20T12:34:56.789Z`
pub fn rfc3339(t: SystemTime) -> String {
    let d = t.duration_since(UNIX_EPOCH).unwrap_or(Duration::ZERO);
    let secs = d.as_secs();
    let ms = d.subsec_millis();
    let days = i64::try_from(secs / 86_400).unwrap_or(0);
    let rem = secs % 86_400;
    let (y, m, day) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{day:02}T{:02}:{:02}:{:02}.{ms:03}Z",
        rem / 3600,
        (rem % 3600) / 60,
        rem % 60
    )
}

/// The UTC calendar date `(year, month, day)` of `t`.
pub(crate) fn ymd(t: SystemTime) -> (i64, u32, u32) {
    let secs = t
        .duration_since(UNIX_EPOCH)
        .unwrap_or(Duration::ZERO)
        .as_secs();
    civil_from_days(i64::try_from(secs / 86_400).unwrap_or(0))
}

/// Days since 1970-01-01 to (year, month, day), proleptic Gregorian (Howard Hinnant).
fn civil_from_days(z: i64) -> (i64, u32, u32) {
    let z = z + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = u32::try_from(doy - (153 * mp + 2) / 5 + 1).unwrap_or(1);
    let m = u32::try_from(if mp < 10 { mp + 3 } else { mp - 9 }).unwrap_or(1);
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// Serde helper: `SystemTime` as an RFC 3339 string.
pub fn serialize_time<S: serde::Serializer>(t: &SystemTime, s: S) -> Result<S::Ok, S::Error> {
    s.serialize_str(&rfc3339(*t))
}

/// Serde helper for `Option<SystemTime>`.
pub fn serialize_opt_time<S: serde::Serializer>(
    t: &Option<SystemTime>,
    s: S,
) -> Result<S::Ok, S::Error> {
    match t {
        Some(t) => s.serialize_str(&rfc3339(*t)),
        None => s.serialize_none(),
    }
}

/// Parses the output of [`rfc3339`] (and any other RFC 3339 instant).
pub fn parse_rfc3339(text: &str) -> Option<SystemTime> {
    let dt = chrono::DateTime::parse_from_rfc3339(text).ok()?;
    let ms = dt.timestamp_millis();
    if ms >= 0 {
        Some(UNIX_EPOCH + Duration::from_millis(u64::try_from(ms).ok()?))
    } else {
        UNIX_EPOCH.checked_sub(Duration::from_millis(ms.unsigned_abs()))
    }
}

/// Serde helper: `SystemTime` from an RFC 3339 string.
pub fn deserialize_time<'de, D: serde::Deserializer<'de>>(d: D) -> Result<SystemTime, D::Error> {
    let s = <String as serde::Deserialize>::deserialize(d)?;
    parse_rfc3339(&s).ok_or_else(|| serde::de::Error::custom(format!("bad RFC 3339 time {s:?}")))
}

/// Serde helper for `Option<SystemTime>`.
pub fn deserialize_opt_time<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<SystemTime>, D::Error> {
    match <Option<String> as serde::Deserialize>::deserialize(d)? {
        None => Ok(None),
        Some(s) => parse_rfc3339(&s)
            .map(Some)
            .ok_or_else(|| serde::de::Error::custom(format!("bad RFC 3339 time {s:?}"))),
    }
}

#[cfg(test)]
mod tests;
