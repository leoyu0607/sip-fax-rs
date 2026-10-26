//! Structured call events and results. M3 persists [`CallResult`]; today it is logged and
//! (with capture enabled) written next to the SIP capture as `result.json`.

use std::time::SystemTime;

use serde::{Deserialize, Serialize};

use crate::numbers::Numbers;
use crate::timefmt::{deserialize_opt_time, deserialize_time, serialize_opt_time, serialize_time};

/// How the T.38 session came about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum T38Path {
    /// The initial INVITE already offered `m=image`.
    InitialInvite,
    /// The peer sent a re-INVITE to T.38.
    PeerReinvite,
    /// We sent the re-INVITE.
    OurReinvite,
}

/// The T.38 parameters in force for a call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct T38Negotiated {
    /// How T.38 was established.
    pub path: T38Path,
    /// `T38FaxVersion`.
    pub version: u8,
    /// `T38MaxBitRate`.
    pub max_bit_rate: u32,
    /// `localTCF` / `transferredTCF`.
    pub rate_management: String,
    /// `T38FaxMaxBuffer` we advertised.
    pub max_buffer: u32,
    /// `T38FaxMaxDatagram` we advertised.
    pub local_max_datagram: u32,
    /// `T38FaxMaxDatagram` the peer offered (after the floor protection).
    pub peer_max_datagram: u32,
    /// `t38UDPFEC` / `t38UDPRedundancy` / `t38UDPNoEC`.
    pub udp_ec: String,
    /// Fill bit removal in force.
    pub fill_bit_removal: bool,
    /// MMR transcoding in force.
    pub transcoding_mmr: bool,
    /// JBIG transcoding in force.
    pub transcoding_jbig: bool,
    /// Where the peer wants UDPTL (`ip:port`), from `c=` and `m=`.
    pub peer_media: Option<String>,
    /// Our UDPTL port.
    pub local_port: u16,
    /// Tolerance decisions taken (missing attributes, raised limits, ...).
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// The G.711 audio of a call that received (or was ready to receive) its fax over audio (M5).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AudioNegotiated {
    /// `PCMU` or `PCMA`: the codec we send (the first of our answer).
    pub codec: String,
    /// Its RTP payload type.
    pub payload_type: u8,
    /// Where the peer wants RTP (`ip:port`), from `c=` and `m=`.
    pub peer_media: Option<String>,
    /// Our RTP port.
    pub local_port: u16,
    /// Why the fax went over audio (`t38_policy = audio-only`, `peer answered 488 to the T.38
    /// re-INVITE`, ...). `None` while the audio only listened (T.38 came instead).
    #[serde(default)]
    pub reason: Option<String>,
}

/// Why a call is counted as failed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum FailureReason {
    /// The caller cancelled before we answered.
    Cancelled,
    /// The INVITE had no (parsable) SDP offer.
    NoSdpOffer,
    /// Nothing in the offer could be answered (488).
    NoAcceptableMedia {
        /// Why.
        detail: String,
    },
    /// `Require` named option tags we do not support (420).
    UnsupportedRequire {
        /// The tags.
        tags: Vec<String>,
    },
    /// The source belongs to no trunk and `unknown_source` is not `log`.
    UnknownSource,
    /// Every UDPTL port of `[media] port_range` was in use (answered 503).
    NoMediaPort,
    /// No ACK for our 200.
    AckTimeout,
    /// T.38 could not be established and this server has no G.711 session (a session factory
    /// without audio, e.g. `fax_session = "hold"`).
    G711PassthroughUnsupported {
        /// What led here ("peer answered 488 to the T.38 re-INVITE", ...).
        cause: String,
    },
    /// Our T.38 re-INVITE failed for another reason (timeout, 5xx, ...).
    T38ReinviteFailed {
        /// Final status, if there was one.
        status: Option<u16>,
        /// Details.
        detail: String,
    },
    /// The peer hung up before T.38 was established.
    PeerHangupBeforeT38,
    /// The stub session / audio hold reached `max_duration_secs`.
    MaxDuration,
    /// The session timer ran out.
    SessionExpired,
    /// The fax session reported a failure.
    Session {
        /// Details.
        detail: String,
    },
    /// The server shut down during the call.
    Shutdown,
    /// Anything else.
    Internal {
        /// Details.
        detail: String,
    },
}

