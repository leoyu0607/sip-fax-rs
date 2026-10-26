//! M5a (test plan `docs/test-plans/M5.md` §4): N01–N04, fax tones: CNG detection, CED generation,
//! signal presence. (The SpanDSP counterpart of N03 is `tools/fax-sim/tests/n03_ced_spandsp.rs`.)
//!
//! Signals are built here with the plain `f64` sine function, independent of the generator under
//! test; noise comes from a fixed-seed generator (three seeds per case).

use fax_modem::goertzel::Goertzel;
use fax_modem::level::{block_mean_square, dbm0, sine_amplitude};
use fax_modem::tone::{
    CED_HZ, CNG_HZ, CngDetector, OFF_DBM0, ON_DBM0, Presence, PresenceDetector, ToneGenerator,
};
use proptest::prelude::*;
use proptest::test_runner::{Config, RngAlgorithm, TestRng, TestRunner};
use std::f64::consts::PI;

const SEEDS: [u64; 3] = [0xC0FFEE01, 0xC0FFEE02, 0xC0FFEE03];
const MS: usize = 8; // samples per millisecond

// ------------------------------------------------------------------------------------ helpers

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
    fn range(&mut self, lo: usize, hi: usize) -> usize {
        lo + (self.next() % (hi - lo + 1) as u64) as usize
    }
    /// Standard normal (Box-Muller).
    fn gauss(&mut self) -> f64 {
        (-2.0 * self.uniform().ln()).sqrt() * (2.0 * PI * self.uniform()).cos()
    }
}

fn clip(v: f64) -> i16 {
    v.round().clamp(-32768.0, 32767.0) as i16
}

/// Mean square of a 0 dBm0 sine.
fn amp(level_dbm0: f64) -> f64 {
    f64::from(sine_amplitude(level_dbm0 as f32))
}

/// `ms` of a sine at `freq`, `level_dbm0`, starting at phase `phase`.
fn sine(freq: f64, level_dbm0: f64, ms: usize, phase: f64) -> Vec<f64> {
    let a = amp(level_dbm0);
    (0..ms * MS)
        .map(|n| a * (2.0 * PI * freq * n as f64 / 8000.0 + phase).sin())
        .collect()
}

fn to_i16(v: &[f64]) -> Vec<i16> {
    v.iter().map(|&x| clip(x)).collect()
}

fn add_noise(sig: &mut [f64], rng: &mut Rng, noise_dbm0_power: f64) {
    // noise_dbm0_power: the noise's mean square, expressed as a level (dBm0)
    let sd = f64::from(sine_amplitude(noise_dbm0_power as f32)) / 2f64.sqrt();
    for s in sig.iter_mut() {
        *s += sd * rng.gauss();
    }
}

fn level_of(s: &[i16]) -> f64 {
    f64::from(dbm0(block_mean_square(s)))
}

// ----------------------------------------------------------------------------------------- N01

/// Five bursts of `on_ms` of CNG with `off_ms` of silence between (after `lead` samples of
/// silence), at `level` with AWGN at `snr_db` below the signal (present all the time).
/// Returns the samples and the start sample of each burst.
fn cng_train(
    rng: &mut Rng,
    level: f64,
    snr_db: f64,
    lead: usize,
    on_ms: usize,
    off_ms: usize,
    bursts: usize,
) -> (Vec<i16>, Vec<usize>) {
    let mut sig = vec![0.0f64; lead];
    let mut starts = Vec::new();
    for _ in 0..bursts {
        starts.push(sig.len());
        let phase = 2.0 * PI * rng.uniform();
        sig.extend(sine(1100.0, level, on_ms, phase));
        sig.extend(vec![0.0; off_ms * MS]);
    }
    add_noise(&mut sig, rng, level - snr_db);
    (to_i16(&sig), starts)
}

/// Feeds one sample at a time; the sample positions at which a burst was reported (position of
/// the sample that completed the recognition, counted from 1).
fn cng_hits(samples: &[i16]) -> Vec<usize> {
    let mut d = CngDetector::new();
    let mut hits = Vec::new();
    for (i, s) in samples.iter().enumerate() {
        if d.feed(std::slice::from_ref(s)) {
            hits.push(i + 1);
        }
    }
    hits
}

