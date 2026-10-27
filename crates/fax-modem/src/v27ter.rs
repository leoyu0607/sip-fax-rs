//! ITU-T V.27 ter receiver: 4800 bit/s (1600 baud, 8-phase) and 2400 bit/s (1200 baud,
//! 4-phase) differential PSK on an 1800 Hz carrier.
//!
//! From the recommendation (1988 Blue Book text, with V.27 Appendix I for the scrambler):
//!
//! * spectrum: 50 % raised cosine, split between transmitter and receiver (§ 2.1) — the receiver
//!   filters with the square-root raised cosine;
//! * encoding: Table 1 (tribits) and Table 2 (dibits) give the phase change from one symbol to
//!   the next; the first bit of a group is the first in time;
//! * turn-on (§ 2.5.1, Table 3): optional unmodulated carrier and silence (talker echo
//!   protection), Segment 3 of 180° reversals (14 or 50 symbols), Segment 4 the equalizer
//!   conditioning pattern of 0° / 180° changes (58 or 1074 symbols), Segment 5 eight symbols of
//!   scrambled ones, then data;
//! * scrambler (§ 9, V.27 Appendix I): 1 + x⁻⁶ + x⁻⁷, self-synchronising, with the guard that
//!   inverts the bit after 33 consecutive positions equal to the bit 8, 9 or 12 before;
//! * the receiver must accept ±7 Hz of carrier offset (§ 3).
//!
//! The receiver: mix down by 1800 Hz, matched RRC filter, cubic interpolation to two samples per
//! symbol with Gardner timing recovery, a T/2-spaced adaptive (normalised LMS) equalizer, a
//! decision-directed second-order carrier loop. Segment 3 locks timing and phase, Segment 4
//! trains the equalizer on 0° / 180° decisions (so the short and the long sequence need no
//! special treatment), the first change that is neither 0° nor 180° is Segment 5, from which on
//! every symbol is data.

use crate::SAMPLE_RATE;
use crate::dsp::{C32, Loop2, cubic, rrc_taps, wrap};
use crate::level::{block_mean_square, dbm0};
use crate::tone::{OFF_DBM0, SignalLevel};
use std::collections::VecDeque;
use std::f32::consts::PI;

/// Carrier frequency (§ 2.1).
pub const CARRIER_HZ: f32 = 1800.0;

/// The two speeds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Rate {
    /// 2400 bit/s: 1200 baud, dibits.
    B2400,
    /// 4800 bit/s: 1600 baud, tribits.
    B4800,
}

impl Rate {
    /// Symbols per second.
    pub fn baud(self) -> f32 {
        match self {
            Rate::B2400 => 1200.0,
            Rate::B4800 => 1600.0,
        }
    }

    /// Bits per symbol.
    pub fn bits_per_symbol(self) -> usize {
        match self {
            Rate::B2400 => 2,
            Rate::B4800 => 3,
        }
    }

    /// Phase steps per turn (8-phase or 4-phase).
    fn steps(self) -> usize {
        match self {
            Rate::B2400 => 4,
            Rate::B4800 => 8,
        }
    }
}

// --------------------------------------------------------------------------------- scrambler

/// The scrambler / descrambler (V.27 Appendix I with the 8-bit guard of V.27 ter § 9).
#[derive(Debug, Clone, Default)]
pub struct Scrambler {
    /// The line sequence (after the guard), newest in bit 0.
    reg: u32,
    /// Consecutive positions of the line sequence equal to the bit 8, 9 or 12 before.
    count: u32,
}

impl Scrambler {
    /// A scrambler with this register (newest bit first in the string of the recommendation's
    /// Appendix I: "0011110" is `0b0111100`).
    pub fn with_register(reg: u32) -> Self {
        Self { reg, count: 0 }
    }

    /// Takes one position: `fire` says whether the guard inverted this bit, `y` is the bit
    /// before the guard, `line` the bit on the line. Feedback is the bit on the line (what a
    /// self-synchronising scrambler shifts in); the repeat counter looks at `y`.
    fn step(&mut self, fire: bool, y: u32, line: u32) {
        if fire {
            self.count = 0;
        } else {
            let r = self.reg;
            let same = |k: u32| ((r >> (k - 1)) & 1) == y;
            if same(8) || same(9) || same(12) {
                self.count += 1;
            } else {
                self.count = 0;
            }
        }
        self.reg = (self.reg << 1) | line;
    }

