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
mod tests {
    use super::*;
    use crate::udptl::{ErrorRecovery, UdptlSender};

    fn ifp(i: usize) -> Vec<u8> {
        vec![0x80 | (i % 7) as u8, i as u8, (i >> 8) as u8, 0x5A]
    }

    fn send_all(mode: ErrorRecovery, n: usize, first: u16) -> Vec<Vec<u8>> {
        let mut tx = UdptlSender::new(mode, first);
        (0..n).map(|i| tx.build(&ifp(i)).unwrap()).collect()
    }

    fn seqs(ev: &[RxEvent]) -> Vec<u16> {
        ev.iter()
            .filter_map(|e| match e {
                RxEvent::Packet { seq, .. } => Some(*seq),
                RxEvent::Gap { .. } => None,
            })
            .collect()
    }

    #[test]
    fn in_order_delivery() {
        let dg = send_all(ErrorRecovery::Redundancy { depth: 2 }, 5, 0);
        let mut rx = UdptlReceiver::default();
        for (i, d) in dg.iter().enumerate() {
            let ev = rx.on_datagram(d).unwrap();
            assert_eq!(
                ev,
                vec![RxEvent::Packet {
                    seq: i as u16,
                    ifp: ifp(i),
                    source: Source::Primary
                }]
            );
        }
        assert_eq!(rx.stats().delivered, 5);
        assert_eq!(rx.held(), 0);
    }

    #[test]
    fn reorder_within_window_and_duplicates() {
        let dg = send_all(ErrorRecovery::None, 6, 0);
        let mut rx = UdptlReceiver::default();
        let mut got = Vec::new();
        for i in [0usize, 2, 1, 1, 4, 3, 3, 5, 0] {
            got.extend(rx.on_datagram(&dg[i]).unwrap());
        }
        assert_eq!(seqs(&got), [0, 1, 2, 3, 4, 5]);
        assert!(!got.iter().any(|e| matches!(e, RxEvent::Gap { .. })));
        let s = rx.stats();
        assert_eq!((s.duplicates, s.late, s.delivered, s.lost), (3, 0, 6, 0));
    }

