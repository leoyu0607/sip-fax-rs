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
