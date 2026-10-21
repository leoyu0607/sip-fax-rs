//! Outcome of a receive call.

use crate::caps::{Compression, Modem, Resolution};

/// Result code of a call. `Ok` is 0; everything else is a failure (some with pages).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ResultCode {
    /// Every page received and the procedure ended normally.
    Ok = 0,
    /// The DIS was retransmitted `dis_retries` times without any answer.
    NoResponseToDis = 1,
    /// T1 expired: no valid command within the identification time.
    T1Expired = 2,
    /// T2 expired while waiting for a command.
    T2ExpiredCommand = 3,
    /// T2 expired while waiting for the image.
    T2ExpiredImage = 4,
    /// The peer sent DCN before delivering any page and before any training failure.
    RemoteDisconnect = 5,
    /// The peer gave up after failure to train (FTT).
    TrainingFailed = 6,
    /// The peer sent DCN in the middle of the procedure after at least one page was accepted.
    PartialDcn = 7,
    /// The DCS requests something this receiver cannot do (ECM, uncompressed, high resolution).
    IncompatibleDcs = 8,
    /// The peer gave up after pages were rejected with RTN.
    PageRejected = 9,
    /// Too many unexpected frames.
    UnexpectedFrames = 10,
    /// The upper layer aborted the call.
    Aborted = 11,
    /// The image decoder could not be set up (internal limits).
    DecoderError = 12,
    /// ECM: the peer kept asking for the same frames (too many PPR) without CTC or EOR.
    EcmFailed = 13,
    /// ECM: the procedure ended normally but the sender gave up on some frames (EOR), so at
    /// least one page has missing data.
    EcmFramesLost = 14,
}

impl ResultCode {
    /// The numeric code (0 = success).
    pub fn as_u8(self) -> u8 {
        self as u8
    }

    /// True only for [`ResultCode::Ok`].
    pub fn is_ok(self) -> bool {
        self == ResultCode::Ok
    }

    /// A stable snake_case name for logs and `result.json`.
    pub fn name(self) -> &'static str {
        match self {
            ResultCode::Ok => "ok",
            ResultCode::NoResponseToDis => "no_response_to_dis",
            ResultCode::T1Expired => "t1_expired",
            ResultCode::T2ExpiredCommand => "t2_expired_command",
            ResultCode::T2ExpiredImage => "t2_expired_image",
            ResultCode::RemoteDisconnect => "remote_disconnect",
            ResultCode::TrainingFailed => "training_failed",
            ResultCode::PartialDcn => "partial_dcn",
            ResultCode::IncompatibleDcs => "incompatible_dcs",
            ResultCode::PageRejected => "page_rejected",
            ResultCode::UnexpectedFrames => "unexpected_frames",
            ResultCode::Aborted => "aborted",
            ResultCode::DecoderError => "decoder_error",
            ResultCode::EcmFailed => "ecm_failed",
            ResultCode::EcmFramesLost => "ecm_frames_lost",
        }
    }
}

/// What a call delivered.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct T30Result {
    /// Result code.
    pub code: ResultCode,
    /// Pages received and confirmed (MCF or RTP).
    pub pages: u32,
    /// Image data rate of the last DCS in bit/s (0 if no DCS was accepted).
    pub bit_rate: u32,
    /// Modem of the last DCS.
    pub modem: Option<Modem>,
    /// Vertical resolution of the last DCS.
    pub resolution: Option<Resolution>,
    /// Coding of the last DCS.
    pub compression: Option<Compression>,
    /// The caller's TSI.
    pub remote_ident: Option<String>,
    /// Bad lines of every confirmed page, in order.
    pub bad_lines_per_page: Vec<u32>,
    /// Time from `Start` to the end in milliseconds.
    pub duration_ms: u64,
    /// True if the last DCS selected the error correction mode.
    pub ecm: bool,
    /// ECM: PPR frames sent (every request for retransmission, repeats included).
    pub ecm_ppr_sent: u32,
    /// ECM: frames the sender abandoned with EOR (their octets are zero filled in the page).
    pub ecm_frames_lost: u32,
}

impl T30Result {
    /// True if the call failed but still delivered pages (partial success).
    pub fn is_partial(&self) -> bool {
        !self.code.is_ok() && self.pages > 0
    }
}
