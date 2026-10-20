//! UDPTL (ITU-T T.38 annex, clause 9 / `UDPTLPacket` ASN.1): packet codec and sender.
//!
//! ```text
//! UDPTLPacket ::= SEQUENCE {
//!     seq-number          INTEGER (0..65535),
//!     primary-ifp-packet  OPEN TYPE (IFPPacket),
//!     error-recovery      CHOICE {
//!         secondary-ifp-packets  SEQUENCE OF OPEN TYPE (IFPPacket),
//!         fec-info               SEQUENCE { fec-npackets INTEGER,
//!                                           fec-data SEQUENCE OF OCTET STRING } } }
//! ```
//!
//! Wire layout (aligned PER): 2 octets sequence number; the primary IFP as a length
//! determinant plus octets; one octet whose top bit selects redundancy (0) or FEC (1); then
//! for redundancy the number of secondary packets (most recent first) each as length +
//! octets, and for FEC an INTEGER (length octet + two's complement) followed by the number of
//! FEC entries and each entry as length + octets.
//!
//! # FEC semantics
//! This follows the de-facto interoperable scheme of SpanDSP / Asterisk: a packet with
//! sequence `n` carries `entries` FEC octet strings and a `span`. Entry `m` (0-based) is the
//! XOR of the `span` packets `n + m - span * entries + k * entries` for `k = 0..span`,
//! zero-extended to the longest. Any single missing packet of such a set is the XOR of the
//! FEC entry with the others; the recovered packet has the length of the longest protected
//! packet (shorter originals come back zero padded, see `Ifp::decode_padded`).

use crate::per::{
    BitReader, BitWriter, PerError, plan_fragments, read_octet_string, write_octet_string,
};
use std::collections::VecDeque;
use thiserror::Error;

/// Default maximum datagram size used to cap redundancy (a typical Ethernet MTU payload).
pub const DEFAULT_MAX_DATAGRAM: usize = 1400;

/// Errors from UDPTL encoding and decoding.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum UdptlError {
    /// Malformed PER (truncation, bad length determinant).
    #[error("malformed PER: {0}")]
    Per(#[from] PerError),
    /// The primary IFP packet to send was empty.
    #[error("empty IFP packet")]
    EmptyIfp,
    /// Octets remain after the packet.
    #[error("{0} trailing octet(s) after the UDPTL packet")]
    TrailingData(usize),
    /// The `fec-npackets` INTEGER is empty, negative or does not fit 32 bits.
    #[error("unsupported fec-npackets integer")]
    BadInteger,
    /// A count that cannot possibly fit in the remaining octets.
    #[error("element count {0} exceeds the remaining input")]
    CountTooLarge(usize),
}

/// The decoded `error-recovery` CHOICE.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UdptlRecovery {
    /// `secondary-ifp-packets`: earlier IFP packets, most recent first
    /// (entry `i` (0-based) is the packet with sequence `seq - 1 - i`).
    Secondary(Vec<Vec<u8>>),
    /// `fec-info`.
    Fec {
        /// `fec-npackets` (the span).
        span: u32,
        /// `fec-data` entries.
        data: Vec<Vec<u8>>,
    },
}

/// A decoded UDPTL packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UdptlPacket {
    /// Sequence number.
    pub seq: u16,
    /// The primary IFP packet (opaque octets; see [`crate::Ifp`]).
    pub primary: Vec<u8>,
    /// Error recovery information.
    pub recovery: UdptlRecovery,
}

/// Octets of an octet string of `len` octets including its length determinant.
fn encoded_len(len: usize) -> usize {
    len + if len < 0x80 {
        1
    } else if len < crate::per::FRAGMENT_UNIT {
        2
    } else {
        3 + len / crate::per::FRAGMENT_UNIT
    }
}

fn write_list(w: &mut BitWriter, items: &[Vec<u8>]) {
    let mut idx = 0usize;
    for chunk in plan_fragments(items.len()) {
        w.write_length_chunk(chunk);
        for it in items.get(idx..idx + chunk).unwrap_or(&[]) {
            write_octet_string(w, it);
        }
        idx += chunk;
    }
}

fn read_list(r: &mut BitReader<'_>) -> Result<Vec<Vec<u8>>, UdptlError> {
    let mut out = Vec::new();
    loop {
        let (count, more) = r.read_length_chunk()?;
        // Every element needs at least its length octet.
        if count > r.remaining_bytes() {
            return Err(UdptlError::CountTooLarge(count));
        }
        for _ in 0..count {
            out.push(read_octet_string(r)?);
        }
        if !more {
            return Ok(out);
        }
    }
}

