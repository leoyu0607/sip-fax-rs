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
                if dbm0(total) >= ON_DBM0 && tone >= CNG_PURITY * total {
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
            on: on_ms * crate::SAMPLE_RATE / 1000,
            off: off_ms * crate::SAMPLE_RATE / 1000,
        }
    }

    /// The T.30 answer tone at `level_dbm0`: 3 s, then 75 ms of silence.
    pub fn ced(level_dbm0: f32) -> Self {
        Self::new(CED_HZ, level_dbm0, 3000, 75)
    }

    /// Samples still to come.
    pub fn remaining(&self) -> u32 {
        self.on + self.off
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

/// Tells when a signal starts (≥ −43 dBm0 for 15 ms) and ends (< −48 dBm0 for 10 ms); the
/// positions are those of the first block of the run, so within 5 ms of the truth.
#[derive(Debug, Clone, Default)]
pub struct PresenceDetector {
    block: Vec<i16>,
    pos: u64,
    on: bool,
    run: u32,
    run_start: u64,
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
                level < OFF_DBM0
            } else {
                level >= ON_DBM0
            };
            if !flips {
                self.run = 0;
                continue;
            }
            if self.run == 0 {
                self.run_start = start;
            }
            self.run += 1;
            let needed = if self.on { OFF_BLOCKS } else { ON_BLOCKS };
            if self.run >= needed {
                self.on = !self.on;
                self.run = 0;
                out.push(if self.on {
                    Presence::On(self.run_start)
                } else {
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
}
