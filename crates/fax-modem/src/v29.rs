//! ITU-T V.29 building blocks: 9600 and 7200 bit/s, amplitude and phase modulation on a 1700 Hz
//! carrier at 2400 baud. This file has the transmit side only (the signal space, the scrambler,
//! the training sequence and a test transmitter); the receiver follows.
//!
//! From the recommendation (§ 2.2, Tables 1 and 2, Figure 4, § 8 with Table 5, § 9, Appendices I
//! and II):
//!
//! * encoding: at 9600 bit/s a quadbit (Q1 Q2 Q3 Q4, the first bit in time is Q1) gives the
//!   amplitude bit Q1 and the phase change Q2 Q3 Q4 (Table 1); at 7200 bit/s a tribit gives the
//!   phase change only (its bits are Q2 Q3 Q4) and Q1 is zero;
//! * points: the absolute phase (steps of 45°, 0 on the +x axis) and Q1 give the amplitude
//!   (Table 2), so 9600 has 16 points and 7200 has 8. The points have integer coordinates (in
//!   phase, quadrature);
//! * turn-on (§ 8): Segment 1 no energy (48 symbols), Segment 2 A B A B … (128), Segment 3
//!   equalizer conditioning from the 7-stage generator of Appendix I (384, 0 → C, 1 → D),
//!   Segment 4 scrambled ones (48), then the data;
//! * the points of Segments 2 and 3 are absolute, so the phase reference of Segment 4 is the
//!   last point of Segment 3;
//! * scrambler (§ 9, Appendix II): 1 + x⁻¹⁸ + x⁻²³, self-synchronising, without the guard of
//!   V.27 ter. It starts at zero at Segment 4 (Segments 1 to 3 do not feed it).
//!
//! Written from the recommendation (see the notes in `.v29-spec.md`); no SpanDSP code is used.

// Until the receiver lands, only the test transmitter uses the signal space, so the rest is
// dead code outside the tests and the `testtx` feature.
#![cfg_attr(not(any(test, feature = "testtx")), allow(dead_code))]

/// Carrier frequency (Hz).
pub const CARRIER_HZ: f32 = 1700.0;

/// Segment 1 (no energy), in symbols.
pub const SEGMENT_1: usize = 48;
/// Segment 2 (A B alternation), in symbols.
pub const SEGMENT_2: usize = 128;
/// Segment 3 (equalizer conditioning), in symbols.
pub const SEGMENT_3: usize = 384;
/// Segment 4 (scrambled ones), in symbols.
pub const SEGMENT_4: usize = 48;

/// The two speeds used by fax (T.30 has no V.29 4800 bit/s).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rate {
    /// 9600 bit/s: quadbits, 16 points.
    B9600,
    /// 7200 bit/s: tribits, 8 points.
    B7200,
}

impl Rate {
    /// Symbols per second.
    pub fn baud(self) -> f32 {
        2400.0
    }

    /// Bits per symbol.
    pub fn bits_per_symbol(self) -> usize {
        match self {
            Rate::B9600 => 4,
            Rate::B7200 => 3,
        }
    }
}

// --------------------------------------------------------------------------------- scrambler

/// The scrambled bits kept by the scrambler: x⁻²³ is the oldest of them.
const HISTORY_MASK: u32 = (1 << 23) - 1;

/// The scrambler and descrambler (§ 9, Appendix II): 1 + x⁻¹⁸ + x⁻²³, self-synchronising.
#[derive(Debug, Clone, Default)]
pub struct Scrambler {
    /// The scrambled bits sent so far, newest in bit 0: bit k is the bit sent k + 1 bits ago.
    reg: u32,
}

impl Scrambler {
    /// Scrambles one data bit into a line bit: Ds = Di ⊕ Ds·x⁻¹⁸ ⊕ Ds·x⁻²³.
    pub fn scramble(&mut self, bit: bool) -> bool {
        let y = u32::from(bit) ^ self.tap();
        self.push(y);
        y != 0
    }

    /// Descrambles one line bit: Do = Ds ⊕ Ds·x⁻¹⁸ ⊕ Ds·x⁻²³.
    pub fn descramble(&mut self, line: bool) -> bool {
        let r = u32::from(line);
        let bit = r ^ self.tap();
        self.push(r);
        bit != 0
    }

    /// Ds·x⁻¹⁸ ⊕ Ds·x⁻²³ (the bits sent 18 and 23 bits ago).
    fn tap(&self) -> u32 {
        ((self.reg >> 17) ^ (self.reg >> 22)) & 1
    }

