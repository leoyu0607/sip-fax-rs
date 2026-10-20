//! M1c dialog / UAS core tests G01-G17, C05 and C10.
//!
//! The endpoint runs on a detached transport (no sockets) under `tokio::time::pause()`; a test
//! plays the remote gateway by injecting messages and reads exactly what the stack put on the
//! wire together with the virtual millisecond it was sent at. There is no real sleeping.

mod common;

use std::sync::Arc;
use std::time::Duration;

use common::*;
use md5::{Digest, Md5};
use sip_core::dialog::session_timer::Refresher;
use sip_core::dialog::{Dialog, DialogError, Role, uas};
use sip_core::endpoint::{
    Credentials, DialogControl, DialogEvent, DialogHandle, Endpoint, EndpointConfig, IncomingCall,
    InviteOptions, ReinviteOutcome, TerminateReason,
};
use sip_core::message::{
    Method, NameAddr, Request, Response, SipMessage, SipUri, StatusCode, parse_datagram,
};
use sip_core::sdp::negotiate::{AnswerPolicy, OriginTracker, answer_offer};
use sip_core::sdp::{SessionDescription, parse_sdp};
use sip_core::transport::{Incoming, Outgoing, TransportHandle, TransportKind};
use tokio::sync::mpsc;
use tokio::time::Instant;

const T38_OFFER: &str = include_str!("vectors/sdp/t38_minimal_offer.sdp");
const AUDIO_OFFER: &str = include_str!("vectors/sdp/sonus_audio_offer.sdp");
const FROM_TAG: &str = "gw1";
const GW_CONTACT: &str = "<sip:gw@10.0.0.1:5070>";

// ---------------------------------------------------------------- harness

struct W {
    ep: Endpoint,
    calls: mpsc::UnboundedReceiver<IncomingCall>,
    rec: Recorder,
    inject: mpsc::Sender<Incoming>,
    t0: Instant,
}

impl W {
    fn new() -> W {
        W::with(|_| {})
    }

    fn with(f: impl FnOnce(&mut EndpointConfig)) -> W {
        let d = TransportHandle::detached(LOCAL.parse().unwrap());
        let mut cfg = EndpointConfig::new();
        f(&mut cfg);
        let (ep, calls) = Endpoint::detached(cfg, d.handle, d.incoming);
        let t0 = Instant::now();
        let rec = Recorder::spawn(t0, d.outgoing);
        W {
            ep,
            calls,
            rec,
            inject: d.inject,
            t0,
        }
    }

    async fn at(&self, ms: u64) {
        tokio::time::sleep_until(self.t0 + Duration::from_millis(ms)).await;
        settle().await;
    }

    fn now_ms(&self) -> u64 {
        self.rec.elapsed_ms()
    }

    async fn feed(&self, m: SipMessage) {
        self.inject
            .send(incoming(m, TransportKind::Udp))
            .await
            .unwrap();
        settle().await;
    }

    async fn feed_req(&self, r: Request) {
        self.feed(SipMessage::Request(r)).await;
    }

    async fn feed_resp(&self, r: Response) {
        self.feed(SipMessage::Response(r)).await;
    }

    async fn next_call(&mut self) -> IncomingCall {
        settle().await;
        self.calls.try_recv().expect("an IncomingCall")
    }

    fn requests(&self, method: &str) -> Vec<(u64, Request)> {
        self.rec
            .all()
            .into_iter()
            .filter_map(|(t, o)| match o {
                Outgoing::Request { req, .. } if req.method.as_str() == method => Some((t, req)),
                _ => None,
            })
            .collect()
    }

    /// Responses to requests of this CSeq method with this status code.
    fn responses(&self, code: u16, cseq_method: &str) -> Vec<(u64, Response)> {
        self.rec
            .all()
            .into_iter()
            .filter_map(|(t, o)| match o {
                Outgoing::Response { resp, .. }
                    if resp.status.code() == code
                        && resp.headers.cseq().map(|c| c.method.to_string()).as_deref()
                            == Ok(cseq_method) =>
                {
                    Some((t, resp))
                }
                _ => None,
            })
            .collect()
    }

    fn ok_to_invite(&self) -> Vec<(u64, Response)> {
        self.responses(200, "INVITE")
    }

    fn our_tag(&self) -> String {
        let (_, r) = self.ok_to_invite().remove(0);
        r.headers.to().unwrap().tag().unwrap().to_string()
    }
}

fn sdp(text: &str) -> SessionDescription {
    parse_sdp(text.as_bytes()).unwrap()
}

fn answer_of(offer: &SessionDescription) -> SessionDescription {
    let policy = AnswerPolicy::new("192.0.2.10", 20000, 30000);
    let mut o = OriginTracker::random("192.0.2.10");
    answer_offer(offer, &policy, &mut o).unwrap()
}

struct Inv {
    branch: &'static str,
    cseq: u32,
    call_id: &'static str,
    record_routes: Vec<&'static str>,
    contact: Option<&'static str>,
    body: Option<&'static str>,
    extra: Vec<(&'static str, &'static str)>,
}

impl Default for Inv {
    fn default() -> Self {
        Inv {
            branch: "z9hG4bKinv1",
            cseq: 1,
            call_id: "call-1@10.0.0.1",
            record_routes: vec![],
            contact: Some(GW_CONTACT),
            body: Some(AUDIO_OFFER),
            extra: vec![],
        }
    }
}

fn parse_req(text: &str) -> Request {
    match parse_datagram(text.as_bytes()) {
        Ok(SipMessage::Request(r)) => r,
        other => panic!("bad test request {other:?}\n{text}"),
    }
}

fn peer_invite(i: &Inv) -> Request {
    let mut t = format!(
        "INVITE sip:fax@192.0.2.10 SIP/2.0\r\nVia: SIP/2.0/UDP 10.0.0.1:5060;branch={}\r\n\
         Max-Forwards: 70\r\nFrom: <sip:gw@10.0.0.1>;tag={FROM_TAG}\r\nTo: <sip:fax@192.0.2.10>\r\n\
         Call-ID: {}\r\nCSeq: {} INVITE\r\n",
        i.branch, i.call_id, i.cseq
    );
    for r in &i.record_routes {
        t.push_str(&format!("Record-Route: {r}\r\n"));
    }
    if let Some(c) = i.contact {
        t.push_str(&format!("Contact: {c}\r\n"));
    }
    for (n, v) in &i.extra {
        t.push_str(&format!("{n}: {v}\r\n"));
    }
    match i.body {
        Some(b) => t.push_str(&format!(
            "Content-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{b}",
            b.len()
        )),
        None => t.push_str("Content-Length: 0\r\n\r\n"),
    }
    parse_req(&t)
}

/// A request from the gateway inside the dialog created by `peer_invite` (To tag = ours).
fn peer_in_dialog(
    method: &str,
    branch: &str,
    cseq: u32,
    to_tag: &str,
    body: Option<&str>,
    extra: &[(&str, &str)],
) -> Request {
    let mut t = format!(
        "{method} sip:fax@192.0.2.10 SIP/2.0\r\nVia: SIP/2.0/UDP 10.0.0.1:5060;branch={branch}\r\n\
         Max-Forwards: 70\r\nFrom: <sip:gw@10.0.0.1>;tag={FROM_TAG}\r\nTo: <sip:fax@192.0.2.10>;tag={to_tag}\r\n\
         Call-ID: call-1@10.0.0.1\r\nCSeq: {cseq} {method}\r\nContact: {GW_CONTACT}\r\n"
    );
    for (n, v) in extra {
        t.push_str(&format!("{n}: {v}\r\n"));
    }
    match body {
        Some(b) => t.push_str(&format!(
            "Content-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{b}",
            b.len()
        )),
        None => t.push_str("Content-Length: 0\r\n\r\n"),
    }
    parse_req(&t)
}

/// Accepts the call and (optionally) ACKs it right away. Returns the dialog handle.
async fn establish(w: &mut W, inv: &Inv, ack_at: Option<u64>) -> DialogHandle {
    w.feed_req(peer_invite(inv)).await;
    let call = w.next_call().await;
    let answer = answer_of(&call.offer().expect("offer"));
    let dlg = call.accept(answer).await.unwrap();
    settle().await;
    if let Some(ms) = ack_at {
        w.at(ms).await;
        let tag = w.our_tag();
        w.feed_req(peer_in_dialog(
            "ACK",
            "z9hG4bKack1",
            inv.cseq,
            &tag,
            None,
            &[],
        ))
        .await;
    }
    dlg
}

