//! D04: webhook against a real HTTP server on localhost. The HMAC is recomputed here with a
//! hand-written HMAC-SHA256 (RFC 2104 on top of the raw `sha2` hash), not with the `hmac` crate and
//! not with any function of the `delivery` crate.

mod common;

use common::*;
use sha2::{Digest, Sha256};
use store::{DeliveryStatus, FaxStatus};

/// HMAC-SHA256 from the RFC 2104 definition.
fn hmac_sha256(key: &[u8], msg: &[u8]) -> [u8; 32] {
    let mut k = [0u8; 64];
    if key.len() > 64 {
        k[..32].copy_from_slice(&Sha256::digest(key));
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut inner = Sha256::new();
    inner.update(k.map(|b| b ^ 0x36));
    inner.update(msg);
    let ih = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(k.map(|b| b ^ 0x5c));
    outer.update(ih);
    outer.finalize().into()
}

fn hex(b: &[u8]) -> String {
    b.iter().map(|x| format!("{x:02x}")).collect()
}

#[test]
fn d04_test_hmac_matches_rfc4231_vectors() {
    // The independent implementation is itself checked against published vectors.
    assert_eq!(
        hex(&hmac_sha256(b"Jefe", b"what do ya want for nothing?")),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
    assert_eq!(
        hex(&hmac_sha256(&[0x0b; 20], b"Hi There")),
        "b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7"
    );
    // Key longer than the block size (test case 6).
    assert_eq!(
        hex(&hmac_sha256(
            &[0xaa; 131],
            b"Test Using Larger Than Block-Size Key - Hash Key First"
        )),
        "60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54"
    );
}

fn hook_routes(url: &str, extra: &str) -> delivery::Routes {
    routes(&format!(
        "[[routes]]\ndefault = true\n[[routes.deliveries]]\nkind = \"webhook\"\nurl = \"{url}\"\n{extra}\n"
    ))
}

#[tokio::test]
async fn d04_webhook_posts_json_with_record_fields_and_signature() {
    let mut env = Env::new().await;
    let hook = Hook::start(200).await;
    let secret = "s3cr3t-key";
    let eng = env.engine(&hook_routes(
        &hook.url(),
        &format!("secret = \"{secret}\"\nsignature_header = \"X-Hub-Sig\"\nheaders = {{ \"X-Env\" = \"test\" }}"),
    ));
    let f = env
        .fax("audiocodes", "025570001", FaxStatus::Received)
        .await;
    eng.dispatch(f).await.unwrap();
    let rep = eng.run_due().await.unwrap();
    assert_eq!((rep.attempted, rep.done), (1, 1));

    let reqs = hook.requests();
    assert_eq!(reqs.len(), 1);
    let r = &reqs[0];
    assert_eq!((r.method.as_str(), r.path.as_str()), ("POST", "/hook"));
    assert_eq!(r.header("content-type"), Some("application/json"));
    assert_eq!(r.header("x-sipfax-event"), Some("fax.received"));
    assert_eq!(r.header("x-sipfax-attempt"), Some("1"));
    assert_eq!(r.header("x-env"), Some("test"));
    // Independent signature over the exact body bytes.
    let want = format!("sha256={}", hex(&hmac_sha256(secret.as_bytes(), &r.body)));
    assert_eq!(r.header("x-hub-sig"), Some(want.as_str()));
    // A signature made with another key must differ (the check above is not vacuous).
    assert_ne!(
        want,
        format!("sha256={}", hex(&hmac_sha256(b"other", &r.body)))
    );
    assert!(
        r.header("x-sipfax-signature").is_none(),
        "custom header name replaces the default"
    );

    let rec = env.record(f).await;
    let j: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(j["event"], "fax.received");
    assert_eq!(j["attempt"], 1);
    assert_eq!(j["id"], f);
    assert_eq!(j["call_id"], rec.call_id);
    assert_eq!(j["trunk"], "audiocodes");
    assert_eq!(j["ani"], "0227001234");
    assert_eq!(j["dnis"], "025570001");
    assert_eq!(j["remote_ident"], "+886 2 2700 1234");
    assert_eq!(j["started_at"], "2026-10-21T00:00:00.000Z");
    assert_eq!(j["ended_at"], "2026-10-21T00:01:00.000Z");
    assert_eq!(j["duration_sec"], 60);
    assert_eq!(j["fax_status"], "received");
    assert_eq!(j["hangup_code"], "completed");
    assert_eq!(j["hangup_direction"], "remote");
    assert_eq!(j["pages"], 2);
    assert_eq!(j["resolution"], "fine");
    assert_eq!(j["bit_rate"], 14400);
    assert_eq!(j["compression"], "MMR");
    assert_eq!(j["ecm"], true);
    assert_eq!(j["pdf_size"], pdf_bytes(f).len());
    assert_eq!(j["tiff_size"], tiff_bytes(f).len());
    assert!(j["answered_at"].is_null());
    // No file system paths leak to the receiver.
    assert!(!String::from_utf8_lossy(&r.body).contains("faxes/"));
}

#[tokio::test]
async fn d04_webhook_without_secret_has_no_signature_header() {
    let mut env = Env::new().await;
    let hook = Hook::start(204).await;
    let eng = env.engine(&hook_routes(&hook.url(), ""));
    let f = env.fax("t", "1", FaxStatus::Partial).await;
    // Partial is only delivered when the route says so.
    assert!(eng.dispatch(f).await.unwrap().is_empty());
    let routes = routes(&format!(
        "[[routes]]\ndefault = true\ndeliver_partial = true\n[[routes.deliveries]]\nkind = \"webhook\"\nurl = \"{}\"\n",
        hook.url()
    ));
    let eng = env.engine(&routes);
    eng.dispatch(f).await.unwrap();
    assert_eq!(eng.run_due().await.unwrap().done, 1);
    let r = &hook.requests()[0];
    assert!(r.header("x-sipfax-signature").is_none());
    assert_eq!(r.header("x-sipfax-event"), Some("fax.partial"));
    let j: serde_json::Value = serde_json::from_slice(&r.body).unwrap();
    assert_eq!(j["fax_status"], "partial");
}

/// Split a multipart body on `boundary` into (headers, content) parts.
fn multipart_parts(body: &[u8], boundary: &str) -> Vec<(String, Vec<u8>)> {
    let delim = format!("--{boundary}").into_bytes();
    let mut parts = Vec::new();
    let find = |from: usize, pat: &[u8]| {
        body[from..]
            .windows(pat.len())
            .position(|w| w == pat)
            .map(|p| p + from)
    };
    let mut cur = find(0, &delim).expect("opening boundary");
    loop {
        let mut pos = cur + delim.len();
        if body[pos..].starts_with(b"--") {
            break;
        }
        assert!(body[pos..].starts_with(b"\r\n"));
        pos += 2;
        let hdr_end = find(pos, b"\r\n\r\n").expect("header end");
        let headers = String::from_utf8(body[pos..hdr_end].to_vec()).unwrap();
        let content_start = hdr_end + 4;
        let next =
            find(content_start, &[b"\r\n".as_slice(), &delim].concat()).expect("next boundary");
        parts.push((headers, body[content_start..next].to_vec()));
        cur = next + 2;
    }
    parts
}

#[tokio::test]
async fn d04_webhook_multipart_attaches_pdf_and_signs_the_whole_body() {
    let mut env = Env::new().await;
    let hook = Hook::start(200).await;
    let secret = "multipart-secret";
    let eng = env.engine(&hook_routes(
        &hook.url(),
        &format!("secret = \"{secret}\"\nattach_pdf = true"),
    ));
    let f = env.fax("t", "025", FaxStatus::Received).await;
    eng.dispatch(f).await.unwrap();
    assert_eq!(eng.run_due().await.unwrap().done, 1);
    let r = &hook.requests()[0];
    let ct = r.header("content-type").unwrap();
    let boundary = ct.strip_prefix("multipart/form-data; boundary=").expect(ct);
    let parts = multipart_parts(&r.body, boundary);
    assert_eq!(parts.len(), 2);
    assert!(parts[0].0.contains("name=\"record\"") && parts[0].0.contains("application/json"));
    let j: serde_json::Value = serde_json::from_slice(&parts[0].1).unwrap();
    assert_eq!(j["id"], f);
    assert!(parts[1].0.contains("name=\"pdf\""), "{}", parts[1].0);
    assert!(
        parts[1].0.contains(&format!("filename=\"fax-{f}.pdf\"")),
        "{}",
        parts[1].0
    );
    assert!(parts[1].0.contains("application/pdf"));
    assert_eq!(
        parts[1].1,
        pdf_bytes(f),
        "PDF bytes survive the multipart framing"
    );
    let want = format!("sha256={}", hex(&hmac_sha256(secret.as_bytes(), &r.body)));
    assert_eq!(r.header("x-sipfax-signature"), Some(want.as_str()));
}

#[tokio::test]
async fn d04_webhook_multipart_without_pdf_file_fails_permanently() {
    let mut env = Env::new().await;
    let hook = Hook::start(200).await;
    let eng = env.engine(&hook_routes(&hook.url(), "attach_pdf = true"));
    let f = env.fax("t", "1", FaxStatus::Received).await;
    std::fs::remove_file(env.data.join(format!("faxes/{f}.pdf"))).unwrap();
    eng.dispatch(f).await.unwrap();
    let rep = eng.run_due().await.unwrap();
    assert_eq!((rep.failed, rep.retry_scheduled), (1, 0));
    assert_eq!(hook.count(), 0, "nothing is sent without the attachment");
}

#[tokio::test]
async fn d04_webhook_status_classes_decide_retry() {
    let mut env = Env::new().await;
    let hook = Hook::start(200).await;
    let eng = env.engine(&hook_routes(&hook.url(), ""));
    // One fax per status; each has its own row.
    let cases: &[(u16, bool)] = &[
        (500, true),
        (502, true),
        (503, true),
        (429, true),
        (408, true),
        (400, false),
        (401, false),
        (404, false),
        (410, false),
        (302, false),
    ];
    for (code, transient) in cases {
        let f = env.fax("t", "1", FaxStatus::Received).await;
        hook.script(&[*code]);
        eng.dispatch(f).await.unwrap();
        let rep = eng.run_due().await.unwrap();
        let at = env.attempts(f).await;
        if *transient {
            assert_eq!((rep.retry_scheduled, rep.failed), (1, 0), "{code}");
            assert_eq!(at.len(), 2, "{code}");
            assert_eq!(at[1].status, DeliveryStatus::Pending);
        } else {
            assert_eq!((rep.retry_scheduled, rep.failed), (0, 1), "{code}");
            assert_eq!(at.len(), 1, "{code}");
        }
        assert_eq!(at[0].status, DeliveryStatus::Failed);
        assert_eq!(
            at[0].last_error.as_deref(),
            Some(format!("HTTP {code}").as_str())
        );
    }
}

#[tokio::test]
async fn d04_webhook_connection_refused_is_transient_and_error_hides_url_secrets() {
    let mut env = Env::new().await;
    let port = closed_port().await;
    let eng = env.engine(&hook_routes(
        &format!("http://user:pw@127.0.0.1:{port}/hook?token=SECRET"),
        "",
    ));
    let f = env.fax("t", "1", FaxStatus::Received).await;
    eng.dispatch(f).await.unwrap();
    let rep = eng.run_due().await.unwrap();
    assert_eq!(rep.retry_scheduled, 1);
    let at = env.attempts(f).await;
    let err = at[0].last_error.clone().unwrap();
    assert!(err.starts_with("request failed"), "{err}");
    // Neither the stored target nor the error text carries credentials or the token.
    for s in [err.as_str(), at[0].target.as_str(), at[1].target.as_str()] {
        assert!(!s.contains("SECRET") && !s.contains("pw@"), "{s}");
    }
    assert_eq!(
        at[0].target,
        format!("webhook:http://127.0.0.1:{port}/hook")
    );
}

#[tokio::test]
async fn d04_webhook_does_not_follow_redirects() {
    let mut env = Env::new().await;
    let hook = Hook::start(307).await;
    let eng = env.engine(&hook_routes(&hook.url(), ""));
    let f = env.fax("t", "1", FaxStatus::Received).await;
    eng.dispatch(f).await.unwrap();
    let rep = eng.run_due().await.unwrap();
    assert_eq!((rep.failed, hook.count()), (1, 1));
}
