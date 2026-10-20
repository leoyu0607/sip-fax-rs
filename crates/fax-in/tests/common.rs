//! Harness for the deterministic (detached, paused-time) fax-in tests: the test plays the
//! upstream gateway by injecting SIP messages and reads exactly what the controller put on the
//! wire together with the virtual millisecond it was sent at. No real sleeping.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use fax_in::trunk::TrunkTable;
use fax_in::{CallEvent, CallResult, FaxIn, FaxInConfig, StartOptions};
use sip_core::endpoint::Endpoint;
use sip_core::message::{Request, Response, SipMessage, StatusCode, parse_datagram};
use sip_core::sdp::{SessionDescription, parse_sdp};
use sip_core::transport::{Incoming, Outgoing, TransportHandle, TransportKind};
use tokio::sync::mpsc;
use tokio::time::Instant;

pub const LOCAL: &str = "192.0.2.10:5060";
pub const GW: &str = "10.0.0.1:5060";
pub const GW_TAG: &str = "gw1";
pub const GW_CONTACT: &str = "<sip:gw@10.0.0.1:5070>";

pub const AUDIO_SDP: &str = "v=0\r\no=Sonus_UAC 1523094871 1523094871 IN IP4 10.20.30.40\r\ns=SIP Media Capabilities\r\nc=IN IP4 10.20.30.41\r\nt=0 0\r\nm=audio 16384 RTP/AVP 0 8 101\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:8 PCMA/8000\r\na=rtpmap:101 telephone-event/8000\r\na=fmtp:101 0-15\r\na=ptime:20\r\na=sendrecv\r\n";

pub const T38_SDP: &str = "v=0\r\no=gw 100 100 IN IP4 203.0.113.9\r\ns=-\r\nc=IN IP4 203.0.113.9\r\nt=0 0\r\nm=image 5004 udptl t38\r\na=T38FaxVersion:0\r\na=T38MaxBitRate:14400\r\na=T38FaxRateManagement:transferredTCF\r\na=T38FaxMaxBuffer:1800\r\na=T38FaxMaxDatagram:200\r\na=T38FaxUdpEC:t38UDPRedundancy\r\n";

pub const AUDIO_AND_T38_SDP: &str = "v=0\r\no=gw 100 100 IN IP4 203.0.113.9\r\ns=-\r\nc=IN IP4 203.0.113.9\r\nt=0 0\r\nm=audio 16384 RTP/AVP 0 8\r\na=rtpmap:0 PCMU/8000\r\na=rtpmap:8 PCMA/8000\r\nm=image 5004 udptl t38\r\na=T38FaxVersion:0\r\na=T38MaxBitRate:14400\r\na=T38FaxRateManagement:transferredTCF\r\na=T38FaxMaxDatagram:200\r\na=T38FaxUdpEC:t38UDPRedundancy\r\n";

pub fn sdp(t: &str) -> SessionDescription {
    parse_sdp(t.as_bytes()).expect("test SDP parses")
}

pub fn parse_req(text: &str) -> Request {
    match parse_datagram(text.as_bytes()) {
        Ok(SipMessage::Request(r)) => r,
        other => panic!("bad test request {other:?}\n{text}"),
    }
}

/// Builds a request from the gateway. `to_tag`: `None` for an initial INVITE.
pub struct Req<'a> {
    pub method: &'a str,
    pub call_id: &'a str,
    pub branch: String,
    pub cseq: u32,
    pub to_tag: Option<&'a str>,
    pub body: Option<&'a str>,
    pub extra: Vec<(&'a str, &'a str)>,
}

