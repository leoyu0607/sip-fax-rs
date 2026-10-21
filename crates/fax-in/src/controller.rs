//! The inbound call controller: one task per call on top of the sip-core [`Endpoint`].
//!
//! Flow (docs/ARCHITECTURE.md "fax-in call flow"): map the source to a trunk (C02), answer
//! G.711 or T.38 according to the trunk's `t38_policy` (C06/C07/C08), then either accept the
//! peer's T.38 re-INVITE or send our own after `initiate_after_secs`. Once T.38 is in force
//! the call is handed to a [`FaxSession`](crate::session::FaxSession). When T.38 cannot be
//! established the call ends with a BYE and a failure reason (G.711 passthrough is M5).

use arc_swap::ArcSwap;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use sip_core::endpoint::{
    CallError, DialogEvent, DialogHandle, Endpoint, EndpointConfig, IncomingCall, ReinviteOutcome,
    TerminateReason,
};
use sip_core::message::{Request, SipUri, StatusCode};
use sip_core::registration::{self, RegistrationConfig, RegistrationHandle, RegistrationState};
use sip_core::sdp::OriginTracker;
use sip_core::transport::{Advertised, MessageTap, TransportKind};
use tokio::sync::{mpsc, watch};
use tokio::task::{JoinHandle, JoinSet};
use tokio::time::Instant;

use crate::capture::Capture;
use crate::config::FaxSessionKind;
use crate::config::{
    ConfigError, FaxInConfig, TransportName, UnknownSource, resolve_peer, split_host_port,
};
use crate::event::{
    ByeBy, CallEvent, CallOutcome, CallResult, FailureReason, T38Negotiated, T38Path,
};
use crate::event::{FaxReport, ReportSink};
use crate::media::{self, Accepted, MediaCtx, Unacceptable};
use crate::ports::PortPool;
use crate::session::{
    FaxSessionFactory, SessionContext, SessionOutcome, StubFactory, UdptlSettings,
};
use crate::t38session::{T38SessionEnv, T38SessionFactory};
use crate::trunk::{Trunk, TrunkTable};

/// Things that can go wrong when starting the controller.
#[derive(Debug, thiserror::Error)]
pub enum StartError {
    /// The configuration is not usable.
    #[error(transparent)]
    Config(#[from] ConfigError),
    /// A socket could not be bound.
    #[error("cannot bind the SIP sockets: {0}")]
    Bind(#[from] io::Error),
}

/// Optional parts of [`FaxIn::start`].
#[derive(Clone, Default)]
pub struct StartOptions {
    /// Creates the fax session of each call (default: the stub that holds the call).
    pub session_factory: Option<Arc<dyn FaxSessionFactory>>,
    /// Relative capture directories are resolved against this (default: the current dir).
    pub base_dir: Option<PathBuf>,
}

/// The part of the configuration that can change while the server runs: every call takes one
/// snapshot when its INVITE arrives and keeps it until it ends, so a change never reaches a call
/// that is in progress. Everything else (the sockets, the UDPTL port pool, the capture writer) is
/// fixed at start and needs a restart to change.
pub struct Snapshot {
    /// The configuration.
    pub cfg: FaxInConfig,
    /// Its trunks, resolved.
    pub table: TrunkTable,
}

struct Shared {
    live: ArcSwap<Snapshot>,
    /// Shared media settings; `t38_port` is 0 here and set per call.
    media: MediaCtx,
    ports: PortPool,
    events: mpsc::UnboundedSender<CallEvent>,
    capture: Option<Arc<Capture>>,
    factory: Arc<dyn FaxSessionFactory>,
    stop: watch::Receiver<bool>,
    /// The configuration the process started with (what the fixed parts were built from).
    started: FaxInConfig,
    /// `true` while the server drains: new INVITEs are answered 503.
    drain: watch::Receiver<bool>,
    /// Calls that are being handled (INVITE accepted by the controller, result not yet sent).
    active: watch::Sender<usize>,
}

/// Counts one call as active for as long as it lives (also when its task is cancelled).
struct ActiveCall(Arc<Shared>);

impl ActiveCall {
    fn new(sh: &Arc<Shared>) -> ActiveCall {
        sh.active.send_modify(|n| *n += 1);
        ActiveCall(sh.clone())
    }
}

impl Drop for ActiveCall {
    fn drop(&mut self) {
        self.0.active.send_modify(|n| *n = n.saturating_sub(1));
    }
}

/// The registration states of the trunks, readable from anywhere (see
/// [`FaxIn::registration_view`]).
#[derive(Clone)]
pub struct RegistrationView(Arc<std::sync::Mutex<Vec<RegItem>>>);

impl RegistrationView {
    /// `(trunk, state)` for each trunk that registers now (the list follows
    /// [`FaxIn::apply_config`]); the state is one line of text: `registering`, `registered`,
    /// `failed: why (attempt n)`, ...
    pub fn snapshot(&self) -> Vec<(String, String)> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|r| (r.name.clone(), registration_text(&r.handle.state())))
            .collect()
    }
}

fn registration_text(s: &RegistrationState) -> String {
    match s {
        RegistrationState::Unregistered => "not started".into(),
        RegistrationState::Registering => "registering".into(),
        RegistrationState::Registered { .. } => "registered".into(),
        RegistrationState::Failed {
            reason, attempts, ..
        } => format!("failed: {reason} (attempt {attempts})"),
        RegistrationState::Stopped => "stopped".into(),
    }
}

/// What [`FaxIn::drain`] reports while it waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DrainProgress {
    /// Calls still in progress.
    pub active: usize,
    /// Time left until the calls are ended.
    pub remaining: Duration,
}

impl Shared {
    fn emit(&self, ev: CallEvent) {
        let _ = self.events.send(ev);
    }
}

