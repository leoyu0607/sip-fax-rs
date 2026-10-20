//! M1b transaction layer tests T01-T13.
//!
//! Two styles are used:
//! * `*_sim` tests drive the pure state machines with a virtual clock (no tokio at all);
//! * the others run the real `TransactionLayer` on a detached transport under
//!   `tokio::time::pause()` (`start_paused = true`) and assert the exact millisecond of every
//!   message / event. There is no real sleeping anywhere in this file.

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use common::*;
use sip_core::message::{Method, Request, Response, SipMessage, StatusCode};
use sip_core::transaction::*;
use sip_core::transport::{Incoming, TransportHandle, TransportKind};
use tokio::sync::mpsc;
use tokio::time::Instant;

use TransportKind::{Tcp, Udp};

// ---------------------------------------------------------------- harness

struct H {
    layer: TransactionLayer,
    tu: mpsc::UnboundedReceiver<TuInput>,
    inject: mpsc::Sender<Incoming>,
    rec: Recorder,
    t0: Instant,
}

impl H {
    fn new() -> H {
        H::with(TimerConfig::default())
    }

    fn with(cfg: TimerConfig) -> H {
        let d = TransportHandle::detached(LOCAL.parse().unwrap());
        let (layer, tu) = TransactionLayer::new(d.handle.clone(), d.incoming, cfg);
        let t0 = Instant::now();
        let rec = Recorder::spawn(t0, d.outgoing);
        H {
            layer,
            tu,
            inject: d.inject,
            rec,
            t0,
        }
    }

    /// Sleeps (virtual time) until `ms` after the start and lets every woken task run.
    async fn at(&self, ms: u64) {
        tokio::time::sleep_until(self.t0 + Duration::from_millis(ms)).await;
        settle().await;
    }

    async fn feed(&self, i: Incoming) {
        self.inject.send(i).await.unwrap();
        settle().await;
    }

    async fn feed_req(&self, r: Request, kind: TransportKind) {
        self.feed(inc_req(r, kind)).await;
    }

    async fn next_request(&mut self) -> IncomingRequest {
        match self.tu.recv().await {
            Some(TuInput::Request(r)) => r,
            Some(TuInput::Ack(_)) => panic!("got Ack"),
            Some(TuInput::StrayResponse(_)) => panic!("got StrayResponse"),
            Some(TuInput::Stateless(_)) => panic!("got Stateless"),
            None => panic!("TU channel closed"),
        }
    }

    fn counts(&self) -> (usize, usize) {
        self.layer.transaction_counts()
    }

    fn dest() -> SocketAddr {
        PEER.parse().unwrap()
    }
}

fn client_request(method: &str) -> Request {
    let mut r = mk_request(method, "unused", "unused", 1, method, None);
    r.headers.remove("Via");
    r
}

fn respond(req: &Request, code: u16, tag: Option<&str>) -> Response {
    Response::for_request(req, StatusCode::new(code).unwrap(), tag)
}

fn drain_events(rx: &mut mpsc::UnboundedReceiver<TuEvent>) -> Vec<TuEvent> {
    let mut v = vec![];
    while let Ok(e) = rx.try_recv() {
        v.push(e);
    }
    v
}

fn ms(v: &[u64]) -> Vec<u64> {
    v.to_vec()
}

// ---------------------------------------------------------------- pure simulator

struct Sim<M: Machine> {
    m: M,
    now: u64,
    timers: Vec<(Timer, u64)>,
    sent: Vec<(u64, SipMessage)>,
    events: Vec<(u64, TuEvent)>,
    started: Vec<(u64, Timer, u64)>, // (when, timer, duration)
    ended: Option<u64>,
}

impl<M: Machine> Sim<M> {
    fn new(m: M, init: Vec<Action>) -> Self {
        let mut s = Sim {
            m,
            now: 0,
            timers: vec![],
            sent: vec![],
            events: vec![],
            started: vec![],
            ended: None,
        };
        s.apply(init);
        s
    }

    fn apply(&mut self, actions: Vec<Action>) {
        for a in actions {
            match a {
                Action::Send(m) => self.sent.push((self.now, m)),
                Action::Start(t, d) => {
                    self.timers.retain(|(x, _)| *x != t);
                    self.started.push((self.now, t, d.as_millis() as u64));
                    self.timers.push((t, self.now + d.as_millis() as u64));
                }
                Action::Cancel(t) => self.timers.retain(|(x, _)| *x != t),
                Action::Notify(e) => self.events.push((self.now, e)),
                Action::Terminate => {
                    if self.ended.is_none() {
                        self.ended = Some(self.now);
                    }
                }
            }
        }
    }

    /// Moves the clock to `t`, firing timers in deadline order.
    fn run_to(&mut self, t: u64) {
        loop {
            let next = self
                .timers
                .iter()
                .filter(|(_, d)| *d <= t)
                .min_by_key(|(_, d)| *d)
                .copied();
            let Some((timer, d)) = next else { break };
            self.now = d;
            self.timers.retain(|(x, _)| *x != timer);
            let a = self.m.on_timer(timer);
            self.apply(a);
        }
        self.now = t;
    }

    fn feed(&mut self, msg: SipMessage) {
        let a = self.m.on_message(msg);
        self.apply(a);
    }

    fn respond(&mut self, r: Response) {
        let a = self.m.on_tu_response(r);
        self.apply(a);
    }

    fn sent_times(&self) -> Vec<u64> {
        self.sent.iter().map(|(t, _)| *t).collect()
    }
}

fn cum(intervals: &[u64]) -> Vec<u64> {
    let mut t = 0;
    intervals
        .iter()
        .map(|i| {
            t += i;
            t
        })
        .collect()
}

// ================================================================ T01

#[tokio::test(start_paused = true)]
async fn t01_invite_server_sends_100_trying_after_200ms() {
    let mut h = H::new();
    h.feed_req(invite("z9hG4bKt01"), Udp).await;
    let ir = h.next_request().await;
    assert_eq!(ir.request.method, Method::Invite);
    h.at(199).await;
    assert_eq!(h.rec.count(), 0, "nothing may be sent before 200 ms");
    h.at(200).await;
    let sent = h.rec.all();
    assert_eq!(sent.len(), 1);
    assert_eq!(sent[0].0, 200, "100 Trying at exactly 200 ms");
    let w = wire(&sent[0].1);
    println!("T01 wire at {} ms:\n{}", sent[0].0, w);
    assert!(w.starts_with("SIP/2.0 100 Trying\r\n"));
    assert!(w.contains("Via: SIP/2.0/UDP 10.0.0.1:5060;branch=z9hG4bKt01\r\n"));
    assert!(
        w.contains("To: <sip:fax@192.0.2.10>\r\n"),
        "100 carries no To tag: {w}"
    );
    assert!(w.contains("CSeq: 1 INVITE\r\n"));
    assert!(w.contains("Call-ID: call-z9hG4bKt01@10.0.0.1\r\n"));
    h.at(60_000).await;
    assert_eq!(h.rec.count(), 1, "100 is sent once");
}

