//! Timer identifiers and the configurable time constants (RFC 3261 17, RFC 6026).

use std::time::Duration;

/// A transaction timer. Names follow RFC 3261 table 4 and RFC 6026.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Timer {
    /// INVITE client request retransmit (UDP).
    A,
    /// INVITE client transaction timeout.
    B,
    /// INVITE client wait for response retransmits (Completed).
    D,
    /// non-INVITE client request retransmit (UDP).
    E,
    /// non-INVITE client transaction timeout.
    F,
    /// INVITE server response retransmit (UDP).
    G,
    /// INVITE server wait for ACK.
    H,
    /// INVITE server wait for ACK retransmits (Confirmed).
    I,
    /// non-INVITE server wait for request retransmits (Completed).
    J,
    /// non-INVITE client wait for response retransmits (Completed).
    K,
    /// INVITE server Accepted state (RFC 6026).
    L,
    /// INVITE client Accepted state (RFC 6026).
    M,
    /// INVITE server: automatic `100 Trying` when the TU has not answered.
    Trying,
}

/// Configurable time constants. Defaults are the RFC 3261 values.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TimerConfig {
    /// RTT estimate (500 ms).
    pub t1: Duration,
    /// Maximum retransmit interval (4 s).
    pub t2: Duration,
    /// Maximum duration a message stays in the network (5 s).
    pub t4: Duration,
    /// INVITE server: delay before the automatic 100 Trying (200 ms).
    pub provisional_delay: Duration,
    /// Timer D on unreliable transports (RFC: at least 32 s).
    pub timer_d: Duration,
}

impl Default for TimerConfig {
    fn default() -> Self {
        TimerConfig {
            t1: Duration::from_millis(500),
            t2: Duration::from_secs(4),
            t4: Duration::from_secs(5),
            provisional_delay: Duration::from_millis(200),
            timer_d: Duration::from_secs(32),
        }
    }
}

impl TimerConfig {
    /// `64 * T1`: Timers B, F, H, L, M and J (unreliable).
    pub fn t64(&self) -> Duration {
        self.t1 * 64
    }

    /// Doubles `current` but never beyond T2 (Timers E and G).
    pub fn double_capped(&self, current: Duration) -> Duration {
        (current * 2).min(self.t2)
    }
}