    fn push(&mut self, bit: u32) {
        self.reg = ((self.reg << 1) | bit) & HISTORY_MASK;
    }
}

// ----------------------------------------------------------------------- training sequence

/// The Segment 3 generator (Appendix I, Figure I-1): seven stages, 1 + x⁻⁶ + x⁻⁷.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TrainingPrbs {
    /// Stage x⁻¹ is bit 6 and stage x⁻⁷ is bit 0, so `{:07b}` reads the stages as the
    /// recommendation writes them.
    state: u8,
}

impl Default for TrainingPrbs {
    /// The initial state 0101010.
    fn default() -> Self {
        Self { state: 0b010_1010 }
    }
}

impl TrainingPrbs {
    /// The next bit: the stage x⁻⁷ before the shift; then x⁻¹ takes x⁻⁶ ⊕ x⁻⁷.
    pub fn next_bit(&mut self) -> bool {
        let out = self.state & 1;
        let fb = ((self.state >> 1) ^ self.state) & 1;
        self.state = (self.state >> 1) | (fb << 6);
        out != 0
    }
}

// ----------------------------------------------------------------------- signal space

/// The unit steps of the phase, 45° each, from the +x axis (integer components).
const DIRECTIONS: [(i8, i8); 8] = [
    (1, 0),
    (1, 1),
    (0, 1),
    (-1, 1),
    (-1, 0),
    (-1, -1),
    (0, -1),
    (1, -1),
];

/// Table 1: the phase change (steps of 45°) for Q2 Q3 Q4 = 000, 001, …, 111.
const PHASE_CHANGE: [u8; 8] = [1, 0, 2, 3, 6, 7, 5, 4];

/// Table 1: the phase change, in steps of 45°, of Q2 Q3 Q4 (the low three bits of `q234`).
pub(crate) fn phase_change(q234: u8) -> u8 {
    PHASE_CHANGE[usize::from(q234 & 7)]
}

/// The phase change and Q1 of one group of bits, in time order (Q1 first at 9600 bit/s; the
/// tribit of 7200 bit/s is Q2 Q3 Q4 and Q1 is zero).
pub(crate) fn group_change(rate: Rate, bits: &[bool]) -> (u8, bool) {
    let v = bits.iter().fold(0u8, |a, &b| (a << 1) | u8::from(b));
    match rate {
        Rate::B9600 => (phase_change(v), v & 8 != 0),
        Rate::B7200 => (phase_change(v), false),
    }
}

/// Table 2: the point (in phase, quadrature) of an absolute phase (steps of 45°) and Q1.
/// Phases on the axes: amplitude 3 (Q1 = 0) or 5 (Q1 = 1). Phases on the diagonals: amplitude
/// √2 (Q1 = 0) or 3√2 (Q1 = 1), which are the integer points (1, 1) and (3, 3).
pub(crate) fn point(phase: u8, q1: bool) -> (i8, i8) {
    let p = usize::from(phase & 7);
    let (x, y) = DIRECTIONS[p];
    let k = match (p.is_multiple_of(2), q1) {
        (true, false) => 3,
        (true, true) => 5,
        (false, false) => 1,
        (false, true) => 3,
    };
    (x * k, y * k)
}

/// The absolute phase step (0..8) of a point, from the signs of its coordinates.
pub(crate) fn phase_of(p: (i8, i8)) -> u8 {
    let s = (p.0.signum(), p.1.signum());
    DIRECTIONS
        .iter()
        .position(|&d| d == s)
        .map_or(0, |i| i as u8)
}

/// Figure 4: the points A, B, C and D of `rate`, in that order. A (180°) and C (0°) have
/// amplitude 3 at both speeds. B (315°) and D (135°) have amplitude 3√2 at 9600 bit/s (Q1 = 1)
/// and √2 at 7200 bit/s (Q1 = 0).
pub(crate) fn figure4(rate: Rate) -> [(i8, i8); 4] {
    let q1 = rate == Rate::B9600;
    [point(4, false), point(7, q1), point(0, false), point(3, q1)]
}

// ------------------------------------------------------------------------- test transmitter

/// The V.29 test transmitter, for the tests of this crate and of others (feature `testtx`).
#[cfg(any(test, feature = "testtx"))]
pub mod testtx {
    use super::*;
    use crate::dsp::{C32, rrc};
    use crate::level::mean_square;

