//! Common vocabulary of the pure transaction state machines.
//!
//! A machine is a plain struct. It is driven by calling `on_message`, `on_timer` and (server
//! side) `on_tu_response`; each call returns the [`Action`]s the caller has to perform. There
//! is no I/O and no clock inside a machine: timers are requested with [`Action::Start`] and
//! the caller reports expiry with `on_timer`. The same machines are used by the tokio driver
//! (`transaction::TransactionLayer`) and by the pure tests.

use std::time::Duration;

use super::timers::Timer;
use crate::message::{Request, Response, SipMessage};

/// Something the machine wants done.
#[derive(Debug, Clone, PartialEq)]
pub enum Action {
    /// Hand a message to the transport (a request for client transactions, a response or ACK
    /// as appropriate).
    Send(SipMessage),
    /// (Re)start a timer; an already running timer of the same kind is replaced.
    Start(Timer, Duration),
    /// Stop a timer (no-op when it is not running).
    Cancel(Timer),
    /// Tell the transaction user something.
    Notify(TuEvent),
    /// The transaction is finished; no further events will be processed.
    Terminate,
}

/// Events delivered to the transaction user (TU).
#[derive(Debug, Clone, PartialEq)]
pub enum TuEvent {
    /// Client: a response (provisional or final). For INVITE, a 2xx is reported exactly once;
    /// retransmissions are absorbed (RFC 6026 Accepted state).
    Response(Response),
    /// Server INVITE in Accepted state: an ACK arrived on this transaction.
    Ack(Request),
    /// Server INVITE: a matching CANCEL was received. The transaction itself has already
    /// answered the INVITE with 487 when no final response had been sent.
    Cancelled,
    /// Client: Timer B / F fired without a final response. Server INVITE: Timer H fired
    /// without an ACK.
    Timeout,
    /// The transport could not send a message; the transaction ends.
    TransportError,
}

/// State names shared by the four machines (RFC 3261 figures 5-8, RFC 6026).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum State {
    /// INVITE client: request sent, nothing received.
    Calling,
    /// non-INVITE: request sent / received, no response yet.
    Trying,
    /// A provisional response was sent / received.
    Proceeding,
    /// A final (non-2xx for INVITE) response was sent / received.
    Completed,
    /// INVITE server: ACK received for a non-2xx.
    Confirmed,
    /// INVITE: a 2xx was sent / received (RFC 6026).
    Accepted,
    /// Finished.
    Terminated,
}

/// Returns `Start(timer, d)`, or - when `d` is zero (reliable transports) - terminates the
/// machine right away. Used for the timers that are 0 on TCP (D, I, J, K).
pub(crate) fn start_or_terminate(
    out: &mut Vec<Action>,
    state: &mut State,
    timer: Timer,
    d: Duration,
) {
    if d.is_zero() {
        *state = State::Terminated;
        out.push(Action::Terminate);
    } else {
        out.push(Action::Start(timer, d));
    }
}

/// Behaviour common to all four machines, so one driver can run any of them.
pub trait Machine: Send + 'static {
    /// A message from the network: the retransmitted request (server machines) or a response
    /// (client machines). ACKs for INVITE server transactions also arrive here.
    fn on_message(&mut self, msg: SipMessage) -> Vec<Action>;
    /// A timer expired.
    fn on_timer(&mut self, timer: Timer) -> Vec<Action>;
    /// The TU supplies a response (server machines only; ignored by clients).
    fn on_tu_response(&mut self, _resp: Response) -> Vec<Action> {
        Vec::new()
    }
    /// A matching CANCEL arrived (INVITE server machine only).
    fn on_cancel(&mut self) -> Vec<Action> {
        Vec::new()
    }
    /// Current state.
    fn state(&self) -> State;
}
