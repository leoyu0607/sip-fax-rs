//! The dialog actor: one tokio task per dialog. It owns the [`Dialog`] state and serialises
//! everything that happens to it: peer requests, ACKs, commands from the
//! [`DialogHandle`](crate::endpoint::DialogHandle), client transaction events of our own
//! requests, and the timers (2xx retransmission, ACK timeout, re-INVITE timeout, 491 retry,
//! session refresh / expiry).

use std::collections::{HashMap, VecDeque};
use std::future::pending;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use bytes::Bytes;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::Instant;

use super::session_timer::{self, Refresher};
use super::uas::RetransmitSchedule;
use super::{Dialog, DialogError, Role};
use crate::endpoint::{
    DialogEvent, EpInner, ReinviteOutcome, ReinviteResponder, TerminateReason, auth_for,
};
use crate::message::{Method, Request, Response, StatusCode};
use crate::sdp::{SessionDescription, parse_sdp};
use crate::transaction::{IncomingRequest, ServerTx, TuEvent};
use crate::transport::{TransportKind, wait_shutdown};

type Reply<T> = oneshot::Sender<Result<T, DialogError>>;

/// Which of our client transactions an event belongs to.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Purpose {
    /// INVITE with this CSeq (initial or re-INVITE).
    Invite(u32),
    Bye,
    Cancel,
}

/// Everything a dialog task can be told.
pub(crate) enum DialogInput {
    /// A peer request inside the dialog (with its server transaction).
    Request(IncomingRequest),
    /// An ACK that matched this dialog (ACK of a 2xx).
    Ack(Request),
    /// A 2xx to one of our INVITEs that no transaction claimed any more.
    StrayOk(Response),
    /// An event of one of our client transactions.
    Client { purpose: Purpose, event: TuEvent },
    /// `DialogHandle::reinvite`.
    Reinvite {
        offer: SessionDescription,
        reply: Reply<ReinviteOutcome>,
    },
    /// `DialogHandle::bye`.
    Bye { reply: Reply<StatusCode> },
    /// `ReinviteResponder::accept / reject`.
    Respond {
        id: u64,
        status: StatusCode,
        answer: Option<SessionDescription>,
        reply: Option<Reply<()>>,
    },
}

/// Forwards the events of a client transaction into the dialog task.
pub(crate) async fn forward(
    mut events: mpsc::UnboundedReceiver<TuEvent>,
    to: mpsc::UnboundedSender<DialogInput>,
    purpose: Purpose,
) {
    while let Some(event) = events.recv().await {
        if to.send(DialogInput::Client { purpose, event }).is_err() {
            return;
        }
    }
}

/// What the dialog task is started with.
pub(crate) struct TaskInit {
    pub dialog: Dialog,
    pub ep: Arc<EpInner>,
    pub input_tx: mpsc::UnboundedSender<DialogInput>,
    pub input_rx: mpsc::UnboundedReceiver<DialogInput>,
    pub events: mpsc::UnboundedSender<DialogEvent>,
    /// UAS: the 2xx just sent for the initial INVITE (retransmitted until ACK).
    pub initial_2xx: Option<(Response, ServerTx, u32)>,
    /// UAC: CSeq of the initial INVITE whose 2xx must be ACKed right away.
    pub initial_ack: Option<u32>,
    /// Our last offer / answer (basis of session refreshes).
    pub local_sdp: Option<SessionDescription>,
    /// Session timer: (interval, we are the refresher).
    pub session: Option<(u32, bool)>,
}

#[derive(Clone, Copy)]
enum T {
    Retx,
    Ack,
    Reinvite,
    Retry,
    Refresh,
    Expire,
}

#[derive(Default)]
struct Timers {
    retx: Option<Instant>,
    ack: Option<Instant>,
    reinvite: Option<Instant>,
    retry: Option<Instant>,
    refresh: Option<Instant>,
    expire: Option<Instant>,
}

impl Timers {
    fn slot(&mut self, t: T) -> &mut Option<Instant> {
        match t {
            T::Retx => &mut self.retx,
            T::Ack => &mut self.ack,
            T::Reinvite => &mut self.reinvite,
            T::Retry => &mut self.retry,
            T::Refresh => &mut self.refresh,
            T::Expire => &mut self.expire,
        }
    }

    fn set(&mut self, t: T, at: Instant) {
        *self.slot(t) = Some(at);
    }