async fn next_event(d: &mut DialogHandle) -> Option<DialogEvent> {
    settle().await;
    tokio::time::timeout(Duration::ZERO, d.next_event())
        .await
        .ok()
        .flatten()
}

fn hdr_all(r: &Request, n: &str) -> Vec<String> {
    r.headers.get_all(n).map(str::to_string).collect()
}

fn resp_to(req: &Request, code: u16, tag: &str) -> Response {
    Response::for_request(req, StatusCode::new(code).unwrap(), Some(tag))
}

fn resp_with(
    req: &Request,
    code: u16,
    tag: &str,
    hdrs: &[(&str, String)],
    body: Option<&str>,
) -> Response {
    let mut r = resp_to(req, code, tag);
    for (n, v) in hdrs {
        r.headers.push(n, v.clone());
    }
    if let Some(b) = body {
        r.headers.push("Content-Type", "application/sdp");
        r.body = b.to_string().into();
    }
    r
}

fn cseq_of(r: &Request) -> u32 {
    r.headers.cseq().unwrap().seq
}

fn md5_hex(s: &str) -> String {
    let mut h = Md5::new();
    h.update(s.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

// ================================================================ G01

#[test]
fn g01_dialog_from_invite_route_set_remote_target_and_cseq() {
    let inv = peer_invite(&Inv {
        record_routes: vec!["<sip:192.0.2.77:5070;lr>", "<sip:192.0.2.88;lr>"],
        ..Inv::default()
    });
    let contact = NameAddr::parse("<sip:fax@192.0.2.10:5060>").unwrap();
    let d = uas::dialog_from_invite(
        &inv,
        "ourtag",
        contact,
        PEER.parse().unwrap(),
        TransportKind::Udp,
    )
    .unwrap();
    assert_eq!(d.role, Role::Uas);
    let rs: Vec<String> = d.route_set.iter().map(|r| r.uri.to_string()).collect();
    assert_eq!(
        rs,
        vec!["sip:192.0.2.77:5070;lr", "sip:192.0.2.88;lr"],
        "original order"
    );
    assert_eq!(
        d.remote_target.to_string(),
        "sip:gw@10.0.0.1:5070",
        "Contact of the INVITE"
    );
    assert_eq!(d.remote_seq, Some(1));
    assert!(
        d.local_seq < (1 << 31),
        "local CSeq below 2^31: {}",
        d.local_seq
    );
    assert_eq!(d.id.call_id, "call-1@10.0.0.1");
    assert_eq!(d.id.local_tag, "ourtag");
    assert_eq!(d.id.remote_tag, FROM_TAG);
    assert_eq!(d.local.tag(), Some("ourtag"));
    assert_eq!(d.remote.tag(), Some(FROM_TAG));
    println!(
        "G01: route_set={rs:?} remote_target={} remote_seq=1 local_seq={}",
        d.remote_target, d.local_seq
    );

    // no Contact: fall back to the source address
    let nc = peer_invite(&Inv {
        contact: None,
        ..Inv::default()
    });
    let d = uas::dialog_from_invite(
        &nc,
        "t",
        NameAddr::parse("<sip:fax@192.0.2.10>").unwrap(),
        PEER.parse().unwrap(),
        TransportKind::Udp,
    )
    .unwrap();
    assert_eq!(d.remote_target.to_string(), "sip:10.0.0.1:5060");
}

#[test]
fn g01_uac_dialog_reverses_the_record_route_of_the_response() {
    let mut inv = peer_invite(&Inv::default());
    inv.headers.set("From", "<sip:fax@192.0.2.10>;tag=ours");
    inv.headers.set("To", "<sip:gw@10.0.0.1>");
    let mut resp = Response::for_request(&inv, StatusCode::OK, Some("theirs"));
    resp.headers.push("Record-Route", "<sip:p2;lr>");
    resp.headers.push("Record-Route", "<sip:p1;lr>");
    resp.headers.push("Contact", "<sip:gw@10.0.0.1:5070>");
    let d = Dialog::from_2xx(
        &inv,
        &resp,
        NameAddr::parse("<sip:fax@192.0.2.10>").unwrap(),
        PEER.parse().unwrap(),
        TransportKind::Udp,
    )
    .unwrap();
    let rs: Vec<String> = d.route_set.iter().map(|r| r.uri.to_string()).collect();
    assert_eq!(rs, vec!["sip:p1;lr", "sip:p2;lr"]);
    assert_eq!(d.id.local_tag, "ours");
    assert_eq!(d.id.remote_tag, "theirs");
    assert_eq!(d.role, Role::Uac);
    assert_eq!(d.local_seq, 1);
}

// ================================================================ G02 / G03

const RETX: [u64; 11] = [
    0, 500, 1500, 3500, 7500, 11500, 15500, 19500, 23500, 27500, 31500,
];

#[tokio::test(start_paused = true)]
async fn g02_2xx_is_retransmitted_until_ack_then_stops() {
    let mut w = W::new();
    let inv = Inv::default();
    let mut dlg = establish(&mut w, &inv, None).await;
    w.at(12_000).await;
    let t: Vec<u64> = w.ok_to_invite().iter().map(|(t, _)| *t).collect();
    assert_eq!(
        t,
        vec![0, 500, 1500, 3500, 7500, 11500],
        "200 sent at these ms"
    );
    // identical bytes every time
    let all = w.ok_to_invite();
    let first = all[0].1.to_bytes();
    assert!(all.iter().all(|(_, r)| r.to_bytes() == first));
    let tag = w.our_tag();
    w.feed_req(peer_in_dialog("ACK", "z9hG4bKack1", 1, &tag, None, &[]))
        .await;
    assert!(matches!(
        next_event(&mut dlg).await,
        Some(DialogEvent::Established)
    ));
    w.at(40_000).await;
    assert_eq!(w.ok_to_invite().len(), 6, "no retransmission after the ACK");
    assert!(next_event(&mut dlg).await.is_none(), "and no AckTimeout");
    println!("G02: 200 instants {t:?}, ACK at 12000, nothing afterwards");
}

#[tokio::test(start_paused = true)]
async fn g03_ack_timeout_is_reported_at_64_t1_and_dialog_survives() {
    let mut w = W::new();
    let mut dlg = establish(&mut w, &Inv::default(), None).await;
    w.at(31_999).await;
    assert!(next_event(&mut dlg).await.is_none(), "not yet at 31999");
    w.at(32_000).await;
    assert!(matches!(
        next_event(&mut dlg).await,
        Some(DialogEvent::AckTimeout)
    ));
    assert_eq!(w.now_ms(), 32_000);
    let t: Vec<u64> = w.ok_to_invite().iter().map(|(t, _)| *t).collect();
    assert_eq!(t, RETX.to_vec());
    w.at(60_000).await;
    assert_eq!(w.ok_to_invite().len(), RETX.len(), "retransmission stopped");
    // the application decides: here a BYE
    let ctl = dlg.control();
    let bye = tokio::spawn(async move { ctl.bye().await });
    settle().await;
    assert_eq!(w.requests("BYE").len(), 1);
    println!("G03: 200 instants {t:?}; AckTimeout at 32000");
    drop(bye);
}

// ================================================================ G04 / G05

async fn bye_wire(inv: Inv) -> (Request, u64, std::net::SocketAddr) {
    let mut w = W::new();
    let dlg = establish(&mut w, &inv, Some(10)).await;
    let ctl = dlg.control();
    let h = tokio::spawn(async move { ctl.bye().await });
    settle().await;
    let all = w.rec.all();
    let (t, req, dest) = all
        .iter()
        .find_map(|(t, o)| match o {
            Outgoing::Request { req, dest, .. } if req.method == Method::Bye => {
                Some((*t, req.clone(), *dest))
            }
            _ => None,
        })
        .expect("BYE on the wire");
    drop(h);
    (req, t, dest)
}

#[tokio::test(start_paused = true)]
async fn g04_bye_with_loose_routing() {
    let (bye, _, dest) = bye_wire(Inv {
        record_routes: vec!["<sip:192.0.2.77:5070;lr>", "<sip:192.0.2.88;lr>"],
        ..Inv::default()
    })
    .await;
    assert_eq!(
        bye.uri.to_string(),
        "sip:gw@10.0.0.1:5070",
        "R-URI = remote target"
    );
    assert_eq!(
        hdr_all(&bye, "Route"),
        vec!["<sip:192.0.2.77:5070;lr>", "<sip:192.0.2.88;lr>"]
    );
    assert_eq!(
        dest,
        "192.0.2.77:5070".parse().unwrap(),
        "sent to the first route"
    );
    let from = bye.headers.from().unwrap();
    let to = bye.headers.to().unwrap();
    assert_eq!(to.tag(), Some(FROM_TAG), "To carries the peer tag");
    assert!(
        from.tag().is_some_and(|t| t.len() == 8),
        "From carries our tag"
    );
    assert_eq!(bye.headers.call_id(), Some("call-1@10.0.0.1"));
    assert!(
        bye.headers
            .top_via()
            .unwrap()
            .branch()
            .unwrap()
            .starts_with("z9hG4bK")
    );
    println!(
        "G04: BYE {} Route={:?} dest={dest}",
        bye.uri,
        hdr_all(&bye, "Route")
    );
}

#[tokio::test(start_paused = true)]
async fn g04_cseq_increments_across_reinvite_and_bye() {
    let mut w = W::new();
    let dlg = establish(&mut w, &Inv::default(), Some(10)).await;
    let ctl = dlg.control();
    let c2 = ctl.clone();
    let re = tokio::spawn(async move { c2.reinvite(sdp(T38_OFFER)).await });
    settle().await;
    let (_, inv) = w.requests("INVITE").remove(0);
    let n = cseq_of(&inv);
    w.feed_resp(resp_with(
        &inv,
        200,
        FROM_TAG,
        &[("Contact", GW_CONTACT.into())],
        Some(T38_OFFER),
    ))
    .await;
    assert!(matches!(
        re.await.unwrap(),
        Ok(ReinviteOutcome::Accepted(_))
    ));
    let bye = tokio::spawn(async move { ctl.bye().await });
    settle().await;
    let (_, b) = w.requests("BYE").remove(0);
    assert_eq!(cseq_of(&b), n + 1, "CSeq + 1");
    drop(bye);
}

#[tokio::test(start_paused = true)]
async fn g05_bye_with_strict_routing() {
    let (bye, _, dest) = bye_wire(Inv {
        record_routes: vec!["<sip:192.0.2.77:5070>", "<sip:192.0.2.88;lr>"],
        ..Inv::default()
    })
    .await;
    assert_eq!(
        bye.uri.to_string(),
        "sip:192.0.2.77:5070",
        "R-URI = first route"
    );
    assert_eq!(
        hdr_all(&bye, "Route"),
        vec!["<sip:192.0.2.88;lr>", "<sip:gw@10.0.0.1:5070>"],
        "rest of the route set, remote target last"
    );
    assert_eq!(dest, "192.0.2.77:5070".parse().unwrap());
    println!("G05: BYE {} Route={:?}", bye.uri, hdr_all(&bye, "Route"));
}

// ================================================================ G06

#[tokio::test(start_paused = true)]
async fn g06_peer_bye_is_answered_ends_the_dialog_and_later_requests_get_481() {
    let mut w = W::new();
    let mut dlg = establish(&mut w, &Inv::default(), Some(10)).await;
    assert!(matches!(
        next_event(&mut dlg).await,
        Some(DialogEvent::Established)
    ));
    let tag = w.our_tag();
    w.at(1000).await;
    w.feed_req(peer_in_dialog("BYE", "z9hG4bKbye1", 2, &tag, None, &[]))
        .await;
    let ok = w.responses(200, "BYE");
    assert_eq!(ok.len(), 1);
    assert_eq!(ok[0].0, 1000);
    assert_eq!(ok[0].1.headers.to().unwrap().tag(), Some(tag.as_str()));
    assert!(matches!(next_event(&mut dlg).await, Some(DialogEvent::Bye)));
    assert!(matches!(
        next_event(&mut dlg).await,
        Some(DialogEvent::Terminated(TerminateReason::RemoteBye))
    ));
    assert!(next_event(&mut dlg).await.is_none());
    assert_eq!(w.ep.dialog_count(), 0);
    // afterwards: unknown dialog
    w.feed_req(peer_in_dialog("INFO", "z9hG4bKinfo1", 3, &tag, None, &[]))
        .await;
    assert_eq!(w.responses(481, "INFO").len(), 1);
    // also a BYE for a dialog that never existed, and one without To tag
    w.feed_req(peer_in_dialog(
        "BYE",
        "z9hG4bKbye2",
        9,
        "nosuchtag",
        None,
        &[],
    ))
    .await;
    assert_eq!(w.responses(481, "BYE").len(), 1);
    println!("G06: BYE answered at 1000 ms, then INFO -> 481");
}

#[tokio::test(start_paused = true)]
async fn g06_out_of_dialog_bye_and_options_without_dialog() {
    let w = W::new();
    let mut bye = peer_in_dialog("BYE", "z9hG4bKbye3", 4, "x", None, &[]);
    bye.headers.set("To", "<sip:fax@192.0.2.10>".to_string());
    w.feed_req(bye).await;
    assert_eq!(w.responses(481, "BYE").len(), 1, "no To tag -> 481");
    let mut reg = peer_in_dialog("REGISTER", "z9hG4bKreg1", 1, "x", None, &[]);
    reg.headers.set("To", "<sip:fax@192.0.2.10>".to_string());
    w.feed_req(reg).await;
    let r = w.responses(405, "REGISTER");
    assert_eq!(r.len(), 1);
    assert!(r[0].1.headers.get("Allow").unwrap().contains("INVITE"));
    let mut hop = peer_in_dialog("OPTIONS", "z9hG4bKmf0", 1, "x", None, &[]);
    hop.headers.set("Max-Forwards", "0");
    hop.headers.set("To", "<sip:fax@192.0.2.10>".to_string());
    w.feed_req(hop).await;
    assert_eq!(w.responses(483, "OPTIONS").len(), 1);
}

// ================================================================ G07 / G08

#[tokio::test(start_paused = true)]
async fn g07_peer_t38_reinvite_is_handed_up_answered_and_acked() {
    let mut w = W::new();
    let mut dlg = establish(&mut w, &Inv::default(), Some(10)).await;
    assert!(matches!(
        next_event(&mut dlg).await,
        Some(DialogEvent::Established)
    ));
    let tag = w.our_tag();
    w.at(2000).await;
    w.feed_req(peer_in_dialog(
        "INVITE",
        "z9hG4bKre1",
        2,
        &tag,
        Some(T38_OFFER),
        &[("Contact", GW_CONTACT)],
    ))
    .await;
    let Some(DialogEvent::ReInvite { offer, responder }) = next_event(&mut dlg).await else {
        panic!("expected ReInvite");
    };
    let offer = offer.expect("offer");
    assert!(
        offer.media[0].is_t38(),
        "T.38 offer handed to the application"
    );
    let answer = answer_of(&offer);
    responder.accept(answer.clone()).await.unwrap();
    settle().await;
    let oks = w.responses(200, "INVITE");
    let re_ok: Vec<_> = oks.iter().filter(|(t, _)| *t == 2000).collect();
    assert_eq!(re_ok.len(), 1);
    let r = &re_ok[0].1;
    assert_eq!(r.headers.get("Content-Type"), Some("application/sdp"));
    assert_eq!(r.headers.cseq().unwrap().seq, 2);
    assert!(r.headers.get("Contact").unwrap().contains("192.0.2.10"));
    assert_eq!(
        parse_sdp(&r.body).unwrap(),
        answer,
        "the answer is on the wire"
    );
    // the 200 is retransmitted until the ACK
    w.at(2600).await;
    assert_eq!(
        w.responses(200, "INVITE")
            .iter()
            .filter(|(t, _)| *t == 2500)
            .count(),
        1
    );
    w.feed_req(peer_in_dialog("ACK", "z9hG4bKack2", 2, &tag, None, &[]))
        .await;
    let n = w.responses(200, "INVITE").len();
    w.at(20_000).await;
    assert_eq!(
        w.responses(200, "INVITE").len(),
        n,
        "ACK stopped the retransmission"
    );
    assert!(next_event(&mut dlg).await.is_none(), "no AckTimeout");
    println!("G07: T.38 re-INVITE answered at 2000 ms, 200 resent at 2500, ACK stopped it");
}

#[tokio::test(start_paused = true)]
async fn g07_unanswered_responder_is_rejected_with_500_on_drop() {
    let mut w = W::new();
    let mut dlg = establish(&mut w, &Inv::default(), Some(10)).await;
    let tag = w.our_tag();
    next_event(&mut dlg).await; // Established
    w.feed_req(peer_in_dialog(
        "INVITE",
        "z9hG4bKre9",
        2,
        &tag,
        Some(T38_OFFER),
        &[],
    ))
    .await;
    let ev = next_event(&mut dlg).await;
    drop(ev);
    settle().await;
    assert_eq!(w.responses(500, "INVITE").len(), 1);
    // a second offer can be answered afterwards
    w.feed_req(peer_in_dialog(
        "INVITE",
        "z9hG4bKre10",
        3,
        &tag,
        Some(T38_OFFER),
        &[],
    ))
    .await;
    let Some(DialogEvent::ReInvite { responder, .. }) = next_event(&mut dlg).await else {
        panic!()
    };
    responder
        .reject(StatusCode::NOT_ACCEPTABLE_HERE)
        .await
        .unwrap();
    settle().await;
    assert_eq!(w.responses(488, "INVITE").len(), 1);
}

#[tokio::test(start_paused = true)]
async fn g08_glare_peer_reinvite_during_our_reinvite_gets_491() {
    let mut w = W::new();
    let dlg = establish(&mut w, &Inv::default(), Some(10)).await;
    let tag = w.our_tag();
    w.at(1000).await;
    let ctl = dlg.control();
    let ours = tokio::spawn(async move { ctl.reinvite(sdp(T38_OFFER)).await });
    settle().await;
    let (t, our_inv) = w.requests("INVITE").remove(0);
    assert_eq!(t, 1000);
    w.at(1200).await;
    w.feed_req(peer_in_dialog(
        "INVITE",
        "z9hG4bKglare",
        2,
        &tag,
        Some(T38_OFFER),
        &[],
    ))
    .await;
    let r491 = w.responses(491, "INVITE");
    assert_eq!(r491.len(), 1, "peer re-INVITE answered with 491");
    assert_eq!(r491[0].0, 1200);
    assert_eq!(
        r491[0].1.headers.top_via().unwrap().branch(),
        Some("z9hG4bKglare")
    );
    // our own re-INVITE is unaffected and completes
    w.feed_resp(resp_with(
        &our_inv,
        200,
        FROM_TAG,
        &[("Contact", GW_CONTACT.into())],
        Some(T38_OFFER),
    ))
    .await;
    assert!(matches!(
        ours.await.unwrap(),
        Ok(ReinviteOutcome::Accepted(_))
    ));
    println!("G08: 491 sent at 1200 ms while our re-INVITE (sent 1000) was pending");
}

// ================================================================ G09

#[tokio::test(start_paused = true)]
async fn g09_reinvite_491_is_retried_within_0_to_2s_and_gives_up_at_the_limit() {
    let mut w = W::with(|c| c.max_491_retries = 2);
    let dlg = establish(&mut w, &Inv::default(), Some(10)).await;
    let ctl = dlg.control();
    let ours = tokio::spawn(async move { ctl.reinvite(sdp(T38_OFFER)).await });
    settle().await;
    let mut seqs = vec![];
    let mut delays = vec![];
    for attempt in 0..3 {
        let sent = w.requests("INVITE");
        assert_eq!(sent.len(), attempt + 1, "attempt {attempt}");
        let (t_sent, inv) = sent.last().unwrap().clone();
        seqs.push(cseq_of(&inv));
        if attempt > 0 {
            delays.push(t_sent - last_491_at(&w));
        }
        w.feed_resp(resp_to(&inv, 491, FROM_TAG)).await;
        // the retry (if any) must come within 2 s
        let before = w.now_ms();
        for step in 1..=2000u64 {
            w.at(before + step).await;
            if w.requests("INVITE").len() > attempt + 1 {
                break;
            }
        }
    }
    let out = ours.await.unwrap().unwrap();
    assert_eq!(out, ReinviteOutcome::Rejected(StatusCode::REQUEST_PENDING));
    assert_eq!(w.requests("INVITE").len(), 3, "original + 2 retries");
    assert!(
        seqs.windows(2).all(|p| p[1] == p[0] + 1),
        "new CSeq per attempt: {seqs:?}"
    );
    assert!(
        delays.iter().all(|d| *d <= 2000),
        "UAS role: retry within 0-2 s: {delays:?}"
    );
    println!("G09: CSeqs {seqs:?}, retry delays {delays:?} ms, final Rejected(491)");
}

fn last_491_at(w: &W) -> u64 {
    // the INVITE answers are injected at the instant the previous attempt was sent + 0;
    // the retry delay is measured from the time the 491 was fed, which is the send instant
    // of the previous INVITE in this test (responses are fed immediately)
    let inv = w.requests("INVITE");
    inv[inv.len() - 2].0
}

#[tokio::test(start_paused = true)]
async fn g09_call_id_owner_waits_2_1_to_4_s() {
    let w = W::new();
    let mut o = InviteOptions::new(
        PEER.parse().unwrap(),
        SipUri::parse("sip:gw@10.0.0.1").unwrap(),
        SipUri::parse("sip:fax@192.0.2.10").unwrap(),
        SipUri::parse("sip:gw@10.0.0.1").unwrap(),
        sdp(AUDIO_OFFER),
    );
    o.transport = TransportKind::Udp;
    let ep = w.ep.clone();
    let inv_task = tokio::spawn(async move { ep.invite(o).await });
    settle().await;
    let (_, inv) = w.requests("INVITE").remove(0);
    w.feed_resp(resp_with(
        &inv,
        200,
        "gwtag",
        &[("Contact", GW_CONTACT.into())],
        Some(AUDIO_OFFER),
    ))
    .await;
    let dlg = inv_task.await.unwrap().unwrap();
    assert_eq!(w.requests("ACK").len(), 1, "automatic ACK");
    let ctl = dlg.control();
    let ours = tokio::spawn(async move { ctl.reinvite(sdp(T38_OFFER)).await });
    settle().await;
    let (t1, re) = w.requests("INVITE").pop().unwrap();
    assert_eq!(w.requests("INVITE").len(), 2);
    w.feed_resp(resp_to(&re, 491, "gwtag")).await;
    let base = w.now_ms();
    let mut retry_at = None;
    for step in 1..=4100u64 {
        w.at(base + step).await;
        if w.requests("INVITE").len() == 3 {
            retry_at = Some(step);
            break;
        }
    }
    let d = retry_at.expect("retry within 4.1 s");
    assert!(
        (2100..=4000).contains(&d),
        "owner delay {d} ms not in 2100..=4000"
    );
    let (_, re2) = w.requests("INVITE").pop().unwrap();
    w.feed_resp(resp_with(
        &re2,
        200,
        "gwtag",
        &[("Contact", GW_CONTACT.into())],
        Some(T38_OFFER),
    ))
    .await;
    assert!(matches!(
        ours.await.unwrap(),
        Ok(ReinviteOutcome::Accepted(_))
    ));
    let _ = t1;
    println!("G09 (owner): retry after {d} ms");
}

// ================================================================ G10 / G11 / G15

#[tokio::test(start_paused = true)]
async fn g10_cseq_lower_than_remote_seq_gets_500() {
    let mut w = W::new();
    let _dlg = establish(&mut w, &Inv::default(), Some(10)).await;
    let tag = w.our_tag();
    w.feed_req(peer_in_dialog("INFO", "z9hG4bKi1", 7, &tag, None, &[]))
        .await;
    assert_eq!(w.responses(200, "INFO").len(), 1);
    w.feed_req(peer_in_dialog("INFO", "z9hG4bKi2", 4, &tag, None, &[]))
        .await;
    let r = w.responses(500, "INFO");
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].1.headers.cseq().unwrap().seq, 4);
    // a CSeq equal to the remote seq (new transaction) is accepted, higher too
    w.feed_req(peer_in_dialog("INFO", "z9hG4bKi3", 8, &tag, None, &[]))
        .await;
    assert_eq!(w.responses(200, "INFO").len(), 2);
    println!("G10: INFO CSeq 7 -> 200, CSeq 4 -> 500, CSeq 8 -> 200");
}

