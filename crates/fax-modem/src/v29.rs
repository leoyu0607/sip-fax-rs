//! ITU-T V.29 building blocks: 9600 and 7200 bit/s, amplitude and phase modulation on a 1700 Hz
//! carrier at 2400 baud. This file has the signal space, the scrambler, the training sequence,
//! a test transmitter and the receiver ([`V29Rx`]).
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

use crate::SAMPLE_RATE;
use crate::dsp::{C32, Loop2, cubic, rrc_taps, wrap};
use crate::level::{block_mean_square, dbm0};
use crate::tone::OFF_DBM0;
use std::collections::VecDeque;
use std::f32::consts::PI;

/// Roll-off of the root-raised-cosine pulse (both ends).
pub const ROLL_OFF: f32 = 0.25;

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

// ----------------------------------------------------------------------------------- receiver

/// The receiver reports the same events as the V.27ter receiver (see [`crate::v27ter::RxEvent`]).
pub use crate::v27ter::RxEvent;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Waiting for Segment 2 (A B alternation), then for the end of it.
    Acquire,
    /// Segment 3: the known C / D points train the equalizer and the carrier.
    Train,
    /// Segment 4 and the data: decision directed.
    Data,
}

/// Equalizer taps (T/2 spaced: ±3.5 symbols).
const EQ_TAPS: usize = 15;
/// The tap that starts at 1. The input alternates on-symbol and half-way samples with the newest
/// (index 0) on a symbol, so the centre must be an even index or it would pass the transitions.
const EQ_CENTRE: usize = 6;
/// Alternations (net of the misses) that lock the carrier on Segment 2 (A and B are 135° apart).
const LOCK_PAIRS: u32 = 16;
/// Samples per presence block (5 ms).
const BLOCK: usize = 40;

/// Carrier loop gains in the data (decision directed): slower than in training.
const DATA_KP: f32 = 0.02;
const DATA_KI: f32 = 0.0005;
/// Gardner timing loop gain in Segment 2 (a strong, regular signal).
const TIMING_K_ACQUIRE: f32 = 0.1;
/// Gardner timing loop gains in Segment 4 and the data; the timing is held through Segment 3.
/// The integral follows a clock offset (100 ppm is 2·10⁻⁴ of a half symbol per symbol), so the
/// equalizer is not left to absorb it. Timing from the equalizer's tap centroid instead made a
/// slow walk of the timing that the equalizer took up by spreading its taps around a centroid
/// that stayed put, until it fell apart after 30 to 40 s of 7200 at 17 dB (M5d B03). Working
/// range measured: kp 0.002–0.01, ki 2·10⁻⁶–10⁻⁵ (B03 47 s pages and B04 both pass).
const DATA_TIMING_KP: f32 = 0.005;
const DATA_TIMING_KI: f32 = 1e-5;
/// The last symbols of Segment 3 that judge whether the training was a training: their squared
/// error against the known C / D points, relative to the power of those points, must stay below
/// `FIT_LIMIT` (0.25 is a MER of 6 dB; a real signal is 15 dB or better at the lowest SNR the
/// speed works at, noise fitted to a training sequence is about 0 dB). A training that does not
/// fit was noise (or a tone of the Segment 2 frequencies) and is given up, without a `Training`
/// event (that is reported together with `Trained`, at the end of Segment 3): without this check
/// the receiver took it for data, and in noise louder than the turn-off threshold it never
/// returned to the search for a Segment 2.
const FIT_SYMBOLS: usize = 128;
const FIT_LIMIT: f32 = 0.25;
/// Equalizer step in the data. Large enough for the equalizer to follow what the timing loop
/// leaves of a 100 ppm clock (with 0.01 the MER was 1.2 to 3 dB worse at 20 dB SNR, M5d review
/// sweep).
const DATA_MU: f32 = 0.05;

/// Table 1 inverted: the Q2 Q3 Q4 bits of a phase change in steps of 45°.
const CHANGE_BITS: [u8; 8] = [0b001, 0b000, 0b010, 0b011, 0b111, 0b110, 0b100, 0b101];

/// The mean power of the points of `rate` (216 / 16 at 9600 bit/s, 44 / 8 at 7200 bit/s). The
/// time average of the matched filter output is proportional to the mean power of the symbols,
/// so this is the level the automatic gain control aims at (the points have magnitude 3).
fn constellation_power(rate: Rate) -> f32 {
    match rate {
        Rate::B9600 => 13.5,
        Rate::B7200 => 5.5,
    }
}

