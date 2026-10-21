//! Transmit side of the T.38 engine: turns T.30 requests into a schedule of IFP packets.
//!
//! Everything here is planned the moment T.30 asks for it: the plan only depends on the request
//! and on when the previous burst ends, so the engine simply stores `(instant, packet)` pairs
//! and releases them as the clock advances. Times are microseconds because the V.21 data rate
//! (300 bit/s) does not divide into whole milliseconds.
//!
//! # Where the numbers come from
//!
//! They are SpanDSP 3.1.1's `t38_terminal` behaviour with transmission pacing enabled (its
//! default), which interoperates with real gateways:
//!
//! | step | SpanDSP | here |
//! |---|---|---|
//! | silence before CED | `no-signal`, then 200 ms | `ced_lead_silence_ms` |
//! | CED duration | 3 s | `ced_ms` |
//! | silence before a V.21 burst | `no-signal` (if something else was active), then 75 ms | `silence_before_burst_ms` |
//! | V.21 preamble flags | 1 s (`modem_startup_time` flags of `v21-preamble`) | `v21_preamble_ms` |
//! | HDLC data | `microseconds_per_tx_chunk` = 30 ms per packet: at 300 bit/s one octet per packet, then wait the octet's air time | `hdlc_chunk_ms` |
//! | end of frame | `hdlc-fcs-OK`, then wait `stuffed bits + 35` bit times (flags, FCS) | same |
//! | end of burst | `hdlc-fcs-OK-sig-end` x3, wait the bit times plus 100 ms, then `no-signal` | `sig_end_hold_ms` |
//! | repetition | indicators x3, data x1, signal end x3 | `indicator_repeat`, `data_end_repeat` |
//!
//! The task statement's "at least 75 ms between v21-preamble and the first frame" is
//! satisfied far more than required by the 1 s flag period: T.30 asks for a V.21 preamble of
//! 1 s +- 15 %, and a gateway that generates the flags itself needs that time before the first
//! octet reaches it.

use std::collections::VecDeque;
use t38::{DataType, Field, FieldType, Ifp, Indicator};

/// V.21 channel 2 bit rate.
const V21_BPS: u64 = 300;

/// Timing parameters of the transmit side.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Pacing {
    /// Master switch. Off, every packet of a burst goes out at the same instant (for TCP
    /// transports, tests and gateways that buffer).
    pub enabled: bool,
    /// Silence (the `no-signal` indicator) before CED.
    pub ced_lead_silence_ms: u64,
    /// How long CED is held (2.6 to 4 s).
    pub ced_ms: u64,
    /// Silence between the previous signal and the next V.21 burst.
    pub silence_before_burst_ms: u64,
    /// Time between the `v21-preamble` indicator and the first octet (T.30: 1 s +- 15 %).
    pub v21_preamble_ms: u64,
    /// Target air time of one `hdlc-data` packet; at 300 bit/s anything up to 41 ms is one
    /// octet per packet.
    pub hdlc_chunk_ms: u64,
    /// Extra hold after the final frame before `no-signal` (SpanDSP: 100 ms).
    pub sig_end_hold_ms: u64,
}

impl Pacing {
    /// SpanDSP's timing (the default).
    pub fn spandsp() -> Self {
        Self {
            enabled: true,
            ced_lead_silence_ms: 200,
            ced_ms: 3_000,
            silence_before_burst_ms: 75,
            v21_preamble_ms: 1_000,
            hdlc_chunk_ms: 30,
            sig_end_hold_ms: 100,
        }
    }

    /// No delays at all.
    pub fn none() -> Self {
        Self {
            enabled: false,
            ..Self::spandsp()
        }
    }
}

impl Default for Pacing {
    fn default() -> Self {
        Self::spandsp()
    }
}

/// One scheduled output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Item {
    /// An IFP packet to emit.
    Packet(Ifp),
    /// The burst that was planned before has fully left: tell T.30.
    TxComplete,
}

/// Plans bursts back to back.
#[derive(Debug)]
pub(crate) struct Scheduler {
    pacing: Pacing,
    indicator_repeat: u8,
    data_end_repeat: u8,
    queue: VecDeque<(u64, Item)>,
    /// When the last planned burst ends.
    cursor_us: u64,
    /// The last indicator that was scheduled.
    current: Indicator,
}

