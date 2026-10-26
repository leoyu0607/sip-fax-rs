//! Fax tones: hearing the calling tone (CNG), sending the answer tone (CED), and telling when
//! the peer's signal starts and ends.

use crate::goertzel::Goertzel;
use crate::level::{block_mean_square, dbm0, sine_amplitude};

/// T.30 calling tone: 1100 Hz, 0.5 s on, 3 s off.
pub const CNG_HZ: f32 = 1100.0;
/// T.30 answer tone: 2100 Hz, 2.6 to 4 s.
pub const CED_HZ: f32 = 2100.0;
/// The quietest signal that counts as present (T.30 / V.21 receive threshold).
pub const ON_DBM0: f32 = -43.0;
/// Below this a present signal has ended (5 dB of hysteresis).
pub const OFF_DBM0: f32 = -48.0;
/// A present signal has also ended when it drops this far below its own level: on a line whose
/// noise is above [`OFF_DBM0`] the absolute threshold is never reached (M5c report §5 ①).
pub const END_DROP_DB: f32 = 10.0;
/// A new signal must also be this far above the noise heard before it.
pub const START_RISE_DB: f32 = 10.0;

/// The turn-off threshold of a signal whose level is `signal_dbm0`: [`OFF_DBM0`], or
/// [`END_DROP_DB`] below the signal when that is higher.
pub fn off_threshold(signal_dbm0: f32) -> f32 {
    OFF_DBM0.max(signal_dbm0 - END_DROP_DB)
}

/// The level of a signal as it goes on, in dB: an average over about 50 ms that a block
/// already below the turn-off threshold does not pull down.
#[derive(Debug, Clone, Copy, Default)]
pub struct SignalLevel {
    level: Option<f32>,
}

impl SignalLevel {
    /// Forgets the signal.
    pub fn reset(&mut self) {
        self.level = None;
    }

    /// The level so far (`None` before the first block).
    pub fn get(&self) -> Option<f32> {
        self.level
    }

    /// The turn-off threshold now.
    pub fn off_threshold(&self) -> f32 {
        self.level.map_or(OFF_DBM0, off_threshold)
    }

    /// Takes in the level of one block; returns whether the block is below the turn-off
    /// threshold (and then leaves the average alone).
    pub fn is_quiet(&mut self, block_dbm0: f32) -> bool {
        if block_dbm0 < self.off_threshold() {
            return true;
        }
        self.level = Some(match self.level {
            None => block_dbm0,
            Some(l) => l + 0.1 * (block_dbm0 - l),
        });
        false
    }
}

// ------------------------------------------------------------------------------------------ CNG

/// Samples per CNG analysis block (20 ms; 50 Hz bins, so 1000 and 1200 Hz fall on nulls).
const CNG_BLOCK: usize = 160;
/// Blocks of tone before CNG is reported (340 ms of a 500 ms tone).
const CNG_BLOCKS: u32 = 17;
/// Share of the block's power that must be at 1100 Hz.
const CNG_PURITY: f32 = 0.7;

/// Hears CNG: reports each burst once, 340 ms after it begins.
#[derive(Debug, Clone)]
pub struct CngDetector {
    g: Goertzel,
    block: Vec<i16>,
    run: u32,
    reported: bool,
}

impl Default for CngDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl CngDetector {
    /// A detector.
    pub fn new() -> Self {
        Self {
            g: Goertzel::new(CNG_HZ),
            block: Vec::with_capacity(CNG_BLOCK),
            run: 0,
            reported: false,
        }
    }

    /// Feeds samples; true when a CNG burst was recognised within them.
    pub fn feed(&mut self, samples: &[i16]) -> bool {
        let mut heard = false;
        for &s in samples {
            self.g.update(s as f32);
            self.block.push(s);
            if self.block.len() == CNG_BLOCK {
                let total = block_mean_square(&self.block);
                let tone = self.g.take();
                self.block.clear();
                // gate at the end-of-signal level, not the start level: a tone at exactly -43 dBm0
                // (the quietest a receiver must handle) has blocks that fall just below -43 with
                // any noise on the line and would never make 17 in a row
                if dbm0(total) >= OFF_DBM0 && tone >= CNG_PURITY * total {
                    self.run += 1;
                    if self.run >= CNG_BLOCKS && !self.reported {
                        self.reported = true;
                        heard = true;
                    }
                } else {
                    self.run = 0;
                    self.reported = false;
                }
            }
        }
        heard
    }
}

// ------------------------------------------------------------------------------- tone generator

/// A sine for a while, then silence for a while, then nothing (CED: 2100 Hz for 3 s, 75 ms of
/// silence).
#[derive(Debug, Clone)]
pub struct ToneGenerator {
    step: f32,
    phase: f32,
    amplitude: f32,
    on: u32,
    off: u32,
}

