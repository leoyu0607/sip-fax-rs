//! The daily retention run (`retention.run_at`, server local time).

use std::future::Future;
use std::time::Duration;

use chrono::{Duration as CDuration, NaiveDateTime, NaiveTime};

/// Parses `HH:MM`.
pub fn parse_run_at(s: &str) -> Option<NaiveTime> {
    let (h, m) = s.split_once(':')?;
    if h.len() != 2 || m.len() != 2 {
        return None;
    }
    NaiveTime::from_hms_opt(h.parse().ok()?, m.parse().ok()?, 0)
}

/// The next instant after `now` (strictly) at which the clock reads `run_at`.
pub fn next_run(now: NaiveDateTime, run_at: NaiveTime) -> NaiveDateTime {
    let today = now.date().and_time(run_at);
    if today > now {
        today
    } else {
        today + CDuration::days(1)
    }
}

/// How long to wait from `now` until the next run.
pub fn wait_until_next(now: NaiveDateTime, run_at: NaiveTime) -> Duration {
    (next_run(now, run_at) - now)
        .to_std()
        .unwrap_or(Duration::ZERO)
}

/// Runs `job` every day at the time `run_at()` names. `now` is the local wall clock and `sleep`
/// the way to wait (both injected: the tests drive them with a virtual clock). The run time is
/// asked for again before every wait, so a change in the database is picked up the next day.
/// Never returns.
pub async fn daily<N, S, SF, R, RF, J, JF>(now: N, sleep: S, run_at: R, mut job: J)
where
    N: Fn() -> NaiveDateTime,
    S: Fn(Duration) -> SF,
    SF: Future<Output = ()>,
    R: Fn() -> RF,
    RF: Future<Output = NaiveTime>,
    J: FnMut() -> JF,
    JF: Future<Output = ()>,
{
    loop {
        let at = run_at().await;
        let wait = wait_until_next(now(), at);
        sleep(wait).await;
        job().await;
        // Waking a hair early (timer granularity) must not run the job twice: step past the minute.
        sleep(Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests;
