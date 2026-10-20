//! Transport layer (docs/design/sip-core.md section 3): UDP socket, TCP listener plus pooled
//! outbound connections, RFC 3581 `received`/`rport` stamping, response routing
//! (RFC 3261 18.2.2), top-Via stamping of outgoing requests, RFC 5626 CRLF keep-alives and
//! the inbound source access policy.
//!
//! The layer is connected to its users only through channels, so the transaction layer can be
//! driven without any socket:
//!
//! * inbound: [`Incoming`] messages arrive on an `mpsc::Receiver<Incoming>`;
//! * outbound: [`TransportHandle`] methods push [`Outgoing`] items. A real
//!   [`TransportLayer`] writes them to sockets; [`TransportHandle::detached`] hands the
//!   outgoing side to the test instead.

pub mod framing;
pub mod policy;
pub mod route;
mod tcp;
mod udp;

use std::io;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::Duration;

use bytes::Bytes;
use tokio::net::{TcpListener, UdpSocket};
use tokio::sync::{mpsc, watch};
use tokio::task::JoinSet;

pub use policy::{Cidr, CidrParseError, Decision, PolicyMode, SourcePolicy};
pub use tcp::{StreamDecoder, StreamEvent};

use crate::message::{Request, Response, SipMessage, StatusCode};

/// Transport protocol of a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TransportKind {
    /// UDP (unreliable, datagram).
    Udp,
    /// TCP (reliable, stream).
    Tcp,
}

impl TransportKind {
    /// Token used in the Via header.
    pub fn as_str(self) -> &'static str {
        match self {
            TransportKind::Udp => "UDP",
            TransportKind::Tcp => "TCP",
        }
    }

    /// `true` for stream transports: no retransmission timers are needed.
    pub fn is_reliable(self) -> bool {
        matches!(self, TransportKind::Tcp)
    }
}

/// A message received from the network.
#[derive(Debug, Clone, PartialEq)]
pub struct Incoming {
    /// The parsed message. For requests the topmost Via has already been stamped with
    /// `received` / `rport`.
    pub msg: SipMessage,
    /// Remote address (for TCP: the peer of the connection it arrived on).
    pub source: SocketAddr,
    /// Transport it arrived on.
    pub transport: TransportKind,
    /// Local address it arrived on.
    pub local: SocketAddr,
}

/// A message to be sent.
#[derive(Debug, Clone, PartialEq)]
pub enum Outgoing {
    /// A request to `dest`. The topmost Via has already been stamped by the handle.
    Request {
        /// The request.
        req: Request,
        /// Destination address.
        dest: SocketAddr,
        /// Transport to use.
        transport: TransportKind,
    },
    /// A response. It is routed by its topmost Via; `source` is the address the request came
    /// from (the connection to reuse for reliable transports).
    Response {
        /// The response.
        resp: Response,
        /// Source address of the request being answered.
        source: SocketAddr,
        /// Transport the request arrived on.
        transport: TransportKind,
    },
}

/// Transport failures.
#[derive(Debug, thiserror::Error)]
pub enum TransportError {
    /// The transport layer has shut down (or the channel to it is gone).
    #[error("transport closed")]
    Closed,
    /// The requested transport is not configured.
    #[error("{0} transport is not enabled")]
    NotEnabled(&'static str),
    /// An I/O error.
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
}

/// The address advertised in the topmost Via of outgoing requests (NAT / multi-homed hosts).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Advertised {
    /// Host or IP literal (no brackets needed for IPv6).
    pub host: String,
    /// Port; `None` uses the port the respective transport is bound to.
    pub port: Option<u16>,
}

/// Transport configuration.
#[derive(Debug, Clone)]
pub struct TransportConfig {
    /// UDP bind address (`None` = no UDP).
    pub udp_bind: Option<SocketAddr>,
    /// TCP listen address (`None` = no TCP listener and no TCP at all).
    pub tcp_bind: Option<SocketAddr>,
    /// Address written into the sent-by of outgoing requests. Default: the bound address.
    pub advertised: Option<Advertised>,
    /// A TCP connection without traffic for this long is closed.
    pub tcp_idle_timeout: Duration,
    /// Outbound TCP connect timeout.
    pub tcp_connect_timeout: Duration,
    /// Inbound source filter (applied to requests).
    pub source_policy: SourcePolicy,
    /// Capacity of the inbound / outbound channels.
    pub queue_capacity: usize,
}

