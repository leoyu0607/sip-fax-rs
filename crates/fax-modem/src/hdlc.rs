//! HDLC framing for the T.30 control channel (T.30 section 5.3, ISO/IEC 13239): flags, bit
//! stuffing and the 16-bit frame check sequence, on a bit stream (first bit sent first; octets
//! go least significant bit first).

/// The flag octet 0111 1110.
pub const FLAG: u8 = 0x7E;

/// The longest frame accepted (address, control, FCF and FIF; T.30 limits FIF well below this).
pub const MAX_FRAME: usize = 260;

/// The FCS of `data` (ITU-T X.25 / T.30: reflected polynomial x¹⁶ + x¹² + x⁵ + 1, initial value
/// all ones, ones' complement of the remainder), as sent: low octet first.
pub fn fcs16(data: &[u8]) -> u16 {
    !crc_update(0xFFFF, data)
}

fn crc_update(mut crc: u16, data: &[u8]) -> u16 {
    for &b in data {
        crc ^= b as u16;
        for _ in 0..8 {
            crc = if crc & 1 != 0 {
                (crc >> 1) ^ 0x8408
            } else {
                crc >> 1
            };
        }
    }
    crc
}

/// The CRC of a frame with its FCS appended is this constant when the frame is intact.
const GOOD_RESIDUE: u16 = 0xF0B8;

// ------------------------------------------------------------------------------------- sender

/// Turns frames into the bits of one HDLC burst: `preamble_flags` flags, the frames separated by
/// one flag each, then `closing_flags` flags.
pub fn encode_burst(frames: &[Vec<u8>], preamble_flags: usize, closing_flags: usize) -> Vec<bool> {
    let mut bits = Vec::with_capacity(
        8 * (preamble_flags + closing_flags)
            + frames.iter().map(|f| 10 * (f.len() + 3)).sum::<usize>(),
    );
    let flag = |bits: &mut Vec<bool>| (0..8).for_each(|i| bits.push(FLAG >> i & 1 != 0));
    (0..preamble_flags.max(1)).for_each(|_| flag(&mut bits));
    for (i, f) in frames.iter().enumerate() {
        if i > 0 {
            flag(&mut bits);
        }
        let fcs = fcs16(f);
        let mut ones = 0;
        for b in f.iter().copied().chain([fcs as u8, (fcs >> 8) as u8]) {
            for i in 0..8 {
                let bit = b >> i & 1 != 0;
                bits.push(bit);
                if bit {
                    ones += 1;
                    if ones == 5 {
                        bits.push(false);
                        ones = 0;
                    }
                } else {
                    ones = 0;
                }
            }
        }
    }
    (0..closing_flags.max(1)).for_each(|_| flag(&mut bits));
    bits
}

// ----------------------------------------------------------------------------------- receiver

/// What the receiver recognised.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HdlcEvent {
    /// Flags are coming in: a burst has started (reported once, after the third flag).
    Preamble,
    /// A frame (FCS removed) and whether its FCS was right. Frames shorter than the FCS, not a
    /// whole number of octets, or longer than [`MAX_FRAME`] come out with `fcs_ok = false`.
    Frame {
        /// The frame octets.
        data: Vec<u8>,
        /// The FCS matched.
        fcs_ok: bool,
    },
    /// Seven or more ones in a row: the frame being received was abandoned.
    Abort,
}

/// Takes bits one at a time and reports frames.
#[derive(Debug, Clone, Default)]
pub struct HdlcRx {
    /// The last 8 bits seen (raw, before destuffing), newest in bit 7.
    shift: u8,
    /// Consecutive ones (raw).
    ones: u32,
    /// Collecting a frame (a flag was seen).
    in_frame: bool,
    octets: Vec<u8>,
    cur: u8,
    nbits: u32,
    overlong: bool,
    flags: u32,
    preamble_told: bool,
}

impl HdlcRx {
    /// A receiver waiting for a flag.
    pub fn new() -> Self {
        Self::default()
    }

    /// Forgets everything (the carrier went away).
    pub fn reset(&mut self) {
        *self = Self::default();
    }

    /// Feeds one bit.
    pub fn bit(&mut self, bit: bool) -> Option<HdlcEvent> {
        self.shift = (self.shift >> 1) | (u8::from(bit) << 7);
        if bit {
            self.ones += 1;
            if self.ones == 7 {
                let was = self.in_frame && (self.nbits > 0 || !self.octets.is_empty());
                self.in_frame = false;
                self.clear_frame();
                self.flags = 0;
                return was.then_some(HdlcEvent::Abort);
            }
            if self.ones > 7 {
                return None;
            }
            if self.in_frame {
                self.push_data_bit(true);
            }
            return None;
        }
        // a zero
        let ones = self.ones;
        self.ones = 0;
        if ones == 6 {
            // 0111 1110: a flag. Its leading zero and six ones went in as data bits; end_frame
            // takes them out.
            let ev = self.end_frame();
            self.in_frame = true;
            self.clear_frame();
            self.flags += 1;
            if self.flags >= 3 && !self.preamble_told {
                self.preamble_told = true;
                return Some(HdlcEvent::Preamble);
            }
            return ev;
        }
        if ones == 5 {
            // a stuffed zero
            return None;
        }
        if ones > 6 {
            // a zero after an abort: wait for a flag
            return None;
        }
        if self.in_frame {
            self.push_data_bit(false);
        }
        None
    }