/// The running fax-in controller.
pub struct FaxIn {
    endpoint: Endpoint,
    stop: watch::Sender<bool>,
    drain: watch::Sender<bool>,
    active: watch::Receiver<usize>,
    acceptor: Option<JoinHandle<()>>,
    registrations: Arc<std::sync::Mutex<Vec<RegItem>>>,
    capture: Option<Arc<Capture>>,
    shared: Arc<Shared>,
}

/// One running registration and the inputs it was started from.
struct RegItem {
    name: String,
    /// What the registration depends on; a different key means a new registration.
    key: String,
    handle: RegistrationHandle,
    log: JoinHandle<()>,
}

/// What [`FaxIn::apply_config`] did.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyOutcome {
    /// Settings that differ from what the process started with and only take effect after a
    /// restart (`sip.bind`, `media.port_range`, ...). Empty when nothing is pending.
    pub restart_required: Vec<String>,
    /// Trunks whose registration was stopped (un-REGISTER) and, when still enabled, started again.
    pub registrations_restarted: Vec<String>,
    /// Trunks added, changed or removed.
    pub trunks_changed: Vec<String>,
}

impl FaxIn {
    /// The sip-core endpoint settings derived from a fax-in configuration. Public so that
    /// tests can build a detached endpoint with exactly the settings `start` would use.
    pub fn endpoint_config(
        cfg: &FaxInConfig,
        table: &TrunkTable,
        tap: Option<MessageTap>,
    ) -> EndpointConfig {
        let mut ec = EndpointConfig::new();
        for t in cfg.transports() {
            match t {
                TransportName::Udp => ec.transport.udp_bind = Some(cfg.sip.bind),
                TransportName::Tcp => ec.transport.tcp_bind = Some(cfg.sip.bind),
            }
        }
        ec.transport.advertised = cfg.sip.advertised.as_ref().map(|a| {
            let (host, port) = split_host_port(a);
            Advertised {
                host: host.to_string(),
                port,
            }
        });
        ec.transport.source_policy = table.source_policy();
        ec.transport.tap = tap;
        ec.user_agent = Some(cfg.sip.user_agent.clone());
        if let Some(a) = &cfg.sip.allow {
            ec.allow = a.clone();
        }
        if let Some(s) = &cfg.sip.supported {
            ec.supported = s.clone();
        }
        // The per-trunk `accept_unknown_require` switch is enforced by the controller (the
        // endpoint cannot know the trunk yet), so the endpoint itself lets everything through.
        ec.accept_unknown_require = true;
        ec.reinvite_timeout = Duration::from_secs(cfg.call.reinvite_timeout_secs.max(1));
        ec.max_491_retries = cfg.call.max_491_retries;
        ec
    }

    /// Binds the sockets, starts capture and registrations, and begins accepting calls.
    pub async fn start(
        cfg: FaxInConfig,
        opts: StartOptions,
    ) -> Result<(FaxIn, mpsc::UnboundedReceiver<CallEvent>), StartError> {
        let table = TrunkTable::build(&cfg)?;
        let (capture, tap) = if cfg.capture.enabled {
            let dir = match &opts.base_dir {
                Some(b) if cfg.capture.dir.is_relative() => b.join(&cfg.capture.dir),
                _ => cfg.capture.dir.clone(),
            };
            let (c, t) = Capture::start(&cfg.capture, dir);
            (Some(c), Some(t))
        } else {
            (None, None)
        };
        let ec = Self::endpoint_config(&cfg, &table, tap);
        let (endpoint, incoming) = Endpoint::bind(ec).await?;
        let (fx, events) = Self::attach(cfg, table, endpoint, incoming, capture, opts).await;
        Ok((fx, events))
    }

    /// Runs the controller on an endpoint that is already bound (or detached, for tests).
    pub async fn attach(
        cfg: FaxInConfig,
        table: TrunkTable,
        endpoint: Endpoint,
        incoming: mpsc::UnboundedReceiver<IncomingCall>,
        capture: Option<Arc<Capture>>,
        opts: StartOptions,
    ) -> (FaxIn, mpsc::UnboundedReceiver<CallEvent>) {
        let (ev_tx, ev_rx) = mpsc::unbounded_channel();
        let (stop_tx, stop_rx) = watch::channel(false);
        let (drain_tx, drain_rx) = watch::channel(false);
        let (active_tx, active_rx) = watch::channel(0usize);
        let max_duration = Duration::from_secs(cfg.call.max_duration_secs);
        let factory: Arc<dyn FaxSessionFactory> = match opts.session_factory {
            Some(f) => f,
            None => match cfg.call.fax_session {
                FaxSessionKind::Hold => Arc::new(StubFactory { max_duration }),
                FaxSessionKind::T38 => {
                    let data_dir = match &opts.base_dir {
                        Some(b) if cfg.storage.data_dir.is_relative() => {
                            b.join(&cfg.storage.data_dir)
                        }
                        _ => cfg.storage.data_dir.clone(),
                    };
                    Arc::new(T38SessionFactory::new(T38SessionEnv {
                        data_dir,
                        capture: capture.clone(),
                        bind_ip: cfg.sip.bind.ip(),
                        station_id: cfg.call.station_id.clone(),
                        max_duration,
                        pacing: crate::t38engine::Pacing::default(),
                    }))
                }
            },
        };
        let media = MediaCtx {
            local_address: cfg.media_address(),
            audio_port: cfg.media.audio_port,
            t38_port: 0,
        };
        // validated by FaxInConfig::parse; a hand-built config with a bad range gets one port
        let ports = PortPool::new(cfg.media.port_range().unwrap_or(crate::config::PortRange {
            low: 30000,
            high: 30000,
        }));
        let shared = Arc::new(Shared {
            started: cfg.clone(),
            live: ArcSwap::from_pointee(Snapshot { cfg, table }),
            media,
            ports,
            events: ev_tx,
            capture: capture.clone(),
            factory,
            stop: stop_rx.clone(),
            drain: drain_rx,
            active: active_tx,
        });
        let registrations = start_registrations(&shared, &endpoint);
        let acceptor = tokio::spawn(accept_loop(shared.clone(), incoming, stop_rx));
        (
            FaxIn {
                endpoint,
                stop: stop_tx,
                drain: drain_tx,
                active: active_rx,
                acceptor: Some(acceptor),
                registrations: Arc::new(std::sync::Mutex::new(registrations)),
                capture,
                shared,
            },
            ev_rx,
        )
    }

