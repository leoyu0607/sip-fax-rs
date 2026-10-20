//! SIP UAC mode: places a call into a SIP fax server through sip-core's UAC API, offering
//! G.711, then either sends the T.38 re-INVITE itself (gateway-initiated) or waits for the
//! server's, and hangs up. Actual T.38 media comes in M2; today this verifies the signalling
//! of both establishment directions against a real `sip-faxd serve`.

use std::net::SocketAddr;
use std::time::Duration;

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
        }
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
    /// Final status of our BYE.
    pub bye_status: u16,
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
    let mut m = MediaDescription::new("image", port, "udptl", &["t38"]);
    m.attributes = vec![
        Attribute::value("T38FaxVersion", "0"),
        Attribute::value("T38MaxBitRate", max_bit_rate.to_string()),
        Attribute::value("T38FaxRateManagement", "transferredTCF"),
        Attribute::value("T38FaxMaxBuffer", "1800"),
        Attribute::value("T38FaxMaxDatagram", "260"),
        Attribute::value("T38FaxUdpEC", "t38UDPRedundancy"),
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

async fn call(ep: &Endpoint, cfg: &UacConfig) -> Result<UacReport, String> {
    let local = match cfg.transport {
        TransportKind::Udp => ep.udp_addr(),
        TransportKind::Tcp => ep.tcp_addr(),
    }
    .ok_or("no local address")?;
    let ip = local.ip().to_string();
    let uri = |user: &str, host: String| {
        SipUri::parse(&format!("sip:{user}@{host}")).map_err(|e| format!("uri: {e}"))
    };
    let mut opts = InviteOptions::new(
        cfg.target,
        uri("fax", cfg.target.to_string())?,
        uri("gw", ip.clone())?,
        uri("fax", cfg.target.to_string())?,
        audio_offer(&ip, 16384),
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
    };
    match cfg.reinvite_by {
        ReinviteBy::Gateway => {
            match dlg
                .reinvite(t38_offer(&ip, 5004, cfg.max_bit_rate))
                .await
                .map_err(|e| format!("re-INVITE: {e}"))?
            {
                ReinviteOutcome::Accepted(a) => report.server_t38_answer = Some(a),
                other => return Err(format!("the server did not accept T.38: {other:?}")),
            }
        }
        ReinviteBy::Server => {
            let wait = tokio::time::timeout(cfg.wait_for_server, async {
                loop {
                    match dlg.next_event().await {
                        Some(DialogEvent::ReInvite { offer, responder }) => {
                            let offer = offer.ok_or("the server's re-INVITE has no SDP")?;
                            let mut policy = AnswerPolicy::new(&ip, 16384, 5004);
                            policy.preferred = MediaPreference::T38;
                            policy.t38.max_bit_rate = cfg.max_bit_rate;
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
            report.server_t38_offer = Some(wait);
        }
    }
    tokio::time::sleep(cfg.hold).await;
    report.bye_status = dlg.bye().await.map_err(|e| format!("BYE: {e}"))?.code();
    Ok(report)
}
