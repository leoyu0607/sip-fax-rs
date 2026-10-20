//! Dialog layer (docs/design/sip-core.md section 5): dialog state, in-dialog request
//! construction (loose and strict routing), UAS 2xx retransmission, session timers and the
//! actor that runs a dialog ([`task`]).

pub mod session_timer;
pub(crate) mod task;
pub mod uas;

use std::net::SocketAddr;

use crate::message::{Method, NameAddr, Request, Response, Uri};
use crate::transaction::new_branch;
use crate::transport::TransportKind;
use crate::transport::route::DEFAULT_PORT;

/// Dialog identifier (RFC 3261 12): Call-ID plus the local and remote tags.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct DialogId {
    /// Call-ID.
    pub call_id: String,
    /// Our tag.
    pub local_tag: String,
    /// The peer's tag.
    pub remote_tag: String,
}

impl DialogId {
    /// Identifier of the dialog a request *received from the peer* belongs to.
    pub fn from_peer_request(req: &Request) -> Option<DialogId> {
        let to = req.headers.to().ok()?;
        let from = req.headers.from().ok()?;
        Some(DialogId {
            call_id: req.headers.call_id()?.to_string(),
            local_tag: to.tag()?.to_string(),
            remote_tag: from.tag().unwrap_or("").to_string(),
        })
    }

    /// Identifier of the dialog a response *to one of our requests* belongs to.
    pub fn from_our_response(resp: &Response) -> Option<DialogId> {
        let to = resp.headers.to().ok()?;
        let from = resp.headers.from().ok()?;
        Some(DialogId {
            call_id: resp.headers.call_id()?.to_string(),
            local_tag: from.tag()?.to_string(),
            remote_tag: to.tag()?.to_string(),
        })
    }
}

/// Which side created the dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Role {
    /// We answered the INVITE.
    Uas,
    /// We sent the INVITE.
    Uac,
}

/// Dialog layer errors.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum DialogError {
    /// The dialog has ended (BYE, shutdown, ...).
    #[error("dialog terminated")]
    Terminated,
    /// A request or response is unusable for dialog handling.
    #[error("invalid message: {0}")]
    Invalid(String),
    /// The transport refused a message.
    #[error("transport error: {0}")]
    Transport(String),
    /// No transaction response arrived (Timer F / B).
    #[error("request timed out")]
    Timeout,
    /// The responder / request referred to is not (or no longer) pending.
    #[error("no such pending request")]
    NoSuchRequest,
}

/// Where the next in-dialog request goes: the first Route, else the remote target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Hop {
    /// Host (brackets of IPv6 literals removed) or name.
    pub host: String,
    /// Port (5060 when the URI has none).
    pub port: u16,
    /// Transport (from `;transport=`, else the dialog's).
    pub transport: TransportKind,
}

impl Hop {
    /// The socket address when the host is an IP literal.
    pub fn socket_addr(&self) -> Option<SocketAddr> {
        self.host
            .parse::<std::net::IpAddr>()
            .ok()
            .map(|ip| SocketAddr::new(ip, self.port))
    }

    /// Resolves the hop (IP literals without DNS).
    pub async fn resolve(&self) -> Option<SocketAddr> {
        if let Some(a) = self.socket_addr() {
            return Some(a);
        }
        tokio::net::lookup_host((self.host.as_str(), self.port))
            .await
            .ok()
            .and_then(|mut it| it.next())
    }
}

/// A request built for a dialog together with where to send it.
#[derive(Debug, Clone)]
pub struct InDialogRequest {
    /// The request (a placeholder Via is present; the transport stamps the real one).
    pub request: Request,
    /// First hop.
    pub hop: Hop,
}

/// Dialog state (RFC 3261 12.1).
#[derive(Debug, Clone)]
pub struct Dialog {
    /// Identifier.
    pub id: DialogId,
    /// Role.
    pub role: Role,
    /// Our address with our tag (From of requests we send).
    pub local: NameAddr,
    /// The peer's address with its tag (To of requests we send).
    pub remote: NameAddr,
    /// Our Contact.
    pub local_contact: NameAddr,
    /// The peer's Contact (request target).
    pub remote_target: Uri,
    /// Route set, in the order they are visited.
    pub route_set: Vec<NameAddr>,
    /// Our CSeq counter (the last number used).
    pub local_seq: u32,
    /// The peer's last CSeq.
    pub remote_seq: Option<u32>,
    /// Default transport towards the peer.
    pub transport: TransportKind,
    /// Source address of the INVITE (UAS) / the address we sent it to (UAC).
    pub peer: SocketAddr,
}

