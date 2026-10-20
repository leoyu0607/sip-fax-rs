//! Runs one [`Machine`] inside a tokio task: executes its actions, keeps its timers on the
//! tokio clock (so `tokio::time::pause()` controls them) and forwards TU events.

use std::net::SocketAddr;

use tokio::sync::{mpsc, watch};
use tokio::time::Instant;

use super::machine::{Action, Machine, TuEvent};
use super::timers::Timer;
use crate::message::{Response, SipMessage};
use crate::transport::{TransportHandle, TransportKind, wait_shutdown};

/// Commands a transaction task accepts.
#[derive(Debug)]
pub(crate) enum Cmd {
    /// A message from the network (retransmitted request, ACK, or response).
    Message(SipMessage),
    /// A response from the TU (server transactions).
    Respond(Response),
    /// A matching CANCEL arrived (INVITE server transactions).
    Cancel,
}

/// How a transaction's messages leave the stack.
#[derive(Debug, Clone, Copy)]
pub(crate) enum Io {
    /// Client: requests go to `dest`.
    Client {
        dest: SocketAddr,
        kind: TransportKind,
    },
    /// Server: responses go back towards `source`.
    Server {
        source: SocketAddr,
        kind: TransportKind,
    },
}

#[derive(Default)]
struct TimerSet(Vec<(Timer, Instant)>);

impl TimerSet {
    fn start(&mut self, t: Timer, d: std::time::Duration) {
        self.cancel(t);
        self.0.push((t, Instant::now() + d));
    }

    fn cancel(&mut self, t: Timer) {
        self.0.retain(|(x, _)| *x != t);
    }

    /// The earliest timer; ties are resolved in start order.
    fn next(&self) -> Option<(Timer, Instant)> {
        let mut best: Option<(Timer, Instant)> = None;
        for &(t, d) in &self.0 {
            if best.is_none_or(|(_, bd)| d < bd) {
                best = Some((t, d));
            }
        }
        best
    }
}

struct Ctx {
    transport: TransportHandle,
    io: Io,
    events: Option<mpsc::UnboundedSender<TuEvent>>,
}

impl Ctx {
    fn notify(&self, e: TuEvent) {
        if let Some(tx) = &self.events {
            let _ = tx.send(e);
        }
    }

    /// Executes actions; returns `true` once the transaction is over.
    async fn exec(&self, actions: Vec<Action>, timers: &mut TimerSet) -> bool {
        for a in actions {
            match a {
                Action::Send(msg) => {
                    let sent = match (self.io, msg) {
                        (Io::Client { dest, kind }, SipMessage::Request(req)) => {
                            self.transport.send_request(req, dest, kind).await
                        }
                        (Io::Server { source, kind }, SipMessage::Response(resp)) => {
                            self.transport.send_response(resp, source, kind).await
                        }
                        _ => Ok(()),
                    };
                    if sent.is_err() {
                        self.notify(TuEvent::TransportError);
                        return true;
                    }
                }
                Action::Start(t, d) => timers.start(t, d),
                Action::Cancel(t) => timers.cancel(t),
                Action::Notify(e) => self.notify(e),
                Action::Terminate => return true,
            }
        }
        false
    }
}

/// Runs `machine` until it terminates, its command channel closes, or shutdown is requested.
pub(crate) async fn drive<M: Machine>(
    mut machine: M,
    initial: Vec<Action>,
    mut rx: mpsc::Receiver<Cmd>,
    transport: TransportHandle,
    io: Io,
    events: Option<mpsc::UnboundedSender<TuEvent>>,
    mut shutdown: watch::Receiver<bool>,
) {
    let ctx = Ctx {
        transport,
        io,
        events,
    };
    let mut timers = TimerSet::default();
    let mut done = ctx.exec(initial, &mut timers).await;
    while !done {
        let next = timers.next();
        let actions = tokio::select! {
            cmd = rx.recv() => match cmd {
                None => return,
                Some(Cmd::Message(m)) => machine.on_message(m),
                Some(Cmd::Respond(r)) => machine.on_tu_response(r),
                Some(Cmd::Cancel) => machine.on_cancel(),
            },
            () = tokio::time::sleep_until(next.map_or_else(Instant::now, |n| n.1)), if next.is_some() => {
                match next {
                    Some((t, _)) => {
                        timers.cancel(t);
                        machine.on_timer(t)
                    }
                    None => Vec::new(),
                }
            }
            () = wait_shutdown(&mut shutdown) => return,
        };
        done = ctx.exec(actions, &mut timers).await;
    }
}
