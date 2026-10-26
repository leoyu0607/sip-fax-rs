//! M5e: faxes over G.711 into the real `sip-faxd serve`. The simulated gateway
//! (`fax_sim::sip_uac` with [`ReinviteBy::Audio`]) has no T.38: it answers the server's T.38
//! re-INVITE with 488 and sends the TIFF with SpanDSP's audio fax modem over RTP
//! (`fax_sim::audio_gateway`). Pages are compared with the source by libtiff and poppler, as
//! in `e2e_m2d`.
//!
//! The plan's E01–E08 (docs/test-plans/M5.md section 8) go in this file too.

mod e2e_common;

use e2e_common::*;

use fax_sim::sip_uac::{ReinviteBy, run_call};

/// Development check of the ROADMAP criterion: `t38_policy = "auto"`, our T.38 re-INVITE is
/// refused, the fax (two pages) arrives over G.711 at V.29 9600, pixel exact.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn d01_refused_t38_then_two_pages_over_g711() {
    let tag = "m5e-d01";
    let f = faxd(tag, 2, "initiate_after_secs = 0.3\n", "");
    let dir = scratch(&format!("src-{tag}"));
    let pages = specs(2, false, 260);
    let src = source_tiff(&dir, "src.tif", &pages);
    let cfg = uac(&f, ReinviteBy::Audio, &src, "0000000001");
    let rep = run_call(cfg)
        .await
        .unwrap_or_else(|e| panic!("{tag}: {e}\n{}", f.log().join("\n")));
    f.wait_results(1);
    let res = result_json(&f, &rep.call_id);
    let run = rep.fax.as_ref().expect("fax run");
    assert!(!run.timed_out, "{tag}: gateway timed out: {run:?}\n{res:#}");
    assert_eq!(
        run.caller_code,
        Some(0),
        "{tag}: SpanDSP says {:?} ({})\n{res:#}",
        run.caller_code,
        run.caller_text
    );
    assert_eq!(rep.t38_refused, 1, "{tag}: the server's T.38 re-INVITE");
    assert_eq!(res["outcome"]["status"], "completed", "{tag}: {res:#}");
    assert!(res["t38"].is_null(), "{tag}: {res:#}");
    let reason = res["audio"]["reason"].as_str().unwrap_or_default();
    assert!(reason.contains("488"), "{tag}: audio reason {reason:?}");
    let fax = &res["fax"];
    assert_eq!(fax["code"], 0, "{tag}: {fax:#}");
    assert_eq!(fax["media"], "g711", "{tag}");
    assert_eq!(fax["modem"], "v29", "{tag}");
    assert_eq!(fax["bit_rate"], 9_600, "{tag}");
    assert_eq!(fax["pages"], 2, "{tag}");
    assert_eq!(fax["remote_ident"], "0000000001", "{tag}");
    assert_eq!(
        fax["bad_lines_per_page"],
        serde_json::json!([0, 0]),
        "{tag}"
    );
    assert!(
        fax["rtp"]["rx_packets"].as_u64().unwrap_or(0) > 500,
        "{tag}: {fax:#}"
    );
    let v = verify_files(tag, fax, &src, &pages);
    assert_eq!(v.pages, 2, "{tag}");
    // audio.log next to sip.log
    let log = std::fs::read_to_string(capture_dir(&f, &rep.call_id).join("audio.log"))
        .unwrap_or_else(|e| panic!("{tag}: audio.log: {e}"));
    assert!(log.contains("V.29 9600 receiver ready"), "{tag}: {log}");
    eprintln!(
        "{tag}: SpanDSP={} | pages={} rate={} dur={}ms | rtp gw tx={} rx={} | server rtp {}",
        run.caller_code.unwrap(),
        fax["pages"],
        fax["bit_rate"],
        fax["duration_ms"],
        run.tx_datagrams,
        run.rx_datagrams,
        fax["rtp"]
    );
    f.stop();
}
