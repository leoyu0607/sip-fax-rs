//! C01: every row of docs/design/m3-store.md section 3 (hang-up direction, reason, fax status),
//! driven by synthetic `CallResult`s. The expected values are written out by hand from the
//! specification table, not computed by the code under test.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use fax_in::event::{ByeBy, CallOutcome, CallResult, FailureReason, FaxReport, UdptlCounters};
use fax_in::summary::{HangupCode, classify, duration_secs};
use store::{FaxStatus, HangupDirection};

const T0: u64 = 1_792_540_800; // 2026-10-21T00:00:00Z

fn at(secs: u64, ms: u64) -> SystemTime {
    UNIX_EPOCH + Duration::new(T0 + secs, 0) + Duration::from_millis(ms)
}

fn fax(code: u8, name: &str, pages: u32) -> FaxReport {
    FaxReport {
        code,
        code_name: name.into(),
        pages,
        bit_rate: 14_400,
        modem: Some("v17".into()),
        resolution: Some("fine".into()),
        compression: Some("mmr".into()),
        remote_ident: Some("0212345678".into()),
        bad_lines_per_page: vec![0; pages as usize],
        duration_ms: 1000,
        partial: code != 0 && pages > 0,
        tiff: None,
        pdf: None,
        store_error: None,
        udptl: UdptlCounters::default(),
        tiff_size: None,
        pdf_size: None,
        ecm: false,
        ecm_ppr_sent: 0,
        ecm_frames_lost: 0,
    }
}

fn result(outcome: CallOutcome, report: Option<FaxReport>, bye: Option<ByeBy>) -> CallResult {
    CallResult {
        call_id: "c01@test".into(),
        trunk: "t".into(),
        peer: "192.0.2.10:5060".into(),
        from: "sip:0200000001@192.0.2.10".into(),
        to: "sip:0200000002@198.51.100.20".into(),
        numbers: Default::default(),
        started_at: at(0, 0),
        answered_at: Some(at(1, 0)),
        t38_established_at: None,
        ended_at: at(63, 0),
        bye_by: bye,
        t38: None,
        fax: report,
        outcome,
    }
}

fn failed(reason: FailureReason) -> CallOutcome {
    CallOutcome::Failed { reason }
}

fn session(detail: &str) -> CallOutcome {
    failed(FailureReason::Session {
        detail: detail.into(),
    })
}

/// One expected row.
struct Row {
    name: &'static str,
    call: CallResult,
    status: FaxStatus,
    dir: HangupDirection,
    code: &'static str,
    sip: Option<i32>,
    t30: Option<i32>,
    pages: i32,
}

