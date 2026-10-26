//! M5b (test plan `docs/test-plans/M5.md` section 5): V01-V03 without SpanDSP (the SpanDSP
//! comparison is `tools/fax-sim/tests/v_v21_spandsp.rs`), and the V.21 receiver on arbitrary
//! input.
//!
//! The source here is a second, separately written FSK generator ([`fsk_signal`]): its own phase
//! accumulator, the bit clock from the sample index, and a sender clock deviation and frequency
//! offset built in. Level, noise and deviations as in V01/V02. Noise from a seeded PRNG, three
//! seeds per condition, 3 x 14 bursts x 3 frames per condition.

use fax_modem::hdlc::{HdlcEvent, HdlcRx, encode_burst};
use fax_modem::level::{mean_square, sine_amplitude};
use fax_modem::v21::{BAUD, MARK_HZ, SPACE_HZ, V21Rx, V21Tx};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};

const SEEDS: [u64; 3] = [0x5B02_1001, 0x5B02_1002, 0x5B02_1003];

struct Rng(u64);

impl Rng {
    fn new(seed: u64) -> Self {
        Self(seed ^ 0x9E37_79B9_7F4A_7C15)
    }
    fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x >> 12;
        x ^= x << 25;
        x ^= x >> 27;
        self.0 = x;
        x.wrapping_mul(0x2545_F491_4F6C_DD1D)
    }
    fn uniform(&mut self) -> f64 {
        ((self.next() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    fn gauss(&mut self) -> f64 {
        (-2.0 * self.uniform().ln()).sqrt() * (2.0 * std::f64::consts::PI * self.uniform()).cos()
    }
    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    fn frame(&mut self, i: usize) -> Vec<u8> {
        let mut f = vec![
            0xFF,
            if i.is_multiple_of(2) { 0x13 } else { 0x03 },
            0x80 + (self.below(8) as u8),
        ];
        let n = self.below(31);
        for _ in 0..n {
            f.push(if i % 4 == 3 {
                0xFF
            } else {
                (self.next() >> 20) as u8
            });
        }
        f
    }
}

/// An FSK signal of `bits`: the sender's clock runs `ppm` fast (bit rate and tone frequencies
/// scale with it), every frequency is shifted by `offset_hz`, the level is `level_dbm0`
/// (sine amplitude), and white noise at `snr_db` below the signal power (if any). 10 ms of noise
/// (or silence) on each side.
fn fsk_signal(
    bits: &[bool],
    level_dbm0: f32,
    ppm: f64,
    offset_hz: f64,
    snr_db: Option<f64>,
    rng: &mut Rng,
) -> Vec<i16> {
    let amp = f64::from(sine_amplitude(level_dbm0));
    let k = 1.0 + ppm * 1e-6;
    let n_sig = (bits.len() as f64 * 8000.0 / f64::from(BAUD) / k) as usize;
    let margin = 80;
    let sd = snr_db.map_or(0.0, |s| (amp * amp / 2.0 / 10f64.powf(s / 10.0)).sqrt());
    let mut phase = 0.0f64;
    let mut out = Vec::with_capacity(n_sig + 2 * margin);
    for n in 0..n_sig + 2 * margin {
        let v = if n >= margin && n < margin + n_sig {
            let idx = (((n - margin) as f64) * f64::from(BAUD) * k / 8000.0) as usize;
            let bit = bits[idx.min(bits.len() - 1)];
            let f = (if bit {
                f64::from(MARK_HZ)
            } else {
                f64::from(SPACE_HZ)
            }) * k
                + offset_hz;
            phase += 2.0 * std::f64::consts::PI * f / 8000.0;
            amp * phase.sin()
        } else {
            0.0
        };
        out.push((v + sd * rng.gauss()).round().clamp(-32768.0, 32767.0) as i16);
    }
    out
}

fn receive(audio: &[i16]) -> Vec<(Vec<u8>, bool)> {
    let mut rx = V21Rx::new();
    let mut h = HdlcRx::new();
    let mut out = Vec::new();
    for chunk in audio.chunks(160) {
        for b in rx.feed(chunk) {
            if let Some(HdlcEvent::Frame { data, fcs_ok }) = h.bit(b) {
                out.push((data, fcs_ok));
            }
        }
    }
    out
}

#[derive(Debug, Default)]
struct Tally {
    sent: usize,
    right: usize,
    bad: usize,
    false_good: usize,
}

fn condition(level: f32, ppm: f64, hz: f64, snr: Option<f64>) -> Tally {
    let mut t = Tally::default();
    for seed in SEEDS {
        let mut rng = Rng::new(seed ^ u64::from(level.to_bits()) ^ snr.map_or(0, |s| s as u64));
        for burst in 0..14 {
            let frames: Vec<Vec<u8>> = (0..3).map(|i| rng.frame(burst + i)).collect();
            let bits = encode_burst(&frames, 38, 2);
            let audio = fsk_signal(&bits, level, ppm, hz, snr, &mut rng);
            let mut want: Vec<&Vec<u8>> = frames.iter().collect();
            t.sent += frames.len();
            for (d, ok) in receive(&audio) {
                if !ok {
                    t.bad += 1;
                } else if let Some(i) = want.iter().position(|w| **w == d) {
                    want.remove(i);
                    t.right += 1;
                } else {
                    t.false_good += 1;
                }
            }
        }
    }
    t
}

#[test]
fn v01_the_generator_is_what_it_says() {
    // the generator's level, and that its tones are the V.21 ones: our own sender's signal and
    // the generator's, for the same bits, have the same power and decode the same
    let bits = encode_burst(&[vec![0xFF, 0x13, 0x80]], 38, 2);
    let mut rng = Rng::new(1);
    let g = fsk_signal(&bits, -20.0, 0.0, 0.0, None, &mut rng);
    let t = V21Tx::new(-20.0).modulate(&bits);
    let power = |s: &[i16]| f64::from(fax_modem::level::block_mean_square(s));
    let (pg, pt) = (power(&g[80..g.len() - 80]), power(&t));
    let want = f64::from(mean_square(-20.0));
    assert!(
        (pg / want - 1.0).abs() < 0.02,
        "generator power {pg} vs {want}"
    );
    assert!((pt / want - 1.0).abs() < 0.02, "V21Tx power {pt} vs {want}");
    assert!(g.len() - 160 >= t.len() - 1 && g.len() - 160 <= t.len() + 1);
    // 100 ppm fast: shorter by 100 ppm
    let fast = fsk_signal(&bits, -20.0, 100.0, 0.0, None, &mut rng);
    let ratio = (fast.len() - 160) as f64 / (g.len() - 160) as f64;
    assert!((ratio - (1.0 - 1e-4)).abs() < 2e-4, "{ratio}");
}

#[test]
fn v01_levels_and_snr_with_a_second_source() {
    for level in [-10.0f32, -20.0, -30.0, -43.0] {
        for snr in [15.0, 20.0, 30.0] {
            let t = condition(level, 0.0, 0.0, Some(snr));
            eprintln!("V01 (pure Rust) {level} dBm0, SNR {snr} dB: {t:?}");
            assert_eq!(t.sent, 126);
            assert_eq!(
                (t.right, t.bad, t.false_good),
                (t.sent, 0, 0),
                "{level} {snr}: {t:?}"
            );
        }
    }
}

#[test]
fn v01_snr_12_db_at_most_one_percent() {
    for level in [-10.0f32, -43.0] {
        let t = condition(level, 0.0, 0.0, Some(12.0));
        eprintln!("V01 (pure Rust) {level} dBm0, SNR 12 dB: {t:?}");
        assert_eq!(t.false_good, 0);
        assert!((t.sent - t.right) as f64 / t.sent as f64 <= 0.01, "{t:?}");
    }
}

#[test]
fn v02_clock_deviation_and_frequency_offset_with_a_second_source() {
    for level in [-10.0f32, -30.0, -43.0] {
        for (ppm, hz) in [
            (100.0, 0.0),
            (-100.0, 0.0),
            (0.0, 10.0),
            (0.0, -10.0),
            (100.0, 10.0),
            (-100.0, -10.0),
            (100.0, -10.0),
            (-100.0, 10.0),
        ] {
            let t = condition(level, ppm, hz, Some(20.0));
            eprintln!("V02 (pure Rust) {level} dBm0, {ppm:+} ppm, {hz:+} Hz: {t:?}");
            assert_eq!(
                (t.right, t.bad, t.false_good),
                (t.sent, 0, 0),
                "{level} {ppm} {hz}: {t:?}"
            );
        }
    }
}

#[test]
fn v01_no_frame_is_invented_from_noise_alone_or_a_tone() {
    // noise at -10 ... -43 dBm0 (a receiver behind the presence detector would not see most of
    // it; here it sees all) and a steady mark or space: no good frame
    for level in [-10.0f32, -30.0, -43.0] {
        for seed in SEEDS {
            let mut rng = Rng::new(seed);
            let sd = f64::from(mean_square(level)).sqrt();
            let noise: Vec<i16> = (0..8000 * 20)
                .map(|_| (sd * rng.gauss()).round().clamp(-32768.0, 32767.0) as i16)
                .collect();
            let got = receive(&noise);
            assert!(
                got.iter().all(|(_, ok)| !ok),
                "{level} dBm0 seed {seed:#x}: a good frame in 20 s of noise"
            );
        }
        for bit in [true, false] {
            let s = V21Tx::new(level).modulate(&vec![bit; 3000]);
            assert!(receive(&s).is_empty());
        }
    }
}

// ------------------------------------------------------------------------------------ proptest

fn runner(seed: u8, cases: u32) -> TestRunner {
    TestRunner::new_with_rng(
        Config {
            cases,
            failure_persistence: None,
            ..Config::default()
        },
        TestRng::from_seed(RngAlgorithm::ChaCha, &[seed; 32]),
    )
}

#[test]
fn v_proptest_any_samples_give_no_panic_and_a_bounded_number_of_bits() {
    let strategy = prop_oneof![
        proptest::collection::vec(any::<i16>(), 0..6000),
        proptest::collection::vec(
            prop_oneof![Just(i16::MIN), Just(i16::MAX), Just(0i16)],
            0..6000
        ),
        (any::<i16>(), 0usize..9000).prop_map(|(v, n)| vec![v; n]),
        (
            proptest::collection::vec(any::<bool>(), 1..300),
            -60.0..-3.0f32
        )
            .prop_map(|(bits, level)| V21Tx::new(level).modulate(&bits)),
    ];
    for seed in 1..=3u8 {
        runner(seed, 200)
            .run(&(strategy.clone(), 1usize..700), |(samples, chunk)| {
                let mut rx = V21Rx::new();
                let mut bits = 0usize;
                for c in samples.chunks(chunk) {
                    bits += rx.feed(c).len();
                }
                // 300 bit/s: one bit per 26.67 samples (the clock may run a little fast)
                prop_assert!(
                    bits <= samples.len() / 26 + 2,
                    "{bits} bits from {} samples",
                    samples.len()
                );
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn v_proptest_frames_survive_any_chunking() {
    let strategy = (
        proptest::collection::vec(proptest::collection::vec(any::<u8>(), 1..40), 1..4),
        -43.0..-10.0f32,
        1usize..2000,
    );
    for seed in 1..=3u8 {
        runner(seed, 60)
            .run(&strategy, |(frames, level, chunk)| {
                let mut tx = V21Tx::new(level);
                let mut s = vec![0i16; 400];
                s.extend(tx.modulate(&encode_burst(&frames, 38, 2)));
                s.extend(vec![0i16; 400]);
                let mut rx = V21Rx::new();
                let mut h = HdlcRx::new();
                let mut got = Vec::new();
                for c in s.chunks(chunk) {
                    for b in rx.feed(c) {
                        if let Some(HdlcEvent::Frame { data, fcs_ok: true }) = h.bit(b) {
                            got.push(data);
                        }
                    }
                }
                prop_assert_eq!(got, frames);
                Ok(())
            })
            .unwrap();
    }
}

#[test]
fn v02_deviations_far_beyond_the_plan_still_decode_because_of_the_bit_clock_loop() {
    // not required: +-100 ppm moves the bit clock by 0.04 bit over a 1.5 s burst, which a fixed
    // clock survives; +-2000 ppm moves it by 0.9 bit and needs the loop that follows the
    // transitions (the mutant with the loop gain at 0 fails here)
    for ppm in [2000.0, -2000.0] {
        let t = condition(-20.0, ppm, 0.0, Some(20.0));
        eprintln!("V02 (pure Rust, record) {ppm:+} ppm: {t:?}");
        assert_eq!(
            (t.right, t.bad, t.false_good),
            (t.sent, 0, 0),
            "{ppm}: {t:?}"
        );
    }
}
