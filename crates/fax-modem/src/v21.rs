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
mod tests {
    use super::*;
    use crate::hdlc::{HdlcEvent, HdlcRx, encode_burst};

    fn frames_through(samples: &[i16]) -> Vec<HdlcEvent> {
        let mut rx = V21Rx::new();
        let mut h = HdlcRx::new();
        rx.feed(samples)
            .into_iter()
            .filter_map(|b| h.bit(b))
            .collect()
    }

    #[test]
    fn the_bit_clock_is_exact() {
        let mut tx = V21Tx::new(-12.0);
        // 300 bits are exactly one second
        assert_eq!(tx.modulate(&[true; 300]).len(), 8000);
        assert_eq!(tx.modulate(&[false; 3]).len(), 80);
    }

    #[test]
    fn frames_go_through_the_modem() {
        let frames = vec![
            vec![0xFF, 0x13, 0x80, 0x00, 0xEE, 0xF8, 0x80],
            vec![0xFF, 0x13, 0xFB, 0x01, 0x02],
        ];
        let mut tx = V21Tx::new(-12.0);
        let mut s = vec![0i16; 400];
        s.extend(tx.modulate(&encode_burst(&frames, 38, 2)));
        s.extend(vec![0i16; 400]);
        let ev = frames_through(&s);
        let got: Vec<_> = ev
            .into_iter()
            .filter(|e| matches!(e, HdlcEvent::Frame { .. }))
            .collect();
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
    fn quiet_and_noisy_signals_still_decode() {
        let frames = vec![vec![0xFF, 0xC8, 0x21, 0x5A, 0xA5, 0x00, 0xFF]];
        for level in [-10.0f32, -43.0] {
            let mut tx = V21Tx::new(level);
            let mut s = tx.modulate(&encode_burst(&frames, 38, 2));
            // white noise 20 dB below the signal
            let noise =
                crate::level::sine_amplitude(level - 20.0) / std::f32::consts::SQRT_2 * 3f32.sqrt();
            let mut x = 0x9E37_79B9u32;
            for v in s.iter_mut() {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                let u = (x as f32 / u32::MAX as f32) * 2.0 - 1.0;
                *v = (*v as f32 + noise * u) as i16;
            }
            let ev = frames_through(&s);
            assert!(
                ev.contains(&HdlcEvent::Frame {
                    data: frames[0].clone(),
                    fcs_ok: true
                }),
                "{level} dBm0: {ev:?}"
            );
        }
    }
}
