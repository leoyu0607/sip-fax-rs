//! Registration client (docs/design/sip-core.md section 9): keeps one REGISTER binding alive.
//!
//! * REGISTER with `Expires`; a 401 / 407 is answered once with a digest response computed by
//!   [`crate::auth::DigestChallenge`];
//! * the binding is refreshed `max(10 % of the lifetime, 30 s)` before it expires;
//! * every failure (non-2xx, rejected credentials, timeout, transport error) is retried with
//!   exponential backoff (`backoff_initial`, doubling, capped at `backoff_max`);
//! * the state is observable through a [`tokio::sync::watch`] channel (for the Web UI / TUI).
//!
//! The task only needs a [`TransactionLayer`], so it can be tested against a fake registrar
//! that sits on a detached transport.

use std::net::SocketAddr;
use std::time::Duration;

use tokio::sync::{mpsc, watch};
use tokio::task::JoinHandle;
use tokio::time::Instant;

use crate::auth::DigestChallenge;
use crate::message::{Method, NameAddr, Request, Response, SipUri, Uri};
use crate::transaction::{TransactionLayer, TuEvent};
use crate::transport::TransportKind;

/// Registration settings.
#[derive(Debug, Clone)]
pub struct RegistrationConfig {
    /// Where to send REGISTER.
    pub registrar: SocketAddr,
    /// Transport towards the registrar.
    pub transport: TransportKind,
    /// Request-URI, e.g. `sip:registrar.example.com`; also the digest `uri`.
    pub registrar_uri: SipUri,
    /// Address-of-record (From / To), e.g. `sip:fax@example.com`.
    pub aor: SipUri,
    /// Contact to register (where the registrar sends calls).
    pub contact: SipUri,
    /// Digest user name.
    pub username: String,
    /// Digest password.
    pub password: String,
    /// Requested binding lifetime in seconds.
    pub expires: u32,
    /// First retry delay after a failure.
    pub backoff_initial: Duration,
    /// Upper bound of the retry delay.
    pub backoff_max: Duration,
    /// Optional `User-Agent` value.
    pub user_agent: Option<String>,
}

impl RegistrationConfig {
    /// Settings with the default lifetime (3600 s) and backoff (5 s doubling up to 300 s).
    pub fn new(
        registrar: SocketAddr,
        transport: TransportKind,
        registrar_uri: SipUri,
        aor: SipUri,
        contact: SipUri,
        username: impl Into<String>,
        password: impl Into<String>,
    ) -> Self {
        RegistrationConfig {
            registrar,
            transport,
            registrar_uri,
            aor,
            contact,
            username: username.into(),
            password: password.into(),
            expires: 3600,
            backoff_initial: Duration::from_secs(5),
            backoff_max: Duration::from_secs(300),
            user_agent: None,
        }
    }
}

/// What the registration is doing, as shown to the operator.
#[derive(Debug, Clone, PartialEq)]
pub enum RegistrationState {
    /// Not started yet.
    Unregistered,
    /// A REGISTER (or its authenticated retry) is in flight.
    Registering,
    /// The binding is active.
    Registered {
        /// Lifetime the registrar granted.
        granted: Duration,
        /// When the next refresh REGISTER will be sent.
        refresh_at: Instant,
    },
    /// The last attempt failed; the next one is scheduled.
    Failed {
        /// Why (status line, `timeout`, `authentication rejected`, ...).
        reason: String,
        /// Consecutive failures so far.
        attempts: u32,
        /// Delay until the retry.
        retry_in: Duration,
    },
    /// Stopped (after the de-registration attempt).
    Stopped,
}

/// Control handle of a running registration.
pub struct RegistrationHandle {
    state: watch::Receiver<RegistrationState>,
    stop: watch::Sender<bool>,
    join: JoinHandle<()>,
}

impl RegistrationHandle {
    /// The current state.
    pub fn state(&self) -> RegistrationState {
        self.state.borrow().clone()
    }