/// The end state of a call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case")]
pub enum CallOutcome {
    /// The fax session (T.38 or G.711) ended normally.
    Completed,
    /// The call failed.
    Failed {
        /// Why.
        reason: FailureReason,
    },
}

impl CallOutcome {
    /// `true` for [`CallOutcome::Failed`].
    pub fn is_failure(&self) -> bool {
        matches!(self, CallOutcome::Failed { .. })
    }
}

/// UDPTL counters of one T.38 session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UdptlCounters {
    /// Datagrams received from the peer (all, including ones that did not parse).
    pub rx_datagrams: u64,
    /// Datagrams sent to the peer.
    pub tx_datagrams: u64,
    /// Received datagrams ignored because they came from another address than the latched one.
    pub rx_foreign: u64,
    /// Received datagrams that failed to parse (or whose IFP packet did not decode).
    pub rx_malformed: u64,
    /// Duplicate datagrams.
    pub rx_duplicates: u64,
    /// Packets recovered from redundancy.
    pub recovered_redundancy: u64,
    /// Packets recovered from FEC.
    pub recovered_fec: u64,
    /// Packets lost for good (reported to T.30 as damaged data).
    pub lost: u64,
}

/// How the fax travelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FaxMedia {
    /// T.38 over UDPTL (0.4 and older reports have no field: they were all T.38).
    #[default]
    T38,
    /// G.711 passthrough: the modems on the audio.
    G711,
}

/// RTP counters of one G.711 session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RtpCounters {
    /// Datagrams received from the peer (all, including ones that did not parse).
    pub rx_packets: u64,
    /// Packets sent to the peer.
    pub tx_packets: u64,
    /// Received datagrams ignored because they came from another address than the latched one.
    pub rx_foreign: u64,
    /// Datagrams that are not RTP, or carry another payload type than PCMU / PCMA.
    pub rx_ignored: u64,
    /// Packets that arrived after their time had been played.
    pub late: u64,
    /// Duplicate packets.
    pub duplicate: u64,
    /// Restarts of the receive buffer (new SSRC, timestamp jump).
    pub resyncs: u64,
    /// Received audio missing (lost packets), in milliseconds.
    pub lost_ms: u64,
}

/// What the T.38 / T.30 fax session delivered. Present in [`CallResult`] when a fax session
/// ran, whether it succeeded or not.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FaxReport {
    /// T.30 result code (0 = success).
    pub code: u8,
    /// Stable name of the code (`ok`, `t2_expired_command`, ...).
    pub code_name: String,
    /// Pages received and confirmed.
    pub pages: u32,
    /// Image data rate of the last DCS in bit/s.
    pub bit_rate: u32,
    /// Modem of the last DCS (`v27ter`, `v29`, `v17`, ...).
    pub modem: Option<String>,
    /// Vertical resolution (`standard`, `fine`, ...).
    pub resolution: Option<String>,
    /// Coding of the last DCS (`mh`, `mr`, `mmr`).
    pub compression: Option<String>,
    /// The caller's TSI.
    pub remote_ident: Option<String>,
    /// Bad lines of every confirmed page.
    pub bad_lines_per_page: Vec<u32>,
    /// T.30 duration in milliseconds.
    pub duration_ms: u64,
    /// The call failed but pages were received and stored.
    pub partial: bool,
    /// The TIFF file, if any page was received.
    pub tiff: Option<String>,
    /// The PDF file, if any page was received.
    pub pdf: Option<String>,
    /// Why the files could not be written, if that failed.
    pub store_error: Option<String>,
    /// UDPTL counters (all zero for G.711).
    pub udptl: UdptlCounters,
    /// T.38 or G.711.
    #[serde(default)]
    pub media: FaxMedia,
    /// RTP counters (G.711 only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rtp: Option<RtpCounters>,
    /// Size of the TIFF file in bytes (0.1.1 and older `result.json` files lack it).
    #[serde(default)]
    pub tiff_size: Option<u64>,
    /// Size of the PDF file in bytes.
    #[serde(default)]
    pub pdf_size: Option<u64>,
    /// The last DCS selected the error correction mode (negotiation result).
    #[serde(default)]
    pub ecm: bool,
    /// ECM: PPR frames we sent (requests for retransmission, repeats included).
    #[serde(default)]
    pub ecm_ppr_sent: u32,
    /// ECM: frames the sender abandoned with EOR (zero filled in the page).
    #[serde(default)]
    pub ecm_frames_lost: u32,
}

