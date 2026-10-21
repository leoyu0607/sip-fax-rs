//! Webhook delivery: HTTP POST of the record as JSON (reqwest, rustls).
//!
//! Request: `Content-Type: application/json` with the record fields, or with `attach_pdf`
//! `multipart/form-data` (part `record` = the same JSON, part `pdf` = the file). With a `secret`,
//! header `<signature_header>: sha256=<lowercase hex HMAC-SHA256 of the exact request body>`.
//! Other headers: `X-Sipfax-Event` (`fax.received` / `fax.partial`) and `X-Sipfax-Attempt`.
//!
//! Failure classes: 2xx is success; connection errors, timeouts, 5xx, 408, 425 and 429 are
//! transient; any other status (4xx, 3xx - redirects are not followed) is permanent.

use crate::config::{Format, WebhookConfig};
use crate::error::DeliveryError;
use crate::source;
use crate::template::sanitize_name;
use chrono::{DateTime, SecondsFormat, Utc};
use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use std::path::Path;
use std::time::Duration;
use store::{FaxRecord, FaxStatus};

/// `sha256=<hex>` signature of `body` under `secret`.
pub fn signature(secret: &str, body: &[u8]) -> String {
    let mut mac =
        <Hmac<Sha256> as KeyInit>::new_from_slice(secret.as_bytes()).expect("HMAC takes any key");
    mac.update(body);
    let tag = mac.finalize().into_bytes();
    let mut s = String::from("sha256=");
    for b in tag {
        s.push_str(&format!("{b:02x}"));
    }
    s
}

fn ts(t: Option<DateTime<Utc>>) -> serde_json::Value {
    match t {
        Some(t) => t.to_rfc3339_opts(SecondsFormat::Millis, true).into(),
        None => serde_json::Value::Null,
    }
}

/// JSON body for `rec` (UTC RFC 3339 time stamps).
pub fn record_json(rec: &FaxRecord, attempt: i32) -> serde_json::Value {
    serde_json::json!({
        "event": event_name(rec.fax_status),
        "attempt": attempt,
        "id": rec.id,
        "call_id": rec.call_id,
        "trunk": rec.trunk,
        "ani": rec.ani,
        "dnis": rec.dnis,
        "remote_ident": rec.remote_ident,
        "started_at": ts(Some(rec.started_at)),
        "answered_at": ts(rec.answered_at),
        "ended_at": ts(rec.ended_at),
        "duration_sec": rec.duration_sec,
        "hangup_direction": rec.hangup_direction.map(|d| d.as_str()),
        "hangup_code": rec.hangup_code,
        "hangup_text": rec.hangup_text,
        "t30_code": rec.t30_code,
        "fax_status": rec.fax_status.as_str(),
        "pages": rec.pages,
        "resolution": rec.resolution,
        "bit_rate": rec.bit_rate,
        "compression": rec.compression,
        "ecm": rec.ecm,
        "pdf_size": rec.pdf_size,
        "tiff_size": rec.tiff_size,
    })
}

fn event_name(s: FaxStatus) -> &'static str {
    match s {
        FaxStatus::Partial => "fax.partial",
        _ => "fax.received",
    }
}

/// Build the request body and its `Content-Type`.
pub async fn build_body(
    cfg: &WebhookConfig,
    rec: &FaxRecord,
    data_dir: &Path,
    attempt: i32,
) -> Result<(Vec<u8>, String), DeliveryError> {
    let json = serde_json::to_vec(&record_json(rec, attempt)).expect("json serialises");
    if !cfg.attach_pdf {
        return Ok((json, "application/json".into()));
    }
    let pdf = source::read(data_dir, rec, Format::Pdf).await?;
    let boundary = format!("sipfax-{:032x}", rand::random::<u128>());
    let name = sanitize_name(&format!("fax-{}.pdf", rec.id));
    let mut b = Vec::with_capacity(json.len() + pdf.len() + 512);
    b.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"record\"\r\nContent-Type: application/json\r\n\r\n"
        )
        .as_bytes(),
    );
    b.extend_from_slice(&json);
    b.extend_from_slice(
        format!(
            "\r\n--{boundary}\r\nContent-Disposition: form-data; name=\"pdf\"; filename=\"{name}\"\r\nContent-Type: application/pdf\r\n\r\n"
        )
        .as_bytes(),
    );
    b.extend_from_slice(&pdf);
    b.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    Ok((b, format!("multipart/form-data; boundary={boundary}")))
}

/// Map an HTTP status to success / failure class.
pub fn classify_status(code: u16) -> Result<(), DeliveryError> {
    match code {
        200..=299 => Ok(()),
        408 | 425 | 429 | 500..=599 => Err(DeliveryError::transient(format!("HTTP {code}"))),
        _ => Err(DeliveryError::permanent(format!("HTTP {code}"))),
    }
}

/// Build the shared HTTP client.
pub fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("sip-faxd/", env!("CARGO_PKG_VERSION")))
        .build()
        .expect("reqwest client")
}

/// POST the record.
pub async fn deliver(
    client: &reqwest::Client,
    cfg: &WebhookConfig,
    rec: &FaxRecord,
    data_dir: &Path,
    attempt: i32,
) -> Result<(), DeliveryError> {
    let (body, ctype) = build_body(cfg, rec, data_dir, attempt).await?;
    let mut req = client
        .post(&cfg.url)
        .timeout(Duration::from_secs(cfg.timeout_secs))
        .header("Content-Type", ctype)
        .header("X-Sipfax-Event", event_name(rec.fax_status))
        .header("X-Sipfax-Attempt", attempt.to_string());
    if let Some(secret) = &cfg.secret {
        req = req.header(cfg.signature_header.as_str(), signature(secret, &body));
    }
    for (k, v) in &cfg.headers {
        req = req.header(k.as_str(), v.as_str());
    }
    match req.body(body).send().await {
        Ok(resp) => classify_status(resp.status().as_u16()),
        Err(e) => {
            // Never log the URL: it may carry a token.
            let e = e.without_url();
            Err(DeliveryError::transient(format!("request failed: {e}")))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::FailureKind;

    #[test]
    fn d04_classify_status_table() {
        for c in [200, 201, 204, 299] {
            assert!(classify_status(c).is_ok(), "{c}");
        }
        for c in [408, 425, 429, 500, 502, 503, 599] {
            assert_eq!(
                classify_status(c).unwrap_err().kind,
                FailureKind::Transient,
                "{c}"
            );
        }
        for c in [301, 302, 400, 401, 403, 404, 410, 422] {
            assert_eq!(
                classify_status(c).unwrap_err().kind,
                FailureKind::Permanent,
                "{c}"
            );
        }
    }

    #[test]
    fn d04_signature_known_vector() {
        // RFC 4231 test case 2.
        assert_eq!(
            signature("Jefe", b"what do ya want for nothing?"),
            "sha256=5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
    }
}
