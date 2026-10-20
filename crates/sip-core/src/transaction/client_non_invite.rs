//! non-INVITE client transaction (RFC 3261 17.1.2).

use std::time::Duration;

use super::machine::{Action, Machine, State, TuEvent, start_or_terminate};
use super::timers::{Timer, TimerConfig};
use crate::message::{Request, SipMessage};

/// non-INVITE client transaction machine.
#[derive(Debug)]
pub struct ClientNonInvite {
    cfg: TimerConfig,
    reliable: bool,
    request: Request,
    state: State,
    e: Duration,
}

impl ClientNonInvite {
    /// Creates the machine. The returned actions send the request and start Timers E (UDP
    /// only) and F.
    pub fn new(request: Request, reliable: bool, cfg: TimerConfig) -> (Self, Vec<Action>) {
        let mut out = vec![Action::Send(SipMessage::Request(request.clone()))];
        if !reliable {
            out.push(Action::Start(Timer::E, cfg.t1));
        }
        out.push(Action::Start(Timer::F, cfg.t64()));
        let m = ClientNonInvite {
            cfg,
            reliable,
            request,
            state: State::Trying,
            e: cfg.t1,
        };
        (m, out)
    }
}

impl Machine for ClientNonInvite {
    fn state(&self) -> State {
        self.state
    }

    fn on_message(&mut self, msg: SipMessage) -> Vec<Action> {
        let SipMessage::Response(resp) = msg else {
            return Vec::new();
        };
        let mut out = Vec::new();
        match self.state {
            State::Trying | State::Proceeding => {
                if resp.status.is_provisional() {
                    // Timer E keeps running; it is re-armed with T2 when it fires.
                    self.state = State::Proceeding;
                    out.push(Action::Notify(TuEvent::Response(resp)));
                } else {
                    self.state = State::Completed;
                    out.push(Action::Cancel(Timer::E));
                    out.push(Action::Cancel(Timer::F));
                    out.push(Action::Notify(TuEvent::Response(resp)));
                    let d = if self.reliable {
                        Duration::ZERO
                    } else {
                        self.cfg.t4
                    };
                    start_or_terminate(&mut out, &mut self.state, Timer::K, d);
                }
            }
            _ => {} // Completed: retransmitted responses are absorbed
        }
        out
    }

    fn on_timer(&mut self, timer: Timer) -> Vec<Action> {
        let mut out = Vec::new();
        match (timer, self.state) {
            (Timer::E, State::Trying) => {
                out.push(Action::Send(SipMessage::Request(self.request.clone())));
                self.e = self.cfg.double_capped(self.e);
                out.push(Action::Start(Timer::E, self.e));
            }
            (Timer::E, State::Proceeding) => {
                out.push(Action::Send(SipMessage::Request(self.request.clone())));
                out.push(Action::Start(Timer::E, self.cfg.t2));
            }
            (Timer::F, State::Trying | State::Proceeding) => {
                self.state = State::Terminated;
                out.push(Action::Cancel(Timer::E));
                out.push(Action::Notify(TuEvent::Timeout));
                out.push(Action::Terminate);
            }
            (Timer::K, State::Completed) => {
                self.state = State::Terminated;
                out.push(Action::Terminate);
            }
            _ => {}
        }
        out
    }
}
