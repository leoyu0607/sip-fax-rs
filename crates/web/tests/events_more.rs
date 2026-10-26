//! V01 (the events of a fax that is really recorded), V04 (an expired session at the handshake)
//! and V06 (100 connections, one of them slow), with real WebSocket clients.
//!
//! V01 is "end to end" in the test plan: a call received by `sip-faxd` through SpanDSP in Docker.
//! That needs the fax-sim image; here the real `fax_in::recorder::Recorder` writes into the real
//! store with the same hook `apps/sip-faxd/src/web_service.rs::recorder_events` installs, and the
//! events travel through the bus, the server and a WebSocket to the client.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use accounts::Role;
use common::*;
use fax_in::event::{CallEvent, CallOutcome, CallResult, FaxReport, UdptlCounters};
use fax_in::numbers::Numbers;
use fax_in::recorder::{RecordWritten, Recorder, RecorderConfig, StoreSink, WrittenHook};
use futures_util::StreamExt;
use serde_json::{Value, json};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::tungstenite::{Error as WsError, Message};
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};
use web::events::EventBus;

type Ws = WebSocketStream<MaybeTlsStream<TcpStream>>;

async fn token(e: &Env, name: &str) -> String {
    let r = reqwest::Client::new()
        .post(e.url("/api/v1/auth/login"))
        .json(&json!({"username": name, "password": PW}))
        .send()
        .await
        .unwrap();
    r.headers()["set-cookie"]
        .to_str()
        .unwrap()
        .split(';')
        .next()
        .unwrap()
        .split_once('=')
        .unwrap()
        .1
        .to_string()
}

fn request(e: &Env, token: &str, query: &str) -> tokio_tungstenite::tungstenite::http::Request<()> {
    let url = format!(
        "{}/api/v1/events{query}",
        e.base.replacen("http://", "ws://", 1)
    );
    let mut rq = url.into_client_request().unwrap();
    rq.headers_mut()
        .insert("cookie", format!("sipfax_session={token}").parse().unwrap());
    rq
}

async fn connect(e: &Env, token: &str, query: &str) -> Result<Ws, WsError> {
    tokio_tungstenite::connect_async(request(e, token, query))
        .await
        .map(|(s, _)| s)
}

async fn next_json(ws: &mut Ws) -> Value {
    loop {
        let m = tokio::time::timeout(Duration::from_secs(5), ws.next())
            .await
            .expect("a frame within 5 s")
            .expect("open")
            .expect("no error");
        match m {
            Message::Text(t) => return serde_json::from_str(&t).unwrap(),
            Message::Ping(_) | Message::Pong(_) => {}
            other => panic!("unexpected {other:?}"),
        }
    }
}

// ------------------------------------------------------------------------------ V01

const T0: u64 = 1_792_540_800;

fn at(secs: u64) -> SystemTime {
    UNIX_EPOCH + Duration::from_secs(T0 + secs)
}

fn numbers() -> Numbers {
    Numbers {
        ani: Some("0200000001".into()),
        ani_source: Some("from".into()),
        dnis: Some("0200000002".into()),
        dnis_source: Some("ruri".into()),
    }
}

fn three_page_result(call: &str) -> CallResult {
    CallResult {
        call_id: call.into(),
        trunk: "audiocodes".into(),
        peer: "192.0.2.10:5060".into(),
        from: "sip:0200000001@192.0.2.10".into(),
        to: "sip:0200000002@198.51.100.20".into(),
        numbers: numbers(),
        started_at: at(0),
        answered_at: Some(at(1)),
        t38_established_at: Some(at(1)),
        ended_at: at(64),
        bye_by: Some(fax_in::event::ByeBy::Local),
        t38: None,
        audio: None,
        fax: Some(FaxReport {
            code: 0,
            code_name: "ok".into(),
            pages: 3,
            bit_rate: 14_400,
            modem: Some("v17".into()),
            resolution: Some("fine".into()),
            compression: Some("mmr".into()),
            remote_ident: Some("0212345678".into()),
            bad_lines_per_page: vec![0; 3],
            duration_ms: 31_000,
            partial: false,
            tiff: Some("/srv/fax/data/faxes/2026/10/21/c1.tif".into()),
            pdf: Some("/srv/fax/data/faxes/2026/10/21/c1.pdf".into()),
            store_error: None,
            udptl: UdptlCounters::default(),
            media: Default::default(),
            rtp: None,
            tiff_size: Some(12_345),
            pdf_size: Some(23_456),
            ecm: true,
            ecm_ppr_sent: 0,
            ecm_frames_lost: 0,
        }),
        outcome: CallOutcome::Completed,
    }
}

/// What `web_service::recorder_events` does with the recorder's writes.
fn recorder_events(bus: Arc<EventBus>) -> WrittenHook {
    Arc::new(move |w| match w {
        RecordWritten::Created { id, call_id } => {
            bus.fax_created(*id, call_id);
        }
        RecordWritten::Pages { id, call_id, pages } => {
            bus.fax_progress(*id, call_id, *pages);
        }
        RecordWritten::Finalized { id, call_id } => {
            bus.fax_finalized(*id, call_id);
        }
    })
}

