//! SDP side of call handling: answer building with the trunk's policy (C06/C07), the SDP
//! tolerance rules (C09) and our own T.38 offer.

use std::net::IpAddr;

use sip_core::sdp::negotiate::{AnswerPolicy, MediaPreference, NegotiateError, answer_offer};
use sip_core::sdp::{
    Connection, MediaDescription, OriginTracker, RateManagement, SessionDescription, T38Params,
    Timing, UdpEc,
};

use crate::event::{T38Negotiated, T38Path};
use crate::trunk::Trunk;

/// Local media settings shared by all calls.
#[derive(Debug, Clone)]
pub struct MediaCtx {
    /// Address for `c=` / `o=`.
    pub local_address: String,
    /// Audio port in answers.
    pub audio_port: u16,
    /// UDPTL port in answers and offers.
    pub t38_port: u16,
}

/// What the answer accepted.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Accepted {
    /// G.711 audio.
    Audio,
    /// T.38.
    T38(Box<T38Negotiated>),
}

/// An offer we cannot (or by policy do not) answer; the call gets 488.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{0}")]
pub struct Unacceptable(pub String);

fn rm_name(r: RateManagement) -> &'static str {
    match r {
        RateManagement::LocalTcf => "localTCF",
        RateManagement::TransferredTcf => "transferredTCF",
    }
}

fn ec_name(e: UdpEc) -> &'static str {
    match e {
        UdpEc::Fec => "t38UDPFEC",
        UdpEc::Redundancy => "t38UDPRedundancy",
        UdpEc::NoEc => "t38UDPNoEC",
    }
}

fn socket_text(ip: &str, port: u16) -> String {
    if ip.contains(':') && !ip.starts_with('[') {
        format!("[{ip}]:{port}")
    } else {
        format!("{ip}:{port}")
    }
}

/// Builds the answer to `offer` according to the trunk's T.38 policy.
///
/// * `audio-only`: the audio line is accepted and `m=image` lines get port 0; an offer with
///   nothing but T.38 is unacceptable (488).
/// * every other policy prefers `m=image` when the offer has one (and port 0 for the audio
///   line); if the T.38 line is unusable the audio line is the fallback.
pub fn answer(
    offer: &SessionDescription,
    trunk: &Trunk,
    ctx: &MediaCtx,
    origin: &mut OriginTracker,
    source: IpAddr,
    path: T38Path,
) -> Result<(SessionDescription, Accepted), Unacceptable> {
    let policy = trunk.cfg.t38_policy;
    let has_t38 = offer.media.iter().any(MediaDescription::is_t38);
    let has_audio = offer
        .media
        .iter()
        .any(|m| m.media.eq_ignore_ascii_case("audio"));
    let mut prefs = Vec::new();
    if has_t38 && policy.accepts_t38() {
        prefs.push(MediaPreference::T38);
    }
    if has_audio {
        prefs.push(MediaPreference::Audio);
    }
    if prefs.is_empty() {
        return Err(Unacceptable(if has_t38 {
            "T.38 offered but t38_policy = audio-only and there is no audio line".into()
        } else {
            "the offer has neither an audio nor a T.38 media line".into()
        }));
    }
    let mut last: Option<NegotiateError> = None;
    for pref in prefs {
        let mut ap = AnswerPolicy::new(&ctx.local_address, ctx.audio_port, ctx.t38_port);
        ap.preferred = pref;
        ap.audio_codecs = trunk.cfg.codecs();
        ap.t38 = trunk.cfg.t38_capabilities();
        // answer_offer only touches `origin` on success
        match answer_offer(offer, &ap, origin) {
            Ok(sdp) => {
                let accepted = accepted_of(offer, &sdp, trunk, ctx, source, path);
                return Ok((sdp, accepted));
            }
            Err(e) => last = Some(e),
        }
    }
    Err(Unacceptable(last.map_or_else(
        || "no acceptable media".into(),
        |e| e.to_string(),
    )))
}

fn accepted_of(
    offer: &SessionDescription,
    answer: &SessionDescription,
    trunk: &Trunk,
    ctx: &MediaCtx,
    source: IpAddr,
    path: T38Path,
) -> Accepted {
    let Some((idx, answered)) = answer.media.iter().enumerate().find(|(_, m)| m.port != 0) else {
        return Accepted::Audio;
    };
    if !answered.is_t38() {
        return Accepted::Audio;
    }
    match offer.media.get(idx) {
        Some(peer_m) => Accepted::T38(Box::new(describe(
            offer, peer_m, answered, trunk, ctx, source, path,
        ))),
        None => Accepted::Audio,
    }
}