fn rows() -> Vec<Row> {
    use FaxStatus::{Failed, NoFax, Partial, Received};
    use HangupDirection::{Local, Remote, System};
    let mut v = Vec::new();
    let mut row = |name, call, status, dir, code, sip, t30, pages| {
        v.push(Row {
            name,
            call,
            status,
            dir,
            code,
            sip,
            t30,
            pages,
        });
    };
    // --- completed: the direction is whoever sent the BYE
    row(
        "completed, we hung up",
        result(
            CallOutcome::Completed,
            Some(fax(0, "ok", 3)),
            Some(ByeBy::Local),
        ),
        Received,
        Local,
        "completed",
        None,
        Some(0),
        3,
    );
    row(
        "completed, the peer hung up",
        result(
            CallOutcome::Completed,
            Some(fax(0, "ok", 1)),
            Some(ByeBy::Remote),
        ),
        Received,
        Remote,
        "completed",
        None,
        Some(0),
        1,
    );
    // --- peer_hangup: BYE before T.30 finished
    row(
        "peer hung up in page 3 (2 pages in)",
        result(
            session("T.30 aborted (11) after 2 page(s)"),
            Some(fax(11, "aborted", 2)),
            Some(ByeBy::Remote),
        ),
        Partial,
        Remote,
        "peer_hangup",
        None,
        Some(11),
        2,
    );
    // --- peer_hangup_no_fax
    row(
        "peer hung up before any page",
        result(
            session("T.30 aborted (11) after 0 page(s)"),
            Some(fax(11, "aborted", 0)),
            Some(ByeBy::Remote),
        ),
        NoFax,
        Remote,
        "peer_hangup_no_fax",
        None,
        Some(11),
        0,
    );
    row(
        "peer hung up before T.38 was established",
        result(
            failed(FailureReason::PeerHangupBeforeT38),
            None,
            Some(ByeBy::Remote),
        ),
        NoFax,
        Remote,
        "peer_hangup_no_fax",
        None,
        None,
        0,
    );
    // --- t30_failed: partial with pages, failed without
    row(
        "T.30 failed after 1 page (the F1 field case)",
        result(
            session("T.30 t2_expired_image (4) after 1 page(s)"),
            Some(fax(4, "t2_expired_image", 1)),
            Some(ByeBy::Local),
        ),
        Partial,
        Local,
        "t30_failed",
        None,
        Some(4),
        1,
    );
    row(
        "T.30 failed without a page (T2 expired)",
        result(
            session("T.30 t2_expired_command (3) after 0 page(s)"),
            Some(fax(3, "t2_expired_command", 0)),
            Some(ByeBy::Local),
        ),
        Failed,
        Local,
        "t30_failed",
        None,
        Some(3),
        0,
    );
    row(
        "training failed without a page",
        result(
            session("T.30 training_failed (6) after 0 page(s)"),
            Some(fax(6, "training_failed", 0)),
            Some(ByeBy::Local),
        ),
        Failed,
        Local,
        "t30_failed",
        None,
        Some(6),
        0,
    );
    row(
        "ECM: the sender gave up on frames, pages kept",
        result(
            session("T.30 ecm_frames_lost (14) after 2 page(s)"),
            Some(fax(14, "ecm_frames_lost", 2)),
            Some(ByeBy::Local),
        ),
        Partial,
        Local,
        "t30_failed",
        None,
        Some(14),
        2,
    );
    row(
        "DCN in the middle after 1 page",
        result(
            session("T.30 partial_dcn (7) after 1 page(s)"),
            Some(fax(7, "partial_dcn", 1)),
            Some(ByeBy::Local),
        ),
        Partial,
        Local,
        "t30_failed",
        None,
        Some(7),
        1,
    );
    // --- no_fax_signal: answered, nothing ever came
    row(
        "no answer to our DIS (an ordinary phone call)",
        result(
            session("T.30 no_response_to_dis (1) after 0 page(s)"),
            Some(fax(1, "no_response_to_dis", 0)),
            Some(ByeBy::Local),
        ),
        NoFax,
        Local,
        "no_fax_signal",
        None,
        Some(1),
        0,
    );
    row(
        "T1 expired without a page",
        result(
            session("T.30 t1_expired (2) after 0 page(s)"),
            Some(fax(2, "t1_expired", 0)),
            Some(ByeBy::Local),
        ),
        NoFax,
        Local,
        "no_fax_signal",
        None,
        Some(2),
        0,
    );
    row(
        "T1 expired after a page is a T.30 failure, not a voice call",
        result(
            session("T.30 t1_expired (2) after 1 page(s)"),
            Some(fax(2, "t1_expired", 1)),
            Some(ByeBy::Local),
        ),
        Partial,
        Local,
        "t30_failed",
        None,
        Some(2),
        1,
    );
    // --- t38_rejected
    row(
        "peer answered 488 to our T.38 re-INVITE",
        result(
            failed(FailureReason::G711PassthroughUnsupported {
                cause: "peer answered 488 to the T.38 re-INVITE".into(),
            }),
            None,
            Some(ByeBy::Local),
        ),
        NoFax,
        System,
        "t38_rejected",
        None,
        None,
        0,
    );
    row(
        "t38_policy audio-only",
        result(
            failed(FailureReason::G711PassthroughUnsupported {
                cause: "t38_policy = audio-only".into(),
            }),
            None,
            Some(ByeBy::Remote),
        ),
        NoFax,
        System,
        "t38_rejected",
        None,
        None,
        0,
    );
    row(
        "T.38 re-INVITE answered 500",
        result(
            failed(FailureReason::T38ReinviteFailed {
                status: Some(500),
                detail: "T.38 re-INVITE answered 500".into(),
            }),
            None,
            Some(ByeBy::Local),
        ),
        NoFax,
        System,
        "t38_rejected",
        Some(500),
        None,
        0,
    );
    row(
        "T.38 re-INVITE never answered",
        result(
            failed(FailureReason::T38ReinviteFailed {
                status: None,
                detail: "no final answer".into(),
            }),
            None,
            Some(ByeBy::Local),
        ),
        Failed,
        System,
        "internal_error",
        None,
        None,
        0,
    );
    // --- caller_cancelled
    row(
        "CANCEL before we answered",
        {
            let mut c = result(failed(FailureReason::Cancelled), None, None);
            c.answered_at = None;
            c
        },
        NoFax,
        Remote,
        "caller_cancelled",
        None,
        None,
        0,
    );
    // --- rejected, with the SIP status
    for (name, reason, sip) in [
        ("no SDP offer", FailureReason::NoSdpOffer, 488),
        (
            "nothing acceptable in the offer",
            FailureReason::NoAcceptableMedia {
                detail: "no codec".into(),
            },
            488,
        ),
        (
            "unsupported Require",
            FailureReason::UnsupportedRequire {
                tags: vec!["100rel".into()],
            },
            420,
        ),
        (
            "source outside every trunk",
            FailureReason::UnknownSource,
            403,
        ),
        ("no free media port", FailureReason::NoMediaPort, 503),
    ] {
        row(
            name,
            {
                let mut c = result(failed(reason), None, None);
                c.answered_at = None;
                c
            },
            NoFax,
            System,
            "rejected",
            Some(sip),
            None,
            0,
        );
    }
    // --- the "by pages" codes: 0 pages = failed, >= 1 page = partial
    for (name, reason, code) in [
        ("ack_timeout", FailureReason::AckTimeout, "ack_timeout"),
        (
            "session_expired",
            FailureReason::SessionExpired,
            "session_expired",
        ),
        ("max_duration", FailureReason::MaxDuration, "max_duration"),
        ("shutdown", FailureReason::Shutdown, "shutdown"),
        (
            "internal_error",
            FailureReason::Internal {
                detail: "boom".into(),
            },
            "internal_error",
        ),
    ] {
        row(
            name,
            result(failed(reason.clone()), None, Some(ByeBy::Local)),
            Failed,
            System,
            code,
            None,
            None,
            0,
        );
        row(
            name,
            result(
                failed(reason),
                Some(fax(11, "aborted", 2)),
                Some(ByeBy::Local),
            ),
            Partial,
            System,
            code,
            None,
            Some(11),
            2,
        );
    }
    // --- a session failure that is not a T.30 result
    row(
        "UDPTL port could not be bound (no fax report)",
        result(session("cannot bind UDPTL port"), None, Some(ByeBy::Local)),
        Failed,
        System,
        "internal_error",
        None,
        None,
        0,
    );
    row(
        "fax fine but the files could not be stored",
        {
            let mut f = fax(0, "ok", 2);
            f.store_error = Some("disk full".into());
            result(
                session("fax received but not stored: disk full"),
                Some(f),
                Some(ByeBy::Local),
            )
        },
        Partial,
        System,
        "internal_error",
        None,
        Some(0),
        2,
    );
    v
}

