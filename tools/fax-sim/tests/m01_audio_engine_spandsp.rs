//! M5c test M01 (test plan `docs/test-plans/M5.md` section 7): SpanDSP's `fax_state` (the calling
//! side, restricted to V.27ter) sends one standard-resolution page as 8 kHz audio into our
//! `AudioEngine` (the answering side), in-process, 20 ms at a time. The page must arrive pixel for
//! pixel, and the result must say V.27ter at 4800 bit/s.

use fax_in::audioengine::{AudioConfig, AudioEngine};
use fax_sim::SplitMix64;
use fax_sim::spandsp::{FaxModem, T30Config};
use fax_sim::tiff_writer::{self, PageSpec, render_bitmap};
use spandsp_sys as sys;
use std::path::PathBuf;

fn tmp(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("m01-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

/// Gaussian noise, `sd` per sample (Box-Muller on SplitMix64).
struct Noise(SplitMix64);

impl Noise {
    fn uniform(&mut self) -> f64 {
        ((self.0.next_u64() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    fn gauss(&mut self) -> f64 {
        (-2.0 * self.uniform().ln()).sqrt() * (2.0 * std::f64::consts::PI * self.uniform()).cos()
    }
}

struct Outcome {
    engine_done: bool,
    caller_code: Option<i32>,
    result: Option<t30::T30Result>,
    pages: Vec<fax_in::t38engine::ReceivedPage>,
    log: Vec<String>,
    sim_ms: u64,
}

/// One call. `noise`: white noise (SNR in dB, seed) on the audio into the engine, relative to
/// the level of SpanDSP's signal (-13 dBm0 or so; measured at -14.0 dBm0).
fn call(spec: PageSpec, modems: i32, noise: Option<(f64, u64)>) -> Outcome {
    let tif = tmp(&format!("m01-{}-{}.tif", spec.height, std::process::id()));
    tiff_writer::write_file(&tif, &[spec]).unwrap();
    let cfg = T30Config {
        tx_file: Some(&tif),
        rx_file: None,
        ecm: false,
        ident: "0000000001",
        modems: Some(modems),
        ..T30Config::default()
    };
    let mut caller = FaxModem::new(true, &cfg).expect("caller");
    let mut ecfg = AudioConfig::default();
    ecfg.t30.local_id = "0287654321".into();
    let mut engine = AudioEngine::new(ecfg);
    engine.start();
    let mut noise = noise.map(|(snr, seed)| {
        // -13 dBm0 signal: rms 16141 * 10^(-13/20)
        let sd = 16141.0 * 10f64.powf(-13.0 / 20.0) / 10f64.powf(snr / 20.0);
        (sd, Noise(SplitMix64::new(seed)))
    });
    let mut sim_ms = 0;
    while sim_ms < 600_000 && !(engine.is_done() && caller.completion_code().is_some()) {
        let mut from_caller = [0i16; 160];
        caller.tx(&mut from_caller);
        if let Some((sd, n)) = noise.as_mut() {
            for s in from_caller.iter_mut() {
                *s = (f64::from(*s) + *sd * n.gauss())
                    .round()
                    .clamp(-32768.0, 32767.0) as i16;
            }
        }
        let mut from_engine = [0i16; 160];
        engine.process(&from_caller, &mut from_engine);
        caller.rx(&mut from_engine);
        sim_ms += 20;
    }
    Outcome {
        engine_done: engine.is_done(),
        caller_code: caller.completion_code(),
        result: engine.result().cloned(),
        pages: engine.take_pages(),
        log: engine.drain_log(),
        sim_ms,
    }
}

fn assert_page_is_pixel_exact(o: &Outcome, spec: &PageSpec) {
    assert_eq!(o.pages.len(), 1, "pages; log {:#?}", o.log);
    let got = &o.pages[0];
    assert_eq!((got.width, got.height), (spec.width, spec.height));
    assert_eq!(got.y_dpi, spec.y_dpi, "vertical resolution");
    assert_eq!(got.bad_lines, 0);
    let dec = t4::decode_page(&got.g4, t4::DecoderConfig::new(got.width, t4::Coding::Mmr)).unwrap();
    assert_eq!(dec.summary.stats.rows, spec.height);
    let want = render_bitmap(spec, 0);
    assert_eq!(dec.bitmap.len(), want.len());
    let diff = dec.bitmap.iter().zip(&want).filter(|(a, b)| a != b).count();
    assert_eq!(diff, 0, "{diff} differing octets");
}

fn assert_v27ter_4800(o: &Outcome, spec: &PageSpec) {
    assert!(
        o.engine_done && o.sim_ms < 600_000,
        "engine not done after {} ms; log {:#?}",
        o.sim_ms,
        o.log
    );
    assert_eq!(
        o.caller_code,
        Some(sys::T30_ERR_OK),
        "SpanDSP's result; log {:#?}",
        o.log
    );
    let r = o.result.as_ref().unwrap();
    assert!(r.code.is_ok(), "{:?}; log {:#?}", r.code, o.log);
    assert_eq!(r.pages, 1);
    assert_eq!(r.modem, Some(t30::Modem::V27ter), "modem of the last DCS");
    assert_eq!(r.bit_rate, 4800, "bit rate of the last DCS");
    assert_eq!(
        o.pages[0].modem,
        t30::Modem::V27ter,
        "modem the page arrived over"
    );
    assert_page_is_pixel_exact(o, spec);
}

#[test]
fn m01_spandsp_v27ter_one_standard_page_is_received_pixel_exact() {
    for height in [300u32, 1100] {
        let spec = PageSpec::standard(height);
        let o = call(spec, sys::T30_SUPPORT_V27TER, None);
        assert_v27ter_4800(&o, &spec);
        eprintln!(
            "M01 clean, {height} rows: {} ms; {:?}",
            o.sim_ms,
            o.result
                .as_ref()
                .map(|r| (r.modem, r.bit_rate, r.duration_ms))
        );
    }
}

/// The end of a signal is found by the level falling below -48 dBm0, for T.30 as for the
/// receiver: white noise louder than that (SNR below about 34 dB against SpanDSP's -14 dBm0)
/// keeps the signal "present" and the engine never sees the end of the TCF (docs/test-reports/
/// M5c.md section 5). The noise here is -55 to -50 dBm0.
#[test]
fn m01_with_line_noise_in_three_seeds() {
    for (snr, seed) in [
        (41.0, 0x4D01_0001u64),
        (38.0, 0x4D01_0002),
        (36.0, 0x4D01_0003),
    ] {
        let spec = PageSpec::standard(300);
        let o = call(spec, sys::T30_SUPPORT_V27TER, Some((snr, seed)));
        assert_v27ter_4800(&o, &spec);
    }
}
