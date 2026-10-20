//! Shared helpers for the transport / transaction / registration tests.
#![allow(dead_code)]

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use sip_core::message::{Request, Response, SipMessage, parse_datagram};
use sip_core::transport::{Incoming, Outgoing, TransportKind};
use tokio::sync::mpsc;
use tokio::time::Instant;

/// Builds a request from text pieces; `to_tag` is optional.
pub fn mk_request(
    method: &str,
    branch: &str,
    sent_by: &str,
    cseq: u32,
    cseq_method: &str,
    to_tag: Option<&str>,
) -> Request {
    let to = match to_tag {
        Some(t) => format!("<sip:fax@192.0.2.10>;tag={t}"),
        None => "<sip:fax@192.0.2.10>".to_string(),
    };
    let text = format!(
        "{method} sip:fax@192.0.2.10 SIP/2.0\r\n\
         Via: SIP/2.0/UDP {sent_by};branch={branch}\r\n\
         Max-Forwards: 70\r\n\
         From: <sip:gw@10.0.0.1>;tag=g1\r\n\
         To: {to}\r\n\
         Call-ID: call-{branch}@10.0.0.1\r\n\
         CSeq: {cseq} {cseq_method}\r\n\
         Contact: <sip:gw@10.0.0.1:5060>\r\n\
         Content-Length: 0\r\n\r\n"
    );
    match parse_datagram(text.as_bytes()) {
        Ok(SipMessage::Request(r)) => r,
        other => panic!("bad test request {other:?}"),
    }
}

pub fn invite(branch: &str) -> Request {
    mk_request("INVITE", branch, "10.0.0.1:5060", 1, "INVITE", None)
}

pub const PEER: &str = "10.0.0.1:5060";
pub const LOCAL: &str = "192.0.2.10:5060";

pub fn inc_req(r: Request, kind: TransportKind) -> Incoming {
    incoming(SipMessage::Request(r), kind)
}

pub fn inc_resp(r: Response, kind: TransportKind) -> Incoming {
    incoming(SipMessage::Response(r), kind)
}

pub fn incoming(msg: SipMessage, kind: TransportKind) -> Incoming {
    Incoming {
        msg,
        source: PEER.parse::<SocketAddr>().unwrap(),
        transport: kind,
        local: LOCAL.parse::<SocketAddr>().unwrap(),
    }
}

pub fn first_line(o: &Outgoing) -> String {
    let bytes = match o {
        Outgoing::Request { req, .. } => req.to_bytes(),
        Outgoing::Response { resp, .. } => resp.to_bytes(),
    };
    String::from_utf8_lossy(&bytes)
        .lines()
        .next()
        .unwrap_or("")
        .to_string()
}

pub fn wire(o: &Outgoing) -> String {
    let bytes = match o {
        Outgoing::Request { req, .. } => req.to_bytes(),
        Outgoing::Response { resp, .. } => resp.to_bytes(),
    };
    String::from_utf8_lossy(&bytes).to_string()
}

pub fn as_response(o: &Outgoing) -> &Response {
    match o {
        Outgoing::Response { resp, .. } => resp,
        other => panic!("expected a response, got {other:?}"),
    }
}

pub fn as_request(o: &Outgoing) -> &Request {
    match o {
        Outgoing::Request { req, .. } => req,
        other => panic!("expected a request, got {other:?}"),
    }
}

/// Records every outgoing item with the (virtual) millisecond it was sent at.
#[derive(Clone)]
pub struct Recorder {
    t0: Instant,
    log: Arc<Mutex<Vec<(u64, Outgoing)>>>,
}

impl Recorder {
    pub fn spawn(t0: Instant, mut rx: mpsc::Receiver<Outgoing>) -> Recorder {
        let rec = Recorder {
            t0,
            log: Arc::new(Mutex::new(Vec::new())),
        };
        let log = rec.log.clone();
        tokio::spawn(async move {
            while let Some(o) = rx.recv().await {
                let ms = (Instant::now() - t0).as_millis() as u64;
                log.lock().unwrap().push((ms, o));
            }
        });
        rec
    }

    pub fn all(&self) -> Vec<(u64, Outgoing)> {
        self.log.lock().unwrap().clone()
    }

    pub fn count(&self) -> usize {
        self.log.lock().unwrap().len()
    }

    /// Instants (ms) of every item whose first line starts with `prefix`.
    pub fn instants(&self, prefix: &str) -> Vec<u64> {
        self.all()
            .iter()
            .filter(|(_, o)| first_line(o).starts_with(prefix))
            .map(|(t, _)| *t)
            .collect()
    }

    pub fn elapsed_ms(&self) -> u64 {
        (Instant::now() - self.t0).as_millis() as u64
    }
}

pub async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}
