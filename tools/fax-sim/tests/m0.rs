//! M0 test cases S01, S03-S10 (S02 lives in s02_ffi_lifecycle.rs).

use fax_sim::tiff_reader::{self, PageInfo};
use fax_sim::tiff_writer::{self, FAX_WIDTH, PageSpec};
use fax_sim::{SessionResult, audio_loopback, t38_loopback};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};

const T30_ERR_OK: i32 = 0;

/// Scratch files live in a directory that is unique per test process, so two suites running
/// at the same time (e.g. concurrent `scripts/ci-local.sh` runs) never overwrite each other.
fn tmp(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("m0-{}", std::process::id()));
    std::fs::create_dir_all(&d).unwrap();
    d.join(name)
}

fn make_input(name: &str, pages: &[PageSpec]) -> PathBuf {
    let p = tmp(name);
    tiff_writer::write_file(&p, pages).unwrap();
    p
}

fn received(path: &Path) -> Vec<PageInfo> {
    tiff_reader::read_file(path).unwrap_or_else(|e| panic!("cannot read {}: {e}", path.display()))
}

/// Assert both completion codes are OK and the received file matches `sent`.
fn assert_ok_transfer(r: &SessionResult, rx: &Path, sent: &[PageSpec]) {
    assert!(!r.timed_out, "session timed out: {r:?}");
    assert_eq!(r.caller_code, Some(T30_ERR_OK), "caller code: {r:?}");
    assert_eq!(r.answerer_code, Some(T30_ERR_OK), "answerer code: {r:?}");
    let got = received(rx);
    assert_eq!(got.len(), sent.len(), "received page count");
    for (i, (g, s)) in got.iter().zip(sent).enumerate() {
        assert_eq!(g.width, s.width, "page {i} width");
        assert_eq!(g.height, s.height, "page {i} height");
    }
}

#[test]
fn s01_spandsp_found_via_pkg_config() {
    let v = spandsp_sys::SPANDSP_VERSION;
    let nums: Vec<u32> = v.split('.').map(|p| p.parse().unwrap()).collect();
    assert!((nums[0], nums[1]) >= (3, 1), "SpanDSP too old: {v}");
    eprintln!("S01 spandsp version {v}");
}

#[test]
fn s01_missing_spandsp_gives_clear_error() {
    // Re-run only the build script with pkg-config unable to find spandsp.
    let manifest = concat!(env!("CARGO_MANIFEST_DIR"), "/../spandsp-sys/Cargo.toml");
    let target = tmp("s01-nested-target");
    let out = Command::new(env!("CARGO"))
        .args(["build", "--offline", "--manifest-path", manifest])
        .env("CARGO_TARGET_DIR", &target)
        .env("PKG_CONFIG_PATH", "/nonexistent")
        .env("PKG_CONFIG_LIBDIR", "/nonexistent")
        .env_remove("RUSTFLAGS")
        .output()
        .unwrap();
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "build unexpectedly succeeded");
    assert!(stderr.contains("cannot find SpanDSP"), "stderr: {stderr}");
    assert!(stderr.contains("scripts/e2e-docker.sh"), "stderr: {stderr}");
}

#[test]
fn s03_test_page_generator() {
    let pages = [
        PageSpec::standard(1100),
        PageSpec::fine(2200),
        PageSpec::standard(400),
    ];
    let p = make_input("s03.tif", &pages);
    let info = tiff_reader::read_file(&p).unwrap();
    assert_eq!(info.len(), 3);
    for (i, (g, s)) in info.iter().zip(&pages).enumerate() {
        assert_eq!(g.width, FAX_WIDTH, "page {i}");
        assert_eq!(g.height, s.height, "page {i}");
        assert_eq!(g.x_dpi, Some(204.0), "page {i}");
        assert_eq!(g.y_dpi, Some(f64::from(s.y_dpi)), "page {i}");
        assert_eq!(g.compression, Some(1));
        assert_eq!(g.bits_per_sample, Some(1));
    }

    // Cross-check against libtiff's tiffinfo when available.
    match Command::new("tiffinfo").arg(&p).output() {
        Ok(o) => {
            assert!(
                o.status.success(),
                "tiffinfo failed: {}",
                String::from_utf8_lossy(&o.stderr)
            );
            let t = String::from_utf8_lossy(&o.stdout);
            assert_eq!(t.matches("TIFF Directory at offset").count(), 3, "{t}");
            assert_eq!(t.matches("Image Width: 1728").count(), 3, "{t}");
            assert!(t.contains("Image Length: 1100"), "{t}");
            assert!(t.contains("Image Length: 2200"), "{t}");
            assert!(t.contains("Image Length: 400"), "{t}");
            assert_eq!(
                t.matches("Resolution: 204, 98 pixels/inch").count(),
                2,
                "{t}"
            );
            assert_eq!(
                t.matches("Resolution: 204, 196 pixels/inch").count(),
                1,
                "{t}"
            );
            eprintln!("S03 tiffinfo cross-check OK");
        }
        Err(e) => panic!("tiffinfo not available ({e}); it is installed in the test image"),
    }
}

