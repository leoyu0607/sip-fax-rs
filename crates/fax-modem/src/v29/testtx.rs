
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
            let w = 2.0 * std::f64::consts::PI * f64::from(CARRIER_HZ + self.carrier_offset_hz) * t;
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
