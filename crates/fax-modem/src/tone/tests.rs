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

#[test]
fn skipped_samples_count_in_the_positions_and_the_noise_is_remembered() {
    let mut d = PresenceDetector::new();
    assert!(d.feed(&noise(-30.0, 803, 6)).is_empty());
    d.skip(30_000);
    // a signal right where listening resumes, 10 dB above the noise heard before
    let mut s = sine(1650.0, -15.0, 500);
    s.extend(noise(-30.0, 4000, 7));
    let ev = d.feed(&s);
    assert_eq!(ev[0], Presence::On(30_803));
    assert!(
        matches!(ev[1], Presence::Off(p) if p.abs_diff(30_803 + 4000) <= 40),
        "{ev:?}"
    );
}
