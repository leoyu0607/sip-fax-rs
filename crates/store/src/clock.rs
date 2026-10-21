//! Injectable clock (retention, optimistic locks and stale-row repair never call `Utc::now()` directly).

use chrono::{DateTime, Duration, TimeZone, Utc};
use std::sync::Mutex;

/// Time source.
pub trait Clock: Send + Sync {
    /// Current time.
    fn now(&self) -> DateTime<Utc>;
}

/// Wall clock.
#[derive(Debug, Default, Clone, Copy)]
pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> DateTime<Utc> {
        Utc::now()
    }
}

/// Manually driven clock for tests.
#[derive(Debug)]
pub struct ManualClock(Mutex<DateTime<Utc>>);

impl ManualClock {
    /// Start at `t`.
    pub fn new(t: DateTime<Utc>) -> Self {
        Self(Mutex::new(t))
    }
    /// Start at the given unix milliseconds.
    pub fn at_millis(ms: i64) -> Self {
        Self::new(Utc.timestamp_millis_opt(ms).single().expect("valid millis"))
    }
    /// Jump to `t`.
    pub fn set(&self, t: DateTime<Utc>) {
        *self.0.lock().unwrap() = t;
    }
    /// Advance by `d`.
    pub fn advance(&self, d: Duration) {
        let mut g = self.0.lock().unwrap();
        *g += d;
    }
}

impl Clock for ManualClock {
    fn now(&self) -> DateTime<Utc> {
        *self.0.lock().unwrap()
    }
}
