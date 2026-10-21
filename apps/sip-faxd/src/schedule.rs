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
mod tests {
    use super::*;
    use chrono::NaiveDate;
    use std::cell::RefCell;
    use std::rc::Rc;

    fn at(d: u32, h: u32, m: u32, s: u32) -> NaiveDateTime {
        NaiveDate::from_ymd_opt(2026, 10, d)
            .unwrap()
            .and_hms_opt(h, m, s)
            .unwrap()
    }

    #[test]
    fn run_at_is_hh_mm() {
        assert_eq!(parse_run_at("03:17"), NaiveTime::from_hms_opt(3, 17, 0));
        assert_eq!(parse_run_at("00:00"), NaiveTime::from_hms_opt(0, 0, 0));
        assert_eq!(parse_run_at("23:59"), NaiveTime::from_hms_opt(23, 59, 0));
        for bad in [
            "", "3:17", "03:7", "24:00", "03:60", "0317", "03:17:00", "ab:cd",
        ] {
            assert_eq!(parse_run_at(bad), None, "{bad}");
        }
    }

    #[test]
    fn the_next_run_is_strictly_in_the_future() {
        let run = NaiveTime::from_hms_opt(3, 17, 0).unwrap();
        // before today's time: today
        assert_eq!(next_run(at(21, 2, 0, 0), run), at(21, 3, 17, 0));
        // one second before: today; exactly at it and after it: tomorrow
        assert_eq!(next_run(at(21, 3, 16, 59), run), at(21, 3, 17, 0));
        assert_eq!(next_run(at(21, 3, 17, 0), run), at(22, 3, 17, 0));
        assert_eq!(next_run(at(21, 23, 59, 59), run), at(22, 3, 17, 0));
        // month change
        assert_eq!(
            next_run(at(31, 12, 0, 0), run),
            NaiveDate::from_ymd_opt(2026, 11, 1)
                .unwrap()
                .and_hms_opt(3, 17, 0)
                .unwrap()
        );
        assert_eq!(
            wait_until_next(at(21, 3, 0, 0), run),
            Duration::from_secs(17 * 60)
        );
        assert_eq!(
            wait_until_next(at(21, 3, 17, 0), run),
            Duration::from_secs(24 * 3600)
        );
    }

    /// Drives `daily` on a virtual wall clock: `sleep` advances it, nothing really waits.
    #[tokio::test(start_paused = true)]
    async fn the_job_runs_once_a_day_at_the_configured_time() {
        let clock = Rc::new(RefCell::new(at(21, 12, 0, 0)));
        let runs: Rc<RefCell<Vec<NaiveDateTime>>> = Rc::default();
        let run_at = Rc::new(RefCell::new(NaiveTime::from_hms_opt(3, 17, 0).unwrap()));
        let limit = 4;
        let c = clock.clone();
        let c2 = clock.clone();
        let r = runs.clone();
        let ra = run_at.clone();
        let ra2 = run_at.clone();
        let task = daily(
            move || *c.borrow(),
            move |d| {
                let c = c2.clone();
                async move {
                    *c.borrow_mut() += CDuration::from_std(d).unwrap();
                }
            },
            move || {
                let t = *ra.borrow();
                async move { t }
            },
            || {
                let (c, r, ra) = (clock.clone(), r.clone(), ra2.clone());
                async move {
                    r.borrow_mut().push(*c.borrow());
                    // the operator changes the time in the database after the second run
                    if r.borrow().len() == 2 {
                        *ra.borrow_mut() = NaiveTime::from_hms_opt(22, 30, 0).unwrap();
                    }
                    if r.borrow().len() == limit {
                        std::future::pending::<()>().await;
                    }
                }
            },
        );
        // `daily` never returns: it parks after the fourth run
        let _ = tokio::time::timeout(Duration::from_secs(5), task).await;
        let runs = runs.borrow();
        assert_eq!(runs.len(), limit);
        assert_eq!(runs[0], at(22, 3, 17, 0));
        assert_eq!(runs[1], at(23, 3, 17, 0));
        // the new time is used from the next wait on
        assert_eq!(runs[2], at(23, 22, 30, 0));
        assert_eq!(runs[3], at(24, 22, 30, 0));
    }
}