#[tokio::test(flavor = "multi_thread")]
async fn v01_a_three_page_fax_gives_created_three_progress_and_finalized_with_consecutive_seq() {
    let e = env().await;
    add_user(&e, "amy", Role::Agent, false).await;
    let t = token(&e, "amy").await;
    let mut ws = connect(&e, &t, "").await.unwrap();
    assert_eq!(next_json(&mut ws).await["type"], "hello");

    let mut cfg = RecorderConfig::new("/srv/fax/data".into());
    cfg.on_written = Some(recorder_events(e.events.clone()));
    let rec = Recorder::start(Arc::new(StoreSink(e.store.clone())), cfg);
    let call = "v01@gw";
    rec.record(&CallEvent::Started {
        call_id: call.into(),
        trunk: "audiocodes".into(),
        peer: "192.0.2.10:5060".into(),
        numbers: numbers(),
        at: at(0),
    });
    // the page events come while the call goes on: the client sees them as they are written
    let mut got = vec![next_json(&mut ws).await];
    for pages in 1..=3u32 {
        rec.record(&CallEvent::PageReceived {
            call_id: call.into(),
            pages,
            at: at(10 * u64::from(pages)),
        });
        got.push(next_json(&mut ws).await);
    }
    rec.record(&CallEvent::Finished(Box::new(three_page_result(call))));
    got.push(next_json(&mut ws).await);

    let kinds: Vec<&str> = got.iter().map(|g| g["type"].as_str().unwrap()).collect();
    assert_eq!(
        kinds,
        [
            "fax.created",
            "fax.progress",
            "fax.progress",
            "fax.progress",
            "fax.finalized"
        ]
    );
    let seqs: Vec<u64> = got.iter().map(|g| g["seq"].as_u64().unwrap()).collect();
    assert_eq!(seqs, [1, 2, 3, 4, 5], "consecutive");
    let pages: Vec<u64> = got[1..4]
        .iter()
        .map(|g| g["data"]["pages"].as_u64().unwrap())
        .collect();
    assert_eq!(pages, [1, 2, 3]);
    // every event names the row that is in the database by now
    let row = e.store.get_by_call_id(call).await.unwrap().unwrap();
    for g in &got {
        assert_eq!(g["data"]["id"], row.id, "{g}");
        assert_eq!(g["data"]["call_id"], call);
    }
    assert_eq!(row.pages, 3);
    assert_eq!(row.fax_status, store::FaxStatus::Received);
    // and what the client is told to fetch next is really there
    let (c, _) = login(&e, "amy").await;
    let v: Value = c
        .get(e.url(&format!("/api/v1/faxes/{}", row.id)))
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    assert_eq!(
        (v["pages"].as_i64(), v["fax_status"].as_str()),
        (Some(3), Some("received"))
    );
    // a client that joins after the fax gets the same five with since=0
    let mut late = connect(&e, &t, "?since=0").await.unwrap();
    assert_eq!(next_json(&mut late).await["seq"], 5);
    let mut replay = Vec::new();
    for _ in 0..5 {
        replay.push(next_json(&mut late).await);
    }
    assert_eq!(replay, got, "a reconnecting client is told the same, once");
    rec.shutdown(Duration::from_secs(5)).await;
    e.stop().await;
}

// ------------------------------------------------------------------------------ V04

#[tokio::test(flavor = "multi_thread")]
async fn v04_a_handshake_with_an_expired_ended_or_missing_session_is_401() {
    let e = env().await;
    add_user(&e, "amy", Role::Agent, false).await;
    let status = |r: Result<Ws, WsError>| match r {
        Err(WsError::Http(resp)) => resp.status().as_u16(),
        other => panic!("expected an HTTP refusal: {:?}", other.map(|_| ())),
    };
    let t = token(&e, "amy").await;
    assert!(connect(&e, &t, "").await.is_ok());
    // idle for more than 8 hours
    e.clock
        .advance(chrono::Duration::hours(8) + chrono::Duration::seconds(1));
    assert_eq!(status(connect(&e, &t, "").await), 401);
    // an ended session (logout) and one that never existed
    let t2 = token(&e, "amy").await;
    assert!(connect(&e, &t2, "").await.is_ok());
    e.auth.logout(&t2).await.unwrap();
    assert_eq!(status(connect(&e, &t2, "").await), 401);
    assert_eq!(status(connect(&e, "0123456789abcdef", "").await), 401);
    // the same request as a plain GET says why
    let r = reqwest::Client::new()
        .get(e.url("/api/v1/events"))
        .header("cookie", format!("sipfax_session={t}"))
        .send()
        .await
        .unwrap();
    assert_eq!(r.status(), 401);
    let v: Value = r.json().await.unwrap();
    assert!(
        ["unauthenticated", "session_expired"].contains(&v["error"]["code"].as_str().unwrap()),
        "{v}"
    );
    e.stop().await;
}