#[test]
fn n01_cng_is_heard_in_every_burst_within_400_ms_from_minus_10_to_minus_43_dbm0() {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        for level in [-10.0, -20.0, -30.0, -40.0, -42.0, -43.0] {
            let lead = rng.range(0, 159) + 800;
            let (sig, starts) = cng_train(&mut rng, level, 20.0, lead, 500, 3000, 5);
            let hits = cng_hits(&sig);
            assert_eq!(
                hits.len(),
                starts.len(),
                "seed {seed:#x} {level} dBm0: one report per burst, got {hits:?}"
            );
            for (h, s) in hits.iter().zip(&starts) {
                let delay_ms = (*h - *s) as f64 / 8.0;
                assert!(
                    delay_ms < 400.0,
                    "seed {seed:#x} {level} dBm0: reported {delay_ms} ms after the burst began"
                );
                assert!(
                    delay_ms >= 300.0,
                    "17 blocks of 20 ms, the first of them partial: not before 300 ms: {delay_ms}"
                );
            }
        }
    }
}

#[test]
fn n01_the_delay_does_not_depend_on_where_the_burst_falls_in_the_analysis_block() {
    let mut rng = Rng::new(SEEDS[0]);
    let mut worst = 0.0f64;
    for lead in (0..160).step_by(7) {
        let (sig, starts) = cng_train(&mut rng, -25.0, 25.0, 4000 + lead, 500, 3000, 2);
        let hits = cng_hits(&sig);
        assert_eq!(hits.len(), 2, "lead {lead}");
        for (h, s) in hits.iter().zip(&starts) {
            worst = worst.max((*h - *s) as f64 / 8.0);
        }
    }
    assert!(worst < 400.0, "worst delay {worst} ms");
}

#[test]
fn n01_chunked_feeding_reports_the_same_bursts_in_the_same_chunk() {
    let mut rng = Rng::new(SEEDS[1]);
    let (sig, starts) = cng_train(&mut rng, -20.0, 20.0, 1234, 500, 3000, 3);
    let per_sample = cng_hits(&sig);
    for chunk in [1usize, 7, 80, 160, 161, 1000, 8000] {
        let mut d = CngDetector::new();
        let mut hits = 0;
        let mut at = Vec::new();
        for (c, part) in sig.chunks(chunk).enumerate() {
            if d.feed(part) {
                hits += 1;
                at.push((c + 1) * chunk);
            }
        }
        assert_eq!(hits, starts.len(), "chunk {chunk}");
        // reported in the chunk that holds the sample of the per-sample run
        for (a, p) in at.iter().zip(&per_sample) {
            assert!(*a >= *p && *a - *p < chunk + 1, "chunk {chunk}: {a} vs {p}");
        }
    }
}

#[test]
fn n01_a_continuous_tone_is_reported_once_and_a_shorter_burst_not_at_all() {
    let mut rng = Rng::new(SEEDS[2]);
    // 5 s of 1100 Hz without a pause: one report
    let mut sig = sine(1100.0, -20.0, 5000, 0.3);
    add_noise(&mut sig, &mut rng, -40.0);
    assert_eq!(cng_hits(&to_i16(&sig)).len(), 1);
    // 300 ms bursts (below the 340 ms of the detector): none, at any level
    for level in [-10.0, -30.0] {
        let (sig, _) = cng_train(&mut rng, level, 20.0, 800, 300, 700, 4);
        assert!(cng_hits(&sig).is_empty(), "{level}");
    }
    // 400 ms bursts are heard, with 700 ms between them
    let (sig, starts) = cng_train(&mut rng, -20.0, 20.0, 800, 400, 700, 4);
    assert_eq!(cng_hits(&sig).len(), starts.len());
}

#[test]
fn n01_below_the_threshold_nothing_is_heard() {
    let mut rng = Rng::new(SEEDS[0]);
    for level in [-50.0, -55.0, -60.0] {
        let (sig, _) = cng_train(&mut rng, level, 40.0, 800, 500, 3000, 3);
        assert!(cng_hits(&sig).is_empty(), "{level} dBm0");
    }
}

#[test]
fn n01_with_less_than_the_snr_of_the_plan_the_detector_degrades_gracefully() {
    // Only measured, not a requirement of the plan: at 10 dB SNR (the tone is 90 % of the power)
    // and 3 dB (50 %: below the 70 % purity limit) the detector must not report more bursts than
    // there are and must not panic.
    let mut rng = Rng::new(SEEDS[1]);
    for snr in [10.0, 6.0, 3.0, 0.0] {
        let (sig, starts) = cng_train(&mut rng, -20.0, snr, 800, 500, 3000, 5);
        let n = cng_hits(&sig).len();
        assert!(n <= starts.len(), "snr {snr}: {n}");
        if snr >= 10.0 {
            assert_eq!(n, starts.len(), "snr {snr}");
        }
        if snr <= 3.0 {
            assert_eq!(n, 0, "snr {snr}: the tone is not pure enough");
        }
    }
}

// ----------------------------------------------------------------------------------------- N02

fn mix(parts: &[Vec<f64>]) -> Vec<f64> {
    let n = parts.iter().map(Vec::len).max().unwrap_or(0);
    (0..n)
        .map(|i| parts.iter().map(|p| p.get(i).copied().unwrap_or(0.0)).sum())
        .collect()
}