/// Collects the negotiated parameters and the tolerance decisions (C09).
///
/// `peer_sdp` / `peer_m` is the description that came from the peer (the offer when the peer
/// offered, our answer's counterpart otherwise); `in_force_m` is the media section that holds
/// the parameters both sides settled on (the answer, whoever wrote it).
fn describe(
    peer_sdp: &SessionDescription,
    peer_m: &MediaDescription,
    in_force_m: &MediaDescription,
    trunk: &Trunk,
    ctx: &MediaCtx,
    source: IpAddr,
    path: T38Path,
) -> T38Negotiated {
    let mut warnings = Vec::new();
    let peer = T38Params::from_media(peer_m).unwrap_or_default();
    let in_force = T38Params::from_media(in_force_m).unwrap_or_default();
    if peer_m.attribute("T38FaxRateManagement").is_none() {
        warnings.push(
            "peer SDP has no T38FaxRateManagement; assuming transferredTCF (T.38 default)".into(),
        );
    }
    if peer_m.attribute("T38FaxUdpEC").is_none() {
        warnings.push("peer SDP has no T38FaxUdpEC; assuming t38UDPRedundancy".into());
    }
    let floor = trunk.cfg.compat.min_max_datagram;
    let mut peer_dg = peer.max_datagram;
    if peer_m.attribute("T38FaxMaxDatagram").is_some() && peer_dg < floor {
        warnings.push(format!(
            "peer T38FaxMaxDatagram {peer_dg} is below the floor {floor}; using {floor}"
        ));
        peer_dg = floor;
    }
    let peer_media = match peer_sdp.effective_connection(peer_m) {
        Some(Connection { address, .. }) if !address.trim().is_empty() => {
            Some(socket_text(address.trim(), peer_m.port))
        }
        _ => {
            warnings.push(
                "peer SDP has no usable c= line (neither session nor media level); using the signalling source address"
                    .into(),
            );
            Some(socket_text(&source.to_string(), peer_m.port))
        }
    };
    for w in &warnings {
        tracing::warn!(trunk = %trunk.name(), "{w}");
    }
    T38Negotiated {
        path,
        version: in_force.version,
        max_bit_rate: in_force.max_bit_rate,
        rate_management: rm_name(in_force.rate_management).into(),
        max_buffer: trunk.cfg.t38.max_buffer,
        local_max_datagram: trunk.cfg.t38.max_datagram,
        peer_max_datagram: peer_dg,
        udp_ec: ec_name(in_force.udp_ec).into(),
        fill_bit_removal: in_force.fill_bit_removal,
        transcoding_mmr: in_force.transcoding_mmr,
        transcoding_jbig: in_force.transcoding_jbig,
        peer_media,
        local_port: ctx.t38_port,
        warnings,
    }
}

/// Our T.38 offer for a re-INVITE: a single `m=image <port> udptl t38` section with the
/// trunk's parameters (the shape gateways send themselves).
pub fn t38_offer(trunk: &Trunk, ctx: &MediaCtx, origin: &mut OriginTracker) -> SessionDescription {
    let mut m = MediaDescription::new("image", ctx.t38_port, "udptl", &["t38"]);
    m.attributes = trunk.cfg.t38_offer_params().to_attributes();
    let desc = SessionDescription {
        version: 0,
        origin: origin.current_origin(),
        session_name: Some("-".into()),
        connection: Some(Connection::for_address(&ctx.local_address)),
        times: vec![Timing { start: 0, stop: 0 }],
        attributes: Vec::new(),
        media: vec![m],
        extra: Vec::new(),
    };
    origin.stamp(desc)
}

/// Reads the peer's answer to our T.38 offer. `None` when it carries no usable T.38 line
/// (missing, or port 0 = rejected).
pub fn read_t38_answer(
    answer: &SessionDescription,
    trunk: &Trunk,
    ctx: &MediaCtx,
    source: IpAddr,
) -> Option<T38Negotiated> {
    let m = answer.media.iter().find(|m| m.is_t38() && m.port != 0)?;
    Some(describe(
        answer,
        m,
        m,
        trunk,
        ctx,
        source,
        T38Path::OurReinvite,
    ))
}
