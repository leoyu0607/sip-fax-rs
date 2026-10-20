//! M1c endpoint integration tests E01-E06 on real localhost sockets (plus the message tap).
//!
//! Two (or 51) `Endpoint`s talk to each other over UDP or TCP. Nothing is mocked below the
//! `Endpoint` API.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sip_core::endpoint::{
    DialogEvent, DialogHandle, Endpoint, EndpointConfig, IncomingCall, InviteOptions,
    ReinviteOutcome, TerminateReason,
};
use sip_core::message::{SipUri, StatusCode};
use sip_core::sdp::negotiate::{AnswerPolicy, OriginTracker, answer_offer};
use sip_core::sdp::{SessionDescription, parse_sdp};
use sip_core::transaction::TimerConfig;
use sip_core::transport::{Advertised, MessageTap, TapDirection, TapEvent, TransportKind};
use tokio::net::{TcpStream, UdpSocket};
use tokio::sync::mpsc;

const T38_OFFER: &str = include_str!("vectors/sdp/t38_minimal_offer.sdp");
const AUDIO_OFFER: &str = include_str!("vectors/sdp/sonus_audio_offer.sdp");

fn loopback() -> SocketAddr {
    "127.0.0.1:0".parse().unwrap()
}

fn base_cfg() -> EndpointConfig {
    EndpointConfig::new().udp(loopback()).tcp(loopback())
}

fn sdp(t: &str) -> SessionDescription {
    parse_sdp(t.as_bytes()).unwrap()
}

fn answer_of(offer: &SessionDescription) -> SessionDescription {
    let policy = AnswerPolicy::new("127.0.0.1", 20000, 30000);
    let mut o = OriginTracker::random("127.0.0.1");
    answer_offer(offer, &policy, &mut o).unwrap()
}

fn target(ep: &Endpoint, kind: TransportKind) -> SocketAddr {
    match kind {
        TransportKind::Udp => ep.udp_addr().unwrap(),
        TransportKind::Tcp => ep.tcp_addr().unwrap(),
    }
}