    /// The bound UDP address.
    pub fn udp_addr(&self) -> Option<std::net::SocketAddr> {
        self.endpoint.udp_addr()
    }

    /// The bound TCP address.
    pub fn tcp_addr(&self) -> Option<std::net::SocketAddr> {
        self.endpoint.tcp_addr()
    }

    /// The underlying endpoint.
    pub fn endpoint(&self) -> &Endpoint {
        &self.endpoint
    }

    /// The registration state of every trunk that registers.
    pub fn registrations(&self) -> Vec<(String, RegistrationState)> {
        self.registrations
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .map(|r| (r.name.clone(), r.handle.state()))
            .collect()
    }

    /// The configuration calls that arrive now will work with.
    pub fn snapshot(&self) -> Arc<Snapshot> {
        self.shared.live.load_full()
    }

    /// Settings of the running configuration that differ from what the process started with and
    /// need a restart to take effect.
    pub fn pending_restart(&self) -> Vec<String> {
        crate::config::restart_required(&self.shared.started, &self.shared.live.load().cfg)
    }

    /// Replaces the configuration for every call that arrives from now on (calls in progress keep
    /// the snapshot they started with): the trunk table and everything per-call, the transport's
    /// source filter, and the registrations (a trunk whose registration settings changed is
    /// de-registered and registered again with the new ones; removed trunks are de-registered).
    ///
    /// Settings that are fixed at start (listen address, UDPTL port range, capture, ...) are not
    /// applied; they are listed in [`ApplyOutcome::restart_required`].
    ///
    /// # Errors
    /// The new configuration does not validate; nothing was changed.
    pub async fn apply_config(&self, cfg: FaxInConfig) -> Result<ApplyOutcome, ConfigError> {
        let table = TrunkTable::build(&cfg)?;
        let old = self.shared.live.load_full();
        let mut trunks_changed = Vec::new();
        for t in table.trunks() {
            match old.table.trunks().iter().find(|o| o.name() == t.name()) {
                Some(o) if format!("{:?}", o.cfg) == format!("{:?}", t.cfg) => {}
                _ => trunks_changed.push(t.name().to_string()),
            }
        }
        for o in old.table.trunks() {
            if !table.trunks().iter().any(|t| t.name() == o.name()) {
                trunks_changed.push(o.name().to_string());
            }
        }
        let restart_required = crate::config::restart_required(&self.shared.started, &cfg);
        self.endpoint
            .transactions()
            .transport()
            .set_source_policy(table.source_policy());
        self.shared.live.store(Arc::new(Snapshot { cfg, table }));
        let registrations_restarted = self.reconcile_registrations().await;
        Ok(ApplyOutcome {
            restart_required,
            registrations_restarted,
            trunks_changed,
        })
    }

    async fn reconcile_registrations(&self) -> Vec<String> {
        let snap = self.shared.live.load_full();
        let mut wanted: Vec<(String, String, RegistrationConfig)> = Vec::new();
        for t in snap.table.trunks() {
            if !t.cfg.register.enabled {
                continue;
            }
            match registration_config(t, &self.endpoint) {
                Some(rc) => wanted.push((t.name().to_string(), registration_key(t), rc)),
                None => {
                    tracing::error!(trunk = %t.name(), "registration is configured but could not be set up")
                }
            }
        }
        // stop what is gone or changed (un-REGISTER first, then the new registration starts)
        let mut stopped = Vec::new();
        let mut keep = Vec::new();
        let current =
            std::mem::take(&mut *self.registrations.lock().unwrap_or_else(|e| e.into_inner()));
        for item in current {
            if wanted
                .iter()
                .any(|(n, k, _)| *n == item.name && *k == item.key)
            {
                keep.push(item);
            } else {
                stopped.push(item.name.clone());
                let _ = tokio::time::timeout(Duration::from_secs(5), item.handle.stop()).await;
                item.log.abort();
            }
        }
        let mut restarted = Vec::new();
        for (name, key, rc) in wanted {
            if keep.iter().any(|i| i.name == name) {
                continue;
            }
            tracing::info!(trunk = %name, "registration (re)started with the new configuration");
            restarted.push(name.clone());
            keep.push(spawn_registration(&self.endpoint, name, key, rc));
        }
        *self.registrations.lock().unwrap_or_else(|e| e.into_inner()) = keep;
        let mut all = stopped;
        for r in restarted {
            if !all.contains(&r) {
                all.push(r);
            }
        }
        all
    }

    /// A handle that reports the registration state of every trunk that registers, for as long
    /// as it is kept (also while the controller shuts down).
    pub fn registration_view(&self) -> RegistrationView {
        RegistrationView(self.registrations.clone())
    }

    /// Calls in progress right now.
    pub fn active_calls(&self) -> usize {
        *self.active.borrow()
    }

    /// Whether [`drain`](Self::drain) has been called: new INVITEs get 503.
    pub fn is_draining(&self) -> bool {
        *self.drain.borrow()
    }

