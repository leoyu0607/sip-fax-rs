//! fax-in on real localhost sockets with sip-core UACs as the peer: end-to-end call flows,
//! C06 over both transports, the source policy (C01 at the controller level), per-call capture
//! (C11) and trunk registration.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fax_in::{
    CallEvent, CallOutcome, CallResult, FailureReason, FaxIn, FaxInConfig, StartOptions, T38Path,
};
use sip_core::endpoint::{
    DialogEvent, DialogHandle, Endpoint, EndpointConfig, InviteOptions, ReinviteOutcome,
};
use sip_core::message::SipUri;
use sip_core::sdp::negotiate::{AnswerPolicy, OriginTracker, answer_offer};
use sip_core::sdp::{SessionDescription, parse_sdp};
use sip_core::transport::TransportKind;
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

const AUDIO_SDP: &str = "v=0\r\no=Sonus_UAC 1523094871 1523094871 IN IP4 127.0.0.1\r\ns=SIP Media Capabilities\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=audio 16384 RTP/AVP 0 8 101\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:8 PCMA/8000\r\na=rtpmap:101 telephone-event/8000\r\na=fmtp:101 0-15\r\na=ptime:20\r\na=sendrecv\r\n";
const T38_SDP: &str = "v=0\r\no=gw 100 100 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=image 5004 udptl t38\r\na=T38FaxVersion:0\r\na=T38MaxBitRate:14400\r\na=T38FaxRateManagement:transferredTCF\r\na=T38FaxMaxBuffer:1800\r\na=T38FaxMaxDatagram:200\r\na=T38FaxUdpEC:t38UDPRedundancy\r\n";

fn sdp(t: &str) -> SessionDescription {
    parse_sdp(t.as_bytes()).unwrap()
}