#[tokio::test(start_paused = true)]
async fn g11_options_in_and_out_of_dialog_carry_allow_accept_supported() {
    let mut w = W::new();
    let mut o = peer_in_dialog("OPTIONS", "z9hG4bKo1", 1, "x", None, &[]);
    o.headers.set("To", "<sip:fax@192.0.2.10>".to_string());
    w.feed_req(o).await;
    let _dlg = establish(&mut w, &Inv::default(), Some(10)).await;
    let tag = w.our_tag();
    w.feed_req(peer_in_dialog("OPTIONS", "z9hG4bKo2", 2, &tag, None, &[]))
        .await;
    let r = w.responses(200, "OPTIONS");
    assert_eq!(r.len(), 2);
    for (_, resp) in &r {
        let allow = resp.headers.get("Allow").unwrap();
        for m in [
            "INVITE", "ACK", "BYE", "CANCEL", "OPTIONS", "INFO", "UPDATE",
        ] {
            assert!(allow.contains(m), "Allow lacks {m}: {allow}");
        }
        assert_eq!(resp.headers.get("Accept"), Some("application/sdp"));
        assert_eq!(resp.headers.get("Supported"), Some("timer"));
    }
    println!("G11: Allow={:?}", r[0].1.headers.get("Allow"));
}