/// A point of the signal space as a complex number.
fn complex(p: (i8, i8)) -> C32 {
    C32::new(f32::from(p.0), f32::from(p.1))
}

/// Whether `z` follows `prev` by ±135° (the rotation from A to B, or from B to A).
fn alternates(z: C32, prev: C32) -> bool {
    let rot = (z * prev.conj()).arg();
    (rot.abs() - 3.0 * PI / 4.0).abs() < PI / 8.0
}

/// The nearest point of `rate` to `z`: its absolute phase step and Q1.
fn nearest(rate: Rate, z: C32) -> (u8, bool) {
    let q1s: &[bool] = match rate {
        Rate::B9600 => &[false, true],
        Rate::B7200 => &[false],
    };
    let mut best = (f32::INFINITY, 0u8, false);
    for p in 0..8u8 {
        for &q1 in q1s {
            let dist = (z - complex(point(p, q1))).norm_sqr();
            if dist < best.0 {
                best = (dist, p, q1);
            }
        }
    }
    (best.1, best.2)
}

/// The receiver for one speed.
#[derive(Debug, Clone)]
pub struct V29Rx {
    rate: Rate,
    /// Input samples per half symbol.
    half: f32,
    /// Mean power the gain control aims at.
    target: f32,
    taps: Vec<f32>,
    // front end
    nco: f32,
    nco_step: f32,
    bb: VecDeque<C32>,
    mf: VecDeque<C32>,
    /// Absolute index of `mf[0]`.
    mf_base: u64,
    /// Samples taken so far.
    n: u64,
    /// Next interpolation instant (absolute sample index, fractional).
    t_next: f64,
    /// The next interpolated sample is on a symbol (else half way).
    on_symbol: bool,
    prev_sym: C32,
    mid: C32,
    gain: f32,
    power: f32,
    // equalizer and carrier
    eq_in: VecDeque<C32>,
    w: Vec<C32>,
    phase: f32,
    pll: Loop2,
    /// The symbol timing on the Gardner error: Segment 2, then (other gains) Segment 4 and the
    /// data.
    timing: Loop2,
    /// The squared decision error of every data symbol (for the MER in the tests).
    #[cfg(test)]
    pub(crate) evm: Vec<f32>,
    /// The previous symbol of Segment 2, in the current carrier frame.
    raw_prev: C32,
    /// Alternations in a row (before the lock).
    run: u32,
    /// The carrier is locked on Segment 2.
    locked: bool,
    /// After a lock: the last symbol of Segment 2 was an A.
    last_a: bool,
    // Segment 3 and the data
    stage: Stage,
    prbs: TrainingPrbs,
    /// Symbols of Segment 3 still to come.
    left: usize,
    /// Squared error and power of the last `FIT_SYMBOLS` symbols of Segment 3.
    fit_err: f32,
    fit_pow: f32,
    /// The absolute phase step of the last decided point (the reference of the next change).
    ref_phase: u8,
    scrambler: Scrambler,
    // presence
    block: Vec<i16>,
    /// The last 5 ms block was at or above the turn-off threshold: a signal is present.
    loud: bool,
    quiet: u32,
    #[cfg(test)]
    pub(crate) trace: Vec<(u8, f32, f32)>,
}

impl V29Rx {
    /// A receiver for `rate`.
    pub fn new(rate: Rate) -> Self {
        let sps = SAMPLE_RATE as f32 / rate.baud();
        let mut w = vec![C32::default(); EQ_TAPS];
        w[EQ_CENTRE] = C32::new(1.0, 0.0);
        Self {
            rate,
            half: sps / 2.0,
            target: constellation_power(rate),
            taps: rrc_taps(sps, ROLL_OFF, 6.0),
            nco: 0.0,
            nco_step: 2.0 * PI * CARRIER_HZ / SAMPLE_RATE as f32,
            bb: VecDeque::new(),
            mf: VecDeque::new(),
            mf_base: 0,
            n: 0,
            t_next: 0.0,
            on_symbol: true,
            prev_sym: C32::default(),
            mid: C32::default(),
            gain: 1.0,
            power: 0.0,
            eq_in: VecDeque::new(),
            w,
            phase: 0.0,
            pll: Loop2::new(0.08, 0.002),
            timing: Loop2::new(TIMING_K_ACQUIRE, 0.0),
            #[cfg(test)]
            evm: Vec::new(),
            raw_prev: C32::default(),
            run: 0,
            locked: false,
            last_a: false,
            stage: Stage::Acquire,
            prbs: TrainingPrbs::default(),
            left: 0,
            fit_err: 0.0,
            fit_pow: 0.0,
            ref_phase: 0,
            scrambler: Scrambler::default(),
            block: Vec::with_capacity(BLOCK),
            loud: false,
            quiet: 0,
            #[cfg(test)]
            trace: Vec::new(),
        }
    }