    /// Scrambles one data bit into the line bit.
    pub fn scramble(&mut self, bit: bool) -> bool {
        let y = u32::from(bit) ^ ((self.reg >> 5) & 1) ^ ((self.reg >> 6) & 1);
        // the guard: 33 positions in a row equal to the bit 8, 9 or 12 before invert the next bit
        let fire = self.count >= 33;
        let line = y ^ u32::from(fire);
        self.step(fire, y, line);
        line != 0
    }

    /// Descrambles one line bit into the data bit.
    pub fn descramble(&mut self, line: bool) -> bool {
        let r = u32::from(line);
        let fire = self.count >= 33;
        let y = r ^ u32::from(fire);
        let bit = y ^ ((self.reg >> 5) & 1) ^ ((self.reg >> 6) & 1);
        self.step(fire, y, r);
        bit != 0
    }
}

/// The phase change of a bit group, in steps of 45° (4800) or 90° (2400): Tables 1 and 2.
/// (Only the test transmitter encodes; the receiver uses [`bits_of`].)
#[cfg_attr(not(test), allow(dead_code))]
fn change_of(rate: Rate, bits: &[bool]) -> usize {
    let v = bits.iter().fold(0usize, |a, &b| (a << 1) | usize::from(b));
    match rate {
        // tribit → 45° steps
        Rate::B4800 => [1, 0, 2, 3, 6, 7, 5, 4][v],
        // dibit → 90° steps: 00 0°, 01 90°, 11 180°, 10 270°
        Rate::B2400 => [0, 1, 3, 2][v],
    }
}

/// The bit group of a phase change (inverse of [`change_of`]).
fn bits_of(rate: Rate, steps: usize, out: &mut Vec<bool>) {
    let v = match rate {
        Rate::B4800 => [0b001, 0b000, 0b010, 0b011, 0b111, 0b110, 0b100, 0b101][steps % 8],
        Rate::B2400 => [0b00, 0b01, 0b11, 0b10][steps % 4],
    };
    let n = rate.bits_per_symbol();
    for i in (0..n).rev() {
        out.push((v >> i) & 1 != 0);
    }
}

// ---------------------------------------------------------------------------------- receiver

/// What the high speed receivers (V.27ter, V.29) report. Both use the same events.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RxEvent {
    /// The training (the carrier's phase reversals of the turn-on) was recognised.
    Training,
    /// The equalizer is trained; data follows.
    Trained,
    /// Descrambled bits (from the start of the data), first in time first.
    Bits(Vec<bool>),
    /// The carrier went away.
    CarrierLost,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Stage {
    /// Waiting for Segment 3. The demodulator runs all the time, so that its filters are full
    /// when a signal starts: the short Segment 3 lasts only 14 symbols (8.75 ms at 4800).
    Acquire,
    /// Segment 4: training on 0° / 180°.
    Train,
    /// Segment 5 and data.
    Data,
}

/// Equalizer taps (T/2 spaced: ±3.5 symbols).
const EQ_TAPS: usize = 15;
/// The tap that starts at 1. The input alternates on-symbol and half-way samples with the newest
/// (index 0) on a symbol, so the centre must be an even index or it would pass the transitions.
const EQ_CENTRE: usize = 6;
/// Reversals in a row that make Segment 3.
const REVERSALS: u32 = 10;
/// Training symbols before Segment 5 may start (Segment 4: 58 or 1074 symbols, plus what is
/// left of Segment 3 after the lock).
const MIN_TRAIN: u32 = 50;
/// Integral gain of the carrier loop in Segment 4 (fast, to pull in a frequency offset before
/// Segment 5: its 45° steps tolerate only 22.5° of phase error) and in the data.
const TRAIN_KI: f32 = 0.006;
const DATA_KI: f32 = 0.002;
/// Off-axis symbols tolerated in the first `MIN_TRAIN` symbols of a training.
const MAX_OFF_AXIS: u32 = 8;
/// Share of the input power that the matched filter must pass for a Segment 3 (a clean signal
/// passes about 2.0 in these units, white noise less, a tone out of band almost nothing).
const MIN_IN_BAND: f32 = 0.25;
/// Samples per presence block (5 ms).
const BLOCK: usize = 40;

