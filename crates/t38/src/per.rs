//! ALIGNED PER (ITU-T X.691) primitives needed by the T.38 IFP and UDPTL codecs.
//!
//! Only the handful of constructs that appear in the T.38 ASN.1 are implemented: single bits,
//! small bit fields, octet alignment, octet strings and the unconstrained length determinant
//! including fragmentation (X.691 section 10.9.3.8).

use thiserror::Error;

/// Largest item count a single (unfragmented) length determinant can carry, plus one.
pub const FRAGMENT_UNIT: usize = 16_384;

/// Largest multiplier of [`FRAGMENT_UNIT`] a fragment header can announce.
pub const MAX_FRAGMENT_MULTIPLIER: usize = 4;

/// A low-level PER decoding failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Error)]
pub enum PerError {
    /// The input ended before the construct was complete.
    #[error("input truncated")]
    Truncated,
    /// A fragmentation header announced a multiplier outside 1..=4.
    #[error("invalid length fragmentation header {0:#04x}")]
    InvalidFragment(u8),
}

/// Bit-level writer producing an ALIGNED PER encoding.
#[derive(Debug, Default, Clone)]
pub struct BitWriter {
    buf: Vec<u8>,
    /// Number of bits already used in the last byte of `buf` (0 means "byte boundary").
    used: u8,
}

impl BitWriter {
    /// Creates an empty writer.
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends one bit.
    pub fn write_bit(&mut self, bit: bool) {
        self.write_bits(u32::from(bit), 1);
    }

    /// Appends the low `n` bits of `value`, most significant first. `n` is clamped to 32.
    pub fn write_bits(&mut self, value: u32, n: u8) {
        let n = n.min(32);
        for i in (0..n).rev() {
            let bit = ((value >> i) & 1) as u8;
            if self.used == 0 {
                self.buf.push(0);
            }
            if let Some(last) = self.buf.last_mut() {
                *last |= bit << (7 - self.used);
            }
            self.used = (self.used + 1) % 8;
        }
    }

    /// Pads with zero bits up to the next octet boundary.
    pub fn align(&mut self) {
        self.used = 0;
    }

    /// Aligns, then appends whole octets.
    pub fn write_bytes(&mut self, bytes: &[u8]) {
        self.align();
        self.buf.extend_from_slice(bytes);
    }

    /// Aligns, then appends a 16-bit big-endian value.
    pub fn write_u16(&mut self, v: u16) {
        self.write_bytes(&v.to_be_bytes());
    }

    /// Encodes one chunk of an unconstrained length determinant (X.691 10.9.3.5-8).
    ///
    /// `chunk` below 16384 uses the one or two octet form; a non-zero multiple of 16384 up to
    /// 4 * 16384 uses the fragment form. Other values are rounded down to a legal chunk by
    /// [`plan_fragments`], which is the only producer of chunks.
    pub fn write_length_chunk(&mut self, chunk: usize) {
        self.align();
        if chunk < 0x80 {
            self.buf.push(chunk as u8);
        } else if chunk < FRAGMENT_UNIT {
            self.buf.push(0x80 | (chunk >> 8) as u8);
            self.buf.push((chunk & 0xFF) as u8);
        } else {
            let m = (chunk / FRAGMENT_UNIT).min(MAX_FRAGMENT_MULTIPLIER);
            self.buf.push(0xC0 | m as u8);
        }
    }

    /// Number of whole octets produced so far (a partial octet counts as one).
    pub fn len(&self) -> usize {
        self.buf.len()
    }

    /// True if nothing has been written.
    pub fn is_empty(&self) -> bool {
        self.buf.is_empty()
    }

    /// Finishes the encoding; a trailing partial octet is zero padded.
    pub fn into_bytes(self) -> Vec<u8> {
        self.buf
    }
}

/// Splits `total` items into the chunk sizes of a fragmented length determinant.
///
/// Every chunk except the last is a multiple of 16384 (at most 4 * 16384); the last chunk is
/// below 16384 and may be 0 (X.691 10.9.3.8.3: a total that is an exact multiple of 16384 is
/// terminated by an empty chunk).
pub fn plan_fragments(total: usize) -> Vec<usize> {
    let mut chunks = Vec::new();
    let mut remaining = total;
    loop {
        if remaining < FRAGMENT_UNIT {
            chunks.push(remaining);
            return chunks;
        }
        let m = (remaining / FRAGMENT_UNIT).min(MAX_FRAGMENT_MULTIPLIER);
        let take = m * FRAGMENT_UNIT;
        chunks.push(take);
        remaining -= take;
    }
}

/// Bit-level reader over an ALIGNED PER encoding.
#[derive(Debug, Clone)]
pub struct BitReader<'a> {
    data: &'a [u8],
    /// Current position in bits.
    pos: usize,
}