    /// The speed.
    pub fn rate(&self) -> Rate {
        self.rate
    }

    /// Whether the equalizer has been trained (Segment 4 started).
    pub fn is_trained(&self) -> bool {
        self.stage == Stage::Data
    }

    fn reset(&mut self) {
        let rate = self.rate;
        #[cfg(test)]
        let (trace, evm) = (
            std::mem::take(&mut self.trace),
            std::mem::take(&mut self.evm),
        );
        *self = Self::new(rate);
        #[cfg(test)]
        {
            self.trace = trace;
            self.evm = evm;
        }
    }

    /// Feeds samples.
    pub fn feed(&mut self, samples: &[i16]) -> Vec<RxEvent> {
        let mut ev = Vec::new();
        let mut bits = Vec::new();
        for &s in samples {
            self.presence(s, &mut ev, &mut bits);
            self.sample(s as f32, &mut ev, &mut bits);
        }
        if !bits.is_empty() {
            ev.push(RxEvent::Bits(bits));
        }
        ev
    }

    fn presence(&mut self, s: i16, ev: &mut Vec<RxEvent>, bits: &mut Vec<bool>) {
        self.block.push(s);
        if self.block.len() < BLOCK {
            return;
        }
        let level = dbm0(block_mean_square(&self.block));
        self.block.clear();
        self.loud = level >= OFF_DBM0;
        if self.stage != Stage::Acquire {
            self.quiet = if level < OFF_DBM0 { self.quiet + 1 } else { 0 };
            if self.quiet >= 2 {
                if !bits.is_empty() {
                    ev.push(RxEvent::Bits(std::mem::take(bits)));
                }
                ev.push(RxEvent::CarrierLost);
                self.reset();
            }
        }
    }

    fn sample(&mut self, x: f32, ev: &mut Vec<RxEvent>, bits: &mut Vec<bool>) {
        // mix down
        let lo = C32::from_angle(-self.nco);
        self.nco += self.nco_step;
        if self.nco > 2.0 * PI {
            self.nco -= 2.0 * PI;
        }
        self.bb.push_back(lo.scale(x));
        if self.bb.len() > self.taps.len() {
            self.bb.pop_front();
        }
        if self.bb.len() < self.taps.len() {
            self.n += 1;
            return;
        }
        let mut m = C32::default();
        for (b, &t) in self.bb.iter().zip(self.taps.iter()) {
            m += b.scale(t);
        }
        // level → the constellation's mean power (slow; frozen once the data starts)
        let p = m.norm_sqr();
        if self.stage != Stage::Data {
            // fast attack (a signal starting), slow release
            let a = if p > self.power { 0.2 } else { 0.005 };
            self.power += a * (p - self.power);
            if self.power > 0.0 {
                self.gain = (self.target / self.power).sqrt();
            }
        }
        if self.mf.is_empty() {
            self.mf_base = self.n;
            self.t_next = self.n as f64 + 1.0;
        }
        self.mf.push_back(m);
        self.n += 1;
        // interpolate every instant that has two samples after it
        while self.t_next + 2.0 <= (self.mf_base + self.mf.len() as u64) as f64 {
            let i = self.t_next.floor();
            let mu = (self.t_next - i) as f32;
            let k = (i as i64 - self.mf_base as i64 - 1).max(0) as usize;
            if k + 3 >= self.mf.len() {
                break;
            }
            let y = [self.mf[k], self.mf[k + 1], self.mf[k + 2], self.mf[k + 3]];
            let v = cubic(y, mu).scale(self.gain);
            self.t_next += self.half as f64;
            self.half_sample(v, ev, bits);
        }
        // drop what can no longer be needed
        while self.mf.len() > 16 && (self.mf_base as f64 + 4.0) < self.t_next {
            self.mf.pop_front();
            self.mf_base += 1;
        }
    }

