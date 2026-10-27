//! ITU-T V.21 channel 2 (the T.30 control channel): 300 bit/s binary FSK, mark (1) = 1650 Hz,
//! space (0) = 1850 Hz.
//!
//! * [`V21Tx`]: phase-continuous FSK; the bit clock is exact over time (26⅔ samples per bit).
//! * [`V21Rx`]: non-coherent detection — the power at each tone over a sliding window of one
//!   bit, their difference decides the bit — and a digital PLL that samples in the middle of
//!   each bit, steered by the transitions.

use crate::SAMPLE_RATE;
use crate::level::sine_amplitude;
use std::collections::VecDeque;
use std::f32::consts::TAU;

/// Mark (binary 1).
pub const MARK_HZ: f32 = 1650.0;
/// Space (binary 0).
pub const SPACE_HZ: f32 = 1850.0;
/// Bits per second.
pub const BAUD: u32 = 300;

// ------------------------------------------------------------------------------------- sender

/// FSK sender.
#[derive(Debug, Clone)]
pub struct V21Tx {
    amplitude: f32,
    phase: f32,
    /// Bit clock: + BAUD per sample, a new bit at every SAMPLE_RATE.
    clock: u32,
}

impl V21Tx {
    /// A sender at `level_dbm0`.
    pub fn new(level_dbm0: f32) -> Self {
        Self {
            amplitude: sine_amplitude(level_dbm0),
            phase: 0.0,
            clock: 0,
        }
    }

    /// The samples of `bits`, continuing the phase of earlier calls.
    pub fn modulate(&mut self, bits: &[bool]) -> Vec<i16> {
        let mut out = Vec::with_capacity(bits.len() * (SAMPLE_RATE / BAUD + 1) as usize);
        for &b in bits {
            let step = TAU * if b { MARK_HZ } else { SPACE_HZ } / SAMPLE_RATE as f32;
            loop {
                out.push((self.amplitude * self.phase.sin()).round() as i16);
                self.phase += step;
                if self.phase >= TAU {
                    self.phase -= TAU;
                }
                self.clock += BAUD;
                if self.clock >= SAMPLE_RATE {
                    self.clock -= SAMPLE_RATE;
                    break;
                }
            }
        }
        out
    }
}

// ----------------------------------------------------------------------------------- receiver

/// Correlation window: one bit (27 samples ≈ 26⅔).
const WINDOW: usize = 27;
/// How strongly a transition pulls the bit clock towards it.
const PLL_GAIN: f32 = 0.25;

/// A sliding correlator at one frequency.
#[derive(Debug, Clone)]
struct Correlator {
    step: f32,
    phase: f32,
    hist: VecDeque<(f32, f32)>,
    i: f32,
    q: f32,
}

impl Correlator {
    fn new(freq: f32) -> Self {
        Self {
            step: TAU * freq / SAMPLE_RATE as f32,
            phase: 0.0,
            hist: VecDeque::with_capacity(WINDOW + 1),
            i: 0.0,
            q: 0.0,
        }
    }

    /// Adds a sample, drops the oldest; returns the power in the window.
    fn update(&mut self, x: f32) -> f32 {
        let (s, c) = self.phase.sin_cos();
        self.phase += self.step;
        if self.phase >= TAU {
            self.phase -= TAU;
        }
        let p = (x * c, x * s);
        self.i += p.0;
        self.q += p.1;
        self.hist.push_back(p);
        if self.hist.len() > WINDOW
            && let Some(old) = self.hist.pop_front()
        {
            self.i -= old.0;
            self.q -= old.1;
        }
        self.i * self.i + self.q * self.q
    }

    fn renormalize(&mut self) {
        // recompute the sums from the window (float drift over a long call)
        self.i = self.hist.iter().map(|p| p.0).sum();
        self.q = self.hist.iter().map(|p| p.1).sum();
    }
}

/// FSK receiver: samples in, bits out.
#[derive(Debug, Clone)]
pub struct V21Rx {
    mark: Correlator,
    space: Correlator,
    /// Bit clock phase in bits: a bit is taken when it passes 1.
    clock: f32,
    last: bool,
    since_renorm: u32,
}

impl Default for V21Rx {
    fn default() -> Self {
        Self::new()
    }
}

impl V21Rx {
    /// A receiver.
    pub fn new() -> Self {
        Self {
            mark: Correlator::new(MARK_HZ),
            space: Correlator::new(SPACE_HZ),
            clock: 0.0,
            last: true,
            since_renorm: 0,
        }
    }

    /// Feeds samples; returns the bits decided within them.
    pub fn feed(&mut self, samples: &[i16]) -> Vec<bool> {
        let per_sample = BAUD as f32 / SAMPLE_RATE as f32;
        let mut bits = Vec::with_capacity(samples.len() / 26 + 1);
        for &s in samples {
            let x = s as f32;
            let m = self.mark.update(x);
            let sp = self.space.update(x);
            let b = m > sp;
            if b != self.last {
                // a transition should fall half way between two sampling instants
                let err = self.clock - 0.5;
                self.clock -= PLL_GAIN * err;
                self.last = b;
            }
            self.clock += per_sample;
            if self.clock >= 1.0 {
                self.clock -= 1.0;
                bits.push(b);
            }
            self.since_renorm += 1;
            if self.since_renorm >= SAMPLE_RATE {
                self.since_renorm = 0;
                self.mark.renormalize();
                self.space.renormalize();
            }
        }
        bits
    }
}

#[cfg(test)]
mod tests;
