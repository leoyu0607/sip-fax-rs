//! High-level endpoint API (docs/design/sip-core.md section 7).
//!
//! ```no_run
//! # async fn demo() -> std::io::Result<()> {
//! use sip_core::endpoint::{Endpoint, EndpointConfig};
//! let cfg = EndpointConfig::new().udp("0.0.0.0:5060".parse().unwrap());
//! let (endpoint, mut incoming) = Endpoint::bind(cfg).await?;
//! while let Some(call) = incoming.recv().await {
//!     // call.request(); call.ringing().await; call.accept(answer).await; call.reject(..)
//!     # let _ = call;
//! }
//! endpoint.shutdown().await;
//! # Ok(()) }
//! ```
//!
//! * UAS: [`Endpoint::bind`] yields [`IncomingCall`]s; [`IncomingCall::accept`] sends the 200,
//!   retransmits it until the ACK arrives and returns a [`DialogHandle`].
//! * UAC: [`Endpoint::invite`] sends the INVITE and returns a [`DialogHandle`] once the 2xx is
//!   in; the ACK is sent automatically and re-sent whenever the 2xx is retransmitted.
//! * [`DialogHandle::reinvite`], [`DialogHandle::bye`], [`DialogHandle::next_event`] work the
//!   same on both sides. All handles are `Send`.
//! * [`Endpoint::shutdown`] ends every task the endpoint started.

use std::io;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot, watch};

use crate::auth::DigestChallenge;
use crate::dialog::session_timer::{self, Negotiation, Refresher, SessionTimerConfig};
use crate::dialog::task::{self, DialogInput, Purpose, TaskInit, build_cancel};
use crate::dialog::{Dialog, DialogId, uas};
use crate::message::{
    Headers, Method, NameAddr, Param, Request, Response, SipMessage, SipUri, StatusCode, Uri,
};
use crate::net;
use crate::sdp::{SessionDescription, parse_sdp};
use crate::transaction::{
    IncomingRequest, ServerTx, TimerConfig, TransactionLayer, TuEvent, TuInput, new_branch,
};
use crate::transport::{
    Incoming, MessageTap, Tasks, TransportConfig, TransportHandle, TransportKind, TransportLayer,
    wait_shutdown,
};

pub use crate::dialog::DialogError;

/// Digest credentials used to answer 401 / 407 to our own requests.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Credentials {
    /// User name.
    pub username: String,
    /// Password.
    pub password: String,
}

/// Endpoint settings.
#[derive(Debug, Clone)]
pub struct EndpointConfig {
    /// Sockets, advertised address, source policy, optional message tap.
    pub transport: TransportConfig,
    /// Transaction timers.
    pub timers: TimerConfig,
    /// `User-Agent` / `Server` value.
    pub user_agent: Option<String>,
    /// User part of our Contact URI (none by default).
    pub contact_user: Option<String>,
    /// Transport our Contact advertises when a call has none of its own (UDP unless only TCP
    /// is bound).
    pub allow: String,
    /// Value of the `Supported` header we send.
    pub supported: String,
    /// Option tags we understand in `Require` (lower case).
    pub understood_options: Vec<String>,
    /// Accept INVITEs whose `Require` lists tags we do not understand (default: answer 420).
    pub accept_unknown_require: bool,
    /// Session timer settings (RFC 4028).
    pub session_timer: SessionTimerConfig,
    /// Credentials for answering 401 / 407 to our in-dialog requests (BYE, re-INVITE,
    /// INVITE).
    pub credentials: Option<Credentials>,
    /// Application-level limit for our re-INVITE to get a final answer (default 32 s).
    pub reinvite_timeout: Duration,
    /// How often a re-INVITE answered with 491 is retried before `Rejected(491)` is reported.
    pub max_491_retries: u32,
    /// Answer unparseable requests with 400 when possible (default true).
    pub answer_malformed: bool,
}

impl Default for EndpointConfig {
    fn default() -> Self {
        EndpointConfig {
            transport: TransportConfig::default(),
            timers: TimerConfig::default(),
            user_agent: None,
            contact_user: None,
            allow: "INVITE, ACK, CANCEL, BYE, OPTIONS, INFO, UPDATE".into(),
            supported: "timer".into(),
            understood_options: vec!["timer".into()],
            accept_unknown_require: false,
            session_timer: SessionTimerConfig::default(),
            credentials: None,
            reinvite_timeout: Duration::from_secs(32),
            max_491_retries: 3,
            answer_malformed: true,
        }
    }
}

