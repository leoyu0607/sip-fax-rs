//! M5e: calls that stay on audio get the real G.711 session (localhost sockets, a sip-core UAC
//! as the gateway and a plain UDP socket as its RTP). No fax machine on the other side: these
//! check the wiring (the RTP port, our RTP and CED, the fallback after a refused T.38
//! re-INVITE, the call result and `audio.log`); the SpanDSP calls are the E tests in Docker.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use fax_in::{CallEvent, CallResult, FaxIn, FaxInConfig, FaxMedia, StartOptions};
use sip_core::endpoint::{DialogEvent, DialogHandle, Endpoint, EndpointConfig, InviteOptions};
use sip_core::message::{SipUri, StatusCode};
use sip_core::rtp::{Law, RtpPacket};
use sip_core::sdp::{SessionDescription, parse_sdp};
use tokio::net::UdpSocket;
use tokio::sync::mpsc;

fn scratch(name: &str) -> PathBuf {
    let d = PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
        .join(format!("fax-in-m5e-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&d);
    std::fs::create_dir_all(&d).unwrap();
    d
}

/// The configuration of one test; `ports` is a range of its own (the tests run in parallel).
fn config(dir: &Path, ports: &str, trunk_extra: &str) -> FaxInConfig {
    FaxInConfig::parse(&format!(
        "[sip]\nbind = \"127.0.0.1:0\"\ntransports = [\"udp\"]\n\
         [media]\nport_range = \"{ports}\"\n\
         [call]\nfax_session = \"t38\"\n\
         [storage]\ndata_dir = {:?}\n\
         [capture]\nenabled = true\ndir = {:?}\nkeep = \"all\"\n\
         [[trunks]]\nname = \"local\"\nallowed_sources = [\"127.0.0.0/8\"]\n{trunk_extra}\n",
        dir.join("data").display().to_string(),
        dir.join("calls").display().to_string(),
    ))
    .unwrap()
}

/// An audio offer whose RTP goes to `rtp`.
fn audio_offer(rtp: SocketAddr) -> SessionDescription {
    let text = format!(
        "v=0\r\no=gw 1 1 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\n\
         m=audio {} RTP/AVP 0 8 101\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:8 PCMA/8000\r\n\
         a=rtpmap:101 telephone-event/8000\r\na=ptime:20\r\na=sendrecv\r\n",
        rtp.port()
    );
    parse_sdp(text.as_bytes()).unwrap()
}

async fn gateway(fx: &FaxIn, rtp: SocketAddr) -> (Endpoint, DialogHandle) {
    let gw = Endpoint::bind(EndpointConfig::new().udp("127.0.0.1:0".parse().unwrap()))
        .await
        .unwrap()
        .0;
    let addr = fx.udp_addr().unwrap();
    let uri = SipUri::parse(&format!("sip:fax@{addr}")).unwrap();
    let opts = InviteOptions::new(
        addr,
        uri.clone(),
        SipUri::parse("sip:gw@127.0.0.1").unwrap(),
        uri,
        audio_offer(rtp),
    );
    let dlg = gw.invite(opts).await.unwrap();
    (gw, dlg)
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

/// Receives our RTP for `ms` while sending silence back (PCMA, so the session also takes the
/// other law); returns the decoded audio and the packets.
async fn exchange_rtp(sock: &UdpSocket, to: SocketAddr, ms: u64) -> (Vec<i16>, Vec<RtpPacket>) {
    let mut audio = Vec::new();
    let mut packets = Vec::new();
    let mut buf = [0u8; 2048];
    let mut seq = 0u16;
    let end = tokio::time::Instant::now() + Duration::from_millis(ms);
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    loop {
        tokio::select! {
            () = tokio::time::sleep_until(end) => break,
            _ = tick.tick() => {
                let p = RtpPacket::new(8, seq, u32::from(seq) * 160, 0x1234, vec![Law::Alaw.silence(); 160]);
                seq = seq.wrapping_add(1);
                let _ = sock.send_to(&p.encode(), to).await;
            }
            r = sock.recv_from(&mut buf) => {
                // (Windows reports an ICMP port unreachable for a send before the session's
                // socket was bound as an error of the next receive)
                let Ok((n, _)) = r else { continue };
                let p = RtpPacket::decode(&buf[..n]).unwrap();
                Law::from_payload_type(p.payload_type).unwrap().decode(&p.payload, &mut audio);
                packets.push(p);
            }
        }
    }
    (audio, packets)
}

/// Zero crossings per second of `s`, halved: the frequency of a tone.
fn tone_hz(s: &[i16]) -> f64 {
    let crossings = s.windows(2).filter(|w| (w[0] < 0) != (w[1] < 0)).count();
    crossings as f64 / 2.0 / (s.len() as f64 / 8000.0)
}

/// `audio-only`: the call is answered on audio with an RTP port from the pool, we send RTP at
/// once and, 200 ms in, CED; a BYE ends it with a G.711 fax report and `audio.log`.
#[tokio::test]
async fn e00_audio_only_answers_with_rtp_and_ced_from_its_own_port() {
    let dir = scratch("audio-only");
    let (fx, mut ev) = FaxIn::start(
        config(&dir, "31000-31099", "t38_policy = \"audio-only\"\n"),
        StartOptions::default(),
    )
    .await
    .unwrap();
    let rtp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let (gw, dlg) = gateway(&fx, rtp.local_addr().unwrap()).await;
    let ans = dlg.remote_sdp().expect("answer");
    let m = &ans.media[0];
    assert_eq!(m.media, "audio");
    assert!(
        (31000..=31099).contains(&m.port) && m.port % 2 == 0,
        "RTP port {} not from the pool",
        m.port
    );
    assert_eq!(m.formats[0], "0", "PCMU first, as offered and configured");
    let ours: SocketAddr = format!("127.0.0.1:{}", m.port).parse().unwrap();

    let (audio, packets) = exchange_rtp(&rtp, ours, 1_500).await;
    assert!(packets.len() >= 60, "{} packets in 1.5 s", packets.len());
    for w in packets.windows(2) {
        assert_eq!(w[1].sequence, w[0].sequence.wrapping_add(1));
        assert_eq!(w[1].timestamp, w[0].timestamp.wrapping_add(160));
        assert_eq!(w[1].payload_type, 0);
    }
    // silence, then CED (2100 Hz) from 200 ms on
    let first_sound = audio.iter().position(|&s| s.unsigned_abs() > 64).unwrap();
    assert!(
        (1_400..=2_000).contains(&first_sound),
        "CED starts at sample {first_sound}"
    );
    let hz = tone_hz(&audio[first_sound + 80..first_sound + 4_080]);
    assert!((hz - 2100.0).abs() < 15.0, "CED at {hz} Hz");

    assert_eq!(dlg.bye().await.unwrap().code(), 200);
    let r = next_result(&mut ev).await;
    let a = r.audio.as_ref().expect("result.audio");
    assert_eq!(a.codec, "PCMU");
    assert_eq!(a.local_port, m.port);
    assert_eq!(a.reason.as_deref(), Some("t38_policy = audio-only"));
    assert!(r.t38.is_none());
    let f = r.fax.as_ref().expect("fax report");
    assert_eq!(f.media, FaxMedia::G711);
    let c = f.rtp.as_ref().expect("RTP counters");
    assert!(c.rx_packets >= 50 && c.tx_packets >= 60, "{c:?}");
    // capture: audio.log with the session's lines, result.json with the audio
    let call_dir = dir
        .join("calls")
        .join(fax_in::capture::dir_name(&r.call_id));
    gw.shutdown().await;
    fx.shutdown().await;
    let log = std::fs::read_to_string(call_dir.join("audio.log")).expect("audio.log");
    assert!(log.contains("answering the fax"), "{log}");
    assert!(log.contains("first RTP packet: PCMA"), "{log}");
    let json = std::fs::read_to_string(call_dir.join("result.json")).unwrap();
    assert!(json.contains("\"media\": \"g711\""), "{json}");
}

/// `auto`: the peer refuses our T.38 re-INVITE with 488: the fax goes over G.711 instead of
/// the call ending (ADR-061), and the result says why.
#[tokio::test]
async fn e02_a_refused_t38_reinvite_falls_back_to_g711() {
    let dir = scratch("fallback");
    let (fx, mut ev) = FaxIn::start(
        config(&dir, "31100-31199", "initiate_after_secs = 0.2\n"),
        StartOptions::default(),
    )
    .await
    .unwrap();
    let rtp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let (gw, mut dlg) = gateway(&fx, rtp.local_addr().unwrap()).await;
    let port = dlg.remote_sdp().unwrap().media[0].port;
    let ours: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    // nothing but silence while we wait for the re-INVITE
    let listen = exchange_rtp(&rtp, ours, 150);
    let (audio, _) = listen.await;
    assert!(
        audio.iter().all(|&s| s.unsigned_abs() <= 64),
        "the session must only listen before the fallback"
    );
    match tokio::time::timeout(Duration::from_secs(5), dlg.next_event())
        .await
        .unwrap()
        .unwrap()
    {
        DialogEvent::ReInvite { offer, responder } => {
            assert!(offer.unwrap().media[0].is_t38());
            responder
                .reject(StatusCode::NOT_ACCEPTABLE_HERE)
                .await
                .unwrap();
        }
        other => panic!("{other:?}"),
    }
    // now CED
    let (audio, _) = exchange_rtp(&rtp, ours, 800).await;
    let first_sound = audio.iter().position(|&s| s.unsigned_abs() > 64);
    assert!(first_sound.is_some(), "no CED after the 488");
    assert_eq!(dlg.bye().await.unwrap().code(), 200);
    let r = next_result(&mut ev).await;
    let reason = r.audio.as_ref().and_then(|a| a.reason.clone());
    assert_eq!(
        reason.as_deref(),
        Some("peer answered 488 to the T.38 re-INVITE"),
        "{r:?}"
    );
    assert_eq!(r.fax.as_ref().unwrap().media, FaxMedia::G711);
    gw.shutdown().await;
    fx.shutdown().await;
}

/// `auto`: the peer accepts T.38: the listening G.711 session gives way, the result is T.38.
#[tokio::test]
async fn e08_t38_accepted_after_listening_on_audio_is_a_t38_call() {
    let dir = scratch("t38-after-audio");
    let (fx, mut ev) = FaxIn::start(
        config(&dir, "31200-31299", "initiate_after_secs = 0.2\n"),
        StartOptions::default(),
    )
    .await
    .unwrap();
    let rtp = UdpSocket::bind("127.0.0.1:0").await.unwrap();
    let (gw, mut dlg) = gateway(&fx, rtp.local_addr().unwrap()).await;
    match tokio::time::timeout(Duration::from_secs(5), dlg.next_event())
        .await
        .unwrap()
        .unwrap()
    {
        DialogEvent::ReInvite { offer, responder } => {
            let offer = offer.unwrap();
            let policy = sip_core::sdp::negotiate::AnswerPolicy::new("127.0.0.1", 40000, 40002);
            let mut o = sip_core::sdp::negotiate::OriginTracker::random("127.0.0.1");
            let answer = sip_core::sdp::negotiate::answer_offer(&offer, &policy, &mut o).unwrap();
            responder.accept(answer).await.unwrap();
        }
        other => panic!("{other:?}"),
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(dlg.bye().await.unwrap().code(), 200);
    let r = next_result(&mut ev).await;
    assert!(r.t38.is_some(), "{r:?}");
    assert!(r.audio.is_none(), "{r:?}");
    if let Some(f) = &r.fax {
        assert_eq!(f.media, FaxMedia::T38);
    }
    gw.shutdown().await;
    fx.shutdown().await;
}
