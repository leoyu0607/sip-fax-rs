//! M5c (test plan `docs/test-plans/M5.md` §6): A09, the V.27ter receiver on input that is not a
//! V.27ter signal: arbitrary samples, white noise at -60 ... -20 dBm0, silence, V.21 and other
//! tones. It must not panic, its output must be bounded, and it must not report a training:
//! with the signal gate at the turn-off threshold (-48 dBm0) the receiver hunts for Segment 3 in
//! every noise louder than that.
//!
//! The normal run covers 60 s of audio per rate. The one hour of white noise per level is
//! `#[ignore]`d; run it in release:
//!
//! ```sh
//! cargo test -p fax-modem --release --test m5c_a09 -- --ignored --nocapture
//! ```

use fax_modem::level::mean_square;
use fax_modem::v21::V21Tx;
use fax_modem::v27ter::{Rate, RxEvent, V27terRx};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};

const SEEDS: [u64; 3] = [0xA090_0001, 0xA090_0002, 0xA090_0003];
const RATES: [Rate; 2] = [Rate::B2400, Rate::B4800];

/// xorshift64* with a Gaussian on top.
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
}

/// What a receiver said over some input.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
struct Counts {
    training: u64,
    trained: u64,
    lost: u64,
    bits: u64,
}

impl std::ops::AddAssign for Counts {
    fn add_assign(&mut self, o: Counts) {
        self.training += o.training;
        self.trained += o.trained;
        self.lost += o.lost;
        self.bits += o.bits;
    }
}

fn feed_counting(rx: &mut V27terRx, samples: &[i16], chunk: usize) -> Counts {
    let mut c = Counts::default();
    for part in samples.chunks(chunk.max(1)) {
        for ev in rx.feed(part) {
            match ev {
                RxEvent::Training => c.training += 1,
                RxEvent::Trained => c.trained += 1,
                RxEvent::Bits(b) => c.bits += b.len() as u64,
                RxEvent::CarrierLost => c.lost += 1,
            }
        }
    }
    c
}

/// White noise of `seconds` at `level` dBm0 (`None`: silence) through a fresh receiver, one
/// second at a time.
fn noise_run(rate: Rate, level: Option<f64>, seconds: u64, seed: u64) -> Counts {
    let mut rx = V27terRx::new(rate);
    let mut rng = Rng::new(seed);
    let sd = level.map_or(0.0, |l| f64::from(mean_square(l as f32)).sqrt());
    let mut total = Counts::default();
    let mut buf = vec![0i16; 8000];
    for _ in 0..seconds {
        for s in buf.iter_mut() {
            *s = (sd * rng.gauss()).round().clamp(-32768.0, 32767.0) as i16;
        }
        total += feed_counting(&mut rx, &buf, 160);
    }
    total
}

const LEVELS: [f64; 10] = [
    -60.0, -55.0, -50.0, -48.0, -46.0, -43.0, -40.0, -30.0, -25.0, -20.0,
];

#[test]
fn a09_white_noise_and_silence_never_make_a_training_short_run() {
    // 3 seeds x (10 levels + silence) x 2 rates: 60 s of audio per rate and seed ... keep the
    // wall-clock short: 2 s per level per seed is 66 s of audio per rate
    for rate in RATES {
        let mut total = Counts::default();
        for seed in SEEDS {
            for level in LEVELS.iter().map(|&l| Some(l)).chain([None]) {
                let c = noise_run(rate, level, 2, seed);
                assert_eq!(
                    c,
                    Counts::default(),
                    "{rate:?} {level:?} dBm0 seed {seed:#x}: events"
                );
                total += c;
            }
        }
        assert_eq!(total.training, 0);
    }
}