// ------------------------------------------------------------------------------ V06

const CLIENTS: usize = 100;
const EVENTS: usize = 2500;
const PAD: usize = 8 * 1024;

/// The sequence number of an event frame (`"seq"` is the first such key: the JSON keys are
/// sorted, and `data` holds no `seq`).
fn seq_of(text: &str) -> Option<u64> {
    let i = text.find("\"seq\":")? + 6;
    let digits: String = text[i..].chars().take_while(char::is_ascii_digit).collect();
    digits.parse().ok()
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn v06_100_connections_all_get_every_event_and_the_one_that_does_not_read_is_cut_off() {
    let e = env().await;
    add_user(&e, "amy", Role::Agent, false).await;
    let t = token(&e, "amy").await;

    // 99 clients that read, and one whose receive buffer is tiny and that never reads
    let received: Vec<Arc<AtomicUsize>> = (0..CLIENTS - 1)
        .map(|_| Arc::new(AtomicUsize::new(0)))
        .collect();
    let broken = Arc::new(AtomicBool::new(false));
    let mut readers = Vec::new();
    for counter in &received {
        let mut ws = connect(&e, &t, "").await.unwrap();
        let hello = next_json(&mut ws).await;
        assert_eq!(hello["seq"], 0);
        let (counter, broken) = (counter.clone(), broken.clone());
        readers.push(tokio::spawn(async move {
            let mut expect = 1u64;
            while let Some(m) = ws.next().await {
                match m {
                    Ok(Message::Text(t)) => {
                        // every connection sees 1, 2, 3, ...: none missing, none twice
                        if seq_of(&t) != Some(expect) {
                            broken.store(true, Ordering::SeqCst);
                            return (
                                expect - 1,
                                Some(format!("seq {:?} for {expect}", seq_of(&t))),
                            );
                        }
                        expect += 1;
                        counter.store(expect as usize - 1, Ordering::SeqCst);
                        if expect as usize > EVENTS {
                            return (expect - 1, None);
                        }
                    }
                    Ok(Message::Ping(_) | Message::Pong(_)) => {}
                    Ok(other) => return (expect - 1, Some(format!("{other:?}"))),
                    Err(err) => return (expect - 1, Some(err.to_string())),
                }
            }
            (expect - 1, Some("closed".into()))
        }));
    }
    let slow_sock = tokio::net::TcpSocket::new_v4().unwrap();
    slow_sock.set_recv_buffer_size(4096).unwrap();
    let addr: std::net::SocketAddr = e.base.trim_start_matches("http://").parse().unwrap();
    let stream = slow_sock.connect(addr).await.unwrap();
    let (mut slow, _) = tokio_tungstenite::client_async(request(&e, &t, ""), stream)
        .await
        .unwrap();
    let hello = tokio::time::timeout(Duration::from_secs(5), slow.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(hello.to_text().unwrap().contains("hello"));

    // publish in batches that stay under what a connection may be behind, and wait for the
    // readers between them: the slow one is the only one that is ever behind
    let pad = "x".repeat(PAD);
    let started = std::time::Instant::now();
    let mut published = 0usize;
    while published < EVENTS {
        for _ in 0..100 {
            e.events
                .publish("load", json!({"n": published, "pad": pad}));
            published += 1;
        }
        let deadline = tokio::time::Instant::now() + Duration::from_secs(60);
        while received
            .iter()
            .any(|c| c.load(Ordering::SeqCst) < published)
        {
            assert!(!broken.load(Ordering::SeqCst), "a reader saw a gap");
            assert!(
                tokio::time::Instant::now() < deadline,
                "the readers stopped at {:?}",
                received.iter().map(|c| c.load(Ordering::SeqCst)).min()
            );
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    }
    let healthy_took = started.elapsed();
    for r in readers {
        let (n, why) = r.await.unwrap();
        assert_eq!((n as usize, why), (EVENTS, None));
    }

    // the one that did not read: the server cut it off (it cannot say why: the buffers are
    // full), so what it can still read ends before the last event
    let mut frames = 0usize;
    let ended = tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            match slow.next().await {
                Some(Ok(Message::Text(_))) => frames += 1,
                Some(Ok(_)) => {}
                Some(Err(_)) | None => break,
            }
        }
    })
    .await;
    assert!(ended.is_ok(), "the slow connection was cut off");
    assert!(
        frames < EVENTS,
        "it got {frames} of {EVENTS} events: the buffers did not fill"
    );
    // the server is as it was for everyone else
    let mut fresh = connect(&e, &t, "?since=2490").await.unwrap();
    assert_eq!(next_json(&mut fresh).await["seq"], EVENTS as u64);
    assert_eq!(next_json(&mut fresh).await["seq"], 2491);
    eprintln!("v06: 99 readers took {healthy_took:?}, the slow one saw {frames} events");
    e.stop().await;
}
