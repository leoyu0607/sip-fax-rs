//! Offer/answer (RFC 3264): audio answers (PCMU / PCMA / telephone-event), T.38 answers,
//! rejected m-lines, and `o=` version bookkeeping.

use super::t38::{T38Capabilities, T38Params};
use super::{
    Attribute, Connection, MediaDescription, Origin, SdpError, SessionDescription, Timing,
};

/// Why an offer cannot be answered.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum NegotiateError {
    /// No offered audio codec is in our preference list (answer 488).
    #[error("no common audio codec")]
    NoCommonCodec,
    /// The offer contains no media section we are willing to accept (answer 488).
    #[error("no acceptable media section in the offer")]
    NoAcceptableMedia,
    /// The media transport protocol is not supported (e.g. RTP/SAVP).
    #[error("unsupported media transport protocol {0}")]
    UnsupportedTransport(String),
    /// The T.38 attributes of the offer are invalid.
    #[error("invalid T.38 offer: {0}")]
    InvalidT38(#[from] SdpError),
}

/// An audio codec we can answer with.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AudioCodec {
    /// Encoding name as used in `a=rtpmap`.
    pub name: &'static str,
    /// Static payload type.
    pub payload_type: u8,
    /// Clock rate.
    pub clock_rate: u32,
}

impl AudioCodec {
    /// G.711 mu-law.
    pub const PCMU: AudioCodec = AudioCodec {
        name: "PCMU",
        payload_type: 0,
        clock_rate: 8000,
    };
    /// G.711 A-law.
    pub const PCMA: AudioCodec = AudioCodec {
        name: "PCMA",
        payload_type: 8,
        clock_rate: 8000,
    };
}

/// Which media section the answer should accept when the offer has several.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MediaPreference {
    /// Accept the first audio section, reject others.
    Audio,
    /// Accept the first `m=image ... t38` section, reject others.
    T38,
    /// Accept the first section (in offer order) that can be answered, reject others.
    FirstOffered,
}

/// Local settings for building an answer.
#[derive(Debug, Clone)]
pub struct AnswerPolicy {
    /// Which media to accept.
    pub preferred: MediaPreference,
    /// Audio codecs in preference order (default PCMU, PCMA).
    pub audio_codecs: Vec<AudioCodec>,
    /// T.38 capabilities.
    pub t38: T38Capabilities,
    /// Local media address (`c=` and `o=`).
    pub local_address: String,
    /// Local RTP port for audio.
    pub audio_port: u16,
    /// Local UDPTL port for T.38.
    pub t38_port: u16,
}

impl AnswerPolicy {
    /// Policy with default codecs/capabilities for the given media address and ports.
    pub fn new(local_address: &str, audio_port: u16, t38_port: u16) -> Self {
        AnswerPolicy {
            preferred: MediaPreference::FirstOffered,
            audio_codecs: vec![AudioCodec::PCMU, AudioCodec::PCMA],
            t38: T38Capabilities::default(),
            local_address: local_address.into(),
            audio_port,
            t38_port,
        }
    }
}

fn is_rtp_avp(proto: &str) -> bool {
    proto.eq_ignore_ascii_case("RTP/AVP")
}

/// Encoding of an offered payload type: from `a=rtpmap`, else the static assignment.
fn offered_encoding(m: &MediaDescription, pt: u8) -> Option<(String, u32)> {
    if let Some(r) = m.rtpmaps().into_iter().find(|r| r.payload_type == pt) {
        return Some((r.encoding, r.clock_rate));
    }
    match pt {
        0 => Some(("PCMU".into(), 8000)),
        8 => Some(("PCMA".into(), 8000)),
        _ => None,
    }
}

/// Builds the audio answer for an `m=audio` offer.
///
/// The answer lists the offered payload types that match `codecs`, in *our* preference order,
/// followed by `telephone-event` if the offer has it. [`NegotiateError::NoCommonCodec`] if no
/// codec matches (telephone-event alone does not count). The direction attribute is mirrored.
pub fn answer_audio(
    offer: &MediaDescription,
    codecs: &[AudioCodec],
    local_port: u16,
) -> Result<MediaDescription, NegotiateError> {
    if !is_rtp_avp(&offer.proto) {
        return Err(NegotiateError::UnsupportedTransport(offer.proto.clone()));
    }
    let offered: Vec<(u8, String, u32)> = offer
        .formats
        .iter()
        .filter_map(|f| f.parse::<u8>().ok())
        .filter_map(|pt| offered_encoding(offer, pt).map(|(n, c)| (pt, n, c)))
        .collect();

    let mut formats = Vec::new();
    let mut attributes = Vec::new();
    for codec in codecs {
        if let Some((pt, _, _)) = offered
            .iter()
            .find(|(_, n, c)| n.eq_ignore_ascii_case(codec.name) && *c == codec.clock_rate)
        {
            formats.push(pt.to_string());
            attributes.push(Attribute::value(
                "rtpmap",
                format!("{} {}/{}", pt, codec.name, codec.clock_rate),
            ));
        }
    }
    if formats.is_empty() {
        return Err(NegotiateError::NoCommonCodec);
    }
    if let Some((pt, _, clock)) = offered
        .iter()
        .find(|(_, n, c)| n.eq_ignore_ascii_case("telephone-event") && *c == 8000)
    {
        formats.push(pt.to_string());
        attributes.push(Attribute::value(
            "rtpmap",
            format!("{pt} telephone-event/{clock}"),
        ));
        if let Some(fmtp) = offer.fmtp(&pt.to_string()) {
            attributes.push(Attribute::value("fmtp", format!("{pt} {fmtp}")));
        }
    }
    let dir = offer.direction();
    if dir != super::Direction::SendRecv {
        attributes.push(Attribute::flag(dir.answered().as_str()));
    }
    Ok(MediaDescription {
        media: offer.media.clone(),
        port: local_port,
        num_ports: None,
        proto: offer.proto.clone(),
        formats,
        connection: None,
        attributes,
        extra: Vec::new(),
    })
}