#[test]
fn c01_every_row_of_the_hangup_table() {
    let rows = rows();
    assert!(rows.len() >= 35, "the table must stay complete");
    for r in rows {
        let s = classify(&r.call);
        let got = (
            s.fax_status,
            s.hangup_direction,
            s.hangup_code.as_str(),
            s.sip_status,
            s.t30_code,
            s.pages,
        );
        let want = (r.status, r.dir, r.code, r.sip, r.t30, r.pages);
        assert_eq!(got, want, "row: {}", r.name);
        assert!(!s.hangup_text.is_empty(), "row {} has no text", r.name);
    }
}

#[test]
fn c01_every_code_of_the_specification_is_reachable() {
    // The 13 fixed codes of the specification (m3-store.md section 3) all come out of the table.
    let codes: std::collections::BTreeSet<&str> = rows().iter().map(|r| r.code).collect();
    for want in [
        "completed",
        "peer_hangup",
        "peer_hangup_no_fax",
        "t30_failed",
        "no_fax_signal",
        "t38_rejected",
        "caller_cancelled",
        "rejected",
        "ack_timeout",
        "session_expired",
        "max_duration",
        "shutdown",
        "internal_error",
    ] {
        assert!(codes.contains(want), "no row produces {want}");
    }
    assert_eq!(codes.len(), 13);
}

#[test]
fn c01_status_never_reports_a_call_in_progress() {
    for r in rows() {
        assert_ne!(classify(&r.call).fax_status, FaxStatus::Receiving);
    }
}

#[test]
fn c01_the_text_names_the_failing_detail() {
    let c = result(
        session("T.30 t2_expired_image (4) after 1 page(s)"),
        Some(fax(4, "t2_expired_image", 1)),
        Some(ByeBy::Local),
    );
    let s = classify(&c);
    assert!(
        s.hangup_text.contains("t2_expired_image (4)"),
        "{}",
        s.hangup_text
    );
    assert!(s.hangup_text.starts_with(HangupCode::T30Failed.text()));
    // fixed text when there is nothing to add
    let ok = classify(&result(
        CallOutcome::Completed,
        Some(fax(0, "ok", 1)),
        Some(ByeBy::Local),
    ));
    assert_eq!(ok.hangup_text, HangupCode::Completed.text());
}

#[test]
fn c01_duration_is_ended_minus_answered() {
    let t = |s, ms| Some(at(s, ms));
    // whole seconds
    assert_eq!(duration_secs(t(1, 0), at(63, 0)), 62);
    // rounded to the nearest second
    assert_eq!(duration_secs(t(1, 0), at(63, 499)), 62);
    assert_eq!(duration_secs(t(1, 0), at(63, 500)), 63);
    // not answered: 0, whatever the end
    assert_eq!(duration_secs(None, at(63, 0)), 0);
    // the end before the answer (clock stepped back): 0, never negative
    assert_eq!(duration_secs(t(10, 0), at(5, 0)), 0);
    // through classify
    let mut c = result(
        CallOutcome::Completed,
        Some(fax(0, "ok", 1)),
        Some(ByeBy::Local),
    );
    assert_eq!(classify(&c).duration_sec, 62);
    c.answered_at = None;
    assert_eq!(classify(&c).duration_sec, 0);
}