#[tokio::test(start_paused = true)]
async fn g15_update_with_sdp_gets_488_without_sdp_gets_200() {
    let mut w = W::new();
    let _dlg = establish(&mut w, &Inv::default(), Some(10)).await;
    let tag = w.our_tag();
    w.feed_req(peer_in_dialog(
        "UPDATE",
        "z9hG4bKu1",
        2,
        &tag,
        Some(T38_OFFER),
        &[],
    ))
    .await;
    assert_eq!(w.responses(488, "UPDATE").len(), 1);
    w.feed_req(peer_in_dialog("UPDATE", "z9hG4bKu2", 3, &tag, None, &[]))
        .await;
    assert_eq!(w.responses(200, "UPDATE").len(), 1);
}

// ================================================================ G12 / G13 / G14

#[tokio::test(start_paused = true)]
async fn g12_refresher_uac_no_refresh_means_bye_at_interval_minus_margin() {
    let mut w = W::new();
    let inv = Inv {
        extra: vec![
            ("Session-Expires", "90;refresher=uac"),
            ("Supported", "timer"),
        ],
        ..Inv::default()
    };
    let mut dlg = establish(&mut w, &inv, Some(10)).await;
    let ok = &w.ok_to_invite()[0].1;
    assert_eq!(ok.headers.get("Session-Expires"), Some("90;refresher=uac"));
    assert_eq!(ok.headers.get("Require"), Some("timer"));
    next_event(&mut dlg).await; // Established
    w.at(59_999).await;
    assert!(w.requests("BYE").is_empty());
    w.at(60_000).await;
    let bye = w.requests("BYE");
    assert_eq!(bye.len(), 1);
    assert_eq!(bye[0].0, 60_000, "interval - min(32, interval/3) = 90 - 30");
    assert!(matches!(
        next_event(&mut dlg).await,
        Some(DialogEvent::SessionExpired)
    ));
    w.feed_resp(resp_to(&bye[0].1, 200, FROM_TAG)).await;
    assert!(matches!(
        next_event(&mut dlg).await,
        Some(DialogEvent::Terminated(TerminateReason::SessionExpired))
    ));
    println!("G12: BYE at 60000 ms");
}

