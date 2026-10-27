//! The events of `/api/v1/events` (spec 4): one sequence for everything, the last
//! [`RING`] events kept for clients that reconnect, and the account revocations that close a
//! user's connections (A08).
//!
//! Numbering, keeping and sending happen under one lock, so a connection that takes its
//! backlog and its live receiver together ([`EventBus::attach`]) sees every event once, in order.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use chrono::Utc;
use serde_json::{Value, json};
use tokio::sync::broadcast;
use tokio::task::JoinHandle;

use crate::state::{ServiceStatus, StatusSource};

/// Events kept for `since`.
pub const RING: usize = 1000;
/// Events a connection may be behind before it is closed as too slow.
pub const LAG: usize = 256;

/// A numbered event as it goes on the wire.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Envelope {
    /// The sequence number (from 1, per process).
    pub seq: u64,
    /// `{"seq", "type", "at", "data"}`.
    pub json: String,
}

/// What a connection starts with.
#[derive(Debug)]
pub enum Backlog {
    /// The events after `since` (empty without `since`).
    Events(Vec<Arc<Envelope>>),
    /// `since` is older than the ring or newer than the last event (the server restarted): the
    /// client has to read the lists again.
    Resync,
}

struct Ring {
    last: u64,
    buf: VecDeque<Arc<Envelope>>,
}

/// The event hub. One per server; cheap to share.
pub struct EventBus {
    ring: Mutex<Ring>,
    tx: broadcast::Sender<Arc<Envelope>>,
    revoked: broadcast::Sender<i64>,
}

impl Default for EventBus {
    fn default() -> Self {
        EventBus::new()
    }
}