impl ToneGenerator {
    /// `freq` Hz at `level_dbm0` for `on_ms`, then `off_ms` of silence.
    pub fn new(freq: f32, level_dbm0: f32, on_ms: u32, off_ms: u32) -> Self {
        Self {
            step: 2.0 * std::f32::consts::PI * freq / crate::SAMPLE_RATE as f32,
            phase: 0.0,
            amplitude: sine_amplitude(level_dbm0),
            on: on_ms.saturating_mul(crate::SAMPLE_RATE / 1000),
            off: off_ms.saturating_mul(crate::SAMPLE_RATE / 1000),
        }
    }

    /// The T.30 answer tone at `level_dbm0`: 3 s, then 75 ms of silence.
    pub fn ced(level_dbm0: f32) -> Self {
        Self::new(CED_HZ, level_dbm0, 3000, 75)
    }

    /// Samples still to come.
    pub fn remaining(&self) -> u32 {
        self.on.saturating_add(self.off)
    }

    /// Fills `out` from the start; returns how many samples were written (less than `out.len()`
    /// only at the end, 0 when finished).
    pub fn fill(&mut self, out: &mut [i16]) -> usize {
        let mut n = 0;
        for o in out.iter_mut() {
            if self.on > 0 {
                *o = (self.amplitude * self.phase.sin())
                    .round()
                    .clamp(-32768.0, 32767.0) as i16;
                self.phase += self.step;
                if self.phase > std::f32::consts::TAU {
                    self.phase -= std::f32::consts::TAU;
                }
                self.on -= 1;
            } else if self.off > 0 {
                *o = 0;
                self.off -= 1;
            } else {
                break;
            }
            n += 1;
        }
        n
    }
}

// ----------------------------------------------------------------------------- signal presence

/// Samples per presence block (5 ms).
const PRESENCE_BLOCK: usize = 40;
/// Consecutive blocks above [`ON_DBM0`] before the signal counts as started (15 ms: a click of
/// up to 5 ms touches at most two blocks).
const ON_BLOCKS: u32 = 3;
/// Consecutive blocks below [`OFF_DBM0`] before the signal counts as ended.
const OFF_BLOCKS: u32 = 2;

/// A change of the peer's signal, at a sample position (counted from the first sample fed).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Presence {
    /// The signal started at this sample.
    On(u64),
    /// The signal ended at this sample.
    Off(u64),
}

/// Tells when a signal starts (≥ −43 dBm0, and [`START_RISE_DB`] above the noise heard before,
/// for 15 ms) and ends (< −48 dBm0, or [`END_DROP_DB`] below its own level, for 10 ms); the
/// positions are those of the first block of the run, so within 5 ms of the truth.
#[derive(Debug, Clone, Default)]
pub struct PresenceDetector {
    block: Vec<i16>,
    pos: u64,
    on: bool,
    run: u32,
    run_start: u64,
    /// The level of the signal while it is on.
    signal: SignalLevel,
    /// The noise while no signal is on (dBm0): follows a drop at once, a rise slowly.
    floor: Option<f32>,
    /// The level of the first block of the current run.
    run_level: f32,
}

impl PresenceDetector {
    /// A detector; the signal is absent at first.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a signal is present now.
    pub fn is_on(&self) -> bool {
        self.on
    }