    /// Stops taking new calls (every new INVITE is answered 503) and waits for the calls in
    /// progress to end, at most `timeout`; `force` (set to `true` at any time) ends the wait at
    /// once. `progress` is called every `every` while calls remain. Returns the number of calls
    /// still in progress, which [`shutdown`](Self::shutdown) then ends (the pages received so
    /// far are written and the result is `shutdown`). Uses the tokio clock, so tests pause it.
    pub async fn drain(
        &self,
        timeout: Duration,
        mut force: watch::Receiver<bool>,
        every: Duration,
        mut progress: impl FnMut(DrainProgress),
    ) -> usize {
        self.drain.send_replace(true);
        let deadline = Instant::now() + timeout;
        let mut active = self.active.clone();
        let every = every.max(Duration::from_millis(1));
        let mut tick = tokio::time::interval_at(Instant::now() + every, every);
        loop {
            let n = *active.borrow_and_update();
            if n == 0 || *force.borrow() {
                return n;
            }
            tokio::select! {
                _ = active.changed() => {}
                r = force.changed() => {
                    // a dropped sender can never force: keep waiting for the calls
                    if r.is_err() {
                        let _ = active.changed().await;
                    }
                }
                () = tokio::time::sleep_until(deadline) => return *active.borrow(),
                _ = tick.tick() => progress(DrainProgress {
                    active: n,
                    remaining: deadline.saturating_duration_since(Instant::now()),
                }),
            }
        }
    }

    /// Ends all calls (BYE, bounded), de-registers, stops the endpoint and flushes capture.
    pub async fn shutdown(mut self) {
        let _ = self.stop.send(true);
        if let Some(a) = self.acceptor.take() {
            let _ = a.await;
        }
        let regs =
            std::mem::take(&mut *self.registrations.lock().unwrap_or_else(|e| e.into_inner()));
        for item in regs {
            let _ = tokio::time::timeout(Duration::from_secs(5), item.handle.stop()).await;
            item.log.abort();
        }
        self.endpoint.shutdown().await;
        if let Some(c) = &self.capture {
            c.shutdown().await;
        }
    }
}

// ------------------------------------------------------------------------ registrations

fn start_registrations(sh: &Arc<Shared>, endpoint: &Endpoint) -> Vec<RegItem> {
    let mut items = Vec::new();
    let snap = sh.live.load_full();
    for t in snap.table.trunks() {
        if !t.cfg.register.enabled {
            continue;
        }
        let Some(cfg) = registration_config(t, endpoint) else {
            tracing::error!(trunk = %t.name(), "registration is configured but could not be set up");
            continue;
        };
        items.push(spawn_registration(
            endpoint,
            t.name().to_string(),
            registration_key(t),
            cfg,
        ));
    }
    items
}

/// Everything a registration is built from; when it differs, the registration is replaced.
fn registration_key(t: &Trunk) -> String {
    format!(
        "{:?}|{:?}|{:?}",
        t.cfg.register, t.cfg.peer, t.cfg.transport
    )
}

fn spawn_registration(
    endpoint: &Endpoint,
    name: String,
    key: String,
    cfg: RegistrationConfig,
) -> RegItem {
    let handle = registration::start(endpoint.transactions().clone(), cfg);
    let mut rx = handle.subscribe();
    let n2 = name.clone();
    let log = tokio::spawn(async move {
        while rx.changed().await.is_ok() {
            let st = rx.borrow().clone();
            match &st {
                RegistrationState::Failed { .. } => {
                    tracing::warn!(trunk = %n2, state = ?st, "registration state");
                }
                _ => tracing::info!(trunk = %n2, state = ?st, "registration state"),
            }
        }
    });
    RegItem {
        name,
        key,
        handle,
        log,
    }
}

fn registration_config(t: &Trunk, endpoint: &Endpoint) -> Option<RegistrationConfig> {
    let r = &t.cfg.register;
    let registrar_text = r.registrar.clone().or_else(|| t.cfg.peer.clone())?;
    let registrar = resolve_peer(&registrar_text).ok()?;
    let domain = r
        .domain
        .clone()
        .unwrap_or_else(|| split_host_port(&registrar_text).0.to_string());
    let host = sip_core::net::uri_host(&domain);
    let registrar_uri = SipUri::parse(&format!("sip:{host}")).ok()?;
    let user = r.username.clone()?;
    let aor = match &r.aor {
        Some(a) => SipUri::parse(a).ok()?,
        None => SipUri::parse(&format!("sip:{user}@{host}")).ok()?,
    };
    let kind = t.cfg.transport.kind();
    let (chost, cport) = endpoint.transactions().transport().sent_by(kind);
    let mut contact = SipUri::new(sip_core::net::uri_host(chost));
    contact.user = Some(user.clone());
    contact.port = Some(cport);
    if kind == TransportKind::Tcp {
        contact.params.push(sip_core::message::Param {
            name: "transport".into(),
            value: Some("tcp".into()),
        });
    }
    let mut cfg = RegistrationConfig::new(
        registrar,
        kind,
        registrar_uri,
        aor,
        contact,
        user,
        r.password.clone()?,
    );
    cfg.expires = r.expires;
    cfg.backoff_initial = Duration::from_secs_f64(r.backoff_initial_secs);
    cfg.backoff_max = Duration::from_secs_f64(r.backoff_max_secs);
    Some(cfg)
}

// ------------------------------------------------------------------------ accept loop

async fn accept_loop(
    sh: Arc<Shared>,
    mut incoming: mpsc::UnboundedReceiver<IncomingCall>,
    mut stop: watch::Receiver<bool>,
) {
    let mut calls = JoinSet::new();
    loop {
        tokio::select! {
            biased;
            () = stopped(&mut stop) => break,
            c = incoming.recv() => match c {
                Some(c) if *sh.drain.borrow() => {
                    calls.spawn(refuse_draining(c));
                }
                Some(c) => {
                    let guard = ActiveCall::new(&sh);
                    calls.spawn(run_call(sh.clone(), c, guard));
                }
                None => break,
            },
            Some(_) = calls.join_next(), if !calls.is_empty() => {}
        }
    }
    // the calls watch the same stop signal and hang up themselves
    while calls.join_next().await.is_some() {}
}

