//! M5b tests V01-V03 (test plan `docs/test-plans/M5.md` section 5): our V.21 channel 2 modem
//! (`fax_modem::v21`) against SpanDSP's `fsk_tx` / `fsk_rx`, the independent counterpart.
//!
//! * V01, V02: SpanDSP sends the frames (`hdlc_tx` bits into `fsk_tx`), a channel with level,
//!   white noise, clock deviation and frequency offset follows, our `V21Rx` + `HdlcRx` receive.
//! * V03: our `V21Tx` sends, SpanDSP's `fsk_rx` + `hdlc_rx` receive; the tone frequencies and the
//!   preamble length of what we send are measured.
//!
//! Frames come in bursts of 5 behind a 1 s preamble (38 flags), 14 bursts per seed, three seeds:
//! 210 frames per condition. A fresh receiver per burst (the engine starts one at every signal
//! onset). Noise, data and bursts come from a seeded PRNG.

mod m5b_common;

use fax_in::audioengine::{AudioConfig, AudioEngine};
use fax_modem::hdlc::{HdlcEvent, HdlcRx, encode_burst};
use fax_modem::v21::{V21Rx, V21Tx};
use m5b_common::{
    Chan, Rng, spandsp_fsk_rx, spandsp_fsk_tx, spandsp_hdlc_bits, spandsp_hdlc_rx, t30_like_frame,
    through,
};

const SEEDS: [u64; 3] = [0x5B02_0001, 0x5B02_0002, 0x5B02_0003];
const BURSTS: usize = 14;
const PER_BURST: usize = 5;
const PREAMBLE: i32 = 38;

/// What the receivers made of all the bursts of a condition.
#[derive(Debug, Default)]
struct Tally {
    sent: usize,
    /// Sent frames that came out with a good FCS and the right data.
    right: usize,
    /// Frames reported with a bad FCS.
    bad_fcs: usize,
    /// Frames reported good that were never sent (an undetected error).
    false_good: usize,
}

impl Tally {
    fn error_rate(&self) -> f64 {
        (self.sent - self.right) as f64 / self.sent as f64
    }
    fn add(&mut self, sent: &[Vec<u8>], got: &[(Vec<u8>, bool)]) {
        self.sent += sent.len();
        let mut want: Vec<&Vec<u8>> = sent.iter().collect();
        for (d, ok) in got {
            if !ok {
                self.bad_fcs += 1;
            } else if let Some(i) = want.iter().position(|w| *w == d) {
                want.remove(i);
                self.right += 1;
            } else {
                self.false_good += 1;
            }
        }
    }
}