#[tokio::test(start_paused = true)]
async fn g12_peer_refresh_resets_the_timer() {
    let mut w = W::new();
    let inv = Inv {
        extra: vec![
            ("Session-Expires", "90;refresher=uac"),
            ("Supported", "timer"),
        ],
        ..Inv::default()
    };
    let mut dlg = establish(&mut w, &inv, Some(10)).await;
    let tag = w.our_tag();
    w.at(40_000).await;
    w.feed_req(peer_in_dialog(
        "UPDATE",
        "z9hG4bKu7",
        2,
        &tag,
        None,
        &[("Session-Expires", "90;refresher=uac")],
    ))
    .await;
    let r = w.responses(200, "UPDATE");
    assert_eq!(
        r[0].1.headers.get("Session-Expires"),
        Some("90;refresher=uac")
    );
    w.at(99_999).await;
    assert!(w.requests("BYE").is_empty(), "40000 + 60000 not reached");
    w.at(100_000).await;
    assert_eq!(w.requests("BYE")[0].0, 100_000);
    drop(next_event(&mut dlg).await);
}

#[tokio::test(start_paused = true)]
async fn g13_refresher_uas_sends_reinvite_at_half_time_with_o_version_plus_one() {
    let mut w = W::new();
    let inv = Inv {
        extra: vec![
            ("Session-Expires", "90;refresher=uas"),
            ("Supported", "timer"),
        ],
        ..Inv::default()
    };
    let _dlg = establish(&mut w, &inv, Some(10)).await;
    assert_eq!(
        w.ok_to_invite()[0].1.headers.get("Session-Expires"),
        Some("90;refresher=uas")
    );
    let answer = parse_sdp(&w.ok_to_invite()[0].1.body).unwrap();
    w.at(44_999).await;
    assert!(w.requests("INVITE").is_empty());
    w.at(45_000).await;
    let invs = w.requests("INVITE");
    assert_eq!(invs.len(), 1);
    assert_eq!(invs[0].0, 45_000);
    let refresh = &invs[0].1;
    assert_eq!(
        refresh.headers.get("Session-Expires"),
        Some("90;refresher=uac")
    );
    let sdp1 = parse_sdp(&refresh.body).unwrap();
    assert_eq!(sdp1.origin.sess_version, answer.origin.sess_version + 1);
    let mut a = answer.clone();
    a.origin.sess_version += 1;
    assert_eq!(sdp1, a, "same SDP apart from the o= version");
    // answer it: ACK goes out, and the next refresh comes 45 s after the 200
    w.feed_resp(resp_with(
        refresh,
        200,
        FROM_TAG,
        &[
            ("Contact", GW_CONTACT.into()),
            ("Session-Expires", "90;refresher=uac".into()),
        ],
        Some(AUDIO_OFFER),
    ))
    .await;
    assert_eq!(w.requests("ACK").len(), 1);
    w.at(90_000).await;
    let invs = w.requests("INVITE");
    assert_eq!(invs.len(), 2);
    assert_eq!(invs[1].0, 90_000);
    assert_eq!(cseq_of(&invs[1].1), cseq_of(&invs[0].1) + 1);
    assert_eq!(
        parse_sdp(&invs[1].1.body).unwrap().origin.sess_version,
        answer.origin.sess_version + 2
    );
    println!("G13: re-INVITE at 45000 and 90000 ms, o= version +1 / +2");
}

#[tokio::test(start_paused = true)]
async fn g14_session_interval_below_our_minimum_gets_422_with_min_se() {
    let mut w = W::new();
    w.feed_req(peer_invite(&Inv {
        extra: vec![("Session-Expires", "30"), ("Supported", "timer")],
        ..Inv::default()
    }))
    .await;
    let r = w.responses(422, "INVITE");
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].1.headers.get("Min-SE"), Some("90"));
    assert!(
        w.calls.try_recv().is_err(),
        "the application never sees the call"
    );
    assert_eq!(w.ep.dialog_count(), 0);
    println!("G14: 422 Min-SE: 90");
}

#[tokio::test(start_paused = true)]
async fn g14_caller_without_timer_support_makes_us_the_refresher() {
    let mut w = W::new();
    let inv = Inv {
        extra: vec![("Session-Expires", "120")],
        ..Inv::default()
    };
    let _dlg = establish(&mut w, &inv, Some(10)).await;
    assert_eq!(
        w.ok_to_invite()[0].1.headers.get("Session-Expires"),
        Some("120;refresher=uas")
    );
    w.at(60_000).await;
    assert_eq!(w.requests("INVITE").len(), 1);
    let _ = Refresher::Uas;
}

// ================================================================ G16 / G17

