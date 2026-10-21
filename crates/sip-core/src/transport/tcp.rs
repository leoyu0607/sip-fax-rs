//! TCP: stream decoding, listener, connection pool keyed by peer address, idle timeout.

use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use bytes::Bytes;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{mpsc, watch};

use super::framing::take_frame;
use super::tap::{self, MessageTap, TapDirection};
use super::{
    Gate, Incoming, SourcePolicy, Tasks, TransportKind, admit_request, lock, wait_shutdown,
};
use crate::message::{ParseError, SipMessage, parse_message};

/// What the stream decoder found in the byte stream.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamEvent {
    /// A complete, parsed message.
    Message(SipMessage),
    /// A CRLFCRLF keep-alive "ping": the connection must answer with a single CRLF.
    Ping,
    /// A framed message that failed to parse; the stream stays usable.
    Malformed(ParseError),
    /// Framing is lost (bad Content-Length, oversize); the connection must be closed.
    Fatal(String),
}

/// Incremental decoder for a SIP byte stream (RFC 3261 18.3, RFC 5626 keep-alives). Feed it
/// whatever the socket returns; message boundaries and segmentation do not matter.
#[derive(Debug, Default)]
pub struct StreamDecoder {
    buf: Vec<u8>,
}

impl StreamDecoder {
    /// Empty decoder.
    pub fn new() -> Self {
        Self::default()
    }

    /// Bytes buffered but not yet part of a complete message.
    pub fn buffered(&self) -> usize {
        self.buf.len()
    }

    /// Appends `data` and returns every event that became available.
    pub fn push(&mut self, data: &[u8]) -> Vec<StreamEvent> {
        self.push_raw(data).into_iter().map(|(e, _)| e).collect()
    }

    /// Like [`Self::push`], but pairs each event with the raw bytes of the frame it came from
    /// (empty for keep-alives and framing errors).
    pub fn push_raw(&mut self, data: &[u8]) -> Vec<(StreamEvent, Bytes)> {
        self.buf.extend_from_slice(data);
        let mut events: Vec<(StreamEvent, Bytes)> = Vec::new();
        loop {
            if self.buf.starts_with(b"\r\n\r\n") {
                self.buf.drain(..4);
                events.push((StreamEvent::Ping, Bytes::new()));
                continue;
            }
            if self.buf == b"\r" {
                break; // first half of a CRLF
            }
            if self.buf.starts_with(b"\r\n") {
                // "\r\n" alone, or "\r\n\r": could still become a double CRLF.
                if self.buf.len() == 2 || (self.buf.len() == 3 && self.buf[2] == b'\r') {
                    break;
                }
                self.buf.drain(..2); // single CRLF keep-alive: ignore
                continue;
            }
            match take_frame(&mut self.buf) {
                Ok(Some(frame)) => {
                    let raw = Bytes::copy_from_slice(&frame);
                    match parse_message(&frame) {
                        Ok((msg, _)) => events.push((StreamEvent::Message(msg), raw)),
                        Err(e) => events.push((StreamEvent::Malformed(e), raw)),
                    }
                }
                Ok(None) => break,
                Err(e) => {
                    self.buf.clear();
                    events.push((StreamEvent::Fatal(e.to_string()), Bytes::new()));
                    break;
                }
            }
        }
        events
    }
}

struct Entry {
    id: u64,
    tx: mpsc::Sender<Bytes>,
}

/// State shared by the listener, all connection tasks and the router.
pub(super) struct TcpShared {
    local: SocketAddr,
    pool: Mutex<HashMap<SocketAddr, Entry>>,
    next_id: AtomicU64,
    idle: Duration,
    connect_timeout: Duration,
    policy: Arc<SourcePolicy>,
    in_tx: mpsc::Sender<Incoming>,
    shutdown: watch::Receiver<bool>,
    tasks: Tasks,
    tap: Option<MessageTap>,
    reply_to_malformed: bool,
}