impl Default for TransportConfig {
    fn default() -> Self {
        TransportConfig {
            udp_bind: None,
            tcp_bind: None,
            advertised: None,
            tcp_idle_timeout: Duration::from_secs(120),
            tcp_connect_timeout: Duration::from_secs(10),
            source_policy: SourcePolicy::allow_all(),
            queue_capacity: 1024,
        }
    }
}

#[derive(Debug)]
struct SentBy {
    udp: (String, u16),
    tcp: (String, u16),
}

/// Cheap, cloneable sender side of the transport.
#[derive(Clone)]
pub struct TransportHandle {
    out: mpsc::Sender<Outgoing>,
    sent_by: Arc<SentBy>,
    pool: Option<Arc<tcp::TcpShared>>,
}

/// A transport that is not attached to any socket; see [`TransportHandle::detached`].
pub struct Detached {
    /// Handle to give to the layer under test.
    pub handle: TransportHandle,
    /// Everything the layer under test sends.
    pub outgoing: mpsc::Receiver<Outgoing>,
    /// Inject messages "from the network" here.
    pub inject: mpsc::Sender<Incoming>,
    /// Receiver for the layer under test (pairs with `inject`).
    pub incoming: mpsc::Receiver<Incoming>,
}

impl TransportHandle {
    /// A handle whose outgoing queue and incoming feed are plain channels (no sockets), for
    /// driving the transaction layer and everything above it deterministically. `advertised`
    /// is used as sent-by for both transports.
    pub fn detached(advertised: SocketAddr) -> Detached {
        let (out, outgoing) = mpsc::channel(1024);
        let (inject, incoming) = mpsc::channel(1024);
        let host = advertised.ip().to_string();
        let sent_by = Arc::new(SentBy {
            udp: (host.clone(), advertised.port()),
            tcp: (host, advertised.port()),
        });
        Detached {
            handle: TransportHandle {
                out,
                sent_by,
                pool: None,
            },
            outgoing,
            inject,
            incoming,
        }
    }

    /// The sent-by (host, port) written into outgoing requests for `kind`.
    pub fn sent_by(&self, kind: TransportKind) -> (&str, u16) {
        let (h, p) = match kind {
            TransportKind::Udp => &self.sent_by.udp,
            TransportKind::Tcp => &self.sent_by.tcp,
        };
        (h.as_str(), *p)
    }

    /// Stamps the topmost Via transport and sent-by (RFC 3261 18.1.1). Idempotent.
    pub fn stamp_request(&self, req: &mut Request, kind: TransportKind) {
        let (host, port) = self.sent_by(kind);
        route::stamp_outbound_via(req, kind, host, port);
    }

    /// Sends a request to `dest`, stamping the topmost Via first.
    pub async fn send_request(
        &self,
        mut req: Request,
        dest: SocketAddr,
        kind: TransportKind,
    ) -> Result<(), TransportError> {
        self.stamp_request(&mut req, kind);
        self.out
            .send(Outgoing::Request {
                req,
                dest,
                transport: kind,
            })
            .await
            .map_err(|_| TransportError::Closed)
    }

    /// Sends a response, routed by its topmost Via (see [`route::response_target`]).
    /// `source` is where the request came from.
    pub async fn send_response(
        &self,
        resp: Response,
        source: SocketAddr,
        kind: TransportKind,
    ) -> Result<(), TransportError> {
        self.out
            .send(Outgoing::Response {
                resp,
                source,
                transport: kind,
            })
            .await
            .map_err(|_| TransportError::Closed)
    }

    /// `true` while a pooled TCP connection to/from `peer` is open (real transport only).
    pub fn has_tcp_connection(&self, peer: SocketAddr) -> bool {
        self.pool.as_ref().is_some_and(|p| p.has_connection(peer))
    }

    /// Number of open pooled TCP connections (real transport only).
    pub fn tcp_connection_count(&self) -> usize {
        self.pool.as_ref().map_or(0, |p| p.connection_count())
    }
}

/// Tracks spawned tasks so that shutdown can wait for all of them.
#[derive(Clone, Default)]
pub(crate) struct Tasks(Arc<Mutex<JoinSet<()>>>);

fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

impl Tasks {
    pub(crate) fn spawn<F>(&self, f: F)
    where
        F: std::future::Future<Output = ()> + Send + 'static,
    {
        let mut set = lock(&self.0);
        while set.try_join_next().is_some() {} // reap finished tasks
        set.spawn(f);
    }

    pub(crate) async fn join_all(&self) {
        loop {
            let mut set = std::mem::take(&mut *lock(&self.0));
            if set.is_empty() {
                return;
            }
            while set.join_next().await.is_some() {}
        }
    }
}

