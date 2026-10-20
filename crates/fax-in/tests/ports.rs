//! M2a: `[media] port_range` at the controller level (E07 without the real media path, which
//! arrives with the T.38 session in M2d): per-call even-port allocation, release, and 503 when
//! the range is exhausted. Deterministic (detached endpoint, paused time).

mod common;

use common::*;
use fax_in::{CallOutcome, FailureReason};
use sip_core::message::SipMessage;

fn config(range: &str) -> String {
    format!("[media]\nport_range = \"{range}\"\n\n{}", base_config(""))
}

fn image_port_of_answer(w: &W, call_id: &str) -> u16 {
    let (_, r) = w
        .responses(200, "INVITE")
        .into_iter()
        .find(|(_, r)| r.headers.call_id() == Some(call_id))
        .unwrap_or_else(|| panic!("no 200 for {call_id}: {:?}", w.status_lines()));
    let body = String::from_utf8(r.body.to_vec()).unwrap();
    let line = body
        .lines()
        .find(|l| l.starts_with("m=image "))
        .unwrap_or_else(|| panic!("no m=image in:\n{body}"));
    line.split_whitespace().nth(1).unwrap().parse().unwrap()
}

fn tag_of_answer(w: &W, call_id: &str) -> String {
    w.responses(200, "INVITE")
        .into_iter()
        .find(|(_, r)| r.headers.call_id() == Some(call_id))
        .map(|(_, r)| r.headers.to().unwrap().tag().unwrap().to_string())
        .unwrap()
}

async fn invite(w: &W, call_id: &str) {
    w.feed(Req::invite(call_id, T38_SDP).build()).await;
}

async fn ack(w: &W, call_id: &str) {
    let tag = tag_of_answer(w, call_id);
    w.feed(Req::in_dialog("ACK", call_id, "ack", 1, &tag).build())
        .await;
}

async fn bye(w: &W, call_id: &str) {
    let tag = tag_of_answer(w, call_id);
    w.feed(Req::in_dialog("BYE", call_id, "bye", 2, &tag).build())
        .await;
}

fn answered_503(w: &W, call_id: &str) -> bool {
    w.responses(503, "INVITE")
        .iter()
        .any(|(_, r)| r.headers.call_id() == Some(call_id))
}

#[tokio::test(start_paused = true)]
async fn e07_exhausted_range_answers_503_and_a_released_port_is_reused() {
    // two usable (even) ports: 30000 and 30002
    let mut w = W::new(&config("30000-30003")).await;
    invite(&w, "p1@gw").await;
    ack(&w, "p1@gw").await;
    invite(&w, "p2@gw").await;
    ack(&w, "p2@gw").await;
    assert_eq!(image_port_of_answer(&w, "p1@gw"), 30000);
    assert_eq!(image_port_of_answer(&w, "p2@gw"), 30002);

    // the third call finds no port: 503, no 180, no 200
    invite(&w, "p3@gw").await;
    assert!(answered_503(&w, "p3@gw"), "{:?}", w.status_lines());
    assert!(
        w.responses(180, "INVITE")
            .iter()
            .all(|(_, r)| r.headers.call_id() != Some("p3@gw"))
    );
    assert!(
        w.responses(200, "INVITE")
            .iter()
            .all(|(_, r)| r.headers.call_id() != Some("p3@gw"))
    );
    let r3 = w
        .results()
        .into_iter()
        .find(|r| r.call_id == "p3@gw")
        .expect("the refused call still produces a result");
    assert_eq!(
        r3.outcome,
        CallOutcome::Failed {
            reason: FailureReason::NoMediaPort
        }
    );
    assert!(r3.answered_at.is_none() && r3.t38.is_none());

    // while the range is full, even a retry is refused
    w.feed(
        Req {
            branch: branch_for("p3@gw", "retry"),
            ..Req::invite("p3b@gw", T38_SDP)
        }
        .build(),
    )
    .await;
    assert!(answered_503(&w, "p3b@gw"));

    // p1 hangs up: its port is free again and the next call receives exactly that port
    bye(&w, "p1@gw").await;
    let r1 = w
        .results()
        .into_iter()
        .find(|r| r.call_id == "p1@gw")
        .unwrap();
    assert_eq!(r1.outcome, CallOutcome::Completed);
    invite(&w, "p4@gw").await;
    assert_eq!(image_port_of_answer(&w, "p4@gw"), 30000);
    ack(&w, "p4@gw").await;

    // and the range is full again
    invite(&w, "p5@gw").await;
    assert!(answered_503(&w, "p5@gw"));

    // p2 ends too; its port (30002) goes to the next call
    bye(&w, "p2@gw").await;
    invite(&w, "p6@gw").await;
    assert_eq!(image_port_of_answer(&w, "p6@gw"), 30002);

    // the ports in use are always distinct
    assert_ne!(
        image_port_of_answer(&w, "p4@gw"),
        image_port_of_answer(&w, "p6@gw")
    );
    w.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_call_that_is_refused_for_other_reasons_does_not_keep_its_port() {
    let mut w = W::new(&config("30000-30001")).await; // a single usable port
    // an INVITE without SDP is refused (488) before a port is taken
    let r = Req {
        body: None,
        ..Req::invite("n1@gw", T38_SDP)
    }
    .build();
    w.feed_from(SipMessage::Request(r), GW).await;
    assert!(
        w.status_lines().iter().any(|(_, l)| l.contains("488")),
        "{:?}",
        w.status_lines()
    );
    // the port was not consumed by the rejected call
    invite(&w, "n2@gw").await;
    assert_eq!(image_port_of_answer(&w, "n2@gw"), 30000);
    w.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn the_leased_port_is_used_in_our_own_t38_reinvite() {
    // audio-only INVITE; the default `auto` policy sends its own T.38 re-INVITE 3 s after the
    // ACK. Its m=image line must carry the port leased for the call (30002 here: the first call
    // holds 30000).
    let mut w = W::new(&config("30000-30003")).await;
    invite(&w, "q1@gw").await; // T.38 call, holds 30000
    ack(&w, "q1@gw").await;
    w.feed(Req::invite("q2@gw", AUDIO_SDP).build()).await;
    w.at(10).await;
    ack(&w, "q2@gw").await;
    w.at(3_100).await;
    let reinvites = w.requests("INVITE");
    let (_, req) = reinvites
        .iter()
        .find(|(_, r)| r.headers.call_id() == Some("q2@gw"))
        .expect("re-INVITE for q2");
    let body = String::from_utf8(req.body.to_vec()).unwrap();
    assert!(body.contains("m=image 30002 udptl t38"), "{body}");
    w.shutdown().await;
}
