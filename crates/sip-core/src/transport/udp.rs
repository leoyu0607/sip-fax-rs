//! UDP receive loop.

use std::net::SocketAddr;
use std::sync::Arc;

use tokio::net::UdpSocket;
use tokio::sync::{mpsc, watch};

use super::tap::{self, MessageTap, TapDirection};
use super::{Gate, Incoming, SourcePolicy, TransportKind, admit_request, wait_shutdown};
use crate::message::{SipMessage, parse_datagram};

/// RFC 5626 section 4.4.1: a double CRLF "ping" is answered with a single CRLF "pong"; a lone
/// CRLF is ignored.
const PING: &[u8] = b"\r\n\r\n";
const PONG: &[u8] = b"\r\n";

pub(super) async fn recv_loop(
    socket: Arc<UdpSocket>,
    local: SocketAddr,
    policy: Arc<SourcePolicy>,
    in_tx: mpsc::Sender<Incoming>,
    mut shutdown: watch::Receiver<bool>,
    tap: Option<MessageTap>,
    reply_to_malformed: bool,
) {
    let mut buf = vec![0u8; 65_536 + 1];
    loop {
        let (n, source) = tokio::select! {
            r = socket.recv_from(&mut buf) => match r {
                Ok(v) => v,
                Err(e) => {
                    // e.g. ICMP port unreachable surfacing as ECONNRESET on some platforms
                    tracing::debug!(error = %e, "UDP recv error");
                    continue;
                }
            },
            () = wait_shutdown(&mut shutdown) => return,
        };
        let data = &buf[..n];
        if data == PING {
            if let Err(e) = socket.send_to(PONG, source).await {
                tracing::debug!(%source, error = %e, "keep-alive pong failed");
            }
            continue;
        }
        if data == PONG {
            continue;
        }
        tap::emit(
            &tap,
            TapDirection::Received,
            source,
            TransportKind::Udp,
            data,
        );
        let msg = match parse_datagram(data) {
            Ok(m) => m,
            Err(e) => {
                if reply_to_malformed && policy.decide(source.ip()).admits() {
                    if let Some(reply) = super::recover::bad_request_reply(data, source) {
                        tracing::debug!(%source, error = %e, "unparseable request answered with 400");
                        tap::emit(&tap, TapDirection::Sent, source, TransportKind::Udp, &reply);
                        let _ = socket.send_to(&reply, source).await;
                        continue;
                    }
                }
                tracing::debug!(%source, error = %e, "unparseable UDP datagram dropped");
                continue;
            }
        };
        let msg = match msg {
            SipMessage::Request(mut req) => match admit_request(&policy, &mut req, source) {
                Gate::Deliver => SipMessage::Request(req),
                Gate::Drop => continue,
                Gate::Reject(bytes) => {
                    tap::emit(&tap, TapDirection::Sent, source, TransportKind::Udp, &bytes);
                    if let Err(e) = socket.send_to(&bytes, source).await {
                        tracing::debug!(%source, error = %e, "403 send failed");
                    }
                    continue;
                }
            },
            resp => resp,
        };
        let incoming = Incoming {
            msg,
            source,
            transport: TransportKind::Udp,
            local,
        };
        if in_tx.send(incoming).await.is_err() {
            return; // nobody is listening any more
        }
    }
}