#[tokio::test(start_paused = true)]
async fn g16_retransmitted_2xx_to_our_reinvite_resends_the_identical_ack() {
    let mut w = W::new();
    let dlg = establish(&mut w, &Inv::default(), Some(10)).await;
    let ctl = dlg.control();
    let re = tokio::spawn(async move { ctl.reinvite(sdp(T38_OFFER)).await });
    settle().await;
    let (_, inv) = w.requests("INVITE").remove(0);
    let t_ok = w.now_ms();
    let ok = resp_with(
        &inv,
        200,
        FROM_TAG,
        &[("Contact", GW_CONTACT.into())],
        Some(T38_OFFER),
    );
    w.feed_resp(ok.clone()).await;
    assert!(matches!(
        re.await.unwrap(),
        Ok(ReinviteOutcome::Accepted(_))
    ));
    let acks = w.requests("ACK");
    assert_eq!(acks.len(), 1);
    assert_eq!(acks[0].0, t_ok, "ACK right away");
    let first = acks[0].1.to_bytes();
    // the first ACK is lost: the gateway retransmits its 200 at +500 and +1500
    for (i, dt) in [500u64, 1500].into_iter().enumerate() {
        w.at(t_ok + dt).await;
        w.feed_resp(ok.clone()).await;
        let acks = w.requests("ACK");
        assert_eq!(
            acks.len(),
            i + 2,
            "ACK resent for retransmitted 200 at +{dt}"
        );
        assert_eq!(acks[i + 1].0, t_ok + dt);
        assert_eq!(acks[i + 1].1.to_bytes(), first, "byte-identical ACK");
    }
    // after Timer M (32 s) the transaction is gone; the stray 200 still gets the ACK
    w.at(t_ok + 40_000).await;
    w.feed_resp(ok.clone()).await;
    let acks = w.requests("ACK");
    assert_eq!(acks.len(), 4);
    assert_eq!(acks[3].1.to_bytes(), first);
    // ACK for a 2xx: own branch (not the INVITE's), CSeq number of the INVITE
    let a = &acks[0].1;
    assert_ne!(
        a.headers.top_via().unwrap().branch(),
        inv.headers.top_via().unwrap().branch()
    );
    assert_eq!(cseq_of(a), cseq_of(&inv));
    assert_eq!(a.headers.to().unwrap().tag(), Some(FROM_TAG));
    println!("G16: ACK at {t_ok}, re-ACK at +500, +1500, +40000 ms, bytes identical");
}

#[tokio::test(start_paused = true)]
async fn g17_reinvite_with_only_a_100_is_cancelled_at_32s_and_reports_timeout() {
    let mut w = W::new();
    let dlg = establish(&mut w, &Inv::default(), Some(10)).await;
    let ctl = dlg.control();
    let c2 = ctl.clone();
    let re = tokio::spawn(async move { c2.reinvite(sdp(T38_OFFER)).await });
    settle().await;
    let (t0, inv) = w.requests("INVITE").remove(0);
    w.at(t0 + 100).await;
    w.feed_resp(resp_to(&inv, 100, FROM_TAG)).await;
    w.at(t0 + 31_999).await;
    assert!(w.requests("CANCEL").is_empty());
    w.at(t0 + 32_000).await;
    let c = w.requests("CANCEL");
    assert_eq!(c.len(), 1);
    assert_eq!(c[0].0, t0 + 32_000, "CANCEL at exactly 32 s");
    assert_eq!(
        c[0].1.headers.top_via().unwrap().branch(),
        inv.headers.top_via().unwrap().branch(),
        "CANCEL uses the INVITE's branch"
    );
    assert_eq!(c[0].1.headers.cseq().unwrap().method, Method::Cancel);
    assert_eq!(cseq_of(&c[0].1), cseq_of(&inv));
    assert_eq!(re.await.unwrap().unwrap(), ReinviteOutcome::Timeout);
    // the peer answers CANCEL and the INVITE; we ACK the 487 and the dialog is usable again
    w.feed_resp(resp_to(&c[0].1, 200, FROM_TAG)).await;
    w.feed_resp(resp_to(&inv, 487, FROM_TAG)).await;
    assert_eq!(w.requests("ACK").len(), 1, "ACK for the 487");
    let c3 = ctl.clone();
    let again = tokio::spawn(async move { c3.reinvite(sdp(T38_OFFER)).await });
    settle().await;
    let invs = w.requests("INVITE");
    assert_eq!(
        invs.len(),
        2,
        "queued re-INVITE starts after the cancelled one ended"
    );
    w.feed_resp(resp_with(
        &invs[1].1,
        200,
        FROM_TAG,
        &[("Contact", GW_CONTACT.into())],
        Some(T38_OFFER),
    ))
    .await;
    assert!(matches!(
        again.await.unwrap(),
        Ok(ReinviteOutcome::Accepted(_))
    ));
    println!(
        "G17: CANCEL at {} ms after the re-INVITE, outcome Timeout",
        32_000
    );
}

#[tokio::test(start_paused = true)]
async fn g17_without_any_response_timer_b_fires_and_no_cancel_is_sent() {
    let mut w = W::new();
    let dlg = establish(&mut w, &Inv::default(), Some(10)).await;
    let ctl = dlg.control();
    let re = tokio::spawn(async move { ctl.reinvite(sdp(T38_OFFER)).await });
    settle().await;
    let t0 = w.requests("INVITE")[0].0;
    w.at(t0 + 32_000).await;
    assert_eq!(re.await.unwrap().unwrap(), ReinviteOutcome::Timeout);
    assert!(
        w.requests("CANCEL").is_empty(),
        "RFC 3261 9.1: no CANCEL before a provisional"
    );
}

#[tokio::test(start_paused = true)]
async fn g17_timeout_is_configurable() {
    let mut w = W::with(|c| c.reinvite_timeout = Duration::from_secs(5));
    let dlg = establish(&mut w, &Inv::default(), Some(10)).await;
    let ctl = dlg.control();
    let re = tokio::spawn(async move { ctl.reinvite(sdp(T38_OFFER)).await });
    settle().await;
    let (t0, inv) = w.requests("INVITE").remove(0);
    w.feed_resp(resp_to(&inv, 180, FROM_TAG)).await;
    w.at(t0 + 5000).await;
    assert_eq!(w.requests("CANCEL")[0].0, t0 + 5000);
    assert_eq!(re.await.unwrap().unwrap(), ReinviteOutcome::Timeout);
}

// ================================================================ C05

#[tokio::test(start_paused = true)]
async fn c05_bye_407_is_retried_once_with_a_correct_digest_then_reported() {
    let mut w = W::with(|c| {
        c.credentials = Some(Credentials {
            username: "faxuser".into(),
            password: "s3cret!".into(),
        })
    });
    let mut dlg = establish(&mut w, &Inv::default(), Some(10)).await;
    let ctl = dlg.control();
    let bye = tokio::spawn(async move { ctl.bye().await });
    settle().await;
    let (_, b1) = w.requests("BYE").remove(0);
    let nonce = "dcd98b7102dd2f0e8b11d0f600bfb0c093";
    let chal = format!("Digest realm=\"fax.example\", nonce=\"{nonce}\", algorithm=MD5");
    w.feed_resp(resp_with(
        &b1,
        407,
        FROM_TAG,
        &[("Proxy-Authenticate", chal.clone())],
        None,
    ))
    .await;
    let byes = w.requests("BYE");
    assert_eq!(byes.len(), 2, "one retry");
    let b2 = &byes[1].1;
    assert_eq!(cseq_of(b2), cseq_of(&b1) + 1);
    let auth = b2.headers.get("Proxy-Authorization").expect("credentials");
    // independent digest (RFC 2069 style, no qop)
    let ha1 = md5_hex("faxuser:fax.example:s3cret!");
    let ha2 = md5_hex(&format!("BYE:{}", b2.uri));
    let want = md5_hex(&format!("{ha1}:{nonce}:{ha2}"));
    assert!(
        auth.contains(&format!("response=\"{want}\"")),
        "{auth} vs {want}"
    );
    assert!(auth.contains("username=\"faxuser\""));
    // second 407: give up and report
    w.feed_resp(resp_with(
        b2,
        407,
        FROM_TAG,
        &[("Proxy-Authenticate", chal)],
        None,
    ))
    .await;
    assert_eq!(w.requests("BYE").len(), 2, "no third attempt");
    assert_eq!(bye.await.unwrap().unwrap(), StatusCode::new(407).unwrap());
    assert!(matches!(
        next_event(&mut dlg).await,
        Some(DialogEvent::Established)
    ));
    assert!(matches!(
        next_event(&mut dlg).await,
        Some(DialogEvent::Terminated(TerminateReason::LocalBye))
    ));
    println!("C05 BYE: 407 -> retry with Proxy-Authorization -> 407 -> reported");
}