    /// One T/2 sample.
    fn half_sample(&mut self, v: C32, ev: &mut Vec<RxEvent>, bits: &mut Vec<bool>) {
        self.eq_in.push_back(v);
        if self.eq_in.len() > EQ_TAPS {
            self.eq_in.pop_front();
        }
        if !self.on_symbol {
            self.mid = v;
            self.on_symbol = true;
            return;
        }
        self.on_symbol = false;
        // Gardner timing error on the symbols either side of the half-way sample
        let e = ((v - self.prev_sym) * self.mid.conj()).re;
        // (frozen without a signal: on near-silence the error is noise and the clock would wander)
        let track = match self.stage {
            Stage::Acquire => self.loud,
            Stage::Train => false,
            Stage::Data => true,
        };
        if track {
            let e = e.clamp(-1.0, 1.0);
            let c = self.timing.update(e);
            self.t_next -= (c * self.half) as f64;
        }
        self.prev_sym = v;
        if self.eq_in.len() < EQ_TAPS {
            return;
        }
        self.symbol(ev, bits);
    }

    fn equalize(&self) -> C32 {
        let mut y = C32::default();
        // w[0] multiplies the newest sample
        for (wi, u) in self.w.iter().zip(self.eq_in.iter().rev()) {
            y += *wi * *u;
        }
        y
    }

    fn adapt(&mut self, err_rot: C32, mu: f32) {
        let err = err_rot * C32::from_angle(self.phase);
        let energy: f32 = self.eq_in.iter().map(|u| u.norm_sqr()).sum::<f32>() + 1e-3;
        let step = mu / energy;
        for (wi, u) in self.w.iter_mut().zip(self.eq_in.iter().rev()) {
            *wi += (err * u.conj()).scale(step);
        }
    }

    fn symbol(&mut self, ev: &mut Vec<RxEvent>, bits: &mut Vec<bool>) {
        let y = self.equalize();
        let z = y * C32::from_angle(-self.phase);
        #[cfg(test)]
        {
            let unit = 2.0 * PI / 8.0;
            let resid = z.arg() - (z.arg() / unit).round() * unit;
            self.trace
                .push((self.stage as u8, z.norm_sqr().sqrt(), resid.to_degrees()));
        }
        match self.stage {
            Stage::Acquire => self.acquire(z, ev),
            Stage::Train => self.train_symbol(z, ev),
            Stage::Data => self.data_symbol(z, bits),
        }
    }

    /// Segment 2: count the ±135° turns (a leaky count, so that one noisy turn does not restart
    /// it). After `LOCK_PAIRS`, lock the carrier with A at 180°, then track it on the known A and B
    /// points. Segment 3 starts with the first point nearer to C than to A after a B.
    fn acquire(&mut self, z: C32, ev: &mut Vec<RxEvent>) {
        let [a, b, c, _] = figure4(self.rate);
        if !self.locked {
            if self.loud && alternates(z, self.raw_prev) {
                self.run += 1;
            } else {
                self.run = self.run.saturating_sub(1);
            }
            if self.run >= LOCK_PAIRS {
                // the last symbol: B if it turned by +135°, else A
                let rot = (z * self.raw_prev.conj()).arg();
                let a_z = if rot > 0.0 { self.raw_prev } else { z };
                let delta = wrap(a_z.arg() - PI);
                self.phase = wrap(self.phase + delta);
                self.locked = true;
                self.last_a = rot < 0.0;
                self.run = 0;
                self.raw_prev = z * C32::from_angle(-delta);
                return;
            }
            self.raw_prev = z;
            return;
        }
        if !self.loud {
            self.locked = false;
            self.run = 0;
            self.raw_prev = z;
            return;
        }
        if !self.last_a && (z - complex(c)).norm_sqr() < (z - complex(a)).norm_sqr() {
            // a point at 0° after a B: Segment 3 starts with this symbol (its PRBS output 0 is C)
            self.stage = Stage::Train;
            self.prbs = TrainingPrbs::default();
            self.left = SEGMENT_3;
            self.fit_err = 0.0;
            self.fit_pow = 0.0;
            // (`Training` is reported when the end of Segment 3 has shown that it was one)
            self.train_symbol(z, ev);
            return;
        }
        // the next point is known (A after a B, else B): it tracks the carrier and trains the
        // equalizer for the rest of Segment 2
        let known = if self.last_a { b } else { a };
        self.track(z, complex(known), 0.05);
        self.last_a = !self.last_a;
        self.raw_prev = z;
    }