    /// Silence before the signal (samples at 8 kHz).
    const LEAD: usize = 400;
    /// Scrambled ones after the data (symbols).
    const TAIL_SYMBOLS: usize = 8;
    /// Roll-off of the root-raised-cosine pulse.
    const ALPHA: f32 = 0.25;

    /// A V.29 transmitter: turn-on sequence, then scrambled data.
    pub struct Tx {
        /// The speed.
        pub rate: Rate,
        /// Level of the signal (dBm0).
        pub level_dbm0: f32,
        /// Offset of the carrier from 1700 Hz (Hz).
        pub carrier_offset_hz: f32,
        /// Sample rate of the sender relative to 8000 Hz (1.0001 = +100 ppm).
        pub clock: f64,
    }

    impl Tx {
        /// The transmitted points of Segments 2 to 4 and of `data` (padded with ones to whole
        /// symbols), as integer coordinates.
        pub fn symbols(&self, data: &[bool]) -> Vec<(i8, i8)> {
            self.points(data, 0)
        }

        /// The points of Segments 2 to 4 and of `data`, then `tail` symbols of scrambled ones.
        fn points(&self, data: &[bool], tail: usize) -> Vec<(i8, i8)> {
            let [a, b, c, d] = figure4(self.rate);
            // Segment 2: A B A B …
            let mut out: Vec<(i8, i8)> = (0..SEGMENT_2)
                .map(|i| if i % 2 == 0 { a } else { b })
                .collect();
            // Segment 3: the generator's bit 0 → C, 1 → D
            let mut prbs = TrainingPrbs::default();
            out.extend((0..SEGMENT_3).map(|_| if prbs.next_bit() { d } else { c }));
            // Segment 4 (ones) and the data, scrambled from zero, in groups of bits; the phase
            // reference is the last point of Segment 3
            let n = self.rate.bits_per_symbol();
            let mut bits = vec![true; SEGMENT_4 * n];
            bits.extend_from_slice(data);
            while !bits.len().is_multiple_of(n) {
                bits.push(true);
            }
            bits.extend(std::iter::repeat_n(true, tail * n));
            let mut s = Scrambler::default();
            let line: Vec<bool> = bits.iter().map(|&b| s.scramble(b)).collect();
            let mut phase = out.last().map_or(0, |&p| phase_of(p));
            for g in line.chunks(n) {
                let (change, q1) = group_change(self.rate, g);
                phase = (phase + change) % 8;
                out.push(point(phase, q1));
            }
            out
        }

        /// The line signal of `data` after the turn-on sequence (Segment 1 is silence of 48
        /// symbols), with silence before and after.
        pub fn modulate(&self, data: &[bool]) -> Vec<i16> {
            let mut syms = vec![C32::default(); SEGMENT_1];
            syms.extend(
                self.points(data, TAIL_SYMBOLS)
                    .into_iter()
                    .map(|(x, y)| C32::new(f32::from(x), f32::from(y))),
            );
            let baud = f64::from(self.rate.baud());
            let fs = f64::from(crate::SAMPLE_RATE) * self.clock;
            let dur = syms.len() as f64 / baud + 0.02;
            let n_out = (dur * fs) as usize;
            let mut wave = Vec::with_capacity(n_out);
            for n in 0..n_out {
                let t = n as f64 / fs;
                let ts = t * baud;
                let k0 = ts.floor() as i64;
                let mut s = C32::default();
                for k in k0 - 8..=k0 + 8 {
                    if k < 0 || k as usize >= syms.len() {
                        continue;
                    }
                    s += syms[k as usize].scale(rrc((ts - k as f64) as f32, ALPHA));
                }
                let w =
                    2.0 * std::f64::consts::PI * f64::from(CARRIER_HZ + self.carrier_offset_hz) * t;
                wave.push(s.re * w.cos() as f32 - s.im * w.sin() as f32);
            }
            // exactly `level_dbm0` over the signal
            let ms = wave
                .iter()
                .map(|&x| f64::from(x) * f64::from(x))
                .sum::<f64>()
                / wave.len().max(1) as f64;
            let k = (mean_square(self.level_dbm0) / (ms as f32).max(1e-12)).sqrt();
            let mut out = vec![0i16; LEAD];
            out.extend(
                wave.iter()
                    .map(|x| (k * x).round().clamp(-32768.0, 32767.0) as i16),
            );
            out.extend(vec![0i16; 400]);
            out
        }
    }
}

