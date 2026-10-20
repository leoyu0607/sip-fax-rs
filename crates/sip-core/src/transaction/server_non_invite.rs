//! non-INVITE server transaction (RFC 3261 17.2.2).

use super::machine::{Action, Machine, State, start_or_terminate};
use super::timers::{Timer, TimerConfig};
use crate::message::{Request, Response, SipMessage};

/// non-INVITE server transaction machine.
#[derive(Debug)]
pub struct ServerNonInvite {
    cfg: TimerConfig,
    reliable: bool,
    request: Request,
    state: State,
    last: Option<Response>,
}

impl ServerNonInvite {
    /// Creates the machine (state Trying) for a freshly received request.
    pub fn new(request: Request, reliable: bool, cfg: TimerConfig) -> Self {
        ServerNonInvite {
            cfg,
            reliable,
            request,
            state: State::Trying,
            last: None,
        }
    }

    /// The request this transaction was created for.
    pub fn request(&self) -> &Request {
        &self.request
    }
}

impl Machine for ServerNonInvite {
    fn state(&self) -> State {
        self.state
    }

    fn on_message(&mut self, msg: SipMessage) -> Vec<Action> {
        // Only a retransmitted request can arrive here.
        if !matches!(msg, SipMessage::Request(_)) {
            return Vec::new();
        }
        match (self.state, &self.last) {
            (State::Proceeding | State::Completed, Some(last)) => {
                vec![Action::Send(SipMessage::Response(last.clone()))]
            }
            _ => Vec::new(), // Trying: nothing to resend yet
        }
    }

    fn on_tu_response(&mut self, resp: Response) -> Vec<Action> {
        let mut out = Vec::new();
        match self.state {
            State::Trying | State::Proceeding => {
                self.last = Some(resp.clone());
                let provisional = resp.status.is_provisional();
                out.push(Action::Send(SipMessage::Response(resp)));
                if provisional {
                    self.state = State::Proceeding;
                } else {
                    self.state = State::Completed;
                    let d = if self.reliable {
                        std::time::Duration::ZERO
                    } else {
                        self.cfg.t64()
                    };
                    start_or_terminate(&mut out, &mut self.state, Timer::J, d);
                }
            }
            _ => {}
        }
        out
    }

    fn on_timer(&mut self, timer: Timer) -> Vec<Action> {
        if timer == Timer::J && self.state == State::Completed {
            self.state = State::Terminated;
            return vec![Action::Terminate];
        }
        Vec::new()
    }
}
