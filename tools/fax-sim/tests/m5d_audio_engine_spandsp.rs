//! M5d tests M02-M08 (test plan `docs/test-plans/M5.md` section 7): SpanDSP's `fax_state` (the
//! calling side) sends pages as 8 kHz audio into our `AudioEngine` (the answering side), in
//! process, 20 ms at a time, over V.29. The channel between them is a pair of hooks: one on the
//! audio towards the engine (it can look at the engine's T.30 state) and one on the audio towards
//! SpanDSP.
//!
//! M01 (V.27ter) is in `m01_audio_engine_spandsp.rs`.

use fax_in::audioengine::{AudioConfig, AudioEngine};
use fax_modem::hdlc::{self, HdlcEvent, HdlcRx};
use fax_modem::v21::{V21Rx, V21Tx};
use fax_sim::SplitMix64;
use fax_sim::spandsp::{FaxModem, T30Config};
use fax_sim::tiff_writer::{self, PageSpec, render_bitmap};
use spandsp_sys as sys;
use std::ffi::c_void;
use std::os::raw::c_int;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU32, Ordering};

fn tmp(name: &str) -> PathBuf {
    static N: AtomicU32 = AtomicU32::new(0);
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("m5d-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(format!("{}-{name}", N.fetch_add(1, Ordering::Relaxed)))
}

/// Gaussian noise (Box-Muller on SplitMix64).
struct Noise(SplitMix64);

impl Noise {
    fn uniform(&mut self) -> f64 {
        ((self.0.next_u64() >> 11) as f64 + 0.5) / (1u64 << 53) as f64
    }
    fn gauss(&mut self) -> f64 {
        (-2.0 * self.uniform().ln()).sqrt() * (2.0 * std::f64::consts::PI * self.uniform()).cos()
    }
}

/// The rms of SpanDSP's signal is about -13 to -14 dBm0; a level in dBm0 as an rms.
fn rms(dbm0: f64) -> f64 {
    16141.0 * 10f64.powf(dbm0 / 20.0)
}

// -------------------------------------------------------------------------------- the call

#[derive(Clone)]
struct Opts {
    pages: Vec<PageSpec>,
    /// `T30_SUPPORT_*` of SpanDSP; `None`: all three.
    modems: Option<i32>,
    /// `T4_COMPRESSION_*` of SpanDSP; `None`: MH, MR and MMR.
    compressions: Option<i32>,
    /// SpanDSP's ECM capability.
    ecm_caller: bool,
    /// Our ECM (`AudioConfig::t30.ecm`).
    ecm_engine: bool,
}

impl Opts {
    fn new(pages: Vec<PageSpec>) -> Self {
        Self {
            pages,
            modems: None,
            compressions: None,
            ecm_caller: false,
            ecm_engine: false,
        }
    }
    fn modems(mut self, m: i32) -> Self {
        self.modems = Some(m);
        self
    }
    fn compressions(mut self, c: i32) -> Self {
        self.compressions = Some(c);
        self
    }
}

struct Outcome {
    engine_done: bool,
    caller_code: Option<i32>,
    result: Option<t30::T30Result>,
    pages: Vec<fax_in::t38engine::ReceivedPage>,
    log: Vec<String>,
    sim_ms: u64,
    /// Everything the engine sent (before the down hook).
    engine_tx: Vec<i16>,
}

/// Returns `true` when the block is lost (the engine is told so, M5e ④).
type Up<'a> = &'a mut dyn FnMut(&AudioEngine, u64, &mut [i16]) -> bool;
type Down<'a> = &'a mut dyn FnMut(u64, &mut [i16]);

/// One call; `up` sees SpanDSP's audio on its way to the engine (and the engine), `down` the
/// engine's audio on its way to SpanDSP. Both get the simulated time in ms.
fn call_with(o: &Opts, up: Up, down: Down) -> Outcome {
    let tif = tmp("pages.tif");
    tiff_writer::write_file(&tif, &o.pages).unwrap();
    let cfg = T30Config {
        tx_file: Some(&tif),
        rx_file: None,
        ecm: o.ecm_caller,
        ident: "0000000001",
        modems: o.modems,
        compressions: o.compressions,
        ..T30Config::default()
    };
    let mut caller = FaxModem::new(true, &cfg).expect("caller");
    let mut ecfg = AudioConfig::default();
    ecfg.t30.local_id = "0287654321".into();
    ecfg.t30.fine = true;
    ecfg.t30.mr = true;
    ecfg.t30.ecm = o.ecm_engine;
    let mut engine = AudioEngine::new(ecfg);
    engine.start();
    let mut sim_ms = 0;
    let mut engine_tx = Vec::new();
    while sim_ms < 900_000 && !(engine.is_done() && caller.completion_code().is_some()) {
        let mut from_caller = [0i16; 160];
        caller.tx(&mut from_caller);
        let lost = up(&engine, sim_ms, &mut from_caller);
        let mut from_engine = [0i16; 160];
        if lost {
            engine.process_lost(from_caller.len(), &mut from_engine);
        } else {
            engine.process(&from_caller, &mut from_engine);
        }
        engine_tx.extend_from_slice(&from_engine);
        down(sim_ms, &mut from_engine);
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
        engine_tx,
    }
}

fn call(o: &Opts) -> Outcome {
    call_with(o, &mut |_, _, _| false, &mut |_, _| {})
}

// ------------------------------------------------------------------------------- assertions

/// Page `i` of `o` arrived pixel for pixel as `spec`.
fn assert_page_exact(o: &Outcome, i: usize, spec: &PageSpec) {
    let got = &o.pages[i];
    assert_eq!(
        (got.width, got.height),
        (spec.width, spec.height),
        "page {i}"
    );
    assert_eq!(got.y_dpi, spec.y_dpi, "page {i}: vertical resolution");
    assert_eq!(got.bad_lines, 0, "page {i}: bad lines");
    let dec = t4::decode_page(&got.g4, t4::DecoderConfig::new(got.width, t4::Coding::Mmr)).unwrap();
    assert_eq!(dec.summary.stats.rows, spec.height, "page {i}");
    let want = render_bitmap(spec, i as u32);
    assert_eq!(dec.bitmap.len(), want.len(), "page {i}");
    let diff = dec.bitmap.iter().zip(&want).filter(|(a, b)| a != b).count();
    assert_eq!(diff, 0, "page {i}: {diff} differing octets");
}

/// A call that went well: SpanDSP's result is OK, ours is OK with `pages` pages, all pixel exact,
/// over `modem` at `rate` (of the last DCS).
fn assert_received(o: &Outcome, specs: &[PageSpec], modem: t30::Modem, rate: &[u32]) {
    assert!(
        o.engine_done && o.sim_ms < 900_000,
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
    assert_eq!(r.pages as usize, specs.len(), "T30Result pages");
    assert_eq!(r.modem, Some(modem), "modem of the last DCS");
    assert!(
        rate.contains(&r.bit_rate),
        "bit rate {} not in {rate:?}",
        r.bit_rate
    );
    assert_eq!(o.pages.len(), specs.len(), "pages; log {:#?}", o.log);
    for (i, spec) in specs.iter().enumerate() {
        assert_eq!(o.pages[i].modem, modem, "page {i}: modem it arrived over");
        assert_page_exact(o, i, spec);
    }
}

fn three_fine_pages() -> Vec<PageSpec> {
    vec![
        PageSpec::fine(300),
        PageSpec::fine(400),
        PageSpec::fine(500),
    ]
}

// ===========================================================================================
// M02: V.29 forced, 3 fine pages, MH and MR

#[test]
fn m02_v29_forced_three_fine_pages_mh() {
    let specs = three_fine_pages();
    let o = call(
        &Opts::new(specs.clone())
            .modems(sys::T30_SUPPORT_V29)
            .compressions(sys::T4_COMPRESSION_T4_1D),
    );
    assert_received(&o, &specs, t30::Modem::V29, &[9600]);
    let r = o.result.as_ref().unwrap();
    assert_eq!(r.compression, Some(t30::caps::Compression::Mh));
    assert_eq!(r.resolution, Some(t30::caps::Resolution::Fine));
    assert!(
        o.pages
            .iter()
            .all(|p| p.source_coding == t30::caps::Compression::Mh),
        "pages arrived as MH"
    );
    eprintln!(
        "M02 MH: {} ms; {:?}",
        o.sim_ms,
        (r.modem, r.bit_rate, r.duration_ms)
    );
}

#[test]
fn m02_v29_forced_three_fine_pages_mr() {
    let specs = three_fine_pages();
    let o = call(
        &Opts::new(specs.clone())
            .modems(sys::T30_SUPPORT_V29)
            .compressions(sys::T4_COMPRESSION_T4_1D | sys::T4_COMPRESSION_T4_2D),
    );
    assert_received(&o, &specs, t30::Modem::V29, &[9600]);
    let r = o.result.as_ref().unwrap();
    assert_eq!(r.compression, Some(t30::caps::Compression::Mr));
    assert!(
        o.pages
            .iter()
            .all(|p| p.source_coding == t30::caps::Compression::Mr),
        "pages arrived as MR"
    );
    eprintln!(
        "M02 MR: {} ms; {:?}",
        o.sim_ms,
        (r.modem, r.bit_rate, r.duration_ms)
    );
}

#[test]
fn m02_standard_resolution_page_over_v29() {
    let specs = vec![PageSpec::standard(600)];
    let o = call(&Opts::new(specs.clone()).modems(sys::T30_SUPPORT_V29));
    assert_received(&o, &specs, t30::Modem::V29, &[9600]);
}

#[test]
fn m02_v29_with_line_noise_in_three_seeds() {
    // white noise on the whole call, kept below the -48 dBm0 end-of-signal threshold (docs/
    // test-reports/M5c.md section 5): -55 to -50 dBm0, SNR 36 to 41 dB against SpanDSP's -14
    for (snr, seed) in [
        (41.0, 0x5D02_0001u64),
        (38.0, 0x5D02_0002),
        (36.0, 0x5D02_0003),
    ] {
        let specs = vec![PageSpec::fine(300), PageSpec::fine(300)];
        let mut noise = Noise(SplitMix64::new(seed));
        let sd = rms(-13.0) / 10f64.powf(snr / 20.0);
        let o = call_with(
            &Opts::new(specs.clone()).modems(sys::T30_SUPPORT_V29),
            &mut |_, _, a| {
                for s in a.iter_mut() {
                    *s = (f64::from(*s) + sd * noise.gauss())
                        .round()
                        .clamp(-32768.0, 32767.0) as i16;
                }
                false
            },
            &mut |_, _| {},
        );
        assert_received(&o, &specs, t30::Modem::V29, &[9600]);
    }
}

// ===========================================================================================
// M03: not forced: SpanDSP picks from our DIS

#[test]
fn m03_spandsp_picks_v29_9600_from_a_dis_without_v17() {
    let specs = vec![PageSpec::fine(300)];
    // all three modems on SpanDSP's side
    let o = call(&Opts::new(specs.clone()));
    assert_received(&o, &specs, t30::Modem::V29, &[9600]);
    // our DIS: V.27ter and V.29, no V.17 (SpanDSP's choice follows from it)
    let r = o.result.as_ref().unwrap();
    assert_ne!(r.modem, Some(t30::Modem::V17));
    // also with V.27ter + V.29 only, and with a second page of standard resolution
    let specs = vec![PageSpec::standard(300), PageSpec::fine(300)];
    let o = call(&Opts::new(specs.clone()).modems(sys::T30_SUPPORT_V27TER | sys::T30_SUPPORT_V29));
    assert_received(&o, &specs, t30::Modem::V29, &[9600]);
}

#[test]
fn m03_a_caller_with_v17_only_falls_back_to_v27ter_4800() {
    // SpanDSP supports V.17 only on paper: our DIS has no V.17, and SpanDSP does not give up, it
    // uses V.27ter 4800 (the modem every fax has), which we receive
    let specs = vec![PageSpec::fine(300)];
    let o = call(&Opts::new(specs.clone()).modems(sys::T30_SUPPORT_V17));
    assert_received(&o, &specs, t30::Modem::V27ter, &[4800]);
}

// ===========================================================================================
// M04: the TCF at 9600 fails: FTT, SpanDSP falls back, the call succeeds

/// A burst of white noise on the TCF: from `from_ms` to `to_ms` after the engine entered `RxTcf`
/// for the first time, at `level` dBm0.
fn tcf_burst(
    level: f64,
    from_ms: u64,
    to_ms: u64,
    seed: u64,
) -> impl FnMut(&AudioEngine, u64, &mut [i16]) -> bool {
    let mut noise = Noise(SplitMix64::new(seed));
    let mut entered: Option<u64> = None;
    let mut done = false;
    move |e, t, a| {
        if done {
            return false;
        }
        if entered.is_none() && e.state() == t30::State::RxTcf {
            entered = Some(t);
        }
        if let Some(t0) = entered {
            if t >= t0 + from_ms && t < t0 + to_ms {
                let sd = rms(level);
                for s in a.iter_mut() {
                    *s = (f64::from(*s) + sd * noise.gauss())
                        .round()
                        .clamp(-32768.0, 32767.0) as i16;
                }
            } else if t >= t0 + to_ms {
                done = true;
            }
        }
        false
    }
}

#[test]
fn m04_a_bad_tcf_at_9600_is_refused_and_the_call_falls_back() {
    for seed in [0x5D04_0001u64, 0x5D04_0002, 0x5D04_0003] {
        let specs = vec![PageSpec::fine(300), PageSpec::fine(300)];
        // 600 ms of noise 5 dB below the signal in the middle of the TCF
        let mut up = tcf_burst(-18.0, 400, 1_000, seed);
        let o = call_with(
            &Opts::new(specs.clone()).modems(sys::T30_SUPPORT_V29),
            &mut up,
            &mut |_, _| {},
        );
        let ftts = o.log.iter().filter(|l| l.starts_with("TCF:")).count();
        let r = o.result.as_ref().unwrap();
        eprintln!(
            "M04 seed {seed:#x}: {} TCF(s); final {:?} {} bit/s; log {:#?}",
            ftts, r.modem, r.bit_rate, o.log
        );
        // the first TCF failed (FTT), and the call went on at a lower rate, to the end
        assert!(ftts >= 2, "only {ftts} TCF(s); log {:#?}", o.log);
        assert_received(&o, &specs, t30::Modem::V29, &[7200]);
    }
}

/// White noise of `snr` dB (relative to SpanDSP's signal, about -13 dBm0) over the whole call.
fn constant_snr(snr: f64, seed: u64) -> impl FnMut(&AudioEngine, u64, &mut [i16]) -> bool {
    let mut noise = Noise(SplitMix64::new(seed));
    let sd = rms(-13.0) / 10f64.powf(snr / 20.0);
    move |_, _, a| {
        for s in a.iter_mut() {
            *s = (f64::from(*s) + sd * noise.gauss())
                .round()
                .clamp(-32768.0, 32767.0) as i16;
        }
        false
    }
}

/// The plan's wording: a channel whose constant SNR makes the TCF of 9600 fail, we answer FTT and
/// SpanDSP falls back and succeeds. Since ADR-066 the engine finds the end of a signal in noise,
/// so this works. Measured by sweeping 8-20 dB (3 seeds each, SpanDSP forced to V.29): 15 dB and
/// up pass the 9600 TCF; at 11 dB the TCF has 94.6-94.9 % zero bits (T.30 needs 95 %) in all
/// three seeds: FTT, then 7200 (one seed, pages 13 and 26 bad lines) or 4800/2400 (two seeds,
/// where the 7200 TCF is refused too); 12-14 dB pass the TCF but the page is rejected (the next
/// test); 10 dB goes down to 2400 after four TCFs; 8 dB cannot be received at all (the last
/// test). A fall-back needs the TCF to fail for the answer to be FTT. Since the M5b review (75 ms
/// of silence after CED, the turnaround counted on the engine's clock) the timing moved and 11 dB
/// gave 95.6 % in seed 0 (CFR), so the condition is 10.5 dB: 92.9-94.4 % in the three seeds.
#[test]
fn m04_constant_snr_10_5_db_fails_the_9600_tcf_with_ftt_and_the_call_succeeds_lower() {
    for seed in 0..3u64 {
        let specs = vec![PageSpec::fine(300), PageSpec::fine(300)];
        let mut up = constant_snr(10.5, 0x5D04_2000 + 11 * 16 + seed);
        let o = call_with(
            &Opts::new(specs.clone()).modems(sys::T30_SUPPORT_V29),
            &mut up,
            &mut |_, _| {},
        );
        let tcfs: Vec<&String> = o.log.iter().filter(|l| l.starts_with("TCF:")).collect();
        let r = o.result.as_ref().unwrap();
        eprintln!(
            "M04 constant 10.5 dB seed {seed}: {} TCF(s) {tcfs:?}; final {:?} {} bit/s, pages {}, bad lines {:?}; {} ms",
            tcfs.len(),
            r.modem,
            r.bit_rate,
            o.pages.len(),
            o.pages.iter().map(|p| p.bad_lines).collect::<Vec<_>>(),
            o.sim_ms
        );
        // the 9600 TCF failed (FTT) ...
        assert!(tcfs.len() >= 2, "log {:#?}", o.log);
        assert!(
            tcfs[0].contains("(need 1440)") && o.log.iter().any(|l| l.starts_with("V.21 tx FTT")),
            "the first TCF is the 9600 one and is answered FTT; log {:#?}",
            o.log
        );
        // ... and SpanDSP went down and got the two pages through
        assert!(o.engine_done, "log {:#?}", o.log);
        assert_eq!(o.caller_code, Some(sys::T30_ERR_OK), "log {:#?}", o.log);
        assert!(r.code.is_ok(), "{:?}", r.code);
        assert!(r.bit_rate < 9600, "{}", r.bit_rate);
        assert_eq!(o.pages.len(), 2);
        for (i, p) in o.pages.iter().enumerate() {
            assert!(
                u64::from(p.bad_lines) * 100 <= u64::from(specs[i].height) * 10,
                "page {i}: {} bad lines",
                p.bad_lines
            );
        }
    }
}

/// At 14 dB the 9600 TCF passes but the page has too many bad lines: RTN, SpanDSP retrains at
/// 7200 (its TCF is good) and resends; the pages arrive pixel exact (3 seeds).
#[test]
fn m04_constant_snr_14_db_page_rejected_with_rtn_then_7200_exact() {
    for seed in 0..3u64 {
        let specs = vec![PageSpec::fine(300), PageSpec::fine(300)];
        let mut up = constant_snr(14.0, 0x5D04_2000 + 14 * 16 + seed);
        let o = call_with(
            &Opts::new(specs.clone()).modems(sys::T30_SUPPORT_V29),
            &mut up,
            &mut |_, _| {},
        );
        let tcfs: Vec<&String> = o.log.iter().filter(|l| l.starts_with("TCF:")).collect();
        eprintln!(
            "M04 constant 14 dB seed {seed}: {} TCF(s) {tcfs:?}; {} ms",
            tcfs.len(),
            o.sim_ms
        );
        assert_eq!(tcfs.len(), 2, "log {:#?}", o.log);
        assert!(
            o.log.iter().any(|l| l.starts_with("page rejected")),
            "the first page is rejected; log {:#?}",
            o.log
        );
        assert!(o.log.iter().any(|l| l.starts_with("V.21 tx RTN")));
        assert_received(&o, &specs, t30::Modem::V29, &[7200]);
    }
}

/// At 12 dB the 9600 TCF passes but the page is rejected (RTN) and the rate falls to 7200; the pages arrive within the
/// T.30 bad-line threshold (3 seeds).
#[test]
fn m04_constant_snr_12_db_pages_within_the_bad_line_threshold() {
    for seed in 0..3u64 {
        let specs = vec![PageSpec::fine(300), PageSpec::fine(300)];
        let mut up = constant_snr(12.0, 0x5D04_2000 + 12 * 16 + seed);
        let o = call_with(
            &Opts::new(specs.clone()).modems(sys::T30_SUPPORT_V29),
            &mut up,
            &mut |_, _| {},
        );
        let r = o.result.as_ref().unwrap();
        eprintln!(
            "M04 constant 12 dB seed {seed}: {:?} {} bit/s, pages {}, bad lines {:?}, {} ms",
            r.modem,
            r.bit_rate,
            o.pages.len(),
            o.pages.iter().map(|p| p.bad_lines).collect::<Vec<_>>(),
            o.sim_ms
        );
        assert!(o.engine_done, "log {:#?}", o.log);
        assert_eq!(o.caller_code, Some(sys::T30_ERR_OK), "log {:#?}", o.log);
        assert!(r.code.is_ok(), "{:?}", r.code);
        assert_eq!(o.pages.len(), 2);
        assert!(r.bit_rate < 9600, "fell back from 9600: {}", r.bit_rate);
        for (i, p) in o.pages.iter().enumerate() {
            assert!(
                u64::from(p.bad_lines) * 100 <= u64::from(specs[i].height) * 10,
                "page {i}: {} bad lines of {}",
                p.bad_lines,
                specs[i].height
            );
        }
    }
}

/// Noise so loud (8 dB) that no TCF can pass: the call must still end (T.30 timers), not hang.
#[test]
fn m04_hopeless_noise_ends_the_call() {
    for seed in 0..3u64 {
        let specs = vec![PageSpec::fine(300)];
        let mut up = constant_snr(8.0, 0x5D04_2000 + 8 * 16 + seed);
        let o = call_with(
            &Opts::new(specs).modems(sys::T30_SUPPORT_V29),
            &mut up,
            &mut |_, _| {},
        );
        let r = o.result.as_ref().map(|r| (r.code, r.pages));
        eprintln!(
            "M04 constant 8 dB seed {seed}: engine done {} after {} ms, ours {r:?}, SpanDSP {:?}",
            o.engine_done, o.sim_ms, o.caller_code
        );
        assert!(o.engine_done, "log {:#?}", o.log);
        assert!(o.sim_ms < 120_000, "{} ms", o.sim_ms);
    }
}

// ===========================================================================================
// M05: one lost packet (20 ms) inside a page

/// One 20 ms packet of the second of three pages (600 ms into it) is lost; returns the outcome.
fn lost_packet_in_page_two() -> (Outcome, Vec<PageSpec>, u64) {
    let specs = vec![
        PageSpec::fine(500),
        PageSpec::fine(500),
        PageSpec::fine(500),
    ];
    let mut entered = 0;
    let mut was_rx = false;
    let mut t_in = 0;
    let mut dropped_at = None;
    let o = call_with(
        &Opts::new(specs.clone()).modems(sys::T30_SUPPORT_V29),
        &mut |e, t, a| {
            let rx = e.state() == t30::State::RxImage;
            if rx && !was_rx {
                entered += 1;
                t_in = t;
            }
            was_rx = rx;
            if entered == 2 && rx && dropped_at.is_none() && t >= t_in + 600 {
                a.fill(0);
                dropped_at = Some(t);
                return true;
            }
            false
        },
        &mut |_, _| {},
    );
    let at = dropped_at.expect("the second page was never being received");
    (o, specs, at)
}

fn describe_m05(o: &Outcome, at: u64) -> String {
    let r = o.result.as_ref().unwrap();
    format!(
        "gap at {at} ms; ours {:?} pages {} bad_lines {:?}; heights {:?}; per page (bad, run) {:?}; SpanDSP {:?}; sim {} ms; log tail {:#?}",
        r.code,
        r.pages,
        r.bad_lines_per_page,
        o.pages.iter().map(|p| p.height).collect::<Vec<_>>(),
        o.pages
            .iter()
            .map(|p| (p.bad_lines, p.longest_bad_run))
            .collect::<Vec<_>>(),
        o.caller_code,
        o.sim_ms,
        &o.log[o.log.len().saturating_sub(12)..]
    )
}

/// The plan: the damaged page has bad lines, T.30 answers MCF (or RTN), the other pages are
/// pixel exact. Met since M5e: the engine is told the block is lost and the receivers skip it
/// (`AudioEngine::process_lost`, ADR-066); before, the gap ended the carrier and the call.
#[test]
fn m05_a_lost_packet_in_the_middle_of_a_page_plan() {
    let (o, specs, at) = lost_packet_in_page_two();
    let desc = describe_m05(&o, at);
    eprintln!("M05: {desc}");
    assert!(o.engine_done, "{desc}");
    assert_eq!(o.pages.len(), 3, "{desc}");
    assert_page_exact(&o, 0, &specs[0]);
    assert_page_exact(&o, 2, &specs[2]);
    assert!(o.pages[1].bad_lines >= 1, "{desc}");
}

// ===========================================================================================
// M06: our MCF is lost: the sender repeats its command

/// The bursts of non-zero audio in `audio` (gaps of silence up to 400 samples are inside a burst),
/// as sample ranges.
fn bursts(audio: &[i16]) -> Vec<std::ops::Range<usize>> {
    let mut out: Vec<std::ops::Range<usize>> = Vec::new();
    let mut i = 0;
    while i < audio.len() {
        if audio[i] == 0 {
            i += 1;
            continue;
        }
        let start = i;
        let mut last = i;
        while i < audio.len() && i - last <= 400 {
            if audio[i] != 0 {
                last = i;
            }
            i += 1;
        }
        out.push(start..last + 1);
    }
    out
}

fn reverse_octets(frame: &[u8]) -> Vec<u8> {
    frame.iter().map(|b| b.reverse_bits()).collect()
}

fn frames_in(audio: &[i16]) -> Vec<Vec<u8>> {
    let mut rx = V21Rx::new();
    let mut h = HdlcRx::new();
    let mut padded = audio.to_vec();
    padded.extend(vec![0i16; 1600]);
    rx.feed(&padded)
        .into_iter()
        .filter_map(|b| match h.bit(b) {
            Some(HdlcEvent::Frame { data, fcs_ok: true }) => Some(reverse_octets(&data)),
            _ => None,
        })
        .collect()
}

#[test]
fn m06_a_lost_mcf_is_repeated_not_taken_for_a_new_page() {
    let specs = vec![
        PageSpec::fine(300),
        PageSpec::fine(300),
        PageSpec::fine(300),
    ];
    let opts = Opts::new(specs.clone()).modems(sys::T30_SUPPORT_V29);
    // the first run, to find the MCFs in what the engine sent
    let first = call(&opts);
    assert_received(&first, &specs, t30::Modem::V29, &[9600]);
    let mcf: Vec<std::ops::Range<usize>> = bursts(&first.engine_tx)
        .into_iter()
        .filter(|b| {
            frames_in(&first.engine_tx[b.clone()])
                .iter()
                .any(|f| f[2] == t30::fcf::MCF)
        })
        .collect();
    assert_eq!(
        mcf.len(),
        3,
        "an MCF after each of the three pages: {mcf:?}"
    );
    // the same call, deterministic, with one MCF replaced by silence (after the MPS, and after
    // the EOP of the last page)
    for (k, name) in [(0usize, "after the first MPS"), (2, "after the last EOP")] {
        let lost = mcf[k].clone();
        let o = call_with(&opts, &mut |_, _, _| false, &mut |t, a| {
            let from = (t / 20) as usize * 160;
            for (i, s) in a.iter_mut().enumerate() {
                if lost.contains(&(from + i)) {
                    *s = 0;
                }
            }
        });
        eprintln!(
            "M06 MCF lost {name}: {} ms (undisturbed {} ms); log {:#?}",
            o.sim_ms, first.sim_ms, o.log
        );
        // the sender timed out and repeated its command; the call completes with three pages,
        // none of them twice, all pixel exact
        assert_received(&o, &specs, t30::Modem::V29, &[9600]);
        assert!(
            o.log
                .iter()
                .any(|l| l.contains("repeated: response resent")),
            "{name}: no repeat seen; log {:#?}",
            o.log
        );
        assert!(o.sim_ms > first.sim_ms, "{name}: the lost MCF cost no time");
    }
}

// ===========================================================================================
// M07: ECM

#[test]
fn m07_a_caller_with_ecm_gets_a_non_ecm_call_because_our_dis_has_no_ecm() {
    // ECM is off by default (the DIS does not offer it), also on the audio path: SpanDSP with
    // ECM falls back and the call is the same as without it
    let specs = vec![PageSpec::fine(300), PageSpec::fine(300)];
    let mut opts = Opts::new(specs.clone()).modems(sys::T30_SUPPORT_V29);
    opts.ecm_caller = true;
    let o = call(&opts);
    assert_received(&o, &specs, t30::Modem::V29, &[9600]);
    assert!(!o.result.as_ref().unwrap().ecm, "the call used ECM");
}

/// ECM on the audio path is not implemented, so `AudioEngine` turns it off whatever the
/// configuration says (M5e ⑤): a trunk with `ecm = true` still receives over G.711, without ECM.
#[test]
fn m07_ecm_enabled_for_the_engine_is_turned_off_on_the_audio_path() {
    let specs = vec![PageSpec::fine(300)];
    let mut opts = Opts::new(specs.clone()).modems(sys::T30_SUPPORT_V29);
    opts.ecm_caller = true;
    opts.ecm_engine = true;
    let o = call(&opts);
    assert_received(&o, &specs, t30::Modem::V29, &[9600]);
    assert!(!o.result.as_ref().unwrap().ecm, "the call used ECM");
}

// ===========================================================================================
// M08: the caller does not wait for our CED

#[test]
fn m08_the_caller_sends_cng_all_through_our_ced_and_dis_and_does_not_hear_them() {
    // SpanDSP hears nothing for the first 7 s (it keeps sending CNG over our CED, CSI and DIS and
    // the DIS repeated after T4), then it hears the line again
    let specs = vec![PageSpec::fine(300)];
    let o = call_with(
        &Opts::new(specs.clone()).modems(sys::T30_SUPPORT_V29),
        &mut |_, _, _| false,
        &mut |t, a| {
            if t < 7_000 {
                a.fill(0);
            }
        },
    );
    assert!(
        o.log.iter().any(|l| l.contains("CNG heard")),
        "the engine did not hear the CNG; log {:#?}",
        o.log
    );
    assert_received(&o, &specs, t30::Modem::V29, &[9600]);
}

/// SpanDSP's V.29 TCF (1.5 s of zeros, 9600) at -14 dBm0.
fn spandsp_v29_tcf() -> Vec<i16> {
    struct BitFeed {
        bits: Vec<bool>,
        pos: usize,
    }
    unsafe extern "C" fn get_bit(user: *mut c_void) -> c_int {
        // SAFETY: `user` is the `BitFeed` below, alive for the whole call.
        let feed = unsafe { &mut *(user as *mut BitFeed) };
        match feed.bits.get(feed.pos) {
            Some(&b) => {
                feed.pos += 1;
                c_int::from(b)
            }
            None => sys::SIG_STATUS_END_OF_DATA,
        }
    }
    let mut feed = Box::new(BitFeed {
        bits: vec![false; 14_400],
        pos: 0,
    });
    // SAFETY: NULL makes SpanDSP allocate; `feed` outlives the state.
    let tx = unsafe {
        sys::v29_tx_init(
            std::ptr::null_mut(),
            9600,
            false,
            Some(get_bit),
            &mut *feed as *mut BitFeed as *mut c_void,
        )
    };
    assert!(!tx.is_null());
    let mut out = Vec::new();
    let mut buf = [0i16; 160];
    // SAFETY: valid state, buffer valid for 160 samples.
    unsafe {
        sys::v29_tx_power(tx, -14.0);
        loop {
            let n = sys::v29_tx(tx, buf.as_mut_ptr(), 160) as usize;
            if n == 0 {
                break;
            }
            out.extend_from_slice(&buf[..n]);
        }
        sys::v29_tx_free(tx);
    }
    out
}

/// The peer's TSI and DCS (V.29 9600) as a V.21 burst, with 800 samples of silence before.
fn tsi_dcs_burst() -> Vec<i16> {
    let tsi = vec![0xFF, 0xC0, t30::fcf::TSI, b' ', b' ', b'1'];
    let dcs = vec![0xFF, 0xC8, t30::fcf::DCS, 0x00, 0x20, 0x00];
    let wire: Vec<Vec<u8>> = [tsi, dcs].iter().map(|f| reverse_octets(f)).collect();
    let mut out = vec![0i16; 800];
    out.extend(V21Tx::new(-14.0).modulate(&hdlc::encode_burst(&wire, 38, 2)));
    out
}

/// Runs the engine over `peer` (then silence to `total_ms`); returns the FCFs it sent and its log.
fn run_peer(e: &mut AudioEngine, peer: &[i16], total_ms: u64) -> (Vec<u8>, Vec<String>) {
    let mut sent = Vec::new();
    let mut pos = 0;
    for _ in 0..total_ms / 20 {
        let mut rx = [0i16; 160];
        let n = peer.len().saturating_sub(pos).min(160);
        rx[..n].copy_from_slice(&peer[pos..pos + n]);
        pos += n;
        let mut out = [0i16; 160];
        e.process(&rx, &mut out);
        sent.extend_from_slice(&out);
    }
    let fcfs = bursts(&sent)
        .iter()
        .flat_map(|b| frames_in(&sent[b.clone()]))
        .map(|f| f[2])
        .collect();
    (fcfs, e.drain_log())
}

/// A peer that does not wait for our CED and DIS: it starts with TSI and DCS 500 ms after the
/// answer, in the middle of our CED. We do not listen while we send (half duplex, also the CED), so
/// that DCS is not heard, and nothing answers it; the same peer repeats its TSI and DCS after our DIS
/// (T.30 has it answer a DIS) and the call goes on: CFR for the TCF.
#[test]
fn m08_a_dcs_in_the_middle_of_our_ced_is_lost_but_the_call_recovers_on_the_repeat() {
    let mut e = AudioEngine::new(AudioConfig::default());
    e.start();
    // 4000 samples of silence, then the burst at 500 ms: in the CED
    let mut early = vec![0i16; 4_000];
    early.extend(tsi_dcs_burst());
    let (f1, log1) = run_peer(&mut e, &early, 3_000);
    eprintln!("M08 early DCS: frames {f1:02x?}; log {log1:#?}");
    assert!(
        !f1.contains(&t30::fcf::CFR) && !f1.contains(&t30::fcf::FTT),
        "the DCS inside the CED was answered: {f1:02x?}"
    );
    assert!(
        !log1.iter().any(|l| l.contains("receiver ready")),
        "a receiver was made for the DCS inside our CED: {log1:#?}"
    );
    // our DIS goes out after the CED; the peer answers it, as soon as it is over, with TSI, DCS
    // and the TCF
    let mut f2: Vec<u8> = Vec::new();
    let mut sent = Vec::new();
    for _ in 0..600 {
        let mut out = [0i16; 160];
        e.process(&[0i16; 160], &mut out);
        sent.extend_from_slice(&out);
        // every 100 ms: is there a whole DIS burst, followed by 100 ms of silence?
        if sent.len() % 800 == 0
            && sent[sent.len().saturating_sub(800)..]
                .iter()
                .all(|&x| x == 0)
        {
            f2 = bursts(&sent)
                .iter()
                .flat_map(|b| frames_in(&sent[b.clone()]))
                .map(|f| f[2])
                .collect();
            if f2.contains(&t30::fcf::DIS) {
                break;
            }
        }
    }
    assert!(
        f2.contains(&t30::fcf::DIS),
        "no DIS after the CED: {f2:02x?}"
    );
    let mut late = tsi_dcs_burst();
    late.extend(vec![0i16; 1_200]);
    late.extend(spandsp_v29_tcf());
    let (f3, log3) = run_peer(&mut e, &late, (late.len() / 160 + 300) as u64 * 20);
    eprintln!("M08 DCS after the DIS: frames {f3:02x?}; log {log3:#?}");
    assert!(
        f3.contains(&t30::fcf::CFR) && !f3.contains(&t30::fcf::FTT),
        "frames {f3:02x?}; log {log3:#?}"
    );
}

// ===========================================================================================
// E04 (in process): what packet loss does to a call

/// One 20 ms block lost (and the engine told so) at every offset 0..1200 ms after the engine
/// entered `RxImage` for the first time: the page training and the first rows of the image are in
/// this window. Every call completes, within the T.30 bad-line threshold.
#[test]
fn m05b_one_lost_block_at_any_offset_after_rx_image_is_survived() {
    for off in (0..=1200u64).step_by(20) {
        let specs = vec![PageSpec::fine(300), PageSpec::fine(300)];
        let mut entered: Option<u64> = None;
        let o = call_with(
            &Opts::new(specs.clone()).modems(sys::T30_SUPPORT_V29),
            &mut |e, t, _| {
                if entered.is_none() && e.state() == t30::State::RxImage {
                    entered = Some(t);
                }
                entered.is_some_and(|t0| t == t0 + off)
            },
            &mut |_, _| {},
        );
        assert!(entered.is_some(), "never in RxImage");
        assert!(o.engine_done, "offset {off}: log {:#?}", o.log);
        assert_eq!(o.caller_code, Some(sys::T30_ERR_OK), "offset {off}");
        assert_eq!(o.pages.len(), 2, "offset {off}");
        for (i, p) in o.pages.iter().enumerate() {
            assert!(
                u64::from(p.bad_lines) * 100 <= u64::from(specs[i].height) * 10,
                "offset {off} page {i}: {} bad lines",
                p.bad_lines
            );
        }
        // the page that lost a block is the first one; the second is exact
        assert_page_exact(&o, 1, &specs[1]);
    }
}

/// Random loss of 0.5 % of the blocks in both directions (a fixed PRNG per seed), two fine pages
/// at V.29, 40 seeds. Recorded: how often the call fails, with ours as the receiver and with
/// SpanDSP's own receiver (the same loss, SpanDSP to SpanDSP). An audio channel has no error
/// correction: a V.21 frame or a training sequence that is hit has to be repeated by the peer, and
/// the peers give up after a few tries, so not every call can survive. The test asserts that ours
/// fails no more often than SpanDSP's own receiver, and that every call ends.
#[test]
fn e04c_random_loss_half_percent_ours_is_no_worse_than_spandsp() {
    let seeds = 40u64;
    let mut ours_fail = Vec::new();
    for seed in 0..seeds {
        let specs = vec![PageSpec::fine(300), PageSpec::fine(300)];
        let mut rng_up = SplitMix64::new(0xE04_0000 + seed);
        let mut rng_down = SplitMix64::new(0xE04_8000 + seed);
        let o = call_with(
            &Opts::new(specs.clone()).modems(sys::T30_SUPPORT_V29),
            &mut |_, _, _| rng_up.next_permille() < 5,
            &mut |_, a| {
                if rng_down.next_permille() < 5 {
                    a.fill(0);
                }
            },
        );
        assert!(o.engine_done && o.sim_ms < 900_000, "seed {seed}: no end");
        let ok = o.caller_code == Some(sys::T30_ERR_OK)
            && o.result.as_ref().is_some_and(|r| r.code.is_ok());
        if ok {
            assert_eq!(o.pages.len(), 2, "seed {seed}");
            for (i, p) in o.pages.iter().enumerate() {
                assert!(
                    u64::from(p.bad_lines) * 100 <= u64::from(specs[i].height) * 10,
                    "seed {seed} page {i}: {} bad lines",
                    p.bad_lines
                );
            }
        } else {
            ours_fail.push((
                seed,
                o.result.as_ref().map(|r| r.code.name()),
                o.caller_code,
                o.pages.len(),
            ));
        }
    }
    // the control: SpanDSP's own receiver
    let mut ctl_fail = 0;
    for seed in 0..seeds {
        let specs = vec![PageSpec::fine(300), PageSpec::fine(300)];
        let tif = tmp("ctl-pages.tif");
        tiff_writer::write_file(&tif, &specs).unwrap();
        let rxf = tmp("ctl-rx.tif");
        let mut caller = FaxModem::new(
            true,
            &T30Config {
                tx_file: Some(&tif),
                rx_file: None,
                ecm: false,
                ident: "0000000001",
                modems: Some(sys::T30_SUPPORT_V29),
                ..T30Config::default()
            },
        )
        .unwrap();
        let mut answerer = FaxModem::new(
            false,
            &T30Config {
                tx_file: None,
                rx_file: Some(&rxf),
                ecm: false,
                ident: "0000000002",
                modems: Some(sys::T30_SUPPORT_V29),
                ..T30Config::default()
            },
        )
        .unwrap();
        let mut r1 = SplitMix64::new(0xE04_0000 + seed);
        let mut r2 = SplitMix64::new(0xE04_8000 + seed);
        let mut t = 0u64;
        while t < 900_000
            && !(caller.completion_code().is_some() && answerer.completion_code().is_some())
        {
            let mut to_answerer = [0i16; 160];
            let mut to_caller = [0i16; 160];
            caller.tx(&mut to_answerer);
            answerer.tx(&mut to_caller);
            if r1.next_permille() < 5 {
                to_answerer.fill(0);
            }
            if r2.next_permille() < 5 {
                to_caller.fill(0);
            }
            answerer.rx(&mut to_answerer);
            caller.rx(&mut to_caller);
            t += 20;
        }
        if !(caller.completion_code() == Some(0) && answerer.completion_code() == Some(0)) {
            ctl_fail += 1;
        }
    }
    eprintln!(
        "E04c: 0.5 % loss each way, {seeds} seeds: ours failed {} {:?}; SpanDSP to SpanDSP failed {ctl_fail}",
        ours_fail.len(),
        ours_fail
    );
    assert!(
        ours_fail.len() <= ctl_fail,
        "ours failed {} of {seeds}, SpanDSP's own receiver {ctl_fail}",
        ours_fail.len()
    );
}