    fn clear(&mut self, t: T) {
        *self.slot(t) = None;
    }

    fn next(&self) -> Option<(T, Instant)> {
        let all = [
            (T::Retx, self.retx),
            (T::Ack, self.ack),
            (T::Reinvite, self.reinvite),
            (T::Retry, self.retry),
            (T::Refresh, self.refresh),
            (T::Expire, self.expire),
        ];
        let mut best: Option<(T, Instant)> = None;
        for (t, at) in all {
            if let Some(at) = at {
                if best.is_none_or(|(_, b)| at < b) {
                    best = Some((t, at));
                }
            }
        }
        best
    }
}

struct Pending2xx {
    resp: Response,
    tx: ServerTx,
    cseq: u32,
    initial: bool,
    sched: RetransmitSchedule,
    next_at: Instant,
}

struct PeerReinvite {
    id: u64,
    tx: ServerTx,
    request: Request,
}

struct Job {
    offer: SessionDescription,
    reply: Option<Reply<ReinviteOutcome>>,
    auth_tried: bool,
    retries: u32,
    is_refresh: bool,
}

struct Active {
    job: Job,
    cseq: u32,
    request: Request,
    got_provisional: bool,
    cancelled: bool,
}

struct PendingBye {
    reply: Option<Reply<StatusCode>>,
    request: Request,
    auth_tried: bool,
    reason: TerminateReason,
}

struct Session {
    interval: u32,
    we_refresh: bool,
}

struct DialogTask {
    dialog: Dialog,
    ep: Arc<EpInner>,
    input_tx: mpsc::UnboundedSender<DialogInput>,
    events: mpsc::UnboundedSender<DialogEvent>,
    timers: Timers,
    confirmed: bool,
    local_sdp: Option<SessionDescription>,
    pending_2xx: Option<Pending2xx>,
    peer_reinvite: Option<PeerReinvite>,
    next_responder_id: u64,
    active: Option<Active>,
    retry: Option<Job>,
    queue: VecDeque<(SessionDescription, Reply<ReinviteOutcome>)>,
    acks: HashMap<u32, (Request, SocketAddr, TransportKind)>,
    bye: Option<PendingBye>,
    session: Option<Session>,
    terminated: bool,
}

/// Runs the dialog until it terminates or the endpoint shuts down.
pub(crate) async fn run(init: TaskInit, mut stop: watch::Receiver<bool>) {
    let TaskInit {
        dialog,
        ep,
        input_tx,
        mut input_rx,
        events,
        initial_2xx,
        initial_ack,
        local_sdp,
        session,
    } = init;
    let id = dialog.id.clone();
    let mut t = DialogTask {
        confirmed: dialog.role == Role::Uac,
        dialog,
        ep: ep.clone(),
        input_tx,
        events,
        timers: Timers::default(),
        local_sdp,
        pending_2xx: None,
        peer_reinvite: None,
        next_responder_id: 1,
        active: None,
        retry: None,
        queue: VecDeque::new(),
        acks: HashMap::new(),
        bye: None,
        session: session.map(|(interval, we_refresh)| Session {
            interval,
            we_refresh,
        }),
        terminated: false,
    };
    if let Some((resp, tx, cseq)) = initial_2xx {
        t.start_2xx(resp, tx, cseq, true);
    }
    t.arm_session();
    if let Some(cseq) = initial_ack {
        t.send_ack(cseq).await;
    }
    while !t.terminated {
        let next = t.timers.next();
        tokio::select! {
            input = input_rx.recv() => match input {
                Some(i) => t.on_input(i).await,
                None => break,
            },
            () = async {
                match next {
                    Some((_, at)) => tokio::time::sleep_until(at).await,
                    None => pending::<()>().await,
                }
            } => {
                if let Some((which, _)) = next {
                    t.on_timer(which).await;
                }
            }
            () = wait_shutdown(&mut stop) => {
                t.terminate(TerminateReason::Shutdown);
            }
        }
        t.try_start_queued().await;
    }
    ep.unregister(&id);
}

impl DialogTask {
    fn emit(&self, e: DialogEvent) {
        let _ = self.events.send(e);
    }

