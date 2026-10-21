//! T.30 fax procedure, receiving (answering) terminal, with optional ECM (T.30 Annex A).
//!
//! The crate is pure: it performs no I/O, owns no clock and never panics on any input.
//!
//! * [`frame`]: HDLC frame layout, FCF values, identification strings.
//! * [`caps`]: DIS generation and DIS / DCS parsing (T.30 Table 2).
//! * [`receiver`]: the state machine, [`Receiver::on_event`].
//! * [`event`]: [`Event`] / [`Action`] / [`Timer`] and the finished [`PageData`].
//! * [`result`]: the [`T30Result`] and its [`ResultCode`].
//! * [`config`]: capabilities, thresholds and timers.
//!
//! Image decoding is delegated to the `t4` crate; this crate has no dependency on the T.38
//! transport, so the same machine can later be driven by a G.711 modem front end.

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

pub mod caps;
pub mod config;
pub mod event;
pub mod frame;
pub mod receiver;
pub mod result;

pub use caps::{
    Compression, Dcs, DcsError, Dis, Modem, Resolution, Unsupported, build_dis_fif, parse_rate,
};
pub use config::{Config, MinScanTime, Modems, PaperLength, PaperWidth, TcfMode, Timers};
pub use event::{Action, Event, PageData, RxSignal, Timer, TxSignal};
pub use frame::{Frame, FrameError, decode_ident, encode_ident, fcf, fcf2, fif_bit, lsb_first};
pub use receiver::{Receiver, State};
pub use result::{ResultCode, T30Result};