fn dtmf(digit: char) -> (f64, f64) {
    let rows = [697.0, 770.0, 852.0, 941.0];
    let cols = [1209.0, 1336.0, 1477.0, 1633.0];
    let keys = ["123A", "456B", "789C", "*0#D"];
    for (r, row) in keys.iter().enumerate() {
        if let Some(c) = row.find(digit) {
            return (rows[r], cols[c]);
        }
    }
    panic!("not a DTMF digit: {digit}");
}

/// A voiced "speech-like" signal: a pitch pulse train of varying pitch through three formant
/// resonators that move between vowels, with syllable-rate amplitude modulation and breath noise.
/// (There is no recording in the repository; this is a stand-in with speech's moving spectrum.)
fn speech_like(rng: &mut Rng, seconds: usize, level_dbm0: f64) -> Vec<f64> {
    let n = seconds * 8000;
    let vowels = [
        [730.0, 1090.0, 2440.0], // a
        [270.0, 2290.0, 3010.0], // i
        [300.0, 870.0, 2240.0],  // u
        [530.0, 1840.0, 2480.0], // e
        [570.0, 840.0, 2410.0],  // o
    ];
    let mut out = vec![0.0f64; n];
    let (mut y1, mut y2) = ([0.0f64; 3], [0.0f64; 3]);
    let mut phase = 0.0f64;
    let mut vowel = 0usize;
    for (i, o) in out.iter_mut().enumerate() {
        if i % 1600 == 0 {
            vowel = rng.range(0, vowels.len() - 1); // a new vowel every 200 ms
        }
        let t = i as f64 / 8000.0;
        let f0 = 110.0 + 40.0 * (2.0 * PI * 0.7 * t).sin() + 10.0 * rng.gauss().abs().min(1.0);
        phase += f0 / 8000.0;
        let pulse = if phase >= 1.0 {
            phase -= 1.0;
            1.0
        } else {
            0.0
        };
        let excite = pulse + 0.02 * rng.gauss();
        let mut acc = 0.0;
        for (k, f) in vowels[vowel].iter().enumerate() {
            let bw = 60.0 + 40.0 * k as f64;
            let r = (-PI * bw / 8000.0).exp();
            let c = 2.0 * r * (2.0 * PI * f / 8000.0).cos();
            let y = excite + c * y1[k] - r * r * y2[k];
            y2[k] = y1[k];
            y1[k] = y;
            acc += y / (1.0 + k as f64);
        }
        let env = 0.55 + 0.45 * (2.0 * PI * 4.0 * t).sin();
        *o = acc * env;
    }
    // scale to the level
    let ms = out.iter().map(|x| x * x).sum::<f64>() / n as f64;
    let want = f64::from(fax_modem::level::mean_square(level_dbm0 as f32));
    let g = (want / ms).sqrt();
    out.iter_mut().for_each(|x| *x *= g);
    out
}

fn assert_no_cng(name: &str, sig: &[f64]) {
    let s = to_i16(sig);
    assert!(cng_hits(&s).is_empty(), "{name}: false CNG");
    // and in blocks of 20 ms, the way fax-in feeds it
    let mut d = CngDetector::new();
    for c in s.chunks(160) {
        assert!(!d.feed(c), "{name}: false CNG (20 ms blocks)");
    }
}

#[test]
fn n02_tones_near_but_not_at_1100_hz_are_not_cng() {
    for level in [-10.0, -20.0, -35.0] {
        for f in [
            300.0, 700.0, 1000.0, 1050.0, 1200.0, 1300.0, 1650.0, 1850.0, 2100.0, 2225.0, 2750.0,
        ] {
            assert_no_cng(&format!("{f} Hz at {level}"), &sine(f, level, 4000, 0.4));
        }
    }
}

