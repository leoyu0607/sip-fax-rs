//! SIP UAC mode: places a call into a SIP fax server through sip-core's UAC API. The T.38
//! switch can happen three ways: T.38 in the initial INVITE, a gateway-initiated re-INVITE
//! after a G.711 call is up, or a server-initiated re-INVITE the UAC answers.
//!
//! Without [`UacConfig::fax`] the call only verifies the signalling (hold, BYE). With it, once
//! T.38 is established the UAC behaves as a real gateway: SpanDSP's `t38_terminal` sends a TIFF
//! over a real UDP socket ([`crate::gateway`]) and the call ends with the server's BYE (or ours).

use std::net::{SocketAddr, UdpSocket};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use crate::gateway::{FaxRun, FaxTx, run_fax};
use t38::ErrorRecovery;

use sip_core::endpoint::{DialogEvent, Endpoint, EndpointConfig, InviteOptions, ReinviteOutcome};
use sip_core::message::SipUri;
use sip_core::sdp::negotiate::{AnswerPolicy, MediaPreference, OriginTracker, answer_offer};
use sip_core::sdp::{
    Attribute, Connection, MediaDescription, SessionDescription, Timing, parse_sdp,
};
use sip_core::transport::TransportKind;

/// Who sends the T.38 re-INVITE.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReinviteBy {
    /// We (the simulated gateway) send it right after the call is up.
    Gateway,
    /// We wait for the server's re-INVITE and answer it.
    Server,
    /// No re-INVITE: the initial INVITE already offers T.38 (`m=image` only).
    InitialInvite,
}

/// Parameters of one simulated call.
#[derive(Debug, Clone)]
pub struct UacConfig {
    /// The server's SIP address.
    pub target: SocketAddr,
    /// UDP or TCP.
    pub transport: TransportKind,
    /// Who sends the T.38 re-INVITE.
    pub reinvite_by: ReinviteBy,
    /// How long to wait for the server's re-INVITE ([`ReinviteBy::Server`]).
    pub wait_for_server: Duration,
    /// How long the call stays up after T.38 is established, before our BYE.
    pub hold: Duration,
    /// Local address to bind (port 0 = any).
    pub local: SocketAddr,
    /// `T38MaxBitRate` we offer / answer with.
    pub max_bit_rate: u32,
    /// Send a fax once T.38 is established (otherwise the call is only held for `hold`).
    pub fax: Option<FaxTx>,
    /// With `fax`: how long to wait for the server's BYE after the transfer before sending ours.
    pub peer_bye_wait: Duration,
    /// `T38FaxVersion` we offer / answer with.
    pub t38_version: u8,
    /// `T38FaxRateManagement` we offer.
    pub rate_management: &'static str,
}

impl UacConfig {
    /// Defaults for a call to `target` on UDP.
    pub fn new(target: SocketAddr, reinvite_by: ReinviteBy) -> Self {
        UacConfig {
            target,
            transport: TransportKind::Udp,
            reinvite_by,
            wait_for_server: Duration::from_secs(15),
            hold: Duration::from_millis(100),
            local: SocketAddr::from(([127, 0, 0, 1], 0)),
            max_bit_rate: 14_400,
            fax: None,
            peer_bye_wait: Duration::from_secs(3),
            t38_version: 0,
            rate_management: "transferredTCF",
        }
    }
}

fn udp_ec_attr(r: ErrorRecovery) -> &'static str {
    match r {
        ErrorRecovery::Fec { .. } => "t38UDPFEC",
        ErrorRecovery::None => "t38UDPNoEC",
        ErrorRecovery::Redundancy { .. } => "t38UDPRedundancy",
    }
}

/// What happened on the wire, as seen by the simulated gateway.
#[derive(Debug, Clone)]
pub struct UacReport {
    /// SIP Call-ID.
    pub call_id: String,
    /// The server's answer to the G.711 INVITE.
    pub initial_answer: Option<SessionDescription>,
    /// The T.38 offer the server sent ([`ReinviteBy::Server`]).
    pub server_t38_offer: Option<SessionDescription>,
    /// The T.38 answer the server gave to our re-INVITE ([`ReinviteBy::Gateway`]).
    pub server_t38_answer: Option<SessionDescription>,
    /// Final status of our BYE (0 when the server hung up first and we did not send one).
    pub bye_status: u16,
    /// The server sent BYE (we answered it).
    pub peer_bye: bool,
    /// The UDP port of our media socket (the port in our T.38 offer / answer).
    pub media_port: u16,
    /// The fax transfer, when [`UacConfig::fax`] was set.
    pub fax: Option<FaxRun>,
}

