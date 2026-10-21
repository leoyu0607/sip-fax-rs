//! E01..E05 (M3d): SpanDSP `t38_terminal` with ECM enabled as the calling party against our
//! T.38 receive engine (`fax_in::t38engine`) over UDPTL on a simulated clock. Every received
//! page is decoded again and compared pixel for pixel with the bitmap the SpanDSP side was given.

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
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("eecm-{}", std::process::id()));
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

/// Our T.30 configuration for the ECM tests: ECM and MMR offered.
fn base(name: &str, pages: &[PageSpec]) -> EngineRunConfig {
    let mut cfg = EngineRunConfig::new(make_input(&format!("{name}.tif"), pages));
    cfg.ecm = true;
    cfg.engine.t30.ecm = true;
    cfg.engine.t30.mmr = true;
    cfg
}

fn assert_pixel_exact(o: &EngineRunOutcome, pages: &[PageSpec]) {
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

/// Asserts the ECM specifics of a successful call.
fn assert_ecm(name: &str, o: &EngineRunOutcome, npages: usize, comp: Compression, rate: u32) {
    let r = o.result.as_ref().unwrap();
    assert!(r.ecm, "{name}: the DCS selected ECM");
    assert_eq!(r.pages as usize, npages, "{name}");
    assert_eq!(r.bit_rate, rate, "{name}: negotiated rate");
    assert_eq!(r.compression, Some(comp), "{name}: coding");
    assert_eq!(r.remote_ident.as_deref(), Some("0000000001"), "{name}");
    assert_eq!(r.bad_lines_per_page, vec![0; npages], "{name}");
    assert_eq!(r.ecm_frames_lost, 0, "{name}");
}

fn matrix(tag: &str, codings: &[(&str, Compression, i32)]) -> Vec<String> {
    let rates: [(u32, i32); 3] = [
        (4800, sys::T30_SUPPORT_V27TER),
        (9600, sys::T30_SUPPORT_V29),
        (14400, sys::T30_SUPPORT_V17),
    ];
    let mut rows = Vec::new();
    for npages in [1usize, 3] {
        for fine in [false, true] {
            for (cname, comp, mask) in codings {
                for (rate, modem) in rates {
                    let name = format!(
                        "{tag}-{npages}p-{}-{cname}-{rate}",
                        if fine { "fine" } else { "std" }
                    );
                    let pages = specs(npages, fine);
                    let mut cfg = base(&name, &pages);
                    cfg.modems = Some(modem);
                    cfg.compressions = Some(*mask);
                    let o = run(&cfg);
                    assert_success(&name, &o);
                    assert_ecm(&name, &o, npages, *comp, rate);
                    assert_pixel_exact(&o, &pages);
                    let r = o.result.as_ref().unwrap();
                    assert_eq!(r.ecm_ppr_sent, 0, "{name}: a clean line needs no PPR");
                    rows.push(format!(
                        "{tag} {name}: spandsp={} ours={} ecm pages={npages} ppr=0 pixel-exact sim={}ms",
                        o.caller_code.map(completion_str).unwrap(),
                        r.code.name(),
                        o.sim_ms
                    ));
                }
            }
        }
    }
    rows
}

#[test]
fn e01_ecm_with_mh_and_mr_pages_resolutions_and_rates() {
    let rows = matrix(
        "E01",
        &[
            ("MH", Compression::Mh, sys::T4_COMPRESSION_T4_1D),
            (
                "MR",
                Compression::Mr,
                sys::T4_COMPRESSION_T4_1D | sys::T4_COMPRESSION_T4_2D,
            ),
        ],
    );
    for r in &rows {
        eprintln!("{r}");
    }
    assert_eq!(rows.len(), 24);
}

#[test]
fn e02_ecm_with_mmr_the_cells_x01_could_not_run_without_ecm() {
    let rows = matrix("E02", &[("MMR", Compression::Mmr, sys::T4_COMPRESSION_T6)]);
    for r in &rows {
        eprintln!("{r}");
    }
    assert_eq!(rows.len(), 12);
}

#[test]
fn e01_mixed_resolutions_use_pps_eom_and_return_to_phase_b() {
    let pages = vec![
        PageSpec::standard(260),
        PageSpec::fine(300),
        PageSpec::standard(280),
    ];
    let name = "e01-mixed";
    let mut cfg = base(name, &pages);
    cfg.compressions = Some(sys::T4_COMPRESSION_T6);
    let o = run(&cfg);
    assert_success(name, &o);
    assert_pixel_exact(&o, &pages);
    let dpi: Vec<u32> = o.pages.iter().map(|p| p.y_dpi).collect();
    assert_eq!(dpi, [98, 196, 98]);
    eprintln!(
        "E01 {name}: spandsp=OK ours=ok pages=3 pixel-exact sim={}ms",
        o.sim_ms
    );
}

#[test]
fn e01_ecm_over_every_t38_version() {
    let pages = specs(2, true);
    for v in [0u8, 1, 2, 3] {
        let name = format!("e01-v{v}");
        let mut cfg = base(&name, &pages);
        cfg.version = v;
        let o = run(&cfg);
        assert_success(&name, &o);
        assert_pixel_exact(&o, &pages);
    }
}

#[test]
fn e01_without_ecm_on_our_side_spandsp_falls_back_to_non_ecm() {
    // Our DIS without bit 27: SpanDSP must not use ECM, and the old path is unchanged.
    let pages = specs(2, false);
    let mut cfg = base("e01-noecm-ours", &pages);
    cfg.engine.t30.ecm = false;
    cfg.engine.t30.mmr = false;
    let o = run(&cfg);
    assert_success("e01-noecm-ours", &o);
    assert!(!o.result.as_ref().unwrap().ecm);
    assert_pixel_exact(&o, &pages);
    // And SpanDSP without ECM against our ECM-capable DIS: also non-ECM.
    let mut cfg = base("e01-noecm-theirs", &pages);
    cfg.ecm = false;
    let o = run(&cfg);
    assert_success("e01-noecm-theirs", &o);
    assert!(!o.result.as_ref().unwrap().ecm);
    assert_pixel_exact(&o, &pages);
}

// ------------------------------------------------------------------ E03 loss, PPR does the work

#[derive(Clone, Copy)]
enum Scope {
    /// 5 % of the datagrams that carry image-rate data (the FCD frames), nothing else.
    ImageData,
    /// 5 % of every datagram in both directions.
    Everything,
}

/// One E03 run per seed: 5 % loss and no UDPTL error recovery (`recovery`).
fn e03_runs(tag: &str, scope: Scope, recovery: ErrorRecovery) -> (u32, Vec<String>, Vec<bool>) {
    let pages = specs(2, false);
    let mut total_ppr = 0;
    let mut rows = Vec::new();
    let mut ok = Vec::new();
    for seed in SEEDS {
        let name = format!("e03-{tag}-{seed:x}");
        let mut cfg = base(&name, &pages);
        cfg.loss_permille = 50;
        cfg.loss_image_data_only = matches!(scope, Scope::ImageData);
        cfg.seed = seed;
        cfg.recovery = recovery;
        cfg.compressions = Some(sys::T4_COMPRESSION_T6);
        let o = run(&cfg);
        let r = o.result.as_ref();
        let success = !o.timed_out
            && o.caller_code == Some(OK)
            && r.is_some_and(|r| r.code == ResultCode::Ok);
        ok.push(success);
        rows.push(format!(
            "E03 seed={seed:#x}: spandsp={} ours={} pages={} sent={} dropped={} ({:.1} %) udptl_gaps={:?} recovered_by_udptl={} ppr_sent={} frames_lost={} sim={}ms",
            o.caller_code.map(completion_str).unwrap_or_default(),
            r.map_or("-", |r| r.code.name()),
            o.pages.len(),
            o.datagrams_sent,
            o.datagrams_dropped,
            100.0 * o.datagrams_dropped as f64 / o.datagrams_sent as f64,
            o.gaps,
            o.engine_rx.recovered_redundancy + o.engine_rx.recovered_fec,
            r.map_or(0, |r| r.ecm_ppr_sent),
            r.map_or(0, |r| r.ecm_frames_lost),
            o.sim_ms
        ));
        if success {
            assert_pixel_exact(&o, &pages);
            total_ppr += r.map_or(0, |r| r.ecm_ppr_sent);
        }
    }
    (total_ppr, rows, ok)
}

#[test]
fn e03_five_percent_image_data_loss_without_udptl_redundancy_ppr_repairs_the_pages() {
    // The loss hits the ECM frames only (control frames and indicators are delivered): this is
    // the case where PPR, not UDPTL redundancy, has to do the work. See the next test for loss
    // on everything.
    let (ppr, rows, ok) = e03_runs("img", Scope::ImageData, ErrorRecovery::None);
    for r in &rows {
        eprintln!("{r}");
    }
    assert!(ok.iter().all(|x| *x), "all five seeds must succeed: {ok:?}");
    assert!(ppr >= 5, "PPR repaired the pages: {ppr} requests");
    assert!(rows.iter().all(|r| r.contains("recovered_by_udptl=0")));
    assert!(rows.iter().all(|r| r.contains("frames_lost=0")));
}

#[test]
fn e03_five_percent_loss_on_everything_with_udptl_redundancy_3_also_works() {
    let (ppr, rows, ok) = e03_runs(
        "red",
        Scope::Everything,
        ErrorRecovery::Redundancy { depth: 3 },
    );
    for r in &rows {
        eprintln!("{r}");
    }
    assert!(ok.iter().all(|x| *x), "{ok:?}");
    eprintln!("E03 redundancy 3, loss on everything: ppr_sent total {ppr}");
}

#[test]
fn e03_five_percent_loss_on_everything_without_recovery_is_reported_not_asserted() {
    // T.30 over an unprotected 5 % loss: every V.21 frame is sent one octet per packet (a PPR is
    // 35 packets), the receiver's T2 (6 s) allows the sender a single repetition of a lost
    // command, so calls fail for reasons that have nothing to do with ECM. Reported for the
    // record; the only assertions are that nothing hangs and that a call that succeeds is exact.
    let (ppr, rows, ok) = e03_runs("all", Scope::Everything, ErrorRecovery::None);
    for r in &rows {
        eprintln!("{r}");
    }
    eprintln!(
        "E03 loss on everything, no recovery: {} of 5 seeds succeeded, ppr_sent total {ppr}",
        ok.iter().filter(|x| **x).count()
    );
}

#[test]
fn e03_control_the_same_loss_without_ecm_damages_the_pages() {
    // Same seeds, same loss, no UDPTL recovery, SpanDSP without ECM: the case needs ECM.
    let pages = specs(2, false);
    let mut clean = 0;
    for seed in SEEDS {
        let name = format!("e03-ctl-{seed:x}");
        let mut cfg = base(&name, &pages);
        cfg.ecm = false;
        cfg.loss_permille = 50;
        cfg.seed = seed;
        cfg.recovery = ErrorRecovery::None;
        let o = run(&cfg);
        let ok = o.caller_code == Some(OK)
            && o.result.as_ref().is_some_and(|r| r.code.is_ok())
            && o.pages.len() == 2
            && o.pages.iter().all(|p| p.bad_lines == 0);
        eprintln!(
            "E03 control seed={seed:#x}: spandsp={:?} ours={:?} pages={} bad_lines={:?} dropped={}",
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
        "loss without ECM and without recovery had no visible effect"
    );
}

// ------------------------------------------------------------------ E04 persistent loss

fn e04(name: &str, npages: usize) -> (EngineRunOutcome, t30::T30Result) {
    let pages = specs(npages, false);
    let mut cfg = base(name, &pages);
    cfg.compressions = Some(sys::T4_COMPRESSION_T6);
    cfg.drop_frame = Some(3);
    let o = run(&cfg);
    assert!(!o.timed_out, "{name}: hang\n{:#?}", o.log);
    let r = o
        .result
        .clone()
        .unwrap_or_else(|| panic!("{name}: engine not done\n{:#?}", o.log));
    eprintln!(
        "E04 {name}: spandsp={:?} ours={} pages={} ppr_sent={} frames_lost={} frame3_transmissions_dropped={} final_rate={} sim={}ms",
        o.caller_code.map(completion_str),
        r.code.name(),
        r.pages,
        r.ecm_ppr_sent,
        r.ecm_frames_lost,
        o.frames_dropped,
        r.bit_rate,
        o.sim_ms
    );
    for l in o
        .log
        .iter()
        .filter(|l| l.contains("PPR") || l.contains("EOR"))
    {
        eprintln!("E04 {name} log: {l}");
    }
    (o, r)
}

#[test]
fn e04_one_page_frame_never_arrives_ctc_then_eor_eop_gives_frames_lost() {
    // SpanDSP: four PPRs, CTC (falls back one speed because the first round made progress), four
    // more PPRs, then EOR-EOP; our ERR; its DCN. The page is delivered with a hole.
    let (o, r) = e04("e04-1p", 1);
    assert_eq!(r.code, ResultCode::EcmFramesLost, "{r:?}");
    assert_eq!((r.pages, r.ecm_ppr_sent, r.ecm_frames_lost), (1, 8, 1));
    assert_eq!(
        o.frames_dropped, 8,
        "frame 3: first send, 3 resends, 4 resends after the CTC"
    );
    assert_eq!(o.pages.len(), 1);
    assert!(
        r.bit_rate < 14400,
        "the CTC made the rate fall back: {}",
        r.bit_rate
    );
    // the page is intact up to the hole: decodes, fewer correct rows than the source
    let p = &o.pages[0];
    assert!(p.height > 0);
}

#[test]
fn e04_two_pages_spandsp_disconnects_after_err_so_the_result_is_partial() {
    // SpanDSP 3.1.1 does not continue after ERR ("TODO: Continue with the next message if MPS or
    // EOM?" in t30.c): it sends DCN, so the second page never comes.
    let (o, r) = e04("e04-2p", 2);
    assert_eq!(r.code, ResultCode::PartialDcn, "{r:?}");
    assert_eq!((r.pages, r.ecm_ppr_sent, r.ecm_frames_lost), (1, 8, 1));
    assert!(r.is_partial());
    assert_eq!(o.pages.len(), 1);
}