#[tokio::test(start_paused = true)]
async fn t01_tu_provisional_before_200ms_suppresses_the_automatic_100() {
    let mut h = H::new();
    h.feed_req(invite("z9hG4bKt01b"), Udp).await;
    let ir = h.next_request().await;
    h.at(150).await;
    ir.tx
        .respond(respond(&ir.request, 180, Some("u1")))
        .await
        .unwrap();
    h.at(1_000).await;
    let sent = h.rec.all();
    assert_eq!(
        sent.len(),
        1,
        "only the TU's 180, no automatic 100: {sent:?}"
    );
    assert_eq!(sent[0].0, 150);
    assert!(first_line(&sent[0].1).starts_with("SIP/2.0 180"));
}

#[tokio::test(start_paused = true)]
async fn t01_provisional_delay_is_configurable() {
    let cfg = TimerConfig {
        provisional_delay: Duration::from_millis(50),
        ..Default::default()
    };
    let mut h = H::with(cfg);
    h.feed_req(invite("z9hG4bKt01c"), Udp).await;
    let _ir = h.next_request().await;
    h.at(1000).await;
    assert_eq!(h.rec.instants("SIP/2.0 100"), ms(&[50]));
}

// ================================================================ T02

#[tokio::test(start_paused = true)]
async fn t02_retransmitted_invite_gets_last_provisional_and_is_absorbed_when_accepted() {
    let mut h = H::new();
    let inv = invite("z9hG4bKt02");
    h.feed_req(inv.clone(), Udp).await;
    let mut ir = h.next_request().await;
    h.at(300).await; // the automatic 100 went out at 200
    h.feed_req(inv.clone(), Udp).await;
    assert_eq!(
        h.rec.instants("SIP/2.0 100"),
        ms(&[200, 300]),
        "100 resent for the retransmission"
    );

    ir.tx
        .respond(respond(&ir.request, 180, Some("u2")))
        .await
        .unwrap();
    h.at(400).await;
    h.feed_req(inv.clone(), Udp).await;
    assert_eq!(
        h.rec.instants("SIP/2.0 180"),
        ms(&[300, 400]),
        "180 sent at 300 (TU), resent at 400"
    );
    let w180 = h
        .rec
        .all()
        .iter()
        .filter(|(_, o)| first_line(o).starts_with("SIP/2.0 180"))
        .map(|(_, o)| wire(o))
        .collect::<Vec<_>>();
    assert_eq!(w180[0], w180[1], "identical bytes");
    assert_eq!(
        h.rec.instants("SIP/2.0 100"),
        ms(&[200, 300]),
        "the 100 is not resent after the 180"
    );

    // 2xx -> Accepted: retransmitted INVITEs are absorbed (the TU owns 2xx retransmission)
    ir.tx
        .respond(respond(&ir.request, 200, Some("u2")))
        .await
        .unwrap();
    h.at(500).await;
    let before = h.rec.count();
    h.feed_req(inv.clone(), Udp).await;
    h.at(600).await;
    assert_eq!(
        h.rec.count(),
        before,
        "no response to a retransmitted INVITE in Accepted"
    );
    assert!(h.tu.try_recv().is_err(), "and the TU never sees it");
    assert!(drain_events(&mut ir.events).is_empty());
    println!(
        "T02 instants: 100@{:?} 180@{:?} 200@{:?}",
        h.rec.instants("SIP/2.0 100"),
        h.rec.instants("SIP/2.0 180"),
        h.rec.instants("SIP/2.0 200")
    );
}

// ================================================================ T03

#[tokio::test(start_paused = true)]
async fn t03_invite_server_timer_g_schedule_ack_stops_it_timer_i_terminates() {
    let mut h = H::new();
    h.feed_req(invite("z9hG4bKt03"), Udp).await;
    let ir = h.next_request().await;
    ir.tx
        .respond(respond(&ir.request, 486, Some("u3")))
        .await
        .unwrap();
    settle().await;
    // Observe up to just after the 5th retransmission, then ACK.
    h.at(11_700).await;
    let t = h.rec.instants("SIP/2.0 486");
    // intervals 500, 1000, 2000, 4000 (cumulative 500, 1500, 3500, 7500, 11500)
    assert_eq!(t, ms(&[0, 500, 1500, 3500, 7500, 11500]));
    let ack = mk_request("ACK", "z9hG4bKt03", PEER, 1, "ACK", Some("u3"));
    h.feed_req(ack, Udp).await;
    h.at(14_000).await;
    assert_eq!(
        h.rec.instants("SIP/2.0 486"),
        t,
        "no retransmission after the ACK"
    );
    assert_eq!(
        h.counts(),
        (1, 0),
        "Confirmed: waiting for Timer I (T4 = 5 s)"
    );
    h.at(16_699).await;
    assert_eq!(h.counts(), (1, 0));
    h.at(16_700).await;
    assert_eq!(h.counts(), (0, 0), "Timer I fired 5000 ms after the ACK");
    println!("T03 486 instants (ms): {t:?}; ACK at 11700; terminated at 16700");
    let mut ir = ir;
    assert!(
        drain_events(&mut ir.events).is_empty(),
        "no TU event for a normal ACK"
    );
}