impl TcpShared {
    #[allow(clippy::too_many_arguments)]
    pub(super) fn new(
        local: SocketAddr,
        idle: Duration,
        connect_timeout: Duration,
        policy: Arc<SourcePolicy>,
        in_tx: mpsc::Sender<Incoming>,
        shutdown: watch::Receiver<bool>,
        tasks: Tasks,
        tap: Option<MessageTap>,
        reply_to_malformed: bool,
    ) -> Self {
        TcpShared {
            local,
            pool: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
            idle,
            connect_timeout,
            policy,
            in_tx,
            shutdown,
            tasks,
            tap,
            reply_to_malformed,
        }
    }

    pub(super) fn has_connection(&self, peer: SocketAddr) -> bool {
        lock(&self.pool).contains_key(&peer)
    }

    pub(super) fn connection_count(&self) -> usize {
        lock(&self.pool).len()
    }

    fn remove(&self, peer: SocketAddr, id: u64) {
        let mut pool = lock(&self.pool);
        if pool.get(&peer).is_some_and(|e| e.id == id) {
            pool.remove(&peer);
        }
    }

    /// Registers a connection for `peer`, replacing a stale entry. Returns its id and the
    /// receiver that the connection task drains.
    fn register(&self, peer: SocketAddr) -> (u64, mpsc::Receiver<Bytes>) {
        let (tx, rx) = mpsc::channel(256);
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        lock(&self.pool).insert(peer, Entry { id, tx });
        (id, rx)
    }

    /// Writes to the existing connection with `peer`; `false` when there is none.
    pub(super) async fn send_existing(&self, peer: SocketAddr, bytes: Bytes) -> bool {
        let found = lock(&self.pool).get(&peer).map(|e| (e.id, e.tx.clone()));
        let Some((id, tx)) = found else {
            return false;
        };
        if tx.send(bytes).await.is_ok() {
            true
        } else {
            self.remove(peer, id); // the connection task is gone
            false
        }
    }

    /// Sends to `dest`, opening (and pooling) a connection when necessary.
    pub(super) async fn send(self: &Arc<Self>, dest: SocketAddr, bytes: Bytes) {
        if self.send_existing(dest, bytes.clone()).await {
            return;
        }
        let (id, rx) = self.register(dest);
        let shared = self.clone();
        self.tasks.spawn(async move {
            let connect = TcpStream::connect(dest);
            let stream = match tokio::time::timeout(shared.connect_timeout, connect).await {
                Ok(Ok(s)) => s,
                Ok(Err(e)) => {
                    tracing::warn!(%dest, error = %e, "TCP connect failed");
                    shared.remove(dest, id);
                    return;
                }
                Err(_) => {
                    tracing::warn!(%dest, "TCP connect timed out");
                    shared.remove(dest, id);
                    return;
                }
            };
            conn_loop(shared, stream, dest, id, rx).await;
        });
        // The entry exists already, so this queues behind the connect.
        let entry_tx = lock(&self.pool).get(&dest).map(|e| e.tx.clone());
        if let Some(tx) = entry_tx {
            let _ = tx.send(bytes).await;
        }
    }
}