    /// A receiver that is notified of every state change.
    pub fn subscribe(&self) -> watch::Receiver<RegistrationState> {
        self.state.clone()
    }

    /// De-registers (`Expires: 0`, best effort, bounded by one transaction timeout) and ends
    /// the task.
    pub async fn stop(self) {
        let _ = self.stop.send(true);
        let _ = self.join.await;
    }
}

/// Delay until the refresh for a binding of `lifetime`: `lifetime - max(10 %, 30 s)`; for a
/// lifetime not longer than that margin, half of it.
pub fn refresh_delay(lifetime: Duration) -> Duration {
    let margin = (lifetime / 10).max(Duration::from_secs(30));
    if lifetime > margin {
        lifetime - margin
    } else {
        lifetime / 2
    }
}

/// Retry delay after `failures` consecutive failures (1-based): initial, 2x, 4x ... capped.
pub fn backoff_delay(cfg: &RegistrationConfig, failures: u32) -> Duration {
    let shift = failures.saturating_sub(1).min(20);
    cfg.backoff_initial
        .saturating_mul(1u32 << shift)
        .min(cfg.backoff_max)
}

/// Starts the registration task.
pub fn start(layer: TransactionLayer, cfg: RegistrationConfig) -> RegistrationHandle {
    let (state_tx, state_rx) = watch::channel(RegistrationState::Unregistered);
    let (stop_tx, stop_rx) = watch::channel(false);
    let join = tokio::spawn(run(layer, cfg, state_tx, stop_rx));
    RegistrationHandle {
        state: state_rx,
        stop: stop_tx,
        join,
    }
}

struct Session {
    layer: TransactionLayer,
    cfg: RegistrationConfig,
    call_id: String,
    from_tag: String,
    cseq: u32,
}

async fn sleep_or_stop(d: Duration, stop: &mut watch::Receiver<bool>) -> bool {
    tokio::select! {
        () = tokio::time::sleep(d) => false,
        _ = stop.wait_for(|s| *s) => true,
    }
}

async fn run(
    layer: TransactionLayer,
    cfg: RegistrationConfig,
    state: watch::Sender<RegistrationState>,
    mut stop: watch::Receiver<bool>,
) {
    let mut s = Session {
        layer,
        call_id: format!("{:016x}@sip-core", rand::random::<u64>()),
        from_tag: format!("{:08x}", rand::random::<u32>()),
        cseq: 0,
        cfg,
    };
    let mut failures = 0u32;
    loop {
        if *stop.borrow() {
            break;
        }
        let _ = state.send(RegistrationState::Registering);
        let wait = match s.attempt(s.cfg.expires).await {
            Ok(granted) => {
                failures = 0;
                let d = refresh_delay(granted);
                let _ = state.send(RegistrationState::Registered {
                    granted,
                    refresh_at: Instant::now() + d,
                });
                d
            }
            Err(reason) => {
                failures += 1;
                let d = backoff_delay(&s.cfg, failures);
                tracing::warn!(%reason, failures, retry_in = ?d, "registration failed");
                let _ = state.send(RegistrationState::Failed {
                    reason,
                    attempts: failures,
                    retry_in: d,
                });
                d
            }
        };
        if sleep_or_stop(wait, &mut stop).await {
            break;
        }
    }
    // De-register (only meaningful when a binding exists).
    if matches!(*state.borrow(), RegistrationState::Registered { .. }) {
        let _ = s.attempt(0).await;
    }
    let _ = state.send(RegistrationState::Stopped);
}