#[tokio::test(start_paused = true)]
async fn t03_timer_g_intervals_are_capped_at_t2_and_scale_with_t1() {
    // default T1/T2: intervals 500 1000 2000 4000 4000 4000 ...
    let mut h = H::new();
    h.feed_req(invite("z9hG4bKt03b"), Udp).await;
    let ir = h.next_request().await;
    ir.tx
        .respond(respond(&ir.request, 486, Some("x")))
        .await
        .unwrap();
    h.at(20_000).await;
    let t = h.rec.instants("SIP/2.0 486");
    let intervals: Vec<u64> = t.windows(2).map(|w| w[1] - w[0]).collect();
    assert_eq!(&intervals[..7], &[500, 1000, 2000, 4000, 4000, 4000, 4000]);
    // T1 = 100 ms, T2 = 700 ms: 100 200 400 700 700 ...
    let mut h = H::with(TimerConfig {
        t1: Duration::from_millis(100),
        t2: Duration::from_millis(700),
        ..Default::default()
    });
    h.feed_req(invite("z9hG4bKt03c"), Udp).await;
    let ir = h.next_request().await;
    ir.tx
        .respond(respond(&ir.request, 486, Some("x")))
        .await
        .unwrap();
    h.at(3_000).await;
    let t = h.rec.instants("SIP/2.0 486");
    let intervals: Vec<u64> = t.windows(2).map(|w| w[1] - w[0]).collect();
    assert_eq!(&intervals[..6], &[100, 200, 400, 700, 700, 700]);
    // Timer H = 64 * T1 = 6400 ms
    h.at(6_399).await;
    assert_eq!(h.counts(), (1, 0));
    h.at(6_400).await;
    assert_eq!(h.counts(), (0, 0));
}

#[test]
fn t03_invite_server_sim_schedule() {
    let inv = invite("z9hG4bKsim03");
    let (m, init) = ServerInvite::new(inv.clone(), false, TimerConfig::default());
    let mut s = Sim::new(m, init);
    assert_eq!(s.started, vec![(0, Timer::Trying, 200)]);
    s.respond(respond(&inv, 486, Some("t")));
    assert_eq!(s.m.state(), State::Completed);
    s.run_to(40_000);
    let retrans = cum(&[500, 1000, 2000, 4000, 4000, 4000, 4000, 4000, 4000, 4000]);
    assert_eq!(
        &s.sent_times()[1..],
        &retrans[..],
        "G retransmissions (cumulative)"
    );
    // Timer H armed with 64*T1 at the moment the final response was sent
    assert!(s.started.contains(&(0, Timer::H, 32_000)));
    // H expiry: TU notified at 32000, then terminated
    assert_eq!(s.events, vec![(32_000, TuEvent::Timeout)]);
    assert_eq!(s.ended, Some(32_000));
    assert_eq!(s.m.state(), State::Terminated);
}

// ================================================================ T04

#[tokio::test(start_paused = true)]
async fn t04_invite_server_timer_h_fires_at_32s_with_correct_retransmission_count() {
    let mut h = H::new();
    h.feed_req(invite("z9hG4bKt04"), Udp).await;
    let mut ir = h.next_request().await;
    ir.tx
        .respond(respond(&ir.request, 486, Some("u4")))
        .await
        .unwrap();
    let ev = ir.events.recv().await;
    assert_eq!(ev, Some(TuEvent::Timeout));
    assert_eq!(
        h.rec.elapsed_ms(),
        32_000,
        "Timer H = 64*T1 = 32 s, 0 ms tolerance"
    );
    let t = h.rec.instants("SIP/2.0 486");
    // initial + 10 retransmissions (500 1500 3500 7500 11500 15500 19500 23500 27500 31500)
    assert_eq!(t.len(), 11);
    assert_eq!(
        &t[1..],
        &cum(&[500, 1000, 2000, 4000, 4000, 4000, 4000, 4000, 4000, 4000])[..]
    );
    assert_eq!(
        ir.events.recv().await,
        None,
        "event channel closes with the transaction"
    );
    assert_eq!(h.counts(), (0, 0));
    println!(
        "T04 486 instants (ms): {t:?}; TU Timeout at {} ms",
        h.rec.elapsed_ms()
    );
}

// ================================================================ T05

#[tokio::test(start_paused = true)]
async fn t05_accepted_state_passes_ack_to_tu_and_timer_l_terminates() {
    let mut h = H::new();
    h.feed_req(invite("z9hG4bKt05"), Udp).await;
    let mut ir = h.next_request().await;
    ir.tx
        .respond(respond(&ir.request, 200, Some("u5")))
        .await
        .unwrap();
    h.at(50).await;
    assert_eq!(h.rec.instants("SIP/2.0 200"), ms(&[0]));
    let ack = mk_request("ACK", "z9hG4bKt05", PEER, 1, "ACK", Some("u5"));
    h.feed_req(ack, Udp).await;
    match ir.events.try_recv() {
        Ok(TuEvent::Ack(a)) => {
            assert_eq!(a.method, Method::Ack);
            assert_eq!(a.headers.to().unwrap().tag(), Some("u5"));
        }
        other => panic!("expected the ACK to reach the TU, got {other:?}"),
    }
    // the transaction does not retransmit the 2xx itself (the TU does)
    h.at(31_999).await;
    assert_eq!(h.rec.instants("SIP/2.0 200"), ms(&[0]));
    assert_eq!(h.counts(), (1, 0));
    h.at(32_000).await;
    assert_eq!(
        h.counts(),
        (0, 0),
        "Timer L = 64*T1 ended the Accepted state"
    );
    assert_eq!(ir.events.recv().await, None);
}

#[test]
fn t05_accepted_sim() {
    let inv = invite("z9hG4bKsim05");
    let (m, init) = ServerInvite::new(inv.clone(), false, TimerConfig::default());
    let mut s = Sim::new(m, init);
    s.run_to(100);
    s.respond(respond(&inv, 200, Some("t")));
    assert_eq!(s.m.state(), State::Accepted);
    assert!(s.started.contains(&(100, Timer::L, 32_000)));
    // INVITE retransmission: absorbed
    let n = s.sent.len();
    s.feed(SipMessage::Request(inv.clone()));
    assert_eq!(s.sent.len(), n);
    // a second 2xx from the TU goes out, a non-2xx does not
    s.respond(respond(&inv, 200, Some("t")));
    assert_eq!(s.sent.len(), n + 1);
    s.respond(respond(&inv, 486, Some("t")));
    assert_eq!(s.sent.len(), n + 1);
    s.run_to(32_100);
    assert_eq!(s.ended, Some(32_100));
}

// ================================================================ T06

