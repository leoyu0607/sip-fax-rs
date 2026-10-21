//! E01..E07 (M2d): the simulated gateway (`fax_sim::sip_uac` + SpanDSP `t38_terminal` on a
//! real UDP socket and clock) calls the real `sip-faxd serve` binary, which receives the fax
//! over T.38 and writes TIFF / PDF.
//!
//! Every received page is compared with the source bitmap twice, by independent tools:
//! (a) libtiff: `tiffcmp -t source.tif ours.tif` plus `tiffcp -c none` and a byte-level compare
//!     of the decoded strips; with a negative control (a one-bit-different reference must make
//!     `tiffcmp -t` fail, ADR-018);
//! (b) poppler: `pdftoppm -mono -rx 204 -ry 98|196` and a compare of the PBM raster.
//!
//! The SpanDSP completion code (T30_ERR_OK = 0) and the fields of `result.json` are asserted.
//! Each test runs its own `sip-faxd` (own config, own ports, own data directory).

mod e2e_common;

use e2e_common::*;
use std::time::Duration;

use fax_sim::gateway::{FaxTx, GatewayQuirks};
use fax_sim::sip_uac::{ReinviteBy, UacConfig, run_call};
use t38::ErrorRecovery;

// ------------------------------------------------------------------ E01 .. E03

async fn single_case(
    tag: &str,
    by: ReinviteBy,
    trunk: &str,
    npages: usize,
    fine: bool,
    path: &str,
    tx: impl FnOnce(&mut FaxTx),
) -> String {
    let f = faxd(tag, 2, trunk, "");
    let dir = scratch(&format!("src-{tag}"));
    let pages = specs(npages, fine, 260);
    let src = source_tiff(&dir, "src.tif", &pages);
    let mut cfg = uac(&f, by, &src, "0000000001");
    tx(cfg.fax.as_mut().unwrap());
    let rep = run_call(cfg)
        .await
        .unwrap_or_else(|e| panic!("{tag}: {e}\n{}", f.log().join("\n")));
    let results = f.wait_results(1);
    let res = result_json(&f, &rep.call_id);
    assert_eq!(results[0]["call_id"], rep.call_id.as_str());
    let fax = assert_success(tag, &rep, &res, npages, "0000000001", path);
    assert_path_layout(tag, &f, &rep.call_id, &fax);
    let v = verify_files(tag, &fax, &src, &pages);
    let line = summary(tag, &rep, &res, &v);
    eprintln!("{line}");
    f.stop();
    line
}

/// E01: gateway-initiated re-INVITE, one page.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e01_gateway_initiated_reinvite_one_page() {
    single_case(
        "e01",
        ReinviteBy::Gateway,
        "",
        1,
        false,
        "peer_reinvite",
        |_| {},
    )
    .await;
}

/// E02: server-initiated re-INVITE, three pages (fine resolution).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e02_server_initiated_reinvite_three_pages() {
    single_case(
        "e02",
        ReinviteBy::Server,
        "initiate_after_secs = 0.3\n",
        3,
        true,
        "our_reinvite",
        |_| {},
    )
    .await;
}

/// E03: T.38 in the initial INVITE, two pages.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e03_t38_in_the_initial_invite() {
    single_case(
        "e03",
        ReinviteBy::InitialInvite,
        "",
        2,
        false,
        "initial_invite",
        |_| {},
    )
    .await;
}

// ------------------------------------------------------------------ E04