impl UdptlPacket {
    /// Encodes the packet.
    ///
    /// # Errors
    /// [`UdptlError::EmptyIfp`] if the primary packet is empty.
    pub fn encode(&self) -> Result<Vec<u8>, UdptlError> {
        if self.primary.is_empty() {
            return Err(UdptlError::EmptyIfp);
        }
        let mut w = BitWriter::new();
        w.write_u16(self.seq);
        write_octet_string(&mut w, &self.primary);
        match &self.recovery {
            UdptlRecovery::Secondary(list) => {
                w.write_bit(false);
                write_list(&mut w, list);
            }
            UdptlRecovery::Fec { span, data } => {
                w.write_bit(true);
                let be = span.to_be_bytes();
                let skip = be.iter().take(3).take_while(|&&b| b == 0).count();
                let mut int: Vec<u8> = be.get(skip..).unwrap_or(&[]).to_vec();
                if int.first().is_some_and(|b| b & 0x80 != 0) {
                    int.insert(0, 0); // keep it non-negative
                }
                w.write_length_chunk(int.len());
                w.write_bytes(&int);
                write_list(&mut w, data);
            }
        }
        Ok(w.into_bytes())
    }

    /// Decodes one datagram; trailing octets are an error.
    ///
    /// # Errors
    /// Any [`UdptlError`]; never panics.
    pub fn decode(buf: &[u8]) -> Result<Self, UdptlError> {
        let mut r = BitReader::new(buf);
        let seq = r.read_u16()?;
        let primary = read_octet_string(&mut r)?;
        let recovery = if r.read_bit()? {
            let (n, more) = r.read_length_chunk()?;
            if more || n == 0 || n > 5 {
                return Err(UdptlError::BadInteger);
            }
            let bytes = r.read_bytes(n)?;
            let first = bytes.first().copied().unwrap_or(0);
            if first & 0x80 != 0 {
                return Err(UdptlError::BadInteger);
            }
            let mut span: u64 = 0;
            for b in bytes {
                span = (span << 8) | u64::from(*b);
            }
            let span = u32::try_from(span).map_err(|_| UdptlError::BadInteger)?;
            UdptlRecovery::Fec {
                span,
                data: read_list(&mut r)?,
            }
        } else {
            UdptlRecovery::Secondary(read_list(&mut r)?)
        };
        if !r.at_end() {
            return Err(UdptlError::TrailingData(r.remaining_bytes()));
        }
        Ok(Self {
            seq,
            primary,
            recovery,
        })
    }
}

/// Error recovery scheme of a [`UdptlSender`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorRecovery {
    /// No recovery data (an empty secondary list is sent).
    None,
    /// Redundancy: each packet repeats the previous `depth` IFP packets.
    Redundancy {
        /// Number of previous packets repeated (the default for T.38 is 3).
        depth: u8,
    },
    /// Parity FEC: see the module documentation for `span` and `entries`.
    Fec {
        /// Number of packets XORed into one FEC entry.
        span: u8,
        /// Number of FEC entries per packet.
        entries: u8,
    },
}

impl Default for ErrorRecovery {
    fn default() -> Self {
        ErrorRecovery::Redundancy { depth: 3 }
    }
}

/// Builds UDPTL datagrams from IFP packets, one sequence number per call.
#[derive(Debug, Clone)]
pub struct UdptlSender {
    mode: ErrorRecovery,
    next_seq: u16,
    /// Packets sent so far (drives the FEC ramp-up and the redundancy depth at start).
    sent: u64,
    /// Recent IFP packets, oldest first; `history[i]` has stream index `base + i`.
    history: VecDeque<Vec<u8>>,
    base: u64,
    max_datagram: usize,
}

impl UdptlSender {
    /// Creates a sender; the first packet gets sequence number `initial_seq`.
    pub fn new(mode: ErrorRecovery, initial_seq: u16) -> Self {
        Self {
            mode,
            next_seq: initial_seq,
            sent: 0,
            history: VecDeque::new(),
            base: 0,
            max_datagram: DEFAULT_MAX_DATAGRAM,
        }
    }

    /// Limits the size of redundancy datagrams: older secondary packets that would push the
    /// datagram over `max` octets are dropped (the primary packet is always sent).
    /// FEC entries are never trimmed because that would change the protected set.
    pub fn with_max_datagram(mut self, max: usize) -> Self {
        self.max_datagram = max;
        self
    }

    /// Sequence number the next call to [`UdptlSender::build`] will use.
    pub fn next_seq(&self) -> u16 {
        self.next_seq
    }