    /// Segment 3: the known C / D points (Appendix I) train the equalizer and the carrier.
    fn train_symbol(&mut self, z: C32, ev: &mut Vec<RxEvent>) {
        let [_, _, c, d] = figure4(self.rate);
        let known = if self.prbs.next_bit() { d } else { c };
        if self.left <= FIT_SYMBOLS {
            self.fit_err += (z - complex(known)).norm_sqr();
            self.fit_pow += complex(known).norm_sqr();
        }
        self.track(z, complex(known), 0.05);
        self.left -= 1;
        if self.left == 0 {
            if self.fit_err > FIT_LIMIT * self.fit_pow {
                // it was not a training: start the search again
                self.reset();
                return;
            }
            self.stage = Stage::Data;
            self.ref_phase = phase_of(known);
            // the carrier is known now: a slower loop keeps the noise of the decisions out
            self.pll.kp = DATA_KP;
            self.pll.ki = DATA_KI;
            self.timing = Loop2::new(DATA_TIMING_KP, DATA_TIMING_KI);

            ev.push(RxEvent::Training);
            ev.push(RxEvent::Trained);
        }
    }

    /// Segment 4 and the data: the nearest point, the phase change from the last decision,
    /// Table 1 back to Q2 Q3 Q4 and Q1 from the amplitude (9600 only).
    fn data_symbol(&mut self, z: C32, bits: &mut Vec<bool>) {
        let (p, q1) = nearest(self.rate, z);
        #[cfg(test)]
        {
            self.evm.push((z - complex(point(p, q1))).norm_sqr());
        }
        let change = (p + 8 - self.ref_phase) % 8;
        self.track(z, complex(point(p, q1)), DATA_MU);
        self.ref_phase = p;
        if self.rate == Rate::B9600 {
            bits.push(self.scrambler.descramble(q1));
        }
        let q234 = CHANGE_BITS[usize::from(change)];
        for i in (0..3).rev() {
            bits.push(self.scrambler.descramble((q234 >> i) & 1 != 0));
        }
    }

    /// Carrier loop and equalizer update towards the point `d`.
    fn track(&mut self, z: C32, d: C32, mu: f32) {
        let err_phase = (z * d.conj()).arg();
        self.phase = wrap(self.phase + self.pll.update(err_phase));
        self.adapt(d - z, mu);
    }
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
                    s += syms[k as usize].scale(rrc((ts - k as f64) as f32, ROLL_OFF));
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

    fn receive(rx: &mut V29Rx, audio: &[i16]) -> (Vec<RxEvent>, Vec<bool>) {
        let mut ev = Vec::new();
        let mut bits = Vec::new();
        for chunk in audio.chunks(160) {
            for e in rx.feed(chunk) {
                match e {
                    RxEvent::Bits(b) => bits.extend(b),
                    other => ev.push(other),
                }
            }
        }
        (ev, bits)
    }

    /// Adds white Gaussian noise (Box–Muller on a fixed-seed xorshift) at `snr_db` below the
    /// signal of `level` dBm0.
    fn add_awgn(audio: &mut [i16], level: f32, snr_db: f32, seed: u32) {
        let sigma = (crate::level::mean_square(level) / 10f32.powf(snr_db / 10.0)).sqrt();
        let mut x = seed | 1;
        let mut uniform = move || {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            (f64::from(x) + 1.0) / (f64::from(u32::MAX) + 2.0)
        };
        for s in audio.iter_mut() {
            let (u1, u2) = (uniform(), uniform());
            let g = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
            *s = (f64::from(*s) + f64::from(sigma) * g)
                .round()
                .clamp(-32768.0, 32767.0) as i16;
        }
    }

    /// The data errors of a transmission (the data starts after the 48 ones of Segment 4).
    fn errors(
        rate: Rate,
        level: f32,
        offset: f32,
        clock: f64,
        snr_db: Option<f32>,
        seed: u32,
    ) -> (usize, usize) {
        let data = prbs(9600, seed);
        let tx = Tx {
            rate,
            level_dbm0: level,
            carrier_offset_hz: offset,
            clock,
        };
        let mut audio = tx.modulate(&data);
        if let Some(snr) = snr_db {
            add_awgn(&mut audio, level, snr, seed.wrapping_mul(7919) + 1);
        }
        let mut rx = V29Rx::new(rate);
        let (ev, bits) = receive(&mut rx, &audio);
        if !ev.contains(&RxEvent::Training) || !ev.contains(&RxEvent::Trained) {
            return (data.len(), data.len());
        }
        let head = SEGMENT_4 * rate.bits_per_symbol();
        let got = bits.get(head..head + data.len()).unwrap_or(&[]);
        let errs = got.iter().zip(&data).filter(|(a, b)| a != b).count() + (data.len() - got.len());
        (errs, data.len())
    }