/// The receiver for one speed.
#[derive(Debug, Clone)]
pub struct V27terRx {
    rate: Rate,
    /// Input samples per half symbol.
    half: f32,
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
    /// Energy that left the matched filter in the current presence block.
    mf_energy: f32,
    /// The last block had a share of at least `MIN_IN_BAND` of its input power in the filter's band.
    in_band: bool,
    // equalizer and carrier
    eq_in: VecDeque<C32>,
    w: Vec<C32>,
    phase: f32,
    pll: Loop2,
    last_decision: C32,
    // state
    stage: Stage,
    reversals: u32,
    /// Symbols trained on so far (Segment 4 lasts at least 58).
    trained: u32,
    /// Off-axis symbols among them.
    off_axis: u32,
    raw_prev: C32,
    scrambler: Scrambler,
    // presence
    block: Vec<i16>,
    /// The last 5 ms block was at or above the turn-off threshold: a signal is present. (Not the
    /// turn-on threshold: a signal at -43 dBm0 has its Segment 3 blocks just below it.)
    loud: bool,
    quiet: u32,
    /// The level of the signal since the training was found, for the end of the carrier.
    signal: SignalLevel,
    #[cfg(test)]
    pub(crate) trace: Vec<(u8, f32, f32)>,
    #[cfg(test)]
    pub(crate) changes: Vec<usize>,
}

impl V27terRx {
    /// A receiver for `rate`.
    pub fn new(rate: Rate) -> Self {
        let sps = SAMPLE_RATE as f32 / rate.baud();
        let mut w = vec![C32::default(); EQ_TAPS];
        w[EQ_CENTRE] = C32::new(1.0, 0.0);
        Self {
            rate,
            half: sps / 2.0,
            taps: rrc_taps(sps, 0.5, 4.0),
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
            mf_energy: 0.0,
            in_band: false,
            eq_in: VecDeque::new(),
            w,
            phase: 0.0,
            pll: Loop2::new(0.08, DATA_KI),
            last_decision: C32::new(1.0, 0.0),
            stage: Stage::Acquire,
            reversals: 0,
            trained: 0,
            off_axis: 0,
            raw_prev: C32::default(),
            scrambler: Scrambler::default(),
            block: Vec::with_capacity(BLOCK),
            loud: false,
            quiet: 0,
            signal: SignalLevel::default(),
            #[cfg(test)]
            trace: Vec::new(),
            #[cfg(test)]
            changes: Vec::new(),
        }
    }

    /// The speed.
    pub fn rate(&self) -> Rate {
        self.rate
    }

    /// Whether the equalizer has been trained (Segment 5 seen).
    pub fn is_trained(&self) -> bool {
        self.stage == Stage::Data
    }

