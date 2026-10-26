//! M5b test V04 (test plan `docs/test-plans/M5.md` section 5): the preamble of a V.21 burst
//! reaches T.30 as `RxSignal::V21Preamble`, the end of the signal as `RxSignal::NoSignal`, and
//! the times are within 30 ms of the real ones. Measured at the `AudioEngine` (through
//! `drain_signals`, the events given to T.30 with the engine's clock). Also locks the response
//! timing of ADR-069: our answer starts 75 ms +- 20 ms after the end of the peer's signal
//! (T.30 5.3.2).
//!
//! References: the preamble is "heard" when the third flag has been sent (the receiver reports
//! it after three flags: 24 bits = 80 ms after the onset); the end is the last sample of the
//! signal. The engine is fed in 20 ms steps (one RTP packet) and the event time is the end of
//! the step that showed it, so the quantisation of the steps is in the numbers; the seeds shift
//! the step phase. The SpanDSP `fsk_tx` source is in `tools/fax-sim/tests/v04_spandsp.rs`.

use fax_in::audioengine::{AudioConfig, AudioEngine};
use fax_modem::hdlc;
use fax_modem::level::mean_square;
use fax_modem::v21::V21Tx;
use fax_modem::v27ter::{self, testtx::Tx as V27Tx};
use t30::RxSignal;

const SEEDS: [u64; 3] = [0x5B04_0001, 0x5B04_0002, 0x5B04_0003];

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
    fn gauss(&mut self) -> f64 {
        let u = |r: &mut Rng| ((r.next() >> 11) as f64 + 0.5) / (1u64 << 53) as f64;
        (-2.0 * u(self).ln()).sqrt() * (2.0 * std::f64::consts::PI * u(self)).cos()
    }
}

fn frames() -> Vec<Vec<u8>> {
    let tsi = vec![0xFF, 0xC0, t30::fcf::TSI, b' ', b' ', b'1'];
    let dcs = vec![0xFF, 0xC8, t30::fcf::DCS, 0x00, 0x46, 0x00];
    [tsi, dcs]
        .iter()
        .map(|f| f.iter().map(|b| b.reverse_bits()).collect())
        .collect()
}

/// Times (samples on the engine's clock) of the events of one burst.
#[derive(Debug)]
pub struct Times {
    pub preamble: Option<u64>,
    pub no_signal: Option<u64>,
    pub onset: u64,
    pub end: u64,
}

/// The engine hears `burst` (a signal, at `level_dbm0` as made) from sample `onset` (about 6 s,
/// after our first DIS), with white noise `snr_db` below `level_dbm0` over everything if given.
fn measure(burst: &[i16], level_dbm0: f64, snr_db: Option<f64>, seed: u64) -> Times {
    let mut rng = Rng::new(seed);
    let sd = snr_db.map_or(0.0, |s| {
        f64::from(mean_square(level_dbm0 as f32)).sqrt() / 10f64.powf(s / 20.0)
    });
    let onset = 6 * 8000 + (seed % 160);
    let end_total = onset as usize + burst.len() + 8000;
    let mut e = AudioEngine::new(AudioConfig::default());
    e.start();
    let (mut preamble, mut no_signal) = (None, None);
    let mut pos = 0usize;
    while pos < end_total {
        let rx: Vec<i16> = (0..160)
            .map(|i| {
                let t = pos + i;
                let s = if t >= onset as usize && t - (onset as usize) < burst.len() {
                    f64::from(burst[t - onset as usize])
                } else {
                    0.0
                };
                (s + sd * rng.gauss()).round().clamp(-32768.0, 32767.0) as i16
            })
            .collect();
        let mut tx = [0i16; 160];
        e.process(&rx, &mut tx);
        pos += 160;
        for (t, sig) in e.drain_signals() {
            // only what follows the peer's onset (our own first DIS is before)
            if t < onset {
                continue;
            }
            match sig {
                RxSignal::V21Preamble if preamble.is_none() => preamble = Some(t),
                RxSignal::NoSignal if no_signal.is_none() => no_signal = Some(t),
                _ => {}
            }
        }
    }
    Times {
        preamble,
        no_signal,
        onset,
        end: onset + burst.len() as u64,
    }
}

/// 80 ms: three flags at 300 bit/s.
const THREE_FLAGS: i64 = 640;

/// Checks a measurement against the references; returns (preamble error, end error) in ms.
fn check(t: &Times, what: &str) -> (f64, f64) {
    let p = t
        .preamble
        .unwrap_or_else(|| panic!("{what}: no V21Preamble: {t:?}"));
    let n = t
        .no_signal
        .unwrap_or_else(|| panic!("{what}: no NoSignal: {t:?}"));
    let pe = (p as i64 - t.onset as i64 - THREE_FLAGS) as f64 / 8.0;
    let ne = (n as i64 - t.end as i64) as f64 / 8.0;
    assert!(
        pe.abs() < 30.0,
        "{what}: preamble event {pe:+.1} ms from the third flag: {t:?}"
    );
    assert!(
        (-1.0..30.0).contains(&ne),
        "{what}: end event {ne:+.1} ms after the signal: {t:?}"
    );
    (pe, ne)
}

