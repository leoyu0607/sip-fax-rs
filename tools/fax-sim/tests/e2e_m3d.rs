//! M3d end-to-end cases through the real `sip-faxd serve` binary: the simulated gateway
//! (SpanDSP `t38_terminal` with ECM enabled on a real UDP socket and clock) sends TIFFs with
//! ECM and MMR; every stored TIFF / PDF is compared with the source twice, with libtiff and with
//! poppler (ADR-025, each with a negative control). Also the I04 DIS-bit assertions.
//!
//! The DIS and DCS frames are read from `udptl.log` (the default `summary` capture keeps the T.30
//! control frames in hex and shows image-rate data as lengths only, ADR-026).

mod e2e_common;

use e2e_common::*;
use fax_sim::gateway::FaxTx;
use fax_sim::sip_uac::{ReinviteBy, run_call};
use fax_sim::tiff_writer::render_bitmap;
use spandsp_sys as sys;
use t38::ErrorRecovery;

fn unhex(h: &str) -> Vec<u8> {
    (0..h.len() / 2)
        .map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

/// Every complete HDLC frame in the summary log whose FCF (X bit cleared) is `fcf`, as octets.
fn frames_with_fcf(log: &str, dir: &str, fcf: u8) -> Vec<Vec<u8>> {
    log.lines()
        .filter(|l| l.split(' ').nth(1) == Some(dir))
        .filter_map(|l| l.split("frame=").nth(1))
        .map(|h| unhex(h.split_whitespace().next().unwrap_or("")))
        .filter(|f| f.len() >= 3 && f[2] & 0x7F == fcf)
        .collect()
}

fn bit(fif: &[u8], n: usize) -> bool {
    fif.get((n - 1) / 8)
        .is_some_and(|b| b & (0x80 >> ((n - 1) % 8)) != 0)
}

struct Case {
    rep: fax_sim::sip_uac::UacReport,
    res: serde_json::Value,
    fax: serde_json::Value,
    log: String,
    server: Server,
}

#[allow(clippy::too_many_arguments)]
async fn call(
    tag: &str,
    trunk: &str,
    npages: usize,
    fine: bool,
    tx: impl FnOnce(&mut FaxTx),
) -> (
    Case,
    Vec<fax_sim::tiff_writer::PageSpec>,
    std::path::PathBuf,
) {
    let f = faxd(tag, 2, trunk, "");
    let dir = scratch(&format!("src-{tag}"));
    let pages = specs(npages, fine, 260);
    let src = source_tiff(&dir, "src.tif", &pages);
    let mut cfg = uac(&f, ReinviteBy::Gateway, &src, "0000000001");
    {
        let t = cfg.fax.as_mut().unwrap();
        t.ecm = true;
        tx(t);
    }
    let rep = run_call(cfg)
        .await
        .unwrap_or_else(|e| panic!("{tag}: {e}\n{}", f.log().join("\n")));
    f.wait_results(1);
    let res = result_json(&f, &rep.call_id);
    let fax = res["fax"].clone();
    let want = fax["udptl"]["rx_datagrams"].as_u64().unwrap_or(0)
        + fax["udptl"]["tx_datagrams"].as_u64().unwrap_or(0);
    let path = capture_dir(&f, &rep.call_id).join("udptl.log");
    let log = wait_for("udptl.log", || {
        let t = std::fs::read_to_string(&path).ok()?;
        (t.lines().count() as u64 >= want).then_some(t)
    });
    (
        Case {
            rep,
            res,
            fax,
            log,
            server: f,
        },
        pages,
        src,
    )
}

/// The ECM facts every ECM case shares: DIS offers ECM (and MMR), the DCS chose ECM, and no image
/// octet is in the capture.
fn assert_ecm_signalling(tag: &str, c: &Case, mmr: bool) {
    let dis = frames_with_fcf(&c.log, "TX", 0x01);
    let dis = dis
        .first()
        .unwrap_or_else(|| panic!("{tag}: no DIS in the log"));
    assert!(bit(&dis[3..], 27), "{tag}: DIS bit 27 (ECM)");
    assert!(bit(&dis[3..], 31), "{tag}: DIS bit 31 (T.6)");
    let dcs = frames_with_fcf(&c.log, "RX", 0x41);
    let dcs = dcs
        .last()
        .unwrap_or_else(|| panic!("{tag}: no DCS in the log"));
    assert!(bit(&dcs[3..], 27), "{tag}: DCS bit 27 (ECM)");
    assert_eq!(bit(&dcs[3..], 31), mmr, "{tag}: DCS bit 31 (T.6)");
    assert!(
        !bit(&dcs[3..], 28),
        "{tag}: SpanDSP always asks for 256 octet frames"
    );
    assert!(
        c.log.contains("data=V17_14400 HdlcData("),
        "{tag}: the image went as HDLC frames at image rate"
    );
    assert!(
        !frames_with_fcf(&c.log, "RX", 0x7D).is_empty(),
        "{tag}: PPS seen"
    );
    assert!(
        !frames_with_fcf(&c.log, "TX", 0x31).is_empty(),
        "{tag}: MCF sent"
    );
    // no FCD frame content: the octets of an image-rate hdlc-data field are shown as a length
    for l in c.log.lines().filter(|l| l.contains("data=V17")) {
        assert!(!l.contains(")="), "{tag}: image octets in the capture: {l}");
        assert!(l.len() < 200, "{tag}: {l}");
    }
}

async fn finish(
    tag: &str,
    c: Case,
    pages: &[fax_sim::tiff_writer::PageSpec],
    src: &std::path::Path,
) {
    let fax = assert_success(
        tag,
        &c.rep,
        &c.res,
        pages.len(),
        "0000000001",
        "peer_reinvite",
    );
    let v = verify_files(tag, &fax, src, pages);
    eprintln!("{}", summary(tag, &c.rep, &c.res, &v));
    c.server.stop();
}

/// ECM is off by default (ADR-035); these cases enable it explicitly.
const ECM_ON: &str = "[trunks.fax]\necm = true\n";

/// E2E-1: ECM + MMR, three fine pages: DIS / DCS bits, pixel-exact twice.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m3d_e2e_01_ecm_mmr_three_fine_pages() {
    let (c, pages, src) = call("m3d01", ECM_ON, 3, true, |t| {
        t.compressions = Some(sys::T4_COMPRESSION_T6);
    })
    .await;
    assert_ecm_signalling("m3d01", &c, true);
    assert_eq!(c.fax["compression"], "mmr");
    finish("m3d01", c, &pages, &src).await;
}