    #[test]
    fn clean_signals_at_both_speeds() {
        for rate in [Rate::B9600, Rate::B7200] {
            for seed in 1..=3 {
                let data = prbs(9600, seed);
                let tx = Tx {
                    rate,
                    level_dbm0: -10.0,
                    carrier_offset_hz: 0.0,
                    clock: 1.0,
                };
                let audio = tx.modulate(&data);
                let mut rx = V29Rx::new(rate);
                let (ev, bits) = receive(&mut rx, &audio);
                assert_eq!(
                    ev,
                    vec![RxEvent::Training, RxEvent::Trained, RxEvent::CarrierLost],
                    "{rate:?} seed {seed}"
                );
                let head = SEGMENT_4 * rate.bits_per_symbol();
                assert!(bits[..head].iter().all(|&b| b), "{rate:?}: segment 4 ones");
                let got = &bits[head..(head + data.len()).min(bits.len())];
                let errs = got.iter().zip(&data).filter(|(a, b)| a != b).count()
                    + (data.len() - got.len());
                assert_eq!(errs, 0, "{rate:?} seed {seed}");
            }
        }
    }

    #[test]
    fn silence_and_noise_do_not_train() {
        for rate in [Rate::B9600, Rate::B7200] {
            let mut rx = V29Rx::new(rate);
            let (ev, bits) = receive(&mut rx, &vec![0i16; 16000]);
            assert!(ev.is_empty() && bits.is_empty(), "{rate:?} silence: {ev:?}");
            // white noise at -50 dBm0 (uniform, so the amplitude a has a² / 3 = mean square)
            let a = (3.0 * crate::level::mean_square(-50.0)).sqrt();
            let mut x = 0x9E37_79B9u32;
            let noise: Vec<i16> = (0..16000)
                .map(|_| {
                    x ^= x << 13;
                    x ^= x >> 17;
                    x ^= x << 5;
                    let u = x as f32 / u32::MAX as f32 * 2.0 - 1.0;
                    (u * a) as i16
                })
                .collect();
            assert!((dbm0(block_mean_square(&noise)) + 50.0).abs() < 0.5);
            let mut rx = V29Rx::new(rate);
            let (ev, _) = receive(&mut rx, &noise);
            assert!(!ev.contains(&RxEvent::Training), "{rate:?} noise: {ev:?}");
        }
    }

    /// A tone that looks like Segment 2 (800 or 2600 Hz turn by 135 degrees a symbol) starts a
    /// training; it does not fit the C / D points of Segment 3, so it is given up before
    /// `Training` and `Trained` are reported, and no garbage bits follow. Before the fit check
    /// the receiver went on to the data state and, in noise above the turn-off threshold, never
    /// left it (M5d B09: one false training in an hour of white noise at -43 dBm0, 9600 bit/s).
    /// The real transmission after it is received.
    #[test]
    fn a_tone_that_looks_like_segment_2_is_given_up_and_the_real_signal_is_received() {
        for rate in [Rate::B9600, Rate::B7200] {
            for hz in [800.0f32, 2600.0] {
                let tone: Vec<i16> = (0..24_000)
                    .map(|i| {
                        (crate::level::sine_amplitude(-20.0)
                            * (2.0 * PI * hz * i as f32 / 8000.0).sin())
                            as i16
                    })
                    .collect();
                let mut rx = V29Rx::new(rate);
                let (ev, bits) = receive(&mut rx, &tone);
                assert!(
                    ev.is_empty() && bits.is_empty(),
                    "{rate:?} {hz}: {ev:?}, {} bits",
                    bits.len()
                );
                // the same receiver now gets a real transmission (after a pause)
                let data = prbs(4000, 11);
                let tx = Tx {
                    rate,
                    level_dbm0: -20.0,
                    carrier_offset_hz: 0.0,
                    clock: 1.0,
                };
                let mut audio = vec![0i16; 4000];
                audio.extend(tx.modulate(&data));
                let (ev, bits) = receive(&mut rx, &audio);
                assert!(ev.contains(&RxEvent::Trained), "{rate:?} {hz}: {ev:?}");
                let off = 48 * rate.bits_per_symbol();
                assert_eq!(&bits[off..off + data.len()], &data[..], "{rate:?} {hz}");
            }
        }
    }

