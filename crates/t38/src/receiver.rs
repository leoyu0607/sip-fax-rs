//! The UDPTL receive side: ordering, duplicate suppression and loss recovery.
//!
//! [`UdptlReceiver`] is a pure state machine. Feed it datagrams with
//! [`UdptlReceiver::on_datagram`]; it returns [`RxEvent`]s in sequence order. Because it owns
//! no clock, a packet that arrives ahead of a hole is held back only until the hole is filled,
//! recovered, or the newest packet is `reorder_window` ahead of it; if the stream goes quiet
//! the driver calls [`UdptlReceiver::flush`] after its own reorder timeout, which releases
//! everything held and reports the holes as gaps. Nothing can therefore get stuck.
//!
//! Sequence numbers are tracked as extended (unwrapped) 64-bit values internally, so the
//! 65535 -> 0 wrap needs no special handling.

use crate::udptl::{UdptlError, UdptlPacket, UdptlRecovery};
use std::collections::BTreeMap;

/// How a delivered packet reached the receiver.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// It arrived as the primary IFP of its own datagram.
    Primary,
    /// It was taken from the secondary (redundancy) list of a later datagram.
    Redundancy,
    /// It was rebuilt from parity FEC. Its length is that of the longest packet in the FEC
    /// set, so a shorter original has trailing zero octets (`Ifp::decode_padded`).
    Fec,
}

/// Something the receiver hands to the upper layer, in sequence order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RxEvent {
    /// An IFP packet.
    Packet {
        /// Its UDPTL sequence number.
        seq: u16,
        /// The IFP octets.
        ifp: Vec<u8>,
        /// How it was obtained.
        source: Source,
    },
    /// `count` consecutive packets were lost and can no longer be recovered.
    Gap {
        /// Number of lost packets.
        count: u32,
    },
}

/// Receiver tuning.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceiverConfig {
    /// How far ahead of the next expected sequence number the newest packet may be before the
    /// receiver stops waiting for the missing ones (default 32).
    pub reorder_window: u16,
    /// Number of recent packets kept for FEC reconstruction (default 256; raised to at least
    /// `reorder_window + 64`).
    pub history: usize,
}

impl Default for ReceiverConfig {
    fn default() -> Self {
        Self {
            reorder_window: 32,
            history: 256,
        }
    }
}

/// Counters, for logging and tests.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ReceiverStats {
    /// Datagrams successfully parsed.
    pub datagrams: u64,
    /// Datagrams that failed to parse.
    pub malformed: u64,
    /// Datagrams whose primary was already known (retransmission or duplicate).
    pub duplicates: u64,
    /// Datagrams older than the delivery point that were not known (arrived too late).
    pub late: u64,
    /// Packets delivered, in order.
    pub delivered: u64,
    /// Packets obtained from secondary (redundancy) lists.
    pub recovered_redundancy: u64,
    /// Packets rebuilt from FEC.
    pub recovered_fec: u64,
    /// Packets reported lost through [`RxEvent::Gap`].
    pub lost: u64,
    /// Times the sequence space jumped so far back that the receiver restarted.
    pub resyncs: u64,
}

/// Restart when a packet is this far behind the delivery point (a genuine late packet is at
/// most about `reorder_window` behind).
const RESYNC_DISTANCE: i64 = 1024;

/// Reorders, de-duplicates and repairs a UDPTL stream.
#[derive(Debug, Clone)]
pub struct UdptlReceiver {
    cfg: ReceiverConfig,
    started: bool,
    /// Extended sequence number of the next packet to deliver.
    next: i64,
    /// Highest extended sequence number seen as a primary packet.
    highest: i64,
    pending: BTreeMap<i64, (Vec<u8>, Source)>,
    history: BTreeMap<i64, Vec<u8>>,
    fec: BTreeMap<i64, (u32, Vec<Vec<u8>>)>,
    stats: ReceiverStats,
}

impl Default for UdptlReceiver {
    fn default() -> Self {
        Self::new(ReceiverConfig::default())
    }
}

impl UdptlReceiver {
    /// Creates a receiver.
    pub fn new(mut cfg: ReceiverConfig) -> Self {
        cfg.history = cfg.history.max(usize::from(cfg.reorder_window) + 64);
        Self {
            cfg,
            started: false,
            next: 0,
            highest: 0,
            pending: BTreeMap::new(),
            history: BTreeMap::new(),
            fec: BTreeMap::new(),
            stats: ReceiverStats::default(),
        }
    }

    /// Counters since creation.
    pub fn stats(&self) -> ReceiverStats {
        self.stats
    }

    /// Number of packets currently held back waiting for a hole to fill.
    pub fn held(&self) -> usize {
        self.pending.len()
    }

    fn extend(&self, seq: u16) -> i64 {
        let diff = seq.wrapping_sub(self.highest as u16) as i16;
        self.highest + i64::from(diff)
    }

    fn reset(&mut self) {
        self.started = false;
        self.pending.clear();
        self.history.clear();
        self.fec.clear();
    }

