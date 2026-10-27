//! UAS-side pieces of the dialog layer: dialog creation from an INVITE and the 2xx
//! retransmission schedule (RFC 3261 13.3.1.4).

use std::net::SocketAddr;
use std::time::Duration;

use super::{Dialog, DialogError, DialogId, Role};
use crate::message::{NameAddr, Request, SipUri, Uri};
use crate::transaction::TimerConfig;
use crate::transport::TransportKind;

/// Creates the UAS dialog for `invite`, answered with our `local_tag` (RFC 3261 12.1.1):
/// route set = Record-Route in order, remote target = Contact (the source address when the
/// INVITE has none), remote CSeq = the INVITE's, local CSeq random below 2^31.
pub fn dialog_from_invite(
    invite: &Request,
    local_tag: &str,
    local_contact: NameAddr,
    source: SocketAddr,
    transport: TransportKind,
) -> Result<Dialog, DialogError> {
    let bad = |what: &str| DialogError::Invalid(what.to_string());
    let call_id = invite
        .headers
        .call_id()
        .ok_or_else(|| bad("no Call-ID"))?
        .to_string();
    let from = invite.headers.from().map_err(|e| bad(&e.to_string()))?;
    let mut to = invite.headers.to().map_err(|e| bad(&e.to_string()))?;
    let cseq = invite.headers.cseq().map_err(|e| bad(&e.to_string()))?;
    let remote_tag = from.tag().unwrap_or("").to_string();
    to.set_tag(local_tag);
    let route_set = invite
        .headers
        .record_routes()
        .map_err(|e| bad(&e.to_string()))?;
    let remote_target = match invite.headers.contacts() {
        Ok(c) if !c.is_empty() => c[0].uri.clone(),
        _ => {
            let mut u = SipUri::new(crate::net::uri_host(&source.ip().to_string()));
            u.port = Some(source.port());
            Uri::Sip(u)
        }
    };
    Ok(Dialog {
        id: DialogId {
            call_id,
            local_tag: local_tag.to_string(),
            remote_tag,
        },
        role: Role::Uas,
        local: to,
        remote: from,
        local_contact,
        remote_target,
        route_set,
        local_seq: rand::random::<u32>() >> 1,
        remote_seq: Some(cseq.seq),
        transport,
        peer: source,
    })
}

/// The 2xx retransmission timer: T1, doubling, capped at T2 (RFC 3261 13.3.1.4).
#[derive(Debug, Clone)]
pub struct RetransmitSchedule {
    interval: Duration,
    t2: Duration,
}

impl RetransmitSchedule {
    /// Starts at T1.
    pub fn new(cfg: &TimerConfig) -> Self {
        RetransmitSchedule {
            interval: cfg.t1,
            t2: cfg.t2,
        }
    }

    /// Delay until the next retransmission, then advances the schedule.
    pub fn next_delay(&mut self) -> Duration {
        let d = self.interval;
        self.interval = (self.interval * 2).min(self.t2);
        d
    }
}

#[cfg(test)]
mod tests;