    fn terminate(&mut self, reason: TerminateReason) {
        if self.terminated {
            return;
        }
        self.terminated = true;
        if let Some(a) = self.active.take() {
            if let Some(r) = a.job.reply {
                let _ = r.send(Err(DialogError::Terminated));
            }
        }
        if let Some(j) = self.retry.take() {
            if let Some(r) = j.reply {
                let _ = r.send(Err(DialogError::Terminated));
            }
        }
        for (_, r) in self.queue.drain(..) {
            let _ = r.send(Err(DialogError::Terminated));
        }
        if let Some(b) = self.bye.take() {
            if let Some(r) = b.reply {
                let _ = r.send(Err(DialogError::Terminated));
            }
        }
        self.pending_2xx = None;
        self.emit(DialogEvent::Terminated(reason));
    }

    // ------------------------------------------------------------------ helpers

    fn we_refresh(&self) -> bool {
        self.session.as_ref().is_some_and(|s| s.we_refresh)
    }

    /// `Session-Expires` value for a request we send / a response we give.
    fn session_value(&self, response: bool) -> Option<String> {
        let s = self.session.as_ref()?;
        let who = match (response, s.we_refresh) {
            // we are the UAC of a request we send: refresher=uac means "we refresh"
            (false, true) => Refresher::Uac,
            (false, false) => Refresher::Uas,
            (true, true) => Refresher::Uas,
            (true, false) => Refresher::Uac,
        };
        Some(format!("{};refresher={}", s.interval, who.as_str()))
    }

    fn arm_session(&mut self) {
        self.timers.clear(T::Refresh);
        self.timers.clear(T::Expire);
        let Some(s) = &self.session else { return };
        let now = Instant::now();
        if s.we_refresh {
            self.timers
                .set(T::Refresh, now + session_timer::refresh_after(s.interval));
        } else {
            self.timers
                .set(T::Expire, now + session_timer::expire_after(s.interval));
        }
    }

    fn adopt_session_from_response(&mut self, resp: &Response) {
        if let Some(se) = resp
            .headers
            .get("Session-Expires")
            .and_then(session_timer::parse_session_expires)
        {
            // the response to a request we sent: refresher=uac means we refresh
            let we_refresh = match se.refresher {
                Some(Refresher::Uac) => true,
                Some(Refresher::Uas) => false,
                None => self.we_refresh(),
            };
            self.session = Some(Session {
                interval: se.delta,
                we_refresh,
            });
        }
    }

    fn response(
        &self,
        req: &Request,
        status: StatusCode,
        sdp: Option<&SessionDescription>,
        contact: bool,
    ) -> Response {
        let mut r = Response::for_request(req, status, Some(&self.dialog.id.local_tag));
        if contact {
            r.headers
                .push("Contact", self.dialog.local_contact.to_string());
        }
        self.ep.decorate(&mut r.headers);
        if status.is_success() && req.method == Method::Invite {
            if let Some(v) = self.session_value(true) {
                r.headers.push("Session-Expires", v);
                if !self.we_refresh() {
                    r.headers.push("Require", "timer");
                }
            }
        }
        if let Some(s) = sdp {
            r.headers.push("Content-Type", "application/sdp");
            r.body = Bytes::from(s.to_sdp_string());
        }
        r
    }

    async fn reply(&self, tx: &ServerTx, resp: Response) {
        let _ = tx.respond(resp).await;
    }

    fn start_2xx(&mut self, resp: Response, tx: ServerTx, cseq: u32, initial: bool) {
        let now = Instant::now();
        let mut sched = RetransmitSchedule::new(&self.ep.layer.timer_config());
        let next_at = now + sched.next_delay();
        self.timers.set(T::Retx, next_at);
        self.timers
            .set(T::Ack, now + self.ep.layer.timer_config().t64());
        self.pending_2xx = Some(Pending2xx {
            resp,
            tx,
            cseq,
            initial,
            sched,
            next_at,
        });
    }

    async fn send_to(&self, req: Request, dest: SocketAddr, kind: TransportKind) -> bool {
        self.ep
            .layer
            .transport()
            .send_request(req, dest, kind)
            .await
            .is_ok()
    }

