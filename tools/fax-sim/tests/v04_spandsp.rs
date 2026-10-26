//! M5b test V04 with SpanDSP as the source (test plan `docs/test-plans/M5.md` section 5): the
//! preamble of a SpanDSP `hdlc_tx` + `fsk_tx` burst reaches T.30 in our `AudioEngine` as
//! `RxSignal::V21Preamble` (reference: the onset plus three flags, 80 ms) and the end of the
//! signal as `RxSignal::NoSignal`, both within 30 ms. The same measurement with our own sender is
//! `crates/fax-in/tests/m5b_v04.rs`.

mod m5b_common;

use fax_in::audioengine::{AudioConfig, AudioEngine};
use fax_modem::level::mean_square;
use m5b_common::{Rng, spandsp_fsk_tx, spandsp_hdlc_bits};
use t30::RxSignal;

const SEEDS: [u64; 3] = [0x5B04_1001, 0x5B04_1002, 0x5B04_1003];

fn burst(level: f32, flags: i32) -> Vec<i16> {
    let tsi = vec![0xFFu8, 0xC0, t30::fcf::TSI, b' ', b'1'];
    let dcs = vec![0xFFu8, 0xC8, t30::fcf::DCS, 0x00, 0x46, 0x00];
    let frames: Vec<Vec<u8>> = [tsi, dcs]
        .iter()
        .map(|f| f.iter().map(|b| b.reverse_bits()).collect())
        .collect();
    spandsp_fsk_tx(&spandsp_hdlc_bits(&frames, flags, 2), level)
}

/// (preamble error, end error) in ms for the engine hearing `signal` from about 6 s.
fn measure(signal: &[i16], level: f64, snr: Option<f64>, seed: u64) -> (f64, f64) {
    let mut rng = Rng::new(seed);
    let sd = snr.map_or(0.0, |s| {
        f64::from(mean_square(level as f32)).sqrt() / 10f64.powf(s / 20.0)
    });
    let first = signal.iter().position(|&s| s != 0).unwrap();
    let last = signal.iter().rposition(|&s| s != 0).unwrap() + 1;
    let at = 6 * 8000 + (seed % 160) as usize;
    let mut e = AudioEngine::new(AudioConfig::default());
    e.start();
    let (mut pre, mut end) = (None, None);
    let mut pos = 0usize;
    while pos < at + signal.len() + 8000 {
        let rx: Vec<i16> = (0..160)
            .map(|i| {
                let t = pos + i;
                let s = if t >= at && t - at < signal.len() {
                    f64::from(signal[t - at])
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
            if (t as usize) < at {
                continue;
            }
            match sig {
                RxSignal::V21Preamble if pre.is_none() => pre = Some(t),
                RxSignal::NoSignal if end.is_none() => end = Some(t),
                _ => {}
            }
        }
    }
    let p = pre.expect("no V21Preamble");
    let n = end.expect("no NoSignal");
    (
        (p as i64 - (at + first) as i64 - 640) as f64 / 8.0,
        (n as i64 - (at + last) as i64) as f64 / 8.0,
    )
}

#[test]
fn v04_spandsp_burst_preamble_and_end_times_within_30_ms() {
    for level in [-10.0f64, -30.0, -42.0] {
        let signal = burst(level as f32, 38);
        for snr in [None, Some(20.0)] {
            for seed in SEEDS {
                let (pe, ne) = measure(&signal, level, snr, seed);
                eprintln!(
                    "V04 SpanDSP {level} dBm0, SNR {snr:?}, seed {seed:#x}: preamble {pe:+.1} ms, end {ne:+.1} ms"
                );
                assert!(pe.abs() < 30.0, "preamble {pe} ms");
                assert!((-1.0..30.0).contains(&ne), "end {ne} ms");
            }
        }
    }
}
