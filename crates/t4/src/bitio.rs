//! MSB-first bit reader over a growing buffer and bit writer.

/// Bit reader over a buffer that can be extended (streaming). Reads past the end yield zeros;
/// callers compare against [`Bits::avail`] to detect that.
#[derive(Debug, Default)]
pub(crate) struct Bits {
    buf: Vec<u8>,
    /// Current position in bits from the start of `buf`.
    pub pos: usize,
    /// No more data will be appended.
    pub eof: bool,
}

impl Bits {
    pub fn extend(&mut self, data: &[u8]) {
        self.buf.extend_from_slice(data);
    }

    /// Drop fully consumed bytes.
    pub fn compact(&mut self) {
        let n = (self.pos / 8).min(self.buf.len());
        if n > 0 {
            self.buf.drain(..n);
            self.pos -= n * 8;
        }
    }

    pub fn total(&self) -> usize {
        self.buf.len() * 8
    }

    /// Bits left after the current position.
    pub fn avail(&self) -> usize {
        self.total().saturating_sub(self.pos)
    }

    /// Bit at absolute bit index `i` (0 past the end).
    pub fn bit_at(&self, i: usize) -> u8 {
        self.buf.get(i >> 3).map_or(0, |b| (b >> (7 - (i & 7))) & 1)
    }

    /// Next `n` (<= 24) bits as an integer; missing bits read as zero.
    pub fn peek(&self, n: u32) -> u32 {
        let byte = self.pos >> 3;
        let sh = (self.pos & 7) as u32;
        let mut v = 0u64;
        for i in 0..5 {
            v = (v << 8) | u64::from(self.buf.get(byte + i).copied().unwrap_or(0));
        }
        ((v >> (40 - sh - n)) & ((1u64 << n) - 1)) as u32
    }

    /// True when every remaining bit is zero.
    pub fn rest_is_zero(&self) -> bool {
        let mut i = self.pos;
        while i < self.total() {
            if i & 7 == 0 && self.buf.get(i >> 3).copied() == Some(0) {
                i += 8;
                continue;
            }
            if self.bit_at(i) != 0 {
                return false;
            }
            i += 1;
        }
        true
    }
}

/// MSB-first bit writer.
#[derive(Debug, Default)]
pub(crate) struct BitWriter {
    buf: Vec<u8>,
    acc: u32,
    nbits: u32,
}

impl BitWriter {
    /// Append the low `bits` (<= 24) bits of `value`.
    pub fn put(&mut self, value: u32, bits: u32) {
        self.acc = (self.acc << bits) | (value & ((1u32 << bits) - 1));
        self.nbits += bits;
        while self.nbits >= 8 {
            self.nbits -= 8;
            self.buf.push((self.acc >> self.nbits) as u8);
        }
        self.acc &= (1u32 << self.nbits) - 1;
    }

    /// Bits written so far.
    pub fn bit_len(&self) -> usize {
        self.buf.len() * 8 + self.nbits as usize
    }

    /// Pad with zero bits to a byte boundary and return the bytes.
    pub fn finish(mut self) -> Vec<u8> {
        if self.nbits > 0 {
            let pad = 8 - self.nbits;
            self.put(0, pad);
        }
        self.buf
    }
}

#[cfg(test)]
mod tests;