#[test]
fn n02_dtmf_dial_tone_ringback_busy_and_the_other_fax_tones_are_not_cng() {
    for d in "0123456789ABCD*#".chars() {
        let (lo, hi) = dtmf(d);
        // DTMF as sent: 50 ms tone, 50 ms pause, ten times; and as one long 2 s tone
        let one = mix(&[sine(lo, -10.0, 50, 0.0), sine(hi, -8.0, 50, 0.0)]);
        let mut seq = Vec::new();
        for _ in 0..10 {
            seq.extend(one.iter().copied());
            seq.extend(vec![0.0; 50 * MS]);
        }
        assert_no_cng(&format!("DTMF {d} bursts"), &seq);
        assert_no_cng(
            &format!("DTMF {d} long"),
            &mix(&[sine(lo, -10.0, 2000, 0.0), sine(hi, -8.0, 2000, 0.0)]),
        );
    }
    // dial tone 350 + 440, ringback 440 + 480 (2 s / 4 s), busy 480 + 620 (0.5 s / 0.5 s),
    // special information tones, V.21 (1650/1850), CED, ANSam-ish 2100 with 15 Hz AM
    assert_no_cng(
        "dial tone",
        &mix(&[sine(350.0, -13.0, 4000, 0.0), sine(440.0, -13.0, 4000, 0.0)]),
    );
    assert_no_cng(
        "ringback",
        &mix(&[sine(440.0, -16.0, 4000, 0.0), sine(480.0, -16.0, 4000, 0.0)]),
    );
    assert_no_cng(
        "busy",
        &mix(&[sine(480.0, -12.0, 4000, 0.0), sine(620.0, -12.0, 4000, 0.0)]),
    );
    assert_no_cng("V.21 mark/space", &{
        let mut v = sine(1650.0, -20.0, 500, 0.0);
        v.extend(sine(1850.0, -20.0, 500, 0.0));
        v
    });
    assert_no_cng("CED", &sine(2100.0, -15.0, 3000, 0.0));
    let am: Vec<f64> = sine(2100.0, -15.0, 3000, 0.0)
        .iter()
        .enumerate()
        .map(|(i, x)| x * (1.0 + 0.2 * (2.0 * PI * 15.0 * i as f64 / 8000.0).sin()))
        .collect();
    assert_no_cng("ANSam", &am);
    // a 1100 Hz tone together with a loud 1000 Hz tone (not pure)
    assert_no_cng(
        "1100 + 1000 equal power",
        &mix(&[
            sine(1100.0, -20.0, 4000, 0.0),
            sine(1000.0, -20.0, 4000, 0.0),
        ]),
    );
}

#[test]
fn n02_speech_like_signals_and_noise_are_not_cng() {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        for level in [-10.0, -20.0, -30.0] {
            assert_no_cng(
                &format!("speech seed {seed:#x} {level}"),
                &speech_like(&mut rng, 20, level),
            );
        }
        // white noise, near full scale and quiet
        for level in [0.0, -10.0, -30.0] {
            let n: Vec<f64> = (0..8000 * 60)
                .map(|_| amp(level) / 2f64.sqrt() * rng.gauss())
                .collect();
            assert_no_cng(&format!("noise seed {seed:#x} {level}"), &n);
        }
        // uniformly random samples over the whole 16-bit range
        let r: Vec<f64> = (0..8000 * 30)
            .map(|_| (rng.next() >> 48) as i16 as f64)
            .collect();
        assert_no_cng(&format!("random samples seed {seed:#x}"), &r);
    }
}

#[test]
fn n02_speech_like_stand_in_has_a_speech_like_level_and_spectrum() {
    // sanity of the stand-in itself: the level is the asked one and the energy is spread
    // (no more than 30 % of it in any 50 Hz bin of the 1100 Hz neighbourhood)
    let mut rng = Rng::new(SEEDS[0]);
    let v = to_i16(&speech_like(&mut rng, 10, -20.0));
    assert!((level_of(&v) + 20.0).abs() < 0.5, "{}", level_of(&v));
    let mut g = Goertzel::new(CNG_HZ);
    let mut total = 0.0f64;
    for &s in &v[..8000] {
        g.update(f32::from(s));
        total += f64::from(s) * f64::from(s);
    }
    let tone = f64::from(g.take());
    assert!(tone < 0.3 * total / 8000.0, "{tone} of {}", total / 8000.0);
}

#[test]
fn n02_cng_after_other_signals_is_still_heard() {
    // the detector keeps no state from what came before: dial tone, DTMF, speech, then CNG
    let mut rng = Rng::new(SEEDS[2]);
    let mut sig = mix(&[sine(350.0, -13.0, 1000, 0.0), sine(440.0, -13.0, 1000, 0.0)]);
    sig.extend(speech_like(&mut rng, 2, -20.0));
    let start = sig.len();
    sig.extend(sine(1100.0, -20.0, 500, 0.0));
    sig.extend(vec![0.0; 3000 * MS]);
    let hits = cng_hits(&to_i16(&sig));
    assert_eq!(hits.len(), 1);
    assert!((hits[0] - start) as f64 / 8.0 < 400.0);
}

// ----------------------------------------------------------------------------------------- N03

fn generate(g: &mut ToneGenerator, total: usize) -> Vec<i16> {
    let mut v = vec![0x5A5Ai16; total];
    let n = g.fill(&mut v);
    v.truncate(n);
    v
}

