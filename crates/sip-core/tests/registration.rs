//! M1b registration client tests C03 and C04 against an in-process fake registrar.
//!
//! The fake registrar sits on `TransportHandle::detached` (the transport API without
//! sockets) below a real `TransactionLayer`; time is paused so every instant is exact.

mod common;

use std::time::Duration;

use md5::{Digest, Md5};
use sip_core::message::{Method, Request, Response, SipMessage, SipUri, StatusCode};
use sip_core::registration::{
    self, RegistrationConfig, RegistrationHandle, RegistrationState, backoff_delay, refresh_delay,
};
use sip_core::transaction::{TimerConfig, TransactionLayer};
use sip_core::transport::{Incoming, Outgoing, TransportHandle, TransportKind};
use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

const REGISTRAR: &str = "192.0.2.1:5060";
const REALM: &str = "fax.example";
const NONCE: &str = "dcd98b7102dd2f0e8b11d0f600bfb0c093";
const USER: &str = "faxuser";
const PASS: &str = "s3cret!";
const REG_URI: &str = "sip:registrar.example.com";

struct Fake {
    out: mpsc::Receiver<Outgoing>,
    inject: mpsc::Sender<Incoming>,
    t0: Instant,
}

impl Fake {
    /// Next request the client sent, with the virtual ms since the start.
    async fn next(&mut self) -> (u64, Request) {
        match self.out.recv().await.expect("transport closed") {
            Outgoing::Request {
                req,
                dest,
                transport,
            } => {
                assert_eq!(dest, REGISTRAR.parse().unwrap());
                assert_eq!(transport, TransportKind::Udp);
                ((Instant::now() - self.t0).as_millis() as u64, req)
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    async fn reply(&self, req: &Request, code: u16, headers: &[(&str, String)]) {
        let mut r = Response::for_request(req, StatusCode::new(code).unwrap(), Some("reg-tag"));
        for (n, v) in headers {
            r.headers.push(n, v.clone());
        }
        self.inject
            .send(Incoming {
                msg: SipMessage::Response(r),
                source: REGISTRAR.parse().unwrap(),
                transport: TransportKind::Udp,
                local: "192.0.2.10:5060".parse().unwrap(),
            })
            .await
            .unwrap();
    }

    async fn challenge(&self, req: &Request, qop: bool, proxy: bool) {
        let mut v = format!("Digest realm=\"{REALM}\", nonce=\"{NONCE}\", algorithm=MD5");
        if qop {
            v.push_str(", qop=\"auth\", opaque=\"op4que\"");
        }
        let (code, name) = if proxy {
            (407, "Proxy-Authenticate")
        } else {
            (401, "WWW-Authenticate")
        };
        self.reply(req, code, &[(name, v)]).await;
    }
}

fn config() -> RegistrationConfig {
    let mut c = RegistrationConfig::new(
        REGISTRAR.parse().unwrap(),
        TransportKind::Udp,
        SipUri::parse(REG_URI).unwrap(),
        SipUri::parse("sip:fax@example.com").unwrap(),
        SipUri::parse("sip:fax@192.0.2.10:5060").unwrap(),
        USER,
        PASS,
    );
    c.backoff_max = Duration::from_secs(30);
    c.user_agent = Some("sip-core-test".into());
    c
}

fn setup(cfg: RegistrationConfig) -> (Fake, RegistrationHandle, TransactionLayer) {
    let d = TransportHandle::detached("192.0.2.10:5060".parse().unwrap());
    let (layer, _tu) = TransactionLayer::new(d.handle.clone(), d.incoming, TimerConfig::default());
    // keep the TU receiver alive: dropping it only discards stray messages
    std::mem::forget(_tu);
    let t0 = Instant::now();
    let handle = registration::start(layer.clone(), cfg);
    (
        Fake {
            out: d.outgoing,
            inject: d.inject,
            t0,
        },
        handle,
        layer,
    )
}

fn md5_hex(s: &str) -> String {
    let mut h = Md5::new();
    h.update(s.as_bytes());
    h.finalize().iter().map(|b| format!("{b:02x}")).collect()
}

/// Splits `Digest a="x", b=y` into pairs (values unquoted).
fn digest_fields(value: &str) -> Vec<(String, String)> {
    let rest = value.strip_prefix("Digest ").expect("Digest scheme");
    rest.split(", ")
        .map(|p| {
            let (k, v) = p.split_once('=').expect("k=v");
            (k.to_string(), v.trim_matches('"').to_string())
        })
        .collect()
}

fn field<'a>(f: &'a [(String, String)], k: &str) -> &'a str {
    f.iter()
        .find(|(n, _)| n == k)
        .map(|(_, v)| v.as_str())
        .unwrap_or_else(|| panic!("no {k} in {f:?}"))
}

async fn wait_state(
    rx: &mut watch::Receiver<RegistrationState>,
    f: impl Fn(&RegistrationState) -> bool,
) -> RegistrationState {
    rx.wait_for(f).await.expect("state channel").clone()
}

// ================================================================ C03

#[tokio::test(start_paused = true)]
async fn c03_register_401_digest_response_is_correct_and_refresh_comes_at_90_percent() {
    let (mut fake, handle, _layer) = setup(config());
    let mut st = handle.subscribe();

    // --- first REGISTER: no credentials
    let (t, r1) = fake.next().await;
    assert_eq!(t, 0);
    let w1 = String::from_utf8(r1.to_bytes().to_vec()).unwrap();
    println!("C03 REGISTER #1:\n{w1}");
    assert!(w1.starts_with("REGISTER sip:registrar.example.com SIP/2.0\r\n"));
    assert!(w1.contains("Via: SIP/2.0/UDP 192.0.2.10:5060;branch=z9hG4bK"));
    assert!(w1.contains("To: <sip:fax@example.com>\r\n"));
    assert!(w1.contains("CSeq: 1 REGISTER\r\n"));
    assert!(w1.contains("Contact: <sip:fax@192.0.2.10:5060>\r\n"));
    assert!(w1.contains("Expires: 3600\r\n"));
    assert!(w1.contains("User-Agent: sip-core-test\r\n"));
    assert!(r1.headers.get("Authorization").is_none());
    assert_eq!(handle.state(), RegistrationState::Registering);
    let call_id = r1.headers.call_id().unwrap().to_string();
    let from1 = r1.headers.from().unwrap();
    assert!(from1.tag().is_some());

    // --- 401 -> second REGISTER with a digest response
    fake.challenge(&r1, false, false).await;
    let (t, r2) = fake.next().await;
    assert_eq!(t, 0, "the authenticated retry follows immediately");
    let w2 = String::from_utf8(r2.to_bytes().to_vec()).unwrap();
    println!("C03 REGISTER #2:\n{w2}");
    assert_eq!(r2.headers.cseq().unwrap().seq, 2);
    assert_eq!(r2.headers.call_id().unwrap(), call_id, "same Call-ID");
    assert_eq!(
        r2.headers.from().unwrap().tag(),
        from1.tag(),
        "same From tag"
    );
    assert_ne!(
        r1.headers.top_via().unwrap().branch(),
        r2.headers.top_via().unwrap().branch(),
        "new transaction, new branch"
    );
    let auth = r2
        .headers
        .get("Authorization")
        .expect("Authorization header");
    let f = digest_fields(auth);
    assert_eq!(field(&f, "username"), USER);
    assert_eq!(field(&f, "realm"), REALM);
    assert_eq!(field(&f, "nonce"), NONCE);
    assert_eq!(field(&f, "uri"), REG_URI);
    assert_eq!(field(&f, "algorithm"), "MD5");
    // Independent recomputation from the known password (RFC 2069 form, no qop):
    let ha1 = md5_hex(&format!("{USER}:{REALM}:{PASS}"));
    let ha2 = md5_hex(&format!("REGISTER:{REG_URI}"));
    let expected = md5_hex(&format!("{ha1}:{NONCE}:{ha2}"));
    assert_eq!(
        ha1, "466add6916c7786b6569c696a9cbdb90",
        "matches the OS md5 tool"
    );
    assert_eq!(
        ha2, "f1ed17dd745d5b3c6faf41acbcf26d64",
        "matches the OS md5 tool"
    );
    assert_eq!(
        expected, "795bff5639daac6742a560f042f068ca",
        "matches the OS md5 tool"
    );
    assert_eq!(field(&f, "response"), expected);
    println!("C03 expected digest response (independent): {expected}");

    // --- 200 with a 3600 s binding
    fake.reply(
        &r2,
        200,
        &[("Contact", "<sip:fax@192.0.2.10:5060>;expires=3600".into())],
    )
    .await;
    let t_ok = Instant::now();
    let s = wait_state(&mut st, |s| {
        matches!(s, RegistrationState::Registered { .. })
    })
    .await;
    let RegistrationState::Registered {
        granted,
        refresh_at,
    } = s
    else {
        unreachable!()
    };
    assert_eq!(granted, Duration::from_secs(3600));
    assert_eq!(refresh_at - t_ok, Duration::from_secs(3240));

    // --- refresh exactly 10 % (360 s) before expiry
    let (t3, r3) = fake.next().await;
    assert_eq!(
        Instant::now() - t_ok,
        Duration::from_secs(3240),
        "refresh at 90 % of 3600 s"
    );
    println!(
        "C03 refresh REGISTER sent {} ms after the 200 (expected 3240000)",
        t3 - (t_ok - fake.t0).as_millis() as u64
    );
    assert_eq!(r3.headers.cseq().unwrap().seq, 3);
    assert_eq!(r3.headers.call_id().unwrap(), call_id);
    assert!(
        r3.headers.get("Authorization").is_none(),
        "each refresh starts with a fresh challenge"
    );

    // --- second round: short lifetime uses the 30 s floor: 200 s -> refresh after 170 s
    fake.challenge(&r3, false, false).await;
    let (_, r4) = fake.next().await;
    assert!(r4.headers.get("Authorization").is_some());
    fake.reply(
        &r4,
        200,
        &[("Contact", "<sip:fax@192.0.2.10:5060>;expires=200".into())],
    )
    .await;
    let t_ok2 = Instant::now();
    let (_, r5) = fake.next().await;
    assert_eq!(
        Instant::now() - t_ok2,
        Duration::from_secs(170),
        "max(10 %, 30 s) = 30 s before a 200 s expiry"
    );
    assert_eq!(r5.headers.cseq().unwrap().seq, 5);
}

#[tokio::test(start_paused = true)]
async fn c03_qop_auth_and_proxy_407_digest_are_computed_correctly() {
    for proxy in [false, true] {
        let (mut fake, handle, _layer) = setup(config());
        let mut st = handle.subscribe();
        let (_, r1) = fake.next().await;
        fake.challenge(&r1, true, proxy).await;
        let (_, r2) = fake.next().await;
        let hdr = if proxy {
            "Proxy-Authorization"
        } else {
            "Authorization"
        };
        assert!(
            r2.headers
                .get(if proxy {
                    "Authorization"
                } else {
                    "Proxy-Authorization"
                })
                .is_none()
        );
        let f = digest_fields(r2.headers.get(hdr).unwrap_or_else(|| panic!("no {hdr}")));
        assert_eq!(field(&f, "qop"), "auth");
        assert_eq!(field(&f, "nc"), "00000001");
        assert_eq!(field(&f, "opaque"), "op4que", "opaque echoed");
        let cnonce = field(&f, "cnonce");
        assert!(!cnonce.is_empty());
        let ha1 = md5_hex(&format!("{USER}:{REALM}:{PASS}"));
        let ha2 = md5_hex(&format!("REGISTER:{REG_URI}"));
        let expected = md5_hex(&format!("{ha1}:{NONCE}:00000001:{cnonce}:auth:{ha2}"));
        assert_eq!(field(&f, "response"), expected, "proxy={proxy}");
        fake.reply(&r2, 200, &[("Expires", "600".into())]).await;
        let s = wait_state(&mut st, |s| {
            matches!(s, RegistrationState::Registered { .. })
        })
        .await;
        let RegistrationState::Registered { granted, .. } = s else {
            unreachable!()
        };
        assert_eq!(
            granted,
            Duration::from_secs(600),
            "lifetime from the Expires header"
        );
    }
}

#[tokio::test(start_paused = true)]
async fn c03_wrong_credentials_second_challenge_is_a_failure_not_a_loop() {
    let (mut fake, handle, _layer) = setup(config());
    let mut st = handle.subscribe();
    let (_, r1) = fake.next().await;
    fake.challenge(&r1, false, false).await;
    let (_, r2) = fake.next().await;
    fake.challenge(&r2, false, false).await; // registrar still unhappy
    let s = wait_state(&mut st, |s| matches!(s, RegistrationState::Failed { .. })).await;
    assert_eq!(
        s,
        RegistrationState::Failed {
            reason: "authentication rejected".into(),
            attempts: 1,
            retry_in: Duration::from_secs(5)
        }
    );
    // exactly two REGISTERs in this attempt, the next one only after the backoff
    let (t, r3) = fake.next().await;
    assert_eq!(t, 5_000);
    assert_eq!(r3.headers.cseq().unwrap().seq, 3);
}

#[tokio::test(start_paused = true)]
async fn c03_stop_deregisters_with_expires_0() {
    let (mut fake, handle, _layer) = setup(config());
    let mut st = handle.subscribe();
    let (_, r1) = fake.next().await;
    fake.reply(
        &r1,
        200,
        &[("Contact", "<sip:fax@192.0.2.10:5060>;expires=3600".into())],
    )
    .await;
    wait_state(&mut st, |s| {
        matches!(s, RegistrationState::Registered { .. })
    })
    .await;
    let stopper = tokio::spawn(async move { handle.stop().await });
    let (_, r2) = fake.next().await;
    assert_eq!(r2.method, Method::Register);
    assert_eq!(r2.headers.get("Expires"), Some("0"));
    assert_eq!(r2.headers.cseq().unwrap().seq, 2);
    fake.reply(&r2, 200, &[]).await;
    stopper.await.unwrap();
    assert_eq!(*st.borrow(), RegistrationState::Stopped);
}

#[test]
fn c03_refresh_and_backoff_arithmetic() {
    let s = Duration::from_secs;
    assert_eq!(refresh_delay(s(3600)), s(3240));
    assert_eq!(refresh_delay(s(300)), s(270), "10 % = 30 s exactly");
    assert_eq!(refresh_delay(s(200)), s(170), "30 s floor");
    assert_eq!(refresh_delay(s(60)), s(30));
    assert_eq!(
        refresh_delay(s(30)),
        s(15),
        "lifetime not above the margin: half"
    );
    assert_eq!(refresh_delay(s(1)), Duration::from_millis(500));
    let c = config();
    let d: Vec<u64> = (1..=8).map(|n| backoff_delay(&c, n).as_secs()).collect();
    assert_eq!(
        d,
        vec![5, 10, 20, 30, 30, 30, 30, 30],
        "5 s doubling, capped at backoff_max"
    );
    assert_eq!(backoff_delay(&c, 0).as_secs(), 5);
    assert_eq!(
        backoff_delay(&c, 4_000_000_000).as_secs(),
        30,
        "no overflow"
    );
}

// ================================================================ C04

#[tokio::test(start_paused = true)]
async fn c04_403_failures_back_off_exponentially_status_is_failed_then_recovers() {
    let (mut fake, handle, _layer) = setup(config());
    let mut st = handle.subscribe();
    let mut observed = vec![];
    let mut last_fail_at = 0u64;
    let mut expected_next = 0u64;
    for attempt in 1..=4u32 {
        let (t, req) = fake.next().await;
        assert_eq!(
            t, expected_next,
            "attempt {attempt} sent at the scheduled instant"
        );
        assert_eq!(req.headers.cseq().unwrap().seq, attempt);
        fake.reply(&req, 403, &[]).await;
        let s = wait_state(
            &mut st,
            |s| matches!(s, RegistrationState::Failed { attempts, .. } if *attempts == attempt),
        )
        .await;
        let RegistrationState::Failed {
            reason,
            attempts,
            retry_in,
        } = s
        else {
            unreachable!()
        };
        assert_eq!(reason, "403 Forbidden");
        assert_eq!(attempts, attempt);
        last_fail_at = (Instant::now() - fake.t0).as_millis() as u64;
        expected_next = last_fail_at + retry_in.as_millis() as u64;
        observed.push(retry_in.as_secs());
    }
    assert_eq!(
        observed,
        vec![5, 10, 20, 30],
        "5, 10, 20, then capped at 30 s"
    );
    println!("C04 403: retry delays {observed:?} s; last failure at {last_fail_at} ms");
    // recovery
    let (t, req) = fake.next().await;
    assert_eq!(t, expected_next);
    fake.reply(
        &req,
        200,
        &[("Contact", "<sip:fax@192.0.2.10:5060>;expires=3600".into())],
    )
    .await;
    let s = wait_state(&mut st, |s| {
        matches!(s, RegistrationState::Registered { .. })
    })
    .await;
    assert!(matches!(s, RegistrationState::Registered { .. }));
    // the failure counter was reset: the next failure starts again at 5 s
    let (_, refresh) = fake.next().await;
    fake.reply(&refresh, 500, &[]).await;
    let s = wait_state(&mut st, |s| matches!(s, RegistrationState::Failed { .. })).await;
    assert_eq!(
        s,
        RegistrationState::Failed {
            reason: "500 Server Internal Error".into(),
            attempts: 1,
            retry_in: Duration::from_secs(5)
        }
    );
}

#[tokio::test(start_paused = true)]
async fn c04_registrar_timeout_fails_after_timer_f_then_retries_and_recovers() {
    let (mut fake, handle, _layer) = setup(config());
    let mut st = handle.subscribe();
    // Silent registrar: collect the REGISTER (re)transmissions until the state turns Failed.
    let mut sent = vec![];
    let failed = loop {
        tokio::select! {
            (t, req) = fake.next() => sent.push((t, req)),
            s = wait_state(&mut st, |s| matches!(s, RegistrationState::Failed { .. })) => break s,
        }
    };
    let times: Vec<u64> = sent.iter().map(|(t, _)| *t).collect();
    assert_eq!(
        times,
        vec![
            0, 500, 1500, 3500, 7500, 11500, 15500, 19500, 23500, 27500, 31500
        ],
        "Timer E retransmissions (capped at T2)"
    );
    assert!(
        sent.iter().all(|(_, r)| r.headers.cseq().unwrap().seq == 1),
        "retransmissions keep the CSeq"
    );
    assert_eq!((Instant::now() - fake.t0).as_millis(), 32_000, "Timer F");
    let RegistrationState::Failed {
        reason,
        attempts,
        retry_in,
    } = failed
    else {
        unreachable!()
    };
    assert!(reason.contains("timeout"), "{reason}");
    assert_eq!((attempts, retry_in), (1, Duration::from_secs(5)));
    println!(
        "C04 timeout: REGISTER at {times:?}, Failed at 32000 ms ({reason}), retry in {retry_in:?}"
    );

    // retry after the backoff with a new CSeq and transaction
    let (t, retry) = fake.next().await;
    assert_eq!(t, 37_000);
    assert_eq!(retry.headers.cseq().unwrap().seq, 2);
    assert_ne!(
        sent[0].1.headers.top_via().unwrap().branch(),
        retry.headers.top_via().unwrap().branch()
    );
    fake.reply(
        &retry,
        200,
        &[("Contact", "<sip:fax@192.0.2.10:5060>;expires=3600".into())],
    )
    .await;
    let s = wait_state(&mut st, |s| {
        matches!(s, RegistrationState::Registered { .. })
    })
    .await;
    assert!(
        matches!(s, RegistrationState::Registered { granted, .. } if granted == Duration::from_secs(3600))
    );
}

#[tokio::test(start_paused = true)]
async fn c04_zero_lifetime_grant_and_garbled_challenge_are_failures() {
    let (mut fake, handle, _layer) = setup(config());
    let mut st = handle.subscribe();
    let (_, r1) = fake.next().await;
    fake.reply(
        &r1,
        200,
        &[("Contact", "<sip:fax@192.0.2.10:5060>;expires=0".into())],
    )
    .await;
    let s = wait_state(&mut st, |s| matches!(s, RegistrationState::Failed { .. })).await;
    let RegistrationState::Failed { reason, .. } = s else {
        unreachable!()
    };
    assert_eq!(reason, "registrar granted a lifetime of 0");
    let (_, r2) = fake.next().await;
    fake.reply(
        &r2,
        401,
        &[("WWW-Authenticate", "Basic realm=\"x\"".into())],
    )
    .await;
    let s = wait_state(&mut st, |s| {
        matches!(s, RegistrationState::Failed { attempts: 2, .. })
    })
    .await;
    let RegistrationState::Failed {
        reason, retry_in, ..
    } = s
    else {
        unreachable!()
    };
    assert!(
        reason.contains("without a usable Digest challenge"),
        "{reason}"
    );
    assert_eq!(retry_in, Duration::from_secs(10));
}
