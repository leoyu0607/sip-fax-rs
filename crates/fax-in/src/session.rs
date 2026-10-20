//! The fax session seam: once T.38 is established the controller hands the call to a
//! [`FaxSession`]. M1d ships a stub that only holds the call; M2 plugs in T.38/T.30.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use tokio::sync::watch;

use crate::event::T38Negotiated;

/// What the session knows about the call.
#[derive(Debug, Clone)]
pub struct SessionContext {
    /// SIP Call-ID.
    pub call_id: String,
    /// Trunk name.
    pub trunk: String,
    /// The T.38 parameters in force.
    pub t38: T38Negotiated,
    /// Turns `true` when the call ends underneath the session (peer BYE, shutdown).
    pub cancel: watch::Receiver<bool>,
}

/// How a session ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SessionOutcome {
    /// The transfer finished; the controller sends BYE.
    Completed,
    /// The call ended underneath the session (peer BYE / shutdown).
    Aborted,
    /// The stub reached its maximum duration; the controller sends BYE.
    MaxDuration,
    /// The session failed; the controller sends BYE.
    Failed(String),
}

/// A fax session running on an established T.38 call.
pub trait FaxSession: Send + 'static {
    /// Runs the session to completion. Must end promptly when `ctx.cancel` turns `true`.
    fn run(
        self: Box<Self>,
        ctx: SessionContext,
    ) -> Pin<Box<dyn Future<Output = SessionOutcome> + Send>>;
}

/// Creates one session per call.
pub trait FaxSessionFactory: Send + Sync + 'static {
    /// A new session for the call described by `ctx`.
    fn create(&self, ctx: &SessionContext) -> Box<dyn FaxSession>;
}

/// Resolves when the cancel flag turns `true` (or its sender is gone).
pub async fn cancelled(rx: &mut watch::Receiver<bool>) {
    let _ = rx.wait_for(|c| *c).await;
}

/// Holds the call until the peer hangs up or `max_duration` passes.
#[derive(Debug, Clone)]
pub struct StubFaxSession {
    /// Longest the call is held.
    pub max_duration: Duration,
}

impl FaxSession for StubFaxSession {
    fn run(
        self: Box<Self>,
        mut ctx: SessionContext,
    ) -> Pin<Box<dyn Future<Output = SessionOutcome> + Send>> {
        Box::pin(async move {
            tokio::select! {
                () = tokio::time::sleep(self.max_duration) => SessionOutcome::MaxDuration,
                () = cancelled(&mut ctx.cancel) => SessionOutcome::Aborted,
            }
        })
    }
}

/// Factory of [`StubFaxSession`]s.
#[derive(Debug, Clone)]
pub struct StubFactory {
    /// Duration handed to every stub session.
    pub max_duration: Duration,
}

impl FaxSessionFactory for StubFactory {
    fn create(&self, _ctx: &SessionContext) -> Box<dyn FaxSession> {
        Box::new(StubFaxSession {
            max_duration: self.max_duration,
        })
    }
}