fn invite_opts(to: &Endpoint, kind: TransportKind, offer: &str) -> InviteOptions {
    let addr = target(to, kind);
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

async fn recv_event(d: &mut DialogHandle) -> DialogEvent {
    tokio::time::timeout(Duration::from_secs(10), d.next_event())
        .await
        .expect("event within 10 s")
        .expect("dialog still alive")
}

async fn next_call(rx: &mut mpsc::UnboundedReceiver<IncomingCall>) -> IncomingCall {
    tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .expect("call within 10 s")
        .expect("channel open")
}

fn first_line(b: &[u8]) -> String {
    String::from_utf8_lossy(b)
        .lines()
        .next()
        .unwrap_or("")
        .to_string()
}

/// Collects tap events of an endpoint.
fn collecting_tap() -> (MessageTap, Arc<Mutex<Vec<TapEvent>>>) {
    let v = Arc::new(Mutex::new(Vec::new()));
    let v2 = v.clone();
    (MessageTap::new(move |e| v2.lock().unwrap().push(e)), v)
}

fn summary(v: &Arc<Mutex<Vec<TapEvent>>>) -> Vec<String> {
    v.lock()
        .unwrap()
        .iter()
        .map(|e| {
            let d = match e.direction {
                TapDirection::Sent => "->",
                TapDirection::Received => "<-",
            };
            let l = first_line(&e.bytes);
            let l = l.strip_suffix(" SIP/2.0").map(str::to_string).unwrap_or(l);
            let l = l
                .split(' ')
                .take(if l.starts_with("SIP/2.0") { 2 } else { 1 })
                .collect::<Vec<_>>()
                .join(" ");
            format!("{d} {l}")
        })
        .collect()
}

// ================================================================ E01 / E02 / E03

/// The reference call flow. `uas_reinvites`: the UAS (callee) sends the T.38 re-INVITE,
/// otherwise the UAC does. `reject_488`: the receiver of the re-INVITE answers 488.
async fn flow(kind: TransportKind, uas_reinvites: bool, reject_488: bool) -> Vec<String> {
    let (tap, seen) = collecting_tap();
    let (uac, _uac_calls) = Endpoint::bind(base_cfg().tap(tap)).await.unwrap();
    let (uas, mut calls) = Endpoint::bind(base_cfg()).await.unwrap();

    let (re_done_tx, re_done_rx) = tokio::sync::oneshot::channel::<()>();
    let uas_task = tokio::spawn(async move {
        let call = next_call(&mut calls).await;
        call.ringing().await.unwrap();
        let answer = answer_of(&call.offer().unwrap());
        let mut dlg = call.accept(answer).await.unwrap();
        assert!(matches!(
            recv_event(&mut dlg).await,
            DialogEvent::Established
        ));
        let mut outcome = None;
        if uas_reinvites {
            outcome = Some(dlg.reinvite(sdp(T38_OFFER)).await.unwrap());
            let _ = re_done_tx.send(());
        } else {
            let DialogEvent::ReInvite { offer, responder } = recv_event(&mut dlg).await else {
                panic!("expected a re-INVITE from the caller");
            };
            let offer = offer.unwrap();
            assert!(offer.media[0].is_t38());
            if reject_488 {
                responder
                    .reject(StatusCode::NOT_ACCEPTABLE_HERE)
                    .await
                    .unwrap();
            } else {
                responder.accept(answer_of(&offer)).await.unwrap();
            }
        }
        if uas_reinvites && reject_488 {
            // the dialog must survive: we hang up ourselves
            assert_eq!(dlg.bye().await.unwrap(), StatusCode::OK);
            assert!(matches!(
                recv_event(&mut dlg).await,
                DialogEvent::Terminated(TerminateReason::LocalBye)
            ));
        } else {
            assert!(matches!(recv_event(&mut dlg).await, DialogEvent::Bye));
            assert!(matches!(
                recv_event(&mut dlg).await,
                DialogEvent::Terminated(TerminateReason::RemoteBye)
            ));
        }
        outcome
    });

    let mut dlg = uac
        .invite(invite_opts(&uas, kind, AUDIO_OFFER))
        .await
        .unwrap();
    assert!(
        dlg.remote_sdp().unwrap().media[0].media == "audio",
        "2xx carries the answer"
    );
    let uac_outcome = if uas_reinvites {
        let DialogEvent::ReInvite { offer, responder } = recv_event(&mut dlg).await else {
            panic!("expected the callee's re-INVITE");
        };
        let offer = offer.unwrap();
        assert!(offer.media[0].is_t38());
        if reject_488 {
            responder
                .reject(StatusCode::NOT_ACCEPTABLE_HERE)
                .await
                .unwrap();
            None
        } else {
            responder.accept(answer_of(&offer)).await.unwrap();
            None
        }
    } else {
        let out = dlg.reinvite(sdp(T38_OFFER)).await.unwrap();
        Some(out)
    };
    if uas_reinvites && reject_488 {
        assert!(matches!(recv_event(&mut dlg).await, DialogEvent::Bye));
        assert!(matches!(
            recv_event(&mut dlg).await,
            DialogEvent::Terminated(TerminateReason::RemoteBye)
        ));
    } else {
        if uas_reinvites {
            // do not hang up before the callee has seen our 200 (BYE could overtake it)
            re_done_rx.await.unwrap();
        }
        assert_eq!(dlg.bye().await.unwrap(), StatusCode::OK);
    }
    let uas_outcome = uas_task.await.unwrap();
    // check outcomes
    let reinvite_outcome = uac_outcome.or(uas_outcome).expect("one side reinvited");
    if reject_488 {
        assert_eq!(
            reinvite_outcome,
            ReinviteOutcome::Rejected(StatusCode::NOT_ACCEPTABLE_HERE)
        );
    } else {
        match reinvite_outcome {
            ReinviteOutcome::Accepted(a) => assert!(a.media[0].is_t38(), "T.38 answer"),
            other => panic!("expected Accepted, got {other:?}"),
        }
    }
    // let the last messages drain, then stop
    tokio::time::sleep(Duration::from_millis(50)).await;
    let s = summary(&seen);
    uac.shutdown().await;
    uas.shutdown().await;
    s
}

fn index_of(s: &[String], needle: &str, from: usize) -> usize {
    from + s[from..]
        .iter()
        .position(|l| l == needle)
        .unwrap_or_else(|| panic!("{needle} not found after {from} in {s:?}"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e01_full_call_flow_over_udp_and_tcp() {
    for kind in [TransportKind::Udp, TransportKind::Tcp] {
        let s = flow(kind, true, false).await;
        // tap on the UAC: INVITE out, 180/200 in, ACK out, re-INVITE in, 200 out, ACK in,
        // BYE out, 200 in
        let i = index_of(&s, "-> INVITE", 0);
        let i = index_of(&s, "<- SIP/2.0 180", i);
        let i = index_of(&s, "<- SIP/2.0 200", i);
        let i = index_of(&s, "-> ACK", i);
        let i = index_of(&s, "<- INVITE", i);
        let i = index_of(&s, "-> SIP/2.0 200", i);
        index_of(&s, "<- ACK", i); // the callee's ACK of our 200
        let i = index_of(&s, "-> BYE", i);
        index_of(&s, "<- SIP/2.0 200", i);
        println!("E01 {kind:?}: {s:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e02_reinvite_sent_by_the_caller_over_udp_and_tcp() {
    for kind in [TransportKind::Udp, TransportKind::Tcp] {
        let s = flow(kind, false, false).await;
        let i = index_of(&s, "-> INVITE", 0);
        let i = index_of(&s, "<- SIP/2.0 200", i);
        let i = index_of(&s, "-> ACK", i);
        let i = index_of(&s, "-> INVITE", i);
        let i = index_of(&s, "<- SIP/2.0 200", i);
        let i = index_of(&s, "-> ACK", i);
        index_of(&s, "-> BYE", i);
        println!("E02 {kind:?}: {s:?}");
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e03_t38_reinvite_rejected_with_488_keeps_the_dialog() {
    for kind in [TransportKind::Udp, TransportKind::Tcp] {
        let s = flow(kind, true, true).await;
        let i = index_of(&s, "<- INVITE", 0);
        let i = index_of(&s, "-> SIP/2.0 488", i);
        let i = index_of(&s, "<- ACK", i); // the 488 is ACKed by the transaction layer
        index_of(&s, "<- BYE", i);
        println!("E03 {kind:?}: {s:?}");
    }
}

// ================================================================ E04

fn port_of(d: &SessionDescription) -> u16 {
    d.media[0].port
}

fn with_port(mut d: SessionDescription, port: u16) -> SessionDescription {
    d.media[0].port = port;
    d
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e04_fifty_concurrent_calls_do_not_mix_up_and_leave_no_tasks_behind() {
    let handle = tokio::runtime::Handle::current();
    let base = handle.metrics().num_alive_tasks();
    let (uac, _uc) = Endpoint::bind(base_cfg()).await.unwrap();
    let (uas, mut calls) = Endpoint::bind(base_cfg()).await.unwrap();
    let uas_addr = uas.udp_addr().unwrap();
    const N: u16 = 50;
    // per call: the server tells the client when its re-INVITE finished, so the BYE cannot
    // overtake the client's own 200 on the wire
    let mut done_tx = std::collections::HashMap::new();
    let mut done_rx = std::collections::HashMap::new();
    for i in 0..N {
        let (t, r) = tokio::sync::oneshot::channel::<()>();
        done_tx.insert(10_000 + i, t);
        done_rx.insert(10_000 + i, r);
    }
    let done_tx = Arc::new(Mutex::new(done_tx));

    // UAS: per call, answer with port+10000, then re-INVITE with port+20000 and expect the
    // caller's answer to carry port+30000.
    let server = tokio::spawn(async move {
        let mut handlers = vec![];
        for _ in 0..N {
            let call = next_call(&mut calls).await;
            let done_tx = done_tx.clone();
            handlers.push(tokio::spawn(async move {
                let offered = port_of(&call.offer().unwrap());
                let answer = with_port(answer_of(&call.offer().unwrap()), offered + 10_000);
                let mut dlg = call.accept(answer).await.unwrap();
                assert!(matches!(
                    recv_event(&mut dlg).await,
                    DialogEvent::Established
                ));
                let out = dlg
                    .reinvite(with_port(sdp(T38_OFFER), offered + 20_000))
                    .await
                    .unwrap();
                match out {
                    ReinviteOutcome::Accepted(a) => {
                        assert_eq!(port_of(&a), offered + 30_000, "answer belongs to this call")
                    }
                    other => panic!("{other:?}"),
                }
                if let Some(t) = done_tx.lock().unwrap().remove(&offered) {
                    let _ = t.send(());
                }
                assert!(matches!(recv_event(&mut dlg).await, DialogEvent::Bye));
                assert!(matches!(
                    recv_event(&mut dlg).await,
                    DialogEvent::Terminated(TerminateReason::RemoteBye)
                ));
                offered
            }));
        }
        let mut seen = vec![];
        for h in handlers {
            seen.push(h.await.unwrap());
        }
        seen
    });

    let mut clients = vec![];
    for i in 0..N {
        let uac = uac.clone();
        let done = done_rx.remove(&(10_000 + i)).unwrap();
        clients.push(tokio::spawn(async move {
            let port = 10_000 + i;
            let mut o = InviteOptions::new(
                uas_addr,
                SipUri::parse(&format!("sip:fax{i}@{uas_addr}")).unwrap(),
                SipUri::parse(&format!("sip:gw{i}@127.0.0.1")).unwrap(),
                SipUri::parse(&format!("sip:fax{i}@{uas_addr}")).unwrap(),
                with_port(sdp(AUDIO_OFFER), port),
            );
            o.transport = TransportKind::Udp;
            let mut dlg = uac.invite(o).await.unwrap();
            assert_eq!(
                port_of(dlg.remote_sdp().unwrap()),
                port + 10_000,
                "call {i} got the answer of call {i}"
            );
            let DialogEvent::ReInvite { offer, responder } = recv_event(&mut dlg).await else {
                panic!("expected re-INVITE");
            };
            let offer = offer.unwrap();
            assert_eq!(port_of(&offer), port + 20_000, "re-INVITE of call {i}");
            responder
                .accept(with_port(answer_of(&offer), port + 30_000))
                .await
                .unwrap();
            done.await.unwrap();
            assert_eq!(dlg.bye().await.unwrap(), StatusCode::OK);
            port
        }));
    }
    let mut got = vec![];
    for c in clients {
        got.push(c.await.unwrap());
    }
    let mut served = server.await.unwrap();
    served.sort_unstable();
    got.sort_unstable();
    assert_eq!(got, (10_000..10_000 + N).collect::<Vec<_>>());
    assert_eq!(
        served, got,
        "the server saw exactly the calls the clients made"
    );
    assert_eq!(uac.dialog_count(), 0);
    assert_eq!(uas.dialog_count(), 0);
    let mid = handle.metrics().num_alive_tasks();
    assert!(mid > base, "endpoints are running tasks ({mid} vs {base})");
    uac.shutdown().await;
    uas.shutdown().await;
    // tasks finish asynchronously with respect to the join handles: allow the runtime to
    // report them
    for _ in 0..100 {
        if handle.metrics().num_alive_tasks() == base {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(
        handle.metrics().num_alive_tasks(),
        base,
        "no task leaked by shutdown()"
    );
    println!(
        "E04: 50 calls ok; alive tasks {base} -> {mid} -> {}",
        handle.metrics().num_alive_tasks()
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e04_shutdown_with_live_dialogs_ends_everything() {
    let handle = tokio::runtime::Handle::current();
    let base = handle.metrics().num_alive_tasks();
    let (uac, _c) = Endpoint::bind(base_cfg()).await.unwrap();
    let (uas, mut calls) = Endpoint::bind(base_cfg()).await.unwrap();
    let srv = tokio::spawn(async move {
        let call = next_call(&mut calls).await;
        let a = answer_of(&call.offer().unwrap());
        call.accept(a).await.unwrap()
    });
    let dlg = uac
        .invite(invite_opts(&uas, TransportKind::Tcp, AUDIO_OFFER))
        .await
        .unwrap();
    let mut s = srv.await.unwrap();
    assert_eq!(uac.dialog_count(), 1);
    uac.shutdown().await;
    uas.shutdown().await;
    assert_eq!(uac.dialog_count(), 0);
    assert!(matches!(
        recv_event(&mut s).await,
        DialogEvent::Established | DialogEvent::Terminated(TerminateReason::Shutdown)
    ));
    drop(dlg);
    for _ in 0..100 {
        if handle.metrics().num_alive_tasks() == base {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(handle.metrics().num_alive_tasks(), base);
}

// ================================================================ E05

fn splitmix(state: &mut u64) -> u64 {
    *state = state.wrapping_add(0x9E37_79B9_7F4A_7C15);
    let mut z = *state;
    z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
    z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
    z ^ (z >> 31)
}

/// A two-socket UDP relay: what A sends to `b_face` is forwarded to B (appearing to come
/// from `a_face`) and vice versa; every packet is dropped with probability 10 % from a
/// fixed-seed generator.
struct Relay {
    a_face: SocketAddr,
    b_face: SocketAddr,
    dropped: Arc<std::sync::atomic::AtomicUsize>,
    forwarded: Arc<std::sync::atomic::AtomicUsize>,
    task: tokio::task::JoinHandle<()>,
}

async fn relay(a_real: SocketAddr, b_real: SocketAddr, seed: u64) -> Relay {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let a_sock = Arc::new(UdpSocket::bind(loopback()).await.unwrap());
    let b_sock = Arc::new(UdpSocket::bind(loopback()).await.unwrap());
    let (a_face, b_face) = (a_sock.local_addr().unwrap(), b_sock.local_addr().unwrap());
    let dropped = Arc::new(AtomicUsize::new(0));
    let forwarded = Arc::new(AtomicUsize::new(0));
    let (d2, f2) = (dropped.clone(), forwarded.clone());
    let task = tokio::spawn(async move {
        let mut state = seed;
        let mut ba = vec![0u8; 65536];
        let mut bb = vec![0u8; 65536];
        loop {
            // from A (arrives at b_face) -> B via a_face; from B (arrives at a_face) -> A via b_face
            let (data, out, dest): (Vec<u8>, Arc<UdpSocket>, SocketAddr) = tokio::select! {
                r = b_sock.recv_from(&mut bb) => {
                    let (n, _) = r.unwrap();
                    (bb[..n].to_vec(), a_sock.clone(), b_real)
                }
                r = a_sock.recv_from(&mut ba) => {
                    let (n, _) = r.unwrap();
                    (ba[..n].to_vec(), b_sock.clone(), a_real)
                }
            };
            if splitmix(&mut state) % 10 == 0 {
                d2.fetch_add(1, Ordering::SeqCst);
                continue;
            }
            f2.fetch_add(1, Ordering::SeqCst);
            let _ = out.send_to(&data, dest).await;
        }
    });
    Relay {
        a_face,
        b_face,
        dropped,
        forwarded,
        task,
    }
}

fn fast_timers() -> TimerConfig {
    TimerConfig {
        t1: Duration::from_millis(40),
        t2: Duration::from_millis(320),
        t4: Duration::from_millis(400),
        provisional_delay: Duration::from_millis(100),
        timer_d: Duration::from_millis(2000),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn e05_call_flow_survives_ten_percent_packet_loss() {
    use std::sync::atomic::Ordering;
    // reserve real ports first, then tell each endpoint to advertise the relay's faces
    let probe_a = UdpSocket::bind(loopback()).await.unwrap();
    let probe_b = UdpSocket::bind(loopback()).await.unwrap();
    let (a_real, b_real) = (probe_a.local_addr().unwrap(), probe_b.local_addr().unwrap());
    drop((probe_a, probe_b));
    let rl = relay(a_real, b_real, 0x5EED_0001).await;
    let mk = |real: SocketAddr, face: SocketAddr| {
        let mut c = EndpointConfig::new().udp(real);
        c.timers = fast_timers();
        c.transport.advertised = Some(Advertised {
            host: "127.0.0.1".into(),
            port: Some(face.port()),
        });
        c
    };
    let (uac, _c) = Endpoint::bind(mk(a_real, rl.a_face)).await.unwrap();
    let (uas, mut calls) = Endpoint::bind(mk(b_real, rl.b_face)).await.unwrap();

    let mut ok_calls = 0;
    for round in 0..20 {
        let srv = {
            // one call at a time keeps the run short; the loss pattern differs per round
            let call_rx = &mut calls;
            let (done_tx, done_rx) = tokio::sync::oneshot::channel::<()>();
            let accept = async {
                let call = next_call(call_rx).await;
                let a = answer_of(&call.offer().unwrap());
                let mut dlg = call.accept(a).await.unwrap();
                // wait for the ACK (it may have been lost; the 200 is retransmitted)
                loop {
                    match recv_event(&mut dlg).await {
                        DialogEvent::Established => break,
                        DialogEvent::AckTimeout => panic!("round {round}: ACK timeout"),
                        _ => {}
                    }
                }
                let out = dlg.reinvite(sdp(T38_OFFER)).await.unwrap();
                assert!(
                    matches!(out, ReinviteOutcome::Accepted(_)),
                    "round {round}: {out:?}"
                );
                let _ = done_tx.send(());
                loop {
                    if let DialogEvent::Terminated(_) = recv_event(&mut dlg).await {
                        break;
                    }
                }
            };
            let uac_side = async {
                let mut o = InviteOptions::new(
                    rl.b_face,
                    SipUri::parse(&format!("sip:fax@{}", rl.b_face)).unwrap(),
                    SipUri::parse("sip:gw@127.0.0.1").unwrap(),
                    SipUri::parse(&format!("sip:fax@{}", rl.b_face)).unwrap(),
                    sdp(AUDIO_OFFER),
                );
                o.transport = TransportKind::Udp;
                let mut dlg = uac.invite(o).await.unwrap();
                let DialogEvent::ReInvite { offer, responder } = recv_event(&mut dlg).await else {
                    panic!("round {round}: expected re-INVITE")
                };
                responder.accept(answer_of(&offer.unwrap())).await.unwrap();
                // hang up only after the callee has our 200 (BYE must not overtake it)
                done_rx.await.unwrap();
                assert_eq!(dlg.bye().await.unwrap(), StatusCode::OK, "round {round}");
            };
            tokio::join!(accept, uac_side)
        };
        let _ = srv;
        ok_calls += 1;
    }
    let (d, f) = (
        rl.dropped.load(Ordering::SeqCst),
        rl.forwarded.load(Ordering::SeqCst),
    );
    println!(
        "E05: {ok_calls} calls completed; relay forwarded {f}, dropped {d} ({:.1} %)",
        100.0 * d as f64 / (d + f) as f64
    );
    assert!(d >= 5, "the relay really dropped packets (seeded)");
    assert_eq!(ok_calls, 20);
    rl.task.abort();
    uac.shutdown().await;
    uas.shutdown().await;
}

// ================================================================ E06

fn garbled(version: &str, extra: &str) -> String {
    format!(
        "INVITE sip:fax@127.0.0.1 SIP/{version}\r\n\
         Via: SIP/2.0/UDP 127.0.0.1:9;branch=z9hG4bKbad1;rport\r\n\
         Max-Forwards: 70\r\n\
         From: <sip:gw@127.0.0.1>;tag=g1\r\n\
         To: <sip:fax@127.0.0.1>\r\n\
         {extra}\
         Call-ID: bad-call@127.0.0.1\r\n\
         CSeq: 7 INVITE\r\n\
         Content-Length: 0\r\n\r\n"
    )
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e06_unparseable_request_gets_400_when_ids_are_recoverable_else_dropped() {
    let (ep, _calls) = Endpoint::bind(base_cfg()).await.unwrap();
    let addr = ep.udp_addr().unwrap();
    let cli = UdpSocket::bind(loopback()).await.unwrap();
    let mut buf = vec![0u8; 4096];

    // (a) bad SIP version: Via, From, To, Call-ID and CSeq are all there
    cli.send_to(garbled("9.9", "").as_bytes(), addr)
        .await
        .unwrap();
    let (n, _) = tokio::time::timeout(Duration::from_secs(5), cli.recv_from(&mut buf))
        .await
        .expect("a reply")
        .unwrap();
    let text = String::from_utf8_lossy(&buf[..n]).to_string();
    assert!(text.starts_with("SIP/2.0 400 "), "{text}");
    assert!(text.contains("branch=z9hG4bKbad1"), "Via echoed: {text}");
    assert!(text.contains("received=127.0.0.1"), "Via stamped: {text}");
    assert!(text.contains("Call-ID: bad-call@127.0.0.1"));
    assert!(text.contains("CSeq: 7 INVITE"));
    assert!(text.contains("From: <sip:gw@127.0.0.1>;tag=g1"));
    assert!(text.contains(";tag="), "To tag added");
    println!("E06 (a): {}", text.lines().next().unwrap());

    // (b) Call-ID missing: cannot be addressed -> silently dropped. A valid OPTIONS sent
    // right after proves nothing came back for the bad one (UDP on loopback keeps order).
    let bad =
        garbled("2.0", "").replace("Call-ID: bad-call@127.0.0.1\r\n", "Content-Length: x\r\n");
    cli.send_to(bad.as_bytes(), addr).await.unwrap();
    let opt = "OPTIONS sip:fax@127.0.0.1 SIP/2.0\r\nVia: SIP/2.0/UDP 127.0.0.1:9;branch=z9hG4bKok1;rport\r\n\
         Max-Forwards: 70\r\nFrom: <sip:gw@127.0.0.1>;tag=g2\r\nTo: <sip:fax@127.0.0.1>\r\n\
         Call-ID: ok-call@127.0.0.1\r\nCSeq: 1 OPTIONS\r\nContent-Length: 0\r\n\r\n"
        .to_string();
    cli.send_to(opt.as_bytes(), addr).await.unwrap();
    let (n, _) = tokio::time::timeout(Duration::from_secs(5), cli.recv_from(&mut buf))
        .await
        .expect("a reply")
        .unwrap();
    let text = String::from_utf8_lossy(&buf[..n]).to_string();
    assert!(
        text.starts_with("SIP/2.0 200 "),
        "first reply is for the OPTIONS: {text}"
    );
    // (c) garbage and a response are never answered
    cli.send_to(b"hello world", addr).await.unwrap();
    cli.send_to(
        b"SIP/2.0 200 OK\r\nVia: SIP/2.0/UDP 127.0.0.1:9;branch=z9hG4bKx\r\nFrom: <sip:a@b>;tag=1\r\nTo: <sip:c@d>\r\nCall-ID: r\r\nCSeq: 1 BYE\r\nContent-Length: 5\r\n\r\n",
        addr,
    )
    .await
    .unwrap();
    cli.send_to(opt.replace("ok1", "ok2").as_bytes(), addr)
        .await
        .unwrap();
    let (n, _) = tokio::time::timeout(Duration::from_secs(5), cli.recv_from(&mut buf))
        .await
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&buf[..n]).to_string();
    assert!(
        text.starts_with("SIP/2.0 200 ") && text.contains("z9hG4bKok2"),
        "{text}"
    );

    // (d) the same over TCP: 400, and the connection stays usable
    let mut tcp = TcpStream::connect(ep.tcp_addr().unwrap()).await.unwrap();
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    let g = garbled("9.9", "").replace("UDP", "TCP");
    tcp.write_all(g.as_bytes()).await.unwrap();
    let mut got = vec![0u8; 4096];
    let n = tokio::time::timeout(Duration::from_secs(5), tcp.read(&mut got))
        .await
        .unwrap()
        .unwrap();
    let t = String::from_utf8_lossy(&got[..n]).to_string();
    assert!(t.starts_with("SIP/2.0 400 "), "{t}");
    tcp.write_all(opt.replace("UDP", "TCP").replace("ok1", "ok3").as_bytes())
        .await
        .unwrap();
    let n = tokio::time::timeout(Duration::from_secs(5), tcp.read(&mut got))
        .await
        .unwrap()
        .unwrap();
    assert!(String::from_utf8_lossy(&got[..n]).starts_with("SIP/2.0 200 "));
    ep.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e06_the_400_answer_can_be_switched_off() {
    let mut cfg = base_cfg();
    cfg.answer_malformed = false;
    let (ep, _calls) = Endpoint::bind(cfg).await.unwrap();
    let cli = UdpSocket::bind(loopback()).await.unwrap();
    cli.send_to(garbled("9.9", "").as_bytes(), ep.udp_addr().unwrap())
        .await
        .unwrap();
    let mut buf = vec![0u8; 1024];
    assert!(
        tokio::time::timeout(Duration::from_millis(300), cli.recv_from(&mut buf))
            .await
            .is_err(),
        "no reply with answer_malformed = false"
    );
    ep.shutdown().await;
}

// ================================================================ message tap

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e01_tap_sees_every_message_with_raw_bytes_direction_peer_and_timestamp() {
    let (tap, seen) = collecting_tap();
    let (uac, _c) = Endpoint::bind(base_cfg().tap(tap)).await.unwrap();
    let (uas, mut calls) = Endpoint::bind(base_cfg()).await.unwrap();
    let before = std::time::SystemTime::now();
    let srv = tokio::spawn(async move {
        let call = next_call(&mut calls).await;
        let a = answer_of(&call.offer().unwrap());
        let _ = call.accept(a).await.unwrap();
    });
    let dlg = uac
        .invite(invite_opts(&uas, TransportKind::Udp, AUDIO_OFFER))
        .await
        .unwrap();
    srv.await.unwrap();
    assert_eq!(dlg.bye().await.unwrap(), StatusCode::OK);
    tokio::time::sleep(Duration::from_millis(50)).await;
    let ev = seen.lock().unwrap().clone();
    let first = &ev[0];
    assert_eq!(first.direction, TapDirection::Sent);
    assert_eq!(first.peer, uas.udp_addr().unwrap());
    assert_eq!(first.transport, TransportKind::Udp);
    assert!(
        first.bytes.starts_with(b"INVITE sip:fax@"),
        "raw request bytes"
    );
    assert!(
        first.bytes.ends_with(AUDIO_OFFER.as_bytes())
            || first.bytes.windows(4).any(|w| w == b"\r\n\r\n")
    );
    assert!(first.at >= before);
    let rx_200 = ev
        .iter()
        .find(|e| e.direction == TapDirection::Received && e.bytes.starts_with(b"SIP/2.0 200"))
        .expect("received 200");
    assert_eq!(rx_200.peer, uas.udp_addr().unwrap());
    // the bytes are exactly what the peer parsed: re-parse them
    assert!(sip_core::message::parse_datagram(&rx_200.bytes).is_ok());
    assert!(
        ev.windows(2).all(|w| w[0].at <= w[1].at),
        "timestamps are monotonic"
    );
    uac.shutdown().await;
    uas.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e01_a_stalled_tap_never_blocks_the_stack() {
    // capacity 1 and nobody reads: the call must still work, and the drops are counted
    let (tap, _rx) = MessageTap::channel(1);
    let tap2 = tap.clone();
    let (uac, _c) = Endpoint::bind(base_cfg().tap(tap)).await.unwrap();
    let (uas, mut calls) = Endpoint::bind(base_cfg()).await.unwrap();
    let srv = tokio::spawn(async move {
        let call = next_call(&mut calls).await;
        let a = answer_of(&call.offer().unwrap());
        let mut dlg = call.accept(a).await.unwrap();
        loop {
            if matches!(recv_event(&mut dlg).await, DialogEvent::Terminated(_)) {
                break;
            }
        }
    });
    let dlg = uac
        .invite(invite_opts(&uas, TransportKind::Udp, AUDIO_OFFER))
        .await
        .unwrap();
    let out = dlg.reinvite(sdp(T38_OFFER)).await;
    assert!(out.is_ok());
    assert_eq!(dlg.bye().await.unwrap(), StatusCode::OK);
    srv.await.unwrap();
    assert!(
        tap2.dropped() > 0,
        "events beyond the capacity were dropped, not awaited"
    );
    uac.shutdown().await;
    uas.shutdown().await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn e01_no_tap_by_default() {
    let cfg = EndpointConfig::new();
    assert!(cfg.transport.tap.is_none());
}
