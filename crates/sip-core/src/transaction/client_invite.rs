//! INVITE client transaction (RFC 3261 17.1.1 with the RFC 6026 Accepted state).

use std::time::Duration;

use super::machine::{Action, Machine, State, TuEvent, start_or_terminate};
use super::timers::{Timer, TimerConfig};
use crate::message::{Method, Request, Response, SipMessage};

/// INVITE client transaction machine.
#[derive(Debug)]
pub struct ClientInvite {
    cfg: TimerConfig,
    reliable: bool,
    request: Request,
    state: State,
    a: Duration,
    /// The ACK generated for a non-2xx final response (resent on retransmitted responses).
    ack: Option<Request>,
}

impl ClientInvite {
    /// Creates the machine. The returned actions send the INVITE and start Timers A (UDP
    /// only) and B.
    pub fn new(request: Request, reliable: bool, cfg: TimerConfig) -> (Self, Vec<Action>) {
        let mut out = vec![Action::Send(SipMessage::Request(request.clone()))];
        if !reliable {
            out.push(Action::Start(Timer::A, cfg.t1));
        }
        out.push(Action::Start(Timer::B, cfg.t64()));
        let m = ClientInvite {
            cfg,
            reliable,
            request,
            state: State::Calling,
            a: cfg.t1,
            ack: None,
        };
        (m, out)
    }

    /// The ACK sent for a non-2xx final response, once there is one.
    pub fn ack(&self) -> Option<&Request> {
        self.ack.as_ref()
    }
}

/// Builds the ACK for a non-2xx final response (RFC 3261 17.1.1.3): same Request-URI, Call-ID,
/// From, CSeq number and the single top Via of the INVITE, To (with tag) from the response,
/// and the INVITE's Route headers.
pub fn build_non_2xx_ack(invite: &Request, resp: &Response) -> Request {
    let mut ack = Request::new(Method::Ack, invite.uri.clone());
    if let Some(v) = invite.headers.get("Via") {
        ack.headers.push("Via", v);
    }
    ack.headers.push(
        "Max-Forwards",
        invite.headers.get("Max-Forwards").unwrap_or("70"),
    );
    if let Some(v) = invite.headers.get("From") {
        ack.headers.push("From", v);
    }
    ack.headers.push(
        "To",
        resp.headers
            .get("To")
            .or(invite.headers.get("To"))
            .unwrap_or(""),
    );
    if let Some(v) = invite.headers.get("Call-ID") {
        ack.headers.push("Call-ID", v);
    }
    if let Ok(cseq) = invite.headers.cseq() {
        ack.headers.push("CSeq", format!("{} ACK", cseq.seq));
    }
    for r in invite.headers.get_all("Route") {
        ack.headers.push("Route", r);
    }
    ack
}

impl Machine for ClientInvite {
    fn state(&self) -> State {
        self.state
    }

    fn on_message(&mut self, msg: SipMessage) -> Vec<Action> {
        let SipMessage::Response(resp) = msg else {
            return Vec::new();
        };
        let mut out = Vec::new();
        match self.state {
            State::Calling | State::Proceeding => {
                if resp.status.is_provisional() {
                    self.state = State::Proceeding;
                    out.push(Action::Cancel(Timer::A));
                    out.push(Action::Cancel(Timer::B));
                    out.push(Action::Notify(TuEvent::Response(resp)));
                } else if resp.status.is_success() {
                    self.state = State::Accepted;
                    out.push(Action::Cancel(Timer::A));
                    out.push(Action::Cancel(Timer::B));
                    out.push(Action::Notify(TuEvent::Response(resp)));
                    out.push(Action::Start(Timer::M, self.cfg.t64()));
                } else {
                    self.state = State::Completed;
                    out.push(Action::Cancel(Timer::A));
                    out.push(Action::Cancel(Timer::B));
                    let ack = build_non_2xx_ack(&self.request, &resp);
                    self.ack = Some(ack.clone());
                    out.push(Action::Send(SipMessage::Request(ack)));
                    out.push(Action::Notify(TuEvent::Response(resp)));
                    let d = if self.reliable {
                        Duration::ZERO
                    } else {
                        self.cfg.timer_d
                    };
                    start_or_terminate(&mut out, &mut self.state, Timer::D, d);
                }
            }
            State::Completed if resp.status.is_final() && !resp.status.is_success() => {
                if let Some(ack) = &self.ack {
                    out.push(Action::Send(SipMessage::Request(ack.clone())));
                }
            }
            // Accepted (RFC 6026): a retransmitted 2xx means our ACK was lost; the TU owns the
            // ACK, so it is told (distinctly from the first 2xx) and resends it.
            State::Accepted if resp.status.is_success() => {
                out.push(Action::Notify(TuEvent::Retransmitted2xx(resp)));
            }
            _ => {}
        }
        out
    }

    fn on_timer(&mut self, timer: Timer) -> Vec<Action> {
        let mut out = Vec::new();
        match (timer, self.state) {
            (Timer::A, State::Calling) => {
                out.push(Action::Send(SipMessage::Request(self.request.clone())));
                self.a *= 2; // Timer A is not capped by T2 for INVITE
                out.push(Action::Start(Timer::A, self.a));
            }
            (Timer::B, State::Calling) => {
                self.state = State::Terminated;
                out.push(Action::Cancel(Timer::A));
                out.push(Action::Notify(TuEvent::Timeout));
                out.push(Action::Terminate);
            }
            (Timer::D, State::Completed) | (Timer::M, State::Accepted) => {
                self.state = State::Terminated;
                out.push(Action::Terminate);
            }
            _ => {}
        }
        out
    }
}
