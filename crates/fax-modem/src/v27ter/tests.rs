use super::testtx::{TRAIN_REGISTER, Tx};
use super::*;

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

#[test]
fn the_training_pattern_is_table_4() {
    // 4800: Segment 4 starts 0° 180° 180° 180° 180° 180° 0° and ends 180° 180° 0° 0°,
    // Segment 5 is 270° 225° 315° 90° 45° 45° 180° 180°
    let tx = Tx {
        rate: Rate::B4800,
        level_dbm0: -10.0,
        long: false,
        carrier_offset_hz: 0.0,
        clock: 1.0,
    };
    let c = tx.changes(&[]);
    let deg = |v: &[usize]| v.iter().map(|s| s * 45).collect::<Vec<_>>();
    assert_eq!(deg(&c[14..21]), vec![0, 180, 180, 180, 180, 180, 0]);
    assert_eq!(deg(&c[14 + 54..14 + 58]), vec![180, 180, 0, 0]);
    assert_eq!(deg(&c[72..80]), vec![270, 225, 315, 90, 45, 45, 180, 180]);
    // 2400: Segment 5 is 270° 90° 270° 270° 270° 270° 0° 0°
    let tx = Tx {
        rate: Rate::B2400,
        ..tx
    };
    let c = tx.changes(&[]);
    assert_eq!(
        c[72..80].iter().map(|s| s * 90).collect::<Vec<_>>(),
        vec![270, 90, 270, 270, 270, 270, 0, 0]
    );
    // the long Segment 4 ends like the short one
    let long = Tx {
        rate: Rate::B4800,
        long: true,
        ..tx
    };
    let c = long.changes(&[]);
    assert_eq!(deg(&c[50 + 1070..50 + 1074]), vec![180, 180, 0, 0]);
    let _ = TRAIN_REGISTER;
}

#[test]
fn the_descrambler_undoes_the_scrambler_with_the_guard() {
    let data = prbs(5000, 7);
    // long runs of ones and zeros make the guard act
    let mut d = data.clone();
    d.extend(vec![true; 300]);
    d.extend(vec![false; 300]);
    d.extend(std::iter::repeat_n([true, false, false], 200).flatten());
    let mut s = Scrambler::default();
    let mut r = Scrambler::default();
    let line: Vec<bool> = d.iter().map(|&b| s.scramble(b)).collect();
    let back: Vec<bool> = line.iter().map(|&b| r.descramble(b)).collect();
    assert_eq!(back, d);
    // the line never repeats a short pattern for long
    let longest_run = line.windows(2).fold((0, 0), |(best, run), w| {
        let run = if w[0] == w[1] { run + 1 } else { 0 };
        (best.max(run), run)
    });
    assert!(longest_run.0 < 45, "{}", longest_run.0);
}

