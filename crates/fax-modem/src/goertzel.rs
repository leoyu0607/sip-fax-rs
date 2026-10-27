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
mod tests;