    /// Changes the error recovery scheme for subsequent packets.
    pub fn set_mode(&mut self, mode: ErrorRecovery) {
        self.mode = mode;
    }

    fn needed_history(&self) -> usize {
        match self.mode {
            ErrorRecovery::None => 0,
            ErrorRecovery::Redundancy { depth } => usize::from(depth),
            ErrorRecovery::Fec { span, entries } => usize::from(span) * usize::from(entries),
        }
    }

    fn get(&self, index: u64) -> Option<&Vec<u8>> {
        let off = index.checked_sub(self.base)?;
        self.history.get(usize::try_from(off).ok()?)
    }

    /// Builds the datagram carrying `ifp` as the primary packet.
    ///
    /// # Errors
    /// [`UdptlError::EmptyIfp`] if `ifp` is empty (the sequence number is not consumed).
    pub fn build(&mut self, ifp: &[u8]) -> Result<Vec<u8>, UdptlError> {
        if ifp.is_empty() {
            return Err(UdptlError::EmptyIfp);
        }
        let n = self.sent;
        let seq = self.next_seq;
        let recovery = match self.mode {
            ErrorRecovery::None => UdptlRecovery::Secondary(Vec::new()),
            ErrorRecovery::Redundancy { depth } => {
                let mut list: Vec<Vec<u8>> = Vec::new();
                let want = u64::from(depth).min(n);
                // seq(2) + primary + choice octet + count octet (+ one entry each below).
                let mut size = 4 + encoded_len(ifp.len());
                for m in 0..want {
                    let Some(pkt) = self.get(n - 1 - m) else {
                        break;
                    };
                    size += encoded_len(pkt.len());
                    if size > self.max_datagram {
                        break;
                    }
                    list.push(pkt.clone());
                }
                UdptlRecovery::Secondary(list)
            }
            ErrorRecovery::Fec { span, entries } => self.build_fec(n, span, entries),
        };
        let pkt = UdptlPacket {
            seq,
            primary: ifp.to_vec(),
            recovery,
        };
        let out = pkt.encode()?;
        self.history.push_back(ifp.to_vec());
        let keep = self.needed_history();
        while self.history.len() > keep {
            self.history.pop_front();
            self.base += 1;
        }
        // `base` must track the stream index of `history.front()`.
        self.sent += 1;
        self.next_seq = seq.wrapping_add(1);
        Ok(out)
    }