fn receive(rx: &mut V27terRx, audio: &[i16]) -> (Vec<RxEvent>, Vec<bool>) {
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

/// The data bits found in the received stream: the first `n` bits after the scrambled
/// ones of Segment 5.
fn find(bits: &[bool], data: &[bool]) -> Option<usize> {
    let probe = &data[..64];
    (0..bits.len().saturating_sub(64)).find(|&i| &bits[i..i + 64] == probe)
}

fn errors(
    rate: Rate,
    level: f32,
    long: bool,
    offset: f32,
    clock: f64,
    seed: u32,
) -> (usize, usize) {
    let data = prbs(9600, seed);
    let tx = Tx {
        rate,
        level_dbm0: level,
        long,
        carrier_offset_hz: offset,
        clock,
    };
    let audio = tx.modulate(&data);
    let mut rx = V27terRx::new(rate);
    let (ev, bits) = receive(&mut rx, &audio);
    assert!(ev.contains(&RxEvent::Training), "{rate:?}: {ev:?}");
    assert!(ev.contains(&RxEvent::Trained), "{rate:?}: {ev:?}");
    if level < -40.0 {
        eprintln!("{rate:?} {level}: events {ev:?}, {} bits", bits.len());
        let p16 = &data[..16];
        if let Some(at) = (0..bits.len() - 16).find(|&i| &bits[i..i + 16] == p16) {
            let first_bad = bits[at..].iter().zip(&data).position(|(a, b)| a != b);
            eprintln!("data at {at}, first error at data bit {first_bad:?}");
        } else {
            eprintln!(
                "16-bit probe not found; first 40 bits {:?}",
                bits[..40].iter().map(|&b| b as u8).collect::<Vec<_>>()
            );
        }
    }
    let Some(at) = find(&bits, &data) else {
        panic!(
            "{rate:?} {level} dBm0 {offset} Hz: data not found in {} bits",
            bits.len()
        );
    };
    let got = &bits[at..(at + data.len()).min(bits.len())];
    let errs = got.iter().zip(&data).filter(|(a, b)| a != b).count() + (data.len() - got.len());
    (errs, data.len())
}

/// Prints what the receiver decided (M5c debugging aid): `cargo test -p fax-modem --release
/// debug_trace -- --ignored --nocapture`.
#[test]
#[ignore = "debugging aid, prints only"]
#[allow(clippy::needless_range_loop)]
fn debug_trace() {
    let tx = Tx {
        rate: Rate::B4800,
        level_dbm0: -43.0,
        long: false,
        carrier_offset_hz: 0.0,
        clock: 1.0,
    };
    let audio = tx.modulate(&prbs(100, 1));
    let mut rx = V27terRx::new(Rate::B4800);
    let _ = receive(&mut rx, &audio);
    for (i, t) in rx
        .trace
        .iter()
        .enumerate()
        .filter(|(_, t)| t.0 >= 1)
        .take(110)
    {
        eprintln!("{i:3} stage {} |z| {:.2} resid {:7.1}", t.0, t.1, t.2);
    }
    eprintln!("trace len {}", rx.trace.len());
    let sent = tx.changes(&prbs(100, 1));
    eprintln!("sent Seg5.. {:?}", &sent[72..100]);
    eprintln!("got        {:?}", &rx.changes[..rx.changes.len().min(28)]);
    // the failing case: seed 3, 9600 bits
    let data = prbs(9600, 3);
    let audio = tx.modulate(&data);
    let mut rx = V27terRx::new(Rate::B4800);
    let _ = receive(&mut rx, &audio);
    let sent = tx.changes(&data);
    let got = &rx.changes;
    let first = sent[72..].iter().zip(got.iter()).position(|(a, b)| a != b);
    eprintln!(
        "long: sent {} data symbols, got {}, first difference at {first:?}",
        sent.len() - 72,
        got.len()
    );
    if let Some(f) = first {
        eprintln!("sent {:?}", &sent[72 + f.saturating_sub(3)..72 + f + 10]);
        eprintln!("got  {:?}", &got[f.saturating_sub(3)..f + 10]);
    }
    // the front end alone: mix down and matched filter, Segment 3 region
    let taps = crate::dsp::rrc_taps(5.0, 0.5, 4.0);
    let mut mf = Vec::new();
    for n in 0..audio.len() {
        let mut m = C32::default();
        for (j, t) in taps.iter().enumerate() {
            if n >= j {
                let idx = n - j;
                let lo = C32::from_angle(-2.0 * PI * CARRIER_HZ * idx as f32 / 8000.0);
                m += lo.scale(audio[idx] as f32 * t);
            }
        }
        mf.push(m);
    }
    for n in (400 + 60)..(400 + 90) {
        eprintln!(
            "mf {n} re {:9.0} im {:9.0} |m| {:9.0}",
            mf[n].re,
            mf[n].im,
            mf[n].norm_sqr().sqrt()
        );
    }
}

#[test]
fn clean_signals_at_both_speeds_and_both_trainings() {
    for rate in [Rate::B4800, Rate::B2400] {
        for long in [false, true] {
            let (e, n) = errors(rate, -10.0, long, 0.0, 1.0, 11);
            assert_eq!(e, 0, "{rate:?} long={long}: {e} of {n}");
        }
    }
}

#[test]
fn quiet_offset_and_drifting_signals() {
    for rate in [Rate::B4800, Rate::B2400] {
        let (e, _) = errors(rate, -43.0, false, 0.0, 1.0, 3);
        assert_eq!(e, 0, "{rate:?} -43 dBm0");
        for off in [-7.0f32, 7.0] {
            let (e, _) = errors(rate, -15.0, false, off, 1.0, 5);
            assert_eq!(e, 0, "{rate:?} {off} Hz");
        }
        for clock in [1.0001f64, 0.9999] {
            let (e, _) = errors(rate, -15.0, false, 0.0, clock, 9);
            assert_eq!(e, 0, "{rate:?} clock {clock}");
        }
    }
}

#[test]
fn the_scrambler_feeds_back_the_line_bit_and_the_guard_inverts_after_33() {
    // Data that scrambles to a line sequence of period 8 for a long stretch: the guard (33
    // positions equal to the bit 8 before) fires, and the descrambler finds the data again.
    let mut tx = Scrambler::with_register(TRAIN_REGISTER);
    // choose the data so that the bit before the guard (y) is the pattern 10110010 repeated
    let pattern = [1u32, 0, 1, 1, 0, 0, 1, 0];
    let mut data = Vec::new();
    let mut line = Vec::new();
    let mut probe = Scrambler::with_register(TRAIN_REGISTER);
    for i in 0..300 {
        let y = pattern[i % 8];
        // data bit that gives y with the register of `probe`
        let bit = (y ^ ((probe.reg >> 5) & 1) ^ ((probe.reg >> 6) & 1)) != 0;
        data.push(bit);
        line.push(probe.scramble(bit));
    }
    let fired = line
        .iter()
        .enumerate()
        .filter(|(i, l)| (**l as u32) != pattern[i % 8])
        .count();
    assert!(
        fired >= 3,
        "the guard fired {fired} times in 300 bits of a period 8 pattern"
    );
    let scrambled: Vec<bool> = data.iter().map(|&b| tx.scramble(b)).collect();
    assert_eq!(scrambled, line);
    let mut rx = Scrambler::with_register(TRAIN_REGISTER);
    let back: Vec<bool> = line.iter().map(|&l| rx.descramble(l)).collect();
    assert_eq!(back, data);
}

/// On a line whose noise is above −48 dBm0 the end of the carrier is still heard: the
/// signal drops 10 dB below its own level (M5e, ADR-066).
#[test]
fn the_carrier_end_is_heard_in_noise_above_the_off_threshold() {
    for rate in [Rate::B4800, Rate::B2400] {
        for seed in 1..=3u64 {
            let tx = testtx::Tx {
                rate,
                level_dbm0: -14.0,
                long: false,
                carrier_offset_hz: 0.0,
                clock: 1.0,
            };
            let mut audio = tx.modulate(&prbs(4800, seed as u32));
            audio.extend(vec![0i16; 4000]);
            // noise everywhere at -34 dBm0: 20 dB below the signal, 14 dB above -48
            let n = noise(-34.0, audio.len(), seed + 40);
            for (a, b) in audio.iter_mut().zip(n) {
                *a = a.saturating_add(b);
            }
            let mut rx = V27terRx::new(rate);
            let (ev, _) = receive(&mut rx, &audio);
            assert!(
                ev.ends_with(&[RxEvent::CarrierLost]) && ev.contains(&RxEvent::Trained),
                "{rate:?} seed {seed}: {ev:?}"
            );
        }
    }
}

/// Noise at `level` dBm0 (fixed-seed Gaussian), `n` samples.
fn noise(level: f32, n: usize, seed: u64) -> Vec<i16> {
    let mut x = seed | 1;
    let mut uni = move || {
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        ((x >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    };
    let sd = f64::from(crate::level::mean_square(level)).sqrt();
    (0..n)
        .map(|_| {
            let g = (-2.0 * uni().ln()).sqrt() * (2.0 * std::f64::consts::PI * uni()).cos();
            (sd * g).round() as i16
        })
        .collect()
}

#[test]
fn a_false_training_in_noise_is_given_up_and_the_real_signal_is_received() {
    for rate in [Rate::B2400, Rate::B4800] {
        // a tone at 1800 Hz + half the symbol rate is a Segment 3 for the receiver; noise
        // follows it, then the real transmission (and noise all the time under it)
        let f = 1800.0 + rate.baud() / 2.0;
        let tone: Vec<i16> = (0..160)
            .map(|i| {
                (crate::level::sine_amplitude(-20.0) * (2.0 * PI * f * i as f32 / 8000.0).sin())
                    as i16
            })
            .collect();
        let tx = Tx {
            rate,
            level_dbm0: -20.0,
            long: true,
            carrier_offset_hz: 0.0,
            clock: 1.0,
        };
        let data = prbs(4000, 7);
        let mut audio = tone;
        audio.extend(noise(-30.0, 8000, 5));
        let start = audio.len();
        let sig = tx.modulate(&data);
        let n = noise(-45.0, sig.len(), 6);
        audio.extend(sig.iter().zip(&n).map(|(a, b)| a.saturating_add(*b)));
        audio.extend(noise(-45.0, 800, 7));
        let mut rx = V27terRx::new(rate);
        let (ev, bits) = receive(&mut rx, &audio);
        let trainings = ev.iter().filter(|e| **e == RxEvent::Training).count();
        assert!(
            trainings >= 2,
            "{rate:?}: the tone is a training too: {ev:?}"
        );
        let at = find(&bits, &data)
            .unwrap_or_else(|| panic!("{rate:?}: data not found (signal starts at {start})"));
        assert_eq!(&bits[at..at + data.len()], &data[..], "{rate:?}");
    }
}

#[test]
fn a_tone_out_of_the_band_of_the_matched_filter_is_not_a_training() {
    // DC after a step, and a low tone: the AGC would make them look like a signal
    for rate in [Rate::B2400, Rate::B4800] {
        let mut audio = vec![4570i16; 32];
        audio.extend(vec![-65i16; 4000]);
        let mut rx = V27terRx::new(rate);
        let (ev, bits) = receive(&mut rx, &audio);
        assert!(ev.is_empty() && bits.is_empty(), "{rate:?}: {ev:?}");
    }
}