/// A new INVITE while the server drains for a stop: 503 (the peer may try another gateway).
async fn refuse_draining(call: IncomingCall) {
    tracing::info!(source = %call.source(), "call refused (503): the service is stopping");
    let _ = call.reject(StatusCode::SERVICE_UNAVAILABLE).await;
}

// ------------------------------------------------------------------------ one call

struct Rec {
    answered_at: Option<SystemTime>,
    t38_at: Option<SystemTime>,
    t38: Option<T38Negotiated>,
    /// Where the fax session leaves its report.
    fax: Option<ReportSink>,
    /// Who sent the BYE.
    bye_by: Option<ByeBy>,
}

async fn run_call(sh: Arc<Shared>, call: IncomingCall, _active: ActiveCall) {
    let started_at = SystemTime::now();
    let call_id = call
        .request()
        .headers
        .call_id()
        .unwrap_or_default()
        .to_string();
    let source = call.source();
    let uri_of = |r: Result<sip_core::message::NameAddr, _>| {
        r.map(|n| n.uri.to_string()).unwrap_or_default()
    };
    let from = uri_of(call.request().headers.from());
    let to = uri_of(call.request().headers.to());

    // C02: the trunk follows from the source address
    let snap = sh.live.load_full();
    let trunk = match snap.table.lookup(source.ip()) {
        Some(t) => t,
        None => match snap.table.unknown_source() {
            UnknownSource::Log => {
                tracing::warn!(%source, call_id, "call from a source outside every trunk; handling it with the generic profile (unknown_source = \"log\")");
                snap.table.fallback()
            }
            UnknownSource::Drop | UnknownSource::Reject => {
                tracing::warn!(%source, call_id, "call from a source outside every trunk: 403");
                let _ = call.reject(StatusCode::FORBIDDEN).await;
                let result = CallResult {
                    call_id,
                    trunk: crate::trunk::UNKNOWN_TRUNK.into(),
                    peer: source.to_string(),
                    from,
                    to,
                    numbers: Default::default(),
                    started_at,
                    answered_at: None,
                    t38_established_at: None,
                    ended_at: SystemTime::now(),
                    bye_by: None,
                    t38: None,
                    fax: None,
                    outcome: CallOutcome::Failed {
                        reason: FailureReason::UnknownSource,
                    },
                };
                sh.emit(CallEvent::Finished(Box::new(result)));
                return;
            }
        },
    };
    if let Some(c) = &sh.capture {
        c.begin(&call_id);
    }
    tracing::info!(call_id, trunk = %trunk.name(), %source, %from, %to, "inbound call");
    let numbers = crate::numbers::extract(call.request(), &trunk.cfg.numbers);
    sh.emit(CallEvent::Started {
        call_id: call_id.clone(),
        trunk: trunk.name().to_string(),
        peer: source.to_string(),
        numbers: numbers.clone(),
        at: started_at,
    });
    let mut rec = Rec {
        answered_at: None,
        t38_at: None,
        t38: None,
        fax: None,
        bye_by: None,
    };
    let mut outcome = drive(&sh, &snap, call, &trunk, &call_id, &mut rec).await;
    let fax: Option<FaxReport> = rec.fax.as_ref().and_then(|s| {
        s.lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    });
    // A call that ended "normally" without delivering the fax is a failure.
    if let (CallOutcome::Completed, Some(f)) = (&outcome, &fax)
        && (f.code != 0 || f.store_error.is_some())
    {
        outcome = failed(FailureReason::Session {
            detail: match &f.store_error {
                Some(e) => format!("fax received but not stored: {e}"),
                None => format!(
                    "T.30 {} ({}) after {} page(s)",
                    f.code_name, f.code, f.pages
                ),
            },
        });
    }
    let result = CallResult {
        call_id: call_id.clone(),
        trunk: trunk.name().to_string(),
        peer: source.to_string(),
        from,
        to,
        numbers,
        started_at,
        answered_at: rec.answered_at,
        t38_established_at: rec.t38_at,
        ended_at: SystemTime::now(),
        bye_by: rec.bye_by,
        t38: rec.t38,
        fax,
        outcome,
    };
    match &result.outcome {
        CallOutcome::Completed => tracing::info!(call_id, trunk = %result.trunk, "call completed"),
        CallOutcome::Failed { reason } => {
            tracing::warn!(call_id, trunk = %result.trunk, ?reason, "call failed");
        }
    }
    if let Some(c) = &sh.capture {
        let json = serde_json::to_string_pretty(&result).unwrap_or_default();
        c.end(&call_id, result.outcome.is_failure(), json);
    }
    sh.emit(CallEvent::Finished(Box::new(result)));
}

fn unsupported_requires(req: &Request) -> Vec<String> {
    req.headers
        .get_all("Require")
        .flat_map(|v| v.split(','))
        .map(|t| t.trim().to_ascii_lowercase())
        .filter(|t| !t.is_empty() && t != "timer")
        .collect()
}

fn failed(reason: FailureReason) -> CallOutcome {
    CallOutcome::Failed { reason }
}