impl<'a> Req<'a> {
    pub fn invite(call_id: &'a str, body: &'a str) -> Req<'a> {
        Req {
            method: "INVITE",
            call_id,
            branch: branch_for(call_id, "inv"),
            cseq: 1,
            to_tag: None,
            body: Some(body),
            extra: vec![],
        }
    }

    pub fn in_dialog(
        method: &'a str,
        call_id: &'a str,
        branch: &'a str,
        cseq: u32,
        to_tag: &'a str,
    ) -> Req<'a> {
        Req {
            method,
            call_id,
            branch: branch_for(call_id, branch),
            cseq,
            to_tag: Some(to_tag),
            body: None,
            extra: vec![],
        }
    }

    pub fn body(mut self, b: &'a str) -> Self {
        self.body = Some(b);
        self
    }

    pub fn hdr(mut self, n: &'a str, v: &'a str) -> Self {
        self.extra.push((n, v));
        self
    }

    pub fn build(&self) -> Request {
        let to = match self.to_tag {
            Some(t) => format!("<sip:fax@192.0.2.10>;tag={t}"),
            None => "<sip:fax@192.0.2.10>".to_string(),
        };
        let mut t = format!(
            "{} sip:fax@192.0.2.10 SIP/2.0\r\nVia: SIP/2.0/UDP 10.0.0.1:5060;branch={}\r\n\
             Max-Forwards: 70\r\nFrom: <sip:gw@10.0.0.1>;tag={GW_TAG}\r\nTo: {to}\r\n\
             Call-ID: {}\r\nCSeq: {} {}\r\nContact: {GW_CONTACT}\r\n",
            self.method, self.branch, self.call_id, self.cseq, self.method
        );
        for (n, v) in &self.extra {
            t.push_str(&format!("{n}: {v}\r\n"));
        }
        match self.body {
            Some(b) => t.push_str(&format!(
                "Content-Type: application/sdp\r\nContent-Length: {}\r\n\r\n{b}",
                b.len()
            )),
            None => t.push_str("Content-Length: 0\r\n\r\n"),
        }
        parse_req(&t)
    }
}

/// A branch that is unique per call (transactions are keyed by branch + sent-by + method).
pub fn branch_for(call_id: &str, what: &str) -> String {
    let clean: String = call_id
        .chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .collect();
    format!("z9hG4bK{what}{clean}")
}

pub fn first_line(o: &Outgoing) -> String {
    wire(o).lines().next().unwrap_or("").to_string()
}

pub fn wire(o: &Outgoing) -> String {
    let bytes = match o {
        Outgoing::Request { req, .. } => req.to_bytes(),
        Outgoing::Response { resp, .. } => resp.to_bytes(),
    };
    String::from_utf8_lossy(&bytes).to_string()
}

#[derive(Clone)]
pub struct Recorder {
    log: Arc<Mutex<Vec<(u64, Outgoing)>>>,
}

impl Recorder {
    fn spawn(t0: Instant, mut rx: mpsc::Receiver<Outgoing>) -> Recorder {
        let log = Arc::new(Mutex::new(Vec::new()));
        let l2 = log.clone();
        tokio::spawn(async move {
            while let Some(o) = rx.recv().await {
                let ms = (Instant::now() - t0).as_millis() as u64;
                l2.lock().unwrap().push((ms, o));
            }
        });
        Recorder { log }
    }

    pub fn all(&self) -> Vec<(u64, Outgoing)> {
        self.log.lock().unwrap().clone()
    }
}

pub async fn settle() {
    for _ in 0..40 {
        tokio::task::yield_now().await;
    }
}

pub struct W {
    pub fx: Option<FaxIn>,
    pub events: mpsc::UnboundedReceiver<CallEvent>,
    pub rec: Recorder,
    pub inject: mpsc::Sender<Incoming>,
    pub t0: Instant,
    seen: Vec<CallEvent>,
}

impl W {
    pub async fn new(toml: &str) -> W {
        let cfg = FaxInConfig::parse(toml).expect("test config parses");
        W::from_config(cfg).await
    }

    pub async fn from_config(cfg: FaxInConfig) -> W {
        let table = TrunkTable::build(&cfg).unwrap();
        let d = TransportHandle::detached(LOCAL.parse().unwrap());
        let ec = FaxIn::endpoint_config(&cfg, &table, None);
        let (ep, calls) = Endpoint::detached(ec, d.handle, d.incoming);
        let t0 = Instant::now();
        let rec = Recorder::spawn(t0, d.outgoing);
        let (fx, events) =
            FaxIn::attach(cfg, table, ep, calls, None, StartOptions::default()).await;
        W {
            fx: Some(fx),
            events,
            rec,
            inject: d.inject,
            t0,
            seen: Vec::new(),
        }
    }