/// The levels of the test; the plan's -43 dBm0 is the engine's own turn-on threshold
/// (`ON_DBM0`) and is the separate, ignored test below.
#[test]
fn v04_preamble_and_end_of_signal_times_are_within_30_ms() {
    for level in [-10.0f64, -30.0, -42.0] {
        for snr in [None, Some(20.0)] {
            for seed in SEEDS {
                let burst =
                    V21Tx::new(level as f32).modulate(&hdlc::encode_burst(&frames(), 38, 2));
                let t = measure(&burst, level, snr, seed);
                let (pe, ne) = check(&t, &format!("{level} dBm0 snr {snr:?} seed {seed:#x}"));
                eprintln!(
                    "V04 {level} dBm0, SNR {snr:?}, seed {seed:#x}: preamble {pe:+.1} ms, end {ne:+.1} ms"
                );
            }
        }
    }
}

#[test]
fn v04_a_short_preamble_still_reports_it_and_the_end() {
    // 4 flags (107 ms): the preamble is reported after the third
    for seed in SEEDS {
        let burst = V21Tx::new(-20.0).modulate(&hdlc::encode_burst(&frames(), 4, 2));
        check(&measure(&burst, -20.0, None, seed), "4 flags");
    }
}

/// The audio of a TCF call: TSI+DCS (V.27ter 4800), the TCF of zeros, and when the engine's
/// first transmission after it starts.
#[test]
fn v04_the_answer_starts_75_ms_plus_or_minus_20_after_the_peer_signal_ends() {
    for seed in SEEDS {
        let mut e = AudioEngine::new(AudioConfig::default());
        e.start();
        let mut rng = Rng::new(seed);
        let sd = f64::from(mean_square(-50.0)).sqrt();
        // our CED and first DIS go by; the peer's DCS follows at 6 s, then its TCF
        let mut dcs = vec![0xFF, 0xC8, t30::fcf::DCS, 0x00, 0x10, 0x00];
        let wire: Vec<Vec<u8>> = [
            vec![0xFF, 0xC0, t30::fcf::TSI, b' ', b'1'],
            std::mem::take(&mut dcs),
        ]
        .iter()
        .map(|f| f.iter().map(|b| b.reverse_bits()).collect())
        .collect();
        let dcs_burst = V21Tx::new(-15.0).modulate(&hdlc::encode_burst(&wire, 38, 2));
        let tcf = V27Tx {
            rate: v27ter::Rate::B4800,
            level_dbm0: -15.0,
            long: true,
            carrier_offset_hz: 0.0,
            clock: 1.0,
        }
        .modulate(&vec![false; 7_200]);
        let dcs_at = 6 * 8000 + (seed % 160) as usize;
        let mut input = vec![0i16; dcs_at];
        input.extend(&dcs_burst);
        // 120 ms of silence (the peer's turnaround), then the TCF
        input.extend(vec![0i16; 960]);
        let tcf_at = input.len();
        input.extend(&tcf);
        // the end of the signal: its last non-zero sample
        let tcf_end = tcf_at + tcf.iter().rposition(|&s| s != 0).unwrap() + 1;
        input.extend(vec![0i16; 8 * 8000]);
        let mut sent_after_dcs: Option<usize> = None;
        let mut sent_after_tcf: Option<usize> = None;
        let mut pos = 0usize;
        while pos < input.len() {
            let rx: Vec<i16> = (0..160)
                .map(|i| {
                    let s = f64::from(input.get(pos + i).copied().unwrap_or(0));
                    (s + sd * rng.gauss()).round().clamp(-32768.0, 32767.0) as i16
                })
                .collect();
            let mut tx = [0i16; 160];
            e.process(&rx, &mut tx);
            for (i, &s) in tx.iter().enumerate() {
                if s != 0 {
                    // the first non-zero sample after the DCS burst ended, and after the TCF
                    let t = pos + i;
                    if t > tcf_end && sent_after_tcf.is_none() {
                        sent_after_tcf = Some(t);
                    }
                    if t > dcs_at + dcs_burst.len() && t < tcf_at && sent_after_dcs.is_none() {
                        sent_after_dcs = Some(t);
                    }
                }
            }
            pos += 160;
        }
        // the TCF is answered (CFR) 75 +- 20 ms after it ends
        let t = sent_after_tcf.expect("no answer to the TCF");
        let gap = (t as i64 - tcf_end as i64) as f64 / 8.0;
        eprintln!("V04 seed {seed:#x}: the CFR starts {gap:.1} ms after the end of the TCF");
        assert!(
            (55.0..=95.0).contains(&gap),
            "{gap} ms (T.30 5.3.2: 75 +- 20)"
        );
        // the DCS is not answered at all (the engine waits for the TCF)
        assert!(
            sent_after_dcs.is_none(),
            "an answer to the DCS at {sent_after_dcs:?}"
        );
    }
}

#[test]
fn v04_plan_minus_43_dbm0() {
    for snr in [None, Some(20.0)] {
        for seed in SEEDS {
            let burst = V21Tx::new(-43.0).modulate(&hdlc::encode_burst(&frames(), 38, 2));
            check(
                &measure(&burst, -43.0, snr, seed),
                &format!("-43 dBm0 {snr:?} {seed:#x}"),
            );
        }
    }
}
