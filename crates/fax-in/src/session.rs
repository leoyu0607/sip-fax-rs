//! The fax session seam: once T.38 is established the controller hands the call to a
//! [`FaxSession`]. The stub only holds the call; [`crate::t38session`] is the real T.38 / T.30 session.

use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

use tokio::sync::watch;

use crate::event::{ReportSink, T38Negotiated};

/// The UDPTL error recovery we send, from the trunk's `[trunks.t38]` settings (the scheme in
/// force comes from [`T38Negotiated::udp_ec`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UdptlSettings {
    /// Redundancy depth for `t38UDPRedundancy`.
    pub redundancy_depth: u8,
    /// FEC span for `t38UDPFEC`.
    pub fec_span: u8,
    /// FEC entries for `t38UDPFEC`.
    pub fec_entries: u8,
}

impl Default for UdptlSettings {
    fn default() -> Self {
        UdptlSettings {
            redundancy_depth: 3,
            fec_span: 3,
            fec_entries: 3,
        }
    }
}

/// What the session knows about the call.
#[derive(Debug, Clone)]
pub struct SessionContext {
    /// SIP Call-ID.
    pub call_id: String,
    /// Trunk name.
    pub trunk: String,
    /// The T.38 parameters in force.
    pub t38: T38Negotiated,
    /// Error recovery settings of the trunk.
    pub udptl: UdptlSettings,
    /// Fax capabilities of the trunk (`[trunks.fax]`): resolutions and ECM.
    pub fax: crate::config::FaxSection,
    /// The session leaves its fax report here before it returns.
    pub report: ReportSink,
    /// The session sends the page count here every time T.30 confirms a page (MCF / RTP), ECM
    /// and non-ECM alike. The controller turns it into `CallEvent::PageReceived`. `None`: nobody
    /// listens.
    pub progress: Option<tokio::sync::mpsc::UnboundedSender<u32>>,
    /// Turns `true` when the call ends underneath the session (peer BYE, shutdown).
    pub cancel: watch::Receiver<bool>,
    /// Our station id (CSI) from the configuration snapshot of this call. `None`: the factory's.
    pub station_id: Option<String>,
    /// Longest the session may run, from the configuration snapshot of this call. `None`: the
    /// factory's.
    pub max_duration: Option<Duration>,
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
            let max = ctx.max_duration.unwrap_or(self.max_duration);
            tokio::select! {
                () = tokio::time::sleep(max) => SessionOutcome::MaxDuration,
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