impl Scheduler {
    pub(crate) fn new(pacing: Pacing, indicator_repeat: u8, data_end_repeat: u8) -> Self {
        Self {
            pacing,
            indicator_repeat: indicator_repeat.max(1),
            data_end_repeat: data_end_repeat.max(1),
            queue: VecDeque::new(),
            cursor_us: 0,
            current: Indicator::NoSignal,
        }
    }

    pub(crate) fn clear(&mut self) {
        self.queue.clear();
    }

    /// Removes the pending "burst has left" markers and returns how many there were.
    pub(crate) fn take_tx_complete(&mut self) -> usize {
        let before = self.queue.len();
        self.queue.retain(|(_, item)| *item != Item::TxComplete);
        before - self.queue.len()
    }

    pub(crate) fn next_at(&self) -> Option<u64> {
        self.queue.front().map(|(t, _)| *t)
    }

    pub(crate) fn pop(&mut self) -> Option<(u64, Item)> {
        self.queue.pop_front()
    }

    fn ms(&self, ms: u64) -> u64 {
        if self.pacing.enabled { ms * 1_000 } else { 0 }
    }

    fn bits_us(&self, bits: u64) -> u64 {
        if self.pacing.enabled {
            bits * 1_000_000 / V21_BPS
        } else {
            0
        }
    }

    fn push(&mut self, at: u64, item: Item) {
        self.queue.push_back((at, item));
    }

    fn indicator(&mut self, at: u64, ind: Indicator) {
        for _ in 0..self.indicator_repeat {
            self.push(at, Item::Packet(Ifp::Indicator(ind)));
        }
        self.current = ind;
    }

    /// CED: `no-signal`, 200 ms of silence, `ced`, held for `ced_ms`.
    pub(crate) fn plan_ced(&mut self, now_us: u64) {
        let mut t = now_us.max(self.cursor_us);
        self.indicator(t, Indicator::NoSignal);
        t += self.ms(self.pacing.ced_lead_silence_ms);
        self.indicator(t, Indicator::Ced);
        t += self.ms(self.pacing.ced_ms);
        self.cursor_us = t;
    }

    /// A V.21 burst of HDLC frames; returns the instant it completes.
    pub(crate) fn plan_frames(&mut self, now_us: u64, frames: &[Vec<u8>]) -> u64 {
        let mut t = now_us.max(self.cursor_us);
        if self.current != Indicator::NoSignal {
            self.indicator(t, Indicator::NoSignal);
        }
        t += self.ms(self.pacing.silence_before_burst_ms);
        self.indicator(t, Indicator::V21Preamble);
        t += self.ms(self.pacing.v21_preamble_ms);
        let chunk_octets = if self.pacing.enabled {
            ((self.pacing.hdlc_chunk_ms * V21_BPS) / 8_000).max(1) as usize
        } else {
            usize::MAX
        };
        let last = frames.len().saturating_sub(1);
        for (i, frame) in frames.iter().enumerate() {
            for chunk in frame.chunks(chunk_octets.min(frame.len().max(1))) {
                self.push(t, Item::Packet(hdlc(FieldType::HdlcData, chunk)));
                t += self.bits_us(chunk.len() as u64 * 8);
            }
            let tail_us = self.bits_us(stuffed_bits(frame) + 16 + 3 + 16);
            if i == last {
                for _ in 0..self.data_end_repeat {
                    self.push(t, Item::Packet(hdlc(FieldType::HdlcFcsOkSigEnd, &[])));
                }
                t += tail_us + self.ms(self.pacing.sig_end_hold_ms);
            } else {
                self.push(t, Item::Packet(hdlc(FieldType::HdlcFcsOk, &[])));
                t += tail_us;
            }
        }
        self.indicator(t, Indicator::NoSignal);
        self.push(t, Item::TxComplete);
        self.cursor_us = t;
        t
    }
}

fn hdlc(field_type: FieldType, data: &[u8]) -> Ifp {
    Ifp::Data {
        data_type: DataType::V21,
        fields: vec![Field::new(field_type, data.to_vec())],
    }
}

/// Number of zero bits HDLC inserts after five consecutive ones in the transmitted bit order
/// (most significant bit of a wire octet first).
pub(crate) fn stuffed_bits(frame: &[u8]) -> u64 {
    let mut ones = 0u32;
    let mut stuffed = 0u64;
    for &b in frame {
        for i in (0..8).rev() {
            if (b >> i) & 1 == 1 {
                ones += 1;
                if ones == 5 {
                    stuffed += 1;
                    ones = 0;
                }
            } else {
                ones = 0;
            }
        }
    }
    stuffed
}
