//! Inbound fax call control: SIP dialog <-> T.38/audio <-> T.30 <-> document.
//!
//! M1d provides the call-control skeleton (no fax engine yet):
//!
//! * [`config`]: TOML configuration with trunk profiles and presets;
//! * [`trunk`]: source IP -> trunk mapping and the transport-level source filter;
//! * [`controller`]: [`FaxIn`], answering G.711 / T.38 and establishing T.38 by
//!   `t38_policy`, then handing the call to a [`session::FaxSession`];
//! * [`ports`]: per-call UDPTL port allocation from `[media] port_range`;
//! * [`event`]: structured call events and results (persisted from M3);
//! * [`capture`]: per-call SIP capture under `logs/calls/<call-id>/`.
//!
//! M2c adds [`t38engine`], the sans-IO T.38 receive engine (IFP in, IFP out, virtual clock)
//! that glues `t38`, `t30` and `t4`. M2d adds [`t38session`], the real [`session::FaxSession`]:
//! a UDPTL socket on the call's pool port, the engine on a real-time clock, and the TIFF / PDF
//! output under `[storage] data_dir`.
//!
//! M5 adds [`audioengine`], the sans-IO G.711 passthrough engine (8 kHz audio in and out, the
//! same T.30 receiver), and [`audiosession`], the session that drives it over RTP on calls that
//! stay on audio.

#![deny(missing_docs)]

pub mod audioengine;
pub mod audiosession;
pub mod capture;
pub mod config;
pub mod controller;
pub mod event;
pub mod media;
pub mod numbers;
pub mod ports;
pub mod recorder;
pub mod session;
pub mod summary;
pub mod t38engine;
pub mod t38session;
mod timefmt;
pub mod trunk;

pub use config::{ConfigError, FaxInConfig, FaxSessionKind};
pub use controller::{DrainProgress, FaxIn, RegistrationView, StartError, StartOptions};
pub use event::{
    AudioNegotiated, ByeBy, CallEvent, CallOutcome, CallResult, FailureReason, FaxMedia, FaxReport,
    ReportSink, RtpCounters, T38Negotiated, T38Path, UdptlCounters,
};
pub use numbers::Numbers;
pub use recorder::{Recorder, RecorderConfig};
pub use summary::{HangupCode, Summary, classify};
pub use timefmt::rfc3339;