/// E04: five concurrent faxes, each with its own pages and TSI; no file may carry another
/// call's pages.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e04_five_concurrent_faxes_do_not_mix() {
    let f = faxd("e04", 5, "", "");
    let dir = scratch("src-e04");
    let modes = [
        ReinviteBy::Gateway,
        ReinviteBy::InitialInvite,
        ReinviteBy::Gateway,
        ReinviteBy::InitialInvite,
        ReinviteBy::Gateway,
    ];
    let mut jobs = Vec::new();
    let mut sources = Vec::new();
    for (i, by) in modes.into_iter().enumerate() {
        let pages = specs(1 + i % 3, i % 2 == 1, 200 + 60 * i as u32);
        let src = source_tiff(&dir, &format!("src{i}.tif"), &pages);
        let tsi = format!("000000010{i}");
        let cfg = uac(&f, by, &src, &tsi);
        sources.push((src, pages, tsi));
        jobs.push(tokio::spawn(run_call(cfg)));
    }
    let mut reports = Vec::new();
    for j in jobs {
        reports.push(
            j.await
                .unwrap()
                .unwrap_or_else(|e| panic!("{e}\n{}", f.log().join("\n"))),
        );
    }
    f.wait_results(5);
    let mut lines = Vec::new();
    let mut seen_files = std::collections::HashSet::new();
    for (rep, (src, pages, tsi)) in reports.iter().zip(&sources) {
        let res = result_json(&f, &rep.call_id);
        let fax = assert_success(
            "e04",
            rep,
            &res,
            pages.len(),
            tsi,
            if res["t38"]["path"] == "initial_invite" {
                "initial_invite"
            } else {
                "peer_reinvite"
            },
        );
        assert!(
            seen_files.insert(fax["tiff"].as_str().unwrap().to_string()),
            "two calls share a file"
        );
        assert_path_layout("e04", &f, &rep.call_id, &fax);
        let v = verify_files(&format!("e04-{tsi}"), &fax, src, pages);
        lines.push(summary(&format!("e04[{tsi}]"), rep, &res, &v));
    }
    // all five ports were in use at the same time and are free again
    let ports: std::collections::HashSet<u64> = f
        .results()
        .iter()
        .map(|r| r["t38"]["local_port"].as_u64().unwrap())
        .collect();
    assert_eq!(ports.len(), 5, "five different UDPTL ports: {ports:?}");
    for l in &lines {
        eprintln!("{l}");
    }
    f.stop();
}

// ------------------------------------------------------------------ E05

/// E05: 5 % datagram loss in both directions with redundancy 3 on both sides; the page must
/// still arrive pixel-exact. Several seeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e05_five_percent_loss_with_redundancy() {
    let seeds = [0x5EED_0001u64, 0x5EED_0002, 0x5EED_0003];
    let mut handles = Vec::new();
    for seed in seeds {
        handles.push(tokio::spawn(async move {
            let tag = format!("e05-{seed:x}");
            let f = faxd(&tag, 2, "", "");
            let dir = scratch(&format!("src-{tag}"));
            let pages = specs(2, false, 300);
            let src = source_tiff(&dir, "src.tif", &pages);
            let mut cfg = uac(&f, ReinviteBy::Gateway, &src, "0000000001");
            {
                let tx = cfg.fax.as_mut().unwrap();
                tx.loss_permille = 50;
                tx.seed = seed;
                tx.recovery = ErrorRecovery::Redundancy { depth: 3 };
            }
            let rep = run_call(cfg)
                .await
                .unwrap_or_else(|e| panic!("{tag}: {e}\n{}", f.log().join("\n")));
            f.wait_results(1);
            let res = result_json(&f, &rep.call_id);
            let fax = assert_success(&tag, &rep, &res, pages.len(), "0000000001", "peer_reinvite");
            let run = rep.fax.as_ref().unwrap();
            assert!(
                run.dropped() > 0,
                "{tag}: the loss injection dropped nothing"
            );
            let v = verify_files(&tag, &fax, &src, &pages);
            let line = summary(&tag, &rep, &res, &v);
            f.stop();
            line
        }));
    }
    for h in handles {
        eprintln!("{}", h.await.unwrap());
    }
}

/// E05b (extra): UDPTL parity FEC instead of redundancy, 3 % loss in both directions. The
/// trunk enables FEC, the gateway offers `t38UDPFEC`, so both directions use FEC.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e05b_three_percent_loss_with_fec() {
    let mut handles = Vec::new();
    for seed in [0x5EED_0011u64, 0x5EED_0012] {
        handles.push(tokio::spawn(async move {
            let tag = format!("e05b-{seed:x}");
            let f = faxd(&tag, 2, "[trunks.t38]\nfec = true\n", "");
            let dir = scratch(&format!("src-{tag}"));
            let pages = specs(2, true, 280);
            let src = source_tiff(&dir, "src.tif", &pages);
            let mut cfg = uac(&f, ReinviteBy::Gateway, &src, "0000000001");
            {
                let tx = cfg.fax.as_mut().unwrap();
                tx.loss_permille = 30;
                tx.seed = seed;
                tx.recovery = ErrorRecovery::Fec {
                    span: 3,
                    entries: 3,
                };
            }
            let rep = run_call(cfg)
                .await
                .unwrap_or_else(|e| panic!("{tag}: {e}\n{}", f.log().join("\n")));
            f.wait_results(1);
            let res = result_json(&f, &rep.call_id);
            assert_eq!(res["t38"]["udp_ec"], "t38UDPFEC", "{tag}");
            let fax = assert_success(&tag, &rep, &res, pages.len(), "0000000001", "peer_reinvite");
            assert!(
                rep.fax.as_ref().unwrap().dropped() > 0,
                "{tag}: nothing dropped"
            );
            let v = verify_files(&tag, &fax, &src, &pages);
            let line = summary(&tag, &rep, &res, &v);
            f.stop();
            format!(
                "{line} | server recovered_fec={}",
                fax["udptl"]["recovered_fec"]
            )
        }));
    }
    for h in handles {
        eprintln!("{}", h.await.unwrap());
    }
}