    /// Builds, caches and sends the ACK for the 2xx of our INVITE `cseq`.
    async fn send_ack(&mut self, cseq: u32) {
        let built = self.dialog.build_ack(cseq);
        let Some(dest) = built.hop.resolve().await else {
            tracing::warn!("cannot resolve ACK destination");
            return;
        };
        let mut req = built.request;
        self.ep
            .layer
            .transport()
            .stamp_request(&mut req, built.hop.transport);
        self.send_to(req.clone(), dest, built.hop.transport).await;
        if self.acks.len() >= 8 {
            if let Some(oldest) = self.acks.keys().min().copied() {
                self.acks.remove(&oldest);
            }
        }
        self.acks.insert(cseq, (req, dest, built.hop.transport));
    }

    async fn resend_ack(&self, cseq: u32) {
        if let Some((req, dest, kind)) = self.acks.get(&cseq) {
            self.send_to(req.clone(), *dest, *kind).await;
        }
    }

    fn spawn_forward(&self, events: mpsc::UnboundedReceiver<TuEvent>, purpose: Purpose) {
        self.ep
            .tasks
            .spawn(forward(events, self.input_tx.clone(), purpose));
    }

    // ------------------------------------------------------------------ inputs

    async fn on_input(&mut self, input: DialogInput) {
        match input {
            DialogInput::Request(ir) => self.on_request(ir).await,
            DialogInput::Ack(req) => self.on_ack(&req),
            DialogInput::StrayOk(resp) => {
                if let Ok(c) = resp.headers.cseq() {
                    self.resend_ack(c.seq).await;
                }
            }
            DialogInput::Client { purpose, event } => match purpose {
                Purpose::Invite(cseq) => self.on_invite_event(cseq, event).await,
                Purpose::Bye => self.on_bye_event(event).await,
                Purpose::Cancel => {
                    tracing::debug!(?event, "CANCEL transaction event");
                }
            },
            DialogInput::Reinvite { offer, reply } => {
                if self.bye.is_some() {
                    let _ = reply.send(Err(DialogError::Terminated));
                } else {
                    self.queue.push_back((offer, reply));
                }
            }
            DialogInput::Bye { reply } => {
                self.begin_bye(Some(reply), TerminateReason::LocalBye).await
            }
            DialogInput::Respond {
                id,
                status,
                answer,
                reply,
            } => {
                let r = self.on_respond(id, status, answer).await;
                if let Some(reply) = reply {
                    let _ = reply.send(r);
                }
            }
        }
    }

    fn on_ack(&mut self, req: &Request) {
        let Ok(c) = req.headers.cseq() else { return };
        let matches = self.pending_2xx.as_ref().is_some_and(|p| p.cseq == c.seq);
        if matches {
            let initial = self.pending_2xx.take().is_some_and(|p| p.initial);
            self.timers.clear(T::Retx);
            self.timers.clear(T::Ack);
            if initial {
                self.confirmed = true;
                self.emit(DialogEvent::Established);
            }
        } else if self.dialog.role == Role::Uas
            && !self.confirmed
            && c.seq == self.dialog.remote_seq.unwrap_or(0)
        {
            // late ACK after AckTimeout
            self.confirmed = true;
            self.emit(DialogEvent::Established);
        }
    }

    // ------------------------------------------------------------------ peer requests

    async fn on_request(&mut self, ir: IncomingRequest) {
        let IncomingRequest {
            request: req, tx, ..
        } = ir;
        let n = req.headers.cseq().map(|c| c.seq).unwrap_or(0);
        if let Some(r) = self.dialog.remote_seq {
            if n < r {
                let resp = self.response(&req, StatusCode::SERVER_INTERNAL_ERROR, None, false);
                self.reply(&tx, resp).await;
                return;
            }
        }
        self.dialog.remote_seq = Some(n);
        if self.bye.is_some() && req.method != Method::Bye {
            let resp = self.response(&req, StatusCode::CALL_DOES_NOT_EXIST, None, false);
            self.reply(&tx, resp).await;
            return;
        }
        match req.method.clone() {
            Method::Bye => {
                let resp = self.response(&req, StatusCode::OK, None, false);
                self.reply(&tx, resp).await;
                if let Some(p) = self.peer_reinvite.take() {
                    let r = self.response(&p.request, StatusCode::REQUEST_TERMINATED, None, false);
                    self.reply(&p.tx, r).await;
                }
                self.emit(DialogEvent::Bye);
                self.terminate(TerminateReason::RemoteBye);
            }
            Method::Invite => self.on_peer_invite(req, tx).await,
            Method::Options => {
                let mut resp = self.response(&req, StatusCode::OK, None, false);
                self.ep.options_headers(&mut resp.headers);
                self.reply(&tx, resp).await;
            }
            Method::Info => {
                let resp = self.response(&req, StatusCode::OK, None, false);
                self.reply(&tx, resp).await;
            }
            Method::Update => {
                if req.body.is_empty() {
                    let mut resp = self.response(&req, StatusCode::OK, None, false);
                    if let Some(v) = self.session_value(true) {
                        resp.headers.push("Session-Expires", v);
                    }
                    self.reply(&tx, resp).await;
                    if self.session.is_some() && !self.we_refresh() {
                        self.arm_session();
                    }
                } else {
                    // M1 limitation (spec section 5): UPDATE with SDP is not supported
                    let resp = self.response(&req, StatusCode::NOT_ACCEPTABLE_HERE, None, false);
                    self.reply(&tx, resp).await;
                }
            }
            _ => {
                let mut resp = self.response(&req, StatusCode::METHOD_NOT_ALLOWED, None, false);
                resp.headers.set("Allow", self.ep.cfg.allow.clone());
                self.reply(&tx, resp).await;
            }
        }
    }

