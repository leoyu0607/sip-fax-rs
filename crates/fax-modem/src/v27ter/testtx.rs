
use super::*;
use crate::dsp::rrc;

/// Segment 4 / 5 register: "0011110", right-hand-most (newest) first.
pub(crate) const TRAIN_REGISTER: u32 = 0b0111100;

/// A V.27 ter transmitter: turn-on sequence, then scrambled data.
pub struct Tx {
    /// The speed.
    pub rate: Rate,
    /// Level of the signal (dBm0).
    pub level_dbm0: f32,
    /// Long turn-on (50 + 1074 symbols) instead of the short one (14 + 58).
    pub long: bool,
    /// Offset of the carrier from 1800 Hz (Hz).
    pub carrier_offset_hz: f32,
    /// Sample rate of the sender relative to 8000 Hz (1.0001 = +100 ppm).
    pub clock: f64,
}

impl Tx {
    /// The phase changes (in 45° / 90° steps) of the turn-on sequence and of `data`.
    pub fn changes(&self, data: &[bool]) -> Vec<usize> {
        let steps = self.rate.steps();
        let half_turn = steps / 2;
        let mut out = Vec::new();
        // Segment 3
        for _ in 0..if self.long { 50 } else { 14 } {
            out.push(half_turn);
        }
        // Segment 4: the first bit of every three of the PRS
        let mut s = Scrambler::with_register(TRAIN_REGISTER);
        for _ in 0..if self.long { 1074 } else { 58 } {
            let b = [s.scramble(true), s.scramble(true), s.scramble(true)];
            out.push(if b[0] { half_turn } else { 0 });
        }
        // Segment 5: scrambled ones, then the data
        let n = self.rate.bits_per_symbol();
        let mut bits: Vec<bool> = std::iter::repeat_n(true, 8 * n)
            .chain(data.iter().copied())
            .collect();
        while !bits.len().is_multiple_of(n) {
            bits.push(true);
        }
        for g in bits.chunks(n) {
            let line: Vec<bool> = g.iter().map(|&b| s.scramble(b)).collect();
            out.push(change_of(self.rate, &line));
        }
        out
    }

    /// The line signal of `data`, with silence before and after.
    /// The line signal of `data` (after the turn-on sequence), with silence before and after.
    pub fn modulate(&self, data: &[bool]) -> Vec<i16> {
        let steps = self.rate.steps();
        let mut k = 0usize;
        let mut syms = Vec::new();
        for c in self.changes(data) {
            k = (k + c) % steps;
            syms.push(C32::from_angle(2.0 * PI * k as f32 / steps as f32 + 0.3));
        }
        let baud = self.rate.baud() as f64;
        let fs = SAMPLE_RATE as f64 * self.clock;
        let dur = syms.len() as f64 / baud + 0.02;
        let lead = 400usize;
        let n_out = (dur * fs) as usize;
        let mut wave = Vec::with_capacity(n_out);
        for n in 0..n_out {
            let t = n as f64 / fs;
            let ts = t * baud;
            let k0 = ts.floor() as i64;
            let mut s = C32::default();
            for k in k0 - 6..=k0 + 6 {
                if k < 0 || k as usize >= syms.len() {
                    continue;
                }
                s += syms[k as usize].scale(rrc((ts - k as f64) as f32, 0.5));
            }
            let w = 2.0 * std::f64::consts::PI * (CARRIER_HZ + self.carrier_offset_hz) as f64 * t;
            wave.push(s.re * w.cos() as f32 - s.im * w.sin() as f32);
        }
        // exactly `level_dbm0` over the signal
        let ms = wave.iter().map(|x| x * x).sum::<f32>() / wave.len().max(1) as f32;
        let k = (crate::level::mean_square(self.level_dbm0) / ms.max(1e-12)).sqrt();
        let mut out = vec![0i16; lead];
        out.extend(
            wave.iter()
                .map(|x| (k * x).round().clamp(-32768.0, 32767.0) as i16),
        );
        out.extend(vec![0i16; 400]);
        out
    }
}