    fn build_fec(&self, n: u64, span: u8, entries: u8) -> UdptlRecovery {
        let (s_cfg, e_cfg) = (u64::from(span), u64::from(entries));
        if s_cfg == 0 || e_cfg == 0 {
            return UdptlRecovery::Fec {
                span: 0,
                data: Vec::new(),
            };
        }
        let (mut s, mut e) = (s_cfg, e_cfg);
        if n < s * e {
            // Ramp-up: only use packets that exist.
            e = n / s;
            if n < s {
                s = 0;
            }
        }
        let mut data = Vec::new();
        for m in 0..e {
            let first = n + m - s * e;
            let mut acc: Vec<u8> = Vec::new();
            for k in 0..s {
                let Some(pkt) = self.get(first + k * e) else {
                    continue;
                };
                if acc.len() < pkt.len() {
                    acc.resize(pkt.len(), 0);
                }
                for (a, b) in acc.iter_mut().zip(pkt) {
                    *a ^= *b;
                }
            }
            data.push(acc);
        }
        UdptlRecovery::Fec {
            span: s as u32,
            data,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ifp(i: u8) -> Vec<u8> {
        vec![0x06, i, i.wrapping_mul(3), 0xF0 | i]
    }

    #[test]
    fn worked_example_redundancy_bytes() {
        // seq 1, primary [02], one secondary [06]: derived by hand from the ASN.1.
        let pkt = UdptlPacket {
            seq: 1,
            primary: vec![0x02],
            recovery: UdptlRecovery::Secondary(vec![vec![0x06]]),
        };
        let enc = pkt.encode().unwrap();
        assert_eq!(enc, [0x00, 0x01, 0x01, 0x02, 0x00, 0x01, 0x01, 0x06]);
        assert_eq!(UdptlPacket::decode(&enc).unwrap(), pkt);
    }

    #[test]
    fn worked_example_fec_bytes() {
        // seq 0x0102, primary [AA BB], span 2, entries [[01 02 03], []].
        let pkt = UdptlPacket {
            seq: 0x0102,
            primary: vec![0xAA, 0xBB],
            recovery: UdptlRecovery::Fec {
                span: 2,
                data: vec![vec![1, 2, 3], vec![]],
            },
        };
        let enc = pkt.encode().unwrap();
        assert_eq!(
            enc,
            [
                0x01, 0x02, 0x02, 0xAA, 0xBB, 0x80, 0x01, 0x02, 0x02, 0x03, 1, 2, 3, 0x00
            ]
        );
        assert_eq!(UdptlPacket::decode(&enc).unwrap(), pkt);
    }

    #[test]
    fn fec_integer_encoding_edges() {
        for (span, int) in [
            (0u32, vec![0x00u8]),
            (127, vec![0x7F]),
            (128, vec![0x00, 0x80]),
            (255, vec![0x00, 0xFF]),
            (256, vec![0x01, 0x00]),
            (0x8000_0000, vec![0x00, 0x80, 0, 0, 0]),
            (u32::MAX, vec![0x00, 0xFF, 0xFF, 0xFF, 0xFF]),
        ] {
            let pkt = UdptlPacket {
                seq: 0,
                primary: vec![1],
                recovery: UdptlRecovery::Fec { span, data: vec![] },
            };
            let enc = pkt.encode().unwrap();
            assert_eq!(
                &enc[5..5 + 1 + int.len()],
                [&[int.len() as u8][..], &int[..]].concat()
            );
            assert_eq!(UdptlPacket::decode(&enc).unwrap(), pkt, "span {span}");
        }
    }

    #[test]
    fn decode_rejects_bad_input() {
        let good = UdptlPacket {
            seq: 5,
            primary: vec![1, 2],
            recovery: UdptlRecovery::Secondary(vec![vec![3]]),
        }
        .encode()
        .unwrap();
        for cut in 0..good.len() {
            assert!(UdptlPacket::decode(&good[..cut]).is_err(), "cut {cut}");
        }
        let mut extra = good.clone();
        extra.push(0);
        assert_eq!(
            UdptlPacket::decode(&extra),
            Err(UdptlError::TrailingData(1))
        );
        // negative / empty / oversized fec integer
        for bad in [
            &[0x80u8, 0x01, 0x80][..],
            &[0x80, 0x00][..],
            &[0x80, 0x06, 1, 2, 3, 4, 5, 6][..],
        ] {
            let mut b = vec![0, 1, 1, 9];
            b.extend_from_slice(bad);
            assert_eq!(UdptlPacket::decode(&b), Err(UdptlError::BadInteger));
        }
        // 5-octet integer that overflows u32
        assert_eq!(
            UdptlPacket::decode(&[0, 1, 1, 9, 0x80, 0x05, 0x01, 0, 0, 0, 0, 0]),
            Err(UdptlError::BadInteger)
        );
        // huge count
        assert!(matches!(
            UdptlPacket::decode(&[0, 1, 1, 9, 0x00, 0x7F]),
            Err(UdptlError::CountTooLarge(127))
        ));
        assert_eq!(
            UdptlPacket {
                seq: 0,
                primary: vec![],
                recovery: UdptlRecovery::Secondary(vec![])
            }
            .encode(),
            Err(UdptlError::EmptyIfp)
        );
    }

    #[test]
    fn large_primary_is_fragmented_and_roundtrips() {
        for len in [16383usize, 16384, 16385, 40000] {
            let pkt = UdptlPacket {
                seq: 9,
                primary: (0..len).map(|i| (i % 255) as u8).collect(),
                recovery: UdptlRecovery::Secondary(vec![]),
            };
            let enc = pkt.encode().unwrap();
            assert_eq!(UdptlPacket::decode(&enc).unwrap(), pkt, "len {len}");
        }
    }

    #[test]
    fn redundancy_depth_zero_to_four() {
        for depth in 0u8..=4 {
            let mut tx = UdptlSender::new(ErrorRecovery::Redundancy { depth }, 0);
            for i in 0u8..8 {
                let enc = tx.build(&ifp(i)).unwrap();
                let pkt = UdptlPacket::decode(&enc).unwrap();
                assert_eq!(pkt.seq, u16::from(i));
                assert_eq!(pkt.primary, ifp(i));
                let UdptlRecovery::Secondary(list) = pkt.recovery else {
                    panic!("expected redundancy")
                };
                let want = usize::from(i).min(usize::from(depth));
                assert_eq!(list.len(), want, "depth {depth} seq {i}");
                for (m, s) in list.iter().enumerate() {
                    assert_eq!(*s, ifp(i - 1 - m as u8), "depth {depth} seq {i} entry {m}");
                }
            }
        }
    }

    #[test]
    fn redundancy_respects_max_datagram() {
        let mut tx =
            UdptlSender::new(ErrorRecovery::Redundancy { depth: 3 }, 0).with_max_datagram(20);
        for i in 0u8..6 {
            let enc = tx.build(&ifp(i)).unwrap();
            assert!(enc.len() <= 20, "len {}", enc.len());
            let pkt = UdptlPacket::decode(&enc).unwrap();
            assert_eq!(pkt.primary, ifp(i));
        }
        let mut tx =
            UdptlSender::new(ErrorRecovery::Redundancy { depth: 3 }, 0).with_max_datagram(1);
        tx.build(&ifp(0)).unwrap();
        let enc = tx.build(&ifp(1)).unwrap();
        // primary is always sent, secondaries are dropped
        assert_eq!(
            UdptlPacket::decode(&enc).unwrap().recovery,
            UdptlRecovery::Secondary(vec![])
        );
    }

    #[test]
    fn fec_ramp_and_xor_sets() {
        let (span, entries) = (3u8, 2u8);
        let mut tx = UdptlSender::new(ErrorRecovery::Fec { span, entries }, 65530);
        let mut sent: Vec<Vec<u8>> = Vec::new();
        for n in 0u8..14 {
            let p = ifp(n);
            let enc = tx.build(&p).unwrap();
            let pkt = UdptlPacket::decode(&enc).unwrap();
            assert_eq!(pkt.seq, 65530u16.wrapping_add(u16::from(n)));
            let UdptlRecovery::Fec { span: s, data } = pkt.recovery else {
                panic!("expected FEC")
            };
            let n = usize::from(n);
            // ramp-up exactly as SpanDSP / Asterisk
            let (exp_s, exp_e) = if n < 6 {
                (if n < 3 { 0 } else { 3 }, n / 3)
            } else {
                (3, 2)
            };
            assert_eq!((s as usize, data.len()), (exp_s, exp_e), "n={n}");
            for (m, entry) in data.iter().enumerate() {
                let mut acc = vec![0u8; 4];
                for k in 0..exp_s {
                    let idx = n + m - exp_s * exp_e + k * exp_e;
                    for (a, b) in acc.iter_mut().zip(&sent[idx]) {
                        *a ^= b;
                    }
                }
                assert_eq!(*entry, acc, "n={n} m={m}");
            }
            sent.push(p);
        }
    }

    #[test]
    fn fec_with_unequal_lengths_xors_zero_extended() {
        let mut tx = UdptlSender::new(
            ErrorRecovery::Fec {
                span: 2,
                entries: 1,
            },
            0,
        );
        let pk = [vec![0xFF], vec![0x0F, 0xF0, 0x11], vec![0x01]];
        let mut last = Vec::new();
        for p in &pk {
            last = tx.build(p).unwrap();
        }
        // seq 2: entry 0 protects packets 0 and 1 => [FF^0F, F0, 11]
        let UdptlRecovery::Fec { span, data } = UdptlPacket::decode(&last).unwrap().recovery else {
            panic!()
        };
        assert_eq!(span, 2);
        assert_eq!(data, vec![vec![0xF0, 0xF0, 0x11]]);
    }

    #[test]
    fn fec_degenerate_params_and_mode_changes() {
        let mut tx = UdptlSender::new(
            ErrorRecovery::Fec {
                span: 0,
                entries: 3,
            },
            0,
        );
        let p = UdptlPacket::decode(&tx.build(&[1]).unwrap()).unwrap();
        assert_eq!(
            p.recovery,
            UdptlRecovery::Fec {
                span: 0,
                data: vec![]
            }
        );
        tx.set_mode(ErrorRecovery::None);
        let p = UdptlPacket::decode(&tx.build(&[2]).unwrap()).unwrap();
        assert_eq!(p.recovery, UdptlRecovery::Secondary(vec![]));
        assert_eq!(p.seq, 1);
        assert_eq!(tx.next_seq(), 2);
        assert_eq!(tx.build(&[]), Err(UdptlError::EmptyIfp));
        assert_eq!(tx.next_seq(), 2);
        assert_eq!(
            ErrorRecovery::default(),
            ErrorRecovery::Redundancy { depth: 3 }
        );
    }

    #[test]
    fn sequence_wraps() {
        let mut tx = UdptlSender::new(ErrorRecovery::Redundancy { depth: 2 }, 65535);
        let a = UdptlPacket::decode(&tx.build(&[1]).unwrap()).unwrap();
        let b = UdptlPacket::decode(&tx.build(&[2]).unwrap()).unwrap();
        assert_eq!((a.seq, b.seq), (65535, 0));
        assert_eq!(b.recovery, UdptlRecovery::Secondary(vec![vec![1]]));
    }
}