    async fn on_peer_invite(&mut self, req: Request, tx: ServerTx) {
        if self.active.is_some() {
            // glare: our re-INVITE is in progress (RFC 3261 14.2)
            let resp = self.response(&req, StatusCode::REQUEST_PENDING, None, false);
            self.reply(&tx, resp).await;
            return;
        }
        if self.peer_reinvite.is_some() || self.pending_2xx.is_some() {
            let mut resp = self.response(&req, StatusCode::SERVER_INTERNAL_ERROR, None, false);
            resp.headers.push("Retry-After", "2");
            self.reply(&tx, resp).await;
            return;
        }
        let cfg = &self.ep.cfg.session_timer;
        if let session_timer::Negotiation::TooSmall { min_se } =
            session_timer::negotiate_uas(&req, cfg)
        {
            let mut resp = self.response(&req, StatusCode::SESSION_INTERVAL_TOO_SMALL, None, false);
            resp.headers.push("Min-SE", min_se.to_string());
            self.reply(&tx, resp).await;
            return;
        }
        let offer = if req.body.is_empty() {
            None
        } else {
            match parse_sdp(&req.body) {
                Ok(s) => Some(s),
                Err(_) => {
                    let resp = self.response(&req, StatusCode::NOT_ACCEPTABLE_HERE, None, false);
                    self.reply(&tx, resp).await;
                    return;
                }
            }
        };
        // a refresh may renegotiate the interval
        if let Some(se) = req
            .headers
            .get("Session-Expires")
            .and_then(session_timer::parse_session_expires)
        {
            if cfg.enabled {
                let we_refresh = match se.refresher {
                    Some(Refresher::Uac) => false,
                    Some(Refresher::Uas) => true,
                    None => self.we_refresh(),
                };
                self.session = Some(Session {
                    interval: se.delta,
                    we_refresh,
                });
            }
        }
        let id = self.next_responder_id;
        self.next_responder_id += 1;
        self.peer_reinvite = Some(PeerReinvite {
            id,
            tx,
            request: req,
        });
        self.emit(DialogEvent::ReInvite {
            offer,
            responder: ReinviteResponder::new(id, self.input_tx.clone()),
        });
    }

    async fn on_respond(
        &mut self,
        id: u64,
        status: StatusCode,
        answer: Option<SessionDescription>,
    ) -> Result<(), DialogError> {
        if !self.peer_reinvite.as_ref().is_some_and(|p| p.id == id) {
            return Err(DialogError::NoSuchRequest);
        }
        let Some(p) = self.peer_reinvite.take() else {
            return Err(DialogError::NoSuchRequest);
        };
        if status.is_success() {
            let resp = self.response(&p.request, status, answer.as_ref(), true);
            let cseq = p.request.headers.cseq().map(|c| c.seq).unwrap_or(0);
            self.reply(&p.tx, resp.clone()).await;
            if answer.is_some() {
                self.local_sdp = answer;
            }
            self.start_2xx(resp, p.tx, cseq, false);
            if self.session.is_some() {
                self.arm_session();
            }
        } else if status.is_final() {
            let resp = self.response(&p.request, status, None, false);
            self.reply(&p.tx, resp).await;
        } else {
            self.peer_reinvite = Some(p);
            return Err(DialogError::Invalid("provisional status".into()));
        }
        Ok(())
    }