#[test]
fn s04_t38_loopback_one_page() {
    let pages = [PageSpec::standard(1100)];
    let tx = make_input("s04-in.tif", &pages);
    let rx = tmp("s04-out.tif");
    let _ = std::fs::remove_file(&rx);
    let t0 = Instant::now();
    let r = t38_loopback::run(&t38_loopback::T38LoopbackConfig::new(tx, rx.clone()));
    eprintln!("S04 {r:?} wall={:?}", t0.elapsed());
    assert_ok_transfer(&r, &rx, &pages);
    assert!(r.packets_sent > 0);
    assert_eq!(r.packets_dropped, 0);
}

#[test]
fn s05_t38_loopback_three_pages() {
    let pages = [
        PageSpec::standard(1100),
        PageSpec::fine(1500),
        PageSpec::standard(700),
    ];
    let tx = make_input("s05-in.tif", &pages);
    let rx = tmp("s05-out.tif");
    let _ = std::fs::remove_file(&rx);
    let r = t38_loopback::run(&t38_loopback::T38LoopbackConfig::new(tx, rx.clone()));
    eprintln!("S05 {r:?}");
    assert_ok_transfer(&r, &rx, &pages);
}

#[test]
fn s06_t38_loopback_ecm() {
    let pages = [PageSpec::standard(1100)];
    let tx = make_input("s06-in.tif", &pages);
    let rx = tmp("s06-out.tif");
    let _ = std::fs::remove_file(&rx);
    let mut cfg = t38_loopback::T38LoopbackConfig::new(tx, rx.clone());
    cfg.ecm = true;
    let r = t38_loopback::run(&cfg);
    eprintln!("S06 {r:?}");
    assert_ok_transfer(&r, &rx, &pages);
}

#[test]
fn s07_audio_loopback() {
    let pages = [PageSpec::standard(1100)];
    let tx = make_input("s07-in.tif", &pages);
    let rx = tmp("s07-out.tif");
    let _ = std::fs::remove_file(&rx);
    let r = audio_loopback::run(&audio_loopback::AudioLoopbackConfig::new(tx, rx.clone()));
    eprintln!("S07 {r:?}");
    assert_ok_transfer(&r, &rx, &pages);
}

#[test]
fn s08_error_path_missing_tx_file() {
    let tx = tmp("s08-does-not-exist.tif");
    let _ = std::fs::remove_file(&tx);
    let rx = tmp("s08-out.tif");
    let _ = std::fs::remove_file(&rx);
    let mut cfg = t38_loopback::T38LoopbackConfig::new(tx, rx.clone());
    cfg.max_sim_ms = 120_000;
    let r = t38_loopback::run(&cfg);
    eprintln!("S08 {r:?}");
    assert!(
        !r.timed_out,
        "session must terminate on its own within the simulated cap: {r:?}"
    );
    assert!(r.sim_ms < cfg.max_sim_ms);
    let code = r.caller_code.expect("caller must report a completion code");
    assert_ne!(code, T30_ERR_OK, "caller must fail: {r:?}");
    let reason = fax_sim::spandsp::completion_str(code);
    eprintln!("S08 caller reason: {reason}");
    assert!(
        reason.contains("TIFF") || reason.contains("file"),
        "unexpected reason: {reason}"
    );
    // no page can have been received
    let pages = std::fs::read(&rx)
        .ok()
        .and_then(|b| tiff_reader::parse(&b).ok());
    assert!(
        pages.is_none(),
        "no pages should be received, got {pages:?}"
    );
}