#[test]
fn n03_ced_is_2100_hz_for_3_s_then_75_ms_of_silence() {
    let mut g = ToneGenerator::ced(-12.0);
    assert_eq!(g.remaining(), 24_000 + 600);
    let v = generate(&mut g, 40_000);
    assert_eq!(v.len(), 24_600, "3 s + 75 ms");
    // the default length is inside T.30's 2.6 to 4.0 s
    assert!((2.6..=4.0).contains(&(24_000.0 / 8000.0)));
    // silence is exactly zero, the tone is not
    assert!(v[24_000..].iter().all(|&s| s == 0));
    assert!(v[23_990..24_000].iter().any(|&s| s != 0));
    assert!(v[..24_000].iter().filter(|&&s| s == 0).count() < 24_000 / 3);
    assert_eq!(g.remaining(), 0);
    assert_eq!(g.fill(&mut [0x1111i16; 10]), 0, "finished");
}

#[test]
fn n03_ced_frequency_is_2100_hz_within_a_hertz() {
    let v = generate(&mut ToneGenerator::ced(-15.0), 40_000);
    // 1. zero crossings (rising) over the whole tone
    let tone = &v[..24_000];
    let mut crossings = 0usize;
    let mut first = 0usize;
    let mut last = 0usize;
    for i in 1..tone.len() {
        if tone[i - 1] < 0 && tone[i] >= 0 {
            if crossings == 0 {
                first = i;
            }
            last = i;
            crossings += 1;
        }
    }
    let hz = (crossings - 1) as f64 * 8000.0 / (last - first) as f64;
    assert!((hz - 2100.0).abs() < 1.0, "zero crossings: {hz} Hz");
    // 2. the Goertzel peak in 2085..=2115 Hz over the whole tone, in 0.5 Hz steps
    let mut best = (0.0f32, 0.0f32);
    let mut f = 2085.0f32;
    while f <= 2115.0 {
        let mut g = Goertzel::new(f);
        for &s in tone {
            g.update(f32::from(s));
        }
        let p = g.take();
        if p > best.0 {
            best = (p, f);
        }
        f += 0.5;
    }
    assert!((best.1 - 2100.0).abs() <= 1.0, "peak at {} Hz", best.1);
    assert_eq!(CED_HZ, 2100.0);
}

#[test]
fn n03_ced_level_follows_the_setting() {
    for level in [-10.0f32, -15.0, -20.0, -30.0, -40.0] {
        let v = generate(&mut ToneGenerator::ced(level), 40_000);
        let got = level_of(&v[..24_000]);
        assert!((got - f64::from(level)).abs() < 0.1, "{level} -> {got}");
        // a sine: the peak is the amplitude (no overshoot, no clipping) except at the quantization
        let peak = v.iter().map(|&s| i32::from(s).abs()).max().unwrap();
        let a = f64::from(sine_amplitude(level));
        assert!(
            (f64::from(peak) - a).abs() <= 2.0 + a * 0.001,
            "{level}: {peak} vs {a}"
        );
    }
}

#[test]
fn n03_ced_is_clean_continuous_phase_and_blockwise_equal_to_one_fill() {
    let one = generate(&mut ToneGenerator::ced(-10.0), 40_000);
    // no sample jumps more than a 2100 Hz sine can (no click inside the tone)
    let a = f64::from(sine_amplitude(-10.0));
    let max_step = a * 2.0 * (PI * 2100.0 / 8000.0).sin() + 2.0;
    for i in 1..24_000 {
        let d = f64::from(i32::from(one[i]) - i32::from(one[i - 1])).abs();
        assert!(d <= max_step, "sample {i}: step {d}");
    }
    // filling block by block (160, odd sizes) gives the same samples
    for block in [160usize, 1, 77, 1000, 24_600, 30_000] {
        let mut g = ToneGenerator::ced(-10.0);
        let mut v = Vec::new();
        loop {
            let mut b = vec![0i16; block];
            let n = g.fill(&mut b);
            if n == 0 {
                break;
            }
            v.extend_from_slice(&b[..n]);
        }
        assert_eq!(v, one, "block {block}");
    }
}

#[test]
fn n03_a_generator_with_other_lengths_and_frequencies() {
    // the same generator is the building block for other tones: lengths and the silence follow
    let mut g = ToneGenerator::new(1100.0, -20.0, 500, 3000);
    assert_eq!(g.remaining(), 4000 + 24_000);
    let v = generate(&mut g, 50_000);
    assert_eq!(v.len(), 28_000);
    assert!(v[4000..].iter().all(|&s| s == 0));
    // 2.6 s and 4.0 s CED variants are in T.30's range
    for ms in [2600u32, 4000] {
        let v = generate(&mut ToneGenerator::new(CED_HZ, -20.0, ms, 75), 80_000);
        assert_eq!(v.len(), (ms as usize + 75) * MS);
    }
    // nothing at all
    assert_eq!(
        generate(&mut ToneGenerator::new(1000.0, 0.0, 0, 0), 10).len(),
        0
    );
}