impl Session {
    fn build(&mut self, expires: u32, auth: Option<&(String, String)>) -> Request {
        self.cseq += 1;
        let mut req = Request::new(Method::Register, self.cfg.registrar_uri.clone());
        req.headers.push("Max-Forwards", "70");
        let mut from = NameAddr::new(Uri::Sip(self.cfg.aor.clone()));
        from.set_tag(self.from_tag.clone());
        req.headers.push("From", from.to_string());
        req.headers.push(
            "To",
            NameAddr::new(Uri::Sip(self.cfg.aor.clone())).to_string(),
        );
        req.headers.push("Call-ID", self.call_id.clone());
        req.headers.push("CSeq", format!("{} REGISTER", self.cseq));
        req.headers.push(
            "Contact",
            NameAddr::new(Uri::Sip(self.cfg.contact.clone())).to_string(),
        );
        req.headers.push("Expires", expires.to_string());
        if let Some(ua) = &self.cfg.user_agent {
            req.headers.push("User-Agent", ua.clone());
        }
        if let Some((name, value)) = auth {
            req.headers.push(name, value.clone());
        }
        req
    }

    /// One registration exchange including the single authenticated retry. Returns the
    /// granted lifetime.
    async fn attempt(&mut self, expires: u32) -> Result<Duration, String> {
        let mut auth: Option<(String, String)> = None;
        for round in 0..2 {
            let req = self.build(expires, auth.as_ref());
            let tx = self
                .layer
                .send_request(req, self.cfg.registrar, self.cfg.transport)
                .map_err(|e| e.to_string())?;
            let resp = final_response(tx.events).await?;
            let code = resp.status.code();
            if resp.status.is_success() {
                return self.granted(&resp, expires);
            }
            if matches!(code, 401 | 407) {
                if round == 1 {
                    return Err("authentication rejected".into());
                }
                let proxy = code == 407;
                let hdr = if proxy {
                    "Proxy-Authenticate"
                } else {
                    "WWW-Authenticate"
                };
                let challenge = resp
                    .headers
                    .get_all(hdr)
                    .find_map(|v| DigestChallenge::from_header_value(v, proxy))
                    .ok_or_else(|| format!("{code} without a usable Digest challenge"))?;
                let value = challenge.build_authorization(
                    &self.cfg.username,
                    &self.cfg.password,
                    "REGISTER",
                    &self.cfg.registrar_uri.to_string(),
                );
                auth = Some((challenge.header_name().to_string(), value));
                continue;
            }
            return Err(format!("{code} {}", resp.reason));
        }
        Err("authentication rejected".into())
    }

    /// Lifetime from the 2xx: our contact's `expires` parameter, else the `Expires` header,
    /// else what we asked for.
    fn granted(&self, resp: &Response, requested: u32) -> Result<Duration, String> {
        if requested == 0 {
            return Ok(Duration::ZERO);
        }
        let from_contact = resp
            .headers
            .contacts()
            .ok()
            .into_iter()
            .flatten()
            .filter(|c| match (c.uri.as_sip(), self.cfg.contact.host.as_str()) {
                (Some(u), h) => u.host.eq_ignore_ascii_case(h) && u.user == self.cfg.contact.user,
                _ => false,
            })
            .find_map(|c| {
                c.param("expires")
                    .flatten()
                    .and_then(|v| v.parse::<u32>().ok())
            });
        let secs = from_contact
            .or_else(|| {
                resp.headers
                    .get("Expires")
                    .and_then(|v| v.trim().parse::<u32>().ok())
            })
            .unwrap_or(requested);
        if secs == 0 {
            return Err("registrar granted a lifetime of 0".into());
        }
        Ok(Duration::from_secs(u64::from(secs)))
    }
}

async fn final_response(mut events: mpsc::UnboundedReceiver<TuEvent>) -> Result<Response, String> {
    while let Some(ev) = events.recv().await {
        match ev {
            TuEvent::Response(r) if r.status.is_final() => return Ok(r),
            TuEvent::Response(_) => {}
            TuEvent::Timeout => return Err("timeout (Timer F)".into()),
            TuEvent::TransportError => return Err("transport error".into()),
            _ => {}
        }
    }
    Err("transaction ended without a final response".into())
}