    fn reset(&mut self) {
        let rate = self.rate;
        #[cfg(test)]
        let trace = std::mem::take(&mut self.trace);
        #[cfg(test)]
        let changes = std::mem::take(&mut self.changes);
        *self = Self::new(rate);
        #[cfg(test)]
        {
            self.trace = trace;
            self.changes = changes;
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
        let in_ms = block_mean_square(&self.block);
        let level = dbm0(in_ms);
        self.in_band = self.mf_energy >= MIN_IN_BAND * in_ms * BLOCK as f32;
        self.mf_energy = 0.0;
        self.block.clear();
        self.loud = level >= OFF_DBM0;
        if self.stage != Stage::Acquire {
            // the end: below −48 dBm0, or 10 dB below the signal (a noisy line)
            self.quiet = if self.signal.is_quiet(level) {
                self.quiet + 1
            } else {
                0
            };
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
        // input level → about unit symbols (slow; frozen once trained)
        let p = m.norm_sqr();
        self.mf_energy += p;
        if self.stage != Stage::Data {
            // fast attack (a signal starting), slow release
            let a = if p > self.power { 0.2 } else { 0.005 };
            self.power += a * (p - self.power);

            if self.power > 0.0 {
                self.gain = 1.0 / self.power.sqrt();
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
        let k = match self.stage {
            Stage::Acquire if !self.loud => 0.0,
            Stage::Acquire => 0.1,
            Stage::Train => 0.03,
            Stage::Data => 0.005,
        };
        self.t_next -= (k * e.clamp(-1.0, 1.0) * self.half) as f64;
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
            let unit = 2.0 * PI / self.rate.steps() as f32;
            let resid = z.arg() - (z.arg() / unit).round() * unit;
            self.trace
                .push((self.stage as u8, z.norm_sqr().sqrt(), resid.to_degrees()));
        }
        match self.stage {
            Stage::Acquire => {
                // Segment 3: 180° from one symbol to the next
                let d = z * self.raw_prev.conj();
                // (Noise makes 8 reversals in a row about once in 4 s, so the two symbols must also
                // have about the same size, as in Segment 3: a false Training takes the receiver
                // out of the hunt until the noise goes away.)
                let (a, b) = (z.norm_sqr(), self.raw_prev.norm_sqr());
                // And the signal must be in the band of the matched filter: the AGC makes whatever
                // leaks through (a DC offset, a tone out of band) look like a full signal.
                let reversal = self.loud
                    && self.in_band
                    && d.re < -0.35 * (a + b)
                    && a < 2.0 * b
                    && b < 2.0 * a;
                self.reversals = if reversal { self.reversals + 1 } else { 0 };
                self.raw_prev = z;
                if self.reversals >= REVERSALS {
                    self.phase = wrap(self.phase + z.arg());
                    self.last_decision = C32::new(1.0, 0.0);
                    self.stage = Stage::Train;
                    ev.push(RxEvent::Training);
                }
            }
            Stage::Train => {
                // 0° or 180° from the last decision; anything else starts Segment 5
                let rel = (z * self.last_decision.conj()).arg();
                let off_axis = rel.abs() > PI / 3.0 && rel.abs() < 2.0 * PI / 3.0;
                let off_axis = off_axis || (self.rate == Rate::B4800 && is_odd_step(rel));
                // Segment 4 has at least 58 symbols: before that an off-axis symbol is noise, but
                // not many of them: a training that is mostly off-axis was noise
                if off_axis && self.trained < MIN_TRAIN {
                    self.off_axis += 1;
                    if self.off_axis > MAX_OFF_AXIS {
                        self.stage = Stage::Acquire;
                        self.reversals = 0;
                        self.off_axis = 0;
                        self.trained = 0;
                        return;
                    }
                }
                if off_axis && self.trained >= MIN_TRAIN {
                    self.stage = Stage::Data;
                    ev.push(RxEvent::Trained);
                    self.data_symbol(z, bits);
                    return;
                }
                let d = if rel.abs() < PI / 2.0 {
                    self.last_decision
                } else {
                    -self.last_decision
                };
                self.pll.ki = TRAIN_KI;
                self.track(z, d, 0.05);
                self.last_decision = d;
                self.trained += 1;
            }
            Stage::Data => self.data_symbol(z, bits),
        }
    }

    fn data_symbol(&mut self, z: C32, bits: &mut Vec<bool>) {
        let steps = self.rate.steps();
        let unit = 2.0 * PI / steps as f32;
        let k = ((z.arg() / unit).round() as i64).rem_euclid(steps as i64) as usize;
        let d = C32::from_angle(k as f32 * unit);
        let prev =
            ((self.last_decision.arg() / unit).round() as i64).rem_euclid(steps as i64) as usize;
        let change = (k + steps - prev) % steps;
        #[cfg(test)]
        self.changes.push(change);
        self.pll.ki = DATA_KI;
        self.track(z, d, 0.01);
        self.last_decision = d;
        let mut line = Vec::with_capacity(3);
        bits_of(self.rate, change, &mut line);
        for b in line {
            bits.push(self.scrambler.descramble(b));
        }
    }

    /// Carrier loop and equalizer update towards decision `d`.
    fn track(&mut self, z: C32, d: C32, mu: f32) {
        let err_phase = (z * d.conj()).arg();
        self.phase = wrap(self.phase + self.pll.update(err_phase));
        self.adapt(d - z, mu);
    }
}

/// Near an odd multiple of 45° (a change the training never makes).
fn is_odd_step(rel: f32) -> bool {
    let k = (rel / (PI / 4.0)).round() as i64;
    k.rem_euclid(2) == 1 && (rel - k as f32 * PI / 4.0).abs() < PI / 8.0
}

// ---------------------------------------------------------------------- test-only transmitter

/// A V.27 ter transmitter for the tests: the receiver's own, and those of other crates through
/// the `testtx` feature. The independent counterpart is SpanDSP's `v27ter_tx`, in `tools/fax-sim`.
#[cfg(any(test, feature = "testtx"))]
pub mod testtx;

#[cfg(test)]
mod tests;