#[tokio::test(start_paused = true)]
async fn t06_non_invite_server_retransmits_final_and_timer_j_is_32s_on_udp() {
    let mut h = H::new();
    let opt = mk_request("OPTIONS", "z9hG4bKt06", PEER, 7, "OPTIONS", None);
    h.feed_req(opt.clone(), Udp).await;
    let ir = h.next_request().await;
    // retransmission while in Trying: nothing to resend yet
    h.feed_req(opt.clone(), Udp).await;
    assert_eq!(h.rec.count(), 0);
    ir.tx
        .respond(respond(&ir.request, 200, Some("o1")))
        .await
        .unwrap();
    h.at(1_000).await;
    h.feed_req(opt.clone(), Udp).await;
    h.at(5_000).await;
    h.feed_req(opt.clone(), Udp).await;
    let t = h.rec.instants("SIP/2.0 200");
    assert_eq!(t, ms(&[0, 1000, 5000]));
    let w = h.rec.all();
    assert_eq!(
        wire(&w[0].1),
        wire(&w[1].1),
        "retransmitted final is byte-identical"
    );
    assert!(
        h.tu.try_recv().is_err(),
        "retransmissions never reach the TU"
    );
    h.at(31_999).await;
    assert_eq!(h.counts(), (1, 0));
    h.at(32_000).await;
    assert_eq!(
        h.counts(),
        (0, 0),
        "Timer J = 64*T1 = 32000 ms after the final response"
    );
    println!("T06 UDP: 200 sent at {t:?}, Timer J ended the transaction at 32000 ms");
}

#[tokio::test(start_paused = true)]
async fn t06_non_invite_server_on_tcp_terminates_immediately_timer_j_is_zero() {
    let mut h = H::new();
    let opt = mk_request("OPTIONS", "z9hG4bKt06t", PEER, 7, "OPTIONS", None);
    h.feed_req(opt, Tcp).await;
    let ir = h.next_request().await;
    assert_eq!(h.counts(), (1, 0));
    ir.tx
        .respond(respond(&ir.request, 200, Some("o1")))
        .await
        .unwrap();
    settle().await;
    assert_eq!(h.counts(), (0, 0), "TCP: Timer J = 0, no waiting");
    assert_eq!(h.rec.elapsed_ms(), 0, "and no virtual time passed");
    assert_eq!(h.rec.instants("SIP/2.0 200"), ms(&[0]));
}

#[test]
fn t06_non_invite_server_sim() {
    let req = mk_request("OPTIONS", "z9hG4bKsim06", PEER, 1, "OPTIONS", None);
    let mut s = Sim::new(
        ServerNonInvite::new(req.clone(), false, TimerConfig::default()),
        vec![],
    );
    assert_eq!(s.m.state(), State::Trying);
    s.respond(respond(&req, 100, None));
    assert_eq!(s.m.state(), State::Proceeding);
    s.feed(SipMessage::Request(req.clone()));
    assert_eq!(s.sent.len(), 2, "provisional resent");
    s.run_to(10);
    s.respond(respond(&req, 404, Some("t")));
    assert_eq!(s.m.state(), State::Completed);
    s.respond(respond(&req, 200, Some("t")));
    assert_eq!(s.sent.len(), 3, "a second final from the TU is ignored");
    s.run_to(40_000);
    assert_eq!(s.ended, Some(10 + 32_000));
}

// ================================================================ T07

#[tokio::test(start_paused = true)]
async fn t07_invite_client_timer_a_schedule_and_timer_b_at_32s() {
    let h = H::new();
    let mut tx = h
        .layer
        .send_request(client_request("INVITE"), H::dest(), Udp)
        .unwrap();
    let ev = tx.events.recv().await;
    assert_eq!(ev, Some(TuEvent::Timeout));
    assert_eq!(h.rec.elapsed_ms(), 32_000, "Timer B = 64*T1");
    let t = h.rec.instants("INVITE");
    assert_eq!(
        t,
        ms(&[0, 500, 1500, 3500, 7500, 15500, 31500]),
        "Timer A doubles without a cap"
    );
    let all = h.rec.all();
    assert!(
        all.iter().all(|(_, o)| wire(o) == wire(&all[0].1)),
        "retransmissions are identical"
    );
    let w = wire(&all[0].1);
    println!("T07 INVITE instants (ms): {t:?}, TU timeout at 32000\n{w}");
    let via = w.lines().find(|l| l.starts_with("Via:")).unwrap();
    assert!(
        via.starts_with("Via: SIP/2.0/UDP 192.0.2.10:5060;branch=z9hG4bK"),
        "{via}"
    );
    assert_eq!(tx.events.recv().await, None);
    assert_eq!(h.counts(), (0, 0));
}

#[test]
fn t07_invite_client_sim() {
    let mut req = invite("z9hG4bKsim07");
    req.headers
        .set("Via", "SIP/2.0/UDP 192.0.2.10:5060;branch=z9hG4bKsim07");
    let (m, init) = ClientInvite::new(req, false, TimerConfig::default());
    let mut s = Sim::new(m, init);
    s.run_to(40_000);
    assert_eq!(
        s.sent_times(),
        ms(&[0, 500, 1500, 3500, 7500, 15500, 31500])
    );
    assert_eq!(s.events, vec![(32_000, TuEvent::Timeout)]);
    assert_eq!(s.ended, Some(32_000));
}

// ================================================================ T08

#[tokio::test(start_paused = true)]
async fn t08_invite_client_non_2xx_generates_ack_resends_it_and_timer_d_ends() {
    let h = H::new();
    let mut tx = h
        .layer
        .send_request(client_request("INVITE"), H::dest(), Udp)
        .unwrap();
    settle().await;
    let invite_wire = wire(&h.rec.all()[0].1);
    h.at(100).await;
    let resp = respond(&tx.request, 486, Some("rtag"));
    h.feed(inc_resp(resp.clone(), Udp)).await;
    let acks = h.rec.instants("ACK");
    assert_eq!(acks, ms(&[100]));
    let ack = h
        .rec
        .all()
        .into_iter()
        .find(|(_, o)| first_line(o).starts_with("ACK"))
        .unwrap()
        .1;
    let aw = wire(&ack);
    println!("T08 ACK wire:\n{aw}");
    let req_via = tx.request.headers.get("Via").unwrap().to_string();
    assert!(aw.starts_with("ACK sip:fax@192.0.2.10 SIP/2.0\r\n"));
    assert_eq!(aw.matches("Via:").count(), 1);
    assert!(
        aw.contains(&format!("Via: {req_via}\r\n")),
        "same Via/branch as the INVITE: {aw}"
    );
    assert!(
        aw.contains("To: <sip:fax@192.0.2.10>;tag=rtag\r\n"),
        "To tag from the response: {aw}"
    );
    assert!(aw.contains("CSeq: 1 ACK\r\n"));
    assert!(aw.contains("From: <sip:gw@10.0.0.1>;tag=g1\r\n"));
    assert!(invite_wire.contains(&format!(
        "Call-ID: {}\r\n",
        tx.request.headers.call_id().unwrap()
    )));
    assert!(aw.contains(&format!(
        "Call-ID: {}\r\n",
        tx.request.headers.call_id().unwrap()
    )));
    // TU is told once
    assert_eq!(
        tx.events.try_recv().ok(),
        Some(TuEvent::Response(resp.clone()))
    );
    // retransmitted 486 -> ACK again, TU not told again
    h.at(300).await;
    h.feed(inc_resp(resp.clone(), Udp)).await;
    assert_eq!(h.rec.instants("ACK"), ms(&[100, 300]));
    assert!(tx.events.try_recv().is_err());
    // Timer A was cancelled by the response: no INVITE retransmission after 100 ms
    h.at(2_000).await;
    assert_eq!(h.rec.instants("INVITE"), ms(&[0]));
    // Timer D = 32 s after the first final response (100 ms)
    h.at(32_099).await;
    assert_eq!(h.counts(), (0, 1));
    h.at(32_100).await;
    assert_eq!(h.counts(), (0, 0), "Timer D");
    assert_eq!(tx.events.recv().await, None);
}

