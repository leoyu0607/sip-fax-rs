//! The call summary shown in the fax list: hang-up direction and reason, fax status, pages and
//! call length (docs/design/m3-store.md section 3). [`classify`] is a pure function of the
//! [`CallResult`], tested row by row (C01).

use std::time::SystemTime;

use store::{FaxStatus, HangupDirection};

use crate::event::{ByeBy, CallOutcome, CallResult, FailureReason};

/// T.30 result codes the classification looks at (`t30::ResultCode` as numbers, so the summary
/// of an old `result.json` does not need the t30 crate's enum).
mod t30code {
    pub const NO_RESPONSE_TO_DIS: u8 = 1;
    pub const T1_EXPIRED: u8 = 2;
    pub const ABORTED: u8 = 11;
}

/// The fixed `hangup_code` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum HangupCode {
    /// T.30 succeeded.
    Completed,
    /// The peer hung up before T.30 finished, with at least one page in.
    PeerHangup,
    /// The peer hung up without any page.
    PeerHangupNoFax,
    /// T.30 failed.
    T30Failed,
    /// Answered, but no fax signal ever came.
    NoFaxSignal,
    /// The peer refused T.38 (G.711 passthrough is not supported yet).
    T38Rejected,
    /// CANCEL before we answered.
    CallerCancelled,
    /// We refused the call.
    Rejected,
    /// No ACK for our 200.
    AckTimeout,
    /// The session timer ran out.
    SessionExpired,
    /// `max_duration_secs` reached.
    MaxDuration,
    /// The service stopped.
    Shutdown,
    /// Anything else.
    InternalError,
}

impl HangupCode {
    /// Every code (the web interface has a text for each of them; a test lists them).
    pub const ALL: [HangupCode; 13] = [
        HangupCode::Completed,
        HangupCode::PeerHangup,
        HangupCode::PeerHangupNoFax,
        HangupCode::T30Failed,
        HangupCode::NoFaxSignal,
        HangupCode::T38Rejected,
        HangupCode::CallerCancelled,
        HangupCode::Rejected,
        HangupCode::AckTimeout,
        HangupCode::SessionExpired,
        HangupCode::MaxDuration,
        HangupCode::Shutdown,
        HangupCode::InternalError,
    ];

    /// The text stored in `faxes.hangup_code`.
    pub fn as_str(self) -> &'static str {
        match self {
            HangupCode::Completed => "completed",
            HangupCode::PeerHangup => "peer_hangup",
            HangupCode::PeerHangupNoFax => "peer_hangup_no_fax",
            HangupCode::T30Failed => "t30_failed",
            HangupCode::NoFaxSignal => "no_fax_signal",
            HangupCode::T38Rejected => "t38_rejected",
            HangupCode::CallerCancelled => "caller_cancelled",
            HangupCode::Rejected => "rejected",
            HangupCode::AckTimeout => "ack_timeout",
            HangupCode::SessionExpired => "session_expired",
            HangupCode::MaxDuration => "max_duration",
            HangupCode::Shutdown => "shutdown",
            HangupCode::InternalError => "internal_error",
        }
    }

    /// The fixed explanation shown next to the code.
    pub fn text(self) -> &'static str {
        match self {
            HangupCode::Completed => "傳真接收完成",
            HangupCode::PeerHangup => "對方在傳真完成前掛斷",
            HangupCode::PeerHangupNoFax => "對方掛斷，沒有收到任何頁面",
            HangupCode::T30Failed => "傳真協定（T.30）失敗",
            HangupCode::NoFaxSignal => "接通後一直沒有傳真訊號（可能是一般電話）",
            HangupCode::T38Rejected => "對方不接受 T.38",
            HangupCode::CallerCancelled => "來電者在接聽前取消",
            HangupCode::Rejected => "本機拒絕來電",
            HangupCode::AckTimeout => "接聽後沒有收到 ACK",
            HangupCode::SessionExpired => "SIP session 逾時",
            HangupCode::MaxDuration => "通話超過最長時間",
            HangupCode::Shutdown => "服務停止",
            HangupCode::InternalError => "內部錯誤",
        }
    }
}

