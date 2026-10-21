//! The real T.38 fax session (`fax_in::t38session`) over real loopback UDP sockets, with the
//! engine's pacing turned off so that a call takes milliseconds. The fax machine on the other
//! end is a small scripted caller in this file (hand-built IFP packets, UDPTL from the `t38`
//! crate); the SpanDSP version of the same thing is in `tools/fax-sim/tests/e2e_m2d.rs`.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use fax_in::capture::Capture;
use fax_in::config::{CaptureConfig, KeepMode, UdptlLogMode};
use fax_in::event::{FaxReport, ReportSink, T38Negotiated, T38Path};
use fax_in::session::{FaxSessionFactory, SessionContext, SessionOutcome, UdptlSettings};
use fax_in::t38engine::Pacing;
use fax_in::t38session::{T38SessionEnv, T38SessionFactory};
use t38::{
    DataType, ErrorRecovery, Field, FieldType, Ifp, Indicator, ReceiverConfig, RxEvent, T38Version,
    UdptlReceiver, UdptlSender,
};
use tokio::net::UdpSocket;
use tokio::sync::watch;

// ---------------------------------------------------------------- the scripted caller

fn data(dt: DataType, ft: FieldType, bytes: &[u8]) -> Ifp {
    Ifp::Data {
        data_type: dt,
        fields: vec![Field::new(ft, bytes.to_vec())],
    }
}

fn hexs(s: &str) -> Vec<u8> {
    s.split_whitespace()
        .map(|x| u8::from_str_radix(x, 16).unwrap())
        .collect()
}

fn tsi(id: &str) -> Vec<u8> {
    let mut v = vec![0xFF, 0xC0, 0xC2];
    let mut f = vec![0x04u8; 20];
    for (i, c) in id.bytes().rev().enumerate() {
        f[i] = c.reverse_bits();
    }
    v.extend(f);
    v
}

const DCS: &str = "FF C8 C1 00 44 1E"; // receive, V.17 14400, standard, MH
const MPS: &str = "FF C8 F2";
const EOP: &str = "FF C8 F4";
const DCN: &str = "FF C8 DF";

fn bitmap(rows: usize, seed: u32) -> Vec<u8> {
    let mut v = vec![0u8; 216 * rows];
    for y in 0..rows {
        for x in 0..216 {
            let k = (y as u32 * 31 + x as u32 * 7 + seed) % 11;
            v[y * 216 + x] = match k {
                0 => 0xFF,
                1 | 2 => 0xF0,
                3 => 0x0F,
                _ => 0x00,
            };
        }
    }
    v
}

fn mh(bm: &[u8], rows: usize) -> Vec<u8> {
    let mut e = t4::Encoder::new(1728, t4::Coding::Mh).unwrap();
    for y in 0..rows {
        e.push_row(&bm[y * 216..(y + 1) * 216]).unwrap();
    }
    e.finish()
}

struct Caller {
    sock: UdpSocket,
    to: SocketAddr,
    tx: UdptlSender,
    rx: UdptlReceiver,
    heard: Vec<Ifp>,
    version: T38Version,
}

impl Caller {
    async fn new(to: SocketAddr) -> Caller {
        Caller::on(UdpSocket::bind("127.0.0.1:0").await.unwrap(), to)
    }

    fn on(sock: UdpSocket, to: SocketAddr) -> Caller {
        Caller {
            sock,
            to,
            tx: UdptlSender::new(ErrorRecovery::Redundancy { depth: 3 }, 0),
            rx: UdptlReceiver::new(ReceiverConfig::default()),
            heard: Vec::new(),
            version: T38Version::V0,
        }
    }

    async fn send(&mut self, ifp: &Ifp) {
        let bytes = ifp.encode(self.version).unwrap();
        let dg = self.tx.build(&bytes).unwrap();
        self.sock.send_to(&dg, self.to).await.unwrap();
    }

    async fn burst(&mut self, frames: &[&[u8]]) {
        self.send(&Ifp::Indicator(Indicator::V21Preamble)).await;
        for (i, f) in frames.iter().enumerate() {
            self.send(&data(DataType::V21, FieldType::HdlcData, f))
                .await;
            let end = if i + 1 == frames.len() {
                FieldType::HdlcFcsOkSigEnd
            } else {
                FieldType::HdlcFcsOk
            };
            self.send(&data(DataType::V21, end, &[])).await;
        }
        self.send(&Ifp::Indicator(Indicator::NoSignal)).await;
    }