/// Where a session leaves its [`FaxReport`] for the controller to pick up.
pub type ReportSink = std::sync::Arc<std::sync::Mutex<Option<FaxReport>>>;

/// Who ended the SIP dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ByeBy {
    /// The peer sent the BYE.
    Remote,
    /// We sent the BYE.
    Local,
}

/// Everything known about a finished call.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CallResult {
    /// SIP Call-ID.
    pub call_id: String,
    /// Trunk the call was mapped to (`unknown` for unmatched sources in log mode).
    pub trunk: String,
    /// Source of the INVITE.
    pub peer: String,
    /// `From` URI.
    pub from: String,
    /// `To` URI.
    pub to: String,
    /// Calling / called number (docs/design/m3-store.md section 2).
    #[serde(default)]
    pub numbers: Numbers,
    /// INVITE received.
    #[serde(
        serialize_with = "serialize_time",
        deserialize_with = "deserialize_time"
    )]
    pub started_at: SystemTime,
    /// 200 sent.
    #[serde(
        serialize_with = "serialize_opt_time",
        deserialize_with = "deserialize_opt_time",
        default
    )]
    pub answered_at: Option<SystemTime>,
    /// T.38 in force.
    #[serde(
        serialize_with = "serialize_opt_time",
        deserialize_with = "deserialize_opt_time",
        default
    )]
    pub t38_established_at: Option<SystemTime>,
    /// Call finished.
    #[serde(
        serialize_with = "serialize_time",
        deserialize_with = "deserialize_time"
    )]
    pub ended_at: SystemTime,
    /// Who sent the BYE (`None` when the dialog never existed, or ended without a BYE).
    #[serde(default)]
    pub bye_by: Option<ByeBy>,
    /// Negotiated T.38 parameters, if T.38 was established.
    #[serde(default)]
    pub t38: Option<T38Negotiated>,
    /// The G.711 audio, if the fax went over audio.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub audio: Option<AudioNegotiated>,
    /// What the fax session delivered (T.30 result, pages, files), if one ran.
    #[serde(default)]
    pub fax: Option<FaxReport>,
    /// The result.
    pub outcome: CallOutcome,
}

/// Something that happened to a call, in order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CallEvent {
    /// An INVITE was mapped to a trunk and is being handled.
    Started {
        /// Call-ID.
        call_id: String,
        /// Trunk name.
        trunk: String,
        /// Source.
        peer: String,
        /// Calling and called number extracted from the INVITE.
        numbers: Numbers,
        /// When.
        at: SystemTime,
    },
    /// We answered with 200 (`t38` tells whether the answer was already T.38).
    Answered {
        /// Call-ID.
        call_id: String,
        /// Whether the answer carried T.38.
        t38: bool,
        /// When.
        at: SystemTime,
    },
    /// T.38 is in force (the fax session starts).
    T38Established {
        /// Call-ID.
        call_id: String,
        /// Parameters.
        params: T38Negotiated,
        /// When.
        at: SystemTime,
    },
    /// T.30 confirmed another page (MCF, or RTP): `pages` pages are in so far. Sent for ECM and
    /// non-ECM calls alike.
    PageReceived {
        /// Call-ID.
        call_id: String,
        /// Pages received and confirmed so far.
        pages: u32,
        /// When.
        at: SystemTime,
    },
    /// The call is over.
    Finished(Box<CallResult>),
}