async fn drive(
    sh: &Arc<Shared>,
    snap: &Arc<Snapshot>,
    call: IncomingCall,
    trunk: &Arc<Trunk>,
    call_id: &str,
    rec: &mut Rec,
) -> CallOutcome {
    let source = call.source();
    if !trunk.cfg.compat.accept_unknown_require {
        let tags = unsupported_requires(call.request());
        if !tags.is_empty() {
            tracing::warn!(call_id, ?tags, "unknown Require: 420");
            let _ = call
                .reject_with(
                    StatusCode::BAD_EXTENSION,
                    &[("Unsupported", &tags.join(", "))],
                )
                .await;
            return failed(FailureReason::UnsupportedRequire { tags });
        }
    }
    let Some(offer) = call.offer() else {
        tracing::warn!(call_id, "INVITE without a usable SDP offer: 488");
        let _ = call.reject(StatusCode::NOT_ACCEPTABLE_HERE).await;
        return failed(FailureReason::NoSdpOffer);
    };
    // One UDPTL port per call, for the whole call. Without a free port the call is refused
    // (nothing has been promised to the peer yet).
    let Some(lease) = sh.ports.acquire() else {
        tracing::error!(
            call_id,
            capacity = sh.ports.capacity(),
            "no free UDPTL port in [media] port_range: 503"
        );
        let _ = call.reject(StatusCode::SERVICE_UNAVAILABLE).await;
        return failed(FailureReason::NoMediaPort);
    };
    let media_ctx = sh.media.with_t38_port(lease.port());
    tracing::debug!(
        call_id,
        port = lease.port(),
        free = sh.ports.available(),
        "UDPTL port leased"
    );
    let mut origin = OriginTracker::random(&media_ctx.local_address);
    let (answer, accepted) = match media::answer(
        &offer,
        trunk,
        &media_ctx,
        &mut origin,
        source.ip(),
        T38Path::InitialInvite,
    ) {
        Ok(x) => x,
        Err(Unacceptable(detail)) => {
            tracing::warn!(call_id, %detail, "offer not acceptable: 488");
            let _ = call.reject(StatusCode::NOT_ACCEPTABLE_HERE).await;
            return failed(FailureReason::NoAcceptableMedia { detail });
        }
    };
    if snap.cfg.call.send_ringing && matches!(call.ringing().await, Err(CallError::Cancelled)) {
        return failed(FailureReason::Cancelled);
    }
    if snap.cfg.call.answer_delay_ms > 0 {
        tokio::time::sleep(Duration::from_millis(snap.cfg.call.answer_delay_ms)).await;
    }
    let dlg = match call.accept(answer).await {
        Ok(d) => d,
        Err(CallError::Cancelled) => return failed(FailureReason::Cancelled),
        Err(e) => {
            return failed(FailureReason::Internal {
                detail: e.to_string(),
            });
        }
    };
    let now = SystemTime::now();
    rec.answered_at = Some(now);
    sh.emit(CallEvent::Answered {
        call_id: call_id.to_string(),
        t38: matches!(accepted, Accepted::T38(_)),
        at: now,
    });
    Live {
        sh,
        snap: snap.clone(),
        trunk,
        call_id,
        source: source.ip(),
        media: media_ctx,
        rec,
        origin,
        control: dlg.control(),
        aux: JoinSet::new(),
        internal_tx: mpsc::unbounded_channel().0,
        session_cancel: None,
        session_finished: false,
        t38: false,
        initiate_at: None,
        audio_deadline: None,
        reinvite_pending: false,
    }
    .run(dlg, accepted)
    .await
}

/// How long the call result waits for the answer to our BYE.
const BYE_WAIT: Duration = Duration::from_secs(5);
/// How long a cancelled fax session may take to write its files.
const SESSION_FINISH_WAIT: Duration = Duration::from_secs(3);
/// ... when the whole server is shutting down.
const BYE_WAIT_SHUTDOWN: Duration = Duration::from_secs(2);

type ReinviteFuture = std::pin::Pin<
    Box<
        dyn std::future::Future<Output = Result<ReinviteOutcome, sip_core::dialog::DialogError>>
            + Send,
    >,
>;

enum Internal {
    Session(SessionOutcome),
}

/// State of one answered call.
struct Live<'a> {
    sh: &'a Arc<Shared>,
    /// The configuration of this call (taken at its INVITE).
    snap: Arc<Snapshot>,
    trunk: &'a Arc<Trunk>,
    call_id: &'a str,
    source: std::net::IpAddr,
    /// Media settings of this call (its UDPTL port).
    media: MediaCtx,
    rec: &'a mut Rec,
    origin: OriginTracker,
    control: sip_core::endpoint::DialogControl,
    /// Aborted when the call ends.
    aux: JoinSet<()>,
    internal_tx: mpsc::UnboundedSender<Internal>,
    session_cancel: Option<watch::Sender<bool>>,
    /// The session's result has been received on the internal channel.
    session_finished: bool,
    t38: bool,
    initiate_at: Option<Instant>,
    audio_deadline: Option<Instant>,
    reinvite_pending: bool,
}

/// Resolves once the stop flag is set. (The guard of `wait_for` must not live across an
/// await in the callers, hence the helper.)
async fn stopped(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|s| *s).await;
}

async fn at(t: Option<Instant>) {
    match t {
        Some(t) => tokio::time::sleep_until(t).await,
        None => std::future::pending().await,
    }
}

