//! Login protection (docs/design/m4-web.md section 2.3): an account is locked after repeated
//! failures, and one client address may only try so often. The state lives in memory (a restart
//! clears it) and every decision uses the injected clock.
//!
//! Unknown account names are counted like known ones, so that "locked" does not tell an attacker
//! which names exist.

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use chrono::{DateTime, Duration, Utc};
use store::Clock;

/// Limits.
#[derive(Debug, Clone, Copy)]
pub struct GuardConfig {
    /// Consecutive failures that lock an account (default 5).
    pub max_failures: u32,
    /// How long it stays locked (default 15 minutes).
    pub lockout: Duration,
    /// Attempts per client address in `ip_window` (default 20).
    pub ip_attempts: usize,
    /// The window of `ip_attempts` (default one minute).
    pub ip_window: Duration,
}

impl Default for GuardConfig {
    fn default() -> Self {
        GuardConfig {
            max_failures: 5,
            lockout: Duration::minutes(15),
            ip_attempts: 20,
            ip_window: Duration::minutes(1),
        }
    }
}

/// Why a login attempt is not even looked at.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Block {
    /// The client address made too many attempts (HTTP 429).
    RateLimited {
        /// Seconds until it may try again.
        retry_after_secs: i64,
    },
    /// The account is locked.
    Locked {
        /// Seconds until it unlocks.
        retry_after_secs: i64,
    },
}

#[derive(Default)]
struct UserState {
    failures: u32,
    locked_until: Option<DateTime<Utc>>,
}

#[derive(Default)]
struct Inner {
    users: HashMap<String, UserState>,
    ips: HashMap<String, VecDeque<DateTime<Utc>>>,
}

/// The guard.
pub struct LoginGuard {
    cfg: GuardConfig,
    clock: Arc<dyn Clock>,
    inner: Mutex<Inner>,
}

const MAX_TRACKED: usize = 10_000;

impl LoginGuard {
    /// A guard with the given limits.
    pub fn new(cfg: GuardConfig, clock: Arc<dyn Clock>) -> LoginGuard {
        LoginGuard {
            cfg,
            clock,
            inner: Mutex::new(Inner::default()),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Called before a password is checked: counts the attempt for the client address and
    /// refuses when the address is over its limit or the account is locked.
    ///
    /// # Errors
    /// [`Block`].
    pub fn check(&self, username: &str, ip: &str) -> Result<(), Block> {
        let now = self.clock.now();
        let mut g = self.lock();
        if g.ips.len() > MAX_TRACKED {
            let cutoff = now - self.cfg.ip_window;
            g.ips.retain(|_, w| w.back().is_some_and(|t| *t > cutoff));
        }
        let w = g.ips.entry(ip.to_string()).or_default();
        while w.front().is_some_and(|t| now - *t >= self.cfg.ip_window) {
            w.pop_front();
        }
        if w.len() >= self.cfg.ip_attempts {
            let oldest = *w.front().expect("the window is not empty");
            let wait = (oldest + self.cfg.ip_window - now).num_seconds().max(1);
            return Err(Block::RateLimited {
                retry_after_secs: wait,
            });
        }
        w.push_back(now);
        if let Some(u) = g.users.get_mut(username)
            && let Some(until) = u.locked_until
        {
            if now < until {
                return Err(Block::Locked {
                    retry_after_secs: (until - now).num_seconds().max(1),
                });
            }
            // the lock has run out: a fresh start
            u.locked_until = None;
            u.failures = 0;
        }
        Ok(())
    }

    /// A wrong password (or an unknown account).
    pub fn record_failure(&self, username: &str) {
        let now = self.clock.now();
        let mut g = self.lock();
        if g.users.len() > MAX_TRACKED {
            g.users
                .retain(|_, u| u.locked_until.is_some_and(|t| t > now));
        }
        let u = g.users.entry(username.to_string()).or_default();
        u.failures += 1;
        if u.failures >= self.cfg.max_failures {
            u.locked_until = Some(now + self.cfg.lockout);
        }
    }

    /// A correct password: the failure count starts over.
    pub fn record_success(&self, username: &str) {
        self.lock().users.remove(username);
    }

    /// Forgets the failures of an account (an administrator reset its password).
    pub fn reset(&self, username: &str) {
        self.record_success(username);
    }
}