// ------------------------------------------------------------------ E06

fn unhex(h: &str) -> Vec<u8> {
    (0..h.len() / 2)
        .map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap())
        .collect()
}

/// E06: capture (`udptl = "full"` so that every datagram can be decoded again). `udptl.log` has one line per datagram (checked against the counters of BOTH
/// sides), every line decodes as UDPTL, and `sip.log` records the whole signalling.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e06_capture_udptl_log_and_sip_log() {
    let f = faxd_with("e06", 2, "", "", "udptl = \"full\"\n");
    let dir = scratch("src-e06");
    let pages = specs(1, false, 260);
    let src = source_tiff(&dir, "src.tif", &pages);
    let rep = run_call(uac(&f, ReinviteBy::Gateway, &src, "0000000001"))
        .await
        .unwrap_or_else(|e| panic!("{e}\n{}", f.log().join("\n")));
    f.wait_results(1);
    let res = result_json(&f, &rep.call_id);
    let fax = assert_success("e06", &rep, &res, 1, "0000000001", "peer_reinvite");
    let run = rep.fax.as_ref().unwrap();
    let cdir = capture_dir(&f, &rep.call_id);

    // udptl.log: wait until it is complete (the writer is asynchronous)
    let srv_rx = fax["udptl"]["rx_datagrams"].as_u64().unwrap();
    let srv_tx = fax["udptl"]["tx_datagrams"].as_u64().unwrap();
    let lines: Vec<String> = wait_for("udptl.log", || {
        let t = std::fs::read_to_string(cdir.join("udptl.log")).ok()?;
        let l: Vec<String> = t.lines().map(str::to_string).collect();
        (l.len() as u64 >= srv_rx + srv_tx).then_some(l)
    });
    assert_eq!(lines.len() as u64, srv_rx + srv_tx, "one line per datagram");
    // no loss was injected: what each side sent is what the other received
    assert_eq!(run.tx_dropped + run.rx_dropped, 0);
    // The server stops reading when it has the DCN, so the copies of the last packets the
    // gateway still sends afterwards (indicator repeats) are never received. They are the
    // newest sequence numbers: what the server did receive is a gap-free prefix (checked below).
    assert!(
        srv_rx <= run.tx_datagrams && run.tx_datagrams - srv_rx <= 8,
        "server rx {srv_rx} vs gateway tx {}",
        run.tx_datagrams
    );
    assert_eq!(srv_tx, run.rx_datagrams, "server tx == gateway rx");
    let (mut rx_seq, mut tx_seq) = (Vec::new(), Vec::new());
    let mut last_ts = String::new();
    for l in &lines {
        let mut it = l.splitn(4, ' ');
        let (ts, dir_, seq, hex) = (
            it.next().unwrap(),
            it.next().unwrap(),
            it.next().unwrap(),
            it.next().unwrap(),
        );
        assert!(ts.ends_with('Z') && ts.len() == 24, "timestamp {ts}");
        assert!(ts >= last_ts.as_str(), "timestamps ordered");
        last_ts = ts.to_string();
        let bytes = unhex(hex);
        let pkt = t38::UdptlPacket::decode(&bytes)
            .unwrap_or_else(|e| panic!("line does not decode: {e} {l}"));
        assert_eq!(pkt.seq.to_string(), seq, "seq column equals the packet's");
        match dir_ {
            "RX" => rx_seq.push(pkt.seq),
            "TX" => tx_seq.push(pkt.seq),
            other => panic!("direction {other}"),
        }
    }
    assert_eq!(rx_seq.len() as u64, srv_rx);
    assert_eq!(tx_seq.len() as u64, srv_tx);
    assert!(
        tx_seq.windows(2).all(|w| w[1] == w[0].wrapping_add(1)),
        "our sequence numbers are consecutive"
    );
    assert!(
        rx_seq.windows(2).all(|w| w[1] == w[0].wrapping_add(1)),
        "no loss: theirs too"
    );
    assert_eq!(tx_seq[0], 0);

    // sip.log: INVITE / 180 / 200 / ACK, the T.38 re-INVITE and its 200, ACK, BYE and its 200
    let sip = wait_for("complete sip.log", || {
        let t = std::fs::read_to_string(cdir.join("sip.log")).ok()?;
        let bye = t.find("BYE sip:")?;
        t[bye..].contains("SIP/2.0 200 OK").then_some(t)
    });
    let order = [
        "INVITE sip:fax@",
        "SIP/2.0 180 Ringing",
        "SIP/2.0 200 OK",
        "ACK sip:",
        "m=image",
        "BYE sip:",
    ];
    let mut at = 0;
    for needle in order {
        let i = sip[at..]
            .find(needle)
            .unwrap_or_else(|| panic!("sip.log lacks {needle:?} after offset {at}:\n{sip}"));
        at += i + needle.len();
    }
    assert_eq!(
        sip.matches("INVITE sip:").count(),
        2,
        "initial INVITE + T.38 re-INVITE"
    );
    assert_eq!(sip.matches("m=image").count(), 2, "T.38 offer and answer");
    assert!(sip.contains("RECV from") && sip.contains("SEND to"));
    eprintln!(
        "e06: udptl.log {} lines = server rx {srv_rx} + tx {srv_tx}; gateway tx {} rx {}; seq tx 0..{} rx {}..{}; sip.log {} bytes, {} messages",
        lines.len(),
        run.tx_datagrams,
        run.rx_datagrams,
        tx_seq.last().unwrap(),
        rx_seq[0],
        rx_seq.last().unwrap(),
        sip.len(),
        sip.matches("] RECV from").count() + sip.matches("] SEND to").count()
    );
    f.stop();
}

