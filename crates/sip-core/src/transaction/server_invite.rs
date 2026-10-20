//! INVITE server transaction (RFC 3261 17.2.1 with the RFC 6026 Accepted state).

use std::time::Duration;

use super::machine::{Action, Machine, State, TuEvent, start_or_terminate};
use super::timers::{Timer, TimerConfig};
use crate::message::{Method, Request, Response, SipMessage, StatusCode};

/// INVITE server transaction machine.
#[derive(Debug)]
pub struct ServerInvite {
    cfg: TimerConfig,
    reliable: bool,
    request: Request,
    state: State,
    /// Last response passed to the transport (provisional or final): what a retransmitted
    /// INVITE gets back.
    last: Option<Response>,
    g: Duration,
}

impl ServerInvite {
    /// Creates the machine for a freshly received INVITE. The returned actions start the
    /// automatic-100 timer.
    pub fn new(request: Request, reliable: bool, cfg: TimerConfig) -> (Self, Vec<Action>) {
        let m = ServerInvite {
            cfg,
            reliable,
            request,
            state: State::Proceeding,
            last: None,
            g: cfg.t1,
        };
        let a = vec![Action::Start(Timer::Trying, cfg.provisional_delay)];
        (m, a)
    }

    /// The INVITE this transaction was created for.
    pub fn request(&self) -> &Request {
        &self.request
    }

    /// The last response sent.
    pub fn last_response(&self) -> Option<&Response> {
        self.last.as_ref()
    }

    fn to_tag_of_last(&self) -> Option<String> {
        self.last
            .as_ref()
            .and_then(|r| r.headers.to().ok())
            .and_then(|t| t.tag().map(str::to_string))
    }
}

impl Machine for ServerInvite {
    fn state(&self) -> State {
        self.state
    }

    fn on_message(&mut self, msg: SipMessage) -> Vec<Action> {
        let SipMessage::Request(req) = msg else {
            return Vec::new();
        };
        let mut out = Vec::new();
        match (req.method.clone(), self.state) {
            // Retransmitted INVITE.
            (Method::Invite, State::Proceeding | State::Completed) => {
                if let Some(last) = &self.last {
                    out.push(Action::Send(SipMessage::Response(last.clone())));
                }
            }
            // Accepted / Confirmed / Terminated: absorbed (the TU retransmits 2xx itself).
            (Method::Invite, _) => {}
            (Method::Ack, State::Completed) => {
                self.state = State::Confirmed;
                out.push(Action::Cancel(Timer::G));
                out.push(Action::Cancel(Timer::H));
                let d = if self.reliable {
                    std::time::Duration::ZERO
                } else {
                    self.cfg.t4
                };
                start_or_terminate(&mut out, &mut self.state, Timer::I, d);
            }
            (Method::Ack, State::Accepted) => {
                out.push(Action::Notify(TuEvent::Ack(req)));
            }
            _ => {}
        }
        out
    }

    fn on_tu_response(&mut self, resp: Response) -> Vec<Action> {
        let mut out = Vec::new();
        match self.state {
            State::Proceeding => {
                out.push(Action::Cancel(Timer::Trying));
                self.last = Some(resp.clone());
                out.push(Action::Send(SipMessage::Response(resp.clone())));
                if resp.status.is_provisional() {
                    return out;
                }
                if resp.status.is_success() {
                    self.state = State::Accepted;
                    out.push(Action::Start(Timer::L, self.cfg.t64()));
                } else {
                    self.state = State::Completed;
                    if !self.reliable {
                        self.g = self.cfg.t1;
                        out.push(Action::Start(Timer::G, self.g));
                    }
                    out.push(Action::Start(Timer::H, self.cfg.t64()));
                }
            }
            State::Accepted if resp.status.is_success() => {
                // Further 2xx (e.g. forked or TU-retransmitted): pass on.
                self.last = Some(resp.clone());
                out.push(Action::Send(SipMessage::Response(resp)));
            }
            _ => {}
        }
        out
    }

    fn on_timer(&mut self, timer: Timer) -> Vec<Action> {
        let mut out = Vec::new();
        match (timer, self.state) {
            (Timer::Trying, State::Proceeding) if self.last.is_none() => {
                let trying = Response::for_request(&self.request, StatusCode::TRYING, None);
                self.last = Some(trying.clone());
                out.push(Action::Send(SipMessage::Response(trying)));
            }
            (Timer::G, State::Completed) => {
                if let Some(last) = &self.last {
                    out.push(Action::Send(SipMessage::Response(last.clone())));
                }
                self.g = self.cfg.double_capped(self.g);
                out.push(Action::Start(Timer::G, self.g));
            }
            (Timer::H, State::Completed) => {
                self.state = State::Terminated;
                out.push(Action::Cancel(Timer::G));
                out.push(Action::Notify(TuEvent::Timeout));
                out.push(Action::Terminate);
            }
            (Timer::I, State::Confirmed) | (Timer::L, State::Accepted) => {
                self.state = State::Terminated;
                out.push(Action::Terminate);
            }
            _ => {}
        }
        out
    }

    fn on_cancel(&mut self) -> Vec<Action> {
        if self.state != State::Proceeding {
            return Vec::new();
        }
        let tag = self
            .to_tag_of_last()
            .unwrap_or_else(|| format!("{:08x}", rand::random::<u32>()));
        let resp = Response::for_request(&self.request, StatusCode::REQUEST_TERMINATED, Some(&tag));
        let mut out = vec![Action::Notify(TuEvent::Cancelled)];
        out.extend(self.on_tu_response(resp));
        out
    }
}
