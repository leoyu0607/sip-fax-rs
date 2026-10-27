use super::testtx::Tx;
use super::*;
use crate::goertzel::Goertzel;
use crate::level::{block_mean_square, dbm0};

fn prbs(n: usize, seed: u32) -> Vec<bool> {
    let mut x = seed | 1;
    (0..n)
        .map(|_| {
            x ^= x << 13;
            x ^= x >> 17;
            x ^= x << 5;
            x & 1 != 0
        })
        .collect()
}

/// The bits of the points of Segment 4 onward, read back as the receiver will: the phase
/// change from the previous point (Table 1, inverted) and Q1 from the amplitude (Table 2).
fn decode(rate: Rate, syms: &[(i8, i8)]) -> Vec<bool> {
    let mut prev = phase_of(syms[SEGMENT_2 + SEGMENT_3 - 1]);
    let mut bits = Vec::new();
    for &p in &syms[SEGMENT_2 + SEGMENT_3..] {
        let ph = phase_of(p);
        let change = (ph + 8 - prev) % 8;
        prev = ph;
        let q234 = (0u8..8).find(|&q| phase_change(q) == change).unwrap_or(0);
        let m = p.0.abs().max(p.1.abs());
        let q1 = if ph.is_multiple_of(2) { m == 5 } else { m == 3 };
        if rate == Rate::B9600 {
            bits.push(q1);
        }
        bits.extend((0..3).rev().map(|i| (q234 >> i) & 1 != 0));
    }
    bits
}

#[test]
fn the_training_prbs_has_the_four_states_and_the_segment_3_start() {
    let mut p = TrainingPrbs::default();
    let mut states = Vec::new();
    let mut out = Vec::new();
    for _ in 0..7 {
        states.push(format!("{:07b}", p.state));
        out.push(p.next_bit());
    }
    assert_eq!(
        &states[..4],
        ["0101010", "1010101", "1101010", "1110101"].as_slice()
    );
    // C D C D C D C
    assert_eq!(out, [false, true, false, true, false, true, false]);
    // a maximal sequence: 127 states, back to the start
    let mut p = TrainingPrbs::default();
    let start = p;
    for k in 1..=127 {
        p.next_bit();
        assert_eq!(p == start, k == 127, "{k}");
    }
}

#[test]
fn the_scrambler_round_trips_and_is_self_synchronising() {
    let data = prbs(3000, 5);
    let mut s = Scrambler::default();
    let line: Vec<bool> = data.iter().map(|&b| s.scramble(b)).collect();
    // from the zero state
    let mut r = Scrambler::default();
    let back: Vec<bool> = line.iter().map(|&b| r.descramble(b)).collect();
    assert_eq!(back, data);
    // from any state: right after 23 bits
    let mut r = Scrambler {
        reg: 0x2A_5F3C & HISTORY_MASK,
    };
    let back: Vec<bool> = line.iter().map(|&b| r.descramble(b)).collect();
    assert_eq!(&back[23..], &data[23..]);
    // zeros from the zero state stay zero
    let mut z = Scrambler::default();
    assert!(!(0..100).any(|_| z.scramble(false)));
}

#[test]
fn table_1_gives_the_phase_changes() {
    // Q2 Q3 Q4 → phase change in degrees
    let rows = [
        (0b001, 0),
        (0b000, 45),
        (0b010, 90),
        (0b011, 135),
        (0b111, 180),
        (0b110, 225),
        (0b100, 270),
        (0b101, 315),
    ];
    for (q, deg) in rows {
        assert_eq!(u16::from(phase_change(q)) * 45, deg, "{q:03b}");
    }
}

#[test]
fn table_2_gives_the_amplitudes() {
    // absolute phase 0, 90, 180, 270°: Q1 = 0 → 3, Q1 = 1 → 5
    assert_eq!(point(0, false), (3, 0));
    assert_eq!(point(0, true), (5, 0));
    assert_eq!(point(2, false), (0, 3));
    assert_eq!(point(6, true), (0, -5));
    // absolute phase 45, 135, 225, 315°: Q1 = 0 → √2 = (1, 1), Q1 = 1 → 3√2 = (3, 3)
    assert_eq!(point(1, false), (1, 1));
    assert_eq!(point(1, true), (3, 3));
    assert_eq!(point(3, false), (-1, 1));
    assert_eq!(point(7, true), (3, -3));
    for p in 0..8 {
        for q in [false, true] {
            assert_eq!(phase_of(point(p, q)), p);
        }
    }
}