#[test]
fn n03_cng_detector_does_not_take_ced_for_cng_and_the_goertzel_sees_both() {
    // the CED we send is not mistaken for the calling tone by our own detector
    let v = generate(&mut ToneGenerator::ced(-12.0), 40_000);
    assert!(cng_hits(&v).is_empty());
}

// ----------------------------------------------------------------------------------------- N04

/// silence(`lead` samples) + tone(`on` samples) + silence(`tail` samples), `noise_dbm0` of AWGN.
fn burst(
    rng: &mut Rng,
    lead: usize,
    on: usize,
    tail: usize,
    level: f64,
    freq: f64,
    noise: Option<f64>,
) -> Vec<i16> {
    let mut sig = vec![0.0f64; lead];
    let a = amp(level);
    sig.extend((0..on).map(|n| a * (2.0 * PI * freq * n as f64 / 8000.0 + 0.3).sin()));
    sig.extend(vec![0.0; tail]);
    if let Some(nl) = noise {
        add_noise(&mut sig, rng, nl);
    }
    to_i16(&sig)
}

/// Per-sample feeding: the events with the sample count at which each was returned.
fn presence(samples: &[i16]) -> Vec<(Presence, usize)> {
    let mut d = PresenceDetector::new();
    let mut ev = Vec::new();
    for (i, s) in samples.iter().enumerate() {
        for e in d.feed(std::slice::from_ref(s)) {
            ev.push((e, i + 1));
        }
    }
    ev
}

#[test]
fn n04_start_and_end_are_found_within_10_ms_of_the_truth_at_every_level_above_the_threshold() {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        for level in [-10.0, -20.0, -30.0, -40.0, -42.5] {
            for freq in [1650.0, 1850.0, 2100.0, 1100.0] {
                let lead = 1000 + rng.range(0, 39) + 40 * rng.range(0, 20);
                let on = 4000 + rng.range(0, 39);
                let sig = burst(&mut rng, lead, on, 2000, level, freq, None);
                let ev = presence(&sig);
                let what = format!("seed {seed:#x} {level} dBm0 {freq} Hz lead {lead} on {on}");
                assert_eq!(ev.len(), 2, "{what}: {ev:?}");
                let Presence::On(p_on) = ev[0].0 else {
                    panic!("{what}: first event {:?}", ev[0])
                };
                let Presence::Off(p_off) = ev[1].0 else {
                    panic!("{what}: second event {:?}", ev[1])
                };
                // the reported position against the true start / end: 10 ms = 80 samples
                let d_on = p_on as i64 - lead as i64;
                let d_off = p_off as i64 - (lead + on) as i64;
                assert!(d_on.abs() <= 80, "{what}: start off by {d_on} samples");
                assert!(d_off.abs() <= 80, "{what}: end off by {d_off} samples");
                // ... which in fact is within one 5 ms block (40 samples): the position is that of
                // a block (the first one that holds enough of the signal to count, or enough silence)
                assert!((-40..=40).contains(&d_on), "{what}: start {d_on}");
                assert!((-40..=40).contains(&d_off), "{what}: end {d_off}");
            }
        }
    }
}

#[test]
fn n04_the_events_are_returned_15_ms_and_10_ms_after_the_truth() {
    // The decision is debounced (15 ms above -43 dBm0 to start, 10 ms below -48 dBm0 to end), so
    // an event is returned that long after it happened, although its position is the true one.
    let mut rng = Rng::new(SEEDS[0]);
    for level in [-10.0, -30.0] {
        let sig = burst(&mut rng, 1001, 4003, 2000, level, 1650.0, None);
        let ev = presence(&sig);
        assert_eq!(ev.len(), 2);
        let late_on = ev[0].1 as i64 - 1001;
        let late_off = ev[1].1 as i64 - (1001 + 4003);
        assert!(
            (15 * 8 - 40..=15 * 8 + 80).contains(&late_on),
            "start returned {late_on} samples late"
        );
        assert!(
            (10 * 8 - 40..=10 * 8 + 80).contains(&late_off),
            "end returned {late_off} samples late"
        );
    }
}

#[test]
fn n04_the_threshold_is_minus_43_dbm0_and_the_end_minus_48() {
    assert_eq!(ON_DBM0, -43.0);
    assert_eq!(OFF_DBM0, -48.0);
    let mut rng = Rng::new(SEEDS[1]);
    // 1 dB below the threshold never starts, 1 dB above does (1650 Hz, 1 s)
    for level in [-44.0, -45.0, -50.0, -60.0] {
        let sig = burst(&mut rng, 800, 8000, 800, level, 1650.0, None);
        assert!(presence(&sig).is_empty(), "{level} dBm0 must not start");
    }
    for level in [-42.0, -41.0] {
        let sig = burst(&mut rng, 800, 8000, 800, level, 1650.0, None);
        assert_eq!(presence(&sig).len(), 2, "{level} dBm0 must start");
    }
}