/// E06b: the default `udptl = "summary"` log has one line per datagram, the T.30 frames in
/// hex and no image data at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e06b_default_udptl_log_is_a_summary_without_image_data() {
    let f = faxd("e06b", 2, "", "");
    let dir = scratch("src-e06b");
    let pages = specs(1, false, 260);
    let src = source_tiff(&dir, "src.tif", &pages);
    let rep = run_call(uac(&f, ReinviteBy::Gateway, &src, "0000000001"))
        .await
        .unwrap_or_else(|e| panic!("{e}\n{}", f.log().join("\n")));
    f.wait_results(1);
    let res = result_json(&f, &rep.call_id);
    let fax = assert_success("e06b", &rep, &res, 1, "0000000001", "peer_reinvite");
    let want = fax["udptl"]["rx_datagrams"].as_u64().unwrap()
        + fax["udptl"]["tx_datagrams"].as_u64().unwrap();
    let path = capture_dir(&f, &rep.call_id).join("udptl.log");
    let log = wait_for("udptl.log", || {
        let t = std::fs::read_to_string(&path).ok()?;
        (t.lines().count() as u64 >= want).then_some(t)
    });
    assert_eq!(log.lines().count() as u64, want);
    for l in log.lines() {
        assert!(l.len() < 200, "a summary line is short: {l}");
        assert!(l.split(' ').nth(3).unwrap().starts_with("len="), "{l}");
    }
    for needle in [
        "frame=ffc801",
        "frame=ffc8c1",
        "frame=ffc821",
        "frame=ffc831",
        "frame=ffc8f4",
        "frame=ffc8df",
        "T4NonEcmData(54)",
        "T4NonEcmSigEnd",
    ] {
        assert!(
            log.contains(needle),
            "summary lacks {needle}\n{}",
            &log[..log.len().min(3000)]
        );
    }
    // the page's image octets (the rendered source bitmap compressed by SpanDSP) are not in it:
    // no hex run longer than a T.30 frame appears anywhere
    let longest = log
        .split(|c: char| !c.is_ascii_hexdigit())
        .map(str::len)
        .max()
        .unwrap();
    assert!(longest <= 2 * 40, "longest hex run {longest}");
    eprintln!(
        "e06b: summary udptl.log {} lines, {} bytes",
        log.lines().count(),
        log.len()
    );
    f.stop();
}

// ------------------------------------------------------------------ E07

