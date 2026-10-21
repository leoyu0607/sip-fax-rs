//! X01..X05 (M2c): SpanDSP `t38_terminal` as the calling party against our T.38 receive engine
//! (`fax_in::t38engine`) over UDPTL on a simulated clock. Every received page is decoded again
//! and compared pixel for pixel with the bitmap the SpanDSP side was given.

use fax_sim::engine_loopback::{EngineRunConfig, EngineRunOutcome, run};
use fax_sim::spandsp::completion_str;
use fax_sim::tiff_writer::{self, PageSpec, render_bitmap};
use spandsp_sys as sys;
use std::path::PathBuf;
use t30::{Compression, ResultCode};
use t38::ErrorRecovery;

const OK: i32 = 0;
const SEEDS: [u64; 5] = [
    0x5EED_0001,
    0x5EED_0002,
    0x5EED_0003,
    0x5EED_0004,
    0x5EED_0005,
];

fn tmp(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("xint-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

fn specs(n: usize, fine: bool) -> Vec<PageSpec> {
    (0..n)
        .map(|i| {
            let h = 260 + 40 * i as u32;
            if fine {
                PageSpec::fine(h)
            } else {
                PageSpec::standard(h)
            }
        })
        .collect()
}

fn make_input(name: &str, pages: &[PageSpec]) -> PathBuf {
    let p = tmp(name);
    tiff_writer::write_file(&p, pages).unwrap();
    p
}

/// Decodes every received page (MMR) and compares it with the source bitmap. Returns the
/// number of pages compared.
fn assert_pixel_exact(o: &EngineRunOutcome, pages: &[PageSpec]) -> usize {
    assert_eq!(o.pages.len(), pages.len(), "page count");
    for (i, (got, spec)) in o.pages.iter().zip(pages).enumerate() {
        let want = render_bitmap(spec, i as u32);
        assert_eq!(
            (got.width, got.height),
            (spec.width, spec.height),
            "page {i} size"
        );
        assert_eq!(got.y_dpi, spec.y_dpi, "page {i} y dpi");
        let dec = t4::decode_page(&got.g4, t4::DecoderConfig::new(got.width, t4::Coding::Mmr))
            .unwrap_or_else(|e| panic!("page {i} g4 undecodable: {e}"));
        assert_eq!(dec.summary.stats.rows, spec.height, "page {i} rows");
        assert_eq!(dec.summary.stats.bad_lines, 0);
        let diff = dec.bitmap.iter().zip(&want).filter(|(a, b)| a != b).count();
        assert_eq!(diff, 0, "page {i}: {diff} differing octets");
        assert_eq!(dec.bitmap.len(), want.len());
    }
    pages.len()
}

fn assert_success(name: &str, o: &EngineRunOutcome) {
    assert!(!o.timed_out, "{name}: timed out\n{:#?}", o.log);
    assert_eq!(
        o.caller_code,
        Some(OK),
        "{name}: SpanDSP says {:?} ({})\nengine: {:?}\n{:#?}",
        o.caller_code,
        o.caller_code.map(completion_str).unwrap_or_default(),
        o.result,
        o.log
    );
    let r = o
        .result
        .as_ref()
        .unwrap_or_else(|| panic!("{name}: engine not done"));
    assert_eq!(r.code, ResultCode::Ok, "{name}: {r:?}\n{:#?}", o.log);
}

fn base(name: &str, pages: &[PageSpec]) -> EngineRunConfig {
    EngineRunConfig::new(make_input(&format!("{name}.tif"), pages))
}

// ------------------------------------------------------------------ X01 matrix

#[test]
fn x01_matrix_pages_resolution_coding_rate() {
    let rates: [(u32, i32); 3] = [
        (4800, sys::T30_SUPPORT_V27TER),
        (9600, sys::T30_SUPPORT_V29),
        (14400, sys::T30_SUPPORT_V17),
    ];
    let codings: [(&str, Compression, i32); 2] = [
        ("MH", Compression::Mh, sys::T4_COMPRESSION_T4_1D),
        (
            "MR",
            Compression::Mr,
            sys::T4_COMPRESSION_T4_1D | sys::T4_COMPRESSION_T4_2D,
        ),
    ];
    let mut rows = Vec::new();
    for npages in [1usize, 3] {
        for fine in [false, true] {
            for (cname, comp, mask) in codings {
                for (rate, modem) in rates {
                    let name = format!(
                        "x01-{npages}p-{}-{cname}-{rate}",
                        if fine { "fine" } else { "std" }
                    );
                    let pages = specs(npages, fine);
                    let mut cfg = base(&name, &pages);
                    cfg.modems = Some(modem);
                    cfg.compressions = Some(mask);
                    let o = run(&cfg);
                    assert_success(&name, &o);
                    let r = o.result.as_ref().unwrap();
                    assert_eq!(r.pages as usize, npages, "{name}");
                    assert_eq!(r.bit_rate, rate, "{name}: negotiated rate");
                    assert_eq!(r.compression, Some(comp), "{name}: coding");
                    assert_eq!(r.remote_ident.as_deref(), Some("0000000001"), "{name}");
                    assert_eq!(r.bad_lines_per_page, vec![0; npages], "{name}");
                    for p in &o.pages {
                        assert_eq!(p.y_dpi, if fine { 196 } else { 98 }, "{name}");
                    }
                    let n = assert_pixel_exact(&o, &pages);
                    rows.push(format!(
                        "X01 {name}: spandsp={} ours={} pages={n} pixel-exact sim={}ms",
                        o.caller_code.map(completion_str).unwrap(),
                        r.code.name(),
                        o.sim_ms
                    ));
                }
            }
        }
    }
    for r in &rows {
        eprintln!("{r}");
    }
    assert_eq!(rows.len(), 24);
}

#[test]
fn x01_forcing_mmr_in_spandsp_falls_back_to_mh_in_non_ecm() {
    // T.30 only allows T.6 together with ECM. SpanDSP enforces that (`mutual_compressions` loses
    // T.6 without ECM) and also keeps MH available whatever mask is given, so "forced MMR"
    // cannot be exercised over non-ECM: the call succeeds with MH, whether or not our DIS
    // (wrongly, without ECM) offers T.6. MMR decoding is covered by the engine's R16 test
    // with a hand-made DCS, and end to end with SpanDSP once ECM exists (M3).
    for (name, offer_mmr) in [("x01-mmr-ours-off", false), ("x01-mmr-ours-on", true)] {
        let pages = specs(1, false);
        let mut cfg = base(name, &pages);
        cfg.compressions = Some(sys::T4_COMPRESSION_T6);
        cfg.engine.t30.mmr = offer_mmr;
        let o = run(&cfg);
        assert_success(name, &o);
        let r = o.result.as_ref().unwrap();
        assert_eq!(r.compression, Some(Compression::Mh), "{name}");
        assert_pixel_exact(&o, &pages);
        eprintln!(
            "X01 {name}: spandsp=OK ours=ok coding={:?} (MMR requested, MH used) pixel-exact",
            r.compression
        );
    }
}

#[test]
fn x01_mixed_resolution_pages_use_eom_and_return_to_phase_b() {
    // SpanDSP signals EOM when the next page needs a different DCS; the engine must answer MCF
    // and send a fresh DIS.
    let pages = vec![
        PageSpec::standard(260),
        PageSpec::fine(300),
        PageSpec::standard(280),
    ];
    let name = "x01-mixed";
    let o = run(&base(name, &pages));
    assert_success(name, &o);
    assert_eq!(assert_pixel_exact(&o, &pages), 3);
    let dpi: Vec<u32> = o.pages.iter().map(|p| p.y_dpi).collect();
    assert_eq!(dpi, [98, 196, 98]);
    eprintln!(
        "X01 {name}: spandsp=OK ours=ok pages=3 pixel-exact sim={}ms",
        o.sim_ms
    );
}

// ------------------------------------------------------------------ X02 loss

#[test]
fn x02_five_percent_loss_redundancy_3_five_seeds() {
    let pages = specs(2, false);
    for seed in SEEDS {
        let name = format!("x02-{seed:x}");
        let mut cfg = base(&name, &pages);
        cfg.loss_permille = 50;
        cfg.seed = seed;
        cfg.recovery = ErrorRecovery::Redundancy { depth: 3 };
        let o = run(&cfg);
        assert_success(&name, &o);
        assert_pixel_exact(&o, &pages);
        assert!(o.datagrams_dropped > 0, "{name}: loss was injected");
        assert_eq!(o.gaps, (0, 0), "{name}: redundancy repaired everything");
        eprintln!(
            "X02 seed={seed:#x}: spandsp=OK ours=ok sent={} dropped={} ({:.1} %) recovered(engine side)={} recovered(caller side)={} gaps={:?} pixel-exact",
            o.datagrams_sent,
            o.datagrams_dropped,
            100.0 * o.datagrams_dropped as f64 / o.datagrams_sent as f64,
            o.engine_rx.recovered_redundancy,
            o.caller_rx.recovered_redundancy,
            o.gaps
        );
    }
}

#[test]
fn x02_control_without_recovery_the_same_loss_breaks_calls() {
    // The scenario needs the redundancy: with no error recovery at all the same seeds must not
    // all come out pixel exact and successful.
    let pages = specs(2, false);
    let mut clean = 0;
    for seed in SEEDS {
        let name = format!("x02-ctl-{seed:x}");
        let mut cfg = base(&name, &pages);
        cfg.loss_permille = 50;
        cfg.seed = seed;
        cfg.recovery = ErrorRecovery::None;
        let o = run(&cfg);
        let ok = o.caller_code == Some(OK)
            && o.result.as_ref().is_some_and(|r| r.code.is_ok())
            && o.pages.len() == 2
            && o.pages.iter().all(|p| p.bad_lines == 0);
        eprintln!(
            "X02 control seed={seed:#x}: spandsp={:?} ours={:?} pages={} bad_lines={:?} dropped={}",
            o.caller_code,
            o.result.as_ref().map(|r| r.code.name()),
            o.pages.len(),
            o.pages.iter().map(|p| p.bad_lines).collect::<Vec<_>>(),
            o.datagrams_dropped
        );
        if ok {
            clean += 1;
        }
    }
    assert!(
        clean < SEEDS.len(),
        "loss without recovery had no visible effect"
    );
}

#[test]
fn x02_fec_recovery_also_works() {
    let pages = specs(1, false);
    for seed in SEEDS {
        let name = format!("x02-fec-{seed:x}");
        let mut cfg = base(&name, &pages);
        cfg.loss_permille = 30;
        cfg.seed = seed;
        cfg.recovery = ErrorRecovery::Fec {
            span: 3,
            entries: 3,
        };
        let o = run(&cfg);
        assert_success(&name, &o);
        assert_pixel_exact(&o, &pages);
    }
}

// ------------------------------------------------------------------ X03 TCF modes

#[test]
fn x03_transferred_tcf_and_local_tcf() {
    let pages = specs(2, false);
    // transferredTCF: the default of both sides
    let o = run(&base("x03-transferred", &pages));
    assert_success("x03-transferred", &o);
    assert_pixel_exact(&o, &pages);
    let t_ms = o.sim_ms;
    // localTCF: SpanDSP skips the TCF (NO_TCF) and we answer the DCS with CFR at once.
    let mut cfg = base("x03-local", &pages);
    cfg.iaf = Some(sys::T30_IAF_MODE_T38 | sys::T30_IAF_MODE_NO_TCF);
    cfg.engine.t30.tcf = t30::TcfMode::Local;
    let o = run(&cfg);
    assert_success("x03-local", &o);
    assert_pixel_exact(&o, &pages);
    assert!(
        o.sim_ms < t_ms,
        "no TCF saves time: {} vs {}",
        o.sim_ms,
        t_ms
    );
    eprintln!(
        "X03 transferredTCF sim={t_ms}ms, localTCF sim={}ms: both OK pixel-exact",
        o.sim_ms
    );
}

#[test]
fn x03_tcf_mismatch_is_survivable_or_fails_cleanly() {
    // SpanDSP sends no TCF but we wait for one: we time out waiting (T2) and answer FTT; SpanDSP
    // has started the page by then. The point of the case: nothing hangs.
    let pages = specs(1, false);
    let mut cfg = base("x03-mismatch", &pages);
    cfg.iaf = Some(sys::T30_IAF_MODE_T38 | sys::T30_IAF_MODE_NO_TCF);
    let o = run(&cfg);
    assert!(!o.timed_out, "{:#?}", o.log);
    eprintln!(
        "X03 mismatch: spandsp={:?} ours={:?} sim={}ms",
        o.caller_code.map(completion_str),
        o.result.as_ref().map(|r| (r.code.name(), r.pages)),
        o.sim_ms
    );
}

// ------------------------------------------------------------------ X04 versions

#[test]
fn x04_t38_versions() {
    let pages = specs(2, true);
    for v in [0u8, 3, 1, 2] {
        let name = format!("x04-v{v}");
        let mut cfg = base(&name, &pages);
        cfg.version = v;
        let o = run(&cfg);
        assert_success(&name, &o);
        assert_pixel_exact(&o, &pages);
        eprintln!(
            "X04 T.38 v{v}: spandsp=OK ours=ok pixel-exact sim={}ms",
            o.sim_ms
        );
    }
}

// ------------------------------------------------------------------ X05 failures

#[test]
fn x05_missing_tx_file() {
    let cfg = EngineRunConfig::new(tmp("does-not-exist.tif"));
    let o = run(&cfg);
    assert!(!o.timed_out, "{:#?}", o.log);
    let sd = o.caller_code.expect("spandsp finished");
    let r = o.result.as_ref().expect("engine finished");
    eprintln!(
        "X05 missing file: spandsp={sd} ({}) ours={} pages={} sim={}ms",
        completion_str(sd),
        r.code.name(),
        r.pages,
        o.sim_ms
    );
    assert_ne!(sd, OK);
    assert_ne!(r.code, ResultCode::Ok);
    assert_eq!(r.pages, 0);
    assert!(o.pages.is_empty());
}

#[test]
fn x05_caller_hangs_up_in_the_middle_of_the_call() {
    let pages = specs(3, false);
    let mut results = Vec::new();
    // early (during training), and later (between/inside pages)
    for at in [9_000u64, 18_000, 30_000] {
        let name = format!("x05-hangup-{at}");
        let mut cfg = base(&name, &pages);
        cfg.caller_hangup_at_ms = Some(at);
        let o = run(&cfg);
        assert!(!o.timed_out, "{name}: hung\n{:#?}", o.log);
        let r = o
            .result
            .as_ref()
            .unwrap_or_else(|| panic!("{name}: engine never finished"));
        let sd = o.caller_code.expect("spandsp finished");
        eprintln!(
            "X05 hangup@{at}ms: spandsp={sd} ({}) ours={} pages={} (kept {}) sim={}ms",
            completion_str(sd),
            r.code.name(),
            r.pages,
            o.pages.len(),
            o.sim_ms
        );
        assert_ne!(r.code, ResultCode::Ok, "{name}");
        assert_eq!(
            r.pages as usize,
            o.pages.len(),
            "{name}: pages reported = pages kept"
        );
        // pages that were delivered are pixel exact
        for (i, p) in o.pages.iter().enumerate() {
            let want = render_bitmap(&pages[i], i as u32);
            let dec =
                t4::decode_page(&p.g4, t4::DecoderConfig::new(p.width, t4::Coding::Mmr)).unwrap();
            assert_eq!(dec.bitmap, want, "{name} page {i}");
        }
        results.push((at, r.code, r.pages));
    }
    assert!(
        results.iter().any(|(_, _, p)| *p > 0),
        "late hangup should keep pages: {results:?}"
    );
}

#[test]
fn x05_peer_silence_after_the_dis_fails_after_the_retries() {
    // A "caller" that never answers is the engine's own R07 scenario at the interop level:
    // run with a tx file but kill SpanDSP before it can answer.
    let pages = specs(1, false);
    let mut cfg = base("x05-silent", &pages);
    cfg.caller_hangup_at_ms = Some(0);
    let o = run(&cfg);
    assert!(!o.timed_out);
    let r = o.result.as_ref().unwrap();
    eprintln!(
        "X05 silent peer: ours={} sim={}ms spandsp={:?}",
        r.code.name(),
        o.sim_ms,
        o.caller_code
    );
    assert_ne!(r.code, ResultCode::Ok);
}