    fn clear_frame(&mut self) {
        self.octets.clear();
        self.cur = 0;
        self.nbits = 0;
        self.overlong = false;
    }

    fn push_data_bit(&mut self, bit: bool) {
        if self.overlong {
            return;
        }
        self.cur |= u8::from(bit) << self.nbits;
        self.nbits += 1;
        if self.nbits == 8 {
            if self.octets.len() >= MAX_FRAME + 2 {
                self.overlong = true;
            } else {
                self.octets.push(self.cur);
            }
            self.cur = 0;
            self.nbits = 0;
        }
    }

    /// At a flag: the bits since the previous flag, minus the flag's own first seven bits
    /// (`0 1 1 1 1 1 1`, pushed as data before its closing zero showed it was a flag).
    fn end_frame(&mut self) -> Option<HdlcEvent> {
        if !self.in_frame {
            return None;
        }
        let mut total = self.octets.len() as u32 * 8 + self.nbits;
        if total < 7 {
            // flags sharing their zero
            return None;
        }
        total -= 7;
        if total == 0 {
            // back-to-back flags
            return None;
        }
        self.flags = 0;
        let mut bits: Vec<bool> = Vec::with_capacity(total as usize);
        'outer: for &o in &self.octets {
            for i in 0..8 {
                if bits.len() as u32 == total {
                    break 'outer;
                }
                bits.push(o >> i & 1 != 0);
            }
        }
        for i in 0..self.nbits {
            if bits.len() as u32 == total {
                break;
            }
            bits.push(self.cur >> i & 1 != 0);
        }
        let whole = total.is_multiple_of(8);
        let data: Vec<u8> = bits
            .chunks(8)
            .filter(|c| c.len() == 8)
            .map(|c| {
                c.iter()
                    .enumerate()
                    .fold(0u8, |a, (i, &b)| a | (u8::from(b) << i))
            })
            .collect();
        let ok_len = whole && data.len() >= 3 && data.len() <= MAX_FRAME + 2 && !self.overlong;
        let fcs_ok = ok_len && crc_update(0xFFFF, &data) == GOOD_RESIDUE;
        let mut data = data;
        data.truncate(data.len().saturating_sub(2));
        Some(HdlcEvent::Frame { data, fcs_ok })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn receive(bits: &[bool]) -> Vec<HdlcEvent> {
        let mut rx = HdlcRx::new();
        bits.iter().filter_map(|&b| rx.bit(b)).collect()
    }

    #[test]
    fn fcs_of_the_check_string() {
        // CRC-16/X-25 of "123456789"
        assert_eq!(fcs16(b"123456789"), 0x906E);
        let mut f = b"123456789".to_vec();
        let c = fcs16(&f);
        f.extend([c as u8, (c >> 8) as u8]);
        assert_eq!(crc_update(0xFFFF, &f), GOOD_RESIDUE);
    }

    #[test]
    fn frames_come_back_with_stuffing_undone() {
        // DIS-like frames, one full of ones (forces stuffing everywhere)
        let frames = vec![
            vec![0xFF, 0x13, 0x80, 0x00, 0xEE, 0xF8],
            vec![0xFF; 20],
            vec![0xFF, 0xC8, 0x5F],
        ];
        let bits = encode_burst(&frames, 10, 2);
        // no six ones in a row anywhere but in flags: count ones runs
        let ev = receive(&bits);
        assert_eq!(ev[0], HdlcEvent::Preamble);
        let got: Vec<_> = ev[1..].to_vec();
        assert_eq!(
            got,
            frames
                .iter()
                .map(|f| HdlcEvent::Frame {
                    data: f.clone(),
                    fcs_ok: true
                })
                .collect::<Vec<_>>()
        );
    }

    #[test]
    fn a_flipped_bit_fails_the_fcs_and_seven_ones_abort() {
        let frames = vec![vec![0xFF, 0x13, 0x80]];
        let mut bits = encode_burst(&frames, 4, 1);
        // flip a bit inside the frame (after the 4 preamble flags)
        bits[32 + 9] = !bits[32 + 9];
        let ev = receive(&bits);
        assert!(
            matches!(ev.last(), Some(HdlcEvent::Frame { fcs_ok: false, .. })),
            "{ev:?}"
        );

        let mut bits = encode_burst(&[], 4, 0);
        bits.extend([true, false, true, true, false, false, true, false]); // some data bits
        bits.extend([true; 7]);
        let ev = receive(&bits);
        assert_eq!(ev.last(), Some(&HdlcEvent::Abort));
    }

    #[test]
    fn short_and_odd_frames_are_bad_not_panics() {
        // flag, 2 data octets (shorter than an FCS + 1), flag
        let mut bits = encode_burst(&[], 3, 0);
        for b in [0x12u8, 0x34] {
            (0..8).for_each(|i| bits.push(b >> i & 1 != 0));
        }
        (0..8).for_each(|i| bits.push(FLAG >> i & 1 != 0));
        let ev = receive(&bits);
        assert!(
            matches!(ev.last(), Some(HdlcEvent::Frame { fcs_ok: false, .. })),
            "{ev:?}"
        );
        // garbage never panics
        let mut x = 0x1234_5678u32;
        let noise: Vec<bool> = (0..100_000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x & 1 != 0
            })
            .collect();
        let _ = receive(&noise);
    }
}