#[tokio::test(start_paused = true)]
async fn c05_reinvite_401_is_retried_with_authorization_and_second_401_is_rejected() {
    let mut w = W::with(|c| {
        c.credentials = Some(Credentials {
            username: "u".into(),
            password: "p".into(),
        })
    });
    let dlg = establish(&mut w, &Inv::default(), Some(10)).await;
    let chal = "Digest realm=\"r\", nonce=\"n1\"".to_string();
    // (a) 401 then 200
    let ctl = dlg.control();
    let t = tokio::spawn(async move { ctl.reinvite(sdp(T38_OFFER)).await });
    settle().await;
    let (_, r1) = w.requests("INVITE").remove(0);
    w.feed_resp(resp_with(
        &r1,
        401,
        FROM_TAG,
        &[("WWW-Authenticate", chal.clone())],
        None,
    ))
    .await;
    let invs = w.requests("INVITE");
    assert_eq!(invs.len(), 2);
    assert!(invs[1].1.headers.get("Authorization").is_some());
    assert_eq!(cseq_of(&invs[1].1), cseq_of(&r1) + 1);
    w.feed_resp(resp_with(
        &invs[1].1,
        200,
        FROM_TAG,
        &[("Contact", GW_CONTACT.into())],
        Some(T38_OFFER),
    ))
    .await;
    assert!(matches!(t.await.unwrap(), Ok(ReinviteOutcome::Accepted(_))));
    // (b) 401 twice
    let ctl = dlg.control();
    let t = tokio::spawn(async move { ctl.reinvite(sdp(AUDIO_OFFER)).await });
    settle().await;
    let (_, r3) = w.requests("INVITE").pop().unwrap();
    w.feed_resp(resp_with(
        &r3,
        401,
        FROM_TAG,
        &[("WWW-Authenticate", chal.clone())],
        None,
    ))
    .await;
    let (_, r4) = w.requests("INVITE").pop().unwrap();
    w.feed_resp(resp_with(
        &r4,
        401,
        FROM_TAG,
        &[("WWW-Authenticate", chal)],
        None,
    ))
    .await;
    assert_eq!(
        t.await.unwrap().unwrap(),
        ReinviteOutcome::Rejected(StatusCode::UNAUTHORIZED)
    );
    assert_eq!(
        w.requests("INVITE").len(),
        4,
        "exactly one retry per re-INVITE"
    );
}

#[tokio::test(start_paused = true)]
async fn c05_without_credentials_a_challenge_is_reported_immediately() {
    let mut w = W::new();
    let dlg = establish(&mut w, &Inv::default(), Some(10)).await;
    let ctl = dlg.control();
    let t = tokio::spawn(async move { ctl.reinvite(sdp(T38_OFFER)).await });
    settle().await;
    let (_, r1) = w.requests("INVITE").remove(0);
    w.feed_resp(resp_with(
        &r1,
        407,
        FROM_TAG,
        &[(
            "Proxy-Authenticate",
            "Digest realm=\"r\", nonce=\"n\"".into(),
        )],
        None,
    ))
    .await;
    assert_eq!(
        t.await.unwrap().unwrap(),
        ReinviteOutcome::Rejected(StatusCode::new(407).unwrap())
    );
    assert_eq!(w.requests("INVITE").len(), 1);
}

// ================================================================ C10

#[tokio::test(start_paused = true)]
async fn c10_unknown_require_gets_420_with_unsupported_unless_the_flag_is_set() {
    let w = W::new();
    w.feed_req(peer_invite(&Inv {
        extra: vec![("Require", "100rel, timer, FooBar")],
        ..Inv::default()
    }))
    .await;
    let r = w.responses(420, "INVITE");
    assert_eq!(r.len(), 1);
    assert_eq!(
        r[0].1.headers.get("Unsupported"),
        Some("100rel, foobar"),
        "only the unknown tags"
    );
    assert_eq!(w.ep.dialog_count(), 0);
    println!(
        "C10: 420 Unsupported: {:?}",
        r[0].1.headers.get("Unsupported")
    );

    // flag: accept anyway
    let mut w2 = W::with(|c| c.accept_unknown_require = true);
    w2.feed_req(peer_invite(&Inv {
        extra: vec![("Require", "100rel")],
        ..Inv::default()
    }))
    .await;
    let call = w2.next_call().await;
    assert_eq!(call.request().headers.get("Require"), Some("100rel"));
    assert!(w2.responses(420, "INVITE").is_empty());

    // understood tag and unknown Supported values are fine by default
    let mut w3 = W::new();
    w3.feed_req(peer_invite(&Inv {
        extra: vec![("Require", "timer"), ("Supported", "weird-ext, 100rel")],
        ..Inv::default()
    }))
    .await;
    let _ = w3.next_call().await;
}

#[tokio::test(start_paused = true)]
async fn c10_unknown_require_applies_to_options_too() {
    let w = W::new();
    let mut o = peer_in_dialog(
        "OPTIONS",
        "z9hG4bKo9",
        1,
        "x",
        None,
        &[("Require", "sec-agree")],
    );
    o.headers.set("To", "<sip:fax@192.0.2.10>".to_string());
    w.feed_req(o).await;
    assert_eq!(w.responses(420, "OPTIONS").len(), 1);
}

// ================================================================ misc: call handling

#[tokio::test(start_paused = true)]
async fn g02_incoming_call_ringing_reject_and_cancel_paths() {
    let mut w = W::new();
    w.feed_req(peer_invite(&Inv::default())).await;
    let call = w.next_call().await;
    call.ringing().await.unwrap();
    settle().await;
    let r180 = w.responses(180, "INVITE");
    assert_eq!(r180.len(), 1);
    assert!(r180[0].1.headers.to().unwrap().tag().is_some());
    call.reject(StatusCode::BUSY_HERE).await.unwrap();
    settle().await;
    let r = w.responses(486, "INVITE");
    assert_eq!(r.len(), 1);
    assert_eq!(
        r[0].1.headers.to().unwrap().tag(),
        r180[0].1.headers.to().unwrap().tag(),
        "same To tag for 180 and the final response"
    );
    // CANCEL before the decision: 487, accept refused
    let inv2 = Inv {
        branch: "z9hG4bKinv2",
        call_id: "call-2@10.0.0.1",
        ..Inv::default()
    };
    w.feed_req(peer_invite(&inv2)).await;
    let call2 = w.next_call().await;
    let mut cancel = peer_invite(&inv2);
    cancel.method = Method::Cancel;
    cancel.headers.set("CSeq", "1 CANCEL".to_string());
    cancel.headers.remove("Content-Type");
    cancel.body = Default::default();
    w.feed_req(cancel).await;
    assert!(call2.is_cancelled());
    assert_eq!(w.responses(487, "INVITE").len(), 1);
    assert_eq!(w.responses(200, "CANCEL").len(), 1);
    assert!(matches!(
        call2.accept(answer_of(&sdp(AUDIO_OFFER))).await,
        Err(sip_core::endpoint::CallError::Cancelled)
    ));
}

#[tokio::test(start_paused = true)]
async fn dialog_errors_after_termination() {
    let mut w = W::new();
    let dlg = establish(&mut w, &Inv::default(), Some(10)).await;
    let tag = w.our_tag();
    w.feed_req(peer_in_dialog("BYE", "z9hG4bKb", 2, &tag, None, &[]))
        .await;
    settle().await;
    let ctl: DialogControl = dlg.control();
    assert_eq!(ctl.bye().await, Err(DialogError::Terminated));
    assert_eq!(
        ctl.reinvite(sdp(T38_OFFER)).await,
        Err(DialogError::Terminated)
    );
    let _ = Arc::new(dlg);
}

// ================================================================ M1d leftovers: Endpoint::invite timeout

fn invite_opts_for(w: &W) -> InviteOptions {
    let _ = w;
    InviteOptions::new(
        PEER.parse().unwrap(),
        SipUri::parse("sip:fax@10.0.0.1").unwrap(),
        SipUri::parse("sip:us@192.0.2.10").unwrap(),
        SipUri::parse("sip:fax@10.0.0.1").unwrap(),
        sdp(AUDIO_OFFER),
    )
}