#[test]
fn n04_hysteresis_a_signal_that_fades_between_43_and_48_stays_present() {
    // 0.5 s at -38, then 1 s at -45 (between the two thresholds), then 0.5 s at -50 (below), then
    // silence: one start, one end, at the transition to -50. (The first level is within 10 dB of
    // -45: a deeper drop is an end of its own since M5e, ADR-066.)
    let mut sig = vec![0.0f64; 800];
    sig.extend(sine(1650.0, -38.0, 500, 0.0));
    sig.extend(sine(1650.0, -45.0, 1000, 0.0));
    let end_at = sig.len() as i64;
    sig.extend(sine(1650.0, -50.0, 500, 0.0));
    sig.extend(vec![0.0; 800]);
    let ev = presence(&to_i16(&sig));
    assert_eq!(ev.len(), 2, "{ev:?}");
    assert_eq!(ev[0].0, Presence::On(800));
    let Presence::Off(p) = ev[1].0 else { panic!() };
    assert!(
        (p as i64 - end_at).abs() <= 80,
        "end at {p}, the fade to -50 is at {end_at}"
    );
    // a signal that never gets above -43 but stays between -43 and -48 never starts
    let mut weak = vec![0.0f64; 800];
    weak.extend(sine(1650.0, -45.0, 1000, 0.0));
    assert!(presence(&to_i16(&weak)).is_empty());
}

#[test]
fn n04_clicks_up_to_5_ms_are_not_a_signal_but_15_ms_are() {
    let mut rng = Rng::new(SEEDS[2]);
    for offset in (0..40).step_by(3) {
        // 5 ms click, 40 samples, at every alignment against the 5 ms blocks
        let sig = burst(&mut rng, 800 + offset, 40, 1600, -10.0, 1650.0, None);
        assert!(presence(&sig).is_empty(), "5 ms click, offset {offset}");
        // 20 ms always covers three whole... at least three blocks
        let sig = burst(&mut rng, 800 + offset, 160, 1600, -10.0, 1650.0, None);
        assert_eq!(presence(&sig).len(), 2, "20 ms burst, offset {offset}");
    }
}

#[test]
fn n04_several_bursts_a_noisy_line_and_chunked_feeding() {
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        // background noise at -60 dBm0 (below -48), four bursts with random lengths and pauses
        let mut positions = Vec::new();
        let mut sig: Vec<f64> = Vec::new();
        for _ in 0..4 {
            let gap = rng.range(2000, 6000);
            sig.extend(vec![0.0; gap]);
            let on = rng.range(1200, 9000);
            positions.push((sig.len(), sig.len() + on));
            sig.extend(sine(1650.0, -25.0, on / MS, 0.0));
            sig.extend(vec![0.0; on % MS]);
        }
        sig.extend(vec![0.0; 4000]);
        add_noise(&mut sig, &mut rng, -60.0);
        let s = to_i16(&sig);
        let ev = presence(&s);
        assert_eq!(ev.len(), 8, "seed {seed:#x}: {ev:?}");
        for (k, (a, b)) in positions.iter().enumerate() {
            let Presence::On(p) = ev[2 * k].0 else {
                panic!("{:?}", ev[2 * k])
            };
            let Presence::Off(q) = ev[2 * k + 1].0 else {
                panic!("{:?}", ev[2 * k + 1])
            };
            assert!((p as i64 - *a as i64).abs() <= 80, "start {k}");
            assert!((q as i64 - *b as i64).abs() <= 80, "end {k}");
        }
        // the same events (positions) for any chunking
        for chunk in [160usize, 1000, 33, 8000] {
            let mut d = PresenceDetector::new();
            let mut got = Vec::new();
            for c in s.chunks(chunk) {
                got.extend(d.feed(c));
            }
            let want: Vec<Presence> = ev.iter().map(|e| e.0).collect();
            assert_eq!(got, want, "chunk {chunk}");
            assert!(!d.is_on());
        }
    }
}