impl<'a> BitReader<'a> {
    /// Creates a reader at the start of `data`.
    pub fn new(data: &'a [u8]) -> Self {
        Self { data, pos: 0 }
    }

    /// Bits not yet consumed.
    pub fn remaining_bits(&self) -> usize {
        (self.data.len() * 8).saturating_sub(self.pos)
    }

    /// Octets not yet consumed after aligning (a partially consumed octet is skipped).
    pub fn remaining_bytes(&self) -> usize {
        self.data.len().saturating_sub(self.pos.div_ceil(8))
    }

    /// Reads one bit.
    pub fn read_bit(&mut self) -> Result<bool, PerError> {
        Ok(self.read_bits(1)? == 1)
    }

    /// Reads `n` (at most 32) bits, most significant first.
    pub fn read_bits(&mut self, n: u8) -> Result<u32, PerError> {
        let n = usize::from(n.min(32));
        if self.remaining_bits() < n {
            return Err(PerError::Truncated);
        }
        let mut v = 0u32;
        for _ in 0..n {
            let byte = self
                .data
                .get(self.pos / 8)
                .copied()
                .ok_or(PerError::Truncated)?;
            v = (v << 1) | u32::from((byte >> (7 - (self.pos % 8))) & 1);
            self.pos += 1;
        }
        Ok(v)
    }

    /// Skips padding up to the next octet boundary.
    pub fn align(&mut self) {
        self.pos = self.pos.div_ceil(8) * 8;
    }

    /// Aligns, then reads `n` octets.
    pub fn read_bytes(&mut self, n: usize) -> Result<&'a [u8], PerError> {
        self.align();
        let start = self.pos / 8;
        let end = start.checked_add(n).ok_or(PerError::Truncated)?;
        let out = self.data.get(start..end).ok_or(PerError::Truncated)?;
        self.pos = end * 8;
        Ok(out)
    }

    /// Aligns, then reads a 16-bit big-endian value.
    pub fn read_u16(&mut self) -> Result<u16, PerError> {
        let b = self.read_bytes(2)?;
        Ok(u16::from_be_bytes([
            b.first().copied().unwrap_or(0),
            b.get(1).copied().unwrap_or(0),
        ]))
    }

    /// Reads one chunk of an unconstrained length determinant.
    ///
    /// Returns `(count, more)`; `more` is true for a fragment header, in which case the caller
    /// must read `count` items and then another chunk.
    pub fn read_length_chunk(&mut self) -> Result<(usize, bool), PerError> {
        self.align();
        let b0 = self.read_bytes(1)?.first().copied().unwrap_or(0);
        if b0 & 0x80 == 0 {
            return Ok((usize::from(b0), false));
        }
        if b0 & 0x40 == 0 {
            let b1 = self.read_bytes(1)?.first().copied().unwrap_or(0);
            return Ok(((usize::from(b0 & 0x3F) << 8) | usize::from(b1), false));
        }
        let m = usize::from(b0 & 0x3F);
        if !(1..=MAX_FRAGMENT_MULTIPLIER).contains(&m) {
            return Err(PerError::InvalidFragment(b0));
        }
        Ok((m * FRAGMENT_UNIT, true))
    }

    /// True if only zero octets remain after aligning.
    pub fn rest_is_zero_padding(&self) -> bool {
        let start = self.pos.div_ceil(8);
        self.data
            .get(start..)
            .is_none_or(|r| r.iter().all(|&b| b == 0))
    }

    /// True if no octet remains after aligning.
    pub fn at_end(&self) -> bool {
        self.remaining_bytes() == 0
    }

    /// Octets consumed so far, counting a partial octet as consumed.
    pub fn consumed_bytes(&self) -> usize {
        self.pos.div_ceil(8)
    }
}

/// Encodes an unconstrained octet string (or open type): length determinant (with
/// fragmentation) followed by the octets.
pub fn write_octet_string(w: &mut BitWriter, data: &[u8]) {
    let mut offset = 0usize;
    for chunk in plan_fragments(data.len()) {
        w.write_length_chunk(chunk);
        let end = offset.saturating_add(chunk).min(data.len());
        w.write_bytes(data.get(offset..end).unwrap_or(&[]));
        offset = end;
    }
}

/// Decodes an unconstrained octet string (or open type), reassembling fragments.
pub fn read_octet_string(r: &mut BitReader<'_>) -> Result<Vec<u8>, PerError> {
    let mut out = Vec::new();
    loop {
        let (n, more) = r.read_length_chunk()?;
        out.extend_from_slice(r.read_bytes(n)?);
        if !more {
            return Ok(out);
        }
    }
}

#[cfg(test)]
mod tests;