    // ------------------------------------------------------------------ timers

    async fn on_timer(&mut self, which: T) {
        let now = Instant::now();
        match which {
            T::Retx => {
                if let Some(p) = &mut self.pending_2xx {
                    let _ = p.tx.respond(p.resp.clone()).await;
                    p.next_at += p.sched.next_delay();
                    let at = p.next_at;
                    self.timers.set(T::Retx, at);
                } else {
                    self.timers.clear(T::Retx);
                }
            }
            T::Ack => {
                self.timers.clear(T::Ack);
                self.timers.clear(T::Retx);
                if self.pending_2xx.take().is_some() {
                    self.emit(DialogEvent::AckTimeout);
                }
            }
            T::Reinvite => self.on_reinvite_timeout().await,
            T::Retry => {
                self.timers.clear(T::Retry);
                if self.peer_reinvite.is_some() {
                    self.timers.set(T::Retry, now + Duration::from_millis(250));
                } else if let Some(job) = self.retry.take() {
                    self.launch(job, None).await;
                }
            }
            T::Refresh => {
                self.timers.clear(T::Refresh);
                let busy = self.active.is_some()
                    || self.retry.is_some()
                    || self.peer_reinvite.is_some()
                    || self.pending_2xx.is_some();
                if busy {
                    self.timers.set(T::Refresh, now + Duration::from_secs(1));
                } else if let Some(mut sdp) = self.local_sdp.clone() {
                    sdp.origin.sess_version += 1;
                    self.local_sdp = Some(sdp.clone());
                    self.launch(
                        Job {
                            offer: sdp,
                            reply: None,
                            auth_tried: false,
                            retries: 0,
                            is_refresh: true,
                        },
                        None,
                    )
                    .await;
                }
            }
            T::Expire => {
                self.timers.clear(T::Expire);
                self.emit(DialogEvent::SessionExpired);
                self.begin_bye(None, TerminateReason::SessionExpired).await;
            }
        }
    }

    // ------------------------------------------------------------------ our re-INVITE

    async fn try_start_queued(&mut self) {
        while !self.terminated
            && self.confirmed
            && self.bye.is_none()
            && self.active.is_none()
            && self.retry.is_none()
            && self.peer_reinvite.is_none()
            && self.pending_2xx.is_none()
        {
            let Some((offer, reply)) = self.queue.pop_front() else {
                return;
            };
            self.local_sdp = Some(offer.clone());
            self.launch(
                Job {
                    offer,
                    reply: Some(reply),
                    auth_tried: false,
                    retries: 0,
                    is_refresh: false,
                },
                None,
            )
            .await;
        }
    }

    fn fail_job(job: Job, e: DialogError) {
        if let Some(r) = job.reply {
            let _ = r.send(Err(e));
        }
    }

    /// Sends one attempt of the re-INVITE `job` (new CSeq each time).
    async fn launch(&mut self, job: Job, auth: Option<(String, String)>) {
        let built = {
            let mut b = self.dialog.build_request(Method::Invite);
            let h = &mut b.request.headers;
            h.push("Contact", self.dialog.local_contact.to_string());
            self.ep.decorate(h);
            if let Some(v) = self.session_value(false) {
                h.push("Session-Expires", v);
                h.push("Min-SE", self.ep.cfg.session_timer.min_se.to_string());
            }
            h.push("Content-Type", "application/sdp");
            if let Some((n, v)) = &auth {
                h.push(n, v.clone());
            }
            b.request.body = Bytes::from(job.offer.to_sdp_string());
            b
        };
        let Some(dest) = built.hop.resolve().await else {
            Self::fail_job(
                job,
                DialogError::Transport("cannot resolve next hop".into()),
            );
            return;
        };
        match self
            .ep
            .layer
            .send_request(built.request, dest, built.hop.transport)
        {
            Ok(tx) => {
                let cseq = tx.request.headers.cseq().map(|c| c.seq).unwrap_or(0);
                self.spawn_forward(tx.events, Purpose::Invite(cseq));
                self.timers
                    .set(T::Reinvite, Instant::now() + self.ep.cfg.reinvite_timeout);
                self.active = Some(Active {
                    job,
                    cseq,
                    request: tx.request,
                    got_provisional: false,
                    cancelled: false,
                });
            }
            Err(e) => Self::fail_job(job, DialogError::Transport(e.to_string())),
        }
    }