/// A per-process unique scratch directory (concurrent test runs must not share one).
fn scratch(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("fax-in-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

fn config(
    capture: Option<(&Path, &str)>,
    trunk_extra: &str,
    sip_extra: &str,
    call_extra: &str,
) -> FaxInConfig {
    let cap = match capture {
        Some((dir, keep)) => format!(
            "[capture]\nenabled = true\ndir = {:?}\nkeep = \"{keep}\"\n",
            dir.display().to_string()
        ),
        None => "[capture]\nenabled = false\n".to_string(),
    };
    FaxInConfig::parse(&format!(
        "[sip]\nbind = \"127.0.0.1:0\"\ntransports = [\"udp\", \"tcp\"]\n{sip_extra}\n\
         [call]\n{call_extra}\n{cap}\n\
         [[trunks]]\nname = \"local\"\nallowed_sources = [\"127.0.0.0/8\"]\n{trunk_extra}\n"
    ))
    .unwrap()
}

async fn start(cfg: FaxInConfig) -> (FaxIn, mpsc::UnboundedReceiver<CallEvent>) {
    FaxIn::start(cfg, StartOptions::default()).await.unwrap()
}

async fn uac() -> Endpoint {
    Endpoint::bind(EndpointConfig::new().udp("127.0.0.1:0".parse().unwrap()))
        .await
        .unwrap()
        .0
}

fn invite_opts(fx: &FaxIn, kind: TransportKind, offer: &str) -> InviteOptions {
    let addr = match kind {
        TransportKind::Udp => fx.udp_addr().unwrap(),
        TransportKind::Tcp => fx.tcp_addr().unwrap(),
    };
    let mut o = InviteOptions::new(
        addr,
        SipUri::parse(&format!("sip:fax@{addr}")).unwrap(),
        SipUri::parse("sip:gw@127.0.0.1").unwrap(),
        SipUri::parse(&format!("sip:fax@{addr}")).unwrap(),
        sdp(offer),
    );
    o.transport = kind;
    o
}

async fn uac_for(kind: TransportKind) -> Endpoint {
    let mut cfg = EndpointConfig::new().udp("127.0.0.1:0".parse().unwrap());
    if kind == TransportKind::Tcp {
        cfg = cfg.tcp("127.0.0.1:0".parse().unwrap());
    }
    Endpoint::bind(cfg).await.unwrap().0
}

async fn next_result(ev: &mut mpsc::UnboundedReceiver<CallEvent>) -> CallResult {
    loop {
        match tokio::time::timeout(Duration::from_secs(10), ev.recv()).await {
            Ok(Some(CallEvent::Finished(r))) => return *r,
            Ok(Some(_)) => {}
            other => panic!("no call result: {other:?}"),
        }
    }
}

async fn next_dialog_event(d: &mut DialogHandle) -> DialogEvent {
    tokio::time::timeout(Duration::from_secs(10), d.next_event())
        .await
        .expect("dialog event within 10 s")
        .expect("dialog alive")
}

fn answer_of(offer: &SessionDescription) -> SessionDescription {
    let policy = AnswerPolicy::new("127.0.0.1", 40000, 40002);
    let mut o = OriginTracker::random("127.0.0.1");
    answer_offer(offer, &policy, &mut o).unwrap()
}

// ================================================================ end-to-end flows

/// Gateway-initiated: audio INVITE, the gateway's own T.38 re-INVITE, BYE (UDP and TCP).
#[tokio::test]
async fn flow_gateway_initiated_t38_reinvite_over_udp_and_tcp() {
    for kind in [TransportKind::Udp, TransportKind::Tcp] {
        let (fx, mut ev) = start(config(None, "t38_policy = \"wait-peer\"\n", "", "")).await;
        let gw = uac_for(kind).await;
        let dlg = gw.invite(invite_opts(&fx, kind, AUDIO_SDP)).await.unwrap();
        // the 200 is an audio answer
        let ans = dlg.remote_sdp().expect("answer");
        assert_eq!(ans.media.len(), 1);
        assert_eq!(ans.media[0].media, "audio");
        assert_eq!(ans.media[0].formats, vec!["0", "8", "101"]);
        assert_eq!(ans.media[0].port, 20000);
        // the gateway switches to T.38
        let out = dlg.reinvite(sdp(T38_SDP)).await.unwrap();
        let ReinviteOutcome::Accepted(t38_ans) = out else {
            panic!("{out:?}")
        };
        assert!(t38_ans.media[0].is_t38());
        assert_eq!(t38_ans.media[0].port, 30000);
        assert_eq!(dlg.bye().await.unwrap().code(), 200);
        let r = next_result(&mut ev).await;
        assert_eq!(r.outcome, CallOutcome::Completed, "{kind:?}");
        assert_eq!(r.trunk, "local");
        let p = r.t38.expect("t38");
        assert_eq!(p.path, T38Path::PeerReinvite);
        assert_eq!(p.peer_media.as_deref(), Some("127.0.0.1:5004"));
        assert!(r.t38_established_at.unwrap() >= r.answered_at.unwrap());
        assert!(r.ended_at >= r.t38_established_at.unwrap());
        gw.shutdown().await;
        fx.shutdown().await;
    }
}

/// Our own re-INVITE after `initiate_after_secs` (real time, 0.2 s here).
#[tokio::test]
async fn flow_we_initiate_the_t38_reinvite() {
    let (fx, mut ev) = start(config(None, "initiate_after_secs = 0.2\n", "", "")).await;
    let gw = uac().await;
    let mut dlg = gw
        .invite(invite_opts(&fx, TransportKind::Udp, AUDIO_SDP))
        .await
        .unwrap();
    let t0 = std::time::Instant::now();
    match next_dialog_event(&mut dlg).await {
        DialogEvent::ReInvite { offer, responder } => {
            let offer = offer.expect("offer");
            let waited = t0.elapsed();
            assert!(waited >= Duration::from_millis(150), "waited {waited:?}");
            let m = &offer.media[0];
            assert!(m.is_t38());
            assert_eq!(m.port, 30000);
            assert_eq!(
                m.attribute("T38FaxUdpEC").unwrap().value.as_deref(),
                Some("t38UDPRedundancy")
            );
            responder.accept(answer_of(&offer)).await.unwrap();
        }
        other => panic!("{other:?}"),
    }
    // wait until the controller has seen the 200 (a BYE sent in the same millisecond could
    // overtake the 200 inside the controller's tasks; a gateway never hangs up that fast)
    loop {
        match tokio::time::timeout(Duration::from_secs(10), ev.recv()).await {
            Ok(Some(CallEvent::T38Established { params, .. })) => {
                assert_eq!(params.path, T38Path::OurReinvite);
                break;
            }
            Ok(Some(_)) => {}
            other => panic!("{other:?}"),
        }
    }
    assert_eq!(dlg.bye().await.unwrap().code(), 200);
    let r = next_result(&mut ev).await;
    assert_eq!(r.outcome, CallOutcome::Completed);
    assert_eq!(r.t38.unwrap().path, T38Path::OurReinvite);
    gw.shutdown().await;
    fx.shutdown().await;
}

/// C06 on real sockets: T.38 in the initial INVITE, both transports.
#[tokio::test]
async fn c06_initial_invite_with_t38_over_udp_and_tcp() {
    for kind in [TransportKind::Udp, TransportKind::Tcp] {
        let (fx, mut ev) = start(config(None, "", "", "")).await;
        let gw = uac_for(kind).await;
        let dlg = gw.invite(invite_opts(&fx, kind, T38_SDP)).await.unwrap();
        let ans = dlg.remote_sdp().unwrap();
        assert_eq!(ans.media.len(), 1);
        assert!(ans.media[0].is_t38());
        assert_eq!(ans.media[0].port, 30000);
        assert_eq!(dlg.bye().await.unwrap().code(), 200);
        let r = next_result(&mut ev).await;
        assert_eq!(r.outcome, CallOutcome::Completed);
        assert_eq!(r.t38.unwrap().path, T38Path::InitialInvite);
        gw.shutdown().await;
        fx.shutdown().await;
    }
}

/// Shutdown hangs up active calls with a BYE and reports them.
#[tokio::test]
async fn shutdown_sends_bye_to_active_calls() {
    let (fx, mut ev) = start(config(None, "", "", "")).await;
    let gw = uac().await;
    let mut dlg = gw
        .invite(invite_opts(&fx, TransportKind::Udp, T38_SDP))
        .await
        .unwrap();
    let shutdown = tokio::spawn(async move { fx.shutdown().await });
    assert!(matches!(
        next_dialog_event(&mut dlg).await,
        DialogEvent::Bye
    ));
    assert!(matches!(
        next_dialog_event(&mut dlg).await,
        DialogEvent::Terminated(_)
    ));
    shutdown.await.unwrap();
    let r = next_result(&mut ev).await;
    assert_eq!(
        r.outcome,
        CallOutcome::Failed {
            reason: FailureReason::Shutdown
        }
    );
    gw.shutdown().await;
}

// ================================================================ source policy

async fn raw_invite(to: SocketAddr, call_id: &str) -> (UdpSocket, String) {
    let s = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let local = s.local_addr().unwrap();
    let text = format!(
        "INVITE sip:fax@{to} SIP/2.0\r\nVia: SIP/2.0/UDP {local};branch=z9hG4bK{call_id};rport\r\n\
         Max-Forwards: 70\r\nFrom: <sip:gw@127.0.0.1>;tag=g1\r\nTo: <sip:fax@{to}>\r\n\
         Call-ID: {call_id}\r\nCSeq: 1 INVITE\r\nContact: <sip:gw@{local}>\r\n\
         Content-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{AUDIO_SDP}",
        AUDIO_SDP.len()
    );
    s.send_to(text.as_bytes(), to).await.unwrap();
    (s, text)
}

async fn recv_text(s: &UdpSocket, ms: u64) -> Option<String> {
    let mut buf = vec![0u8; 4096];
    match tokio::time::timeout(Duration::from_millis(ms), s.recv_from(&mut buf)).await {
        Ok(Ok((n, _))) => Some(String::from_utf8_lossy(&buf[..n]).to_string()),
        _ => None,
    }
}

/// C01 at the application level: the three `unknown_source` modes with real packets from an
/// address that is on no trunk's list.
#[tokio::test]
async fn c01_unknown_source_modes_drop_reject_log() {
    let other = "[sip]\nunknown_source = \"MODE\"\n";
    // the trunk only allows 10.99.0.0/16; we are 127.0.0.1
    let mk = |mode: &str| {
        FaxInConfig::parse(&format!(
            "[sip]\nbind = \"127.0.0.1:0\"\nunknown_source = \"{mode}\"\n[capture]\nenabled = false\n\
             [[trunks]]\nname = \"far\"\nallowed_sources = [\"10.99.0.0/16\"]\n"
        ))
        .unwrap()
    };
    let _ = other;

    // drop: not a single packet comes back, nothing reaches the controller
    let (fx, mut ev) = start(mk("drop")).await;
    let (s, _) = raw_invite(fx.udp_addr().unwrap(), "drop1").await;
    assert_eq!(recv_text(&s, 400).await, None, "dropped silently");
    assert!(ev.try_recv().is_err(), "no call event");
    fx.shutdown().await;

    // reject: 403, and no call
    let (fx, mut ev) = start(mk("reject")).await;
    let (s, _) = raw_invite(fx.udp_addr().unwrap(), "reject1").await;
    let reply = recv_text(&s, 2000).await.expect("a 403");
    assert!(reply.starts_with("SIP/2.0 403 "), "{reply}");
    assert!(reply.contains("Call-ID: reject1"), "{reply}");
    assert!(ev.try_recv().is_err(), "no call event");
    fx.shutdown().await;

    // log: the call is answered by the generic profile and reported as trunk "unknown"
    let (fx, mut ev) = start(mk("log")).await;
    let (s, _) = raw_invite(fx.udp_addr().unwrap(), "log1").await;
    let mut got = Vec::new();
    while let Some(t) = recv_text(&s, 1000).await {
        let done = t.starts_with("SIP/2.0 200 ");
        got.push(t);
        if done {
            break;
        }
    }
    assert!(
        got.last().is_some_and(|t| t.starts_with("SIP/2.0 200 ")),
        "{got:?}"
    );
    match tokio::time::timeout(Duration::from_secs(5), ev.recv()).await {
        Ok(Some(CallEvent::Started { trunk, .. })) => assert_eq!(trunk, "unknown"),
        other => panic!("{other:?}"),
    }
    fx.shutdown().await;
}

// ================================================================ C11 capture

/// One parsed entry of a sip.log.
#[derive(Debug)]
struct Entry {
    ts: String,
    dir: String,
    peer: String,
    transport: String,
    bytes: usize,
    first_line: String,
    raw: String,
}

fn parse_log(text: &str) -> Vec<Entry> {
    let mut out = Vec::new();
    let mut lines = text.split('\n').peekable();
    while let Some(h) = lines.next() {
        if !h.starts_with('[') {
            continue;
        }
        // [2026-10-20T12:34:56.789Z] RECV from 127.0.0.1:5060 (UDP, 412 bytes)
        let (ts, rest) = h[1..].split_once("] ").expect("timestamp");
        let mut it = rest.split(' ');
        let dir = it.next().unwrap().to_string();
        let _prep = it.next().unwrap();
        let peer = it.next().unwrap().to_string();
        let transport = it
            .next()
            .unwrap()
            .trim_start_matches('(')
            .trim_end_matches(',')
            .to_string();
        let bytes: usize = it.next().unwrap().parse().unwrap();
        let mut raw = String::new();
        while raw.len() < bytes {
            let l = lines.next().expect("message body");
            raw.push_str(l);
            raw.push('\n');
        }
        // the writer ends a message that lacks a final newline with one
        let raw_trim = if raw.len() > bytes {
            &raw[..bytes]
        } else {
            &raw[..]
        };
        out.push(Entry {
            ts: ts.to_string(),
            dir,
            peer,
            transport,
            bytes,
            first_line: raw_trim.lines().next().unwrap_or("").to_string(),
            raw: raw_trim.to_string(),
        });
    }
    out
}

fn call_dirs(dir: &Path) -> Vec<String> {
    let mut v: Vec<String> = std::fs::read_dir(dir)
        .map(|r| {
            r.filter_map(|e| e.ok())
                .map(|e| e.file_name().to_string_lossy().to_string())
                .collect()
        })
        .unwrap_or_default();
    v.sort();
    v
}

async fn full_call(fx: &FaxIn, gw: &Endpoint) -> String {
    let dlg = gw
        .invite(invite_opts(fx, TransportKind::Udp, AUDIO_SDP))
        .await
        .unwrap();
    let id = dlg.id().call_id.clone();
    let out = dlg.reinvite(sdp(T38_SDP)).await.unwrap();
    assert!(matches!(out, ReinviteOutcome::Accepted(_)));
    assert_eq!(dlg.bye().await.unwrap().code(), 200);
    id
}

#[tokio::test]
async fn c11_a_call_leaves_sip_log_and_result_json_under_its_call_id() {
    let dir = scratch("c11a");
    let (fx, mut ev) = start(config(
        Some((&dir, "all")),
        "t38_policy = \"wait-peer\"\n",
        "",
        "",
    ))
    .await;
    let gw = uac().await;
    let call_id = full_call(&fx, &gw).await;
    let result = next_result(&mut ev).await;
    assert_eq!(result.call_id, call_id);
    gw.shutdown().await;
    fx.shutdown().await; // flushes the capture writer

    assert_eq!(
        call_dirs(&dir),
        vec![call_id.clone()],
        "exactly one call directory"
    );
    let log = std::fs::read_to_string(dir.join(&call_id).join("sip.log")).unwrap();
    let entries = parse_log(&log);
    let seq: Vec<String> = entries
        .iter()
        .map(|e| {
            let l = e.first_line.replace(" SIP/2.0", "");
            let l: Vec<&str> = l.split(' ').collect();
            let what = if l[0] == "SIP/2.0" {
                format!("{} {}", l[1], l[2])
            } else {
                l[0].to_string()
            };
            format!("{} {}", e.dir, what)
        })
        .collect();
    assert_eq!(
        seq,
        vec![
            "RECV INVITE",
            "SEND 180",
            "SEND 200",
            "RECV ACK",
            "RECV INVITE",
            "SEND 200",
            "RECV ACK",
            "RECV BYE",
            "SEND 200",
        ]
        .into_iter()
        .map(|s| s
            .replace("180", "180 Ringing")
            .replace("SEND 200", "SEND 200 OK"))
        .collect::<Vec<_>>(),
        "log:\n{log}"
    );
    // every entry: RFC 3339 UTC timestamp, UDP, peer = the gateway, byte count = raw length
    let gw_port = entries[0].peer.rsplit(':').next().unwrap().to_string();
    for e in &entries {
        assert_eq!(e.transport, "UDP");
        assert_eq!(e.bytes, e.raw.len(), "{e:?}");
        assert!(e.peer.starts_with("127.0.0.1:"), "{e:?}");
        assert_eq!(e.ts.len(), 24, "{}", e.ts);
        assert!(
            e.ts.ends_with('Z') && e.ts.as_bytes()[10] == b'T',
            "{}",
            e.ts
        );
        assert!(e.raw.contains(&format!("Call-ID: {call_id}")), "{e:?}");
    }
    // the INVITE entry holds the raw SDP offer; timestamps never go backwards
    assert!(
        entries[0].raw.contains("m=audio 16384 RTP/AVP 0 8 101"),
        "{}",
        entries[0].raw
    );
    assert!(entries[0].raw.contains("\r\n"), "raw CRLF preserved");
    assert!(entries.windows(2).all(|w| w[0].ts <= w[1].ts));
    assert!(entries[0].peer.ends_with(&gw_port));
    // the 200 to the first INVITE carries our T.38-less audio answer
    assert!(
        entries[2].raw.contains("m=audio 20000 RTP/AVP"),
        "{}",
        entries[2].raw
    );
    // result.json
    let json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join(&call_id).join("result.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(json["call_id"], call_id.as_str());
    assert_eq!(json["trunk"], "local");
    assert_eq!(json["outcome"]["status"], "completed");
    assert_eq!(json["t38"]["path"], "peer_reinvite");
    assert_eq!(json["t38"]["max_bit_rate"], 14400);
    assert!(json["started_at"].as_str().unwrap().ends_with('Z'));
}

#[tokio::test]
async fn c11_keep_failed_drops_successful_calls_and_keeps_failed_ones() {
    let dir = scratch("c11b");
    let (fx, mut ev) = start(config(
        Some((&dir, "failed")),
        "t38_policy = \"wait-peer\"\n",
        "",
        "",
    ))
    .await;
    let gw = uac().await;
    let ok_id = full_call(&fx, &gw).await;
    assert_eq!(next_result(&mut ev).await.outcome, CallOutcome::Completed);

    // a failed call: unknown Require -> 420
    let mut o = invite_opts(&fx, TransportKind::Udp, AUDIO_SDP);
    o.headers.push(("Require".into(), "FooBar".into()));
    let err = gw.invite(o).await.expect_err("rejected");
    assert!(err.to_string().contains("420"), "{err}");
    let failed = next_result(&mut ev).await;
    assert!(failed.outcome.is_failure());
    gw.shutdown().await;
    fx.shutdown().await;

    let dirs = call_dirs(&dir);
    assert_eq!(
        dirs,
        vec![failed.call_id.clone()],
        "only the failed call is kept, not {ok_id}"
    );
    let log = std::fs::read_to_string(dir.join(&failed.call_id).join("sip.log")).unwrap();
    let entries = parse_log(&log);
    assert_eq!(entries[0].dir, "RECV");
    assert!(
        entries[0]
            .first_line
            .starts_with("INVITE sip:fax@127.0.0.1"),
        "{}",
        entries[0].first_line
    );
    assert!(entries[0].raw.contains("Require: FooBar"));
    assert_eq!(entries[1].dir, "SEND");
    assert!(
        entries[1].first_line.starts_with("SIP/2.0 420 "),
        "{}",
        entries[1].first_line
    );
    assert!(
        entries[1].raw.contains("Unsupported: foobar"),
        "{}",
        entries[1].raw
    );
    let json: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(dir.join(&failed.call_id).join("result.json")).unwrap(),
    )
    .unwrap();
    assert_eq!(json["outcome"]["status"], "failed");
    assert_eq!(json["outcome"]["reason"]["kind"], "unsupported_require");
    assert_eq!(json["outcome"]["reason"]["tags"][0], "foobar");
}

#[tokio::test]
async fn c11_capture_can_be_disabled_and_is_bounded() {
    // disabled: nothing is written
    let dir = scratch("c11c");
    let (fx, mut ev) = start(config(None, "t38_policy = \"wait-peer\"\n", "", "")).await;
    let gw = uac().await;
    full_call(&fx, &gw).await;
    next_result(&mut ev).await;
    gw.shutdown().await;
    fx.shutdown().await;
    assert!(call_dirs(&dir).is_empty());

    // bounded: a tiny max_bytes_per_call keeps the first message only
    let dir = scratch("c11d");
    let mut cfg = config(Some((&dir, "all")), "t38_policy = \"wait-peer\"\n", "", "");
    cfg.capture.max_bytes_per_call = 700;
    let (fx, mut ev) = start(cfg).await;
    let gw = uac().await;
    let id = full_call(&fx, &gw).await;
    next_result(&mut ev).await;
    gw.shutdown().await;
    fx.shutdown().await;
    let log = std::fs::read_to_string(dir.join(&id).join("sip.log")).unwrap();
    assert!(log.contains("further message(s) not stored"), "{log}");
    assert!(log.len() < 4000, "{} bytes", log.len());
}

// ================================================================ registration

#[tokio::test]
async fn trunk_registration_sends_register_with_the_configured_identity() {
    let registrar = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let raddr = registrar.local_addr().unwrap();
    let probe = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
    let our_port = probe.local_addr().unwrap().port();
    drop(probe);
    let cfg = FaxInConfig::parse(&format!(
        "[sip]\nbind = \"127.0.0.1:{our_port}\"\n[capture]\nenabled = false\n\
         [[trunks]]\nname = \"reg\"\nallowed_sources = [\"127.0.0.1\"]\n\
         [trunks.register]\nenabled = true\nregistrar = \"{raddr}\"\ndomain = \"example.test\"\n\
         username = \"fax1\"\npassword = \"pw\"\nexpires = 600\n"
    ))
    .unwrap();
    // a registrar that answers every REGISTER with 200 (and hands the first one to the test)
    let (first_tx, mut first_rx) = mpsc::unbounded_channel::<String>();
    let reg_task = tokio::spawn(async move {
        let mut buf = vec![0u8; 4096];
        loop {
            let Ok((n, from)) = registrar.recv_from(&mut buf).await else {
                return;
            };
            let req = String::from_utf8_lossy(&buf[..n]).to_string();
            let h = |name: &str| {
                req.lines()
                    .find(|l| l.starts_with(name))
                    .unwrap_or("")
                    .to_string()
            };
            let ok = format!(
                "SIP/2.0 200 OK\r\n{}\r\n{}\r\n{};tag=reg1\r\n{}\r\n{}\r\nExpires: 600\r\nContent-Length: 0\r\n\r\n",
                h("Via:"),
                h("From:"),
                h("To:"),
                h("Call-ID:"),
                h("CSeq:")
            );
            let _ = first_tx.send(req);
            let _ = registrar.send_to(ok.as_bytes(), from).await;
        }
    });
    let (fx, _ev) = start(cfg).await;
    let req = tokio::time::timeout(Duration::from_secs(5), first_rx.recv())
        .await
        .expect("REGISTER")
        .unwrap();
    assert!(
        req.starts_with("REGISTER sip:example.test SIP/2.0\r\n"),
        "{req}"
    );
    assert!(req.contains("\r\nTo: <sip:fax1@example.test>"), "{req}");
    assert!(
        req.contains("\r\nFrom: <sip:fax1@example.test>;tag="),
        "{req}"
    );
    assert!(
        req.contains(&format!("\r\nContact: <sip:fax1@127.0.0.1:{our_port}>")),
        "{req}"
    );
    assert!(req.contains("\r\nExpires: 600"), "{req}");
    let mut registered = false;
    for _ in 0..50 {
        let st = fx.registrations();
        assert_eq!(st[0].0, "reg");
        if matches!(
            st[0].1,
            sip_core::registration::RegistrationState::Registered { .. }
        ) {
            registered = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(registered, "{:?}", fx.registrations());
    fx.shutdown().await;
    // shutdown de-registers (Expires: 0)
    let dereg = tokio::time::timeout(Duration::from_secs(5), first_rx.recv())
        .await
        .expect("de-REGISTER")
        .unwrap();
    assert!(dereg.contains("\r\nExpires: 0"), "{dereg}");
    reg_task.abort();
}