fn hop_of(uri: &Uri, default: TransportKind) -> Hop {
    match uri.as_sip() {
        Some(u) => {
            let transport = match u.param("transport").flatten() {
                Some(t) if t.eq_ignore_ascii_case("tcp") => TransportKind::Tcp,
                Some(t) if t.eq_ignore_ascii_case("udp") => TransportKind::Udp,
                _ => default,
            };
            Hop {
                host: u
                    .host
                    .trim_start_matches('[')
                    .trim_end_matches(']')
                    .to_string(),
                port: u.port.unwrap_or(DEFAULT_PORT),
                transport,
            }
        }
        None => Hop {
            host: String::new(),
            port: DEFAULT_PORT,
            transport: default,
        },
    }
}

impl Dialog {
    /// Creates the UAC dialog from the INVITE we sent and its (first) 2xx: route set = the
    /// response's Record-Route reversed, remote target = its Contact (the INVITE's
    /// Request-URI when absent).
    pub fn from_2xx(
        invite: &Request,
        resp: &Response,
        local_contact: NameAddr,
        peer: SocketAddr,
        transport: TransportKind,
    ) -> Result<Dialog, DialogError> {
        let bad = |e: &dyn std::fmt::Display| DialogError::Invalid(e.to_string());
        let from = invite.headers.from().map_err(|e| bad(&e))?;
        let to = resp.headers.to().map_err(|e| bad(&e))?;
        let cseq = invite.headers.cseq().map_err(|e| bad(&e))?;
        let mut route_set = resp.headers.record_routes().map_err(|e| bad(&e))?;
        route_set.reverse();
        let remote_target = match resp.headers.contacts() {
            Ok(c) if !c.is_empty() => c[0].uri.clone(),
            _ => invite.uri.clone(),
        };
        Ok(Dialog {
            id: DialogId {
                call_id: invite.headers.call_id().unwrap_or("").to_string(),
                local_tag: from.tag().unwrap_or("").to_string(),
                remote_tag: to.tag().unwrap_or("").to_string(),
            },
            role: Role::Uac,
            local: from,
            remote: to,
            local_contact,
            remote_target,
            route_set,
            local_seq: cseq.seq,
            remote_seq: None,
            transport,
            peer,
        })
    }

    /// `true` when the route set exists and its first entry is a strict router (no `lr`).
    pub fn is_strict_routed(&self) -> bool {
        self.route_set
            .first()
            .and_then(|r| r.uri.as_sip())
            .is_some_and(|u| !u.is_loose_router())
    }

    /// The first hop of the next request.
    pub fn next_hop(&self) -> Hop {
        match self.route_set.first() {
            Some(r) => hop_of(&r.uri, self.transport),
            None => hop_of(&self.remote_target, self.transport),
        }
    }

    /// Builds an in-dialog request (RFC 3261 12.2.1). The CSeq is incremented except for ACK
    /// (use [`Dialog::build_ack`]) and CANCEL.
    ///
    /// * loose routing: Request-URI = remote target, Route = the route set;
    /// * strict routing: Request-URI = first route, Route = the rest plus the remote target.
    pub fn build_request(&mut self, method: Method) -> InDialogRequest {
        self.local_seq += 1;
        let seq = self.local_seq;
        self.assemble(method, seq)
    }

    /// Builds the ACK for the 2xx of the INVITE with CSeq `invite_seq` (own branch, CSeq
    /// number of the INVITE).
    pub fn build_ack(&self, invite_seq: u32) -> InDialogRequest {
        self.assemble(Method::Ack, invite_seq)
    }

    fn assemble(&self, method: Method, seq: u32) -> InDialogRequest {
        let hop = self.next_hop();
        let (uri, routes): (Uri, Vec<String>) = if self.is_strict_routed() {
            let first = self.route_set[0].uri.clone();
            let mut r: Vec<String> = self.route_set[1..].iter().map(|n| n.to_string()).collect();
            r.push(format!("<{}>", self.remote_target));
            (first, r)
        } else {
            (
                self.remote_target.clone(),
                self.route_set.iter().map(|n| n.to_string()).collect(),
            )
        };
        let mut req = Request::new(method.clone(), uri);
        req.headers.push(
            "Via",
            format!(
                "SIP/2.0/{} invalid;branch={}",
                hop.transport.as_str(),
                new_branch()
            ),
        );
        req.headers.push("Max-Forwards", "70");
        for r in routes {
            req.headers.push("Route", r);
        }
        req.headers.push("From", self.local.to_string());
        req.headers.push("To", self.remote.to_string());
        req.headers.push("Call-ID", self.id.call_id.clone());
        req.headers.push("CSeq", format!("{seq} {method}"));
        InDialogRequest { request: req, hop }
    }
}