/// `Endpoint::invite` with a provisional answer but no final one: Timer B no longer runs, so
/// the 180 s application timeout (`InviteOptions::timeout`, default 180 s) ends the attempt
/// with a CANCEL exactly then.
#[tokio::test(start_paused = true)]
async fn e08_invite_gives_up_after_180s_with_a_cancel() {
    let w = W::new();
    let opts = invite_opts_for(&w);
    assert_eq!(opts.timeout, Duration::from_secs(180), "default");
    let ep = w.ep.clone();
    let job = tokio::spawn(async move { ep.invite(opts).await });
    settle().await;
    let (t0, inv) = w.requests("INVITE").remove(0);
    assert_eq!(t0, 0);
    w.at(1_000).await;
    w.feed_resp(resp_to(&inv, 180, "theirs")).await;
    w.at(179_999).await;
    assert!(w.requests("CANCEL").is_empty(), "no CANCEL before 180 s");
    assert!(!job.is_finished());
    w.at(180_000).await;
    let cancels = w.requests("CANCEL");
    assert_eq!(cancels.len(), 1);
    let (t, c) = &cancels[0];
    assert_eq!(*t, 180_000);
    assert_eq!(
        c.headers.top_via().unwrap().branch(),
        inv.headers.top_via().unwrap().branch(),
        "CANCEL uses the INVITE's branch"
    );
    assert_eq!(c.headers.cseq().unwrap().method, Method::Cancel);
    assert_eq!(
        c.headers.cseq().unwrap().seq,
        inv.headers.cseq().unwrap().seq
    );
    let err = job.await.unwrap().expect_err("invite fails");
    assert!(
        matches!(err, sip_core::endpoint::InviteError::Timeout),
        "{err:?}"
    );
}

/// The timeout is configurable, and with no answer at all Timer B (32 s) ends the attempt
/// first, without a CANCEL.
#[tokio::test(start_paused = true)]
async fn e08_invite_timeout_is_configurable_and_timer_b_wins_without_any_answer() {
    let w = W::new();
    let mut opts = invite_opts_for(&w);
    opts.timeout = Duration::from_secs(10);
    let ep = w.ep.clone();
    let job = tokio::spawn(async move { ep.invite(opts).await });
    settle().await;
    let (_, inv) = w.requests("INVITE").remove(0);
    w.feed_resp(resp_to(&inv, 180, "theirs")).await;
    w.at(10_000).await;
    assert_eq!(w.requests("CANCEL")[0].0, 10_000);
    assert!(job.await.unwrap().is_err());

    // no response at all: Timer B (64*T1 = 32 s) fires, nothing is cancelled
    let w2 = W::new();
    let ep = w2.ep.clone();
    let opts = invite_opts_for(&w2);
    let job = tokio::spawn(async move { ep.invite(opts).await });
    settle().await;
    w2.at(31_999).await;
    assert!(!job.is_finished());
    w2.at(32_000).await;
    let err = job.await.unwrap().expect_err("invite fails");
    assert!(matches!(err, sip_core::endpoint::InviteError::Timeout));
    assert!(w2.requests("CANCEL").is_empty());
    let sent = w2.requests("INVITE");
    assert_eq!(
        sent.len(),
        7,
        "Timer A retransmissions up to 32 s: {sent:?}"
    );
}

/// M1d: `IncomingCall::reject_with` adds the given headers to the final response (fax-in uses
/// it for `Unsupported` on a 420).
#[tokio::test(start_paused = true)]
async fn g02_reject_with_adds_headers_and_validates_the_status() {
    let mut w = W::new();
    w.feed_req(peer_invite(&Inv::default())).await;
    let call = w.next_call().await;
    // a 2xx is not a rejection
    let err = w.next_call_err_for_2xx(call).await;
    assert!(
        matches!(err, sip_core::endpoint::CallError::BadStatus(_)),
        "{err:?}"
    );
    let inv2 = Inv {
        branch: "z9hG4bKinv9",
        call_id: "call-9@10.0.0.1",
        ..Inv::default()
    };
    w.feed_req(peer_invite(&inv2)).await;
    let call2 = w.next_call().await;
    call2
        .reject_with(
            StatusCode::BAD_EXTENSION,
            &[("Unsupported", "foo, bar"), ("Retry-After", "5")],
        )
        .await
        .unwrap();
    settle().await;
    let r = w.responses(420, "INVITE");
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].1.headers.get("Unsupported"), Some("foo, bar"));
    assert_eq!(r[0].1.headers.get("Retry-After"), Some("5"));
    assert!(r[0].1.headers.to().unwrap().tag().is_some());
}

impl W {
    async fn next_call_err_for_2xx(&self, call: IncomingCall) -> sip_core::endpoint::CallError {
        call.reject_with(StatusCode::OK, &[]).await.unwrap_err()
    }
}

// ================================================================ K01 (M2a)

/// K01: the 200 to our re-INVITE and the peer's BYE reach the stack in the same instant (two
/// datagrams back to back, no scheduling point between them). `reinvite()` must report
/// `Accepted`: the 200 was received first. Before the fix the BYE could overtake the 200 on its
/// way through the transaction layer (different tasks), terminate the dialog and make
/// `reinvite()` fail with `Terminated`.
#[tokio::test(start_paused = true)]
async fn k01_reinvite_200_and_bye_in_the_same_instant_still_report_accepted() {
    for round in 0..20 {
        let mut w = W::new();
        let mut dlg = establish(&mut w, &Inv::default(), Some(10)).await;
        assert!(matches!(
            next_event(&mut dlg).await,
            Some(DialogEvent::Established)
        ));
        let ctl = dlg.control();
        let ours = tokio::spawn(async move { ctl.reinvite(sdp(T38_OFFER)).await });
        settle().await;
        let (_, inv) = w.requests("INVITE").remove(0);
        let tag = w.our_tag();
        let ok = resp_with(
            &inv,
            200,
            FROM_TAG,
            &[("Contact", GW_CONTACT.to_string())],
            Some(T38_OFFER),
        );
        let bye = peer_in_dialog("BYE", "z9hG4bKk01bye", 2, &tag, None, &[]);
        // both land in the transport queue before anything runs
        w.inject
            .send(incoming(SipMessage::Response(ok), TransportKind::Udp))
            .await
            .unwrap();
        w.inject
            .send(incoming(SipMessage::Request(bye), TransportKind::Udp))
            .await
            .unwrap();
        settle().await;
        let out = ours.await.unwrap();
        assert!(
            matches!(out, Ok(ReinviteOutcome::Accepted(_))),
            "round {round}: expected Accepted, got {out:?}"
        );
        // the BYE is still honoured: answered, event + termination follow, ACK for the 200 was sent
        assert_eq!(w.responses(200, "BYE").len(), 1, "round {round}");
        assert_eq!(
            w.requests("ACK").len(),
            1,
            "round {round}: ACK for the re-INVITE 200"
        );
        assert!(matches!(next_event(&mut dlg).await, Some(DialogEvent::Bye)));
        assert!(matches!(
            next_event(&mut dlg).await,
            Some(DialogEvent::Terminated(TerminateReason::RemoteBye))
        ));
        assert_eq!(w.ep.dialog_count(), 0);
    }
}

/// A BYE alone (no 200 ever arrives) still fails the pending re-INVITE with `Terminated`, and
/// quickly: the grace given to an in-flight 200 must not delay the BYE handling.
#[tokio::test(start_paused = true)]
async fn k01_bye_without_a_200_still_terminates_the_pending_reinvite() {
    let mut w = W::new();
    let mut dlg = establish(&mut w, &Inv::default(), Some(10)).await;
    let _ = next_event(&mut dlg).await;
    let ctl = dlg.control();
    let ours = tokio::spawn(async move { ctl.reinvite(sdp(T38_OFFER)).await });
    settle().await;
    let tag = w.our_tag();
    w.at(500).await;
    w.feed_req(peer_in_dialog("BYE", "z9hG4bKk01bye2", 2, &tag, None, &[]))
        .await;
    let out = ours.await.unwrap();
    assert!(matches!(out, Err(DialogError::Terminated)), "{out:?}");
    let ok = w.responses(200, "BYE");
    assert_eq!(ok.len(), 1);
    assert_eq!(
        ok[0].0, 500,
        "the BYE is answered in the same virtual millisecond"
    );
}