    async fn image(&mut self, bytes: &[u8]) {
        let dt = DataType::V17_14400;
        self.send(&Ifp::Indicator(Indicator::V17_14400LongTraining))
            .await;
        for c in bytes.chunks(54) {
            self.send(&data(dt, FieldType::T4NonEcmData, c)).await;
        }
        self.send(&data(dt, FieldType::T4NonEcmSigEnd, &[])).await;
        self.send(&Ifp::Indicator(Indicator::NoSignal)).await;
    }

    /// Reads datagrams until `done` accepts the packets heard so far.
    async fn wait(&mut self, what: &str, done: impl Fn(&[Ifp]) -> bool) {
        let mut buf = vec![0u8; 4096];
        let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
        while !done(&self.heard) {
            let (n, _) = tokio::time::timeout_at(deadline, self.sock.recv_from(&mut buf))
                .await
                .unwrap_or_else(|_| panic!("timed out waiting for {what}; heard {:?}", self.heard))
                .unwrap();
            for e in self.rx.on_datagram(&buf[..n]).unwrap() {
                if let RxEvent::Packet { ifp, .. } = e {
                    self.heard
                        .push(Ifp::decode_prefix(&ifp, self.version).unwrap().0);
                }
            }
        }
    }

    /// Waits until the server has sent `n` frames in total (CSI and DIS are two).
    async fn wait_frames(&mut self, n: usize) {
        self.wait("a frame from the server", |h| server_fcfs(h).len() >= n)
            .await;
    }
}

/// DIS, CFR, MCF, ... in the order the server sent them.
fn server_fcfs(heard: &[Ifp]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut cur = Vec::new();
    for p in heard {
        if let Ifp::Data { fields, .. } = p {
            for f in fields {
                match f.field_type {
                    FieldType::HdlcData => cur.extend(&f.data),
                    FieldType::HdlcFcsOk | FieldType::HdlcFcsOkSigEnd if !cur.is_empty() => {
                        out.push(cur[2]);
                        cur.clear();
                    }
                    _ => {}
                }
            }
        }
    }
    out
}

// ---------------------------------------------------------------- the session under test

fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port()
}

struct Run {
    handle: tokio::task::JoinHandle<SessionOutcome>,
    report: ReportSink,
    cancel: watch::Sender<bool>,
    data_dir: PathBuf,
}

fn negotiated(local_port: u16, peer: SocketAddr) -> T38Negotiated {
    T38Negotiated {
        path: T38Path::PeerReinvite,
        version: 0,
        max_bit_rate: 14_400,
        rate_management: "transferredTCF".into(),
        max_buffer: 1800,
        local_max_datagram: 260,
        peer_max_datagram: 260,
        udp_ec: "t38UDPRedundancy".into(),
        fill_bit_removal: false,
        transcoding_mmr: false,
        transcoding_jbig: false,
        peer_media: Some(peer.to_string()),
        local_port,
        warnings: Vec::new(),
    }
}

