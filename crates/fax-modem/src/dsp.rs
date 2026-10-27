//! Building blocks of the high speed receivers (V.27ter, V.29): complex samples, the square-root
//! raised cosine pulse, cubic interpolation, a second-order loop filter.

use std::f32::consts::PI;
use std::ops::{Add, AddAssign, Mul, Neg, Sub};

/// A complex number.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct C32 {
    /// Real part (in phase).
    pub re: f32,
    /// Imaginary part (quadrature).
    pub im: f32,
}

impl C32 {
    /// `re + j·im`.
    pub const fn new(re: f32, im: f32) -> Self {
        Self { re, im }
    }

    /// `e^{jθ}`.
    pub fn from_angle(theta: f32) -> Self {
        let (s, c) = theta.sin_cos();
        Self { re: c, im: s }
    }

    /// The complex conjugate.
    pub fn conj(self) -> Self {
        Self {
            re: self.re,
            im: -self.im,
        }
    }

    /// |z|².
    pub fn norm_sqr(self) -> f32 {
        self.re * self.re + self.im * self.im
    }

    /// The argument, in (−π, π].
    pub fn arg(self) -> f32 {
        self.im.atan2(self.re)
    }

    /// Multiplication by a real.
    pub fn scale(self, k: f32) -> Self {
        Self {
            re: self.re * k,
            im: self.im * k,
        }
    }
}

impl Add for C32 {
    type Output = C32;
    fn add(self, o: C32) -> C32 {
        C32::new(self.re + o.re, self.im + o.im)
    }
}

impl AddAssign for C32 {
    fn add_assign(&mut self, o: C32) {
        self.re += o.re;
        self.im += o.im;
    }
}

impl Sub for C32 {
    type Output = C32;
    fn sub(self, o: C32) -> C32 {
        C32::new(self.re - o.re, self.im - o.im)
    }
}

impl Mul for C32 {
    type Output = C32;
    fn mul(self, o: C32) -> C32 {
        C32::new(
            self.re * o.re - self.im * o.im,
            self.re * o.im + self.im * o.re,
        )
    }
}

impl Neg for C32 {
    type Output = C32;
    fn neg(self) -> C32 {
        C32::new(-self.re, -self.im)
    }
}

/// The square-root raised cosine pulse with roll-off `alpha`, at `t` symbol periods from its
/// centre (peak 1 + α(4/π − 1) at 0).
pub fn rrc(t: f32, alpha: f32) -> f32 {
    let at = t.abs();
    if at < 1e-6 {
        return 1.0 + alpha * (4.0 / PI - 1.0);
    }
    if alpha > 0.0 && (at - 1.0 / (4.0 * alpha)).abs() < 1e-5 {
        let a = (PI / (4.0 * alpha)).sin();
        let b = (PI / (4.0 * alpha)).cos();
        return alpha / 2f32.sqrt() * ((1.0 + 2.0 / PI) * a + (1.0 - 2.0 / PI) * b);
    }
    let num = (PI * t * (1.0 - alpha)).sin() + 4.0 * alpha * t * (PI * t * (1.0 + alpha)).cos();
    let den = PI * t * (1.0 - (4.0 * alpha * t).powi(2));
    num / den
}

/// FIR taps of a matched RRC filter sampled `samples_per_symbol` times per symbol over ±`span`
/// symbols, normalised to unit energy.
pub fn rrc_taps(samples_per_symbol: f32, alpha: f32, span: f32) -> Vec<f32> {
    let half = (span * samples_per_symbol).ceil() as i32;
    let mut taps: Vec<f32> = (-half..=half)
        .map(|n| rrc(n as f32 / samples_per_symbol, alpha))
        .collect();
    let e: f32 = taps.iter().map(|t| t * t).sum::<f32>().sqrt();
    if e > 0.0 {
        taps.iter_mut().for_each(|t| *t /= e);
    }
    taps
}

/// Cubic (4-point Lagrange) interpolation between `y[1]` and `y[2]` at fraction `mu` ∈ [0, 1).
pub fn cubic(y: [C32; 4], mu: f32) -> C32 {
    let m1 = mu - 1.0;
    let m2 = mu - 2.0;
    let mp = mu + 1.0;
    let c0 = -mu * m1 * m2 / 6.0;
    let c1 = mp * m1 * m2 / 2.0;
    let c2 = -mp * mu * m2 / 2.0;
    let c3 = mp * mu * m1 / 6.0;
    y[0].scale(c0) + y[1].scale(c1) + y[2].scale(c2) + y[3].scale(c3)
}

/// A second-order (proportional + integral) loop filter.
#[derive(Debug, Clone, Copy)]
pub struct Loop2 {
    /// Proportional gain.
    pub kp: f32,
    /// Integral gain.
    pub ki: f32,
    /// The integrator (the frequency estimate of a phase loop).
    pub integ: f32,
}

impl Loop2 {
    /// A loop with these gains.
    pub fn new(kp: f32, ki: f32) -> Self {
        Self { kp, ki, integ: 0.0 }
    }

    /// The correction for `err`.
    pub fn update(&mut self, err: f32) -> f32 {
        self.integ += self.ki * err;
        self.kp * err + self.integ
    }
}

/// Wraps an angle into (−π, π].
pub fn wrap(a: f32) -> f32 {
    let mut a = a % (2.0 * PI);
    if a > PI {
        a -= 2.0 * PI;
    } else if a <= -PI {
        a += 2.0 * PI;
    }
    a
}

#[cfg(test)]
mod tests;
