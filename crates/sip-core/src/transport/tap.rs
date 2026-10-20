//! Optional message tap: an observer that sees every SIP message the transport sends or
//! receives (raw bytes, peer, direction, wall-clock time). It is off unless configured
//! (`TransportConfig::tap`) and it never blocks the stack: the callback runs inline on the I/O
//! task and must return quickly, and the channel flavour ([`MessageTap::channel`]) drops
//! events (counting them) instead of waiting when its receiver is slow.

use std::fmt;
use std::net::SocketAddr;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::SystemTime;

use bytes::Bytes;
use tokio::sync::mpsc;

use super::TransportKind;

/// Which way a tapped message travelled.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TapDirection {
    /// The stack received it.
    Received,
    /// The stack sent it.
    Sent,
}

/// One tapped SIP message.
#[derive(Debug, Clone)]
pub struct TapEvent {
    /// Wall-clock time the message was received / handed to the socket.
    pub at: SystemTime,
    /// Direction.
    pub direction: TapDirection,
    /// The remote address (source of a received message, destination of a sent one).
    pub peer: SocketAddr,
    /// Transport used.
    pub transport: TransportKind,
    /// The message exactly as it was on the wire (a received message is tapped before
    /// parsing, so unparseable datagrams show up too).
    pub bytes: Bytes,
}

type Callback = dyn Fn(TapEvent) + Send + Sync;

/// Handle to the observer; cheap to clone.
#[derive(Clone)]
pub struct MessageTap {
    f: Arc<Callback>,
    dropped: Arc<AtomicU64>,
}

impl fmt::Debug for MessageTap {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MessageTap")
            .field("dropped", &self.dropped())
            .finish()
    }
}

impl MessageTap {
    /// A tap that calls `f` for every message. `f` runs on the transport's I/O tasks and must
    /// not block.
    pub fn new(f: impl Fn(TapEvent) + Send + Sync + 'static) -> Self {
        MessageTap {
            f: Arc::new(f),
            dropped: Arc::new(AtomicU64::new(0)),
        }
    }

    /// A tap that forwards events into a bounded channel. When the channel is full the event
    /// is dropped and counted ([`Self::dropped`]); the stack never waits.
    pub fn channel(capacity: usize) -> (MessageTap, mpsc::Receiver<TapEvent>) {
        let (tx, rx) = mpsc::channel(capacity.max(1));
        let dropped = Arc::new(AtomicU64::new(0));
        let d = dropped.clone();
        let tap = MessageTap {
            f: Arc::new(move |ev| {
                if tx.try_send(ev).is_err() {
                    d.fetch_add(1, Ordering::Relaxed);
                }
            }),
            dropped,
        };
        (tap, rx)
    }

    /// Number of events a channel tap had to drop because its receiver was full or gone.
    pub fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    pub(crate) fn emit(
        &self,
        direction: TapDirection,
        peer: SocketAddr,
        transport: TransportKind,
        bytes: &[u8],
    ) {
        (self.f)(TapEvent {
            at: SystemTime::now(),
            direction,
            peer,
            transport,
            bytes: Bytes::copy_from_slice(bytes),
        });
    }
}

/// Emits to an optional tap.
pub(crate) fn emit(
    tap: &Option<MessageTap>,
    direction: TapDirection,
    peer: SocketAddr,
    transport: TransportKind,
    bytes: &[u8],
) {
    if let Some(t) = tap {
        t.emit(direction, peer, transport, bytes);
    }
}
