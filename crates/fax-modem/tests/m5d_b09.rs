//! M5d (test plan `docs/test-plans/M5.md` §6): B09, the V.29 receiver on input that is not a V.29
//! signal: arbitrary samples, white noise at -60 ... -20 dBm0, silence, V.21, V.27ter and other
//! tones. It must not panic, its output must be bounded, and it must not report a training: with
//! the signal gate at the turn-off threshold (-48 dBm0) the receiver hunts for Segment 2 in
//! every noise louder than that.
//!
//! The normal run covers 60 s of audio per rate. The one hour of white noise per level is
//! `#[ignore]`d; run it in release:
//!
//! ```sh
//! cargo test -p fax-modem --release --test m5d_b09 -- --ignored --nocapture
//! ```

use fax_modem::level::mean_square;
use fax_modem::v21::V21Tx;
use fax_modem::v29::{Rate, RxEvent, V29Rx};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};

const SEEDS: [u64; 3] = [0xB090_0001, 0xB090_0002, 0xB090_0003];
const RATES: [Rate; 2] = [Rate::B7200, Rate::B9600];

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

fn feed_counting(rx: &mut V29Rx, samples: &[i16], chunk: usize) -> Counts {
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
    let mut rx = V29Rx::new(rate);
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
fn b09_white_noise_and_silence_never_make_a_training_short_run() {
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
#[ignore = "one hour of audio per level and rate: cargo test -p fax-modem --release --test m5d_b09 -- --ignored --nocapture"]
fn b09_one_hour_of_white_noise_per_level() {
    let hours: u64 = std::env::var("B09_SECONDS")
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(3600);
    let mut false_trainings = 0;
    for rate in RATES {
        for (i, level) in LEVELS.iter().map(|&l| Some(l)).chain([None]).enumerate() {
            let c = noise_run(rate, level, hours, SEEDS[i % 3]);
            println!(
                "B09 1h {rate:?} {:>9} dBm0: false Training {} (Trained {}, CarrierLost {}, bits {})",
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

/// Any frequency: a pure tone is no training. (A tone at 800 or 2600 Hz turns by 135 degrees a
/// symbol like the A/B alternation of Segment 2, and used to start a training that the end of
/// Segment 3 only then rejected, see `b09_no_pure_tone_is_taken_for_a_training`.)
fn any_tone_freq() -> impl Strategy<Value = f64> {
    50.0..3900.0f64
}

fn input() -> BoxedStrategy<Vec<i16>> {
    let base = prop_oneof![
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
        // a tone, at any level
        (any_tone_freq(), -60.0..-3.0f64, 100usize..6000)
            .prop_map(|(f, level, n)| sine(f, level, n)),
        // V.21 (a modem of the same call), any bits, any level
        (
            proptest::collection::vec(any::<bool>(), 1..300),
            -60.0..-3.0f32
        )
            .prop_map(|(bits, level)| V21Tx::new(level).modulate(&bits)),
    ]
    .boxed();
    // V.27ter (the other high speed modem of T.30) needs the test transmitter: the feature
    // `testtx` (on in `cargo test --workspace`, where fax-in asks for it)
    #[cfg(feature = "testtx")]
    let base = prop_oneof![base, v27ter_signal()].boxed();
    base
}

#[cfg(feature = "testtx")]
fn v27ter_signal() -> impl Strategy<Value = Vec<i16>> {
    (
        any::<bool>(),
        any::<bool>(),
        -45.0..-5.0f32,
        proptest::collection::vec(any::<bool>(), 0..2000),
    )
        .prop_map(|(fast, long, level, bits)| {
            fax_modem::v27ter::testtx::Tx {
                rate: if fast {
                    fax_modem::v27ter::Rate::B4800
                } else {
                    fax_modem::v27ter::Rate::B2400
                },
                level_dbm0: level,
                long,
                carrier_offset_hz: 0.0,
                clock: 1.0,
            }
            .modulate(&bits)
        })
}

#[test]
fn b09_proptest_no_panic_no_event_for_anything_that_is_not_a_v29_signal() {
    for seed in [1u8, 2, 3] {
        for rate in RATES {
            runner(seed, 300)
                .run(
                    &(input(), proptest::collection::vec(1usize..900, 1..6)),
                    |(samples, chunks)| {
                        let mut rx = V29Rx::new(rate);
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

/// The start of a real V.29 transmission up to the first `Trained`, so that a receiver fed with it
/// is in the data state.
#[cfg(feature = "testtx")]
fn trained_lead(rate: Rate) -> Vec<i16> {
    let audio = fax_modem::v29::testtx::Tx {
        rate,
        level_dbm0: -20.0,
        carrier_offset_hz: 0.0,
        clock: 1.0,
    }
    .modulate(&[true; 64]);
    let mut rx = V29Rx::new(rate);
    let mut fed = 0;
    for chunk in audio.chunks(160) {
        fed += chunk.len();
        if rx.feed(chunk).contains(&RxEvent::Trained) {
            return audio[..fed].to_vec();
        }
    }
    panic!("the test transmitter was not trained");
}

#[cfg(feature = "testtx")]
#[test]
fn b09_proptest_after_a_training_anything_is_bounded() {
    // a receiver that has been trained (the turn-on sequence of a real transmission) is then fed
    // arbitrary samples: no panic, and no more bits than the symbol rate allows
    for seed in [1u8, 2, 3] {
        for rate in RATES {
            let lead = trained_lead(rate);
            let bps = rate.bits_per_symbol() as u64;
            runner(seed, 200)
                .run(&input(), |samples| {
                    let mut rx = V29Rx::new(rate);
                    let mut c = feed_counting(&mut rx, &lead, 160);
                    assert_eq!(c.training, 1, "the lead trains the receiver");
                    c += feed_counting(&mut rx, &samples, 160);
                    let total = (lead.len() + samples.len()) as u64;
                    // 2400 symbols per 8000 samples, `bps` bits each
                    prop_assert!(
                        c.bits <= total * 3 * bps / 10 + 8 * bps,
                        "{} bits from {total} samples",
                        c.bits
                    );
                    Ok(())
                })
                .unwrap();
        }
    }
}

/// No pure tone, 100 to 3900 Hz in steps of 25 Hz at -40, -20 and -6 dBm0, makes a training. A
/// tone at 800 or 2600 Hz (the image of 1700 +- 900 Hz) looks like the A/B alternation of
/// Segment 2 to the lock, and the same goes for the bands around 100-350, 650-950, 2450-2750 and
/// 3050-3350 Hz; the fit check at the end of Segment 3 rejects them, without a `Training` event
/// (before it, such a tone gave `Training`, `Trained` and a stream of garbage bits).
#[test]
fn b09_no_pure_tone_is_taken_for_a_training() {
    for rate in RATES {
        let mut f = 100.0;
        while f <= 3900.0 {
            for level in [-40.0, -20.0, -6.0] {
                let mut rx = V29Rx::new(rate);
                let c = feed_counting(&mut rx, &sine(f, level, 12_000), 160);
                assert_eq!(c, Counts::default(), "{rate:?} {f} Hz at {level} dBm0");
            }
            f += 25.0;
        }
    }
}