impl EventBus {
    /// An empty hub.
    pub fn new() -> EventBus {
        EventBus {
            ring: Mutex::new(Ring {
                last: 0,
                buf: VecDeque::with_capacity(RING),
            }),
            tx: broadcast::channel(LAG).0,
            revoked: broadcast::channel(64).0,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Ring> {
        self.ring.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Numbers, keeps and sends an event; returns its number. Never blocks on a client.
    pub fn publish(&self, kind: &str, data: Value) -> u64 {
        let mut r = self.lock();
        r.last += 1;
        let seq = r.last;
        let e = Arc::new(Envelope {
            seq,
            json: json!({"seq": seq, "type": kind, "at": Utc::now(), "data": data}).to_string(),
        });
        if r.buf.len() == RING {
            r.buf.pop_front();
        }
        r.buf.push_back(e.clone());
        // nobody connected: nothing to do
        let _ = self.tx.send(e);
        seq
    }

    /// The number of the last event (0: none yet).
    pub fn last_seq(&self) -> u64 {
        self.lock().last
    }

    /// The live receiver, what comes before it (the events after `since`, or
    /// [`Backlog::Resync`] when they are not all kept any more) and the last number then.
    pub fn attach(&self, since: Option<u64>) -> (broadcast::Receiver<Arc<Envelope>>, Backlog, u64) {
        let r = self.lock();
        let rx = self.tx.subscribe();
        let backlog = match since {
            None => Backlog::Events(Vec::new()),
            Some(s) if s > r.last => Backlog::Resync,
            Some(s) if s == r.last => Backlog::Events(Vec::new()),
            Some(s) => match r.buf.front() {
                Some(first) if first.seq <= s + 1 => {
                    Backlog::Events(r.buf.iter().filter(|e| e.seq > s).cloned().collect())
                }
                _ => Backlog::Resync,
            },
        };
        (rx, backlog, r.last)
    }

    /// Closes every connection of account `user_id` (logout, password change, disabled,
    /// demoted, deleted).
    pub fn revoke_user(&self, user_id: i64) {
        let _ = self.revoked.send(user_id);
    }

    /// The accounts whose connections are to be closed.
    pub fn revocations(&self) -> broadcast::Receiver<i64> {
        self.revoked.subscribe()
    }

    // ---------------------------------------------------------------- the fax events

    /// `fax.created`: a call was answered and its row (`receiving`) is in the database.
    pub fn fax_created(&self, id: i64, call_id: &str) {
        self.publish("fax.created", json!({"id": id, "call_id": call_id}));
    }

    /// `fax.progress`: another page was confirmed.
    pub fn fax_progress(&self, id: i64, call_id: &str, pages: u32) {
        self.publish(
            "fax.progress",
            json!({"id": id, "call_id": call_id, "pages": pages}),
        );
    }

    /// `fax.finalized`: the call ended and the row holds the result.
    pub fn fax_finalized(&self, id: i64, call_id: &str) {
        self.publish("fax.finalized", json!({"id": id, "call_id": call_id}));
    }

    /// `delivery.updated`: the delivery attempts of fax `fax_id` changed.
    pub fn delivery_updated(&self, fax_id: i64) {
        self.publish("delivery.updated", json!({"fax_id": fax_id}));
    }
}

/// What `status.changed` carries: the service without its clock.
fn status_value(s: &ServiceStatus) -> Value {
    json!({
        "state": s.state,
        "active_calls": s.active_calls,
        "trunks": s.trunks,
    })
}

/// Publishes `status.changed` whenever the calls in progress, the registrations or the state
/// differ from what was last seen; looks every `every`. `pending` gives the settings waiting
/// for a restart (the configuration service's list).
pub fn watch_status(
    bus: Arc<EventBus>,
    source: Arc<dyn StatusSource>,
    pending: impl Fn() -> Vec<String> + Send + 'static,
    every: Duration,
) -> JoinHandle<()> {
    tokio::spawn(async move {
        let mut seen: Option<Value> = None;
        let mut tick = tokio::time::interval(every);
        tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tick.tick().await;
            let mut now = status_value(&source.status());
            now["pending_restart"] = json!(pending());
            if seen.as_ref() != Some(&now) {
                // the first look is the starting point, not a change
                if seen.is_some() {
                    bus.publish("status.changed", now.clone());
                }
                seen = Some(now);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn seqs(b: &Backlog) -> Vec<u64> {
        match b {
            Backlog::Events(v) => v.iter().map(|e| e.seq).collect(),
            Backlog::Resync => panic!("resync"),
        }
    }

    #[test]
    fn since_replays_what_followed_and_resyncs_outside_the_ring() {
        let bus = EventBus::new();
        assert!(matches!(bus.attach(Some(0)).1, Backlog::Events(v) if v.is_empty()));
        for i in 0..(RING as u64 + 10) {
            bus.publish("t", json!(i));
        }
        let last = bus.last_seq();
        assert_eq!(last, RING as u64 + 10);
        assert_eq!(
            seqs(&bus.attach(Some(last - 3)).1),
            [last - 2, last - 1, last]
        );
        assert!(seqs(&bus.attach(Some(last)).1).is_empty());
        assert!(seqs(&bus.attach(None).1).is_empty());
        // the oldest kept is last - RING + 1: since = last - RING still fits, one less does not
        assert_eq!(seqs(&bus.attach(Some(last - RING as u64)).1).len(), RING);
        assert!(matches!(
            bus.attach(Some(last - RING as u64 - 1)).1,
            Backlog::Resync
        ));
        // a client of an earlier process
        assert!(matches!(bus.attach(Some(last + 1)).1, Backlog::Resync));
    }

    #[test]
    fn the_live_receiver_continues_the_backlog_without_gaps() {
        let bus = EventBus::new();
        bus.publish("a", json!(1));
        let (mut rx, b, last) = bus.attach(Some(0));
        assert_eq!(last, 1);
        assert_eq!(seqs(&b), [1]);
        bus.publish("b", json!(2));
        let e = rx.try_recv().unwrap();
        assert_eq!(e.seq, 2);
        let v: Value = serde_json::from_str(&e.json).unwrap();
        assert_eq!(
            (v["seq"].as_u64(), v["type"].as_str()),
            (Some(2), Some("b"))
        );
        assert_eq!(v["data"], 2);
        assert!(v["at"].is_string());
    }

    #[tokio::test(start_paused = true)]
    async fn status_changes_are_published_once() {
        struct S(Mutex<ServiceStatus>);
        impl StatusSource for S {
            fn status(&self) -> ServiceStatus {
                self.0.lock().unwrap().clone()
            }
        }
        let s = Arc::new(S(Mutex::new(ServiceStatus {
            version: "v".into(),
            state: "running".into(),
            started_at: "t".into(),
            uptime_secs: 0,
            listen: vec![],
            active_calls: 0,
            trunks: vec![],
            advertised_missing: false,
        })));
        let bus = Arc::new(EventBus::new());
        let (mut rx, _, _) = bus.attach(None);
        let task = watch_status(bus.clone(), s.clone(), Vec::new, Duration::from_secs(1));
        tokio::time::sleep(Duration::from_millis(2500)).await;
        // the uptime moves, nothing that counts does
        s.0.lock().unwrap().uptime_secs = 99;
        tokio::time::sleep(Duration::from_secs(2)).await;
        assert!(rx.try_recv().is_err());
        s.0.lock().unwrap().active_calls = 1;
        tokio::time::sleep(Duration::from_secs(2)).await;
        let e = rx.try_recv().unwrap();
        assert!(e.json.contains("\"status.changed\""), "{}", e.json);
        assert!(e.json.contains("\"active_calls\":1"), "{}", e.json);
        assert!(rx.try_recv().is_err(), "once");
        task.abort();
    }
}
