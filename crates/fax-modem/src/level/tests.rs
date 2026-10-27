use super::*;

#[test]
fn a_sine_measures_at_its_level() {
    for want in [0.0f32, -10.0, -43.0] {
        let a = sine_amplitude(want);
        let s: Vec<i16> = (0..8000)
            .map(|n| (a * (2.0 * std::f32::consts::PI * 1000.0 * n as f32 / 8000.0).sin()) as i16)
            .collect();
        let got = dbm0(block_mean_square(&s));
        assert!((got - want).abs() < 0.1, "{want} -> {got}");
    }
    // full scale is +3.14 dBm0
    assert!((dbm0(mean_square(FULL_SCALE_DBM0)) - FULL_SCALE_DBM0).abs() < 1e-3);
    assert!((sine_amplitude(FULL_SCALE_DBM0) - 32767.0).abs() < 5.0);
    assert_eq!(dbm0(0.0), -200.0);
    assert_eq!(block_mean_square(&[]), 0.0);
}