/// E07: a port range of two ports. Two calls hold both ports, the third gets 503; once the
/// first two are over a later call succeeds.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e07_port_range_exhaustion_503_then_recovery() {
    let f = faxd("e07", 2, "", "");
    let dir = scratch("src-e07");
    let pages = specs(1, false, 260);
    let src = source_tiff(&dir, "src.tif", &pages);

    // two calls that just hold their T.38 session (no fax) for a few seconds
    let mut holders = Vec::new();
    for _ in 0..2 {
        let mut cfg = UacConfig::new(f.addr().parse().unwrap(), ReinviteBy::InitialInvite);
        cfg.hold = Duration::from_secs(4);
        holders.push(tokio::spawn(run_call(cfg)));
    }
    // wait until the server has leased both ports (no fixed sleep)
    wait_for("both UDPTL ports leased", || {
        (f.log()
            .iter()
            .filter(|l| l.contains("UDPTL port leased"))
            .count()
            >= 2)
            .then_some(())
    });
    let third = run_call(uac(&f, ReinviteBy::InitialInvite, &src, "0000000001")).await;
    let err = third.expect_err("the third call must be refused");
    assert!(err.contains("503"), "expected a 503, got: {err}");
    assert!(
        f.log()
            .iter()
            .any(|l| l.contains("no free UDPTL port in [media] port_range")),
        "the exhaustion is logged as an error:\n{}",
        f.log().join("\n")
    );
    for h in holders {
        h.await
            .unwrap()
            .unwrap_or_else(|e| panic!("holder: {e}\n{}", f.log().join("\n")));
    }
    // the two held calls and the refused one are all reported
    let res = f.wait_results(3);
    let refused: Vec<_> = res
        .iter()
        .filter(|r| r["outcome"]["reason"]["kind"] == "no_media_port")
        .collect();
    assert_eq!(refused.len(), 1, "{res:#?}");
    assert!(refused[0]["t38"].is_null());

    // ports are back: a fax now goes through
    let rep = run_call(uac(&f, ReinviteBy::InitialInvite, &src, "0000000001"))
        .await
        .unwrap_or_else(|e| panic!("late call: {e}\n{}", f.log().join("\n")));
    f.wait_results(4);
    let res = result_json(&f, &rep.call_id);
    let fax = assert_success("e07", &rep, &res, 1, "0000000001", "initial_invite");
    let v = verify_files("e07", &fax, &src, &pages);
    eprintln!("{}", summary("e07 (after 503)", &rep, &res, &v));
    f.stop();
}

// ------------------------------------------------------------------ F1-05

/// F1-05: the gateway behaves like an AudioCodes Mediant 1000 (field case F1): every V.21
/// frame that SpanDSP ends with `hdlc-fcs-OK-sig-end` goes out as `hdlc-fcs-OK`, then
/// `hdlc-sig-end` 140 ms later (the `no-signal` after it 140 ms later too). Eight pages through
/// the real `sip-faxd`; the TIFF and the PDF are each compared pixel by pixel with the source
/// (libtiff and poppler, with the negative controls of ADR-025).
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn f1_05_eight_pages_from_a_gateway_that_splits_fcs_ok_and_sig_end() {
    let tag = "f1_05";
    let f = faxd(tag, 2, "", "");
    let dir = scratch("src-f1_05");
    let pages = specs(8, false, 120);
    let src = source_tiff(&dir, "src.tif", &pages);
    let mut cfg = uac(&f, ReinviteBy::Gateway, &src, "0000000001");
    cfg.fax.as_mut().unwrap().quirks = GatewayQuirks {
        split_fcs_sig_end: Some(Duration::from_millis(140)),
        half_duplex: true,
    };
    let rep = run_call(cfg)
        .await
        .unwrap_or_else(|e| panic!("{tag}: {e}\n{}", f.log().join("\n")));
    let results = f.wait_results(1);
    let res = result_json(&f, &rep.call_id);
    assert_eq!(results[0]["call_id"], rep.call_id.as_str());
    // the quirk really was in effect: the DCS, one command per page (8) and the DCN = 10 frames
    let splits = rep.fax.as_ref().unwrap().quirk_splits;
    assert!(splits > 8, "{tag}: only {splits} frames were split");
    let fax = assert_success(tag, &rep, &res, 8, "0000000001", "peer_reinvite");
    // (no response was ever talked into the gateway's own signal: nothing was discarded
    // except what the half duplex model discards by design, which must be nothing here)
    assert_eq!(
        rep.fax.as_ref().unwrap().half_duplex_dropped,
        0,
        "{tag}: the server talked over the gateway's V.21 signal"
    );
    assert_path_layout(tag, &f, &rep.call_id, &fax);
    let v = verify_files(tag, &fax, &src, &pages);
    assert_eq!(v.pages, 8);
    eprintln!("{} | split frames {splits}", summary(tag, &rep, &res, &v));
    f.stop();
}