pub(super) async fn accept_loop(listener: TcpListener, shared: Arc<TcpShared>) {
    let mut shutdown = shared.shutdown.clone();
    loop {
        let (stream, peer) = tokio::select! {
            r = listener.accept() => match r {
                Ok(v) => v,
                Err(e) => {
                    tracing::warn!(error = %e, "TCP accept failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            () = wait_shutdown(&mut shutdown) => return,
        };
        let (id, rx) = shared.register(peer);
        let s = shared.clone();
        shared.tasks.spawn(conn_loop(s, stream, peer, id, rx));
    }
}

async fn conn_loop(
    shared: Arc<TcpShared>,
    stream: TcpStream,
    peer: SocketAddr,
    id: u64,
    mut rx: mpsc::Receiver<Bytes>,
) {
    let _ = stream.set_nodelay(true);
    let local = stream.local_addr().unwrap_or(shared.local);
    let (mut rd, mut wr) = stream.into_split();
    let mut dec = StreamDecoder::new();
    let mut buf = vec![0u8; 16 * 1024];
    let mut shutdown = shared.shutdown.clone();
    let idle = tokio::time::sleep(shared.idle);
    tokio::pin!(idle);
    'conn: loop {
        tokio::select! {
            r = rd.read(&mut buf) => {
                let n = match r {
                    Ok(0) | Err(_) => break 'conn,
                    Ok(n) => n,
                };
                idle.as_mut().reset(tokio::time::Instant::now() + shared.idle);
                for (ev, raw) in dec.push_raw(&buf[..n]) {
                    if !raw.is_empty() {
                        tap::emit(&shared.tap, TapDirection::Received, peer, TransportKind::Tcp, &raw);
                    }
                    match ev {
                        StreamEvent::Ping => {
                            if wr.write_all(b"\r\n").await.is_err() {
                                break 'conn;
                            }
                        }
                        StreamEvent::Malformed(e) => {
                            if shared.reply_to_malformed && shared.policy.decide(peer.ip()).admits()
                                && let Some(reply) = super::recover::bad_request_reply(&raw, peer) {
                                    tap::emit(&shared.tap, TapDirection::Sent, peer, TransportKind::Tcp, &reply);
                                    if wr.write_all(&reply).await.is_err() {
                                        break 'conn;
                                    }
                                    continue;
                                }
                            tracing::debug!(%peer, error = %e, "malformed message on TCP dropped");
                        }
                        StreamEvent::Fatal(e) => {
                            tracing::warn!(%peer, error = %e, "TCP framing lost, closing");
                            break 'conn;
                        }
                        StreamEvent::Message(msg) => {
                            let msg = match msg {
                                SipMessage::Request(mut req) => {
                                    match admit_request(&shared.policy, &mut req, peer) {
                                        Gate::Deliver => SipMessage::Request(req),
                                        Gate::Drop => break 'conn,
                                        Gate::Reject(bytes) => {
                                            tap::emit(&shared.tap, TapDirection::Sent, peer, TransportKind::Tcp, &bytes);
                                            if wr.write_all(&bytes).await.is_err() {
                                                break 'conn;
                                            }
                                            continue;
                                        }
                                    }
                                }
                                resp => resp,
                            };
                            let incoming = Incoming {
                                msg,
                                source: peer,
                                transport: TransportKind::Tcp,
                                local,
                            };
                            if shared.in_tx.send(incoming).await.is_err() {
                                break 'conn;
                            }
                        }
                    }
                }
            }
            out = rx.recv() => {
                let Some(bytes) = out else { break 'conn };
                idle.as_mut().reset(tokio::time::Instant::now() + shared.idle);
                if let Err(e) = wr.write_all(&bytes).await {
                    tracing::debug!(%peer, error = %e, "TCP write failed");
                    break 'conn;
                }
            }
            () = &mut idle => {
                tracing::debug!(%peer, "TCP connection idle, closing");
                break 'conn;
            }
            () = wait_shutdown(&mut shutdown) => break 'conn,
        }
    }
    shared.remove(peer, id);
    let _ = wr.shutdown().await;
}

#[cfg(test)]
mod tests {
    use super::*;

    const A: &str = "OPTIONS sip:a@b SIP/2.0\r\nVia: SIP/2.0/TCP h;branch=z9hG4bK1\r\nTo: <sip:a@b>\r\nFrom: <sip:c@d>;tag=1\r\nCall-ID: 1\r\nCSeq: 1 OPTIONS\r\nMax-Forwards: 70\r\nContent-Length: 4\r\n\r\nabcd";
    const B: &str = "SIP/2.0 200 OK\r\nVia: SIP/2.0/TCP h;branch=z9hG4bK1\r\nTo: <sip:a@b>;tag=2\r\nFrom: <sip:c@d>;tag=1\r\nCall-ID: 1\r\nCSeq: 1 OPTIONS\r\nContent-Length: 0\r\n\r\n";

    fn methods(evs: &[StreamEvent]) -> Vec<String> {
        evs.iter()
            .map(|e| match e {
                StreamEvent::Message(SipMessage::Request(r)) => r.method.to_string(),
                StreamEvent::Message(SipMessage::Response(r)) => r.status.to_string(),
                StreamEvent::Ping => "PING".into(),
                StreamEvent::Malformed(_) => "MALFORMED".into(),
                StreamEvent::Fatal(_) => "FATAL".into(),
            })
            .collect()
    }

    #[test]
    fn n04_two_messages_in_one_segment() {
        let mut d = StreamDecoder::new();
        let evs = d.push(format!("{A}{B}").as_bytes());
        assert_eq!(methods(&evs), ["OPTIONS", "200"]);
        assert_eq!(d.buffered(), 0);
    }

    #[test]
    fn n04_every_split_point_yields_the_same_messages() {
        let all = format!("{A}{B}");
        for cut in 0..=all.len() {
            let mut d = StreamDecoder::new();
            let mut evs = d.push(&all.as_bytes()[..cut]);
            evs.extend(d.push(&all.as_bytes()[cut..]));
            assert_eq!(methods(&evs), ["OPTIONS", "200"], "split at {cut}");
        }
    }

    #[test]
    fn n04_one_message_in_three_segments() {
        let mut d = StreamDecoder::new();
        let b = A.as_bytes();
        assert!(d.push(&b[..30]).is_empty());
        assert!(d.push(&b[30..b.len() - 2]).is_empty());
        let evs = d.push(&b[b.len() - 2..]);
        let StreamEvent::Message(SipMessage::Request(r)) = &evs[0] else {
            panic!("{evs:?}")
        };
        assert_eq!(&r.body[..], b"abcd");
    }

    #[test]
    fn n04_byte_by_byte() {
        let all = format!("{A}\r\n\r\n{B}");
        let mut d = StreamDecoder::new();
        let mut evs = vec![];
        for b in all.as_bytes() {
            evs.extend(d.push(&[*b]));
        }
        assert_eq!(methods(&evs), ["OPTIONS", "PING", "200"]);
    }

    #[test]
    fn n05_keepalives() {
        let mut d = StreamDecoder::new();
        assert_eq!(methods(&d.push(b"\r\n\r\n")), ["PING"]);
        assert!(
            d.push(b"\r\n").is_empty(),
            "lone CRLF: wait, nothing to report"
        );
        assert_eq!(d.buffered(), 2);
        // a lone CRLF followed by a message is ignored
        assert_eq!(methods(&d.push(A.as_bytes())), ["OPTIONS"]);
        assert_eq!(methods(&d.push(b"\r\n\r\n\r\n\r\n")), ["PING", "PING"]);
        // "\r\n\r" then "\n" completes a ping
        assert!(d.push(b"\r\n\r").is_empty());
        assert_eq!(methods(&d.push(b"\n")), ["PING"]);
    }

    #[test]
    fn n04_bad_content_length_is_fatal_and_bad_message_is_not() {
        let mut d = StreamDecoder::new();
        let evs = d.push(b"INVITE sip:a@b SIP/2.0\r\nContent-Length: nope\r\n\r\n");
        assert_eq!(methods(&evs), ["FATAL"]);
        // framed but unparseable (no mandatory headers): stream continues
        let mut d = StreamDecoder::new();
        let evs = d.push(format!("BYE sip:a@b SIP/2.0\r\nContent-Length: 0\r\n\r\n{B}").as_bytes());
        assert_eq!(methods(&evs), ["MALFORMED", "200"]);
    }
}