/// Builds the answer for an `m=image <port> udptl t38` offer.
pub fn answer_t38(
    offer: &MediaDescription,
    caps: &T38Capabilities,
    local_port: u16,
) -> Result<MediaDescription, NegotiateError> {
    let params = T38Params::from_media(offer)?;
    Ok(MediaDescription {
        media: offer.media.clone(),
        port: local_port,
        num_ports: None,
        proto: offer.proto.clone(),
        formats: offer.formats.clone(),
        connection: None,
        attributes: params.answer(caps).to_attributes(),
        extra: Vec::new(),
    })
}

/// Rejects a media section (RFC 3264 section 6): the section stays, port becomes 0, everything
/// else is preserved.
pub fn reject_media(offer: &MediaDescription) -> MediaDescription {
    MediaDescription {
        port: 0,
        ..offer.clone()
    }
}

/// Builds a complete answer to `offer`. Exactly one media section is accepted (chosen by
/// `policy.preferred`); all others are rejected with port 0. The `o=` line is stamped by
/// `origin`.
pub fn answer_offer(
    offer: &SessionDescription,
    policy: &AnswerPolicy,
    origin: &mut OriginTracker,
) -> Result<SessionDescription, NegotiateError> {
    let mut chosen: Option<(usize, MediaDescription)> = None;
    let mut first_err: Option<NegotiateError> = None;
    for (i, m) in offer.media.iter().enumerate() {
        let wanted = match policy.preferred {
            MediaPreference::Audio => m.media.eq_ignore_ascii_case("audio"),
            MediaPreference::T38 => m.is_t38(),
            MediaPreference::FirstOffered => m.is_t38() || m.media.eq_ignore_ascii_case("audio"),
        };
        if !wanted {
            continue;
        }
        let r = if m.is_t38() {
            answer_t38(m, &policy.t38, policy.t38_port)
        } else {
            answer_audio(m, &policy.audio_codecs, policy.audio_port)
        };
        match r {
            Ok(a) => {
                chosen = Some((i, a));
                break;
            }
            Err(e) => {
                if first_err.is_none() {
                    first_err = Some(e);
                }
                if policy.preferred != MediaPreference::FirstOffered {
                    break; // the single wanted section failed: report why
                }
            }
        }
    }
    let Some((idx, accepted)) = chosen else {
        return Err(first_err.unwrap_or(NegotiateError::NoAcceptableMedia));
    };
    let media = offer
        .media
        .iter()
        .enumerate()
        .map(|(i, m)| {
            if i == idx {
                accepted.clone()
            } else {
                reject_media(m)
            }
        })
        .collect();
    let desc = SessionDescription {
        version: 0,
        origin: origin.current_origin(),
        session_name: Some("-".into()),
        connection: Some(Connection::for_address(&policy.local_address)),
        times: vec![Timing { start: 0, stop: 0 }],
        attributes: Vec::new(),
        media,
        extra: Vec::new(),
    };
    Ok(origin.stamp(desc))
}

/// Assigns `o=` session versions (RFC 3264 section 8): the version stays the same while the
/// description is unchanged and is incremented by one whenever it changes.
#[derive(Debug, Clone)]
pub struct OriginTracker {
    username: String,
    sess_id: u64,
    address: String,
    version: u64,
    last: Option<SessionDescription>,
}

impl OriginTracker {
    /// Creates a tracker; the first stamped description gets `initial_version`.
    pub fn new(username: &str, sess_id: u64, initial_version: u64, address: &str) -> Self {
        OriginTracker {
            username: username.into(),
            sess_id,
            address: address.into(),
            version: initial_version,
            last: None,
        }
    }

    /// Tracker with a random session id and version (both below 2^31).
    pub fn random(address: &str) -> Self {
        let id: u32 = rand::random::<u32>() >> 1;
        let ver: u32 = rand::random::<u32>() >> 1;
        Self::new("-", u64::from(id), u64::from(ver), address)
    }

    /// The `o=` line with the current version.
    pub fn current_origin(&self) -> Origin {
        Origin {
            username: self.username.clone(),
            sess_id: self.sess_id,
            sess_version: self.version,
            nettype: "IN".into(),
            addrtype: if self.address.contains(':') {
                "IP6"
            } else {
                "IP4"
            }
            .into(),
            address: self.address.clone(),
        }
    }

    /// Replaces the `o=` line of `desc` and returns it. Compared with the previously stamped
    /// description (ignoring the version itself): identical keeps the version, anything else
    /// increments it.
    pub fn stamp(&mut self, mut desc: SessionDescription) -> SessionDescription {
        desc.origin = self.current_origin();
        let mut normalized = desc.clone();
        normalized.origin.sess_version = 0;
        match &self.last {
            None => {}
            Some(prev) if *prev == normalized => {}
            Some(_) => {
                self.version += 1;
                desc.origin.sess_version = self.version;
            }
        }
        self.last = Some(normalized);
        desc
    }
}
