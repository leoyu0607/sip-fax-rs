//! Inputs and outputs of the receiver state machine.

use crate::caps::{Compression, Modem, Resolution};
use crate::result::T30Result;

/// The four T.30 timers the receiver uses.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Timer {
    /// T1: identification (35 s).
    T1,
    /// T2: waiting for the next command or for the image (6 s).
    T2,
    /// T4: waiting for a response, here the DIS retransmission interval (3 s).
    T4,
    /// Silence in the middle of an image (not a T.30 timer; SpanDSP's `MID_RX_TIMEOUT`).
    Data,
}

impl Timer {
    /// All timers.
    pub const ALL: [Timer; 4] = [Timer::T1, Timer::T2, Timer::T4, Timer::Data];

    pub(crate) fn index(self) -> usize {
        match self {
            Timer::T1 => 0,
            Timer::T2 => 1,
            Timer::T4 => 2,
            Timer::Data => 3,
        }
    }
}

/// A signal heard from the peer (a T.38 indicator, as seen by the T.30 layer).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RxSignal {
    /// Calling tone.
    Cng,
    /// Answer tone.
    Ced,
    /// V.21 channel 2 preamble: an HDLC burst follows.
    V21Preamble,
    /// Any high speed modem training: image (or TCF) data follows.
    Training,
    /// The signal ended.
    NoSignal,
}

/// A signal this side asks the front end to send.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TxSignal {
    /// Answer tone (CED), 2.6 to 4 s, followed by 75 ms of silence.
    Ced,
}

/// Events accepted by [`crate::Receiver::on_event`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// The call was answered: start phase A / B.
    Start,
    /// An HDLC frame (address, control, FCF, FIF; FCS stripped) with the verdict of its FCS.
    Frame {
        /// The frame octets.
        data: Vec<u8>,
        /// False for a frame received with a bad FCS (or with lost octets).
        fcs_ok: bool,
    },
    /// A signal from the peer.
    Signal(RxSignal),
    /// Non-ECM image (or TCF) data, in T.4 code order (first code bit is the most significant).
    ImageData(Vec<u8>),
    /// The image / TCF signal ended.
    DataEnd,
    /// The front end finished transmitting everything requested by a [`Action::SendFrames`].
    TxComplete,
    /// A timer started with [`Action::StartTimer`] expired.
    Timer(Timer),
    /// The upper layer ends the call (hangup, error).
    Abort,
}

/// A finished page, ready to be stored.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageData {
    /// 1-based page number within the call.
    pub number: u32,
    /// Pixels per row.
    pub width: u32,
    /// Rows.
    pub rows: u32,
    /// Vertical resolution.
    pub resolution: Resolution,
    /// Horizontal resolution in dpi.
    pub x_dpi: u32,
    /// Vertical resolution in dpi.
    pub y_dpi: u32,
    /// Coding the page arrived in.
    pub compression: Compression,
    /// Modem it arrived over.
    pub modem: Modem,
    /// Packed 1-bit rows, 1 = black, `ceil(width / 8)` bytes per row.
    pub bitmap: Vec<u8>,
    /// Rows that failed to decode and were replaced.
    pub bad_lines: u32,
    /// Longest run of consecutive bad rows.
    pub longest_bad_run: u32,
    /// Why decoding of the page stopped.
    pub end: t4::EndReason,
}

/// Actions emitted by [`crate::Receiver::on_event`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Send a signal.
    SendSignal(TxSignal),
    /// Send these HDLC frames as one V.21 burst (the last one is the final frame). The front end
    /// reports [`Event::TxComplete`] when the burst has left.
    SendFrames(Vec<Vec<u8>>),
    /// (Re)start a timer.
    StartTimer {
        /// Which timer.
        timer: Timer,
        /// Duration in milliseconds.
        ms: u32,
    },
    /// Stop a timer.
    CancelTimer(Timer),
    /// A page was accepted.
    Page(Box<PageData>),
    /// A diagnostic line (unexpected frames, ignored input).
    Log(String),
    /// The call is over.
    Done(Box<T30Result>),
}