#[test]
fn t08_ack_construction_copies_route_and_uses_response_to() {
    let mut inv = invite("z9hG4bKsim08");
    inv.headers.push("Route", "<sip:p1.example.com;lr>");
    inv.headers.push("Route", "<sip:p2.example.com;lr>");
    let resp = respond(&inv, 404, Some("zzz"));
    let ack = build_non_2xx_ack(&inv, &resp);
    let w = String::from_utf8(ack.to_bytes().to_vec()).unwrap();
    assert!(
        w.contains("Route: <sip:p1.example.com;lr>\r\nRoute: <sip:p2.example.com;lr>\r\n"),
        "{w}"
    );
    assert!(w.contains("To: <sip:fax@192.0.2.10>;tag=zzz\r\n"));
    assert!(w.contains("Max-Forwards: 70\r\n"));
}

// ================================================================ T09

#[tokio::test(start_paused = true)]
async fn t09_invite_client_2xx_is_passed_up_once_retransmissions_are_distinct_events() {
    let mut h = H::new();
    let mut tx = h
        .layer
        .send_request(client_request("INVITE"), H::dest(), Udp)
        .unwrap();
    settle().await;
    h.at(50).await;
    let r180 = respond(&tx.request, 180, Some("c1"));
    h.feed(inc_resp(r180.clone(), Udp)).await;
    h.at(100).await;
    let r200 = respond(&tx.request, 200, Some("c1"));
    h.feed(inc_resp(r200.clone(), Udp)).await;
    h.at(400).await;
    h.feed(inc_resp(r200.clone(), Udp)).await;
    h.at(900).await;
    h.feed(inc_resp(r200.clone(), Udp)).await;
    let evs = drain_events(&mut tx.events);
    assert_eq!(
        evs,
        vec![
            TuEvent::Response(r180),
            TuEvent::Response(r200.clone()),
            TuEvent::Retransmitted2xx(r200.clone()),
            TuEvent::Retransmitted2xx(r200)
        ],
        "180 once, 200 once as Response, the two copies as Retransmitted2xx (M1b review fix)"
    );
    assert!(
        h.rec.instants("ACK").is_empty(),
        "the TU (not the transaction) ACKs a 2xx"
    );
    assert_eq!(h.rec.instants("INVITE"), ms(&[0]), "1xx stopped Timer A");
    assert!(
        h.tu.try_recv_ok().is_none(),
        "retransmitted 2xx belong to the transaction, they are not stray responses"
    );
    // Accepted lasts 64*T1 after the 2xx (RFC 6026 Timer M)
    h.at(32_099).await;
    assert_eq!(h.counts(), (0, 1));
    h.at(32_100).await;
    assert_eq!(h.counts(), (0, 0));
    println!("T09: TU events = [Response(180), Response(200), Retransmitted2xx x2 (400, 900 ms)]");
}

trait TryRecvOk {
    fn try_recv_ok(&mut self) -> Option<()>;
}
impl TryRecvOk for mpsc::UnboundedReceiver<TuInput> {
    fn try_recv_ok(&mut self) -> Option<()> {
        self.try_recv().ok().map(|_| ())
    }
}

// ================================================================ T10

#[tokio::test(start_paused = true)]
async fn t10_non_invite_client_timer_e_schedule_and_timer_f() {
    let h = H::new();
    let mut tx = h
        .layer
        .send_request(client_request("OPTIONS"), H::dest(), Udp)
        .unwrap();
    assert_eq!(tx.events.recv().await, Some(TuEvent::Timeout));
    assert_eq!(h.rec.elapsed_ms(), 32_000, "Timer F");
    let t = h.rec.instants("OPTIONS");
    let want: Vec<u64> = std::iter::once(0)
        .chain(cum(&[
            500, 1000, 2000, 4000, 4000, 4000, 4000, 4000, 4000, 4000,
        ]))
        .collect();
    assert_eq!(t, want, "E: 500, 1000, 2000, then capped at T2 = 4000");
    println!("T10(a) OPTIONS instants: {t:?}; Timer F at 32000");
    assert_eq!(tx.events.recv().await, None);
}

#[tokio::test(start_paused = true)]
async fn t10_non_invite_client_provisional_switches_to_t2_interval() {
    let h = H::new();
    let mut tx = h
        .layer
        .send_request(client_request("OPTIONS"), H::dest(), Udp)
        .unwrap();
    settle().await;
    h.at(600).await;
    let prov = respond(&tx.request, 100, None);
    h.feed(inc_resp(prov.clone(), Udp)).await;
    assert_eq!(tx.events.recv().await, Some(TuEvent::Response(prov)));
    let _ = tx.events.recv().await; // runs to Timer F
    let t = h.rec.instants("OPTIONS");
    // 0, 500 (E in Trying, next would be 1500), 1500 (E fires in Proceeding -> reset to T2),
    // then every 4000 ms
    assert_eq!(
        t,
        ms(&[0, 500, 1500, 5500, 9500, 13500, 17500, 21500, 25500, 29500])
    );
    println!("T10(b) OPTIONS instants with provisional at 600: {t:?}");
}