/// What the fax list shows about a finished call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Summary {
    /// Final fax status (never `Receiving`).
    pub fax_status: FaxStatus,
    /// Who ended the call.
    pub hangup_direction: HangupDirection,
    /// Fixed reason code.
    pub hangup_code: HangupCode,
    /// Explanation of the reason (the fixed text, plus the failing detail where there is one).
    pub hangup_text: String,
    /// SIP status of a refusal or of a failed T.38 re-INVITE.
    pub sip_status: Option<i32>,
    /// T.30 result code, when a fax session ran.
    pub t30_code: Option<i32>,
    /// Pages received and confirmed.
    pub pages: i32,
    /// Call length in seconds: `ended_at - answered_at`, 0 when the call was not answered.
    pub duration_sec: i32,
}

/// `ended - answered` in whole seconds (rounded), 0 without an answer or when the clock went back.
pub fn duration_secs(answered: Option<SystemTime>, ended: SystemTime) -> i32 {
    let Some(a) = answered else { return 0 };
    let ms = ended.duration_since(a).map_or(0, |d| d.as_millis());
    i32::try_from((ms + 500) / 1000).unwrap_or(i32::MAX)
}

/// Pages in, as the status decides: some pages is `partial`, none is `failed`.
fn by_pages(pages: i32) -> FaxStatus {
    if pages >= 1 {
        FaxStatus::Partial
    } else {
        FaxStatus::Failed
    }
}

struct Verdict {
    status: FaxStatus,
    dir: HangupDirection,
    code: HangupCode,
    sip: Option<i32>,
    detail: Option<String>,
}

fn v(status: FaxStatus, dir: HangupDirection, code: HangupCode) -> Verdict {
    Verdict {
        status,
        dir,
        code,
        sip: None,
        detail: None,
    }
}

fn rejected(sip: i32, detail: impl Into<String>) -> Verdict {
    Verdict {
        sip: Some(sip),
        detail: Some(detail.into()),
        ..v(
            FaxStatus::NoFax,
            HangupDirection::System,
            HangupCode::Rejected,
        )
    }
}

/// Maps a finished call to its summary.
pub fn classify(r: &CallResult) -> Summary {
    let fax = r.fax.as_ref();
    let pages = fax.map_or(0, |f| i32::try_from(f.pages).unwrap_or(i32::MAX));
    let t30_code = fax.map(|f| i32::from(f.code));
    let system = HangupDirection::System;
    let remote = HangupDirection::Remote;
    let local = HangupDirection::Local;
    let verdict = match &r.outcome {
        CallOutcome::Completed => match fax {
            Some(f) if f.code == 0 => v(
                FaxStatus::Received,
                match r.bye_by {
                    Some(ByeBy::Remote) => remote,
                    _ => local,
                },
                HangupCode::Completed,
            ),
            Some(_) => t30_verdict(fax, pages),
            // the stub session of `[call] fax_session = "hold"` never receives anything
            None => v(FaxStatus::NoFax, remote, HangupCode::PeerHangupNoFax),
        },
        CallOutcome::Failed { reason } => match reason {
            FailureReason::Cancelled => v(FaxStatus::NoFax, remote, HangupCode::CallerCancelled),
            FailureReason::NoSdpOffer => rejected(488, "INVITE 沒有可用的 SDP"),
            FailureReason::NoAcceptableMedia { detail } => rejected(488, detail.clone()),
            FailureReason::UnsupportedRequire { tags } => {
                rejected(420, format!("不支援的 Require：{}", tags.join(", ")))
            }
            FailureReason::UnknownSource => rejected(403, "來源不在任何 trunk 的白名單"),
            FailureReason::NoMediaPort => rejected(503, "沒有可用的媒體埠"),
            FailureReason::AckTimeout => v(by_pages(pages), system, HangupCode::AckTimeout),
            FailureReason::G711PassthroughUnsupported { cause } => Verdict {
                detail: Some(cause.clone()),
                ..v(FaxStatus::NoFax, system, HangupCode::T38Rejected)
            },
            FailureReason::T38ReinviteFailed { status, detail } => match status {
                Some(s) => Verdict {
                    sip: Some(i32::from(*s)),
                    detail: Some(detail.clone()),
                    ..v(FaxStatus::NoFax, system, HangupCode::T38Rejected)
                },
                None => Verdict {
                    detail: Some(detail.clone()),
                    ..v(by_pages(pages), system, HangupCode::InternalError)
                },
            },
            FailureReason::PeerHangupBeforeT38 => {
                v(FaxStatus::NoFax, remote, HangupCode::PeerHangupNoFax)
            }
            FailureReason::MaxDuration => v(by_pages(pages), system, HangupCode::MaxDuration),
            FailureReason::SessionExpired => v(by_pages(pages), system, HangupCode::SessionExpired),
            FailureReason::Shutdown => v(by_pages(pages), system, HangupCode::Shutdown),
            FailureReason::Internal { detail } => Verdict {
                detail: Some(detail.clone()),
                ..v(by_pages(pages), system, HangupCode::InternalError)
            },
            FailureReason::Session { detail } => match fax {
                None => Verdict {
                    detail: Some(detail.clone()),
                    ..v(by_pages(pages), system, HangupCode::InternalError)
                },
                // the files could not be written although T.30 was fine
                Some(f) if f.code == 0 => Verdict {
                    detail: Some(detail.clone()),
                    ..v(by_pages(pages), system, HangupCode::InternalError)
                },
                Some(_) => t30_verdict(fax, pages),
            },
        },
    };
    let text = match &verdict.detail {
        Some(d) if !d.is_empty() => format!("{}：{d}", verdict.code.text()),
        _ => verdict.code.text().to_string(),
    };
    Summary {
        fax_status: verdict.status,
        hangup_direction: verdict.dir,
        hangup_code: verdict.code,
        hangup_text: text,
        sip_status: verdict.sip,
        t30_code,
        pages,
        duration_sec: duration_secs(r.answered_at, r.ended_at),
    }
}