/// One hour of white noise at each level, and silence, per rate: the count of false `Training`
/// (and of what follows from it) per level. The plan asks for 0.
#[test]
#[ignore = "one hour of audio per level and rate: cargo test -p fax-modem --release --test m5c_a09 -- --ignored --nocapture"]
fn a09_one_hour_of_white_noise_per_level() {
    let hours: u64 = std::env::var("A09_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3600);
    let mut false_trainings = 0;
    for rate in RATES {
        for (i, level) in LEVELS.iter().map(|&l| Some(l)).chain([None]).enumerate() {
            let c = noise_run(rate, level, hours, SEEDS[i % 3]);
            println!(
                "A09 1h {rate:?} {:>9} dBm0: false Training {} (Trained {}, CarrierLost {}, bits {})",
                level.map_or("silence".to_string(), |l| format!("{l:.0}")),
                c.training,
                c.trained,
                c.lost,
                c.bits
            );
            false_trainings += c.training;
        }
    }
    assert_eq!(false_trainings, 0, "false Training events");
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

/// A sine of `freq` Hz at `level` dBm0.
fn sine(freq: f64, level: f64, n: usize) -> Vec<i16> {
    let a = f64::from(fax_modem::level::sine_amplitude(level as f32));
    (0..n)
        .map(|i| (a * (2.0 * std::f64::consts::PI * freq * i as f64 / 8000.0).sin()).round() as i16)
        .collect()
}

/// A frequency that is not one of a Segment 3 (a V.27ter turn-on is a tone at 1800 Hz plus or
/// minus half the symbol rate: 1000 / 2600 Hz at 4800 bit/s, 1200 / 2400 Hz at 2400 bit/s; the
/// receiver takes +-300 Hz of them for it). Everything else is not training.
fn not_segment3_freq() -> impl Strategy<Value = f64> {
    prop_oneof![50.0..650.0f64, 1450.0..2150.0f64, 2950.0..3900.0f64]
}

fn input() -> impl Strategy<Value = Vec<i16>> {
    prop_oneof![
        // arbitrary samples
        proptest::collection::vec(any::<i16>(), 0..4000),
        // runs of one value (silence, DC, full scale) with other runs between
        proptest::collection::vec(
            (any::<i16>(), 1usize..300).prop_map(|(v, n)| vec![v; n]),
            0..30
        )
        .prop_map(|runs| runs.concat()),
        proptest::collection::vec(
            prop_oneof![Just(i16::MIN), Just(i16::MAX), Just(0i16)],
            0..4000
        ),
        // white noise at any level from -70 to -10 dBm0
        (any::<u64>(), -70.0..-10.0f64, 0usize..6000).prop_map(|(seed, level, n)| {
            let mut rng = Rng::new(seed);
            let sd = f64::from(mean_square(level as f32)).sqrt();
            (0..n)
                .map(|_| (sd * rng.gauss()).round().clamp(-32768.0, 32767.0) as i16)
                .collect()
        }),
        // silence
        (0usize..8000).prop_map(|n| vec![0i16; n]),
        // a tone that is not a turn-on, at any level
        (not_segment3_freq(), -60.0..-3.0f64, 100usize..6000)
            .prop_map(|(f, level, n)| sine(f, level, n)),
        // V.21 (a modem of the same call), any bits, any level
        (
            proptest::collection::vec(any::<bool>(), 1..300),
            -60.0..-3.0f32
        )
            .prop_map(|(bits, level)| V21Tx::new(level).modulate(&bits)),
    ]
}

#[test]
fn a09_proptest_no_panic_no_event_for_anything_that_is_not_a_v27ter_signal() {
    for seed in [1u8, 2, 3] {
        for rate in RATES {
            runner(seed, 300)
                .run(
                    &(input(), proptest::collection::vec(1usize..900, 1..6)),
                    |(samples, chunks)| {
                        let mut rx = V27terRx::new(rate);
                        let mut c = Counts::default();
                        let mut rest: &[i16] = &samples;
                        for n in chunks.iter().cycle().take(chunks.len() * 64) {
                            if rest.is_empty() {
                                break;
                            }
                            let (a, b) = rest.split_at((*n).min(rest.len()));
                            c += feed_counting(&mut rx, a, 160);
                            rest = b;
                        }
                        c += feed_counting(&mut rx, rest, 160);
                        prop_assert_eq!(c, Counts::default());
                        Ok(())
                    },
                )
                .unwrap();
        }
    }
}

#[test]
fn a09_proptest_after_a_training_anything_is_bounded() {
    // a receiver that has been trained (the turn-on sequence of a real transmission is fed by the
    // unit tests; here a tone at the Segment 3 frequency does it) is then fed arbitrary samples:
    // no panic, and no more bits than the symbol rate allows
    for seed in [1u8, 2, 3] {
        for rate in RATES {
            let f = 1800.0 + f64::from(rate.baud()) / 2.0;
            runner(seed, 200)
                .run(&input(), |samples| {
                    let mut rx = V27terRx::new(rate);
                    let lead = sine(f, -20.0, 1600);
                    let mut c = feed_counting(&mut rx, &lead, 160);
                    c += feed_counting(&mut rx, &samples, 160);
                    let total = (lead.len() + samples.len()) as u64;
                    // bits per sample: at most 3 bits per symbol, 1600 symbols per 8000 samples
                    prop_assert!(
                        c.bits <= total * 3 / 5 + 8,
                        "{} bits from {total} samples",
                        c.bits
                    );
                    Ok(())
                })
                .unwrap();
        }
    }
}

/// Recorded, not required: the calling tone (1100 Hz) is within the Segment 3 band of both rates
/// (a tone at 1800 Hz minus half the symbol rate is a turn-on to this receiver); it is only
/// ever heard before the DCS, when no V.27ter receiver exists yet.
#[test]
fn a09_recorded_the_calling_tone_looks_like_segment_3() {
    for rate in RATES {
        let mut rx = V27terRx::new(rate);
        let c = feed_counting(&mut rx, &sine(1100.0, -20.0, 8000), 160);
        println!("A09 (recorded) 1100 Hz at {rate:?}: {c:?}");
    }
}
