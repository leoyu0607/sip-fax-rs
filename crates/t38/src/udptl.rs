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
mod tests;