/// A fax session that ended with a T.30 failure code.
fn t30_verdict(fax: Option<&crate::event::FaxReport>, pages: i32) -> Verdict {
    let code = fax.map_or(0, |f| f.code);
    let name = fax.map_or("", |f| f.code_name.as_str());
    let detail = format!("{name} ({code})");
    let with = |mut x: Verdict| {
        x.detail = Some(detail.clone());
        x
    };
    match code {
        // our own abort, which only the end of the call causes: the peer hung up under the session
        t30code::ABORTED if pages >= 1 => with(v(
            FaxStatus::Partial,
            HangupDirection::Remote,
            HangupCode::PeerHangup,
        )),
        t30code::ABORTED => with(v(
            FaxStatus::NoFax,
            HangupDirection::Remote,
            HangupCode::PeerHangupNoFax,
        )),
        // we spoke first (DIS) and nothing ever answered
        t30code::NO_RESPONSE_TO_DIS | t30code::T1_EXPIRED if pages == 0 => with(v(
            FaxStatus::NoFax,
            HangupDirection::Local,
            HangupCode::NoFaxSignal,
        )),
        _ => with(v(
            by_pages(pages),
            HangupDirection::Local,
            HangupCode::T30Failed,
        )),
    }
}

#[cfg(test)]
mod all_codes_tests {
    use super::HangupCode;

    /// Adding a variant fails to compile here until `ALL` (and so the web interface's list of
    /// texts, test U02) is thought about.
    fn index(c: HangupCode) -> usize {
        match c {
            HangupCode::Completed => 0,
            HangupCode::PeerHangup => 1,
            HangupCode::PeerHangupNoFax => 2,
            HangupCode::T30Failed => 3,
            HangupCode::NoFaxSignal => 4,
            HangupCode::T38Rejected => 5,
            HangupCode::CallerCancelled => 6,
            HangupCode::Rejected => 7,
            HangupCode::AckTimeout => 8,
            HangupCode::SessionExpired => 9,
            HangupCode::MaxDuration => 10,
            HangupCode::Shutdown => 11,
            HangupCode::InternalError => 12,
        }
    }

    #[test]
    fn all_lists_every_code_once_with_distinct_text() {
        let mut seen = std::collections::BTreeSet::new();
        for (i, c) in HangupCode::ALL.iter().enumerate() {
            assert_eq!(index(*c), i, "ALL is in declaration order");
            assert!(seen.insert(c.as_str()), "{} twice", c.as_str());
        }
        assert_eq!(seen.len(), 13);
    }
}
