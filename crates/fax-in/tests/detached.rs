//! fax-in call control, deterministic: C02, C06-C10 and the failure paths. The controller
//! runs on a detached sip-core endpoint under `tokio::time::pause()`; the test is the
//! upstream gateway.

mod common;

use common::*;
use fax_in::{CallEvent, CallOutcome, FailureReason, T38Path};
use sip_core::message::SipMessage;

fn t38_events(w: &mut W) -> Vec<(String, T38Path)> {
    w.events()
        .into_iter()
        .filter_map(|e| match e {
            CallEvent::T38Established {
                call_id, params, ..
            } => Some((call_id, params.path)),
            _ => None,
        })
        .collect()
}

fn media_lines(sdp: &str) -> Vec<String> {
    sdp.lines()
        .filter(|l| l.starts_with("m="))
        .map(str::to_string)
        .collect()
}

fn answer_body(w: &W) -> String {
    let (_, r) = w.responses(200, "INVITE").remove(0);
    String::from_utf8(r.body.to_vec()).unwrap()
}

// ================================================================ C02

#[tokio::test(start_paused = true)]
async fn c02_each_call_is_bound_to_the_trunk_of_its_source_address() {
    let mut w = W::new(
        "[sip]\nbind = \"192.0.2.10:5060\"\nadvertised = \"192.0.2.10:5060\"\n\
         [[trunks]]\nname = \"alpha\"\nallowed_sources = [\"10.0.0.0/24\"]\n\
         [[trunks]]\nname = \"beta\"\nallowed_sources = [\"10.0.1.0/24\", \"172.16.5.5\"]\nt38_policy = \"wait-peer\"\n",
    )
    .await;
    // two calls at the same time from different trunks' address ranges
    let a = Req::invite("call-a@gw", AUDIO_SDP).build();
    w.feed_from(SipMessage::Request(a), "10.0.0.9:5060").await;
    let b = Req::invite("call-b@gw", AUDIO_SDP).build();
    w.feed_from(SipMessage::Request(b), "10.0.1.7:5060").await;
    let c = Req::invite("call-c@gw", AUDIO_SDP).build();
    w.feed_from(SipMessage::Request(c), "172.16.5.5:5060").await;
    // ACKs so that the policies start their clocks
    for (id, src) in [
        ("call-a@gw", "10.0.0.9:5060"),
        ("call-b@gw", "10.0.1.7:5060"),
        ("call-c@gw", "172.16.5.5:5060"),
    ] {
        let tag = w
            .responses(200, "INVITE")
            .into_iter()
            .find(|(_, r)| r.headers.call_id() == Some(id))
            .map(|(_, r)| r.headers.to().unwrap().tag().unwrap().to_string())
            .unwrap();
        w.feed_from(
            SipMessage::Request(Req::in_dialog("ACK", id, "ack", 1, &tag).build()),
            src,
        )
        .await;
    }
    let started: Vec<(String, String, String)> = w
        .events()
        .into_iter()
        .filter_map(|e| match e {
            CallEvent::Started {
                call_id,
                trunk,
                peer,
                ..
            } => Some((call_id, trunk, peer)),
            _ => None,
        })
        .collect();
    assert_eq!(
        started,
        vec![
            ("call-a@gw".into(), "alpha".into(), "10.0.0.9:5060".into()),
            ("call-b@gw".into(), "beta".into(), "10.0.1.7:5060".into()),
            ("call-c@gw".into(), "beta".into(), "172.16.5.5:5060".into()),
        ]
    );
    // the profiles really differ: alpha (auto) sends a re-INVITE after 3 s, beta (wait-peer) never
    w.at(3_100).await;
    // (the unanswered INVITE is retransmitted by Timer A, so look at the Call-IDs)
    let mut ids: Vec<String> = w
        .requests("INVITE")
        .iter()
        .map(|(_, r)| r.headers.call_id().unwrap().to_string())
        .collect();
    ids.dedup();
    assert_eq!(
        ids,
        vec!["call-a@gw".to_string()],
        "only the auto trunk initiates"
    );
    assert_eq!(w.requests("INVITE")[0].0, 3_000);
    w.at(20_000).await;
    assert!(
        w.requests("INVITE")
            .iter()
            .all(|(_, r)| r.headers.call_id() == Some("call-a@gw"))
    );
    // and the results carry the trunk
    for (id, src) in [
        ("call-a@gw", "10.0.0.9:5060"),
        ("call-b@gw", "10.0.1.7:5060"),
        ("call-c@gw", "172.16.5.5:5060"),
    ] {
        let tag = w
            .responses(200, "INVITE")
            .into_iter()
            .find(|(_, r)| r.headers.call_id() == Some(id))
            .map(|(_, r)| r.headers.to().unwrap().tag().unwrap().to_string())
            .unwrap();
        w.feed_from(
            SipMessage::Request(Req::in_dialog("BYE", id, "bye", 2, &tag).build()),
            src,
        )
        .await;
    }
    let mut got: Vec<(String, String)> = w
        .results()
        .into_iter()
        .map(|r| (r.call_id, r.trunk))
        .collect();
    got.sort();
    assert_eq!(
        got,
        vec![
            ("call-a@gw".to_string(), "alpha".to_string()),
            ("call-b@gw".to_string(), "beta".to_string()),
            ("call-c@gw".to_string(), "beta".to_string()),
        ]
    );
    w.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn c02_unknown_source_is_rejected_with_403_or_handled_by_the_generic_profile() {
    // reject / drop: the controller itself answers 403 (the transport normally drops earlier)
    let mut w = W::new(&format!("{}\n", base_config(""))).await;
    w.feed_from(
        SipMessage::Request(Req::invite("scan@x", AUDIO_SDP).build()),
        "198.51.100.7:5060",
    )
    .await;
    let forbidden = w.responses(403, "INVITE");
    assert_eq!(forbidden.len(), 1, "{:?}", w.status_lines());
    assert_eq!(forbidden[0].1.headers.call_id(), Some("scan@x"));
    assert!(w.responses(200, "INVITE").is_empty());
    let r = w.results();
    assert_eq!(r.len(), 1);
    assert_eq!(r[0].trunk, "unknown");
    assert_eq!(
        r[0].outcome,
        CallOutcome::Failed {
            reason: FailureReason::UnknownSource
        }
    );
    w.shutdown().await;

    // log: processed with the generic profile, trunk name "unknown"
    let mut w = W::new(
        "[sip]\nbind = \"192.0.2.10:5060\"\nadvertised = \"192.0.2.10\"\nunknown_source = \"log\"\n\
         [[trunks]]\nname = \"gw\"\nallowed_sources = [\"10.0.0.0/24\"]\n",
    )
    .await;
    w.feed_from(
        SipMessage::Request(Req::invite("first-call@x", AUDIO_SDP).build()),
        "198.51.100.7:5060",
    )
    .await;
    assert_eq!(w.responses(200, "INVITE").len(), 1);
    let started = w.events().into_iter().find_map(|e| match e {
        CallEvent::Started { trunk, .. } => Some(trunk),
        _ => None,
    });
    assert_eq!(started.as_deref(), Some("unknown"));
    w.shutdown().await;
}

// ================================================================ C06

#[tokio::test(start_paused = true)]
async fn c06_t38_in_the_initial_invite_is_answered_with_t38_and_no_reinvite_follows() {
    let mut w = W::new(&base_config("")).await;
    let tag = w.call("c06@gw", T38_SDP, Some(10)).await;
    let (t, ok) = w.responses(200, "INVITE").remove(0);
    assert_eq!(t, 0);
    let body = String::from_utf8(ok.body.to_vec()).unwrap();
    assert_eq!(
        media_lines(&body),
        vec!["m=image 30000 udptl t38"],
        "{body}"
    );
    for attr in [
        "a=T38FaxVersion:0",
        "a=T38MaxBitRate:14400",
        "a=T38FaxRateManagement:transferredTCF",
        "a=T38FaxMaxBuffer:1800",
        "a=T38FaxMaxDatagram:260",
        "a=T38FaxUdpEC:t38UDPRedundancy",
    ] {
        assert!(
            body.lines().any(|l| l == attr),
            "missing {attr} in:\n{body}"
        );
    }
    assert!(body.contains("c=IN IP4 192.0.2.10\r\n"), "{body}");
    assert_eq!(ok.headers.get("Content-Type"), Some("application/sdp"));
    // 180 came first (same instant), then 200
    let lines = w.status_lines();
    assert_eq!(lines[0].1, "SIP/2.0 180 Ringing");
    assert_eq!(lines[1].1, "SIP/2.0 200 OK");
    // five minutes of virtual time: no re-INVITE, no BYE
    w.at(300_000).await;
    assert!(
        w.requests("INVITE").is_empty(),
        "no re-INVITE: {:?}",
        w.status_lines()
    );
    assert!(w.requests("BYE").is_empty());
    let ev = w.events();
    assert!(
        ev.iter()
            .any(|e| matches!(e, CallEvent::Answered { t38: true, .. }))
    );
    assert_eq!(
        t38_events(&mut w),
        vec![("c06@gw".to_string(), T38Path::InitialInvite)]
    );
    // hangs up: Completed, with the negotiated parameters
    w.feed(Req::in_dialog("BYE", "c06@gw", "bye", 2, &tag).build())
        .await;
    let r = w.results().remove(0);
    assert_eq!(r.outcome, CallOutcome::Completed);
    let p = r.t38.expect("t38 params");
    assert_eq!(
        (
            p.path,
            p.version,
            p.max_bit_rate,
            p.rate_management.as_str(),
            p.local_max_datagram,
            p.peer_max_datagram,
            p.udp_ec.as_str()
        ),
        (
            T38Path::InitialInvite,
            0,
            14400,
            "transferredTCF",
            260,
            200,
            "t38UDPRedundancy"
        )
    );
    assert_eq!(p.peer_media.as_deref(), Some("203.0.113.9:5004"));
    assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    assert!(r.answered_at.is_some() && r.t38_established_at.is_some());
    w.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn c06_audio_only_policy_rejects_a_t38_only_invite_with_488() {
    let mut w = W::new(&base_config("t38_policy = \"audio-only\"\n")).await;
    w.feed(Req::invite("c06b@gw", T38_SDP).build()).await;
    assert_eq!(
        w.responses(488, "INVITE").len(),
        1,
        "{:?}",
        w.status_lines()
    );
    assert!(w.responses(200, "INVITE").is_empty());
    let r = w.results().remove(0);
    match r.outcome {
        CallOutcome::Failed {
            reason: FailureReason::NoAcceptableMedia { detail },
        } => {
            assert!(detail.contains("audio-only"), "{detail}");
        }
        other => panic!("{other:?}"),
    }
    w.shutdown().await;
}

// ================================================================ C07

#[tokio::test(start_paused = true)]
async fn c07_audio_plus_image_offer_picks_one_line_by_policy_and_zeroes_the_other() {
    // auto: T.38 wins, the audio line is kept with port 0
    let mut w = W::new(&base_config("")).await;
    w.feed(Req::invite("c07a@gw", AUDIO_AND_T38_SDP).build())
        .await;
    let body = answer_body(&w);
    assert_eq!(
        media_lines(&body),
        vec!["m=audio 0 RTP/AVP 0 8", "m=image 30000 udptl t38"],
        "{body}"
    );
    assert_eq!(t38_events(&mut w).len(), 1);
    w.shutdown().await;

    // audio-only: audio wins, the image line is kept with port 0
    let mut w = W::new(&base_config("t38_policy = \"audio-only\"\n")).await;
    w.feed(Req::invite("c07b@gw", AUDIO_AND_T38_SDP).build())
        .await;
    let body = answer_body(&w);
    assert_eq!(
        media_lines(&body),
        vec!["m=audio 20000 RTP/AVP 0 8", "m=image 0 udptl t38"],
        "{body}"
    );
    assert!(t38_events(&mut w).is_empty());
    assert!(body.contains("a=rtpmap:0 PCMU/8000"), "{body}");
    w.shutdown().await;

    // image offered first, audio second: order of m-lines is preserved
    let swapped = "v=0\r\no=gw 1 1 IN IP4 203.0.113.9\r\ns=-\r\nc=IN IP4 203.0.113.9\r\nt=0 0\r\nm=image 5004 udptl t38\r\na=T38FaxVersion:0\r\nm=audio 16384 RTP/AVP 0\r\na=rtpmap:0 PCMU/8000\r\n";
    let mut w = W::new(&base_config("")).await;
    w.feed(Req::invite("c07c@gw", swapped).build()).await;
    assert_eq!(
        media_lines(&answer_body(&w)),
        vec!["m=image 30000 udptl t38", "m=audio 0 RTP/AVP 0"]
    );
    w.shutdown().await;
}

// ================================================================ C08

fn reinvite_text(w: &W, call_id: &str) -> (u64, sip_core::message::Request) {
    let _ = call_id;
    w.requests("INVITE").remove(0)
}

#[tokio::test(start_paused = true)]
async fn c08_auto_sends_its_own_t38_reinvite_3s_after_the_ack() {
    let mut w = W::new(&base_config("")).await;
    let tag = w.call("c08a@gw", AUDIO_SDP, Some(40)).await;
    // 200 carries the audio answer
    let body = answer_body(&w);
    assert_eq!(
        media_lines(&body),
        vec!["m=audio 20000 RTP/AVP 0 8 101"],
        "{body}"
    );
    w.at(3_039).await;
    assert!(w.requests("INVITE").is_empty(), "not before ACK + 3 s");
    w.at(3_040).await;
    let (t, inv) = reinvite_text(&w, "c08a@gw");
    assert_eq!(t, 3_040, "ACK at 40 ms + initiate_after_secs 3");
    // request construction: in-dialog, to the gateway's Contact
    assert_eq!(inv.uri.to_string(), "sip:gw@10.0.0.1:5070");
    assert_eq!(inv.headers.to().unwrap().tag(), Some(GW_TAG));
    assert_eq!(inv.headers.from().unwrap().tag(), Some(tag.as_str()));
    let off = String::from_utf8(inv.body.to_vec()).unwrap();
    assert_eq!(media_lines(&off), vec!["m=image 30000 udptl t38"], "{off}");
    for attr in [
        "a=T38FaxVersion:0",
        "a=T38MaxBitRate:14400",
        "a=T38FaxRateManagement:transferredTCF",
        "a=T38FaxMaxBuffer:1800",
        "a=T38FaxMaxDatagram:260",
        "a=T38FaxUdpEC:t38UDPRedundancy",
    ] {
        assert!(off.lines().any(|l| l == attr), "missing {attr}:\n{off}");
    }
    // o= version moved on from the audio answer
    let a_ver = sdp(&body).origin.sess_version;
    let o_ver = sdp(&off).origin.sess_version;
    assert_eq!(o_ver, a_ver + 1);
    assert_eq!(sdp(&body).origin.sess_id, sdp(&off).origin.sess_id);
    // gateway accepts with a T.38 answer: we ACK and the session starts
    w.at(3_100).await;
    w.feed_resp(resp_to(&inv, 200, GW_TAG, Some(T38_SDP))).await;
    assert_eq!(w.requests("ACK").len(), 1, "ACK for the re-INVITE's 200");
    assert_eq!(
        t38_events(&mut w),
        vec![("c08a@gw".into(), T38Path::OurReinvite)]
    );
    // the gateway hangs up: Completed
    w.feed(Req::in_dialog("BYE", "c08a@gw", "bye", 2, &tag).build())
        .await;
    let r = w.results().remove(0);
    assert_eq!(r.outcome, CallOutcome::Completed);
    assert_eq!(r.t38.unwrap().path, T38Path::OurReinvite);
    w.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn c08_auto_accepts_the_peers_t38_reinvite_and_never_sends_its_own() {
    let mut w = W::new(&base_config("")).await;
    let tag = w.call("c08b@gw", AUDIO_SDP, Some(0)).await;
    w.at(1_000).await;
    let re = Req::in_dialog("INVITE", "c08b@gw", "re1", 2, &tag)
        .body(T38_SDP)
        .build();
    w.feed(re).await;
    let oks = w.responses(200, "INVITE");
    assert_eq!(oks.len(), 2, "initial 200 and the re-INVITE's 200");
    let (t, ok) = &oks[1];
    assert_eq!(*t, 1_000);
    let b = String::from_utf8(ok.body.to_vec()).unwrap();
    assert_eq!(media_lines(&b), vec!["m=image 30000 udptl t38"], "{b}");
    w.feed(Req::in_dialog("ACK", "c08b@gw", "ack2", 2, &tag).build())
        .await;
    w.at(30_000).await;
    assert!(w.requests("INVITE").is_empty(), "no re-INVITE of ours");
    assert_eq!(
        t38_events(&mut w),
        vec![("c08b@gw".into(), T38Path::PeerReinvite)]
    );
    w.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn c08_wait_peer_never_initiates_but_accepts_a_late_peer_reinvite() {
    let mut w = W::new(&base_config("t38_policy = \"wait-peer\"\n")).await;
    let tag = w.call("c08c@gw", AUDIO_SDP, Some(0)).await;
    w.at(120_000).await;
    assert!(
        w.requests("INVITE").is_empty(),
        "wait-peer: silent for 2 minutes"
    );
    assert!(w.requests("BYE").is_empty());
    w.feed(
        Req::in_dialog("INVITE", "c08c@gw", "re1", 2, &tag)
            .body(T38_SDP)
            .build(),
    )
    .await;
    assert_eq!(w.responses(200, "INVITE").len(), 2);
    assert_eq!(
        t38_events(&mut w),
        vec![("c08c@gw".into(), T38Path::PeerReinvite)]
    );
    w.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn c08_initiate_uses_initiate_after_secs() {
    let mut w = W::new(&base_config(
        "t38_policy = \"initiate\"\ninitiate_after_secs = 0.5\n",
    ))
    .await;
    w.call("c08d@gw", AUDIO_SDP, Some(100)).await;
    w.at(599).await;
    assert!(w.requests("INVITE").is_empty());
    w.at(600).await;
    assert_eq!(w.requests("INVITE")[0].0, 600);
    w.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn c08_audio_only_answers_488_to_t38_and_never_initiates() {
    let mut w = W::new(&format!(
        "{}\n[call]\nmax_duration_secs = 20\n",
        // [call] must come before [[trunks]] in no particular order; TOML tables may be in any order
        base_config("t38_policy = \"audio-only\"\n")
    ))
    .await;
    let tag = w.call("c08e@gw", AUDIO_SDP, Some(0)).await;
    w.at(10_000).await;
    assert!(
        w.requests("INVITE").is_empty(),
        "audio-only never offers T.38"
    );
    w.feed(
        Req::in_dialog("INVITE", "c08e@gw", "re1", 2, &tag)
            .body(T38_SDP)
            .build(),
    )
    .await;
    let rej = w.responses(488, "INVITE");
    assert_eq!(rej.len(), 1);
    assert_eq!(rej[0].0, 10_000);
    assert!(t38_events(&mut w).is_empty());
    // nothing else happens until the audio hold limit: BYE at answer + 20 s
    w.at(19_999).await;
    assert!(w.requests("BYE").is_empty());
    w.at(20_000).await;
    let bye = w.requests("BYE");
    assert_eq!(bye.len(), 1);
    assert_eq!(bye[0].0, 20_000);
    w.feed_resp(resp_to(&bye[0].1, 200, GW_TAG, None)).await;
    let r = w.results().remove(0);
    match r.outcome {
        CallOutcome::Failed {
            reason: FailureReason::G711PassthroughUnsupported { cause },
        } => {
            assert!(cause.contains("audio-only"), "{cause}");
        }
        other => panic!("{other:?}"),
    }
    w.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn c08_peer_refusing_our_t38_reinvite_ends_the_call_with_bye_and_a_reason() {
    for status in [488u16, 415, 606] {
        let mut w = W::new(&base_config("")).await;
        let call_id = format!("fb{status}@gw");
        let tag = w.call(&call_id, AUDIO_SDP, Some(0)).await;
        w.at(3_000).await;
        let (_, inv) = w.requests("INVITE").remove(0);
        w.feed_resp(resp_to(&inv, status, GW_TAG, None)).await;
        // the transaction layer ACKs the final non-2xx, then we hang up
        let acks = w.requests("ACK");
        assert_eq!(acks.len(), 1, "ACK of the {status}");
        assert_eq!(
            acks[0].1.headers.cseq().unwrap().seq,
            inv.headers.cseq().unwrap().seq
        );
        let bye = w.requests("BYE");
        assert_eq!(bye.len(), 1, "BYE after {status}: {:?}", w.status_lines());
        assert_eq!(bye[0].0, 3_000);
        assert_eq!(bye[0].1.uri.to_string(), "sip:gw@10.0.0.1:5070");
        assert_eq!(
            bye[0].1.headers.cseq().unwrap().seq,
            inv.headers.cseq().unwrap().seq + 1
        );
        w.feed_resp(resp_to(&bye[0].1, 200, GW_TAG, None)).await;
        let r = w.results().remove(0);
        match r.outcome {
            CallOutcome::Failed {
                reason: FailureReason::G711PassthroughUnsupported { cause },
            } => {
                assert!(cause.contains(&status.to_string()), "{cause}");
            }
            other => panic!("{other:?}"),
        }
        assert!(r.t38.is_none());
        let _ = tag;
        w.shutdown().await;
    }
}

#[tokio::test(start_paused = true)]
async fn c08_other_reinvite_failures_are_reported_as_such() {
    // 500 is not a "T.38 refused" status
    let mut w = W::new(&base_config("")).await;
    w.call("f500@gw", AUDIO_SDP, Some(0)).await;
    w.at(3_000).await;
    let (_, inv) = w.requests("INVITE").remove(0);
    w.feed_resp(resp_to(&inv, 500, GW_TAG, None)).await;
    let bye = w.requests("BYE");
    assert_eq!(bye.len(), 1);
    w.feed_resp(resp_to(&bye[0].1, 200, GW_TAG, None)).await;
    let r = w.results().remove(0);
    assert_eq!(
        r.outcome,
        CallOutcome::Failed {
            reason: FailureReason::T38ReinviteFailed {
                status: Some(500),
                detail: "T.38 re-INVITE answered 500".into()
            }
        }
    );
    w.shutdown().await;

    // 200 whose answer rejects the image line (port 0) counts as a refusal
    let mut w = W::new(&base_config("")).await;
    w.call("f200@gw", AUDIO_SDP, Some(0)).await;
    w.at(3_000).await;
    let (_, inv) = w.requests("INVITE").remove(0);
    let rejected = T38_SDP.replace("m=image 5004", "m=image 0");
    w.feed_resp(resp_to(&inv, 200, GW_TAG, Some(&rejected)))
        .await;
    let bye = w.requests("BYE");
    assert_eq!(bye.len(), 1);
    w.feed_resp(resp_to(&bye[0].1, 200, GW_TAG, None)).await;
    match w.results().remove(0).outcome {
        CallOutcome::Failed {
            reason: FailureReason::G711PassthroughUnsupported { cause },
        } => {
            assert!(cause.contains("without a usable m=image"), "{cause}");
        }
        other => panic!("{other:?}"),
    }
    w.shutdown().await;

    // no final answer at all: the 32 s re-INVITE timeout
    let mut w = W::new(&base_config("")).await;
    w.call("f408@gw", AUDIO_SDP, Some(0)).await;
    w.at(3_000).await;
    let (_, inv) = w.requests("INVITE").remove(0);
    w.feed_resp(resp_to(&inv, 180, GW_TAG, None)).await;
    w.at(3_000 + 32_000).await;
    assert_eq!(w.requests("CANCEL")[0].0, 35_000);
    let bye = w.requests("BYE");
    assert!(!bye.is_empty(), "{:?}", w.status_lines());
    w.feed_resp(resp_to(&bye[0].1, 200, GW_TAG, None)).await;
    match w.results().remove(0).outcome {
        CallOutcome::Failed {
            reason: FailureReason::T38ReinviteFailed { status: None, .. },
        } => {}
        other => panic!("{other:?}"),
    }
    w.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn c08_stub_session_holds_the_call_until_max_duration_then_hangs_up() {
    let mut w = W::new(&format!(
        "{}\n[call]\nmax_duration_secs = 30\n",
        base_config("")
    ))
    .await;
    w.call("max@gw", T38_SDP, Some(0)).await;
    w.at(29_999).await;
    assert!(w.requests("BYE").is_empty());
    w.at(30_000).await;
    let bye = w.requests("BYE");
    assert_eq!(bye.len(), 1);
    assert_eq!(bye[0].0, 30_000);
    w.feed_resp(resp_to(&bye[0].1, 200, GW_TAG, None)).await;
    let r = w.results().remove(0);
    assert_eq!(
        r.outcome,
        CallOutcome::Failed {
            reason: FailureReason::MaxDuration
        }
    );
    assert!(r.t38.is_some(), "T.38 was in force");
    w.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn c08_peer_hangup_before_t38_is_a_failure_and_cancel_is_reported() {
    let mut w = W::new(&base_config("")).await;
    let tag = w.call("early@gw", AUDIO_SDP, Some(0)).await;
    w.at(500).await;
    w.feed(Req::in_dialog("BYE", "early@gw", "bye", 2, &tag).build())
        .await;
    assert_eq!(w.responses(200, "BYE").len(), 1);
    assert_eq!(
        w.results().remove(0).outcome,
        CallOutcome::Failed {
            reason: FailureReason::PeerHangupBeforeT38
        }
    );
    assert!(
        w.requests("INVITE").is_empty(),
        "no re-INVITE after the call ended"
    );
    w.shutdown().await;

    // CANCEL during the answer delay
    let mut w = W::new(&format!(
        "{}\n[call]\nanswer_delay_ms = 2000\n",
        base_config("")
    ))
    .await;
    w.feed(Req::invite("cancel@gw", AUDIO_SDP).build()).await;
    assert_eq!(w.status_lines()[0].1, "SIP/2.0 180 Ringing");
    w.at(500).await;
    let mut cancel = Req::invite("cancel@gw", AUDIO_SDP).build();
    cancel.method = sip_core::message::Method::Cancel;
    cancel.headers.set("CSeq", "1 CANCEL");
    cancel.body = Default::default();
    w.feed(cancel).await;
    assert_eq!(w.responses(200, "CANCEL").len(), 1);
    assert_eq!(w.responses(487, "INVITE").len(), 1);
    w.at(3_000).await;
    assert!(w.responses(200, "INVITE").is_empty(), "never answered");
    assert_eq!(
        w.results().remove(0).outcome,
        CallOutcome::Failed {
            reason: FailureReason::Cancelled
        }
    );
    w.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn c08_missing_ack_ends_the_call() {
    let mut w = W::new(&base_config("")).await;
    w.call("noack@gw", AUDIO_SDP, None).await;
    w.at(31_999).await;
    assert!(w.requests("BYE").is_empty());
    w.at(32_000).await;
    assert_eq!(w.requests("BYE")[0].0, 32_000);
    // the peer never answers the BYE either: the result is not held back longer than 5 s
    w.at(36_999).await;
    assert!(w.results().is_empty());
    w.at(37_000).await;
    assert_eq!(
        w.results().remove(0).outcome,
        CallOutcome::Failed {
            reason: FailureReason::AckTimeout
        }
    );
    w.shutdown().await;
}

// ================================================================ C09

#[tokio::test(start_paused = true)]
async fn c09_sdp_tolerance_defaults_floor_and_connection_placement() {
    // missing RateManagement + UdpEC, tiny MaxDatagram, session-level c= only, odd attribute case
    let sloppy = "v=0\r\no=gw 1 1 IN IP4 203.0.113.9\r\ns=-\r\nc=IN IP4 203.0.113.50\r\nt=0 0\r\nm=image 6000 udptl t38\r\na=T38faxVersion:0\r\na=T38MaxBitRate:9600\r\na=T38FaxMaxDatagram:10\r\na=T38FaxFillBitRemoval\r\na=T38FaxTranscodingMMR:0\r\na=X-vendor-thing:42\r\n";
    let mut w = W::new(&base_config("")).await;
    w.feed(Req::invite("c09a@gw", sloppy).build()).await;
    let body = answer_body(&w);
    assert_eq!(
        media_lines(&body),
        vec!["m=image 30000 udptl t38"],
        "{body}"
    );
    // RateManagement defaulted to transferredTCF in the answer, bit rate = min(9600, 14400)
    assert!(
        body.contains("a=T38FaxRateManagement:transferredTCF"),
        "{body}"
    );
    assert!(body.contains("a=T38MaxBitRate:9600"), "{body}");
    assert!(
        !body.contains("FillBitRemoval"),
        "we do not support it, so not in the answer: {body}"
    );
    let p = w
        .events()
        .into_iter()
        .find_map(|e| match e {
            CallEvent::T38Established { params, .. } => Some(params),
            _ => None,
        })
        .unwrap();
    assert_eq!(p.peer_max_datagram, 72, "raised to the floor");
    assert_eq!(p.local_max_datagram, 260);
    assert_eq!(
        p.peer_media.as_deref(),
        Some("203.0.113.50:6000"),
        "session-level c="
    );
    assert_eq!(p.max_bit_rate, 9600);
    assert_eq!(p.rate_management, "transferredTCF");
    let joined = p.warnings.join(" | ");
    assert!(joined.contains("T38FaxRateManagement"), "{joined}");
    assert!(joined.contains("T38FaxUdpEC"), "{joined}");
    assert!(
        joined.contains("T38FaxMaxDatagram 10 is below the floor 72"),
        "{joined}"
    );
    assert_eq!(p.warnings.len(), 3, "{joined}");
    w.shutdown().await;

    // c= only on the media level
    let media_c = "v=0\r\no=gw 1 1 IN IP4 203.0.113.9\r\ns=-\r\nt=0 0\r\nm=image 6001 udptl t38\r\nc=IN IP4 203.0.113.51\r\na=T38FaxVersion:0\r\na=T38FaxRateManagement:localTCF\r\na=T38FaxUdpEC:t38UDPNoEC\r\na=T38FaxMaxDatagram:300\r\n";
    let mut w = W::new(&base_config("")).await;
    w.feed(Req::invite("c09b@gw", media_c).build()).await;
    let body = answer_body(&w);
    assert!(
        body.contains("a=T38FaxRateManagement:localTCF"),
        "copied from the offer: {body}"
    );
    assert!(body.contains("a=T38FaxUdpEC:t38UDPNoEC"), "{body}");
    let p = w
        .events()
        .into_iter()
        .find_map(|e| match e {
            CallEvent::T38Established { params, .. } => Some(params),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        p.peer_media.as_deref(),
        Some("203.0.113.51:6001"),
        "media-level c="
    );
    assert_eq!(p.peer_max_datagram, 300);
    assert!(p.warnings.is_empty(), "{:?}", p.warnings);
    w.shutdown().await;

    // no c= at all: fall back to the signalling source
    let no_c = media_c.replace("c=IN IP4 203.0.113.51\r\n", "");
    let mut w = W::new(&base_config("")).await;
    w.feed(Req::invite("c09c@gw", &no_c).build()).await;
    let p = w
        .events()
        .into_iter()
        .find_map(|e| match e {
            CallEvent::T38Established { params, .. } => Some(params),
            _ => None,
        })
        .unwrap();
    assert_eq!(p.peer_media.as_deref(), Some("10.0.0.1:6001"));
    assert!(
        p.warnings.iter().any(|x| x.contains("no usable c= line")),
        "{:?}",
        p.warnings
    );
    w.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn c09_the_datagram_floor_is_configurable() {
    let offer = T38_SDP.replace("T38FaxMaxDatagram:200", "T38FaxMaxDatagram:150");
    let mut w = W::new(&base_config("[trunks.compat]\nmin_max_datagram = 180\n")).await;
    w.feed(Req::invite("floor@gw", &offer).build()).await;
    let p = w
        .events()
        .into_iter()
        .find_map(|e| match e {
            CallEvent::T38Established { params, .. } => Some(params),
            _ => None,
        })
        .unwrap();
    assert_eq!(p.peer_max_datagram, 180);
    assert!(
        p.warnings[0].contains("150") && p.warnings[0].contains("180"),
        "{:?}",
        p.warnings
    );
    w.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn c09_trunk_t38_parameters_shape_the_answer_and_our_offer() {
    let cfg = "[trunks.t38]\nmax_version = 3\nmax_bit_rate = 33600\nmax_buffer = 2000\nmax_datagram = 400\nfec = true\nrate_management = \"localTCF\"\nudp_ec = \"fec\"\nfill_bit_removal = true\ntranscoding_mmr = true\n";
    let mut w = W::new(&base_config(cfg)).await;
    // peer offers v2 / 14400 / FEC / fill-bit-removal / MMR
    let offer = "v=0\r\no=gw 1 1 IN IP4 203.0.113.9\r\ns=-\r\nc=IN IP4 203.0.113.9\r\nt=0 0\r\nm=image 5004 udptl t38\r\na=T38FaxVersion:2\r\na=T38MaxBitRate:14400\r\na=T38FaxFillBitRemoval\r\na=T38FaxTranscodingMMR\r\na=T38FaxRateManagement:transferredTCF\r\na=T38FaxMaxDatagram:200\r\na=T38FaxUdpEC:t38UDPFEC\r\n";
    w.feed(Req::invite("params@gw", offer).build()).await;
    let body = answer_body(&w);
    for attr in [
        "a=T38FaxVersion:2",
        "a=T38MaxBitRate:14400",
        "a=T38FaxFillBitRemoval",
        "a=T38FaxTranscodingMMR",
        "a=T38FaxRateManagement:transferredTCF",
        "a=T38FaxMaxBuffer:2000",
        "a=T38FaxMaxDatagram:400",
        "a=T38FaxUdpEC:t38UDPFEC",
    ] {
        assert!(body.lines().any(|l| l == attr), "missing {attr}:\n{body}");
    }
    w.shutdown().await;

    // our own offer uses rate_management / udp_ec from the trunk
    let mut w = W::new(&base_config(cfg)).await;
    w.call("params2@gw", AUDIO_SDP, Some(0)).await;
    w.at(3_000).await;
    let off = String::from_utf8(w.requests("INVITE")[0].1.body.to_vec()).unwrap();
    for attr in [
        "a=T38FaxVersion:3",
        "a=T38MaxBitRate:33600",
        "a=T38FaxRateManagement:localTCF",
        "a=T38FaxMaxBuffer:2000",
        "a=T38FaxMaxDatagram:400",
        "a=T38FaxUdpEC:t38UDPFEC",
        "a=T38FaxFillBitRemoval",
        "a=T38FaxTranscodingMMR",
    ] {
        assert!(off.lines().any(|l| l == attr), "missing {attr}:\n{off}");
    }
    w.shutdown().await;
}

// ================================================================ C10

#[tokio::test(start_paused = true)]
async fn c10_unknown_require_is_420_unless_the_trunk_accepts_it() {
    let strict = base_config("");
    let mut w = W::new(&strict).await;
    w.feed(
        Req::invite("req1@gw", AUDIO_SDP)
            .hdr("Require", "timer, FooBar, 100rel")
            .build(),
    )
    .await;
    let r = w.responses(420, "INVITE");
    assert_eq!(r.len(), 1, "{:?}", w.status_lines());
    assert_eq!(r[0].1.headers.get("Unsupported"), Some("foobar, 100rel"));
    assert!(w.responses(200, "INVITE").is_empty());
    match w.results().remove(0).outcome {
        CallOutcome::Failed {
            reason: FailureReason::UnsupportedRequire { tags },
        } => {
            assert_eq!(tags, vec!["foobar".to_string(), "100rel".to_string()]);
        }
        other => panic!("{other:?}"),
    }
    w.shutdown().await;

    // `Require: timer` alone is understood
    let mut w = W::new(&strict).await;
    w.feed(
        Req::invite("req2@gw", AUDIO_SDP)
            .hdr("Require", "timer")
            .hdr("Session-Expires", "1800;refresher=uac")
            .hdr("Supported", "timer")
            .build(),
    )
    .await;
    assert_eq!(w.responses(200, "INVITE").len(), 1);
    w.shutdown().await;

    // the flag is per trunk
    let mut w = W::new(
        "[sip]\nbind = \"192.0.2.10:5060\"\nadvertised = \"192.0.2.10\"\n\
         [[trunks]]\nname = \"lenient\"\nallowed_sources = [\"10.0.0.0/24\"]\n[trunks.compat]\naccept_unknown_require = true\n\
         [[trunks]]\nname = \"strict\"\nallowed_sources = [\"10.0.1.0/24\"]\n",
    )
    .await;
    let a = Req::invite("req3@gw", AUDIO_SDP)
        .hdr("Require", "FooBar")
        .build();
    w.feed_from(SipMessage::Request(a), "10.0.0.5:5060").await;
    let b = Req::invite("req4@gw", AUDIO_SDP)
        .hdr("Require", "FooBar")
        .build();
    w.feed_from(SipMessage::Request(b), "10.0.1.5:5060").await;
    let ok = w.responses(200, "INVITE");
    assert_eq!(ok.len(), 1);
    assert_eq!(ok[0].1.headers.call_id(), Some("req3@gw"));
    let bad = w.responses(420, "INVITE");
    assert_eq!(bad.len(), 1);
    assert_eq!(bad[0].1.headers.call_id(), Some("req4@gw"));
    w.shutdown().await;
}

// ================================================================ misc offers

#[tokio::test(start_paused = true)]
async fn offers_we_cannot_answer_get_488() {
    // no SDP at all
    let mut w = W::new(&base_config("")).await;
    let mut inv = Req::invite("nosdp@gw", "");
    inv.body = None;
    w.feed(inv.build()).await;
    assert_eq!(w.responses(488, "INVITE").len(), 1);
    assert_eq!(
        w.results().remove(0).outcome,
        CallOutcome::Failed {
            reason: FailureReason::NoSdpOffer
        }
    );
    // audio without a common codec
    let g729 = "v=0\r\no=gw 1 1 IN IP4 203.0.113.9\r\ns=-\r\nc=IN IP4 203.0.113.9\r\nt=0 0\r\nm=audio 4000 RTP/AVP 18\r\na=rtpmap:18 G729/8000\r\n";
    w.feed(Req::invite("g729@gw", g729).build()).await;
    assert_eq!(w.responses(488, "INVITE").len(), 2);
    let r = w.results();
    match &r[1].outcome {
        CallOutcome::Failed {
            reason: FailureReason::NoAcceptableMedia { detail },
        } => {
            assert!(detail.contains("no common audio codec"), "{detail}");
        }
        other => panic!("{other:?}"),
    }
    w.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn reinvites_the_controller_cannot_follow_get_488() {
    let mut w = W::new(&base_config("t38_policy = \"wait-peer\"\n")).await;
    let tag = w.call("odd@gw", AUDIO_SDP, Some(0)).await;
    // offer-less re-INVITE
    w.feed(Req::in_dialog("INVITE", "odd@gw", "re1", 2, &tag).build())
        .await;
    assert_eq!(w.responses(488, "INVITE").len(), 1);
    // once on T.38, a re-INVITE back to audio is refused; a T.38 refresh is answered again
    w.feed(
        Req::in_dialog("INVITE", "odd@gw", "re2", 3, &tag)
            .body(T38_SDP)
            .build(),
    )
    .await;
    assert_eq!(w.responses(200, "INVITE").len(), 2);
    w.feed(Req::in_dialog("ACK", "odd@gw", "ack3", 3, &tag).build())
        .await;
    w.feed(
        Req::in_dialog("INVITE", "odd@gw", "re3", 4, &tag)
            .body(AUDIO_SDP)
            .build(),
    )
    .await;
    assert_eq!(w.responses(488, "INVITE").len(), 2);
    w.feed(
        Req::in_dialog("INVITE", "odd@gw", "re4", 5, &tag)
            .body(T38_SDP)
            .build(),
    )
    .await;
    let oks = w.responses(200, "INVITE");
    assert_eq!(oks.len(), 3);
    // the refresh keeps the o= session id and the version of the previous T.38 answer
    let v1 = sdp(std::str::from_utf8(&oks[1].1.body).unwrap()).origin;
    let v2 = sdp(std::str::from_utf8(&oks[2].1.body).unwrap()).origin;
    assert_eq!(v1, v2);
    assert_eq!(t38_events(&mut w).len(), 1, "one T.38 session, not two");
    w.shutdown().await;
}
