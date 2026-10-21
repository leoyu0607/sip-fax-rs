//! A thin UDPTL wrapper around [`T38Engine`].
//!
//! Outgoing IFP packets get UDPTL framing (one sequence number each, with the configured
//! redundancy or FEC). Incoming datagrams go through [`UdptlReceiver`], which orders them,
//! drops duplicates and repairs losses; what cannot be repaired reaches the engine as lost
//! data, so a damaged HDLC frame is reported with a bad FCS instead of being silently glued
//! together.

use super::{EngineConfig, T38Engine, TimedIfp};
use t38::{ErrorRecovery, Ifp, ReceiverConfig, RxEvent, UdptlError, UdptlReceiver, UdptlSender};

/// An outgoing datagram with its due time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimedDatagram {
    /// Due time in milliseconds.
    pub at_ms: u64,
    /// The UDPTL datagram.
    pub bytes: Vec<u8>,
}

/// [`T38Engine`] speaking UDPTL.
#[derive(Debug)]
pub struct UdptlEngine {
    engine: T38Engine,
    sender: UdptlSender,
    receiver: UdptlReceiver,
    /// Since when packets have been held back for a missing one.
    held_since: Option<u64>,
    reorder_wait_ms: u64,
    /// Datagrams that could not be parsed, packets that could not be decoded, gaps reported.
    pub malformed: u64,
    /// Number of `Gap` events (unrecoverable losses) seen.
    pub gaps: u64,
}

impl UdptlEngine {
    /// Creates the engine. `recovery` is what we send; incoming datagrams may use either form.
    pub fn new(cfg: EngineConfig, recovery: ErrorRecovery, rx: ReceiverConfig) -> Self {
        Self {
            engine: T38Engine::new(cfg),
            sender: UdptlSender::new(recovery, 0),
            receiver: UdptlReceiver::new(rx),
            held_since: None,
            reorder_wait_ms: 60,
            malformed: 0,
            gaps: 0,
        }
    }

    /// How long a packet that arrived ahead of a hole is held before the hole is given up
    /// (default 60 ms).
    pub fn set_reorder_wait_ms(&mut self, ms: u64) {
        self.reorder_wait_ms = ms;
    }

    /// Limits the size of the datagrams we send (the peer's `T38FaxMaxDatagram`): redundancy
    /// entries that would exceed it are dropped, the primary packet is always sent.
    pub fn set_max_datagram(&mut self, max: usize) {
        self.sender = self.sender.clone().with_max_datagram(max);
    }

    /// The wrapped engine.
    pub fn engine(&self) -> &T38Engine {
        &self.engine
    }

    /// The wrapped engine, mutable.
    pub fn engine_mut(&mut self) -> &mut T38Engine {
        &mut self.engine
    }

    /// The UDPTL receiver counters.
    pub fn receiver_stats(&self) -> t38::ReceiverStats {
        self.receiver.stats()
    }

    /// See [`T38Engine::start`].
    pub fn start(&mut self, now_ms: u64) {
        self.engine.start(now_ms);
    }

    /// Advances the engine and gives up on holes that waited too long.
    pub fn advance(&mut self, now_ms: u64) {
        self.engine.advance(now_ms);
        if let Some(since) = self.held_since
            && now_ms.saturating_sub(since) >= self.reorder_wait_ms
        {
            let events = self.receiver.flush();
            self.held_since = None;
            self.deliver(now_ms, events);
        }
    }

    /// A datagram arrived.
    ///
    /// # Errors
    /// The parse error; the datagram is ignored (and counted).
    pub fn on_datagram(&mut self, now_ms: u64, bytes: &[u8]) -> Result<(), UdptlError> {
        self.advance(now_ms);
        match self.receiver.on_datagram(bytes) {
            Ok(events) => {
                self.deliver(now_ms, events);
                if self.receiver.held() > 0 {
                    self.held_since.get_or_insert(now_ms);
                } else {
                    self.held_since = None;
                }
                Ok(())
            }
            Err(e) => {
                self.malformed += 1;
                Err(e)
            }
        }
    }

    fn deliver(&mut self, now_ms: u64, events: Vec<RxEvent>) {
        let version = self.engine.config().version;
        for e in events {
            match e {
                RxEvent::Packet { ifp, .. } => match Ifp::decode_prefix(&ifp, version) {
                    Ok((pkt, _)) => self.engine.on_ifp(now_ms, &pkt),
                    Err(_) => {
                        self.malformed += 1;
                        self.engine.on_loss(now_ms);
                    }
                },
                RxEvent::Gap { .. } => {
                    self.gaps += 1;
                    self.engine.on_loss(now_ms);
                }
            }
        }
    }

    /// Ends the call.
    pub fn abort(&mut self, now_ms: u64) {
        self.engine.abort(now_ms);
    }

    /// Datagrams that fell due, in order, ready for the socket.
    pub fn poll_datagrams(&mut self) -> Vec<TimedDatagram> {
        let version = self.engine.config().version;
        let mut out = Vec::new();
        for TimedIfp { at_ms, ifp } in self.engine.poll_timed() {
            let Ok(bytes) = ifp.encode(version) else {
                continue;
            };
            if let Ok(datagram) = self.sender.build(&bytes) {
                out.push(TimedDatagram {
                    at_ms,
                    bytes: datagram,
                });
            }
        }
        out
    }

    /// See [`T38Engine::next_wakeup_ms`]; also wakes up for the reorder timeout.
    pub fn next_wakeup_ms(&self) -> Option<u64> {
        let hole = self.held_since.map(|s| s + self.reorder_wait_ms);
        [self.engine.next_wakeup_ms(), hole]
            .into_iter()
            .flatten()
            .min()
    }

    /// True once the call is over.
    pub fn is_done(&self) -> bool {
        self.engine.is_done()
    }
}