/// E2E-2: ECM with MH and with MR (SpanDSP's mask restricts the coding).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m3d_e2e_02_ecm_mh_and_mr() {
    for (tag, mask, name) in [
        ("m3d02mh", sys::T4_COMPRESSION_T4_1D, "mh"),
        (
            "m3d02mr",
            sys::T4_COMPRESSION_T4_1D | sys::T4_COMPRESSION_T4_2D,
            "mr",
        ),
    ] {
        let (c, pages, src) = call(tag, ECM_ON, 2, false, |t| t.compressions = Some(mask)).await;
        assert_ecm_signalling(tag, &c, false);
        assert_eq!(c.fax["compression"], name, "{tag}");
        finish(tag, c, &pages, &src).await;
    }
}

/// E2E-3: ECM through 5 % loss with UDPTL redundancy (the stored pages are still exact).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m3d_e2e_03_ecm_under_loss_with_redundancy() {
    let (c, pages, src) = call("m3d03", ECM_ON, 2, false, |t| {
        t.compressions = Some(sys::T4_COMPRESSION_T6);
        t.loss_permille = 50;
        t.seed = 0x5eed_0301;
        t.recovery = ErrorRecovery::Redundancy { depth: 3 };
    })
    .await;
    assert!(
        c.rep.fax.as_ref().unwrap().dropped() > 0,
        "loss was injected"
    );
    assert_ecm_signalling("m3d03", &c, true);
    finish("m3d03", c, &pages, &src).await;
}