#[test]
fn nine_six_hundred_has_the_16_points() {
    let mut got: Vec<(i8, i8)> = (0..8)
        .flat_map(|p| [point(p, false), point(p, true)])
        .collect();
    let mut want = vec![
        (3, 0),
        (-3, 0),
        (0, 3),
        (0, -3),
        (5, 0),
        (-5, 0),
        (0, 5),
        (0, -5),
        (1, 1),
        (1, -1),
        (-1, 1),
        (-1, -1),
        (3, 3),
        (3, -3),
        (-3, 3),
        (-3, -3),
    ];
    got.sort();
    got.dedup();
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn seven_two_hundred_has_the_8_points() {
    let mut got: Vec<(i8, i8)> = (0..8).map(|p| point(p, false)).collect();
    let mut want = vec![
        (3, 0),
        (-3, 0),
        (0, 3),
        (0, -3),
        (1, 1),
        (1, -1),
        (-1, 1),
        (-1, -1),
    ];
    got.sort();
    got.dedup();
    want.sort();
    assert_eq!(got, want);
}

#[test]
fn figure_4_points_per_rate() {
    assert_eq!(
        figure4(Rate::B9600),
        [(-3, 0), (3, -3), (3, 0), (-3, 3)],
        "9600: A B C D"
    );
    assert_eq!(
        figure4(Rate::B7200),
        [(-3, 0), (1, -1), (3, 0), (-1, 1)],
        "7200: A B C D"
    );
}

#[test]
fn symbols_have_the_turn_on_counts_and_start() {
    for rate in [Rate::B9600, Rate::B7200] {
        let tx = Tx {
            rate,
            level_dbm0: -10.0,
            carrier_offset_hz: 0.0,
            clock: 1.0,
        };
        let data = prbs(1001, 3);
        let s = tx.symbols(&data);
        let n = rate.bits_per_symbol();
        assert_eq!(s.len(), 128 + 384 + 48 + data.len().div_ceil(n), "{rate:?}");
        let [a, b, c, d] = figure4(rate);
        assert_eq!(s[0], a, "{rate:?}");
        assert!(
            s[..SEGMENT_2]
                .iter()
                .enumerate()
                .all(|(i, &p)| p == if i % 2 == 0 { a } else { b }),
            "{rate:?}: A B alternation"
        );
        assert_eq!(s[128..135], [c, d, c, d, c, d, c], "{rate:?}: segment 3");
    }
}

#[test]
fn the_symbols_decode_to_the_data_through_the_scrambler() {
    for rate in [Rate::B9600, Rate::B7200] {
        let tx = Tx {
            rate,
            level_dbm0: -10.0,
            carrier_offset_hz: 0.0,
            clock: 1.0,
        };
        let data = prbs(3001, 11);
        let s = tx.symbols(&data);
        let line = decode(rate, &s);
        let mut r = Scrambler::default();
        let got: Vec<bool> = line.iter().map(|&b| r.descramble(b)).collect();
        let n = rate.bits_per_symbol();
        assert!(got[..SEGMENT_4 * n].iter().all(|&b| b), "{rate:?}");
        assert_eq!(
            &got[SEGMENT_4 * n..SEGMENT_4 * n + data.len()],
            &data[..],
            "{rate:?}"
        );
    }
}

#[test]
fn modulate_is_at_the_level_and_centred_at_1700_hz() {
    for rate in [Rate::B9600, Rate::B7200] {
        let tx = Tx {
            rate,
            level_dbm0: -10.0,
            carrier_offset_hz: 0.0,
            clock: 1.0,
        };
        let audio = tx.modulate(&prbs(20000, 9));
        let mid = &audio[4000..audio.len() - 4000];
        let level = dbm0(block_mean_square(mid));
        assert!((level + 10.0).abs() < 0.5, "{rate:?}: {level} dBm0");
        let peak = audio.iter().map(|x| x.unsigned_abs()).max().unwrap_or(0);
        assert!(peak < 32000, "{rate:?}: peak {peak}");
        let power = |f: f32| {
            let mut g = Goertzel::new(f);
            mid.iter().for_each(|&x| g.update(f32::from(x)));
            g.take()
        };
        let (p1700, p300, p3700) = (power(1700.0), power(300.0), power(3700.0));
        assert!(p1700 > 10.0 * p300, "{rate:?}: 1700 {p1700} 300 {p300}");
        assert!(
            p1700 > 1000.0 * p3700,
            "{rate:?}: 1700 {p1700} 3700 {p3700}"
        );
    }
}

fn receive(rx: &mut V29Rx, audio: &[i16]) -> (Vec<RxEvent>, Vec<bool>) {
    let mut ev = Vec::new();
    let mut bits = Vec::new();
    for chunk in audio.chunks(160) {
        for e in rx.feed(chunk) {
            match e {
                RxEvent::Bits(b) => bits.extend(b),
                other => ev.push(other),
            }
        }
    }
    (ev, bits)
}

/// Adds white Gaussian noise (Box–Muller on a fixed-seed xorshift) at `snr_db` below the
/// signal of `level` dBm0.
fn add_awgn(audio: &mut [i16], level: f32, snr_db: f32, seed: u32) {
    let sigma = (crate::level::mean_square(level) / 10f32.powf(snr_db / 10.0)).sqrt();
    let mut x = seed | 1;
    let mut uniform = move || {
        x ^= x << 13;
        x ^= x >> 17;
        x ^= x << 5;
        (f64::from(x) + 1.0) / (f64::from(u32::MAX) + 2.0)
    };
    for s in audio.iter_mut() {
        let (u1, u2) = (uniform(), uniform());
        let g = (-2.0 * u1.ln()).sqrt() * (2.0 * std::f64::consts::PI * u2).cos();
        *s = (f64::from(*s) + f64::from(sigma) * g)
            .round()
            .clamp(-32768.0, 32767.0) as i16;
    }
}

/// The data errors of a transmission (the data starts after the 48 ones of Segment 4).
fn errors(
    rate: Rate,
    level: f32,
    offset: f32,
    clock: f64,
    snr_db: Option<f32>,
    seed: u32,
) -> (usize, usize) {
    let data = prbs(9600, seed);
    let tx = Tx {
        rate,
        level_dbm0: level,
        carrier_offset_hz: offset,
        clock,
    };
    let mut audio = tx.modulate(&data);
    if let Some(snr) = snr_db {
        add_awgn(&mut audio, level, snr, seed.wrapping_mul(7919) + 1);
    }
    let mut rx = V29Rx::new(rate);
    let (ev, bits) = receive(&mut rx, &audio);
    if !ev.contains(&RxEvent::Training) || !ev.contains(&RxEvent::Trained) {
        return (data.len(), data.len());
    }
    let head = SEGMENT_4 * rate.bits_per_symbol();
    let got = bits.get(head..head + data.len()).unwrap_or(&[]);
    let errs = got.iter().zip(&data).filter(|(a, b)| a != b).count() + (data.len() - got.len());
    (errs, data.len())
}

/// On a line whose noise is above −48 dBm0 the end of the carrier is still heard: the
/// signal drops 10 dB below its own level (M5e, ADR-066).
#[test]
fn the_carrier_end_is_heard_in_noise_above_the_off_threshold() {
    for (rate, snr) in [(Rate::B9600, 20.0), (Rate::B7200, 17.0)] {
        for seed in 1..=3 {
            let tx = Tx {
                rate,
                level_dbm0: -14.0,
                carrier_offset_hz: 0.0,
                clock: 1.0,
            };
            let mut audio = tx.modulate(&prbs(4800, seed));
            audio.extend(vec![0i16; 4000]);
            // noise everywhere, at about -34 / -31 dBm0
            add_awgn(&mut audio, -14.0, snr, seed + 40);
            let mut rx = V29Rx::new(rate);
            let (ev, _) = receive(&mut rx, &audio);
            assert_eq!(
                ev,
                vec![RxEvent::Training, RxEvent::Trained, RxEvent::CarrierLost],
                "{rate:?} seed {seed}"
            );
        }
    }
}

#[test]
fn clean_signals_at_both_speeds() {
    for rate in [Rate::B9600, Rate::B7200] {
        for seed in 1..=3 {
            let data = prbs(9600, seed);
            let tx = Tx {
                rate,
                level_dbm0: -10.0,
                carrier_offset_hz: 0.0,
                clock: 1.0,
            };
            let audio = tx.modulate(&data);
            let mut rx = V29Rx::new(rate);
            let (ev, bits) = receive(&mut rx, &audio);
            assert_eq!(
                ev,
                vec![RxEvent::Training, RxEvent::Trained, RxEvent::CarrierLost],
                "{rate:?} seed {seed}"
            );
            let head = SEGMENT_4 * rate.bits_per_symbol();
            assert!(bits[..head].iter().all(|&b| b), "{rate:?}: segment 4 ones");
            let got = &bits[head..(head + data.len()).min(bits.len())];
            let errs =
                got.iter().zip(&data).filter(|(a, b)| a != b).count() + (data.len() - got.len());
            assert_eq!(errs, 0, "{rate:?} seed {seed}");
        }
    }
}

#[test]
fn silence_and_noise_do_not_train() {
    for rate in [Rate::B9600, Rate::B7200] {
        let mut rx = V29Rx::new(rate);
        let (ev, bits) = receive(&mut rx, &vec![0i16; 16000]);
        assert!(ev.is_empty() && bits.is_empty(), "{rate:?} silence: {ev:?}");
        // white noise at -50 dBm0 (uniform, so the amplitude a has a² / 3 = mean square)
        let a = (3.0 * crate::level::mean_square(-50.0)).sqrt();
        let mut x = 0x9E37_79B9u32;
        let noise: Vec<i16> = (0..16000)
            .map(|_| {
                x ^= x << 13;
                x ^= x >> 17;
                x ^= x << 5;
                let u = x as f32 / u32::MAX as f32 * 2.0 - 1.0;
                (u * a) as i16
            })
            .collect();
        assert!((dbm0(block_mean_square(&noise)) + 50.0).abs() < 0.5);
        let mut rx = V29Rx::new(rate);
        let (ev, _) = receive(&mut rx, &noise);
        assert!(!ev.contains(&RxEvent::Training), "{rate:?} noise: {ev:?}");
    }
}

/// A tone that looks like Segment 2 (800 or 2600 Hz turn by 135 degrees a symbol) starts a
/// training; it does not fit the C / D points of Segment 3, so it is given up before
/// `Training` and `Trained` are reported, and no garbage bits follow. Before the fit check
/// the receiver went on to the data state and, in noise above the turn-off threshold, never
/// left it (M5d B09: one false training in an hour of white noise at -43 dBm0, 9600 bit/s).
/// The real transmission after it is received.
#[test]
fn a_tone_that_looks_like_segment_2_is_given_up_and_the_real_signal_is_received() {
    for rate in [Rate::B9600, Rate::B7200] {
        for hz in [800.0f32, 2600.0] {
            let tone: Vec<i16> = (0..24_000)
                .map(|i| {
                    (crate::level::sine_amplitude(-20.0)
                        * (2.0 * PI * hz * i as f32 / 8000.0).sin()) as i16
                })
                .collect();
            let mut rx = V29Rx::new(rate);
            let (ev, bits) = receive(&mut rx, &tone);
            assert!(
                ev.is_empty() && bits.is_empty(),
                "{rate:?} {hz}: {ev:?}, {} bits",
                bits.len()
            );
            // the same receiver now gets a real transmission (after a pause)
            let data = prbs(4000, 11);
            let tx = Tx {
                rate,
                level_dbm0: -20.0,
                carrier_offset_hz: 0.0,
                clock: 1.0,
            };
            let mut audio = vec![0i16; 4000];
            audio.extend(tx.modulate(&data));
            let (ev, bits) = receive(&mut rx, &audio);
            assert!(ev.contains(&RxEvent::Trained), "{rate:?} {hz}: {ev:?}");
            let off = 48 * rate.bits_per_symbol();
            assert_eq!(&bits[off..off + data.len()], &data[..], "{rate:?} {hz}");
        }
    }
}

/// Time of `feed` on 10 s of 9600 bit/s audio (release build, prints only).
#[test]
#[ignore = "timing report, prints only"]
fn feed_speed_report() {
    let tx = Tx {
        rate: Rate::B9600,
        level_dbm0: -15.0,
        carrier_offset_hz: 0.0,
        clock: 1.0,
    };
    let audio = tx.modulate(&prbs(96_000, 1));
    let audio: Vec<i16> = audio.iter().cycle().take(80_000).copied().collect();
    let start = std::time::Instant::now();
    let mut rx = V29Rx::new(Rate::B9600);
    let _ = receive(&mut rx, &audio);
    eprintln!("feed 10 s of 9600 audio: {:?}", start.elapsed());
}

/// Zero errors on 9600 data bits, seeds 1 to 3, at both speeds.
fn check_clean(level: f32, offset: f32, clock: f64, snr_db: Option<f32>) {
    for rate in [Rate::B9600, Rate::B7200] {
        for seed in 1..=3 {
            let (errs, n) = errors(rate, level, offset, clock, snr_db, seed);
            assert_eq!(
                errs, 0,
                "{rate:?} {level} dBm0 {offset} Hz {clock} {snr_db:?} seed {seed} ({n} bits)"
            );
        }
    }
}

#[test]
fn robustness_level_minus_43() {
    check_clean(-43.0, 0.0, 1.0, None);
}

#[test]
fn robustness_carrier_offset_plus_and_minus_7_hz() {
    check_clean(-15.0, 7.0, 1.0, None);
    check_clean(-15.0, -7.0, 1.0, None);
}

#[test]
fn robustness_clock_plus_and_minus_100_ppm() {
    check_clean(-15.0, 0.0, 1.0001, None);
    check_clean(-15.0, 0.0, 0.9999, None);
}

#[test]
fn robustness_awgn() {
    for (rate, snr) in [(Rate::B9600, 20.0), (Rate::B7200, 17.0)] {
        for seed in 1..=3 {
            let (errs, n) = errors(rate, -15.0, 0.0, 1.0, Some(snr), seed);
            assert_eq!(errs, 0, "{rate:?} {snr} dB seed {seed} ({n} bits)");
        }
    }
}

/// Test plan B04: ±100 ppm and ±7 Hz, each and together, with the noise of B03 (9600 at
/// 20 dB, 7200 at 17 dB). The MER must stay within 1.5 dB of the noise-only case: the
/// in-band SNR is 2.2 dB above the wideband one (the matched filter passes 2400 of 4000 Hz).
#[test]
fn robustness_offset_and_clock_under_noise() {
    for (rate, snr) in [(Rate::B9600, 20.0), (Rate::B7200, 17.0)] {
        let floor = snr + 10.0 * (4000.0f32 / 2400.0).log10();
        for (offset, clock) in [
            (0.0, 1.0001),
            (0.0, 0.9999),
            (7.0, 1.0),
            (-7.0, 1.0),
            (7.0, 1.0001),
            (-7.0, 0.9999),
        ] {
            for seed in 1..=3 {
                let (errs, mer) = errors_and_mer(rate, offset, clock, snr, seed);
                let what = format!("{rate:?} {offset} Hz {clock} {snr} dB seed {seed}");
                assert_eq!(errs, 0, "{what}");
                assert!(
                    mer > floor - 1.5,
                    "{what}: MER {mer:.2} dB, floor {floor:.2}"
                );
            }
        }
    }
}

/// Bit errors and MER (dB, Segment 4 and the data) at −15 dBm0 with noise.
fn errors_and_mer(rate: Rate, offset: f32, clock: f64, snr: f32, seed: u32) -> (usize, f32) {
    let data = prbs(9600, seed);
    let tx = Tx {
        rate,
        level_dbm0: -15.0,
        carrier_offset_hz: offset,
        clock,
    };
    let mut audio = tx.modulate(&data);
    add_awgn(&mut audio, -15.0, snr, seed.wrapping_mul(7919) + 1);
    let mut rx = V29Rx::new(rate);
    let (_, bits) = receive(&mut rx, &audio);
    let head = SEGMENT_4 * rate.bits_per_symbol();
    let got = bits.get(head..head + data.len()).unwrap_or(&[]);
    let errs = got.iter().zip(&data).filter(|(a, b)| a != b).count() + data.len() - got.len();
    (errs, mer_of(&rx, rate, data.len()))
}

/// Test plan B03 over a whole page: 340 000 bits of 7200 (47 s) at 17 dB SNR, also with
/// +100 ppm and +7 Hz. With the timing taken from the equalizer's tap centroid the timing
/// walked, the taps spread and the data was lost after 30 to 40 s in every seed (M5d B03).
#[test]
fn a_long_7200_transmission_at_17_db_stays_locked() {
    for (offset, clock, seed) in [(0.0, 1.0, 1), (7.0, 1.0001, 2)] {
        let rate = Rate::B7200;
        let data = prbs(340_000, seed);
        let tx = Tx {
            rate,
            level_dbm0: -15.0,
            carrier_offset_hz: offset,
            clock,
        };
        let mut audio = tx.modulate(&data);
        add_awgn(&mut audio, -15.0, 17.0, seed.wrapping_mul(7919) + 1);
        let mut rx = V29Rx::new(rate);
        let (_, bits) = receive(&mut rx, &audio);
        let head = SEGMENT_4 * rate.bits_per_symbol();
        let got = bits.get(head..head + data.len()).unwrap_or(&[]);
        assert_eq!(got.len(), data.len(), "{offset} Hz {clock}");
        let errs = got.iter().zip(&data).filter(|(a, b)| a != b).count();
        assert_eq!(errs, 0, "{offset} Hz {clock} seed {seed}");
        let mer = mer_of(&rx, rate, data.len());
        let floor = 17.0 + 10.0 * (4000.0f32 / 2400.0).log10();
        assert!(mer > floor - 1.5, "{offset} Hz {clock}: MER {mer:.2} dB");
    }
}

/// The loop integrator (the frequency estimate, radians per symbol) when Segment 4 starts,
/// for ±7 Hz (prints only).
#[test]
#[ignore = "integrator report, prints only"]
fn integrator_at_training_end() {
    for rate in [Rate::B9600, Rate::B7200] {
        for offset in [7.0f32, -7.0] {
            let data = prbs(9600, 1);
            let tx = Tx {
                rate,
                level_dbm0: -15.0,
                carrier_offset_hz: offset,
                clock: 1.0,
            };
            let audio = tx.modulate(&data);
            let mut rx = V29Rx::new(rate);
            let mut trained = None;
            for chunk in audio.chunks(160) {
                let _ = rx.feed(chunk);
                if rx.is_trained() && trained.is_none() {
                    trained = Some(rx.pll.integ);
                }
            }
            eprintln!(
                "{rate:?} {offset:+} Hz: integrator at Segment 4 {trained:?} rad/symbol \
                     (expected {:.4})",
                2.0 * std::f32::consts::PI * offset / 2400.0
            );
        }
    }
}

/// The timing integrator at the last Data symbol for ±100 ppm (clean; prints only).
#[test]
#[ignore = "timing integrator report, prints only"]
fn timing_integrator_report() {
    for clock in [1.0001f64, 0.9999] {
        let tx = Tx {
            rate: Rate::B9600,
            level_dbm0: -15.0,
            carrier_offset_hz: 0.0,
            clock,
        };
        let audio = tx.modulate(&prbs(9600, 1));
        let mut rx = V29Rx::new(Rate::B9600);
        let mut last = None;
        for chunk in audio.chunks(160) {
            let _ = rx.feed(chunk);
            if rx.stage == Stage::Data {
                last = Some(rx.timing.integ);
            }
        }
        eprintln!(
            "clock {clock}: integrator at the end of the data {last:?} (samples per symbol: x half)"
        );
    }
}

/// MER (dB) over Segment 4 and the data symbols only (not the tail or the noise after it).
fn mer_of(rx: &V29Rx, rate: Rate, data_bits: usize) -> f32 {
    let n = (SEGMENT_4 + data_bits.div_ceil(rate.bits_per_symbol())).min(rx.evm.len());
    let mse = rx.evm[..n].iter().sum::<f32>() / n as f32;
    10.0 * (constellation_power(rate) / mse).log10()
}