    /// Processes one received datagram.
    ///
    /// # Errors
    /// Returns the parse error for a malformed datagram; the receiver state is unchanged.
    pub fn on_datagram(&mut self, buf: &[u8]) -> Result<Vec<RxEvent>, UdptlError> {
        let pkt = match UdptlPacket::decode(buf) {
            Ok(p) => p,
            Err(e) => {
                self.stats.malformed += 1;
                return Err(e);
            }
        };
        self.stats.datagrams += 1;
        let mut ext = i64::from(pkt.seq);
        if self.started {
            ext = self.extend(pkt.seq);
            if self.next - ext > RESYNC_DISTANCE {
                self.stats.resyncs += 1;
                self.reset();
                ext = i64::from(pkt.seq);
            }
        }
        if !self.started {
            self.started = true;
            self.highest = ext;
            let secondaries = match &pkt.recovery {
                UdptlRecovery::Secondary(l) => l.len() as i64,
                UdptlRecovery::Fec { .. } => 0,
            };
            self.next = ext - secondaries;
        }
        if ext < self.next {
            if self.history.contains_key(&ext) {
                self.stats.duplicates += 1;
            } else {
                self.stats.late += 1;
            }
            return Ok(Vec::new());
        }
        self.highest = self.highest.max(ext);
        if self.pending.contains_key(&ext) {
            // Already obtained through redundancy / FEC of a later datagram, or a duplicate.
            // Its own recovery data may still fill other holes, so keep processing.
            self.stats.duplicates += 1;
        } else {
            self.insert(ext, pkt.primary, Source::Primary);
        }
        match pkt.recovery {
            UdptlRecovery::Secondary(list) => {
                for (i, data) in list.into_iter().enumerate() {
                    let s = ext - 1 - i as i64;
                    if s >= self.next && !self.history.contains_key(&s) {
                        self.stats.recovered_redundancy += 1;
                        self.insert(s, data, Source::Redundancy);
                    }
                }
            }
            UdptlRecovery::Fec { span, data } => {
                self.fec.insert(ext, (span, data));
                self.repair_with_fec();
            }
        }
        self.trim();
        let mut events = Vec::new();
        self.drain(false, &mut events);
        Ok(events)
    }

    /// Releases everything that is held back, reporting the holes as [`RxEvent::Gap`].
    /// Call this when the driver's reorder timeout expires or the stream ends.
    pub fn flush(&mut self) -> Vec<RxEvent> {
        let mut events = Vec::new();
        self.drain(true, &mut events);
        events
    }

    fn insert(&mut self, seq: i64, data: Vec<u8>, source: Source) {
        self.history.insert(seq, data.clone());
        self.pending.insert(seq, (data, source));
    }

    fn trim(&mut self) {
        let floor = self.highest - self.cfg.history as i64;
        self.history = self.history.split_off(&floor);
        self.fec = self.fec.split_off(&floor);
    }

    fn repair_with_fec(&mut self) {
        let max_set = self.cfg.history as i64;
        loop {
            let mut found: Option<(i64, Vec<u8>)> = None;
            'outer: for (&l, (span, data)) in &self.fec {
                let entries = data.len() as i64;
                let span = i64::from(*span);
                if span == 0 || entries == 0 || span.saturating_mul(entries) > max_set {
                    continue;
                }
                for (m, fec_entry) in data.iter().enumerate() {
                    let first = l + m as i64 - span * entries;
                    let mut missing = None;
                    let mut count = 0;
                    for k in 0..span {
                        let idx = first + k * entries;
                        if !self.history.contains_key(&idx) {
                            count += 1;
                            missing = Some(idx);
                        }
                    }
                    let (1, Some(x)) = (count, missing) else {
                        continue;
                    };
                    if x < self.next {
                        continue; // already passed (or declared lost): nothing to deliver
                    }
                    let mut rebuilt = fec_entry.clone();
                    for k in 0..span {
                        let idx = first + k * entries;
                        if idx == x {
                            continue;
                        }
                        if let Some(other) = self.history.get(&idx) {
                            for (a, b) in rebuilt.iter_mut().zip(other) {
                                *a ^= *b;
                            }
                        }
                    }
                    found = Some((x, rebuilt));
                    break 'outer;
                }
            }
            match found {
                Some((x, data)) => {
                    self.stats.recovered_fec += 1;
                    self.insert(x, data, Source::Fec);
                }
                None => return,
            }
        }
    }

    fn drain(&mut self, force: bool, out: &mut Vec<RxEvent>) {
        loop {
            if let Some((ifp, source)) = self.pending.remove(&self.next) {
                out.push(RxEvent::Packet {
                    seq: self.next as u16,
                    ifp,
                    source,
                });
                self.stats.delivered += 1;
                self.next += 1;
                continue;
            }
            let Some(&first_held) = self.pending.keys().next() else {
                return;
            };
            if force || self.highest - self.next >= i64::from(self.cfg.reorder_window) {
                let count = (first_held - self.next).max(0);
                out.push(RxEvent::Gap {
                    count: u32::try_from(count).unwrap_or(u32::MAX),
                });
                self.stats.lost += count as u64;
                self.next = first_held;
            } else {
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests;