fn scratch(name: &str) -> PathBuf {
    let d = std::env::temp_dir().join(format!("fax-in-t38session-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn env(data_dir: &Path, capture: Option<Arc<Capture>>, max: Duration) -> T38SessionEnv {
    T38SessionEnv {
        data_dir: data_dir.to_path_buf(),
        capture,
        bind_ip: "127.0.0.1".parse().unwrap(),
        station_id: "0287654321".into(),
        max_duration: max,
        pacing: Pacing::none(),
    }
}

fn start_with(
    name: &str,
    call_id: &str,
    peer: SocketAddr,
    port: u16,
    capture: Option<Arc<Capture>>,
    max: Duration,
) -> Run {
    let data_dir = scratch(name);
    let (cancel, cancel_rx) = watch::channel(false);
    let report: ReportSink = Arc::default();
    let ctx = SessionContext {
        call_id: call_id.into(),
        trunk: "t".into(),
        t38: negotiated(port, peer),
        udptl: UdptlSettings::default(),
        report: report.clone(),
        cancel: cancel_rx,
    };
    let factory = T38SessionFactory::new(env(&data_dir, capture, max));
    let session = factory.create(&ctx);
    let handle = tokio::spawn(session.run(ctx));
    Run {
        handle,
        report,
        cancel,
        data_dir,
    }
}

fn start(name: &str, call_id: &str, peer: SocketAddr, port: u16) -> Run {
    start_with(name, call_id, peer, port, None, Duration::from_secs(60))
}

impl Run {
    async fn finish(self) -> (SessionOutcome, FaxReport, PathBuf) {
        let out = tokio::time::timeout(Duration::from_secs(15), self.handle)
            .await
            .expect("session did not finish")
            .unwrap();
        let rep = self.report.lock().unwrap().clone().expect("a fax report");
        (out, rep, self.data_dir)
    }
}

/// Plays DCS, TCF and `pages` pages; the last one ends with EOP, `then` follows.
async fn play(c: &mut Caller, pages: &[(Vec<u8>, usize)], finish_with_dcn: bool) {
    c.wait_frames(2).await; // CSI, DIS
    c.burst(&[&tsi("12345678"), &hexs(DCS)]).await;
    c.image(&vec![0u8; 2700]).await;
    c.wait_frames(3).await; // CFR
    for (i, (bm, rows)) in pages.iter().enumerate() {
        c.image(&mh(bm, *rows)).await;
        let cmd = if i + 1 == pages.len() { EOP } else { MPS };
        c.burst(&[&hexs(cmd)]).await;
        c.wait_frames(4 + i).await; // MCF
    }
    if finish_with_dcn {
        c.burst(&[&hexs(DCN)]).await;
    }
}

// ---------------------------------------------------------------- tests

#[tokio::test]
async fn s01_a_two_page_fax_is_received_and_stored() {
    let caller_port = free_udp_port();
    let port = free_udp_port();
    let to = SocketAddr::from(([127, 0, 0, 1], port));
    let sock = UdpSocket::bind(("127.0.0.1", caller_port)).await.unwrap();
    let run = start("s01", "call/1@host", sock.local_addr().unwrap(), port);
    let mut c = Caller::on(sock, to);
    let pages = vec![(bitmap(120, 0), 120), (bitmap(130, 1), 130)];
    play(&mut c, &pages, true).await;
    assert_eq!(server_fcfs(&c.heard), vec![0x02, 0x01, 0x21, 0x31, 0x31]);
    let (out, rep, dir) = run.finish().await;
    assert_eq!(out, SessionOutcome::Completed);
    assert_eq!((rep.code, rep.code_name.as_str()), (0, "ok"));
    assert_eq!(rep.pages, 2);
    assert_eq!(rep.bit_rate, 14_400);
    assert_eq!(rep.remote_ident.as_deref(), Some("12345678"));
    assert_eq!(rep.bad_lines_per_page, vec![0, 0]);
    assert!(!rep.partial && rep.store_error.is_none());
    assert_eq!(rep.compression.as_deref(), Some("mh"));
    assert_eq!(rep.resolution.as_deref(), Some("standard"));
    assert!(rep.udptl.rx_datagrams > 100 && rep.udptl.tx_datagrams > 3);
    // files: <data>/faxes/YYYY/MM/DD/<sanitised call-id>-<hash>.tif|pdf
    let tif = PathBuf::from(rep.tiff.clone().unwrap());
    let pdf = PathBuf::from(rep.pdf.clone().unwrap());
    let rel = tif.strip_prefix(&dir).unwrap();
    let parts: Vec<_> = rel.iter().map(|p| p.to_str().unwrap()).collect();
    assert_eq!(parts.len(), 5, "{parts:?}");
    assert_eq!(parts[0], "faxes");
    assert!(
        parts[4].starts_with("call_1@host-") && parts[4].ends_with(".tif"),
        "{parts:?}"
    );
    assert_eq!(tif.file_stem(), pdf.file_stem());
    let tiff_bytes = std::fs::read(&tif).unwrap();
    assert_eq!(&tiff_bytes[..4], b"II*\0");
    assert!(std::fs::read(&pdf).unwrap().starts_with(b"%PDF"));
    assert!(
        String::from_utf8_lossy(&tiff_bytes).contains("TSI: 12345678"),
        "the TSI is recorded in ImageDescription"
    );
    // no temporary files are left behind
    let left: Vec<_> = std::fs::read_dir(tif.parent().unwrap())
        .unwrap()
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .collect();
    assert_eq!(left.len(), 2, "{left:?}");
}

#[tokio::test]
async fn s02_a_fax_that_breaks_off_keeps_its_finished_pages_flagged_partial() {
    let port = free_udp_port();
    let to = SocketAddr::from(([127, 0, 0, 1], port));
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let run = start("s02", "partial-1", sock.local_addr().unwrap(), port);
    let mut c = Caller::on(sock, to);
    c.wait_frames(2).await;
    c.burst(&[&tsi("55555"), &hexs(DCS)]).await;
    c.image(&vec![0u8; 2700]).await;
    c.wait_frames(3).await;
    let (bm, rows) = (bitmap(120, 3), 120);
    c.image(&mh(&bm, rows)).await;
    c.burst(&[&hexs(MPS)]).await;
    c.wait_frames(4).await;
    // page 2 starts and the caller hangs up in the middle
    let half = mh(&bitmap(120, 4), 120);
    c.send(&Ifp::Indicator(Indicator::V17_14400LongTraining))
        .await;
    c.send(&data(
        DataType::V17_14400,
        FieldType::T4NonEcmData,
        &half[..half.len() / 2],
    ))
    .await;
    c.burst(&[&hexs(DCN)]).await;
    let (out, rep, _) = run.finish().await;
    assert!(
        matches!(out, SessionOutcome::Failed(ref d) if d.contains("partial_dcn")),
        "{out:?}"
    );
    assert_eq!((rep.pages, rep.partial), (1, true));
    assert_eq!(rep.code_name, "partial_dcn");
    assert_ne!(rep.code, 0);
    assert!(
        PathBuf::from(rep.tiff.unwrap()).is_file(),
        "partial result stored"
    );
    assert!(PathBuf::from(rep.pdf.unwrap()).is_file());
}

#[tokio::test]
async fn s03_cancel_before_anything_happens_reports_aborted_without_files() {
    let port = free_udp_port();
    let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let run = start("s03", "cancelled", silent.local_addr().unwrap(), port);
    let mut c = Caller::on(silent, SocketAddr::from(([127, 0, 0, 1], port)));
    c.wait_frames(2).await; // the DIS goes out
    run.cancel.send(true).unwrap();
    let (out, rep, dir) = run.finish().await;
    assert_eq!(out, SessionOutcome::Aborted);
    assert_eq!((rep.code_name.as_str(), rep.pages), ("aborted", 0));
    assert!(rep.tiff.is_none() && rep.pdf.is_none());
    assert!(!dir.join("faxes").exists());
}

#[tokio::test]
async fn s04_peer_hangup_after_the_last_mcf_counts_as_success() {
    let port = free_udp_port();
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let run = start("s04", "no-dcn", sock.local_addr().unwrap(), port);
    let mut c = Caller::on(sock, SocketAddr::from(([127, 0, 0, 1], port)));
    play(&mut c, &[(bitmap(100, 5), 100)], false).await;
    // the gateway ends the call (BYE) without ever sending the DCN
    run.cancel.send(true).unwrap();
    let (out, rep, _) = run.finish().await;
    assert_eq!(
        out,
        SessionOutcome::Aborted,
        "the controller turns this into Completed"
    );
    assert_eq!((rep.code, rep.pages, rep.partial), (0, 1, false));
    assert!(rep.tiff.is_some());
}

#[tokio::test]
async fn s05_the_maximum_duration_ends_the_session() {
    let port = free_udp_port();
    let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let run = start_with(
        "s05",
        "slow",
        silent.local_addr().unwrap(),
        port,
        None,
        Duration::from_millis(300),
    );
    let (out, rep, _) = run.finish().await;
    assert_eq!(out, SessionOutcome::MaxDuration);
    assert_eq!(rep.code_name, "aborted");
    drop(silent);
}

#[tokio::test]
async fn s06_a_port_that_is_taken_fails_the_session() {
    let taken = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let port = taken.local_addr().unwrap().port();
    let out = {
        let (cancel, cancel_rx) = watch::channel(false);
        let ctx = SessionContext {
            call_id: "busy".into(),
            trunk: "t".into(),
            t38: negotiated(port, "127.0.0.1:9".parse().unwrap()),
            udptl: UdptlSettings::default(),
            report: Arc::default(),
            cancel: cancel_rx,
        };
        let f = T38SessionFactory::new(env(&scratch("s06"), None, Duration::from_secs(5)));
        let out = f.create(&ctx).run(ctx).await;
        drop(cancel);
        out
    };
    assert!(
        matches!(out, SessionOutcome::Failed(ref d) if d.contains("cannot bind")),
        "{out:?}"
    );
}

#[tokio::test]
async fn s07_the_peer_address_is_latched_and_other_sources_are_ignored() {
    let port = free_udp_port();
    // the SDP names a port nobody listens on; the real caller sends from another one
    let wrong: SocketAddr = format!("127.0.0.1:{}", free_udp_port()).parse().unwrap();
    let run = start("s07", "latch", wrong, port);
    let to = SocketAddr::from(([127, 0, 0, 1], port));
    let mut c = Caller::new(to).await;
    // first contact: a valid UDPTL packet from the real caller
    c.send(&Ifp::Indicator(Indicator::NoSignal)).await;
    c.wait_frames(2).await; // the DIS is answered to the latched address, not to `wrong`
    // a stranger now talks to the port: ignored
    let stranger = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    stranger.send_to(&[0, 1, 2, 3], to).await.unwrap();
    // and garbage from the latched peer is counted, not fatal
    c.sock.send_to(&[9], to).await.unwrap();
    tokio::time::sleep(Duration::from_millis(100)).await;
    run.cancel.send(true).unwrap();
    let (_, rep, _) = run.finish().await;
    assert_eq!(rep.udptl.rx_foreign, 1, "{:?}", rep.udptl);
    assert!(rep.udptl.rx_malformed >= 1, "{:?}", rep.udptl);
    assert!(rep.udptl.rx_datagrams >= 3);
}

#[tokio::test]
async fn s08_udptl_log_lines_follow_the_keep_and_size_rules() {
    let dir = scratch("s08-cap");
    for (keep, failed, expect_file) in [
        (KeepMode::All, false, true),
        (KeepMode::Failed, false, false),
        (KeepMode::Failed, true, true),
    ] {
        let cfg = CaptureConfig {
            enabled: true,
            dir: dir.clone(),
            keep,
            max_bytes_per_call: 100_000,
            udptl: UdptlLogMode::Full,
        };
        let (cap, _tap) = Capture::start(&cfg, dir.clone());
        let id = format!("cap-{keep:?}-{failed}");
        cap.begin(&id);
        let port = free_udp_port();
        let silent = UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let run = start_with(
            "s08",
            &id,
            silent.local_addr().unwrap(),
            port,
            Some(cap.clone()),
            Duration::from_secs(30),
        );
        let mut c = Caller::on(silent, SocketAddr::from(([127, 0, 0, 1], port)));
        c.wait_frames(2).await;
        c.send(&Ifp::Indicator(Indicator::NoSignal)).await;
        run.cancel.send(true).unwrap();
        let (_, rep, _) = run.finish().await;
        cap.end(&id, failed, "{}".into());
        cap.shutdown().await;
        let path = dir.join(fax_in::capture::dir_name(&id)).join("udptl.log");
        assert_eq!(path.is_file(), expect_file, "{keep:?} failed={failed}");
        if expect_file {
            let text = std::fs::read_to_string(&path).unwrap();
            let lines: Vec<_> = text.lines().collect();
            assert_eq!(
                lines.len() as u64,
                rep.udptl.rx_datagrams + rep.udptl.tx_datagrams,
                "one line per datagram"
            );
            let tx = lines
                .iter()
                .filter(|l| l.split(' ').nth(1) == Some("TX"))
                .count();
            assert_eq!(tx as u64, rep.udptl.tx_datagrams);
            for l in &lines {
                let f: Vec<_> = l.split(' ').collect();
                assert_eq!(f.len(), 4, "{l}");
                assert!(f[0].ends_with('Z') && f[2].parse::<u16>().is_ok());
                assert!(f[3].len() % 2 == 0 && f[3].chars().all(|c| c.is_ascii_hexdigit()));
            }
        }
    }
    // the size cap is honoured and reported
    let cfg = CaptureConfig {
        enabled: true,
        dir: dir.clone(),
        keep: KeepMode::All,
        max_bytes_per_call: 300,
        udptl: UdptlLogMode::Full,
    };
    let (cap, _tap) = Capture::start(&cfg, dir.clone());
    cap.begin("small");
    for i in 0..20u8 {
        cap.udptl(
            "small",
            SystemTime::UNIX_EPOCH,
            i % 2 == 0,
            &[0, i, 0xAB, 0xCD],
            T38Version::V0,
            &mut Vec::new(),
        );
    }
    cap.end("small", false, "{}".into());
    cap.shutdown().await;
    let text = std::fs::read_to_string(dir.join("small").join("udptl.log")).unwrap();
    assert!(
        text.lines().count() < 20 && text.contains("further datagram(s) not stored"),
        "{text}"
    );
    assert!(
        text.starts_with("1970-01-01T00:00:00.000Z TX 0 0000abcd\n"),
        "{text}"
    );
}

#[test]
fn s09_the_recovery_scheme_follows_the_negotiated_ec() {
    use fax_in::t38session::recovery_for;
    let s = UdptlSettings {
        redundancy_depth: 1,
        fec_span: 2,
        fec_entries: 3,
    };
    assert_eq!(
        recovery_for("t38UDPFEC", &s),
        ErrorRecovery::Fec {
            span: 2,
            entries: 3
        }
    );
    assert_eq!(recovery_for("t38UDPNoEC", &s), ErrorRecovery::None);
    assert_eq!(
        recovery_for("t38UDPRedundancy", &s),
        ErrorRecovery::Redundancy { depth: 1 }
    );
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

/// Runs a one-page fax with capture in `mode`; returns the udptl.log text (if any) and the
/// hex of the first octets of the page's image data.
async fn captured_fax(mode: UdptlLogMode, name: &str) -> (Option<String>, String) {
    let dir = scratch(name);
    let cfg = CaptureConfig {
        enabled: true,
        dir: dir.clone(),
        keep: KeepMode::All,
        max_bytes_per_call: 1 << 20,
        udptl: mode,
    };
    let (cap, _tap) = Capture::start(&cfg, dir.clone());
    cap.begin("priv");
    let port = free_udp_port();
    let sock = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let run = start_with(
        name,
        "priv",
        sock.local_addr().unwrap(),
        port,
        Some(cap.clone()),
        Duration::from_secs(30),
    );
    let mut c = Caller::on(sock, SocketAddr::from(([127, 0, 0, 1], port)));
    let page = mh(&bitmap(100, 5), 100);
    play(&mut c, &[(bitmap(100, 5), 100)], true).await;
    let (out, rep, _) = run.finish().await;
    assert_eq!(out, SessionOutcome::Completed);
    assert_eq!(rep.pages, 1);
    cap.end("priv", false, "{}".into());
    cap.shutdown().await;
    let path = dir.join("priv").join("udptl.log");
    (std::fs::read_to_string(path).ok(), hex(&page[..16]))
}

#[tokio::test]
async fn s10_summary_log_has_the_t30_frames_but_no_image_data() {
    let (log, image) = captured_fax(UdptlLogMode::Summary, "s10").await;
    let log = log.expect("udptl.log");
    assert!(
        !log.contains(&image),
        "image octets {image} leaked into the summary log"
    );
    // no long hex at all: only T.30 control frames (DIS/DCS are the longest, < 40 octets)
    for l in log.lines() {
        assert!(l.len() < 200, "{l}");
        let f: Vec<_> = l.split(' ').collect();
        assert!(f[1] == "RX" || f[1] == "TX");
        assert!(f[2].parse::<u16>().is_ok());
        assert!(f[3].starts_with("len="), "{l}");
    }
    // the T.30 frames are there: our CSI/DIS (TX) and the caller's DCS, MPS/EOP, DCN (RX)
    assert!(log.contains("TX") && log.contains("HdlcData("));
    assert!(log.contains("frame=ffc801"), "DIS in hex:\n{log}");
    assert!(log.contains("frame=ffc8c100441e"), "DCS in hex");
    assert!(
        log.contains("frame=ffc821") && log.contains("frame=ffc831"),
        "CFR and MCF"
    );
    assert!(
        log.contains("frame=ffc8f4") && log.contains("frame=ffc8df"),
        "EOP and DCN"
    );
    assert!(log.contains("ind=V21Preamble") && log.contains("ind=V17_14400LongTraining"));
    assert!(
        log.contains("data=V17_14400 T4NonEcmData(54)"),
        "lengths only"
    );
    assert!(log.contains("T4NonEcmSigEnd(0)"));
}

#[tokio::test]
async fn s11_full_log_has_the_image_data_and_off_writes_nothing() {
    let (log, image) = captured_fax(UdptlLogMode::Full, "s11").await;
    let log = log.expect("udptl.log");
    assert!(log.contains(&image), "full mode records the image octets");
    let (log, _) = captured_fax(UdptlLogMode::Off, "s12").await;
    assert!(log.is_none(), "off writes no udptl.log");
}

#[test]
fn s12_default_capture_mode_is_summary() {
    assert_eq!(CaptureConfig::default().udptl, UdptlLogMode::Summary);
    let c = fax_in::FaxInConfig::parse("[capture]\nudptl = \"full\"\n").unwrap();
    assert_eq!(c.capture.udptl, UdptlLogMode::Full);
    assert!(fax_in::FaxInConfig::parse("[capture]\nudptl = \"x\"\n").is_err());
}