/// The G.711 offer of a gateway (PCMU, PCMA, telephone-event).
pub fn audio_offer(address: &str, port: u16) -> SessionDescription {
    let text = format!(
        "v=0\r\no=fax-sim 1523094871 1523094871 IN IP4 {address}\r\ns=SIP Media Capabilities\r\n\
         c=IN IP4 {address}\r\nt=0 0\r\nm=audio {port} RTP/AVP 0 8 101\r\na=rtpmap:0 PCMU/8000\r\n\
         a=rtpmap:8 PCMA/8000\r\na=rtpmap:101 telephone-event/8000\r\na=fmtp:101 0-15\r\n\
         a=ptime:20\r\na=sendrecv\r\n"
    );
    parse_sdp(text.as_bytes()).expect("static SDP parses")
}

/// A gateway-style T.38 re-INVITE offer.
pub fn t38_offer(address: &str, port: u16, max_bit_rate: u32) -> SessionDescription {
    t38_offer_with(
        address,
        port,
        max_bit_rate,
        0,
        "transferredTCF",
        "t38UDPRedundancy",
    )
}

/// A gateway-style T.38 offer with the given version, rate management and UDPTL EC.
pub fn t38_offer_with(
    address: &str,
    port: u16,
    max_bit_rate: u32,
    version: u8,
    rate_management: &str,
    udp_ec: &str,
) -> SessionDescription {
    let mut m = MediaDescription::new("image", port, "udptl", &["t38"]);
    m.attributes = vec![
        Attribute::value("T38FaxVersion", version.to_string()),
        Attribute::value("T38MaxBitRate", max_bit_rate.to_string()),
        Attribute::value("T38FaxRateManagement", rate_management),
        Attribute::value("T38FaxMaxBuffer", "1800"),
        Attribute::value("T38FaxMaxDatagram", "260"),
        Attribute::value("T38FaxUdpEC", udp_ec),
    ];
    let mut o = OriginTracker::random(address);
    let desc = SessionDescription {
        version: 0,
        origin: o.current_origin(),
        session_name: Some("-".into()),
        connection: Some(Connection::for_address(address)),
        times: vec![Timing { start: 0, stop: 0 }],
        attributes: Vec::new(),
        media: vec![m],
        extra: Vec::new(),
    };
    o.stamp(desc)
}

/// Places the call and runs it to the BYE.
pub async fn run_call(cfg: UacConfig) -> Result<UacReport, String> {
    let mut ec = EndpointConfig::new().udp(cfg.local);
    if cfg.transport == TransportKind::Tcp {
        ec = ec.tcp(cfg.local);
    }
    let (ep, _incoming) = Endpoint::bind(ec).await.map_err(|e| format!("bind: {e}"))?;
    let r = call(&ep, &cfg).await;
    ep.shutdown().await;
    r
}

/// The UDPTL address (`c=` and `m=image` port) in an SDP.
pub fn t38_remote(sdp: &SessionDescription) -> Option<SocketAddr> {
    let m = sdp.media.iter().find(|m| m.is_t38() && m.port != 0)?;
    let c = sdp.effective_connection(m)?;
    let ip: std::net::IpAddr = c.address.trim().parse().ok()?;
    Some(SocketAddr::new(ip, m.port))
}

