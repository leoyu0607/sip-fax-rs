//! Message serialization: CRLF line endings, `Content-Length` always recomputed.

use bytes::Bytes;

use super::{Headers, Request, Response, SipMessage};

fn clean(s: &str) -> String {
    // A CR/LF inside a value would let it forge extra header lines; fold it into a space.
    if s.contains(['\r', '\n']) {
        s.replace(['\r', '\n'], " ")
    } else {
        s.to_string()
    }
}

fn write_headers(out: &mut Vec<u8>, headers: &Headers, body_len: usize) {
    let mut wrote_cl = false;
    for h in headers.iter() {
        let is_cl = h.name.eq_ignore_ascii_case("Content-Length");
        if is_cl && wrote_cl {
            continue; // duplicates would conflict with the recomputed value
        }
        out.extend_from_slice(clean(&h.wire_name()).as_bytes());
        out.extend_from_slice(b": ");
        if is_cl {
            out.extend_from_slice(body_len.to_string().as_bytes());
            wrote_cl = true;
        } else {
            out.extend_from_slice(clean(&h.value).as_bytes());
        }
        out.extend_from_slice(b"\r\n");
    }
    if !wrote_cl {
        out.extend_from_slice(format!("Content-Length: {body_len}\r\n").as_bytes());
    }
    out.extend_from_slice(b"\r\n");
}

impl Request {
    /// Serializes the request. `Content-Length` is (re)computed from the body.
    pub fn to_bytes(&self) -> Bytes {
        let mut out = Vec::with_capacity(512 + self.body.len());
        out.extend_from_slice(
            format!(
                "{} {} SIP/2.0\r\n",
                clean(self.method.as_str()),
                clean(&self.uri.to_string())
            )
            .as_bytes(),
        );
        write_headers(&mut out, &self.headers, self.body.len());
        out.extend_from_slice(&self.body);
        Bytes::from(out)
    }
}

impl Response {
    /// Serializes the response. `Content-Length` is (re)computed from the body.
    pub fn to_bytes(&self) -> Bytes {
        let mut out = Vec::with_capacity(512 + self.body.len());
        out.extend_from_slice(
            format!("SIP/2.0 {} {}\r\n", self.status, clean(&self.reason)).as_bytes(),
        );
        write_headers(&mut out, &self.headers, self.body.len());
        out.extend_from_slice(&self.body);
        Bytes::from(out)
    }
}

impl SipMessage {
    /// Serializes either variant.
    pub fn to_bytes(&self) -> Bytes {
        match self {
            SipMessage::Request(r) => r.to_bytes(),
            SipMessage::Response(r) => r.to_bytes(),
        }
    }
}
