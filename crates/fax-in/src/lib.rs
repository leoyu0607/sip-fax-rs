//! Inbound fax call control: SIP dialog <-> T.38/audio <-> T.30 <-> document.
//!
//! M1d provides the call-control skeleton (no fax engine yet):
//!
//! * [`config`]: TOML configuration with trunk profiles and presets;
//! * [`trunk`]: source IP -> trunk mapping and the transport-level source filter;
//! * [`controller`]: [`FaxIn`], answering G.711 / T.38 and establishing T.38 by
//!   `t38_policy`, then handing the call to a [`session::FaxSession`];
//! * [`event`]: structured call events and results (persisted from M3);
//! * [`capture`]: per-call SIP capture under `logs/calls/<call-id>/`.

#![deny(missing_docs)]

pub mod capture;
pub mod config;
pub mod controller;
pub mod event;
pub mod media;
pub mod session;
mod timefmt;
pub mod trunk;

pub use config::{ConfigError, FaxInConfig};
pub use controller::{FaxIn, StartError, StartOptions};
pub use event::{CallEvent, CallOutcome, CallResult, FailureReason, T38Negotiated, T38Path};
pub use timefmt::rfc3339;