    /// Time of `feed` on 10 s of 9600 bit/s audio (release build, prints only).
    #[test]
    #[ignore = "timing report, prints only"]
    fn feed_speed_report() {
        let tx = Tx {
            rate: Rate::B9600,
            level_dbm0: -15.0,
            carrier_offset_hz: 0.0,
            clock: 1.0,
        };
        let audio = tx.modulate(&prbs(96_000, 1));
        let audio: Vec<i16> = audio.iter().cycle().take(80_000).copied().collect();
        let start = std::time::Instant::now();
        let mut rx = V29Rx::new(Rate::B9600);
        let _ = receive(&mut rx, &audio);
        eprintln!("feed 10 s of 9600 audio: {:?}", start.elapsed());
    }

    /// Zero errors on 9600 data bits, seeds 1 to 3, at both speeds.
    fn check_clean(level: f32, offset: f32, clock: f64, snr_db: Option<f32>) {
        for rate in [Rate::B9600, Rate::B7200] {
            for seed in 1..=3 {
                let (errs, n) = errors(rate, level, offset, clock, snr_db, seed);
                assert_eq!(
                    errs, 0,
                    "{rate:?} {level} dBm0 {offset} Hz {clock} {snr_db:?} seed {seed} ({n} bits)"
                );
            }
        }
    }

    #[test]
    fn robustness_level_minus_43() {
        check_clean(-43.0, 0.0, 1.0, None);
    }

    #[test]
    fn robustness_carrier_offset_plus_and_minus_7_hz() {
        check_clean(-15.0, 7.0, 1.0, None);
        check_clean(-15.0, -7.0, 1.0, None);
    }

    #[test]
    fn robustness_clock_plus_and_minus_100_ppm() {
        check_clean(-15.0, 0.0, 1.0001, None);
        check_clean(-15.0, 0.0, 0.9999, None);
    }

    #[test]
    fn robustness_awgn() {
        for (rate, snr) in [(Rate::B9600, 20.0), (Rate::B7200, 17.0)] {
            for seed in 1..=3 {
                let (errs, n) = errors(rate, -15.0, 0.0, 1.0, Some(snr), seed);
                assert_eq!(errs, 0, "{rate:?} {snr} dB seed {seed} ({n} bits)");
            }
        }
    }

    /// Test plan B04: ±100 ppm and ±7 Hz, each and together, with the noise of B03 (9600 at
    /// 20 dB, 7200 at 17 dB). The MER must stay within 1.5 dB of the noise-only case: the
    /// in-band SNR is 2.2 dB above the wideband one (the matched filter passes 2400 of 4000 Hz).
    #[test]
    fn robustness_offset_and_clock_under_noise() {
        for (rate, snr) in [(Rate::B9600, 20.0), (Rate::B7200, 17.0)] {
            let floor = snr + 10.0 * (4000.0f32 / 2400.0).log10();
            for (offset, clock) in [
                (0.0, 1.0001),
                (0.0, 0.9999),
                (7.0, 1.0),
                (-7.0, 1.0),
                (7.0, 1.0001),
                (-7.0, 0.9999),
            ] {
                for seed in 1..=3 {
                    let (errs, mer) = errors_and_mer(rate, offset, clock, snr, seed);
                    let what = format!("{rate:?} {offset} Hz {clock} {snr} dB seed {seed}");
                    assert_eq!(errs, 0, "{what}");
                    assert!(
                        mer > floor - 1.5,
                        "{what}: MER {mer:.2} dB, floor {floor:.2}"
                    );
                }
            }
        }
    }

    /// Bit errors and MER (dB, Segment 4 and the data) at −15 dBm0 with noise.
    fn errors_and_mer(rate: Rate, offset: f32, clock: f64, snr: f32, seed: u32) -> (usize, f32) {
        let data = prbs(9600, seed);
        let tx = Tx {
            rate,
            level_dbm0: -15.0,
            carrier_offset_hz: offset,
            clock,
        };
        let mut audio = tx.modulate(&data);
        add_awgn(&mut audio, -15.0, snr, seed.wrapping_mul(7919) + 1);
        let mut rx = V29Rx::new(rate);
        let (_, bits) = receive(&mut rx, &audio);
        let head = SEGMENT_4 * rate.bits_per_symbol();
        let got = bits.get(head..head + data.len()).unwrap_or(&[]);
        let errs = got.iter().zip(&data).filter(|(a, b)| a != b).count() + data.len() - got.len();
        (errs, mer_of(&rx, rate, data.len()))
    }