/// Our receiver on `audio`: frames with their verdicts.
fn ours_receive(audio: &[i16]) -> Vec<(Vec<u8>, bool)> {
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

/// SpanDSP sends bursts through `ch`, our receiver listens: 3 seeds x 14 bursts x 5 frames.
fn condition(ch: &Chan) -> Tally {
    let mut t = Tally::default();
    for seed in SEEDS {
        let mut rng =
            Rng::new(seed ^ (ch.level.to_bits() >> 20) ^ ch.snr_db.map_or(0, |s| s as u64));
        for burst in 0..BURSTS {
            let frames: Vec<Vec<u8>> = (0..PER_BURST)
                .map(|i| t30_like_frame(&mut rng, burst + i))
                .collect();
            let bits = spandsp_hdlc_bits(&frames, PREAMBLE, 2);
            let tx = spandsp_fsk_tx(&bits, -10.0);
            let heard = through(&tx, ch, &mut rng);
            t.add(&frames, &ours_receive(heard.gated()));
        }
    }
    t
}

fn report(id: &str, what: &str, t: &Tally) {
    eprintln!(
        "{id} {what}: {} frames, right {}, bad FCS {}, false good {}, error rate {:.3} %",
        t.sent,
        t.right,
        t.bad_fcs,
        t.false_good,
        100.0 * t.error_rate()
    );
}

// ------------------------------------------------------------------------------------- V01

#[test]
fn v01_levels_minus_10_to_minus_43_and_snr_15_to_30() {
    for level in [-10.0, -20.0, -30.0, -43.0] {
        for snr in [15.0, 20.0, 30.0] {
            let t = condition(&Chan::clean(level).snr(snr));
            report("V01", &format!("{level} dBm0, SNR {snr} dB"), &t);
            assert_eq!(t.sent, 210);
            assert_eq!(t.right, t.sent, "{level} dBm0, SNR {snr} dB: {t:?}");
            assert_eq!(t.bad_fcs, 0, "{level} dBm0, SNR {snr} dB: {t:?}");
            assert_eq!(t.false_good, 0, "{level} dBm0, SNR {snr} dB: {t:?}");
        }
    }
}

#[test]
fn v01_clean_signal_at_every_level() {
    for level in [-10.0, -20.0, -30.0, -43.0] {
        let t = condition(&Chan::clean(level));
        report("V01", &format!("{level} dBm0, no noise"), &t);
        assert_eq!((t.right, t.bad_fcs, t.false_good), (t.sent, 0, 0), "{t:?}");
    }
}

#[test]
fn v01_snr_12_db_has_at_most_one_percent_errors() {
    for level in [-10.0, -43.0] {
        let t = condition(&Chan::clean(level).snr(12.0));
        report("V01", &format!("{level} dBm0, SNR 12 dB"), &t);
        assert_eq!(t.false_good, 0, "an error got through the FCS: {t:?}");
        assert!(t.error_rate() <= 0.01, "{level} dBm0, SNR 12 dB: {t:?}");
    }
}

#[test]
fn v01_the_error_rate_curve_below_the_plan_is_recorded() {
    // not required by the plan: where the receiver gives out
    for snr in [6.0, 8.0, 10.0] {
        let t = condition(&Chan::clean(-20.0).snr(snr));
        report("V01 (record)", &format!("-20 dBm0, SNR {snr} dB"), &t);
        assert_eq!(t.false_good, 0, "an error got through the FCS: {t:?}");
    }
}

// ------------------------------------------------------------------------------------- V02

#[test]
fn v02_clock_deviation_and_frequency_offset() {
    let devs: [(f64, f64); 8] = [
        (100.0, 0.0),
        (-100.0, 0.0),
        (0.0, 10.0),
        (0.0, -10.0),
        (100.0, 10.0),
        (-100.0, -10.0),
        (100.0, -10.0),
        (-100.0, 10.0),
    ];
    for level in [-10.0, -30.0, -43.0] {
        for (ppm, hz) in devs {
            let t = condition(&Chan::clean(level).snr(20.0).ppm(ppm).offset(hz));
            report(
                "V02",
                &format!("{level} dBm0, SNR 20 dB, {ppm:+} ppm, {hz:+} Hz"),
                &t,
            );
            assert_eq!(t.sent, 210);
            assert_eq!(
                (t.right, t.bad_fcs, t.false_good),
                (t.sent, 0, 0),
                "{level} dBm0, {ppm} ppm, {hz} Hz: {t:?}"
            );
        }
    }
}

#[test]
fn v02_deviations_without_noise_and_with_a_lot_of_it() {
    for (ppm, hz) in [(100.0, 10.0), (-100.0, -10.0)] {
        let t = condition(&Chan::clean(-20.0).ppm(ppm).offset(hz));
        report(
            "V02",
            &format!("-20 dBm0, no noise, {ppm:+} ppm, {hz:+} Hz"),
            &t,
        );
        assert_eq!((t.right, t.bad_fcs, t.false_good), (t.sent, 0, 0), "{t:?}");
        let t = condition(&Chan::clean(-20.0).snr(15.0).ppm(ppm).offset(hz));
        report(
            "V02",
            &format!("-20 dBm0, SNR 15 dB, {ppm:+} ppm, {hz:+} Hz"),
            &t,
        );
        assert_eq!((t.right, t.bad_fcs, t.false_good), (t.sent, 0, 0), "{t:?}");
    }
}

#[test]
fn v02_the_limits_of_the_deviations_are_recorded() {
    // not required by the plan: how far past +-100 ppm / +-10 Hz it goes
    for (ppm, hz) in [
        (500.0, 0.0),
        (-500.0, 0.0),
        (0.0, 20.0),
        (0.0, -20.0),
        (0.0, 40.0),
    ] {
        let t = condition(&Chan::clean(-20.0).snr(20.0).ppm(ppm).offset(hz));
        report(
            "V02 (record)",
            &format!("-20 dBm0, SNR 20 dB, {ppm:+} ppm, {hz:+} Hz"),
            &t,
        );
        assert_eq!(t.false_good, 0, "{t:?}");
    }
}

// ------------------------------------------------------------------------------------- V03

fn ours_burst(frames: &[Vec<u8>], level: f32) -> Vec<i16> {
    let mut tx = V21Tx::new(level);
    let mut s = vec![0i16; 800];
    s.extend(tx.modulate(&encode_burst(frames, PREAMBLE as usize, 2)));
    s.extend(vec![0i16; 800]);
    s
}

#[test]
fn v03_spandsp_receives_our_frames() {
    for level in [-10.0f32, -20.0, -30.0, -40.0] {
        let mut t = Tally::default();
        for seed in SEEDS {
            let mut rng = Rng::new(seed ^ 0x33);
            for burst in 0..BURSTS {
                let frames: Vec<Vec<u8>> = (0..PER_BURST)
                    .map(|i| t30_like_frame(&mut rng, burst + i))
                    .collect();
                let audio = ours_burst(&frames, level);
                // SpanDSP: fsk_rx, then its own hdlc_rx
                let bits = spandsp_fsk_rx(&audio);
                t.add(&frames, &spandsp_hdlc_rx(&bits));
            }
        }
        report("V03", &format!("our V21Tx at {level} dBm0 into fsk_rx"), &t);
        assert_eq!(t.sent, 210);
        assert_eq!(
            (t.right, t.bad_fcs, t.false_good),
            (t.sent, 0, 0),
            "{level}: {t:?}"
        );
    }
}

#[test]
fn v03_spandsp_receives_our_frames_through_noise_and_deviations() {
    // not in the plan: the channel of V01/V02 after our sender, SpanDSP's receiver behind it,
    // next to SpanDSP's own sender into the same receiver (what SpanDSP does to itself)
    for ch in [
        Chan::clean(-15.0).snr(20.0),
        Chan::clean(-30.0).snr(20.0).ppm(100.0).offset(10.0),
        Chan::clean(-30.0).snr(20.0).ppm(-100.0).offset(-10.0),
    ] {
        let mut ours = Tally::default();
        let mut theirs = Tally::default();
        for seed in SEEDS {
            let mut rng = Rng::new(seed ^ 0x34);
            for burst in 0..BURSTS {
                let frames: Vec<Vec<u8>> = (0..PER_BURST)
                    .map(|i| t30_like_frame(&mut rng, burst + i))
                    .collect();
                let mut tx = V21Tx::new(-10.0);
                let samples = tx.modulate(&encode_burst(&frames, PREAMBLE as usize, 2));
                let heard = through(&samples, &ch, &mut Rng::new(seed ^ 0x35 ^ burst as u64));
                ours.add(&frames, &spandsp_hdlc_rx(&spandsp_fsk_rx(heard.gated())));
                let sp = spandsp_fsk_tx(&spandsp_hdlc_bits(&frames, PREAMBLE, 2), -10.0);
                let heard = through(&sp, &ch, &mut Rng::new(seed ^ 0x35 ^ burst as u64));
                theirs.add(&frames, &spandsp_hdlc_rx(&spandsp_fsk_rx(heard.gated())));
            }
        }
        report("V03 (extra) ours  -> fsk_rx", &format!("{ch:?}"), &ours);
        report("V03 (extra) SpanDSP -> fsk_rx", &format!("{ch:?}"), &theirs);
        assert_eq!((ours.false_good, theirs.false_good), (0, 0));
        // what SpanDSP does to its own signal is the yardstick: we are not worse by more than
        // 1 % of the frames
        assert!(
            ours.error_rate() <= theirs.error_rate() + 0.01,
            "{ch:?}: ours {ours:?}, SpanDSP {theirs:?}"
        );
    }
}

/// The frequency of a tone from its zero crossings (interpolated): (crossings - 1) / 2 half
/// periods over the time from the first to the last.
fn tone_hz(samples: &[i16]) -> f64 {
    let mut times = Vec::new();
    for i in 1..samples.len() {
        let (a, b) = (f64::from(samples[i - 1]), f64::from(samples[i]));
        if a < 0.0 && b >= 0.0 {
            // upward crossings only: one per period
            times.push((i - 1) as f64 + (-a) / (b - a));
        }
    }
    (times.len() - 1) as f64 / (times[times.len() - 1] - times[0]) * 8000.0
}

#[test]
fn v03_the_tones_are_1650_and_1850_hz() {
    for level in [-10.0f32, -30.0] {
        let mut tx = V21Tx::new(level);
        let mark = tone_hz(&tx.modulate(&vec![true; 3000]));
        let space = tone_hz(&tx.modulate(&vec![false; 3000]));
        eprintln!("V03 tones at {level} dBm0: mark {mark:.3} Hz, space {space:.3} Hz");
        assert!((mark - 1650.0).abs() <= 1.0, "mark {mark}");
        assert!((space - 1850.0).abs() <= 1.0, "space {space}");
    }
    // the same measurement on SpanDSP's own tones, to show the method has the resolution
    let sp_mark = tone_hz(&spandsp_fsk_tx(&vec![true; 3000], -10.0));
    let sp_space = tone_hz(&spandsp_fsk_tx(&vec![false; 3000], -10.0));
    eprintln!("V03 SpanDSP tones: mark {sp_mark:.3} Hz, space {sp_space:.3} Hz");
    assert!((sp_mark - 1650.0).abs() <= 1.0 && (sp_space - 1850.0).abs() <= 1.0);
    // and phase-continuous at the changes: no sample step larger than the signal allows
    let mut tx = V21Tx::new(-10.0);
    let s = tx.modulate(&[true, false, true, true, false, false, true, false]);
    let amp = f64::from(*s.iter().max().unwrap());
    let max_step = s
        .windows(2)
        .map(|w| (f64::from(w[1]) - f64::from(w[0])).abs())
        .fold(0.0, f64::max);
    // a 1850 Hz sine changes by at most 2 pi f / fs = 1.453 of its amplitude per sample
    assert!(
        max_step <= 1.46 * amp,
        "step {max_step} for amplitude {amp}"
    );
}

#[test]
fn v03_spandsp_hears_the_tones_as_ones_and_zeros() {
    // all-ones then all-zeros, as bits: SpanDSP's receiver reads what we meant
    let mut tx = V21Tx::new(-12.0);
    let mut bits = vec![true; 150];
    bits.extend(vec![false; 150]);
    bits.extend(vec![true; 150]);
    let mut s = vec![0i16; 800];
    s.extend(tx.modulate(&bits));
    s.extend(vec![0i16; 800]);
    let got = spandsp_fsk_rx(&s);
    let ones = got.iter().filter(|&&b| b).count();
    let zeros = got.len() - ones;
    eprintln!("V03 fsk_rx: {} bits, {ones} ones, {zeros} zeros", got.len());
    assert!(
        (130..=170).contains(&zeros),
        "{zeros} zeros of the 150 sent (inverted tones would give ~300 or ~0)"
    );
}

/// The audio the engine sends over 12 s: CED, the first DIS, and the DIS repeated after T4.
fn engine_audio() -> Vec<i16> {
    let mut engine = AudioEngine::new(AudioConfig::default());
    engine.start();
    let mut out = Vec::new();
    let silence = [0i16; 160];
    let mut tx = [0i16; 160];
    for _ in 0..(12 * 50) {
        engine.process(&silence, &mut tx);
        out.extend_from_slice(&tx);
    }
    out
}

/// Runs of signal in `audio` (blocks of 10 ms with a sample above 50; a gap of 30 ms or less
/// does not end a run): (first block, last block).
fn signal_runs(audio: &[i16]) -> Vec<(usize, usize)> {
    let mut runs: Vec<(usize, usize)> = Vec::new();
    for i in 0..audio.len() / 80 {
        if audio[i * 80..(i + 1) * 80].iter().any(|&s| s.abs() > 50) {
            match runs.last_mut() {
                Some(r) if i <= r.1 + 3 => r.1 = i,
                _ => runs.push((i, i)),
            }
        }
    }
    runs
}

#[test]
fn v03_our_preamble_is_one_second_plus_or_minus_15_percent() {
    // The DIS the engine repeats after T4 (3 s) stands alone in the audio (the first one follows
    // the CED without a gap, see the next test), and is the same burst as the first.
    let audio = engine_audio();
    let runs = signal_runs(&audio);
    eprintln!("V03 engine audio: runs of 10 ms blocks {runs:?}");
    assert!(
        runs.len() >= 2,
        "CED+DIS and a repeated DIS expected: {runs:?}"
    );
    let (b0, b1) = runs[1];
    let lo = (b0 * 80..(b0 + 1) * 80)
        .find(|&i| audio[i].abs() > 50)
        .unwrap();
    let hi = (b1 * 80..(b1 + 1) * 80)
        .rfind(|&i| audio[i].abs() > 50)
        .unwrap()
        + 1;
    let burst = &audio[lo.saturating_sub(80)..(hi + 80).min(audio.len())];
    // what is in it
    let mut rx = V21Rx::new();
    let mut h = HdlcRx::new();
    let mut frames = Vec::new();
    for b in rx.feed(burst) {
        if let Some(HdlcEvent::Frame { data, fcs_ok: true }) = h.bit(b) {
            frames.push(data);
        }
    }
    assert!(!frames.is_empty(), "no frame in the repeated DIS burst");
    let frame_bits = encode_burst(&frames, 1, 1).len() - 16;
    let total_bits = (hi - lo) as f64 * 300.0 / 8000.0;
    // the burst ends with two closing flags
    let preamble_bits = total_bits - frame_bits as f64 - 16.0;
    let preamble_s = preamble_bits / 300.0;
    eprintln!(
        "V03 engine burst: {} frames, {:.1} ms on air, preamble {:.3} s ({} flags)",
        frames.len(),
        (hi - lo) as f64 / 8.0,
        preamble_s,
        (preamble_bits / 8.0).round()
    );
    assert!(
        (0.85..=1.15).contains(&preamble_s),
        "preamble {preamble_s} s is not 1 s +- 15 %"
    );
}

/// The number of samples of silence between the end of the CED (2100 Hz) and the start of the
/// first V.21 burst, from the sample of the first block that is not 2100 Hz.
fn ced_to_dis_gap_samples(audio: &[i16]) -> (usize, usize) {
    use fax_modem::goertzel::Goertzel;
    // 5 ms blocks: the block power at 2100 Hz against all of it
    let mut gap = 0usize;
    let mut ced = 0usize;
    let mut seen_ced = false;
    for blk in audio.chunks_exact(40) {
        let mut g = Goertzel::new(2100.0);
        blk.iter().for_each(|&s| g.update(f32::from(s)));
        let at_2100 = g.take();
        let total = fax_modem::level::block_mean_square(blk);
        if total < 100.0 {
            if seen_ced {
                gap += 40;
            }
        } else if at_2100 > 0.5 * total {
            if seen_ced && gap > 0 {
                break;
            }
            seen_ced = true;
            ced += 40;
        } else if seen_ced {
            // not CED, not silence: the V.21 signal
            break;
        }
    }
    (gap, ced)
}

#[test]
fn v03_extra_ced_is_followed_by_75_ms_of_silence() {
    let audio = engine_audio();
    let (gap, ced) = ced_to_dis_gap_samples(&audio);
    assert!(ced >= 2_900 * 8, "CED of {} ms", ced / 8);
    eprintln!("V03 CED to DIS: {} ms of silence", gap / 8);
    assert!(
        (55 * 8..=95 * 8).contains(&gap),
        "{} ms of silence between CED and the first V.21 signal (T.30 5.3.2: 75 ms +- 20 ms)",
        gap / 8
    );
}