    #[test]
    fn redundancy_recovers_single_and_burst() {
        let dg = send_all(ErrorRecovery::Redundancy { depth: 3 }, 10, 0);
        let mut rx = UdptlReceiver::default();
        let mut got = Vec::new();
        for i in [0usize, 1, 2, 6, 7, 8, 9] {
            // 3,4,5 lost (3 consecutive): packet 6 carries 5,4,3.
            got.extend(rx.on_datagram(&dg[i]).unwrap());
        }
        assert_eq!(seqs(&got), [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        assert_eq!(rx.stats().recovered_redundancy, 3);
        let sources: Vec<Source> = got
            .iter()
            .filter_map(|e| match e {
                RxEvent::Packet { source, .. } => Some(*source),
                _ => None,
            })
            .collect();
        assert_eq!(&sources[3..6], [Source::Redundancy; 3]);
        for e in &got {
            if let RxEvent::Packet { seq, ifp: data, .. } = e {
                assert_eq!(*data, ifp(usize::from(*seq)));
            }
        }
    }

    #[test]
    fn unrecoverable_gap_is_reported_and_stream_continues() {
        let dg = send_all(ErrorRecovery::Redundancy { depth: 1 }, 10, 0);
        let mut rx = UdptlReceiver::new(ReceiverConfig {
            reorder_window: 4,
            history: 64,
        });
        let mut got = Vec::new();
        // lose 3 and 4 (depth 1 only brings back 4 with packet 5)... lose 3,4,5: 6 recovers 5 only.
        for i in [0usize, 1, 2, 6, 7, 8, 9] {
            got.extend(rx.on_datagram(&dg[i]).unwrap());
        }
        // Lost 3 and 4; 5 recovered. window 4: hole released once highest - next >= 4.
        assert_eq!(
            got.iter()
                .map(|e| match e {
                    RxEvent::Gap { count } => format!("gap{count}"),
                    RxEvent::Packet { seq, .. } => seq.to_string(),
                })
                .collect::<Vec<_>>(),
            ["0", "1", "2", "gap2", "5", "6", "7", "8", "9"]
        );
        assert_eq!(rx.stats().lost, 2);
        // later packets keep flowing
        let more = send_all(ErrorRecovery::Redundancy { depth: 1 }, 12, 0);
        let ev = rx.on_datagram(&more[10]).unwrap();
        assert_eq!(seqs(&ev), [10]);
    }

    #[test]
    fn flush_releases_held_packets_with_gap() {
        let dg = send_all(ErrorRecovery::None, 5, 0);
        let mut rx = UdptlReceiver::default();
        assert_eq!(seqs(&rx.on_datagram(&dg[0]).unwrap()), [0]);
        assert!(rx.on_datagram(&dg[2]).unwrap().is_empty());
        assert!(rx.on_datagram(&dg[4]).unwrap().is_empty());
        assert_eq!(rx.held(), 2);
        let ev = rx.flush();
        assert_eq!(
            ev.iter()
                .map(|e| format!("{e:?}").chars().take(12).collect::<String>())
                .count(),
            4
        );
        assert_eq!(ev[0], RxEvent::Gap { count: 1 });
        assert!(matches!(ev[1], RxEvent::Packet { seq: 2, .. }));
        assert_eq!(ev[2], RxEvent::Gap { count: 1 });
        assert!(matches!(ev[3], RxEvent::Packet { seq: 4, .. }));
        assert_eq!(rx.held(), 0);
        assert!(rx.flush().is_empty());
        // a late arrival of 3 is dropped
        assert!(rx.on_datagram(&dg[3]).unwrap().is_empty());
        assert_eq!(rx.stats().late, 1);
        assert_eq!(rx.stats().lost, 2);
    }

    #[test]
    fn fec_recovers_one_loss_per_set() {
        // span 3 entries 1: every packet protects the 3 before it.
        let dg = send_all(
            ErrorRecovery::Fec {
                span: 3,
                entries: 1,
            },
            12,
            0,
        );
        let mut rx = UdptlReceiver::default();
        let mut got = Vec::new();
        for (i, d) in dg.iter().enumerate() {
            if i == 5 || i == 9 {
                continue;
            }
            got.extend(rx.on_datagram(d).unwrap());
        }
        assert_eq!(seqs(&got), (0..12).collect::<Vec<u16>>());
        assert_eq!(rx.stats().recovered_fec, 2);
        for e in &got {
            if let RxEvent::Packet {
                seq,
                ifp: data,
                source,
            } = e
            {
                assert_eq!(*data, ifp(usize::from(*seq)), "seq {seq}");
                assert_eq!(*source == Source::Fec, *seq == 5 || *seq == 9);
            }
        }
    }

    #[test]
    fn fec_two_entries_interleaved_burst() {
        // span 2 entries 2: protects sets {n-4,n-2} and {n-3,n-1}: survives a burst of 2.
        let dg = send_all(
            ErrorRecovery::Fec {
                span: 2,
                entries: 2,
            },
            14,
            0,
        );
        let mut rx = UdptlReceiver::default();
        let mut got = Vec::new();
        for (i, d) in dg.iter().enumerate() {
            if i == 6 || i == 7 {
                continue;
            }
            got.extend(rx.on_datagram(d).unwrap());
        }
        got.extend(rx.flush());
        assert_eq!(seqs(&got), (0..14).collect::<Vec<u16>>());
        assert_eq!(rx.stats().recovered_fec, 2);
        assert_eq!(rx.stats().lost, 0);
    }

    #[test]
    fn fec_padded_recovery_for_shorter_packet() {
        let mut tx = UdptlSender::new(
            ErrorRecovery::Fec {
                span: 2,
                entries: 1,
            },
            0,
        );
        let pk = [vec![0x01u8], vec![0x02, 0x03, 0x04], vec![0x05], vec![0x06]];
        let dg: Vec<_> = pk.iter().map(|p| tx.build(p).unwrap()).collect();
        let mut rx = UdptlReceiver::default();
        let mut got = Vec::new();
        for i in [0usize, 2, 3] {
            got.extend(rx.on_datagram(&dg[i]).unwrap());
        }
        // 1 is rebuilt from packet 2's FEC (set {0,1}); it has the longest length (3).
        got.extend(rx.flush());
        assert!(got.iter().any(|e| *e
            == RxEvent::Packet {
                seq: 1,
                ifp: vec![0x02, 0x03, 0x04],
                source: Source::Fec
            }));
        // packet 2 (length 1) recovered when 3 arrives with set {1,2}... but only if 2 missing.
        let mut rx = UdptlReceiver::default();
        let mut got = Vec::new();
        for i in [0usize, 1, 3] {
            got.extend(rx.on_datagram(&dg[i]).unwrap());
        }
        assert!(got.iter().any(|e| *e
            == RxEvent::Packet {
                seq: 2,
                ifp: vec![0x05, 0x00, 0x00],
                source: Source::Fec
            }));
    }

    #[test]
    fn two_losses_in_one_set_not_recoverable() {
        let dg = send_all(
            ErrorRecovery::Fec {
                span: 3,
                entries: 1,
            },
            8,
            0,
        );
        let mut rx = UdptlReceiver::new(ReceiverConfig {
            reorder_window: 3,
            history: 64,
        });
        let mut got = Vec::new();
        for (i, d) in dg.iter().enumerate() {
            if i == 4 || i == 5 {
                continue;
            }
            got.extend(rx.on_datagram(d).unwrap());
        }
        got.extend(rx.flush());
        assert!(got.contains(&RxEvent::Gap { count: 2 }));
        assert_eq!(seqs(&got), [0, 1, 2, 3, 6, 7]);
    }

    #[test]
    fn sequence_wrap_65534_to_1() {
        let dg = send_all(ErrorRecovery::Redundancy { depth: 2 }, 6, 65533);
        let mut rx = UdptlReceiver::default();
        let mut got = Vec::new();
        // 65533, 65534 lost? deliver 65533, then 0 (65535 lost, recovered), 65534 late dup order.
        for i in [0usize, 1, 3, 2, 4, 5] {
            got.extend(rx.on_datagram(&dg[i]).unwrap());
        }
        assert_eq!(seqs(&got), [65533, 65534, 65535, 0, 1, 2]);
        assert!(!got.iter().any(|e| matches!(e, RxEvent::Gap { .. })));
        // loss across the wrap
        let mut rx = UdptlReceiver::default();
        let mut got = Vec::new();
        for i in [0usize, 1, 4, 5] {
            got.extend(rx.on_datagram(&dg[i]).unwrap());
        }
        assert_eq!(seqs(&got), [65533, 65534, 65535, 0, 1, 2]);
        assert_eq!(rx.stats().recovered_redundancy, 2);
    }

    #[test]
    fn first_packet_brings_its_redundancy() {
        let dg = send_all(ErrorRecovery::Redundancy { depth: 3 }, 6, 0);
        let mut rx = UdptlReceiver::default();
        // packet 0 lost: packet 1 delivers 0 and 1
        let ev = rx.on_datagram(&dg[1]).unwrap();
        assert_eq!(seqs(&ev), [0, 1]);
        // joining late (first seen: 5, with 4,3,2 behind it)
        let mut rx = UdptlReceiver::default();
        let ev = rx.on_datagram(&dg[5]).unwrap();
        assert_eq!(seqs(&ev), [2, 3, 4, 5]);
    }

    #[test]
    fn far_jump_is_a_gap_and_far_back_resyncs() {
        let a = send_all(ErrorRecovery::None, 1, 0);
        let b = send_all(ErrorRecovery::None, 1, 5000);
        let mut rx = UdptlReceiver::default();
        rx.on_datagram(&a[0]).unwrap();
        let ev = rx.on_datagram(&b[0]).unwrap();
        assert_eq!(
            ev,
            vec![
                RxEvent::Gap { count: 4999 },
                RxEvent::Packet {
                    seq: 5000,
                    ifp: ifp(0),
                    source: Source::Primary
                },
            ]
        );
        // peer restarts numbering at 0: more than RESYNC_DISTANCE behind => start over
        let ev = rx.on_datagram(&a[0]).unwrap();
        assert_eq!(seqs(&ev), [0]);
        assert_eq!(rx.stats().resyncs, 1);
    }

    #[test]
    fn malformed_datagram_leaves_state_untouched() {
        let dg = send_all(ErrorRecovery::None, 2, 0);
        let mut rx = UdptlReceiver::default();
        rx.on_datagram(&dg[0]).unwrap();
        assert!(rx.on_datagram(&[1, 2, 3]).is_err());
        assert_eq!(rx.stats().malformed, 1);
        assert_eq!(seqs(&rx.on_datagram(&dg[1]).unwrap()), [1]);
    }

    #[test]
    fn hostile_fec_parameters_do_not_hang() {
        let pkt = |span, n| {
            UdptlPacket {
                seq: 3,
                primary: vec![1],
                recovery: UdptlRecovery::Fec {
                    span,
                    data: vec![vec![0u8; 4]; n],
                },
            }
            .encode()
            .unwrap()
        };
        for (span, n) in [
            (0u32, 3usize),
            (u32::MAX, 2),
            (1_000_000, 40),
            (1, 0),
            (7, 9),
        ] {
            let mut rx = UdptlReceiver::default();
            let _ = rx.on_datagram(&pkt(span, n)).unwrap();
        }
    }
}
