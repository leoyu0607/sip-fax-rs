//! G.711 passthrough fax modems (ADR-061): tones, and later V.21 / HDLC, V.27ter and V.29.
//!
//! Pure signal processing on 8 kHz 16-bit linear samples: no I/O, no clock, no dependency on any
//! other fax crate. Every receiver takes any input without panicking.
//!
//! * [`level`]: dBm0 and signal power.
//! * [`goertzel`]: single-frequency power.
//! * [`tone`]: the CNG detector, tone generators (CED), the signal-present detector.
//!
//! Written from the ITU-T recommendations (V.21, V.27ter, V.29, T.30, G.711); no SpanDSP code is
//! used. SpanDSP serves as the independent counterpart in the tests only.

#![deny(missing_docs)]
#![cfg_attr(
    not(test),
    deny(
        clippy::unwrap_used,
        clippy::expect_used,
        clippy::panic,
        clippy::unreachable
    )
)]

pub mod goertzel;
pub mod level;
pub mod tone;

/// The sample rate of everything in this crate (G.711).
pub const SAMPLE_RATE: u32 = 8000;