    async fn on_invite_event(&mut self, cseq: u32, ev: TuEvent) {
        match ev {
            TuEvent::Retransmitted2xx(_) => self.resend_ack(cseq).await,
            TuEvent::Response(r) => {
                if !self.active.as_ref().is_some_and(|a| a.cseq == cseq) {
                    return;
                }
                if r.status.is_provisional() {
                    if let Some(a) = &mut self.active {
                        a.got_provisional = true;
                    }
                } else if r.status.is_success() {
                    self.on_reinvite_2xx(r).await;
                } else {
                    self.on_reinvite_failure(r).await;
                }
            }
            TuEvent::Timeout if self.active.as_ref().is_some_and(|a| a.cseq == cseq) => {
                self.timers.clear(T::Reinvite);
                if let Some(a) = self.active.take() {
                    if !a.cancelled {
                        Self::outcome(a.job, Ok(ReinviteOutcome::Timeout));
                    }
                }
            }
            TuEvent::TransportError if self.active.as_ref().is_some_and(|a| a.cseq == cseq) => {
                self.timers.clear(T::Reinvite);
                if let Some(a) = self.active.take() {
                    Self::fail_job(a.job, DialogError::Transport("send failed".into()));
                }
            }
            _ => {}
        }
    }

    fn outcome(job: Job, o: Result<ReinviteOutcome, DialogError>) {
        if let Some(r) = job.reply {
            let _ = r.send(o);
        }
    }

    async fn on_reinvite_2xx(&mut self, resp: Response) {
        self.timers.clear(T::Reinvite);
        let Some(a) = self.active.take() else { return };
        self.send_ack(a.cseq).await;
        if a.cancelled {
            return; // the caller already got Timeout; the ACK keeps the peer's INVITE closed
        }
        self.adopt_session_from_response(&resp);
        if self.session.is_some() {
            self.arm_session();
        }
        let outcome = if resp.body.is_empty() {
            ReinviteOutcome::NoAnswer
        } else {
            match parse_sdp(&resp.body) {
                Ok(s) => ReinviteOutcome::Accepted(s),
                Err(_) => ReinviteOutcome::NoAnswer,
            }
        };
        Self::outcome(a.job, Ok(outcome));
    }

    async fn on_reinvite_failure(&mut self, resp: Response) {
        self.timers.clear(T::Reinvite);
        let Some(mut a) = self.active.take() else {
            return;
        };
        if a.cancelled {
            return;
        }
        let code = resp.status.code();
        if code == 491 && a.job.retries < self.ep.cfg.max_491_retries {
            // RFC 3261 14.1: the Call-ID owner waits 2.1-4 s, the other side 0-2 s
            let ms = if self.dialog.role == Role::Uac {
                2100 + rand::random::<u64>() % 1901
            } else {
                rand::random::<u64>() % 2001
            };
            a.job.retries += 1;
            self.timers
                .set(T::Retry, Instant::now() + Duration::from_millis(ms));
            self.retry = Some(a.job);
            return;
        }
        if matches!(code, 401 | 407) && !a.job.auth_tried {
            if let Some(auth) = auth_for(&self.ep.cfg, &resp, "INVITE", &a.request.uri.to_string())
            {
                a.job.auth_tried = true;
                self.launch(a.job, Some(auth)).await;
                return;
            }
        }
        let refresh_dead = a.job.is_refresh && matches!(code, 408 | 481);
        Self::outcome(a.job, Ok(ReinviteOutcome::Rejected(resp.status)));
        if refresh_dead {
            self.emit(DialogEvent::SessionExpired);
            self.begin_bye(None, TerminateReason::SessionExpired).await;
        }
    }

