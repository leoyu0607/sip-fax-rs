//! X06 (M2a): UDPTL layer in the T.38 loopback, built on the `t38` crate, and the packet-loss
//! scenarios that failed in M0 (ECM + 5% loss, docs/test-reports/M0.md section 3.4).

use fax_sim::t38_loopback::{T38LoopbackConfig, UdptlLayer, run_detailed};
use fax_sim::tiff_reader::{self, PageInfo};
use fax_sim::tiff_writer::{self, FAX_WIDTH, PageSpec};
use std::path::{Path, PathBuf};
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
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("x06-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

fn input(name: &str) -> PathBuf {
    let p = tmp(name);
    tiff_writer::write_file(&p, &[PageSpec::standard(1100)]).unwrap();
    p
}

fn received(path: &Path) -> Vec<PageInfo> {
    tiff_reader::read_file(path).unwrap_or_default()
}

struct Run {
    caller: Option<i32>,
    answerer: Option<i32>,
    pages: Vec<PageInfo>,
    sent: u64,
    dropped: u64,
    recovered: u64,
    gaps: u64,
    timed_out: bool,
}

fn run(
    name: &str,
    ecm: bool,
    version: Option<u8>,
    recovery: Option<ErrorRecovery>,
    seed: u64,
) -> Run {
    let rx = tmp(&format!("{name}-out.tif"));
    let _ = std::fs::remove_file(&rx);
    let mut cfg = T38LoopbackConfig::new(input(&format!("{name}-in.tif")), rx.clone());
    cfg.ecm = ecm;
    cfg.loss_permille = 50;
    cfg.seed = seed;
    cfg.t38_version = version;
    cfg.udptl = recovery.map(|recovery| UdptlLayer { recovery });
    let o = run_detailed(&cfg);
    let (recovered, gaps) = o
        .udptl
        .map(|u| {
            (
                u.to_answerer.recovered_redundancy
                    + u.to_answerer.recovered_fec
                    + u.to_caller.recovered_redundancy
                    + u.to_caller.recovered_fec,
                u.gap_events,
            )
        })
        .unwrap_or((0, 0));
    let r = Run {
        caller: o.result.caller_code,
        answerer: o.result.answerer_code,
        pages: received(&rx),
        sent: o.result.packets_sent,
        dropped: o.result.packets_dropped,
        recovered,
        gaps,
        timed_out: o.result.timed_out,
    };
    eprintln!(
        "X06 {name} seed={seed:#x} ecm={ecm} caller={:?} answerer={:?} sent={} dropped={} recovered={} gap_events={} pages={:?}",
        r.caller,
        r.answerer,
        r.sent,
        r.dropped,
        r.recovered,
        r.gaps,
        r.pages
            .iter()
            .map(|p| (p.width, p.height))
            .collect::<Vec<_>>()
    );
    r
}

fn assert_clean(r: &Run) {
    assert!(!r.timed_out);
    assert_eq!(r.caller, Some(OK));
    assert_eq!(r.answerer, Some(OK));
    assert_eq!(r.pages.len(), 1);
    assert_eq!(r.pages[0].width, FAX_WIDTH);
    assert_eq!(
        r.pages[0].height, 1100,
        "no rows may be lost when nothing is unrecoverable"
    );
}

/// The case that failed in M0: ECM with 5% loss now succeeds for 5 seeds because redundancy
/// repairs the lost datagrams.
#[test]
fn x06_ecm_5pct_loss_redundancy3_five_seeds() {
    for seed in SEEDS {
        let r = run(
            "ecm-red3",
            true,
            Some(3),
            Some(ErrorRecovery::Redundancy { depth: 3 }),
            seed,
        );
        assert!(r.dropped > 0, "loss injection must actually drop datagrams");
        assert!(r.recovered > 0, "redundancy must have been used");
        assert_eq!(r.gaps, 0, "depth 3 repairs every loss in this model");
        assert_clean(&r);
    }
}

/// Same with T.38 version 0 (the SpanDSP default).
#[test]
fn x06_ecm_5pct_loss_redundancy3_v0() {
    for seed in SEEDS {
        let r = run(
            "ecm-red3-v0",
            true,
            None,
            Some(ErrorRecovery::Redundancy { depth: 3 }),
            seed,
        );
        assert!(r.dropped > 0);
        assert_clean(&r);
    }
}

/// Non-ECM: with redundancy not a single image row is lost (M0 S09 lost 51 rows to 5% loss).
#[test]
fn x06_non_ecm_5pct_loss_redundancy3_loses_no_rows() {
    for seed in SEEDS {
        let r = run(
            "non-ecm-red3",
            false,
            Some(3),
            Some(ErrorRecovery::Redundancy { depth: 3 }),
            seed,
        );
        assert!(r.dropped > 0);
        assert!(r.recovered > 0);
        assert_clean(&r);
    }
}

/// Parity FEC (span 3, 1 entry): survives single losses within a set; ECM absorbs the rest
/// through its own retransmission.
#[test]
fn x06_ecm_5pct_loss_fec() {
    for seed in SEEDS {
        let r = run(
            "ecm-fec",
            true,
            Some(3),
            Some(ErrorRecovery::Fec {
                span: 3,
                entries: 1,
            }),
            seed,
        );
        assert!(r.dropped > 0);
        assert!(r.recovered > 0, "FEC must have repaired something");
        assert!(!r.timed_out);
        assert_eq!(r.caller, Some(OK));
        assert_eq!(r.answerer, Some(OK));
        assert_eq!(r.pages.len(), 1);
        assert_eq!(r.pages[0].height, 1100);
    }
}

/// Lossless baseline through the UDPTL layer, every recovery mode.
#[test]
fn x06_lossless_udptl_layer_is_transparent() {
    for (name, mode) in [
        ("none", ErrorRecovery::None),
        ("red0", ErrorRecovery::Redundancy { depth: 0 }),
        ("red3", ErrorRecovery::Redundancy { depth: 3 }),
        (
            "fec",
            ErrorRecovery::Fec {
                span: 2,
                entries: 2,
            },
        ),
    ] {
        let rx = tmp(&format!("clean-{name}-out.tif"));
        let mut cfg = T38LoopbackConfig::new(input(&format!("clean-{name}-in.tif")), rx.clone());
        cfg.ecm = true;
        cfg.udptl = Some(UdptlLayer { recovery: mode });
        let o = run_detailed(&cfg);
        eprintln!("X06 clean {name}: {:?} udptl={:?}", o.result, o.udptl);
        assert_eq!(o.result.caller_code, Some(OK));
        assert_eq!(o.result.answerer_code, Some(OK));
        assert_eq!(o.result.packets_dropped, 0);
        let u = o.udptl.unwrap();
        assert_eq!(u.gap_events, 0);
        assert_eq!(u.to_answerer.duplicates + u.to_caller.duplicates, 0);
        assert_eq!(u.to_answerer.malformed + u.to_caller.malformed, 0);
        assert_eq!(received(&rx)[0].height, 1100);
    }
}

/// Negative control: the raw model of M0 (no UDPTL, no recovery) still fails ECM + 5% loss for
/// at least one of the same seeds, proving the scenario is hard enough to need the UDPTL layer.
#[test]
fn x06_control_raw_model_without_recovery_fails_for_some_seed() {
    let mut failures = 0;
    for seed in SEEDS {
        let r = run("ecm-raw", true, None, None, seed);
        if r.caller != Some(OK) || r.answerer != Some(OK) || r.timed_out {
            failures += 1;
        }
    }
    eprintln!("X06 control: raw model failed {failures}/5 seeds");
    assert!(failures >= 1);
}
