//! ITU-T G.711 A-law and μ-law, on 16-bit linear samples.
//!
//! Written from the recommendation (the segment / quantization tables of G.711 Tables 1a, 1b,
//! 2a, 2b, with the usual 16-bit scaling of the ITU reference implementation in G.191). Sipress'
//! codec (`core/src/rtp/audio.rs`) was the starting point but is not used: its A-law decoder
//! returned 13-bit values, its A-law encoder lost the mantissa in the first two segments and its
//! μ-law decoder was off by up to 99 at full scale (see `docs/sipress-extraction.md`).

/// μ-law bias (G.711 Table 2a, scaled to 16 bits).
const ULAW_BIAS: i32 = 0x84;

/// Index of the highest set bit (`v` > 0).
const fn top_bit(v: i32) -> i32 {
    31 - v.leading_zeros() as i32
}

/// 16-bit linear → A-law.
pub const fn linear_to_alaw(sample: i16) -> u8 {
    let (mask, linear) = if sample >= 0 {
        (0xD5u8, sample as i32)
    } else {
        (0x55u8, -(sample as i32) - 1)
    };
    let seg = top_bit(linear | 0xFF) - 7;
    if seg >= 8 {
        return 0x7F ^ mask;
    }
    let shift = if seg == 0 { 4 } else { seg + 3 };
    let aval = ((seg << 4) | ((linear >> shift) & 0x0F)) as u8;
    aval ^ mask
}

/// A-law → 16-bit linear (the midpoint of the quantization interval).
pub const fn alaw_to_linear(a: u8) -> i16 {
    let a = a ^ 0x55;
    let mut t = ((a & 0x0F) as i32) << 4;
    let seg = ((a & 0x70) >> 4) as i32;
    if seg == 0 {
        t += 8;
    } else {
        t = (t + 0x108) << (seg - 1);
    }
    if a & 0x80 != 0 { t as i16 } else { -t as i16 }
}

/// 16-bit linear → μ-law.
pub const fn linear_to_ulaw(sample: i16) -> u8 {
    // mu-law is a sign-magnitude law: -x and x differ only in the sign bit (A-law below takes the
    // one's complement of a negative value instead, as the A-law tables do)
    let (mask, linear) = if sample < 0 {
        (0x7Fu8, ULAW_BIAS - sample as i32)
    } else {
        (0xFFu8, ULAW_BIAS + sample as i32)
    };
    let seg = top_bit(linear | 0xFF) - 7;
    if seg >= 8 {
        return 0x7F ^ mask;
    }
    let uval = ((seg << 4) | ((linear >> (seg + 3)) & 0x0F)) as u8;
    uval ^ mask
}

/// μ-law → 16-bit linear.
pub const fn ulaw_to_linear(u: u8) -> i16 {
    let u = !u;
    let t = ((((u & 0x0F) as i32) << 3) + ULAW_BIAS) << ((u & 0x70) >> 4);
    if u & 0x80 != 0 {
        (ULAW_BIAS - t) as i16
    } else {
        (t - ULAW_BIAS) as i16
    }
}

const fn table(alaw: bool) -> [i16; 256] {
    let mut t = [0i16; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = if alaw {
            alaw_to_linear(i as u8)
        } else {
            ulaw_to_linear(i as u8)
        };
        i += 1;
    }
    t
}

static ALAW: [i16; 256] = table(true);
static ULAW: [i16; 256] = table(false);

/// The two G.711 laws, with their RTP payload types (RFC 3551).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Law {
    /// μ-law, PCMU, payload type 0.
    Ulaw,
    /// A-law, PCMA, payload type 8.
    Alaw,
}

impl Law {
    /// The static RTP payload type.
    pub const fn payload_type(self) -> u8 {
        match self {
            Law::Ulaw => 0,
            Law::Alaw => 8,
        }
    }

    /// The law of a static payload type, if it is one.
    pub const fn from_payload_type(pt: u8) -> Option<Law> {
        match pt {
            0 => Some(Law::Ulaw),
            8 => Some(Law::Alaw),
            _ => None,
        }
    }

    /// The SDP encoding name (`PCMU` / `PCMA`).
    pub const fn encoding_name(self) -> &'static str {
        match self {
            Law::Ulaw => "PCMU",
            Law::Alaw => "PCMA",
        }
    }

    /// The code of silence (linear 0).
    pub const fn silence(self) -> u8 {
        match self {
            Law::Ulaw => 0xFF,
            Law::Alaw => 0xD5,
        }
    }

    /// Decodes one code.
    pub fn decode_one(self, code: u8) -> i16 {
        match self {
            Law::Ulaw => ULAW[code as usize],
            Law::Alaw => ALAW[code as usize],
        }
    }

    /// Encodes one sample.
    pub const fn encode_one(self, sample: i16) -> u8 {
        match self {
            Law::Ulaw => linear_to_ulaw(sample),
            Law::Alaw => linear_to_alaw(sample),
        }
    }

    /// Decodes `codes`, appending to `out`.
    pub fn decode(self, codes: &[u8], out: &mut Vec<i16>) {
        let t = match self {
            Law::Ulaw => &ULAW,
            Law::Alaw => &ALAW,
        };
        out.extend(codes.iter().map(|&c| t[c as usize]));
    }

    /// Encodes `samples`, appending to `out`.
    pub fn encode(self, samples: &[i16], out: &mut Vec<u8>) {
        out.extend(samples.iter().map(|&s| self.encode_one(s)));
    }
}

#[cfg(test)]
mod tests;
