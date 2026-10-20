//! SpanDSP-based fax simulation toolkit (test tooling only, never shipped).
//!
//! * [`tiff_writer`] / [`tiff_reader`]: independent minimal TIFF implementations.
//! * [`spandsp`]: safe RAII wrappers over `spandsp-sys`.
//! * [`sip_uac`]: a simulated gateway placing a call into `sip-faxd` through sip-core (M1d).
//! * [`t38_loopback`] / [`audio_loopback`]: deterministic in-process fax sessions
//!   driven by a simulated clock (no sleeping).

pub mod audio_loopback;
pub mod sip_uac;
pub mod spandsp;
pub mod t38_loopback;
pub mod tiff_reader;
pub mod tiff_writer;
mod util;

pub use util::{Fnv64, SplitMix64};

/// Samples per simulation step (20 ms at 8 kHz).
pub const STEP_SAMPLES: usize = 160;
/// Milliseconds per simulation step.
pub const STEP_MS: u64 = 20;

/// Result of one simulated fax session (T.38 or audio).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SessionResult {
    /// Phase E completion code of the calling (transmitting) side, if reported.
    pub caller_code: Option<i32>,
    /// Phase E completion code of the answering (receiving) side, if reported.
    pub answerer_code: Option<i32>,
    /// Simulated duration in milliseconds.
    pub sim_ms: u64,
    /// True if the session hit `max_sim_ms` before both sides completed.
    pub timed_out: bool,
    /// Packet copies emitted by the terminals (T.38 only; 0 for audio).
    pub packets_sent: u64,
    /// Packet copies discarded by loss injection.
    pub packets_dropped: u64,
    /// Hash over the full packet/sample exchange; equal for identical runs.
    pub packet_hash: u64,
}