    /// Feeds samples; returns the changes within them.
    pub fn feed(&mut self, samples: &[i16]) -> Vec<Presence> {
        let mut out = Vec::new();
        for &s in samples {
            self.block.push(s);
            if self.block.len() < PRESENCE_BLOCK {
                continue;
            }
            let start = self.pos;
            self.pos += PRESENCE_BLOCK as u64;
            let level = dbm0(block_mean_square(&self.block));
            self.block.clear();
            let flips = if self.on {
                self.signal.is_quiet(level)
            } else {
                // the first block only tells the noise (a signal already on then is missed
                // until it ends)
                let floor = self.floor.unwrap_or(level);
                let loud = level >= ON_DBM0 && level >= floor + START_RISE_DB;
                if !loud {
                    self.floor = Some(match self.floor {
                        Some(f) if level > f => f + 0.02 * (level - f),
                        _ => level,
                    });
                }
                loud
            };
            if !flips {
                self.run = 0;
                continue;
            }
            if self.run == 0 {
                self.run_start = start;
                self.run_level = level;
            }
            self.run += 1;
            let needed = if self.on { OFF_BLOCKS } else { ON_BLOCKS };
            if self.run >= needed {
                self.on = !self.on;
                self.run = 0;
                out.push(if self.on {
                    self.signal.reset();
                    self.signal.is_quiet(level);
                    Presence::On(self.run_start)
                } else {
                    // what is left is the line's noise
                    self.floor = Some(self.run_level);
                    Presence::Off(self.run_start)
                });
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_very_long_tone_does_not_overflow() {
        assert_eq!(
            ToneGenerator::new(1100.0, -10.0, 600_000, 0).remaining(),
            4_800_000
        );
        assert_eq!(
            ToneGenerator::new(1100.0, -10.0, u32::MAX, 0).remaining(),
            u32::MAX
        );
    }

    fn sine(freq: f32, dbm0: f32, ms: u32) -> Vec<i16> {
        let mut g = ToneGenerator::new(freq, dbm0, ms, 0);
        let mut v = vec![0; (ms * 8) as usize];
        assert_eq!(g.fill(&mut v), v.len());
        v
    }

    #[test]
    fn cng_is_heard_once_per_burst_and_neighbours_are_not() {
        let mut d = CngDetector::new();
        let mut burst = sine(CNG_HZ, -20.0, 500);
        burst.extend(vec![0; 3000 * 8]);
        assert!(d.feed(&burst));
        assert!(d.feed(&burst), "the next burst again");
        for f in [1000.0, 1200.0, 2100.0] {
            let mut d = CngDetector::new();
            assert!(!d.feed(&sine(f, -20.0, 500)), "{f} Hz");
        }
        // too quiet
        assert!(!CngDetector::new().feed(&sine(CNG_HZ, -50.0, 500)));
    }

    #[test]
    fn ced_lasts_three_seconds_then_75_ms_of_silence() {
        let mut g = ToneGenerator::ced(-12.0);
        assert_eq!(g.remaining(), 24_000 + 600);
        let mut v = vec![1; 30_000];
        assert_eq!(g.fill(&mut v), 24_600);
        assert!(v[24_000..24_600].iter().all(|&s| s == 0));
        assert_eq!(v[24_600], 1, "beyond the end is left alone");
        assert_eq!(g.fill(&mut v), 0);
        let level = dbm0(block_mean_square(&v[..24_000]));
        assert!((level + 12.0).abs() < 0.1, "{level}");
    }

    #[test]
    fn presence_reports_start_and_end_within_a_block() {
        let mut d = PresenceDetector::new();
        let mut s = vec![0i16; 800]; // 100 ms silence
        s.extend(sine(1650.0, -20.0, 200)); // from sample 800
        s.extend(vec![0i16; 800]); // ends at sample 2400
        let ev = d.feed(&s);
        assert_eq!(ev, vec![Presence::On(800), Presence::Off(2400)]);
        assert!(!d.is_on());
        // a 5 ms click is not a signal
        let mut click = vec![0i16; 400];
        click[100..140].copy_from_slice(&sine(1650.0, -10.0, 5));
        assert!(PresenceDetector::new().feed(&click).is_empty());
    }

    /// White noise at `dbm0` (xorshift, uniform: the level is what counts here).
    fn noise(dbm0: f32, n: usize, seed: u64) -> Vec<i16> {
        let mut x = seed | 1;
        let a = crate::level::mean_square(dbm0).sqrt() * 3f32.sqrt();
        (0..n)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 7;
                x ^= x << 17;
                ((x >> 11) as f32 / (1u64 << 53) as f32 * 2.0 - 1.0) * a
            })
            .map(|v| v as i16)
            .collect()
    }

    #[test]
    fn presence_hears_the_end_of_a_signal_on_a_line_noisier_than_the_off_threshold() {
        for (seed, noise_dbm0) in [(1, -44.0), (2, -35.0), (3, -30.0)] {
            let mut d = PresenceDetector::new();
            let mut s = noise(noise_dbm0, 4000, seed);
            let tone = sine(1650.0, -14.0, 500);
            let n = noise(noise_dbm0, tone.len(), seed + 10);
            s.extend(tone.iter().zip(&n).map(|(a, b)| a.saturating_add(*b)));
            s.extend(noise(noise_dbm0, 8000, seed + 20)); // the signal ends at sample 8000
            let ev = d.feed(&s);
            assert_eq!(ev.len(), 2, "{noise_dbm0} dBm0 noise: {ev:?}");
            match (ev[0], ev[1]) {
                (Presence::On(on), Presence::Off(off)) => {
                    assert!(on.abs_diff(4000) <= 40, "{noise_dbm0}: on at {on}");
                    assert!(off.abs_diff(8000) <= 40, "{noise_dbm0}: off at {off}");
                }
                other => panic!("{noise_dbm0}: {other:?}"),
            }
        }
    }

    #[test]
    fn steady_noise_above_the_on_threshold_is_not_a_signal() {
        let mut d = PresenceDetector::new();
        assert!(d.feed(&noise(-30.0, 16_000, 5)).is_empty());
    }
}
