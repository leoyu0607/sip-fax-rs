//! The Goertzel filter: the power of one frequency over a block.

use crate::SAMPLE_RATE;

/// A Goertzel filter for one frequency.
#[derive(Debug, Clone, Copy)]
pub struct Goertzel {
    coeff: f32,
    s1: f32,
    s2: f32,
    n: u32,
}

impl Goertzel {
    /// A filter for `freq` Hz.
    pub fn new(freq: f32) -> Self {
        Self {
            coeff: 2.0 * (2.0 * std::f32::consts::PI * freq / SAMPLE_RATE as f32).cos(),
            s1: 0.0,
            s2: 0.0,
            n: 0,
        }
    }

    /// Feeds one sample.
    pub fn update(&mut self, x: f32) {
        let s = x + self.coeff * self.s1 - self.s2;
        self.s2 = self.s1;
        self.s1 = s;
        self.n += 1;
    }

    /// The mean square the frequency contributes to the samples fed since the last reset
    /// (a sine of amplitude A at the filter's frequency gives A² / 2, like
    /// [`crate::level::block_mean_square`] of the sine alone); then resets.
    pub fn take(&mut self) -> f32 {
        let n = self.n.max(1) as f32;
        let p = self.s1 * self.s1 + self.s2 * self.s2 - self.coeff * self.s1 * self.s2;
        self.s1 = 0.0;
        self.s2 = 0.0;
        self.n = 0;
        2.0 * p.max(0.0) / (n * n)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn power(freq: f32, tone: f32, n: usize) -> f32 {
        let mut g = Goertzel::new(freq);
        for i in 0..n {
            g.update(1000.0 * (2.0 * std::f32::consts::PI * tone * i as f32 / 8000.0).sin());
        }
        g.take()
    }

    #[test]
    fn measures_its_frequency_and_not_its_neighbours() {
        // amplitude 1000: mean square 500 000
        let on = power(1100.0, 1100.0, 160);
        assert!((on - 500_000.0).abs() / 500_000.0 < 0.02, "{on}");
        // 1000 and 1200 Hz fall on the nulls of a 160-sample block (50 Hz bins)
        assert!(power(1100.0, 1000.0, 160) < 500.0);
        assert!(power(1100.0, 1200.0, 160) < 500.0);
        // reset after take
        let mut g = Goertzel::new(1100.0);
        assert_eq!(g.take(), 0.0);
    }
}