impl Live<'_> {
    fn max_duration(&self) -> Duration {
        Duration::from_secs(self.snap.cfg.call.max_duration_secs)
    }

    fn audio_only_outcome(&self) -> CallOutcome {
        failed(FailureReason::G711PassthroughUnsupported {
            cause: "t38_policy = audio-only; the call stayed on G.711".into(),
        })
    }

    /// T.38 is in force: record it and start the fax session.
    fn establish(&mut self, p: T38Negotiated) {
        if self.t38 {
            return;
        }
        self.t38 = true;
        self.initiate_at = None;
        self.audio_deadline = None;
        let now = SystemTime::now();
        self.rec.t38 = Some(p.clone());
        self.rec.t38_at = Some(now);
        tracing::info!(call_id = self.call_id, path = ?p.path, version = p.version,
            max_bit_rate = p.max_bit_rate, rate_management = %p.rate_management,
            udp_ec = %p.udp_ec, peer_media = ?p.peer_media, "T.38 established");
        self.sh.emit(CallEvent::T38Established {
            call_id: self.call_id.to_string(),
            params: p.clone(),
            at: now,
        });
        let (cancel_tx, cancel_rx) = watch::channel(false);
        let t38cfg = &self.trunk.cfg.t38;
        let report: ReportSink = Arc::default();
        self.rec.fax = Some(report.clone());
        // every page T.30 confirms becomes a CallEvent::PageReceived
        let (progress_tx, mut progress_rx) = mpsc::unbounded_channel::<u32>();
        {
            let sh = self.sh.clone();
            let call_id = self.call_id.to_string();
            self.aux.spawn(async move {
                while let Some(pages) = progress_rx.recv().await {
                    sh.emit(CallEvent::PageReceived {
                        call_id: call_id.clone(),
                        pages,
                        at: SystemTime::now(),
                    });
                }
            });
        }
        let ctx = SessionContext {
            call_id: self.call_id.to_string(),
            trunk: self.trunk.name().to_string(),
            t38: p,
            udptl: UdptlSettings {
                redundancy_depth: t38cfg.redundancy_depth,
                fec_span: t38cfg.fec_span,
                fec_entries: t38cfg.fec_entries,
            },
            fax: self.trunk.cfg.fax.clone(),
            report,
            progress: Some(progress_tx),
            cancel: cancel_rx,
            station_id: Some(self.snap.cfg.call.station_id.clone()),
            max_duration: Some(self.max_duration()),
        };
        let session = self.sh.factory.create(&ctx);
        let tx = self.internal_tx.clone();
        self.aux.spawn(async move {
            let out = session.run(ctx).await;
            let _ = tx.send(Internal::Session(out));
        });
        self.session_cancel = Some(cancel_tx);
    }

    fn cancel_session(&self) {
        if let Some(c) = &self.session_cancel {
            let _ = c.send(true);
        }
    }

    fn fallback(&self, cause: String) -> (CallOutcome, bool) {
        tracing::warn!(call_id = self.call_id, %cause,
            "G.711 passthrough not yet supported; ending the call");
        (
            failed(FailureReason::G711PassthroughUnsupported { cause }),
            true,
        )
    }

    async fn on_peer_reinvite(
        &mut self,
        offer: Option<sip_core::sdp::SessionDescription>,
        responder: sip_core::endpoint::ReinviteResponder,
    ) {
        let reject = StatusCode::NOT_ACCEPTABLE_HERE;
        let Some(offer) = offer else {
            tracing::warn!(
                call_id = self.call_id,
                "re-INVITE without an SDP offer: 488"
            );
            let _ = responder.reject(reject).await;
            return;
        };
        let offers_t38 = offer.media.iter().any(|m| m.is_t38());
        if self.t38 && !offers_t38 {
            tracing::warn!(
                call_id = self.call_id,
                "re-INVITE back to audio while T.38 is in force: 488"
            );
            let _ = responder.reject(reject).await;
            return;
        }
        match media::answer(
            &offer,
            self.trunk,
            &self.media,
            &mut self.origin,
            self.source,
            T38Path::PeerReinvite,
        ) {
            Ok((sdp, accepted)) => {
                if responder.accept(sdp).await.is_err() {
                    return;
                }
                if let Accepted::T38(p) = accepted {
                    self.establish(*p);
                }
            }
            Err(Unacceptable(d)) => {
                tracing::warn!(call_id = self.call_id, detail = %d, "re-INVITE not acceptable: 488");
                let _ = responder.reject(reject).await;
            }
        }
    }

    /// Starts our T.38 re-INVITE. The returned future is polled by the call loop itself (not
    /// by a helper task) so that its result is seen before a BYE the peer sends right behind
    /// its 200.
    fn start_own_reinvite(&mut self) -> ReinviteFuture {
        let offer = media::t38_offer(self.trunk, &self.media, &mut self.origin);
        self.reinvite_pending = true;
        let ctl = self.control.clone();
        tracing::info!(call_id = self.call_id, "sending the T.38 re-INVITE");
        Box::pin(async move { ctl.reinvite(offer).await })
    }

    /// `Some((outcome, send_bye))` when the call is over.
    fn on_reinvite_result(
        &mut self,
        r: Result<ReinviteOutcome, sip_core::dialog::DialogError>,
    ) -> Option<(CallOutcome, bool)> {
        self.reinvite_pending = false;
        match r {
            Ok(ReinviteOutcome::Accepted(sdp)) => {
                match media::read_t38_answer(&sdp, self.trunk, &self.media, self.source) {
                    Some(p) => {
                        self.establish(p);
                        None
                    }
                    None => Some(self.fallback(
                        "peer answered the T.38 re-INVITE without a usable m=image line".into(),
                    )),
                }
            }
            Ok(ReinviteOutcome::Rejected(st)) if matches!(st.code(), 488 | 415 | 606) => {
                Some(self.fallback(format!("peer answered {} to the T.38 re-INVITE", st.code())))
            }
            Ok(ReinviteOutcome::Rejected(st)) => Some((
                failed(FailureReason::T38ReinviteFailed {
                    status: Some(st.code()),
                    detail: format!("T.38 re-INVITE answered {}", st.code()),
                }),
                true,
            )),
            Ok(ReinviteOutcome::Timeout) => Some((
                failed(FailureReason::T38ReinviteFailed {
                    status: None,
                    detail: "no final answer to the T.38 re-INVITE".into(),
                }),
                true,
            )),
            Ok(ReinviteOutcome::NoAnswer) => Some(
                self.fallback("peer answered the T.38 re-INVITE with a 2xx without SDP".into()),
            ),
            // the dialog is gone; its Terminated event decides
            Err(_) => None,
        }
    }

    fn after_peer_bye(&self, session: Option<SessionOutcome>) -> CallOutcome {
        if self.t38 {
            match session {
                Some(SessionOutcome::Failed(d)) => failed(FailureReason::Session { detail: d }),
                Some(SessionOutcome::MaxDuration) => failed(FailureReason::MaxDuration),
                _ => CallOutcome::Completed,
            }
        } else if !self.trunk.cfg.t38_policy.accepts_t38() {
            self.audio_only_outcome()
        } else {
            failed(FailureReason::PeerHangupBeforeT38)
        }
    }

    async fn run(mut self, mut dlg: DialogHandle, accepted: Accepted) -> CallOutcome {
        let (itx, mut irx) = mpsc::unbounded_channel();
        self.internal_tx = itx;
        let mut stop = self.sh.stop.clone();
        match accepted {
            Accepted::T38(p) => self.establish(*p),
            Accepted::Audio => {
                self.audio_deadline = Some(Instant::now() + self.max_duration());
                if !self.trunk.cfg.t38_policy.accepts_t38() {
                    tracing::warn!(
                        call_id = self.call_id,
                        "t38_policy = audio-only: the call stays on G.711 and G.711 passthrough is not implemented yet"
                    );
                }
            }
        }
        let mut reinvite: Option<ReinviteFuture> = None;
        let mut peer_bye = false;
        let mut session_result: Option<SessionOutcome> = None;
        let (outcome, send_bye, shutting_down) = loop {
            tokio::select! {
                biased;
                () = stopped(&mut stop) => {
                    break (failed(FailureReason::Shutdown), true, true);
                }
                r = async {
                    match reinvite.as_mut() {
                        Some(f) => f.await,
                        None => std::future::pending().await,
                    }
                } => {
                    reinvite = None;
                    if let Some((o, bye)) = self.on_reinvite_result(r) {
                        break (o, bye, false);
                    }
                }
                ev = dlg.next_event() => {
                    let Some(ev) = ev else {
                        break (self.after_dialog_end(peer_bye, &mut irx, &mut session_result).await, false, false);
                    };
                    match ev {
                        DialogEvent::Established => {
                            if self.trunk.cfg.t38_policy.initiates() && !self.t38 && !self.reinvite_pending {
                                self.initiate_at = Some(Instant::now() + self.trunk.cfg.initiate_after());
                            }
                        }
                        DialogEvent::ReInvite { offer, responder } => {
                            self.on_peer_reinvite(offer, responder).await;
                        }
                        DialogEvent::Bye => {
                            peer_bye = true;
                            self.rec.bye_by = Some(ByeBy::Remote);
                            self.cancel_session();
                        }
                        DialogEvent::AckTimeout => {
                            tracing::warn!(call_id = self.call_id, "no ACK for our 200");
                            break (failed(FailureReason::AckTimeout), true, false);
                        }
                        DialogEvent::SessionExpired => {}
                        DialogEvent::Terminated(reason) => {
                            let o = match reason {
                                TerminateReason::RemoteBye => {
                                    self.rec.bye_by = Some(ByeBy::Remote);
                                    self.after_dialog_end(true, &mut irx, &mut session_result).await
                                }
                                TerminateReason::SessionExpired => failed(FailureReason::SessionExpired),
                                TerminateReason::LocalBye => failed(FailureReason::Internal {
                                    detail: "dialog ended by our own BYE".into(),
                                }),
                                TerminateReason::Shutdown => failed(FailureReason::Shutdown),
                            };
                            break (o, false, false);
                        }
                    }
                }
                Some(i) = irx.recv() => match i {
                    Internal::Session(out) => {
                        self.session_finished = true;
                        match out {
                        SessionOutcome::Completed => break (CallOutcome::Completed, true, false),
                        SessionOutcome::MaxDuration => {
                            tracing::warn!(call_id = self.call_id, "maximum call duration reached");
                            break (failed(FailureReason::MaxDuration), true, false);
                        }
                        SessionOutcome::Failed(d) => {
                            break (failed(FailureReason::Session { detail: d }), true, false);
                        }
                        SessionOutcome::Aborted => session_result = Some(SessionOutcome::Aborted),
                        }
                    }
                },
                () = at(self.initiate_at) => {
                    self.initiate_at = None;
                    if !self.t38 && !self.reinvite_pending {
                        reinvite = Some(self.start_own_reinvite());
                    }
                }
                () = at(self.audio_deadline) => {
                    self.audio_deadline = None;
                    tracing::warn!(call_id = self.call_id, "maximum call duration reached without T.38");
                    let o = if self.trunk.cfg.t38_policy.accepts_t38() {
                        failed(FailureReason::MaxDuration)
                    } else {
                        self.audio_only_outcome()
                    };
                    break (o, true, false);
                }
            }
        };
        self.cancel_session();
        // Let the session finish what it is doing (it writes the received pages when it is
        // cancelled) before the call result is read.
        if self.t38 && !self.session_finished {
            let wait = async { irx.recv().await };
            let _ = tokio::time::timeout(SESSION_FINISH_WAIT, wait).await;
        }
        if send_bye {
            // The call is over either way; do not hold the result back for an unresponsive peer
            // (the BYE transaction itself keeps retransmitting inside the dialog task).
            let wait = if shutting_down {
                BYE_WAIT_SHUTDOWN
            } else {
                BYE_WAIT
            };
            self.rec.bye_by = Some(ByeBy::Local);
            let _ = tokio::time::timeout(wait, self.control.bye()).await;
        }
        self.aux.abort_all();
        outcome
    }

    /// The dialog is over (peer BYE or event stream closed): collect the session's verdict.
    async fn after_dialog_end(
        &mut self,
        _peer_bye: bool,
        irx: &mut mpsc::UnboundedReceiver<Internal>,
        session_result: &mut Option<SessionOutcome>,
    ) -> CallOutcome {
        self.cancel_session();
        if self.t38 && session_result.is_none() {
            let wait = async { irx.recv().await.map(|Internal::Session(o)| o) };
            if let Ok(Some(o)) = tokio::time::timeout(SESSION_FINISH_WAIT, wait).await {
                self.session_finished = true;
                *session_result = Some(o);
            }
        }
        self.after_peer_bye(session_result.take())
    }
}