/// Resolves when shutdown has been requested (or the layer was dropped).
pub(crate) async fn wait_shutdown(rx: &mut watch::Receiver<bool>) {
    loop {
        if *rx.borrow() {
            return;
        }
        if rx.changed().await.is_err() {
            return;
        }
    }
}

/// Result of checking a freshly received request against the source policy.
pub(crate) enum Gate {
    /// Hand the (stamped) request upwards.
    Deliver,
    /// Discard silently.
    Drop,
    /// Discard and send these bytes (a 403) back to the source.
    Reject(Bytes),
}

/// Applies the source policy and the Via stamping to a request that just entered the stack.
pub(crate) fn admit_request(policy: &SourcePolicy, req: &mut Request, source: SocketAddr) -> Gate {
    match policy.decide(source.ip()) {
        Decision::Allow => {}
        Decision::AllowAndWarn => {
            tracing::warn!(%source, method = %req.method,
                "request from a source outside the allow-list accepted (log-only mode)");
        }
        Decision::Drop => {
            tracing::debug!(%source, method = %req.method,
                "request from a source outside the allow-list dropped");
            return Gate::Drop;
        }
        Decision::Reject403 => {
            tracing::warn!(%source, method = %req.method,
                "request from a source outside the allow-list rejected with 403");
            if req.method == crate::message::Method::Ack {
                return Gate::Drop; // an ACK never gets a response
            }
            let tag = format!("{:08x}", rand::random::<u32>());
            // Stamp first so the reply carries received/rport like any other response.
            route::stamp_inbound_via(req, source);
            let resp = Response::for_request(req, StatusCode::FORBIDDEN, Some(&tag));
            return Gate::Reject(resp.to_bytes());
        }
    }
    route::stamp_inbound_via(req, source);
    Gate::Deliver
}

/// The running transport: sockets, tasks and the outgoing router.
pub struct TransportLayer {
    handle: TransportHandle,
    udp_local: Option<SocketAddr>,
    tcp_local: Option<SocketAddr>,
    shutdown: watch::Sender<bool>,
    tasks: Tasks,
}

impl TransportLayer {
    /// Binds the configured sockets and starts the I/O tasks. Returns the layer and the
    /// channel on which inbound messages arrive.
    pub async fn bind(
        cfg: TransportConfig,
    ) -> io::Result<(TransportLayer, mpsc::Receiver<Incoming>)> {
        let udp = match cfg.udp_bind {
            Some(a) => Some(Arc::new(UdpSocket::bind(a).await?)),
            None => None,
        };
        let listener = match cfg.tcp_bind {
            Some(a) => Some(TcpListener::bind(a).await?),
            None => None,
        };
        let udp_local = udp.as_ref().map(|s| s.local_addr()).transpose()?;
        let tcp_local = listener.as_ref().map(|l| l.local_addr()).transpose()?;

        let sent_by = |bound: Option<SocketAddr>| -> (String, u16) {
            let adv = cfg.advertised.as_ref();
            let host = adv
                .map(|a| a.host.clone())
                .or_else(|| bound.map(|b| b.ip().to_string()))
                .unwrap_or_default();
            let port = adv
                .and_then(|a| a.port)
                .or(bound.map(|b| b.port()))
                .unwrap_or(5060);
            (host, port)
        };
        let sent_by = Arc::new(SentBy {
            udp: sent_by(udp_local),
            tcp: sent_by(tcp_local),
        });

        let (in_tx, in_rx) = mpsc::channel(cfg.queue_capacity);
        let (out_tx, out_rx) = mpsc::channel(cfg.queue_capacity);
        let (shutdown, shutdown_rx) = watch::channel(false);
        let tasks = Tasks::default();
        let policy = Arc::new(cfg.source_policy.clone());

        let tcp_shared = match (listener, tcp_local) {
            (Some(listener), Some(local)) => {
                let shared = Arc::new(tcp::TcpShared::new(
                    local,
                    cfg.tcp_idle_timeout,
                    cfg.tcp_connect_timeout,
                    policy.clone(),
                    in_tx.clone(),
                    shutdown_rx.clone(),
                    tasks.clone(),
                ));
                let s = shared.clone();
                tasks.spawn(tcp::accept_loop(listener, s));
                Some(shared)
            }
            _ => None,
        };
        if let (Some(sock), Some(local)) = (udp.clone(), udp_local) {
            tasks.spawn(udp::recv_loop(
                sock,
                local,
                policy.clone(),
                in_tx.clone(),
                shutdown_rx.clone(),
            ));
        }
        tasks.spawn(route_outgoing(
            out_rx,
            udp,
            tcp_shared.clone(),
            shutdown_rx,
            tasks.clone(),
        ));

        let handle = TransportHandle {
            out: out_tx,
            sent_by,
            pool: tcp_shared,
        };
        Ok((
            TransportLayer {
                handle,
                udp_local,
                tcp_local,
                shutdown,
                tasks,
            },
            in_rx,
        ))
    }

