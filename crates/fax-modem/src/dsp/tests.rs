use super::*;

#[test]
fn rrc_is_a_nyquist_root() {
    // the RRC convolved with itself is zero at non-zero whole symbols (sampled finely)
    let sps = 20.0;
    let taps = rrc_taps(sps, 0.5, 8.0);
    let n = taps.len();
    let conv = |shift: usize| -> f32 { (0..n - shift).map(|i| taps[i] * taps[i + shift]).sum() };
    let peak = conv(0);
    assert!((peak - 1.0).abs() < 1e-3);
    for k in 1..4 {
        assert!(
            conv(k * sps as usize).abs() < 0.01,
            "isi at {k}: {}",
            conv(k * sps as usize)
        );
    }
    assert!(rrc(0.5, 0.5).is_finite());
}

#[test]
fn cubic_is_exact_on_a_cubic() {
    let f = |x: f32| C32::new(x * x * x - 2.0 * x, 1.0 - x * x);
    let y = [f(-1.0), f(0.0), f(1.0), f(2.0)];
    for mu in [0.0f32, 0.25, 0.5, 0.9] {
        let got = cubic(y, mu);
        let want = f(mu);
        assert!((got - want).norm_sqr() < 1e-8, "{mu}");
    }
    assert!((wrap(3.0 * PI) - PI).abs() < 1e-5);
    assert!((C32::from_angle(1.0).arg() - 1.0).abs() < 1e-6);
}