async fn call(ep: &Endpoint, cfg: &UacConfig) -> Result<UacReport, String> {
    let local = match cfg.transport {
        TransportKind::Udp => ep.udp_addr(),
        TransportKind::Tcp => ep.tcp_addr(),
    }
    .ok_or("no local address")?;
    let ip = local.ip().to_string();
    // The media socket exists before the first SDP so that datagrams the server sends right
    // after it answers are queued, not lost.
    let media = UdpSocket::bind(SocketAddr::new(local.ip(), 0))
        .map_err(|e| format!("media socket: {e}"))?;
    let media_port = media
        .local_addr()
        .map_err(|e| format!("media socket: {e}"))?
        .port();
    let ec_attr = cfg
        .fax
        .as_ref()
        .map_or("t38UDPRedundancy", |f| udp_ec_attr(f.recovery));
    let t38_offer = |port: u16| {
        t38_offer_with(
            &ip,
            port,
            cfg.max_bit_rate,
            cfg.t38_version,
            cfg.rate_management,
            ec_attr,
        )
    };
    let uri = |user: &str, host: String| {
        SipUri::parse(&format!("sip:{user}@{host}")).map_err(|e| format!("uri: {e}"))
    };
    let first_offer = match cfg.reinvite_by {
        ReinviteBy::InitialInvite => t38_offer(media_port),
        _ => audio_offer(&ip, 16384),
    };
    let mut opts = InviteOptions::new(
        cfg.target,
        uri("fax", cfg.target.to_string())?,
        uri("gw", ip.clone())?,
        uri("fax", cfg.target.to_string())?,
        first_offer,
    );
    opts.transport = cfg.transport;
    opts.timeout = Duration::from_secs(30);
    let mut dlg = ep.invite(opts).await.map_err(|e| format!("INVITE: {e}"))?;
    let mut report = UacReport {
        call_id: dlg.id().call_id.clone(),
        initial_answer: dlg.remote_sdp().cloned(),
        server_t38_offer: None,
        server_t38_answer: None,
        bye_status: 0,
        peer_bye: false,
        media_port,
        fax: None,
    };
    let remote: Option<SocketAddr>;
    match cfg.reinvite_by {
        ReinviteBy::InitialInvite => {
            let a = report
                .initial_answer
                .clone()
                .ok_or("the 200 to the T.38 INVITE has no SDP")?;
            remote = t38_remote(&a);
            report.server_t38_answer = Some(a);
        }
        ReinviteBy::Gateway => {
            match dlg
                .reinvite(t38_offer(media_port))
                .await
                .map_err(|e| format!("re-INVITE: {e}"))?
            {
                ReinviteOutcome::Accepted(a) => {
                    remote = t38_remote(&a);
                    report.server_t38_answer = Some(a);
                }
                other => return Err(format!("the server did not accept T.38: {other:?}")),
            }
        }
        ReinviteBy::Server => {
            let wait = tokio::time::timeout(cfg.wait_for_server, async {
                loop {
                    match dlg.next_event().await {
                        Some(DialogEvent::ReInvite { offer, responder }) => {
                            let offer = offer.ok_or("the server's re-INVITE has no SDP")?;
                            let mut policy = AnswerPolicy::new(&ip, 16384, media_port);
                            policy.preferred = MediaPreference::T38;
                            policy.t38.max_bit_rate = cfg.max_bit_rate;
                            policy.t38.max_version = cfg.t38_version;
                            let mut origin = OriginTracker::random(&ip);
                            let answer = answer_offer(&offer, &policy, &mut origin)
                                .map_err(|e| format!("cannot answer the T.38 offer: {e}"))?;
                            responder
                                .accept(answer)
                                .await
                                .map_err(|e| format!("answering the re-INVITE: {e}"))?;
                            return Ok::<_, String>(offer);
                        }
                        Some(DialogEvent::Established) => {}
                        Some(other) => return Err(format!("unexpected dialog event {other:?}")),
                        None => return Err("the dialog ended before the server's re-INVITE".into()),
                    }
                }
            })
            .await
            .map_err(|_| "no re-INVITE from the server in time".to_string())??;
            remote = t38_remote(&wait);
            report.server_t38_offer = Some(wait);
        }
    }
    let Some(fax) = cfg.fax.clone() else {
        tokio::time::sleep(cfg.hold).await;
        report.bye_status = dlg.bye().await.map_err(|e| format!("BYE: {e}"))?.code();
        return Ok(report);
    };

    // The gateway proper: SpanDSP sends the fax over the media socket.
    let remote = remote.ok_or("the server's T.38 SDP has no usable address")?;
    let stop = Arc::new(AtomicBool::new(false));
    let stop2 = stop.clone();
    let mut job = tokio::task::spawn_blocking(move || run_fax(&media, remote, &fax, &stop2));
    let run = loop {
        tokio::select! {
            r = &mut job => break r.map_err(|e| format!("fax thread: {e}"))??,
            ev = dlg.next_event() => match ev {
                Some(DialogEvent::Bye) => {
                    report.peer_bye = true;
                    stop.store(true, Ordering::Relaxed);
                }
                Some(DialogEvent::ReInvite { .. }) => {} // dropped: answered 500
                Some(_) => {}
                None => stop.store(true, Ordering::Relaxed),
            },
        }
    };
    report.fax = Some(run);
    if !report.peer_bye {
        // the server normally hangs up as soon as the fax is complete
        let _ = tokio::time::timeout(cfg.peer_bye_wait, async {
            while let Some(ev) = dlg.next_event().await {
                if matches!(ev, DialogEvent::Bye) {
                    return true;
                }
            }
            false
        })
        .await
        .map(|b| report.peer_bye = b);
    }
    if !report.peer_bye {
        report.bye_status = dlg.bye().await.map_err(|e| format!("BYE: {e}"))?.code();
    }
    Ok(report)
}