    pub async fn at(&self, ms: u64) {
        tokio::time::sleep_until(self.t0 + Duration::from_millis(ms)).await;
        settle().await;
    }

    pub fn now_ms(&self) -> u64 {
        (Instant::now() - self.t0).as_millis() as u64
    }

    pub async fn feed_from(&self, m: SipMessage, source: &str) {
        self.inject
            .send(Incoming {
                msg: m,
                source: source.parse::<SocketAddr>().unwrap(),
                transport: TransportKind::Udp,
                local: LOCAL.parse::<SocketAddr>().unwrap(),
            })
            .await
            .unwrap();
        settle().await;
    }

    pub async fn feed(&self, r: Request) {
        self.feed_from(SipMessage::Request(r), GW).await;
    }

    pub async fn feed_resp(&self, r: Response) {
        self.feed_from(SipMessage::Response(r), GW).await;
    }

    pub fn requests(&self, method: &str) -> Vec<(u64, Request)> {
        self.rec
            .all()
            .into_iter()
            .filter_map(|(t, o)| match o {
                Outgoing::Request { req, .. } if req.method.as_str() == method => Some((t, req)),
                _ => None,
            })
            .collect()
    }

    /// Responses with this status to a request with this CSeq method.
    pub fn responses(&self, code: u16, cseq_method: &str) -> Vec<(u64, Response)> {
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

    pub fn status_lines(&self) -> Vec<(u64, String)> {
        self.rec
            .all()
            .iter()
            .map(|(t, o)| (*t, first_line(o)))
            .collect()
    }

    /// All call events seen so far (drains the channel).
    pub fn events(&mut self) -> Vec<CallEvent> {
        while let Ok(e) = self.events.try_recv() {
            self.seen.push(e);
        }
        self.seen.clone()
    }

    pub fn results(&mut self) -> Vec<CallResult> {
        self.events()
            .into_iter()
            .filter_map(|e| match e {
                CallEvent::Finished(r) => Some(*r),
                _ => None,
            })
            .collect()
    }

    /// The To tag of our first 200 to the INVITE of `call_id`.
    pub fn our_tag(&self) -> String {
        let (_, r) = self.responses(200, "INVITE").remove(0);
        r.headers.to().unwrap().tag().unwrap().to_string()
    }

    /// Plays the gateway: INVITE with `body`, then (when `ack_at` is set) the ACK at that ms.
    /// Returns our tag.
    pub async fn call(&self, call_id: &str, body: &str, ack_at: Option<u64>) -> String {
        self.feed(Req::invite(call_id, body).build()).await;
        let tag = self.our_tag();
        if let Some(ms) = ack_at {
            self.at(ms).await;
            self.feed(Req::in_dialog("ACK", call_id, "ack1", 1, &tag).build())
                .await;
        }
        tag
    }

    pub async fn shutdown(&mut self) {
        if let Some(fx) = self.fx.take() {
            fx.shutdown().await;
        }
    }
}

/// A response to a request we sent (in-dialog), with `peer_tag` as To tag.
pub fn resp_to(req: &Request, code: u16, peer_tag: &str, body: Option<&str>) -> Response {
    let mut r = Response::for_request(req, StatusCode::new(code).unwrap(), Some(peer_tag));
    if let Some(b) = body {
        r.headers.push("Content-Type", "application/sdp");
        r.body = b.to_string().into();
    }
    r.headers.push("Contact", GW_CONTACT);
    r
}

/// Our SDP of a response/request body.
pub fn body_sdp(body: &[u8]) -> SessionDescription {
    parse_sdp(body).expect("body is SDP")
}

pub fn base_config(extra_trunk: &str) -> String {
    format!(
        "[sip]\nbind = \"192.0.2.10:5060\"\nadvertised = \"192.0.2.10:5060\"\n\n\
         [[trunks]]\nname = \"gw\"\nallowed_sources = [\"10.0.0.0/24\"]\n{extra_trunk}\n"
    )
}
