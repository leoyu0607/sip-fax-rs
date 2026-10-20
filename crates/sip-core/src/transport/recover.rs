//! Best-effort recovery of the transaction identifiers of an unparseable request, so that the
//! stack can still answer `400 Bad Request` (RFC 3261 8.2.1 / 16.3). When Via, From, To,
//! Call-ID or CSeq cannot be recovered there is nothing to address a response with, and the
//! caller drops the datagram.

use std::net::SocketAddr;

use bytes::Bytes;

use super::route;
use crate::message::{Method, Request, Response, SipUri, StatusCode, Via, canonical_name};

/// Builds the serialized `400` for the garbled request `data`, or `None` when it must not or
/// cannot be answered (responses and ACKs are never answered; all five headers are needed).
pub fn bad_request_reply(data: &[u8], source: SocketAddr) -> Option<Bytes> {
    let text = String::from_utf8_lossy(data);
    let head = text
        .split("\r\n\r\n")
        .next()
        .and_then(|h| h.split("\n\n").next())
        .unwrap_or("");
    // unfold continuation lines
    let mut lines: Vec<String> = Vec::new();
    for raw in head.split('\n') {
        let l = raw.trim_end_matches('\r');
        if (l.starts_with(' ') || l.starts_with('\t')) && !lines.is_empty() {
            if let Some(last) = lines.last_mut() {
                last.push(' ');
                last.push_str(l.trim());
            }
        } else {
            lines.push(l.to_string());
        }
    }
    let start = lines.first()?;
    let mut parts = start.split_ascii_whitespace();
    let method = parts.next()?;
    if method.to_ascii_uppercase().starts_with("SIP/") || Method::from_token(method) == Method::Ack
    {
        return None;
    }
    let (mut vias, mut from, mut to, mut call_id, mut cseq) = (Vec::new(), None, None, None, None);
    for l in &lines[1..] {
        let Some((name, value)) = l.split_once(':') else {
            continue;
        };
        let value = value.trim();
        if value.is_empty() {
            continue;
        }
        match canonical_name(name.trim()).0.as_str() {
            "Via" => vias.push(value.to_string()),
            "From" => from = from.or(Some(value.to_string())),
            "To" => to = to.or(Some(value.to_string())),
            "Call-ID" => call_id = call_id.or(Some(value.to_string())),
            "CSeq" => cseq = cseq.or(Some(value.to_string())),
            _ => {}
        }
    }
    let (from, mut to, call_id, cseq) = (from?, to?, call_id?, cseq?);
    // the first element of the first Via must be usable
    let first_via = vias.first()?.split(',').next()?.trim().to_string();
    Via::parse(&first_via).ok()?;
    // stamp received / rport on the topmost Via like for any other request
    let mut probe = Request::new(Method::Options, SipUri::new("invalid"));
    probe.headers.push("Via", first_via.clone());
    route::stamp_inbound_via(&mut probe, source);
    let stamped = probe.headers.get("Via").unwrap_or(&first_via).to_string();

    let mut resp = Response::new(StatusCode::BAD_REQUEST);
    resp.headers.push("Via", stamped);
    for (i, v) in vias.iter().enumerate() {
        // keep the remaining Vias (also the rest of a comma list in the first one)
        if i == 0 {
            if let Some((_, rest)) = v.split_once(',') {
                resp.headers.push("Via", rest.trim());
            }
        } else {
            resp.headers.push("Via", v.clone());
        }
    }
    if !to.to_ascii_lowercase().contains("tag=") {
        to = format!("{to};tag={:08x}", rand::random::<u32>());
    }
    resp.headers.push("From", from);
    resp.headers.push("To", to);
    resp.headers.push("Call-ID", call_id);
    resp.headers.push("CSeq", cseq);
    Some(resp.to_bytes())
}