    /// Test plan B03 over a whole page: 340 000 bits of 7200 (47 s) at 17 dB SNR, also with
    /// +100 ppm and +7 Hz. With the timing taken from the equalizer's tap centroid the timing
    /// walked, the taps spread and the data was lost after 30 to 40 s in every seed (M5d B03).
    #[test]
    fn a_long_7200_transmission_at_17_db_stays_locked() {
        for (offset, clock, seed) in [(0.0, 1.0, 1), (7.0, 1.0001, 2)] {
            let rate = Rate::B7200;
            let data = prbs(340_000, seed);
            let tx = Tx {
                rate,
                level_dbm0: -15.0,
                carrier_offset_hz: offset,
                clock,
            };
            let mut audio = tx.modulate(&data);
            add_awgn(&mut audio, -15.0, 17.0, seed.wrapping_mul(7919) + 1);
            let mut rx = V29Rx::new(rate);
            let (_, bits) = receive(&mut rx, &audio);
            let head = SEGMENT_4 * rate.bits_per_symbol();
            let got = bits.get(head..head + data.len()).unwrap_or(&[]);
            assert_eq!(got.len(), data.len(), "{offset} Hz {clock}");
            let errs = got.iter().zip(&data).filter(|(a, b)| a != b).count();
            assert_eq!(errs, 0, "{offset} Hz {clock} seed {seed}");
            let mer = mer_of(&rx, rate, data.len());
            let floor = 17.0 + 10.0 * (4000.0f32 / 2400.0).log10();
            assert!(mer > floor - 1.5, "{offset} Hz {clock}: MER {mer:.2} dB");
        }
    }

    /// The loop integrator (the frequency estimate, radians per symbol) when Segment 4 starts,
    /// for ±7 Hz (prints only).
    #[test]
    #[ignore = "integrator report, prints only"]
    fn integrator_at_training_end() {
        for rate in [Rate::B9600, Rate::B7200] {
            for offset in [7.0f32, -7.0] {
                let data = prbs(9600, 1);
                let tx = Tx {
                    rate,
                    level_dbm0: -15.0,
                    carrier_offset_hz: offset,
                    clock: 1.0,
                };
                let audio = tx.modulate(&data);
                let mut rx = V29Rx::new(rate);
                let mut trained = None;
                for chunk in audio.chunks(160) {
                    let _ = rx.feed(chunk);
                    if rx.is_trained() && trained.is_none() {
                        trained = Some(rx.pll.integ);
                    }
                }
                eprintln!(
                    "{rate:?} {offset:+} Hz: integrator at Segment 4 {trained:?} rad/symbol \
                     (expected {:.4})",
                    2.0 * std::f32::consts::PI * offset / 2400.0
                );
            }
        }
    }

    /// The timing integrator at the last Data symbol for ±100 ppm (clean; prints only).
    #[test]
    #[ignore = "timing integrator report, prints only"]
    fn timing_integrator_report() {
        for clock in [1.0001f64, 0.9999] {
            let tx = Tx {
                rate: Rate::B9600,
                level_dbm0: -15.0,
                carrier_offset_hz: 0.0,
                clock,
            };
            let audio = tx.modulate(&prbs(9600, 1));
            let mut rx = V29Rx::new(Rate::B9600);
            let mut last = None;
            for chunk in audio.chunks(160) {
                let _ = rx.feed(chunk);
                if rx.stage == Stage::Data {
                    last = Some(rx.timing.integ);
                }
            }
            eprintln!(
                "clock {clock}: integrator at the end of the data {last:?} (samples per symbol: x half)"
            );
        }
    }

    /// MER (dB) over Segment 4 and the data symbols only (not the tail or the noise after it).
    fn mer_of(rx: &V29Rx, rate: Rate, data_bits: usize) -> f32 {
        let n = (SEGMENT_4 + data_bits.div_ceil(rate.bits_per_symbol())).min(rx.evm.len());
        let mse = rx.evm[..n].iter().sum::<f32>() / n as f32;
        10.0 * (constellation_power(rate) / mse).log10()
    }
}