    /// A handle for sending.
    pub fn handle(&self) -> TransportHandle {
        self.handle.clone()
    }

    /// Bound UDP address.
    pub fn udp_addr(&self) -> Option<SocketAddr> {
        self.udp_local
    }

    /// Bound TCP listener address.
    pub fn tcp_addr(&self) -> Option<SocketAddr> {
        self.tcp_local
    }

    /// Stops every task (listener, connections, router) and waits until they are gone.
    pub async fn shutdown(self) {
        let _ = self.shutdown.send(true);
        self.tasks.join_all().await;
    }
}

/// Outgoing router: serialises messages and writes them to the right socket / connection.
async fn route_outgoing(
    mut rx: mpsc::Receiver<Outgoing>,
    udp: Option<Arc<UdpSocket>>,
    tcp: Option<Arc<tcp::TcpShared>>,
    mut shutdown: watch::Receiver<bool>,
    tasks: Tasks,
) {
    loop {
        let item = tokio::select! {
            i = rx.recv() => match i { Some(i) => i, None => return },
            () = wait_shutdown(&mut shutdown) => return,
        };
        match item {
            Outgoing::Request {
                req,
                dest,
                transport,
            } => {
                let bytes = req.to_bytes();
                send_bytes(&udp, &tcp, transport, dest, bytes).await;
            }
            Outgoing::Response {
                resp,
                source,
                transport,
            } => {
                let bytes = resp.to_bytes();
                let target = resp
                    .headers
                    .top_via()
                    .ok()
                    .map(|v| route::response_target(&v, transport));
                match (transport, target) {
                    (TransportKind::Tcp, t) => {
                        let Some(pool) = tcp.clone() else {
                            tracing::warn!("TCP response but TCP is not enabled");
                            continue;
                        };
                        if pool.send_existing(source, bytes.clone()).await {
                            continue;
                        }
                        // The original connection is gone: reconnect to the sent-by.
                        let Some(t) = t else {
                            tracing::warn!(%source, "response without usable Via dropped");
                            continue;
                        };
                        let (u, tc, tasks2) = (udp.clone(), tcp.clone(), tasks.clone());
                        tasks2.spawn(async move {
                            if let Some(dest) = resolve(&t).await {
                                send_bytes(&u, &tc, TransportKind::Tcp, dest, bytes).await;
                            }
                        });
                    }
                    (TransportKind::Udp, t) => match t {
                        None => send_bytes(&udp, &tcp, TransportKind::Udp, source, bytes).await,
                        Some(t) => match t.socket_addr() {
                            Some(dest) => {
                                send_bytes(&udp, &tcp, TransportKind::Udp, dest, bytes).await
                            }
                            None => {
                                let (u, tc) = (udp.clone(), tcp.clone());
                                tasks.spawn(async move {
                                    if let Some(dest) = resolve(&t).await {
                                        send_bytes(&u, &tc, TransportKind::Udp, dest, bytes).await;
                                    }
                                });
                            }
                        },
                    },
                }
            }
        }
    }
}

async fn resolve(t: &route::Target) -> Option<SocketAddr> {
    if let Some(a) = t.socket_addr() {
        return Some(a);
    }
    match tokio::net::lookup_host((t.host.as_str(), t.port)).await {
        Ok(mut it) => it.next(),
        Err(e) => {
            tracing::warn!(host = %t.host, error = %e, "cannot resolve response destination");
            None
        }
    }
}

async fn send_bytes(
    udp: &Option<Arc<UdpSocket>>,
    tcp: &Option<Arc<tcp::TcpShared>>,
    kind: TransportKind,
    dest: SocketAddr,
    bytes: Bytes,
) {
    match kind {
        TransportKind::Udp => match udp {
            Some(s) => {
                if let Err(e) = s.send_to(&bytes, dest).await {
                    tracing::warn!(%dest, error = %e, "UDP send failed");
                }
            }
            None => tracing::warn!(%dest, "UDP send requested but UDP is not enabled"),
        },
        TransportKind::Tcp => match tcp {
            Some(p) => p.send(dest, bytes).await,
            None => tracing::warn!(%dest, "TCP send requested but TCP is not enabled"),
        },
    }
}