#[tokio::test(start_paused = true)]
async fn t10_non_invite_client_final_response_starts_timer_k() {
    let h = H::new();
    let mut tx = h
        .layer
        .send_request(client_request("OPTIONS"), H::dest(), Udp)
        .unwrap();
    settle().await;
    h.at(6_000).await;
    let fin = respond(&tx.request, 200, Some("s"));
    h.feed(inc_resp(fin.clone(), Udp)).await;
    h.at(7_000).await;
    h.feed(inc_resp(fin.clone(), Udp)).await; // retransmission: absorbed
    assert_eq!(drain_events(&mut tx.events), vec![TuEvent::Response(fin)]);
    assert_eq!(
        h.rec.instants("OPTIONS"),
        ms(&[0, 500, 1500, 3500]),
        "no retransmission after the response"
    );
    h.at(10_999).await;
    assert_eq!(h.counts(), (0, 1));
    h.at(11_000).await;
    assert_eq!(
        h.counts(),
        (0, 0),
        "Timer K = T4 = 5000 ms after the final response"
    );
    assert_eq!(
        tx.events.recv().await,
        None,
        "no Timeout: Timer F was cancelled"
    );
}

#[test]
fn t10_non_invite_client_sim_and_tcp() {
    let mut req = mk_request(
        "OPTIONS",
        "z9hG4bKsim10",
        "192.0.2.10:5060",
        1,
        "OPTIONS",
        None,
    );
    req.headers
        .set("Via", "SIP/2.0/UDP 192.0.2.10:5060;branch=z9hG4bKsim10");
    let (m, init) = ClientNonInvite::new(req.clone(), false, TimerConfig::default());
    let mut s = Sim::new(m, init);
    s.run_to(500);
    assert_eq!(s.m.state(), State::Trying);
    s.feed(SipMessage::Response(respond(&req, 180, None)));
    assert_eq!(s.m.state(), State::Proceeding);
    s.run_to(40_000);
    assert_eq!(s.events.last(), Some(&(32_000, TuEvent::Timeout)));
    // TCP: one send, no E, F still armed, K = 0
    let (m, init) = ClientNonInvite::new(req.clone(), true, TimerConfig::default());
    let mut s = Sim::new(m, init);
    s.run_to(31_999);
    assert_eq!(s.sent_times(), ms(&[0]));
    s.feed(SipMessage::Response(respond(&req, 200, Some("t"))));
    assert_eq!(
        s.ended,
        Some(31_999),
        "Timer K is 0 on TCP: terminated right away"
    );
}

// ================================================================ T11

#[tokio::test(start_paused = true)]
async fn t11_tcp_invite_client_does_not_retransmit_but_timer_b_still_applies() {
    let h = H::new();
    let mut tx = h
        .layer
        .send_request(client_request("INVITE"), H::dest(), Tcp)
        .unwrap();
    assert_eq!(tx.events.recv().await, Some(TuEvent::Timeout));
    assert_eq!(h.rec.elapsed_ms(), 32_000);
    assert_eq!(
        h.rec.instants("INVITE"),
        ms(&[0]),
        "sent exactly once over TCP"
    );
    let w = wire(&h.rec.all()[0].1);
    assert!(
        w.contains("Via: SIP/2.0/TCP 192.0.2.10:5060;branch=z9hG4bK"),
        "{w}"
    );
}

#[tokio::test(start_paused = true)]
async fn t11_tcp_non_invite_client_does_not_retransmit() {
    let h = H::new();
    let mut tx = h
        .layer
        .send_request(client_request("OPTIONS"), H::dest(), Tcp)
        .unwrap();
    assert_eq!(tx.events.recv().await, Some(TuEvent::Timeout));
    assert_eq!(h.rec.instants("OPTIONS"), ms(&[0]));
}

#[tokio::test(start_paused = true)]
async fn t11_tcp_invite_client_non_2xx_acks_and_terminates_immediately() {
    let h = H::new();
    let tx = h
        .layer
        .send_request(client_request("INVITE"), H::dest(), Tcp)
        .unwrap();
    settle().await;
    h.at(10).await;
    h.feed(inc_resp(respond(&tx.request, 486, Some("t")), Tcp))
        .await;
    assert_eq!(h.rec.instants("ACK"), ms(&[10]));
    assert_eq!(h.counts(), (0, 0), "Timer D = 0 on TCP");
}

#[tokio::test(start_paused = true)]
async fn t11_tcp_invite_server_does_not_retransmit_and_ack_terminates_at_once() {
    let mut h = H::new();
    h.feed_req(invite("z9hG4bKt11"), Tcp).await;
    let mut ir = h.next_request().await;
    ir.tx
        .respond(respond(&ir.request, 486, Some("u")))
        .await
        .unwrap();
    h.at(20_000).await;
    assert_eq!(h.rec.instants("SIP/2.0 486"), ms(&[0]), "no Timer G on TCP");
    h.feed_req(
        mk_request("ACK", "z9hG4bKt11", PEER, 1, "ACK", Some("u")),
        Tcp,
    )
    .await;
    assert_eq!(h.counts(), (0, 0), "Timer I = 0 on TCP");
    assert!(drain_events(&mut ir.events).is_empty());
    // and Timer H still applies without an ACK
    h.feed_req(invite("z9hG4bKt11b"), Tcp).await;
    let mut ir2 = h.next_request().await;
    ir2.tx
        .respond(respond(&ir2.request, 486, Some("u")))
        .await
        .unwrap();
    assert_eq!(ir2.events.recv().await, Some(TuEvent::Timeout));
    assert_eq!(h.rec.elapsed_ms(), 20_000 + 32_000);
}

// ================================================================ T12

