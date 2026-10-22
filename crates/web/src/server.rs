//! The listener: plain TCP or rustls, HTTP/1.1 and HTTP/2 (with upgrades, for the WebSocket),
//! the security headers of spec 5.3, and a graceful shutdown.

use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use axum::Router;
use axum::extract::connect_info::ConnectInfo;
use axum::http::{HeaderName, HeaderValue, Request};
use hyper::body::Incoming;
use hyper_util::rt::{TokioExecutor, TokioIo};
use hyper_util::server::conn::auto::Builder;
use hyper_util::server::graceful::GracefulShutdown;
use rustls::ServerConfig;
use tokio::net::TcpListener;
use tokio::sync::watch;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;
use tower::ServiceExt;
use tower_http::set_header::SetResponseHeaderLayer;

/// A TLS handshake that takes longer than this is dropped.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

/// `Content-Security-Policy` of every response (spec 5.3).
pub const CSP: &str =
    "default-src 'self'; img-src 'self' data:; connect-src 'self'; frame-ancestors 'none'";

/// `Strict-Transport-Security` of HTTPS responses.
pub const HSTS: &str = "max-age=31536000";

/// Adds the security headers (spec 5.3) to every response of `router`; HSTS only with HTTPS.
pub fn with_security_headers(router: Router, https: bool) -> Router {
    let set = |name: &'static str, value: &'static str| {
        SetResponseHeaderLayer::overriding(
            HeaderName::from_static(name),
            HeaderValue::from_static(value),
        )
    };
    let r = router
        .layer(set("content-security-policy", CSP))
        .layer(set("x-content-type-options", "nosniff"))
        .layer(set("referrer-policy", "same-origin"));
    if https {
        r.layer(set("strict-transport-security", HSTS))
    } else {
        r
    }
}

/// The running server.
pub struct WebServer {
    addr: SocketAddr,
    stop: watch::Sender<bool>,
    task: JoinHandle<()>,
}

impl WebServer {
    /// Binds `bind` and serves `app` (with TLS when `tls` is set). Every request carries
    /// [`ConnectInfo<SocketAddr>`] with the client address.
    ///
    /// # Errors
    /// The address cannot be bound.
    pub async fn start(
        bind: SocketAddr,
        app: Router,
        tls: Option<Arc<ServerConfig>>,
    ) -> std::io::Result<WebServer> {
        let listener = TcpListener::bind(bind).await?;
        let addr = listener.local_addr()?;
        let (stop, stop_rx) = watch::channel(false);
        let task = tokio::spawn(accept_loop(
            listener,
            app,
            tls.map(TlsAcceptor::from),
            stop_rx,
        ));
        Ok(WebServer { addr, stop, task })
    }

    /// The bound address (the real port when `bind` had port 0).
    pub fn local_addr(&self) -> SocketAddr {
        self.addr
    }

    /// Stops accepting, lets the requests in progress finish for at most `grace`, then closes
    /// what is left (long-lived connections such as WebSockets included).
    pub async fn shutdown(self, grace: Duration) {
        let _ = self.stop.send(true);
        let mut task = self.task;
        if tokio::time::timeout(grace, &mut task).await.is_err() {
            task.abort();
            let _ = task.await;
        }
    }
}

async fn accept_loop(
    listener: TcpListener,
    app: Router,
    tls: Option<TlsAcceptor>,
    mut stop: watch::Receiver<bool>,
) {
    let graceful = GracefulShutdown::new();
    let builder = Builder::new(TokioExecutor::new());
    loop {
        let (tcp, peer) = tokio::select! {
            _ = stop.changed() => break,
            r = listener.accept() => match r {
                Ok(x) => x,
                Err(e) => {
                    // e.g. too many open files: do not spin
                    tracing::warn!("web: accept failed: {e}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            },
        };
        let _ = tcp.set_nodelay(true);
        let app = app.clone();
        let service = hyper::service::service_fn(move |mut req: Request<Incoming>| {
            req.extensions_mut().insert(ConnectInfo(peer));
            let app = app.clone();
            async move { Ok::<_, Infallible>(app.oneshot(req).await.unwrap_or_else(|e| match e {})) }
        });
        let builder = builder.clone();
        let watcher = graceful.watcher();
        let tls = tls.clone();
        tokio::spawn(async move {
            match tls {
                None => {
                    let conn = builder.serve_connection_with_upgrades(TokioIo::new(tcp), service);
                    let _ = watcher.watch(conn.into_owned()).await;
                }
                Some(acceptor) => {
                    let stream =
                        match tokio::time::timeout(HANDSHAKE_TIMEOUT, acceptor.accept(tcp)).await {
                            Ok(Ok(s)) => s,
                            Ok(Err(e)) => {
                                tracing::debug!(%peer, "web: TLS handshake failed: {e}");
                                return;
                            }
                            Err(_) => {
                                tracing::debug!(%peer, "web: TLS handshake timed out");
                                return;
                            }
                        };
                    let conn =
                        builder.serve_connection_with_upgrades(TokioIo::new(stream), service);
                    let _ = watcher.watch(conn.into_owned()).await;
                }
            }
        });
    }
    drop(listener);
    graceful.shutdown().await;
}
