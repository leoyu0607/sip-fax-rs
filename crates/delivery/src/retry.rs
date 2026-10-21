//! Retry schedule (spec `docs/design/m3-store.md` section 9).

use chrono::Duration;

/// Default `max_attempts`.
pub const DEFAULT_MAX_ATTEMPTS: u32 = 10;

/// Delay before the next attempt after attempt number `failed_attempt` (1-based) failed:
/// 1, 5, 15, 60 minutes, then every 4 hours.
pub fn backoff(failed_attempt: u32) -> Duration {
    match failed_attempt {
        0 | 1 => Duration::minutes(1),
        2 => Duration::minutes(5),
        3 => Duration::minutes(15),
        4 => Duration::minutes(60),
        _ => Duration::hours(4),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn d05_backoff_table() {
        let got: Vec<i64> = (1..=7).map(|n| backoff(n).num_minutes()).collect();
        assert_eq!(got, vec![1, 5, 15, 60, 240, 240, 240]);
        assert_eq!(backoff(0).num_minutes(), 1);
    }
}