#[tokio::test(start_paused = true)]
async fn t12_cancel_of_pending_invite_gives_200_for_cancel_and_487_for_invite() {
    let mut h = H::new();
    h.feed_req(invite("z9hG4bKt12"), Udp).await;
    let mut ir = h.next_request().await;
    ir.tx
        .respond(respond(&ir.request, 180, Some("uas1")))
        .await
        .unwrap();
    h.at(1_000).await;
    let cancel = mk_request("CANCEL", "z9hG4bKt12", PEER, 1, "CANCEL", None);
    h.feed_req(cancel.clone(), Udp).await;
    let at_1000: Vec<_> = h
        .rec
        .all()
        .into_iter()
        .filter(|(t, _)| *t == 1000)
        .collect();
    assert_eq!(at_1000.len(), 2, "{at_1000:?}");
    let w200 = at_1000
        .iter()
        .map(|(_, o)| wire(o))
        .find(|w| w.starts_with("SIP/2.0 200 OK"))
        .expect("200 for the CANCEL");
    let w487 = at_1000
        .iter()
        .map(|(_, o)| wire(o))
        .find(|w| w.starts_with("SIP/2.0 487"))
        .expect("487 for the INVITE");
    println!("T12 200:\n{w200}\nT12 487:\n{w487}");
    assert!(w200.contains("CSeq: 1 CANCEL\r\n"));
    assert!(w487.contains("CSeq: 1 INVITE\r\n"));
    assert!(
        w487.contains("To: <sip:fax@192.0.2.10>;tag=uas1\r\n"),
        "487 reuses the To tag of the 180: {w487}"
    );
    assert!(w200.contains("tag="), "CANCEL answer carries a To tag");
    assert_eq!(drain_events(&mut ir.events), vec![TuEvent::Cancelled]);
    // the UAC ACKs the 487 -> Confirmed, G stops
    h.feed_req(
        mk_request("ACK", "z9hG4bKt12", PEER, 1, "ACK", Some("uas1")),
        Udp,
    )
    .await;
    h.at(10_000).await;
    assert_eq!(
        h.rec.instants("SIP/2.0 487"),
        ms(&[1000]),
        "no G retransmission after the ACK"
    );
    // a retransmitted CANCEL is answered from the CANCEL's own transaction
    h.feed_req(cancel, Udp).await;
    let t200: Vec<u64> = h.rec.instants("SIP/2.0 200");
    assert_eq!(t200, ms(&[1000, 10_000]));
    assert_eq!(h.rec.instants("SIP/2.0 487").len(), 1);
}

#[tokio::test(start_paused = true)]
async fn t12_cancel_without_matching_transaction_gets_481() {
    let mut h = H::new();
    let cancel = mk_request("CANCEL", "z9hG4bKnomatch", PEER, 1, "CANCEL", None);
    h.feed_req(cancel, Udp).await;
    let sent = h.rec.all();
    assert_eq!(sent.len(), 1);
    let w = wire(&sent[0].1);
    println!("T12 481:\n{w}");
    assert!(w.starts_with("SIP/2.0 481 Call/Transaction Does Not Exist\r\n"));
    assert!(w.contains("CSeq: 1 CANCEL\r\n"));
    assert!(h.tu.try_recv().is_err(), "the TU is not bothered");
}

#[tokio::test(start_paused = true)]
async fn t12_cancel_after_final_response_gets_200_but_invite_is_untouched() {
    let mut h = H::new();
    h.feed_req(invite("z9hG4bKt12c"), Udp).await;
    let mut ir = h.next_request().await;
    ir.tx
        .respond(respond(&ir.request, 200, Some("u")))
        .await
        .unwrap();
    h.at(100).await;
    h.feed_req(
        mk_request("CANCEL", "z9hG4bKt12c", PEER, 1, "CANCEL", None),
        Udp,
    )
    .await;
    assert_eq!(
        h.rec.instants("SIP/2.0 200"),
        ms(&[0, 100]),
        "CANCEL answered 200"
    );
    assert!(
        h.rec.instants("SIP/2.0 487").is_empty(),
        "INVITE already final: no 487"
    );
    assert!(
        drain_events(&mut ir.events).is_empty(),
        "no Cancelled event"
    );
}

#[test]
fn t12_cancel_sim_uses_existing_tag_or_generates_one() {
    let inv = invite("z9hG4bKsim12");
    let (m, init) = ServerInvite::new(inv.clone(), false, TimerConfig::default());
    let mut s = Sim::new(m, init);
    let a = s.m.on_cancel();
    s.apply(a);
    let SipMessage::Response(r) = &s.sent[0].1 else {
        panic!()
    };
    assert_eq!(r.status.code(), 487);
    assert!(
        r.headers.to().unwrap().tag().is_some_and(|t| t.len() == 8),
        "generated tag"
    );
    assert_eq!(s.events, vec![(0, TuEvent::Cancelled)]);
    // second CANCEL: state is Completed now, nothing more happens
    let a = s.m.on_cancel();
    assert!(a.is_empty());
}

// ================================================================ T13

#[test]
fn t13_server_key_matching_rules() {
    let key = |r: &Request| ServerKey::from_request(r).expect("has magic cookie");
    let inv = mk_request("INVITE", "z9hG4bKaaa", "10.0.0.1:5060", 1, "INVITE", None);
    let opt = mk_request("OPTIONS", "z9hG4bKaaa", "10.0.0.1:5060", 1, "OPTIONS", None);
    assert_ne!(key(&inv), key(&opt), "same branch, different method");
    let other_sb = mk_request("INVITE", "z9hG4bKaaa", "10.0.0.2:5060", 1, "INVITE", None);
    assert_ne!(
        key(&inv),
        key(&other_sb),
        "same branch, different sent-by host"
    );
    let other_port = mk_request("INVITE", "z9hG4bKaaa", "10.0.0.1:5070", 1, "INVITE", None);
    assert_ne!(
        key(&inv),
        key(&other_port),
        "same branch, different sent-by port"
    );
    let other_branch = mk_request("INVITE", "z9hG4bKbbb", "10.0.0.1:5060", 1, "INVITE", None);
    assert_ne!(key(&inv), key(&other_branch));
    assert_eq!(key(&inv), key(&inv.clone()));
    // default port and case-insensitive host
    let a = mk_request(
        "INVITE",
        "z9hG4bKaaa",
        "Host.Example.COM",
        1,
        "INVITE",
        None,
    );
    let b = mk_request(
        "INVITE",
        "z9hG4bKaaa",
        "host.example.com:5060",
        1,
        "INVITE",
        None,
    );
    assert_eq!(key(&a), key(&b));
    // ACK maps onto the INVITE transaction; CANCEL does not
    let ack = mk_request("ACK", "z9hG4bKaaa", "10.0.0.1:5060", 1, "ACK", None);
    let cancel = mk_request("CANCEL", "z9hG4bKaaa", "10.0.0.1:5060", 1, "CANCEL", None);
    assert_eq!(key(&ack), key(&inv));
    assert_ne!(key(&cancel), key(&inv));
    // no magic cookie: not matchable
    let old = mk_request("INVITE", "oldstyle123", "10.0.0.1:5060", 1, "INVITE", None);
    assert_eq!(ServerKey::from_request(&old), None);
    // client keys: branch + CSeq method
    let r = respond(&inv, 200, Some("t"));
    assert_eq!(
        ClientKey::from_response(&r),
        Some(ClientKey {
            branch: "z9hG4bKaaa".into(),
            method: Method::Invite
        })
    );
    assert_eq!(ClientKey::from_request(&inv), ClientKey::from_response(&r));
}