fn run_lossy(name: &str, ecm: bool) -> (SessionResult, Vec<PageInfo>) {
    let tx = make_input(&format!("{name}-in.tif"), &[PageSpec::standard(1100)]);
    let rx = tmp(&format!("{name}-out.tif"));
    let _ = std::fs::remove_file(&rx);
    let mut cfg = t38_loopback::T38LoopbackConfig::new(tx, rx.clone());
    cfg.ecm = ecm;
    cfg.loss_permille = 50;
    cfg.seed = 0x5EED_0001;
    let r = t38_loopback::run(&cfg);
    eprintln!("{name} {r:?}");
    assert!(
        r.packets_dropped > 0,
        "loss injection must actually drop packets"
    );
    assert!(r.packets_dropped < r.packets_sent);
    assert!(!r.timed_out, "{r:?}");
    assert_eq!(r.caller_code, Some(T30_ERR_OK), "{r:?}");
    assert_eq!(r.answerer_code, Some(T30_ERR_OK), "{r:?}");
    (r, received(&rx))
}

/// Plan case S09 (non-ECM): the page completes despite 5% loss. In non-ECM mode
/// SpanDSP cannot retransmit lost image data, so rows are lost: the received
/// page keeps its width but may be shorter than the source. This is physical
/// behaviour of non-ECM fax, so the height is asserted as a bounded range
/// rather than exactly equal (documented in the M0 test report).
#[test]
fn s09_packet_loss_injection() {
    let (_r, got) = run_lossy("s09", false);
    assert_eq!(got.len(), 1, "page count");
    assert_eq!(got[0].width, FAX_WIDTH);
    assert!(
        (990..=1100).contains(&got[0].height),
        "height {} outside 90%..100% of source 1100",
        got[0].height
    );
}

#[test]
fn s10_determinism_and_speed() {
    let pages = [PageSpec::standard(1100)];
    let tx = make_input("s10-in.tif", &pages);

    // speed: S04 scenario under 30 s wall time, on a simulated clock
    let rx = tmp("s10-out-a.tif");
    let t0 = Instant::now();
    let r = t38_loopback::run(&t38_loopback::T38LoopbackConfig::new(
        tx.clone(),
        rx.clone(),
    ));
    let wall = t0.elapsed();
    eprintln!("S10 clean run {r:?} wall={wall:?}");
    assert_ok_transfer(&r, &rx, &pages);
    assert!(wall < Duration::from_secs(30), "wall time {wall:?}");
    assert!(
        Duration::from_millis(r.sim_ms) > wall,
        "simulated time ({} ms) should exceed wall time ({wall:?}): clock must be simulated",
        r.sim_ms
    );

    // determinism: same seed (with loss) twice => identical hash and stats
    let run_seeded = |seed: u64, out: &str| {
        let mut cfg = t38_loopback::T38LoopbackConfig::new(tx.clone(), tmp(out));
        cfg.loss_permille = 50;
        cfg.seed = seed;
        t38_loopback::run(&cfg)
    };
    let a = run_seeded(42, "s10-out-b.tif");
    let b = run_seeded(42, "s10-out-c.tif");
    let c = run_seeded(43, "s10-out-d.tif");
    eprintln!(
        "S10 hashes a={:016x} b={:016x} c(seed43)={:016x}",
        a.packet_hash, b.packet_hash, c.packet_hash
    );
    assert_eq!(a, b, "same seed must reproduce the identical session");
    assert_ne!(
        a.packet_hash, c.packet_hash,
        "different seed should change the sequence"
    );
    assert!(a.packets_dropped > 0);
    // lossless runs are deterministic too
    let rx2 = tmp("s10-out-e.tif");
    let r2 = t38_loopback::run(&t38_loopback::T38LoopbackConfig::new(tx, rx2));
    assert_eq!(r.packet_hash, r2.packet_hash);
}