/// I04 (DIS part): `resolutions = ["standard"]` makes the DIS announce standard only (bit 15
/// clear), and SpanDSP, which has a fine page, sends it at standard resolution.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m3d_i04_standard_only_dis_and_a_fine_sender_drops_to_standard() {
    let (c, pages, _src) = call(
        "m3d04",
        "[trunks.fax]\nresolutions = [\"standard\"]\necm = true\n",
        1,
        true,
        |t| t.compressions = Some(sys::T4_COMPRESSION_T6),
    )
    .await;
    let dis = frames_with_fcf(&c.log, "TX", 0x01);
    let fif = &dis.first().expect("DIS")[3..];
    assert!(!bit(fif, 15), "DIS bit 15 (fine) must be clear: {fif:02x?}");
    assert!(bit(fif, 27) && bit(fif, 31), "ECM and MMR still offered");
    assert_eq!(fif[..3], [0x00, 0x75, 0x1F], "octets 1..3 of the DIS");
    let dcs = frames_with_fcf(&c.log, "RX", 0x41);
    let dcs = dcs.last().expect("DCS");
    assert!(!bit(&dcs[3..], 15), "the DCS is standard resolution");
    let gw = c.rep.fax.as_ref().unwrap();
    eprintln!(
        "m3d04: SpanDSP={:?} ({}) result.fax={}",
        gw.caller_code, gw.caller_text, c.fax
    );
    assert_eq!(gw.caller_code, Some(0), "{}", gw.caller_text);
    assert_eq!(c.fax["code"], 0);
    assert_eq!(c.fax["resolution"], "standard");
    let tif = std::path::PathBuf::from(c.fax["tiff"].as_str().unwrap());
    let info = String::from_utf8_lossy(
        &std::process::Command::new("tiffinfo")
            .arg(&tif)
            .output()
            .unwrap()
            .stdout,
    )
    .into_owned();
    assert!(info.contains("Resolution: 204, 98 pixels/inch"), "{info}");
    // The stored page is the fine source squashed to standard by SpanDSP: decode it with libtiff
    // and find out how (every second row, or the pairs ORed), then require an exact match.
    let work = scratch("verify-m3d04");
    let raw = work.join("raw.tif");
    run("tiffcp", &["-c", "none", s(&tif), s(&raw)]);
    let got = read_uncompressed(&std::fs::read(&raw).unwrap());
    assert_eq!(got.len(), 1);
    let (w, h, bm) = &got[0];
    let src = render_bitmap(&pages[0], 0);
    let rb = (*w as usize).div_ceil(8);
    assert_eq!(*w, pages[0].width);
    assert_eq!(*h as usize * 2, pages[0].height as usize, "half the rows");
    let rows = |f: &dyn Fn(usize) -> Vec<u8>| -> Vec<u8> { (0..*h as usize).flat_map(f).collect() };
    let even = rows(&|y| src[2 * y * rb..(2 * y + 1) * rb].to_vec());
    let odd = rows(&|y| src[(2 * y + 1) * rb..(2 * y + 2) * rb].to_vec());
    let ored = rows(&|y| {
        (0..rb)
            .map(|x| src[2 * y * rb + x] | src[(2 * y + 1) * rb + x])
            .collect()
    });
    let which = if *bm == even {
        "every even row"
    } else if *bm == odd {
        "every odd row"
    } else if *bm == ored {
        "pairs ORed"
    } else {
        "NONE"
    };
    eprintln!("m3d04: SpanDSP squashed the fine page to standard by: {which}");
    assert_ne!(
        which, "NONE",
        "the stored page is not a squashing of the source"
    );
    c.server.stop();
}

/// `ecm = false`: the DIS carries neither bit 27 nor bit 31, and an ECM-capable sender falls
/// back to the non-ECM procedure.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn m3d_e2e_04_ecm_off_dis_has_no_ecm_bits() {
    let (c, pages, src) = call("m3d05", "[trunks.fax]\necm = false\n", 2, false, |_| {}).await;
    let dis = frames_with_fcf(&c.log, "TX", 0x01);
    let fif = &dis.first().expect("DIS")[3..];
    assert!(!bit(fif, 27) && !bit(fif, 31), "{fif:02x?}");
    assert_eq!(fif, [0x00, 0x77, 0x1E]);
    let dcs = frames_with_fcf(&c.log, "RX", 0x41);
    assert!(
        !bit(&dcs.last().expect("DCS")[3..], 27),
        "sender must not use ECM"
    );
    finish("m3d05", c, &pages, &src).await;
}