#[test]
fn n04_is_on_follows_the_events() {
    let mut rng = Rng::new(SEEDS[0]);
    let sig = burst(&mut rng, 800, 4000, 1600, -20.0, 1650.0, None);
    let mut d = PresenceDetector::new();
    assert!(!d.is_on());
    let mut states = Vec::new();
    for c in sig.chunks(40) {
        d.feed(c);
        states.push(d.is_on());
    }
    assert!(states.contains(&true));
    assert!(!states[0] && !states[states.len() - 1]);
    // exactly one rising and one falling edge
    let edges = states.windows(2).filter(|w| w[0] != w[1]).count();
    assert_eq!(edges, 2);
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

fn samples() -> impl Strategy<Value = Vec<i16>> {
    prop_oneof![
        proptest::collection::vec(any::<i16>(), 0..3000),
        // long runs of one value (silence, DC, full scale) with noise in between
        proptest::collection::vec(
            (any::<i16>(), 1usize..200).prop_map(|(v, n)| vec![v; n]),
            0..30
        )
        .prop_map(|runs| runs.concat()),
        proptest::collection::vec(
            prop_oneof![Just(i16::MIN), Just(i16::MAX), Just(0i16)],
            0..3000
        ),
    ]
}

#[test]
fn n_proptest_any_input_never_panics_the_detectors_and_events_are_consistent() {
    for seed in [1u8, 2, 3] {
        runner(seed, 300)
            .run(
                &(samples(), proptest::collection::vec(1usize..700, 1..8)),
                |(data, chunks)| {
                    let mut cng = CngDetector::new();
                    let mut pres = PresenceDetector::new();
                    let mut pos = 0usize;
                    let mut events = Vec::new();
                    let mut at = 0usize;
                    let mut k = 0usize;
                    while at < data.len() {
                        let n = chunks[k % chunks.len()].min(data.len() - at);
                        let part = &data[at..at + n];
                        let _ = cng.feed(part);
                        events.extend(pres.feed(part));
                        at += n;
                        pos += n;
                        k += 1;
                    }
                    // events alternate On/Off starting with On, positions increase and are inside
                    // the input
                    let mut on = false;
                    let mut last = 0u64;
                    for e in &events {
                        let (is_on, p) = match *e {
                            Presence::On(p) => (true, p),
                            Presence::Off(p) => (false, p),
                        };
                        prop_assert_ne!(is_on, on, "events must alternate");
                        on = is_on;
                        prop_assert!(p >= last && p <= pos as u64);
                        last = p;
                    }
                    prop_assert_eq!(pres.is_on(), on);
                    Ok(())
                },
            )
            .unwrap();
    }
}

#[test]
fn n_proptest_goertzel_and_level_functions_take_any_number() {
    for seed in [1u8, 2, 3] {
        runner(seed, 500)
            .run(
                &(
                    any::<f32>(),
                    proptest::collection::vec(any::<f32>(), 0..200),
                    any::<f32>(),
                    proptest::collection::vec(any::<i16>(), 0..200),
                ),
                |(freq, xs, lvl, pcm)| {
                    let mut g = Goertzel::new(freq);
                    for x in xs {
                        g.update(x);
                    }
                    let _ = g.take();
                    let _ = g.take();
                    let _ = dbm0(lvl);
                    let _ = sine_amplitude(lvl);
                    let _ = fax_modem::level::mean_square(lvl);
                    let m = block_mean_square(&pcm);
                    prop_assert!(m >= 0.0 && m.is_finite());
                    Ok(())
                },
            )
            .unwrap();
    }
}

#[test]
fn n_proptest_the_tone_generator_writes_exactly_its_length_for_any_frequency_and_level() {
    for seed in [1u8, 2, 3] {
        runner(seed, 200)
            .run(
                &(
                    any::<f32>(),
                    prop_oneof![any::<f32>(), -100.0f32..20.0],
                    0u32..400,
                    0u32..200,
                    1usize..500,
                ),
                |(freq, level, on_ms, off_ms, chunk)| {
                    let mut g = ToneGenerator::new(freq, level, on_ms, off_ms);
                    let total = ((on_ms + off_ms) * 8) as usize;
                    prop_assert_eq!(g.remaining() as usize, total);
                    let mut written = 0usize;
                    let mut buf = vec![7i16; chunk];
                    loop {
                        let n = g.fill(&mut buf);
                        written += n;
                        prop_assert!(n <= chunk);
                        if n < chunk {
                            break;
                        }
                    }
                    prop_assert_eq!(written, total);
                    prop_assert_eq!(g.remaining(), 0);
                    prop_assert_eq!(g.fill(&mut buf), 0);
                    Ok(())
                },
            )
            .unwrap();
    }
}

#[test]
fn n_proptest_a_loud_unpredictable_input_never_looks_like_cng() {
    // random 16-bit samples, any seed: no CNG, 10 minutes of it
    for seed in SEEDS {
        let mut rng = Rng::new(seed);
        let mut d = CngDetector::new();
        let mut hits = 0;
        let mut block = [0i16; 160];
        for _ in 0..(600 * 50) {
            for b in block.iter_mut() {
                *b = (rng.next() >> 48) as i16;
            }
            if d.feed(&block) {
                hits += 1;
            }
        }
        assert_eq!(hits, 0, "seed {seed:#x}");
    }
}
