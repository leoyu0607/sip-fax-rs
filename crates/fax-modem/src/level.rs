//! Signal levels in dBm0 (G.711): a sine at the full scale of the 16-bit linear range is
//! +3.14 dBm0, so a 0 dBm0 sine has an RMS of 32767 / √2 · 10^(−3.14 / 20) ≈ 16141.

/// The level of a full-scale sine (the G.711 overload point).
// (+3.14 dBm0 is G.711's figure, not an approximation of π)
#[allow(clippy::approx_constant)]
pub const FULL_SCALE_DBM0: f32 = 3.14;

/// RMS of a 0 dBm0 signal on the 16-bit linear scale.
pub const DBM0_RMS: f32 = 16_141.0;

/// Peak amplitude of a sine at `dbm0`.
pub fn sine_amplitude(dbm0: f32) -> f32 {
    DBM0_RMS * std::f32::consts::SQRT_2 * 10f32.powf(dbm0 / 20.0)
}

/// Mean square of a signal at `dbm0`.
pub fn mean_square(dbm0: f32) -> f32 {
    let r = DBM0_RMS * 10f32.powf(dbm0 / 20.0);
    r * r
}

/// The level of a mean square, in dBm0 (−∞ for silence is reported as −200).
pub fn dbm0(mean_square: f32) -> f32 {
    if mean_square <= 0.0 {
        return -200.0;
    }
    10.0 * (mean_square / (DBM0_RMS * DBM0_RMS)).log10()
}

/// Mean square of a block of samples.
pub fn block_mean_square(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum / samples.len() as f64) as f32
}

#[cfg(test)]
mod tests;