    /// Application-level re-INVITE timeout (M1b review fix 2): CANCEL and report `Timeout`.
    async fn on_reinvite_timeout(&mut self) {
        self.timers.clear(T::Reinvite);
        let Some(a) = &mut self.active else { return };
        if a.cancelled {
            // the CANCEL got no final answer either: stop tracking the transaction
            self.active = None;
            return;
        }
        a.cancelled = true;
        let reply = a.job.reply.take();
        let cancel = a.got_provisional.then(|| build_cancel(&a.request));
        if let Some(r) = reply {
            let _ = r.send(Ok(ReinviteOutcome::Timeout));
        }
        if let Some(cancel) = cancel {
            let kind = self.dialog.next_hop().transport;
            if let Some(dest) = self.dialog.next_hop().resolve().await {
                if let Ok(tx) = self.ep.layer.send_request(cancel, dest, kind) {
                    self.spawn_forward(tx.events, Purpose::Cancel);
                }
            }
            // keep the INVITE transaction tracked until its 487 (or a racing 2xx) arrives
            self.timers.set(
                T::Reinvite,
                Instant::now() + self.ep.layer.timer_config().t64(),
            );
        } else {
            self.active = None;
        }
    }

    // ------------------------------------------------------------------ BYE

    async fn begin_bye(&mut self, reply: Option<Reply<StatusCode>>, reason: TerminateReason) {
        if self.bye.is_some() {
            if let Some(r) = reply {
                let _ = r.send(Err(DialogError::Terminated));
            }
            return;
        }
        self.send_bye(reply, reason, false, None).await;
    }

    async fn send_bye(
        &mut self,
        reply: Option<Reply<StatusCode>>,
        reason: TerminateReason,
        auth_tried: bool,
        auth: Option<(String, String)>,
    ) {
        let mut b = self.dialog.build_request(Method::Bye);
        self.ep.decorate(&mut b.request.headers);
        if let Some((n, v)) = &auth {
            b.request.headers.push(n, v.clone());
        }
        let fail = |reply: Option<Reply<StatusCode>>, e: DialogError| {
            if let Some(r) = reply {
                let _ = r.send(Err(e));
            }
        };
        let Some(dest) = b.hop.resolve().await else {
            fail(
                reply,
                DialogError::Transport("cannot resolve next hop".into()),
            );
            self.terminate(reason);
            return;
        };
        match self.ep.layer.send_request(b.request, dest, b.hop.transport) {
            Ok(tx) => {
                self.spawn_forward(tx.events, Purpose::Bye);
                self.bye = Some(PendingBye {
                    reply,
                    request: tx.request,
                    auth_tried,
                    reason,
                });
            }
            Err(e) => {
                fail(reply, DialogError::Transport(e.to_string()));
                self.terminate(reason);
            }
        }
    }

    async fn on_bye_event(&mut self, ev: TuEvent) {
        let Some(b) = self.bye.take() else { return };
        match ev {
            TuEvent::Response(r) if r.status.is_provisional() => self.bye = Some(b),
            TuEvent::Response(r) => {
                let code = r.status.code();
                if matches!(code, 401 | 407) && !b.auth_tried {
                    if let Some(auth) =
                        auth_for(&self.ep.cfg, &r, "BYE", &b.request.uri.to_string())
                    {
                        self.send_bye(b.reply, b.reason, true, Some(auth)).await;
                        return;
                    }
                }
                if let Some(rp) = b.reply {
                    let _ = rp.send(Ok(r.status));
                }
                self.terminate(b.reason);
            }
            TuEvent::Timeout => {
                if let Some(rp) = b.reply {
                    let _ = rp.send(Err(DialogError::Timeout));
                }
                self.terminate(b.reason);
            }
            TuEvent::TransportError => {
                if let Some(rp) = b.reply {
                    let _ = rp.send(Err(DialogError::Transport("send failed".into())));
                }
                self.terminate(b.reason);
            }
            _ => self.bye = Some(b),
        }
    }
}

/// CANCEL for an INVITE we sent (RFC 3261 9.1): same Request-URI, top Via (same branch),
/// Call-ID, From, To and CSeq number; Route headers copied.
pub(crate) fn build_cancel(invite: &Request) -> Request {
    let mut c = Request::new(Method::Cancel, invite.uri.clone());
    if let Some(v) = invite.headers.get("Via") {
        c.headers.push("Via", v);
    }
    c.headers.push("Max-Forwards", "70");
    for r in invite.headers.get_all("Route") {
        c.headers.push("Route", r);
    }
    for n in ["From", "To", "Call-ID"] {
        if let Some(v) = invite.headers.get(n) {
            c.headers.push(n, v);
        }
    }
    if let Ok(cs) = invite.headers.cseq() {
        c.headers.push("CSeq", format!("{} CANCEL", cs.seq));
    }
    c
}