#[tokio::test(start_paused = true)]
async fn t13_layer_keeps_distinct_transactions_apart_and_matches_retransmissions() {
    let mut h = H::new();
    // same branch, different method -> two transactions
    h.feed_req(
        mk_request("INVITE", "z9hG4bKm", "10.0.0.1:5060", 1, "INVITE", None),
        Udp,
    )
    .await;
    h.feed_req(
        mk_request("OPTIONS", "z9hG4bKm", "10.0.0.1:5060", 1, "OPTIONS", None),
        Udp,
    )
    .await;
    // same branch + method, different sent-by -> another one
    h.feed_req(
        mk_request("INVITE", "z9hG4bKm", "10.0.0.9:5060", 1, "INVITE", None),
        Udp,
    )
    .await;
    let a = h.next_request().await;
    let b = h.next_request().await;
    let c = h.next_request().await;
    assert_eq!(
        [
            a.request.method.clone(),
            b.request.method.clone(),
            c.request.method.clone()
        ],
        [Method::Invite, Method::Options, Method::Invite]
    );
    assert_eq!(h.counts(), (3, 0));
    // an exact retransmission creates nothing new
    h.feed_req(
        mk_request("OPTIONS", "z9hG4bKm", "10.0.0.1:5060", 1, "OPTIONS", None),
        Udp,
    )
    .await;
    assert!(h.tu.try_recv().is_err());
    assert_eq!(h.counts(), (3, 0));
    // answering one does not affect the others
    b.tx.respond(respond(&b.request, 200, Some("x")))
        .await
        .unwrap();
    settle().await;
    assert_eq!(h.rec.instants("SIP/2.0 200"), ms(&[0]));
}

#[tokio::test(start_paused = true)]
async fn t13_ack_for_non_2xx_hits_the_invite_transaction_other_acks_go_to_the_tu() {
    let mut h = H::new();
    h.feed_req(invite("z9hG4bKack"), Udp).await;
    let ir = h.next_request().await;
    ir.tx
        .respond(respond(&ir.request, 486, Some("u")))
        .await
        .unwrap();
    h.at(700).await;
    h.feed_req(
        mk_request("ACK", "z9hG4bKack", PEER, 1, "ACK", Some("u")),
        Udp,
    )
    .await;
    assert!(
        h.tu.try_recv().is_err(),
        "matched ACK is consumed by the transaction"
    );
    h.at(5_000).await;
    assert_eq!(
        h.rec.instants("SIP/2.0 486"),
        ms(&[0, 500]),
        "the ACK stopped Timer G (Confirmed)"
    );
    // ACK of a 2xx has its own branch: no transaction, goes to the TU
    h.feed_req(
        mk_request("ACK", "z9hG4bKother", PEER, 1, "ACK", Some("u")),
        Udp,
    )
    .await;
    match h.tu.try_recv() {
        Ok(TuInput::Ack(i)) => assert_eq!(i.source, PEER.parse::<SocketAddr>().unwrap()),
        _ => panic!("expected TuInput::Ack"),
    }
}

#[tokio::test(start_paused = true)]
async fn t13_responses_match_on_branch_and_cseq_method_requests_without_cookie_are_stateless() {
    let mut h = H::new();
    let tx = h
        .layer
        .send_request(client_request("INVITE"), H::dest(), Udp)
        .unwrap();
    settle().await;
    // same branch but CSeq method OPTIONS -> not this transaction
    let mut wrong = respond(&tx.request, 200, Some("t"));
    wrong.headers.set("CSeq", "1 OPTIONS");
    h.feed(inc_resp(wrong, Udp)).await;
    assert!(matches!(h.tu.try_recv(), Ok(TuInput::StrayResponse(_))));
    // different branch -> stray
    let mut other = respond(&tx.request, 200, Some("t"));
    other
        .headers
        .set("Via", "SIP/2.0/UDP 192.0.2.10:5060;branch=z9hG4bKdifferent");
    h.feed(inc_resp(other, Udp)).await;
    assert!(matches!(h.tu.try_recv(), Ok(TuInput::StrayResponse(_))));
    // RFC 2543 style request: handed to the TU without a transaction
    h.feed_req(
        mk_request("INVITE", "oldstyle1", PEER, 1, "INVITE", None),
        Udp,
    )
    .await;
    assert!(matches!(h.tu.try_recv(), Ok(TuInput::Stateless(_))));
    assert_eq!(h.counts(), (0, 1));
}

#[tokio::test(start_paused = true)]
async fn t13_client_api_rejects_ack_and_duplicates_and_fills_in_branch() {
    let h = H::new();
    let err = h
        .layer
        .send_request(client_request("ACK"), H::dest(), Udp)
        .err()
        .unwrap();
    assert!(matches!(err, TransactionError::AckHasNoTransaction));
    let mut r = client_request("OPTIONS");
    r.headers
        .push("Via", "SIP/2.0/UDP whatever;branch=z9hG4bKfixed");
    let t = h.layer.send_request(r.clone(), H::dest(), Udp).unwrap();
    assert_eq!(
        t.request.headers.top_via().unwrap().branch(),
        Some("z9hG4bKfixed")
    );
    assert!(matches!(
        h.layer.send_request(r, H::dest(), Udp),
        Err(TransactionError::Duplicate)
    ));
    let auto = h
        .layer
        .send_request(client_request("OPTIONS"), H::dest(), Udp)
        .unwrap();
    let br = auto
        .request
        .headers
        .top_via()
        .unwrap()
        .branch()
        .unwrap()
        .to_string();
    assert!(br.starts_with("z9hG4bK") && br.len() > 10, "{br}");
    assert_eq!(new_branch().len(), 7 + 16);
}

#[tokio::test(start_paused = true)]
async fn t13_shutdown_ends_every_task() {
    let mut h = H::new();
    h.feed_req(invite("z9hG4bKsd"), Udp).await;
    let _ir = h.next_request().await;
    let _c = h
        .layer
        .send_request(client_request("OPTIONS"), H::dest(), Udp)
        .unwrap();
    settle().await;
    assert_eq!(h.counts(), (1, 1));
    h.layer.shutdown().await;
    assert_eq!(h.counts(), (0, 0));
}
