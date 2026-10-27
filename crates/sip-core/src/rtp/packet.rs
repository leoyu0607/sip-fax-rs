//! RTP packets (RFC 3550 section 5.1).
//!
//! Extracted from Sipress `core/src/rtp/packet.rs` (MIT, Copyright (c) 2025 LeoYu). Changed: the
//! decoder now reads the marker bit, the CSRC list, the header extension and the padding (the
//! original took everything after byte 12 as the payload), refuses packets whose lengths do not
//! add up, and returns why; the encoder writes all of them; `received_at_us` is gone (the
//! receiver keeps its own times).

use thiserror::Error;

/// Size of the fixed header.
pub const RTP_HEADER_SIZE: usize = 12;

/// The header extension (RFC 3550 section 5.3.1): profile-defined id and 32-bit words.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extension {
    /// The 16 profile-defined bits.
    pub profile: u16,
    /// The extension data, a multiple of 4 bytes.
    pub data: Vec<u8>,
}

/// One RTP packet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RtpPacket {
    /// The marker bit.
    pub marker: bool,
    /// Payload type (7 bits).
    pub payload_type: u8,
    /// Sequence number.
    pub sequence: u16,
    /// Timestamp (8 kHz for G.711: +160 per 20 ms).
    pub timestamp: u32,
    /// Synchronization source.
    pub ssrc: u32,
    /// Contributing sources (at most 15).
    pub csrc: Vec<u32>,
    /// The header extension, if any.
    pub extension: Option<Extension>,
    /// The payload, without padding.
    pub payload: Vec<u8>,
}

/// Why a datagram is not an RTP packet.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum RtpError {
    /// Shorter than the fixed header, or than the CSRC list / extension it announces.
    #[error("truncated RTP packet ({0} bytes)")]
    Truncated(usize),
    /// The version is not 2.
    #[error("RTP version {0} (expected 2)")]
    Version(u8),
    /// The padding count is zero or longer than what follows the header.
    #[error("bad RTP padding length {0}")]
    Padding(u8),
}

impl RtpPacket {
    /// A packet with no marker, CSRC or extension.
    pub fn new(
        payload_type: u8,
        sequence: u16,
        timestamp: u32,
        ssrc: u32,
        payload: Vec<u8>,
    ) -> Self {
        Self {
            marker: false,
            payload_type,
            sequence,
            timestamp,
            ssrc,
            csrc: Vec::new(),
            extension: None,
            payload,
        }
    }

    /// The datagram (no padding is added). At most 15 CSRCs are written; extension data is
    /// padded with zeros to a multiple of 4 bytes and cut at 65 535 words (the length field).
    pub fn encode(&self) -> Vec<u8> {
        let cc = self.csrc.len().min(15);
        let ext_words = self
            .extension
            .as_ref()
            .map_or(0, |e| e.data.len().div_ceil(4))
            .min(usize::from(u16::MAX));
        let mut buf =
            Vec::with_capacity(RTP_HEADER_SIZE + 4 * cc + 4 + 4 * ext_words + self.payload.len());
        let x = if self.extension.is_some() { 0x10 } else { 0 };
        buf.push(0x80 | x | cc as u8);
        buf.push((u8::from(self.marker) << 7) | (self.payload_type & 0x7F));
        buf.extend_from_slice(&self.sequence.to_be_bytes());
        buf.extend_from_slice(&self.timestamp.to_be_bytes());
        buf.extend_from_slice(&self.ssrc.to_be_bytes());
        for c in &self.csrc[..cc] {
            buf.extend_from_slice(&c.to_be_bytes());
        }
        if let Some(e) = &self.extension {
            buf.extend_from_slice(&e.profile.to_be_bytes());
            buf.extend_from_slice(&(ext_words as u16).to_be_bytes());
            let data = &e.data[..e.data.len().min(4 * ext_words)];
            buf.extend_from_slice(data);
            buf.resize(buf.len() + (4 * ext_words - data.len()), 0);
        }
        buf.extend_from_slice(&self.payload);
        buf
    }

    /// Parses a datagram.
    pub fn decode(data: &[u8]) -> Result<Self, RtpError> {
        if data.len() < RTP_HEADER_SIZE {
            return Err(RtpError::Truncated(data.len()));
        }
        let version = data[0] >> 6;
        if version != 2 {
            return Err(RtpError::Version(version));
        }
        let padding = data[0] & 0x20 != 0;
        let has_ext = data[0] & 0x10 != 0;
        let cc = (data[0] & 0x0F) as usize;
        let marker = data[1] & 0x80 != 0;
        let payload_type = data[1] & 0x7F;
        let sequence = u16::from_be_bytes([data[2], data[3]]);
        let timestamp = u32::from_be_bytes([data[4], data[5], data[6], data[7]]);
        let ssrc = u32::from_be_bytes([data[8], data[9], data[10], data[11]]);

        let mut pos = RTP_HEADER_SIZE;
        let csrc_end = pos + 4 * cc;
        if data.len() < csrc_end {
            return Err(RtpError::Truncated(data.len()));
        }
        let csrc = data[pos..csrc_end]
            .as_chunks::<4>()
            .0
            .iter()
            .map(|c| u32::from_be_bytes(*c))
            .collect();
        pos = csrc_end;

        let extension = if has_ext {
            if data.len() < pos + 4 {
                return Err(RtpError::Truncated(data.len()));
            }
            let profile = u16::from_be_bytes([data[pos], data[pos + 1]]);
            let words = u16::from_be_bytes([data[pos + 2], data[pos + 3]]) as usize;
            let end = pos + 4 + 4 * words;
            if data.len() < end {
                return Err(RtpError::Truncated(data.len()));
            }
            let ext = Extension {
                profile,
                data: data[pos + 4..end].to_vec(),
            };
            pos = end;
            Some(ext)
        } else {
            None
        };

        let mut end = data.len();
        if padding {
            let n = data[end - 1];
            if n == 0 || n as usize > end - pos {
                return Err(RtpError::Padding(n));
            }
            end -= n as usize;
        }
        Ok(Self {
            marker,
            payload_type,
            sequence,
            timestamp,
            ssrc,
            csrc,
            extension,
            payload: data[pos..end].to_vec(),
        })
    }
}

#[cfg(test)]
mod tests;