impl EndpointConfig {
    /// Defaults, nothing bound yet.
    pub fn new() -> Self {
        Self::default()
    }

    /// Binds UDP on `addr`.
    #[must_use]
    pub fn udp(mut self, addr: SocketAddr) -> Self {
        self.transport.udp_bind = Some(addr);
        self
    }

    /// Listens for TCP on `addr`.
    #[must_use]
    pub fn tcp(mut self, addr: SocketAddr) -> Self {
        self.transport.tcp_bind = Some(addr);
        self
    }

    /// Installs a message tap (see [`MessageTap`]).
    #[must_use]
    pub fn tap(mut self, tap: MessageTap) -> Self {
        self.transport.tap = Some(tap);
        self
    }
}

pub(crate) struct EpInner {
    pub(crate) cfg: EndpointConfig,
    pub(crate) layer: TransactionLayer,
    pub(crate) tasks: Tasks,
    dialogs: Mutex<std::collections::HashMap<DialogId, mpsc::UnboundedSender<DialogInput>>>,
    stop: watch::Sender<bool>,
    transport: Mutex<Option<TransportLayer>>,
}

fn lock<T>(m: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

fn random_tag() -> String {
    format!("{:08x}", rand::random::<u32>())
}

impl EpInner {
    pub(crate) fn unregister(&self, id: &DialogId) {
        lock(&self.dialogs).remove(id);
    }

    fn lookup(&self, id: &DialogId) -> Option<mpsc::UnboundedSender<DialogInput>> {
        lock(&self.dialogs).get(id).cloned()
    }

    /// Adds `Allow`, `Supported` and `User-Agent` unless already present.
    pub(crate) fn decorate(&self, h: &mut Headers) {
        if !h.contains("Allow") && !self.cfg.allow.is_empty() {
            h.push("Allow", self.cfg.allow.clone());
        }
        if !h.contains("Supported") && !self.cfg.supported.is_empty() {
            h.push("Supported", self.cfg.supported.clone());
        }
        if let Some(ua) = &self.cfg.user_agent {
            if !h.contains("User-Agent") {
                h.push("User-Agent", ua.clone());
            }
        }
    }

    /// Headers of an OPTIONS answer.
    pub(crate) fn options_headers(&self, h: &mut Headers) {
        self.decorate(h);
        h.push("Accept", "application/sdp");
    }

    /// Our Contact for a call on `kind`.
    pub(crate) fn contact_for(&self, kind: TransportKind) -> NameAddr {
        let (host, port) = self.layer.transport().sent_by(kind);
        let mut u = SipUri::new(net::uri_host(host));
        u.user = self.cfg.contact_user.clone();
        u.port = Some(port);
        if kind == TransportKind::Tcp {
            u.params.push(Param {
                name: "transport".into(),
                value: Some("tcp".into()),
            });
        }
        NameAddr::new(Uri::Sip(u))
    }

    async fn quick_reply(
        &self,
        ir: &IncomingRequest,
        status: StatusCode,
        extra: impl FnOnce(&mut Response),
    ) {
        let mut r = Response::for_request(&ir.request, status, Some(&random_tag()));
        self.decorate(&mut r.headers);
        extra(&mut r);
        let _ = ir.tx.respond(r).await;
    }

    /// The `Require` tags of `req` that we do not understand.
    fn unsupported_requires(&self, req: &Request) -> Vec<String> {
        req.headers
            .get_all("Require")
            .flat_map(|v| v.split(','))
            .map(|t| t.trim().to_ascii_lowercase())
            .filter(|t| !t.is_empty())
            .filter(|t| !self.cfg.understood_options.iter().any(|u| u == t))
            .collect()
    }
}

/// `Authorization` / `Proxy-Authorization` for retrying a request that was answered with
/// 401 / 407; `None` without credentials or a usable Digest challenge.
pub(crate) fn auth_for(
    cfg: &EndpointConfig,
    resp: &Response,
    method: &str,
    uri: &str,
) -> Option<(String, String)> {
    let creds = cfg.credentials.as_ref()?;
    let proxy = resp.status.code() == 407;
    let hdr = if proxy {
        "Proxy-Authenticate"
    } else {
        "WWW-Authenticate"
    };
    let ch = resp
        .headers
        .get_all(hdr)
        .find_map(|v| DigestChallenge::from_header_value(v, proxy))?;
    let value = ch.build_authorization(&creds.username, &creds.password, method, uri);
    Some((ch.header_name().to_string(), value))
}

/// Errors of the call-handling API.
#[derive(Debug, thiserror::Error)]
pub enum CallError {
    /// The caller cancelled the INVITE (487 was already sent by the transaction layer).
    #[error("the call was cancelled")]
    Cancelled,
    /// The endpoint or the transaction has shut down.
    #[error("endpoint closed")]
    Closed,
    /// `reject` needs a final non-2xx status.
    #[error("not a rejection status: {0}")]
    BadStatus(StatusCode),
    /// The INVITE could not be turned into a dialog.
    #[error(transparent)]
    Dialog(#[from] DialogError),
}

/// Errors of [`Endpoint::invite`].
#[derive(Debug, thiserror::Error)]
pub enum InviteError {
    /// The call was rejected with this final status.
    #[error("INVITE rejected with {0}")]
    Rejected(StatusCode),
    /// No final answer in time (a CANCEL was sent if the peer had answered provisionally).
    #[error("INVITE timed out")]
    Timeout,
    /// Transport problem.
    #[error("transport error: {0}")]
    Transport(String),
    /// The 2xx could not be turned into a dialog.
    #[error(transparent)]
    Dialog(#[from] DialogError),
    /// The endpoint shut down.
    #[error("endpoint closed")]
    Closed,
}

/// Parameters of an outgoing call.
#[derive(Debug, Clone)]
pub struct InviteOptions {
    /// Where the INVITE is sent (first hop).
    pub target: SocketAddr,
    /// Transport towards `target`.
    pub transport: TransportKind,
    /// Request-URI.
    pub request_uri: SipUri,
    /// From address (the tag is added).
    pub from: SipUri,
    /// To address.
    pub to: SipUri,
    /// The offer.
    pub sdp: SessionDescription,
    /// Ask for a session timer of this many seconds (we refresh).
    pub session_expires: Option<u32>,
    /// Extra headers, appended as given.
    pub headers: Vec<(String, String)>,
    /// Give up (CANCEL) when there is no final response after this long (default 180 s).
    pub timeout: Duration,
}

impl InviteOptions {
    /// Options with the defaults filled in.
    pub fn new(
        target: SocketAddr,
        request_uri: SipUri,
        from: SipUri,
        to: SipUri,
        sdp: SessionDescription,
    ) -> Self {
        InviteOptions {
            target,
            transport: TransportKind::Udp,
            request_uri,
            from,
            to,
            sdp,
            session_expires: None,
            headers: Vec::new(),
            timeout: Duration::from_secs(180),
        }
    }
}

/// Result of [`DialogHandle::reinvite`].
// The SDP is boxed nowhere on purpose: `Accepted(SessionDescription)` is the documented API.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq)]
pub enum ReinviteOutcome {
    /// 2xx with this answer (the ACK has been sent).
    Accepted(SessionDescription),
    /// 2xx without a usable SDP body (ACKed anyway).
    NoAnswer,
    /// Final non-2xx status (after the 491 and authentication retries were used up).
    Rejected(StatusCode),
    /// No final response within the re-INVITE timeout; a CANCEL was sent.
    Timeout,
}

/// Why a dialog ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminateReason {
    /// The peer sent BYE.
    RemoteBye,
    /// We sent BYE.
    LocalBye,
    /// The session timer ran out (we sent BYE).
    SessionExpired,
    /// The endpoint shut down.
    Shutdown,
}

/// Things that happen to a dialog.
#[allow(clippy::large_enum_variant)]
#[derive(Debug)]
pub enum DialogEvent {
    /// UAS only: the ACK of the initial 2xx arrived.
    Established,
    /// The peer sent a re-INVITE; answer it through `responder` (an unanswered responder is
    /// rejected with 500 when dropped).
    ReInvite {
        /// The offered SDP (`None` for an offer-less re-INVITE).
        offer: Option<SessionDescription>,
        /// Use to answer.
        responder: ReinviteResponder,
    },
    /// The peer sent BYE (already answered with 200); [`DialogEvent::Terminated`] follows.
    Bye,
    /// No ACK for our 2xx within 64*T1. The dialog stays up; the application decides whether
    /// to send BYE.
    AckTimeout,
    /// The session timer expired (we are sending BYE); `Terminated` follows.
    SessionExpired,
    /// The dialog is over. Always the last event.
    Terminated(TerminateReason),
}

/// Answers a peer re-INVITE.
#[derive(Debug)]
pub struct ReinviteResponder {
    id: u64,
    to: mpsc::UnboundedSender<DialogInput>,
    done: bool,
}

impl ReinviteResponder {
    pub(crate) fn new(id: u64, to: mpsc::UnboundedSender<DialogInput>) -> Self {
        ReinviteResponder {
            id,
            to,
            done: false,
        }
    }

    async fn send(
        mut self,
        status: StatusCode,
        answer: Option<SessionDescription>,
    ) -> Result<(), DialogError> {
        self.done = true;
        let (tx, rx) = oneshot::channel();
        self.to
            .send(DialogInput::Respond {
                id: self.id,
                status,
                answer,
                reply: Some(tx),
            })
            .map_err(|_| DialogError::Terminated)?;
        rx.await.map_err(|_| DialogError::Terminated)?
    }

    /// Answers 200 with `answer`.
    pub async fn accept(self, answer: SessionDescription) -> Result<(), DialogError> {
        self.send(StatusCode::OK, Some(answer)).await
    }

    /// Answers with a final non-2xx status (e.g. 488).
    pub async fn reject(self, status: StatusCode) -> Result<(), DialogError> {
        self.send(status, None).await
    }
}

impl Drop for ReinviteResponder {
    fn drop(&mut self) {
        if !self.done {
            let _ = self.to.send(DialogInput::Respond {
                id: self.id,
                status: StatusCode::SERVER_INTERNAL_ERROR,
                answer: None,
                reply: None,
            });
        }
    }
}

/// Handle to a running dialog.
#[derive(Debug)]
pub struct DialogHandle {
    id: DialogId,
    input: mpsc::UnboundedSender<DialogInput>,
    events: mpsc::UnboundedReceiver<DialogEvent>,
    remote_sdp: Option<SessionDescription>,
}

impl DialogHandle {
    /// The dialog identifier.
    pub fn id(&self) -> &DialogId {
        &self.id
    }

    /// The peer's SDP from the initial exchange: the INVITE body on the UAS side, the 2xx
    /// body on the UAC side.
    pub fn remote_sdp(&self) -> Option<&SessionDescription> {
        self.remote_sdp.as_ref()
    }

    /// A cloneable control handle (re-INVITE / BYE) that can be used while another task waits
    /// in [`DialogHandle::next_event`].
    pub fn control(&self) -> DialogControl {
        DialogControl {
            id: self.id.clone(),
            input: self.input.clone(),
        }
    }

    /// See [`DialogControl::reinvite`].
    pub async fn reinvite(
        &self,
        offer: SessionDescription,
    ) -> Result<ReinviteOutcome, DialogError> {
        self.control().reinvite(offer).await
    }

    /// See [`DialogControl::bye`].
    pub async fn bye(&self) -> Result<StatusCode, DialogError> {
        self.control().bye().await
    }

    /// The next dialog event; `None` after [`DialogEvent::Terminated`].
    pub async fn next_event(&mut self) -> Option<DialogEvent> {
        self.events.recv().await
    }
}

/// Cloneable command side of a dialog.
#[derive(Debug, Clone)]
pub struct DialogControl {
    id: DialogId,
    input: mpsc::UnboundedSender<DialogInput>,
}

impl DialogControl {
    /// The dialog identifier.
    pub fn id(&self) -> &DialogId {
        &self.id
    }

    /// Sends a re-INVITE with `offer` and waits for its outcome. Re-INVITEs are serialised; on
    /// the UAS side they wait for the ACK of the initial 2xx. 491 is retried (0-2 s, or
    /// 2.1-4 s when we sent the original INVITE), 401 / 407 once with credentials, and a
    /// missing final answer ends in [`ReinviteOutcome::Timeout`] after a CANCEL.
    pub async fn reinvite(
        &self,
        offer: SessionDescription,
    ) -> Result<ReinviteOutcome, DialogError> {
        let (tx, rx) = oneshot::channel();
        self.input
            .send(DialogInput::Reinvite { offer, reply: tx })
            .map_err(|_| DialogError::Terminated)?;
        rx.await.map_err(|_| DialogError::Terminated)?
    }

    /// Sends BYE and returns its final status (200, or e.g. 407 when the credentials were
    /// rejected). The dialog is over afterwards in every case.
    pub async fn bye(&self) -> Result<StatusCode, DialogError> {
        let (tx, rx) = oneshot::channel();
        self.input
            .send(DialogInput::Bye { reply: tx })
            .map_err(|_| DialogError::Terminated)?;
        rx.await.map_err(|_| DialogError::Terminated)?
    }
}

fn spawn_dialog(
    ep: &Arc<EpInner>,
    mut init: TaskInit,
    input_tx: mpsc::UnboundedSender<DialogInput>,
    events_rx: mpsc::UnboundedReceiver<DialogEvent>,
    remote_sdp: Option<SessionDescription>,
) -> DialogHandle {
    let id = init.dialog.id.clone();
    init.input_tx = input_tx.clone();
    let stop = ep.stop.subscribe();
    ep.tasks.spawn(task::run(init, stop));
    DialogHandle {
        id,
        input: input_tx,
        events: events_rx,
        remote_sdp,
    }
}

/// A new INVITE waiting for the application's decision.
pub struct IncomingCall {
    ep: Arc<EpInner>,
    request: Request,
    source: SocketAddr,
    transport: TransportKind,
    tx: ServerTx,
    events: Mutex<mpsc::UnboundedReceiver<TuEvent>>,
    cancelled: AtomicBool,
    to_tag: String,
    neg: Negotiation,
}

impl IncomingCall {
    /// The INVITE (topmost Via stamped with `received` / `rport`).
    pub fn request(&self) -> &Request {
        &self.request
    }

    /// Where the INVITE came from.
    pub fn source(&self) -> SocketAddr {
        self.source
    }

    /// Transport the INVITE arrived on.
    pub fn transport(&self) -> TransportKind {
        self.transport
    }

    /// The SDP offer, if the INVITE has a body that parses.
    pub fn offer(&self) -> Option<SessionDescription> {
        if self.request.body.is_empty() {
            return None;
        }
        parse_sdp(&self.request.body).ok()
    }

    /// `true` once the caller's CANCEL was seen (the 487 is already sent).
    pub fn is_cancelled(&self) -> bool {
        let mut ev = lock(&self.events);
        while let Ok(e) = ev.try_recv() {
            if e == TuEvent::Cancelled {
                self.cancelled.store(true, Ordering::SeqCst);
            }
        }
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Sends `180 Ringing`.
    pub async fn ringing(&self) -> Result<(), CallError> {
        if self.is_cancelled() {
            return Err(CallError::Cancelled);
        }
        let mut r = Response::for_request(&self.request, StatusCode::RINGING, Some(&self.to_tag));
        r.headers
            .push("Contact", self.ep.contact_for(self.transport).to_string());
        self.tx.respond(r).await.map_err(|_| CallError::Closed)
    }

    /// Answers with a final non-2xx status.
    pub async fn reject(self, status: StatusCode) -> Result<(), CallError> {
        if !status.is_final() || status.is_success() {
            return Err(CallError::BadStatus(status));
        }
        if self.is_cancelled() {
            return Err(CallError::Cancelled);
        }
        let mut r = Response::for_request(&self.request, status, Some(&self.to_tag));
        self.ep.decorate(&mut r.headers);
        self.tx.respond(r).await.map_err(|_| CallError::Closed)
    }

    /// Answers 200 with `answer`, creates the dialog, and retransmits the 200 until the ACK
    /// arrives ([`DialogEvent::Established`]) or 64*T1 pass ([`DialogEvent::AckTimeout`]).
    pub async fn accept(self, answer: SessionDescription) -> Result<DialogHandle, CallError> {
        if self.is_cancelled() {
            return Err(CallError::Cancelled);
        }
        let ep = self.ep.clone();
        let contact = ep.contact_for(self.transport);
        let dialog = uas::dialog_from_invite(
            &self.request,
            &self.to_tag,
            contact.clone(),
            self.source,
            self.transport,
        )?;
        let mut resp = Response::for_request(&self.request, StatusCode::OK, Some(&self.to_tag));
        for rr in self.request.headers.get_all("Record-Route") {
            resp.headers.push("Record-Route", rr);
        }
        resp.headers.push("Contact", contact.to_string());
        ep.decorate(&mut resp.headers);
        let mut session = None;
        if let Negotiation::Active {
            interval,
            refresher,
        } = self.neg
        {
            resp.headers.push(
                "Session-Expires",
                format!("{interval};refresher={}", refresher.as_str()),
            );
            if refresher == Refresher::Uac {
                resp.headers.push("Require", "timer");
            }
            session = Some((interval, refresher == Refresher::Uas));
        }
        resp.headers.push("Content-Type", "application/sdp");
        resp.body = Bytes::from(answer.to_sdp_string());
        let cseq = self.request.headers.cseq().map(|c| c.seq).unwrap_or(0);

        let (input_tx, input_rx) = mpsc::unbounded_channel();
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        lock(&ep.dialogs).insert(dialog.id.clone(), input_tx.clone());
        if self.tx.respond(resp.clone()).await.is_err() {
            ep.unregister(&dialog.id);
            return Err(CallError::Closed);
        }
        let init = TaskInit {
            dialog,
            ep: ep.clone(),
            input_tx: input_tx.clone(),
            input_rx,
            events: ev_tx,
            initial_2xx: Some((resp, self.tx.clone(), cseq)),
            initial_ack: None,
            local_sdp: Some(answer),
            session,
        };
        let remote = self.offer();
        Ok(spawn_dialog(&ep, init, input_tx, ev_rx, remote))
    }
}

/// The SIP endpoint: transport, transactions, dialogs.
#[derive(Clone)]
pub struct Endpoint {
    inner: Arc<EpInner>,
}

impl Endpoint {
    /// Binds the configured sockets and starts the endpoint. Incoming INVITEs arrive on the
    /// returned channel.
    pub async fn bind(
        mut cfg: EndpointConfig,
    ) -> io::Result<(Endpoint, mpsc::UnboundedReceiver<IncomingCall>)> {
        cfg.transport.reply_to_malformed = cfg.answer_malformed;
        let (transport, incoming) = TransportLayer::bind(cfg.transport.clone()).await?;
        let handle = transport.handle();
        Ok(Self::start(cfg, handle, incoming, Some(transport)))
    }

    /// Starts an endpoint on a detached transport (no sockets; the caller injects and
    /// observes messages). Used by deterministic tests.
    pub fn detached(
        cfg: EndpointConfig,
        handle: TransportHandle,
        incoming: mpsc::Receiver<Incoming>,
    ) -> (Endpoint, mpsc::UnboundedReceiver<IncomingCall>) {
        Self::start(cfg, handle, incoming, None)
    }

    fn start(
        cfg: EndpointConfig,
        handle: TransportHandle,
        incoming: mpsc::Receiver<Incoming>,
        transport: Option<TransportLayer>,
    ) -> (Endpoint, mpsc::UnboundedReceiver<IncomingCall>) {
        let (layer, tu) = TransactionLayer::new(handle, incoming, cfg.timers);
        let (stop, stop_rx) = watch::channel(false);
        let inner = Arc::new(EpInner {
            cfg,
            layer,
            tasks: Tasks::default(),
            dialogs: Mutex::new(std::collections::HashMap::new()),
            stop,
            transport: Mutex::new(transport),
        });
        let (call_tx, call_rx) = mpsc::unbounded_channel();
        inner
            .tasks
            .spawn(run_dispatcher(inner.clone(), tu, call_tx, stop_rx));
        (Endpoint { inner }, call_rx)
    }

    /// Bound UDP address.
    pub fn udp_addr(&self) -> Option<SocketAddr> {
        lock(&self.inner.transport)
            .as_ref()
            .and_then(|t| t.udp_addr())
    }

    /// Bound TCP address.
    pub fn tcp_addr(&self) -> Option<SocketAddr> {
        lock(&self.inner.transport)
            .as_ref()
            .and_then(|t| t.tcp_addr())
    }

    /// The transaction layer (for registrations and other TU code sharing the endpoint).
    pub fn transactions(&self) -> &TransactionLayer {
        &self.inner.layer
    }

    /// Number of dialogs currently running.
    pub fn dialog_count(&self) -> usize {
        lock(&self.inner.dialogs).len()
    }

    /// Stops the dispatcher, all dialogs, all transactions and the sockets, and waits until
    /// every task is gone.
    pub async fn shutdown(&self) {
        let _ = self.inner.stop.send(true);
        self.inner.layer.shutdown().await;
        self.inner.tasks.join_all().await;
        let t = lock(&self.inner.transport).take();
        if let Some(t) = t {
            t.shutdown().await;
        }
    }

    /// Places a call. Returns once the 2xx has arrived; the ACK is sent automatically (and
    /// re-sent for every retransmitted 2xx).
    pub async fn invite(&self, opts: InviteOptions) -> Result<DialogHandle, InviteError> {
        let ep = &self.inner;
        let kind = opts.transport;
        let contact = ep.contact_for(kind);
        let call_id = format!(
            "{:016x}{:08x}@{}",
            rand::random::<u64>(),
            rand::random::<u32>(),
            ep.layer.transport().sent_by(kind).0
        );
        let mut from = NameAddr::new(Uri::Sip(opts.from.clone()));
        from.set_tag(random_tag());
        let to = NameAddr::new(Uri::Sip(opts.to.clone()));
        let mut seq = (rand::random::<u32>() >> 1).max(1);
        let mut auth: Option<(String, String)> = None;
        for attempt in 0..2 {
            let mut req = Request::new(Method::Invite, Uri::Sip(opts.request_uri.clone()));
            req.headers.push(
                "Via",
                format!("SIP/2.0/{} invalid;branch={}", kind.as_str(), new_branch()),
            );
            req.headers.push("Max-Forwards", "70");
            req.headers.push("From", from.to_string());
            req.headers.push("To", to.to_string());
            req.headers.push("Call-ID", call_id.clone());
            req.headers.push("CSeq", format!("{seq} INVITE"));
            req.headers.push("Contact", contact.to_string());
            ep.decorate(&mut req.headers);
            if let Some(se) = opts.session_expires {
                req.headers
                    .push("Session-Expires", format!("{se};refresher=uac"));
                req.headers
                    .push("Min-SE", ep.cfg.session_timer.min_se.to_string());
            }
            for (n, v) in &opts.headers {
                req.headers.push(n, v.clone());
            }
            if let Some((n, v)) = &auth {
                req.headers.push(n, v.clone());
            }
            req.headers.push("Content-Type", "application/sdp");
            req.body = Bytes::from(opts.sdp.to_sdp_string());

            let mut tx = ep
                .layer
                .send_request(req, opts.target, kind)
                .map_err(|e| InviteError::Transport(e.to_string()))?;
            let deadline = tokio::time::Instant::now() + opts.timeout;
            let final_resp = loop {
                let ev = match tokio::time::timeout_at(deadline, tx.events.recv()).await {
                    Ok(Some(ev)) => ev,
                    Ok(None) => return Err(InviteError::Closed),
                    Err(_) => {
                        let _ = ep
                            .layer
                            .send_request(build_cancel(&tx.request), opts.target, kind);
                        return Err(InviteError::Timeout);
                    }
                };
                match ev {
                    TuEvent::Response(r) if r.status.is_provisional() => {}
                    TuEvent::Response(r) => break r,
                    TuEvent::Timeout => return Err(InviteError::Timeout),
                    TuEvent::TransportError => {
                        return Err(InviteError::Transport("send failed".into()));
                    }
                    _ => {}
                }
            };
            if final_resp.status.is_success() {
                let dialog =
                    Dialog::from_2xx(&tx.request, &final_resp, contact, opts.target, kind)?;
                // a Session-Expires in the 2xx (re)defines the timer; refresher=uac means we refresh
                let session = final_resp
                    .headers
                    .get("Session-Expires")
                    .and_then(session_timer::parse_session_expires)
                    .map(|se| (se.delta, se.refresher.is_none_or(|r| r == Refresher::Uac)));
                let (input_tx, input_rx) = mpsc::unbounded_channel();
                let (ev_tx, ev_rx) = mpsc::unbounded_channel();
                lock(&ep.dialogs).insert(dialog.id.clone(), input_tx.clone());
                ep.tasks.spawn(task::forward(
                    tx.events,
                    input_tx.clone(),
                    Purpose::Invite(seq),
                ));
                let init = TaskInit {
                    dialog,
                    ep: ep.clone(),
                    input_tx: input_tx.clone(),
                    input_rx,
                    events: ev_tx,
                    initial_2xx: None,
                    initial_ack: Some(seq),
                    local_sdp: Some(opts.sdp.clone()),
                    session,
                };
                let remote = parse_sdp(&final_resp.body).ok();
                return Ok(spawn_dialog(ep, init, input_tx, ev_rx, remote));
            }
            let code = final_resp.status.code();
            if attempt == 0 && matches!(code, 401 | 407) {
                if let Some(a) =
                    auth_for(&ep.cfg, &final_resp, "INVITE", &tx.request.uri.to_string())
                {
                    auth = Some(a);
                    seq += 1;
                    continue;
                }
            }
            return Err(InviteError::Rejected(final_resp.status));
        }
        Err(InviteError::Closed)
    }
}

// ---------------------------------------------------------------------------- dispatcher

async fn run_dispatcher(
    ep: Arc<EpInner>,
    mut tu: mpsc::UnboundedReceiver<TuInput>,
    calls: mpsc::UnboundedSender<IncomingCall>,
    mut stop: watch::Receiver<bool>,
) {
    loop {
        let input = tokio::select! {
            i = tu.recv() => match i { Some(i) => i, None => return },
            () = wait_shutdown(&mut stop) => return,
        };
        match input {
            TuInput::Request(ir) => on_request(&ep, ir, &calls).await,
            TuInput::Ack(inc) => {
                if let SipMessage::Request(req) = inc.msg {
                    if let Some(tx) =
                        DialogId::from_peer_request(&req).and_then(|id| ep.lookup(&id))
                    {
                        let _ = tx.send(DialogInput::Ack(req));
                    }
                }
            }
            TuInput::StrayResponse(inc) => {
                if let SipMessage::Response(resp) = inc.msg {
                    let is_invite_2xx = resp.status.is_success()
                        && resp
                            .headers
                            .cseq()
                            .is_ok_and(|c| c.method == Method::Invite);
                    if is_invite_2xx {
                        if let Some(tx) =
                            DialogId::from_our_response(&resp).and_then(|id| ep.lookup(&id))
                        {
                            let _ = tx.send(DialogInput::StrayOk(resp));
                        }
                    }
                }
            }
            TuInput::Stateless(inc) => {
                // RFC 2543 style branch: no transaction, answer statelessly
                if let SipMessage::Request(req) = inc.msg {
                    if req.method != Method::Ack {
                        let r = Response::for_request(
                            &req,
                            StatusCode::BAD_REQUEST,
                            Some(&random_tag()),
                        );
                        let _ = ep
                            .layer
                            .transport()
                            .send_response(r, inc.source, inc.transport)
                            .await;
                    }
                }
            }
        }
    }
}

async fn on_request(
    ep: &Arc<EpInner>,
    ir: IncomingRequest,
    calls: &mpsc::UnboundedSender<IncomingCall>,
) {
    let method = ir.request.method.clone();
    if ir.request.headers.max_forwards() == Ok(Some(0)) {
        ep.quick_reply(&ir, StatusCode::TOO_MANY_HOPS, |_| {}).await;
        return;
    }
    // C10: unknown Require -> 420 + Unsupported (switchable)
    if !ep.cfg.accept_unknown_require {
        let unknown = ep.unsupported_requires(&ir.request);
        if !unknown.is_empty() {
            ep.quick_reply(&ir, StatusCode::BAD_EXTENSION, |r| {
                r.headers.push("Unsupported", unknown.join(", "));
            })
            .await;
            return;
        }
    }
    let has_to_tag = ir.request.headers.to().is_ok_and(|t| t.tag().is_some());
    if has_to_tag {
        let target = DialogId::from_peer_request(&ir.request).and_then(|id| ep.lookup(&id));
        match target {
            Some(tx) => {
                if let Err(mpsc::error::SendError(DialogInput::Request(ir))) =
                    tx.send(DialogInput::Request(ir))
                {
                    ep.quick_reply(&ir, StatusCode::CALL_DOES_NOT_EXIST, |_| {})
                        .await;
                }
            }
            None => {
                ep.quick_reply(&ir, StatusCode::CALL_DOES_NOT_EXIST, |_| {})
                    .await;
            }
        }
        return;
    }
    match method {
        Method::Invite => {
            let neg = session_timer::negotiate_uas(&ir.request, &ep.cfg.session_timer);
            if let Negotiation::TooSmall { min_se } = neg {
                ep.quick_reply(&ir, StatusCode::SESSION_INTERVAL_TOO_SMALL, |r| {
                    r.headers.push("Min-SE", min_se.to_string());
                })
                .await;
                return;
            }
            let IncomingRequest {
                request,
                source,
                transport,
                tx,
                events,
                ..
            } = ir;
            let call = IncomingCall {
                ep: ep.clone(),
                request,
                source,
                transport,
                tx: tx.clone(),
                events: Mutex::new(events),
                cancelled: AtomicBool::new(false),
                to_tag: random_tag(),
                neg,
            };
            if let Err(mpsc::error::SendError(call)) = calls.send(call) {
                // nobody is listening for calls any more
                let mut r = Response::for_request(
                    &call.request,
                    StatusCode::SERVICE_UNAVAILABLE,
                    Some(&call.to_tag),
                );
                r.headers.push("Retry-After", "10");
                let _ = tx.respond(r).await;
            }
        }
        Method::Options => {
            ep.quick_reply(&ir, StatusCode::OK, |r| ep.options_headers(&mut r.headers))
                .await;
        }
        Method::Bye | Method::Update | Method::Info | Method::Notify => {
            ep.quick_reply(&ir, StatusCode::CALL_DOES_NOT_EXIST, |_| {})
                .await;
        }
        _ => {
            let allow = ep.cfg.allow.clone();
            ep.quick_reply(&ir, StatusCode::METHOD_NOT_ALLOWED, |r| {
                r.headers.set("Allow", allow);
            })
            .await;
        }
    }
}