#[cfg(test)]
mod tests {
    use super::testtx::Tx;
    use super::*;
    use crate::goertzel::Goertzel;
    use crate::level::{block_mean_square, dbm0};

    fn prbs(n: usize, seed: u32) -> Vec<bool> {
        let mut x = seed | 1;
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                x & 1 != 0
            })
            .collect()
    }

    /// The bits of the points of Segment 4 onward, read back as the receiver will: the phase
    /// change from the previous point (Table 1, inverted) and Q1 from the amplitude (Table 2).
    fn decode(rate: Rate, syms: &[(i8, i8)]) -> Vec<bool> {
        let mut prev = phase_of(syms[SEGMENT_2 + SEGMENT_3 - 1]);
        let mut bits = Vec::new();
        for &p in &syms[SEGMENT_2 + SEGMENT_3..] {
            let ph = phase_of(p);
            let change = (ph + 8 - prev) % 8;
            prev = ph;
            let q234 = (0u8..8).find(|&q| phase_change(q) == change).unwrap_or(0);
            let m = p.0.abs().max(p.1.abs());
            let q1 = if ph.is_multiple_of(2) { m == 5 } else { m == 3 };
            if rate == Rate::B9600 {
                bits.push(q1);
            }
            bits.extend((0..3).rev().map(|i| (q234 >> i) & 1 != 0));
        }
        bits
    }

    #[test]
    fn the_training_prbs_has_the_four_states_and_the_segment_3_start() {
        let mut p = TrainingPrbs::default();
        let mut states = Vec::new();
        let mut out = Vec::new();
        for _ in 0..7 {
            states.push(format!("{:07b}", p.state));
            out.push(p.next_bit());
        }
        assert_eq!(
            &states[..4],
            ["0101010", "1010101", "1101010", "1110101"].as_slice()
        );
        // C D C D C D C
        assert_eq!(out, [false, true, false, true, false, true, false]);
        // a maximal sequence: 127 states, back to the start
        let mut p = TrainingPrbs::default();
        let start = p;
        for k in 1..=127 {
            p.next_bit();
            assert_eq!(p == start, k == 127, "{k}");
        }
    }

    #[test]
    fn the_scrambler_round_trips_and_is_self_synchronising() {
        let data = prbs(3000, 5);
        let mut s = Scrambler::default();
        let line: Vec<bool> = data.iter().map(|&b| s.scramble(b)).collect();
        // from the zero state
        let mut r = Scrambler::default();
        let back: Vec<bool> = line.iter().map(|&b| r.descramble(b)).collect();
        assert_eq!(back, data);
        // from any state: right after 23 bits
        let mut r = Scrambler {
            reg: 0x2A_5F3C & HISTORY_MASK,
        };
        let back: Vec<bool> = line.iter().map(|&b| r.descramble(b)).collect();
        assert_eq!(&back[23..], &data[23..]);
        // zeros from the zero state stay zero
        let mut z = Scrambler::default();
        assert!(!(0..100).any(|_| z.scramble(false)));
    }

    #[test]
    fn table_1_gives_the_phase_changes() {
        // Q2 Q3 Q4 → phase change in degrees
        let rows = [
            (0b001, 0),
            (0b000, 45),
            (0b010, 90),
            (0b011, 135),
            (0b111, 180),
            (0b110, 225),
            (0b100, 270),
            (0b101, 315),
        ];
        for (q, deg) in rows {
            assert_eq!(u16::from(phase_change(q)) * 45, deg, "{q:03b}");
        }
    }

    #[test]
    fn table_2_gives_the_amplitudes() {
        // absolute phase 0, 90, 180, 270°: Q1 = 0 → 3, Q1 = 1 → 5
        assert_eq!(point(0, false), (3, 0));
        assert_eq!(point(0, true), (5, 0));
        assert_eq!(point(2, false), (0, 3));
        assert_eq!(point(6, true), (0, -5));
        // absolute phase 45, 135, 225, 315°: Q1 = 0 → √2 = (1, 1), Q1 = 1 → 3√2 = (3, 3)
        assert_eq!(point(1, false), (1, 1));
        assert_eq!(point(1, true), (3, 3));
        assert_eq!(point(3, false), (-1, 1));
        assert_eq!(point(7, true), (3, -3));
        for p in 0..8 {
            for q in [false, true] {
                assert_eq!(phase_of(point(p, q)), p);
            }
        }
    }

    #[test]
    fn nine_six_hundred_has_the_16_points() {
        let mut got: Vec<(i8, i8)> = (0..8)
            .flat_map(|p| [point(p, false), point(p, true)])
            .collect();
        let mut want = vec![
            (3, 0),
            (-3, 0),
            (0, 3),
            (0, -3),
            (5, 0),
            (-5, 0),
            (0, 5),
            (0, -5),
            (1, 1),
            (1, -1),
            (-1, 1),
            (-1, -1),
            (3, 3),
            (3, -3),
            (-3, 3),
            (-3, -3),
        ];
        got.sort();
        got.dedup();
        want.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn seven_two_hundred_has_the_8_points() {
        let mut got: Vec<(i8, i8)> = (0..8).map(|p| point(p, false)).collect();
        let mut want = vec![
            (3, 0),
            (-3, 0),
            (0, 3),
            (0, -3),
            (1, 1),
            (1, -1),
            (-1, 1),
            (-1, -1),
        ];
        got.sort();
        got.dedup();
        want.sort();
        assert_eq!(got, want);
    }

    #[test]
    fn figure_4_points_per_rate() {
        assert_eq!(
            figure4(Rate::B9600),
            [(-3, 0), (3, -3), (3, 0), (-3, 3)],
            "9600: A B C D"
        );
        assert_eq!(
            figure4(Rate::B7200),
            [(-3, 0), (1, -1), (3, 0), (-1, 1)],
            "7200: A B C D"
        );
    }

    #[test]
    fn symbols_have_the_turn_on_counts_and_start() {
        for rate in [Rate::B9600, Rate::B7200] {
            let tx = Tx {
                rate,
                level_dbm0: -10.0,
                carrier_offset_hz: 0.0,
                clock: 1.0,
            };
            let data = prbs(1001, 3);
            let s = tx.symbols(&data);
            let n = rate.bits_per_symbol();
            assert_eq!(s.len(), 128 + 384 + 48 + data.len().div_ceil(n), "{rate:?}");
            let [a, b, c, d] = figure4(rate);
            assert_eq!(s[0], a, "{rate:?}");
            assert!(
                s[..SEGMENT_2]
                    .iter()
                    .enumerate()
                    .all(|(i, &p)| p == if i % 2 == 0 { a } else { b }),
                "{rate:?}: A B alternation"
            );
            assert_eq!(s[128..135], [c, d, c, d, c, d, c], "{rate:?}: segment 3");
        }
    }

    #[test]
    fn the_symbols_decode_to_the_data_through_the_scrambler() {
        for rate in [Rate::B9600, Rate::B7200] {
            let tx = Tx {
                rate,
                level_dbm0: -10.0,
                carrier_offset_hz: 0.0,
                clock: 1.0,
            };
            let data = prbs(3001, 11);
            let s = tx.symbols(&data);
            let line = decode(rate, &s);
            let mut r = Scrambler::default();
            let got: Vec<bool> = line.iter().map(|&b| r.descramble(b)).collect();
            let n = rate.bits_per_symbol();
            assert!(got[..SEGMENT_4 * n].iter().all(|&b| b), "{rate:?}");
            assert_eq!(
                &got[SEGMENT_4 * n..SEGMENT_4 * n + data.len()],
                &data[..],
                "{rate:?}"
            );
        }
    }

    #[test]
    fn modulate_is_at_the_level_and_centred_at_1700_hz() {
        for rate in [Rate::B9600, Rate::B7200] {
            let tx = Tx {
                rate,
                level_dbm0: -10.0,
                carrier_offset_hz: 0.0,
                clock: 1.0,
            };
            let audio = tx.modulate(&prbs(20000, 9));
            let mid = &audio[4000..audio.len() - 4000];
            let level = dbm0(block_mean_square(mid));
            assert!((level + 10.0).abs() < 0.5, "{rate:?}: {level} dBm0");
            let peak = audio.iter().map(|x| x.unsigned_abs()).max().unwrap_or(0);
            assert!(peak < 32000, "{rate:?}: peak {peak}");
            let power = |f: f32| {
                let mut g = Goertzel::new(f);
                mid.iter().for_each(|&x| g.update(f32::from(x)));
                g.take()
            };
            let (p1700, p300, p3700) = (power(1700.0), power(300.0), power(3700.0));
            assert!(p1700 > 10.0 * p300, "{rate:?}: 1700 {p1700} 300 {p300}");
            assert!(
                p1700 > 1000.0 * p3700,
                "{rate:?}: 1700 {p1700} 3700 {p3700}"
            );
        }
    }
}
