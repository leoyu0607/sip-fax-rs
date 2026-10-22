//! The service's side of the control interface: the state that `status` / `calls` /
//! `subscribe` report, the stop request, and the request loop of one connection. Nothing here
//! touches a socket: [`Hub::serve_connection`] works on any async reader / writer, so it runs
//! (and is tested) on every platform; `control.rs` puts a unix socket under it.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use async_trait::async_trait;
use fax_in::{CallEvent, classify, rfc3339};
use tokio::io::{AsyncBufReadExt, AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, BufReader};
use tokio::sync::{broadcast, watch};

use super::protocol::{CallInfo, Event, Reply, Request, StatusInfo, TrunkStatus};

/// Loads the runtime settings from the database again (`reload`); `serve` connects
/// [`crate::runtime::ConfigReloader`]. Without one the control interface answers "not available".
#[async_trait]
pub trait Reloader: Send + Sync {
    /// Reloads and says what changed.
    async fn reload(&self) -> Result<String, String>;
}

/// What never changes while the process runs.
#[derive(Debug, Clone)]
pub struct HubInfo {
    /// Process id.
    pub pid: u32,
    /// Version string.
    pub version: String,
    /// The SIP listeners.
    pub listen: Vec<String>,
}

/// A request to stop, as the first client (or signal) made it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct StopRequest {
    /// End the calls at once.
    pub force: bool,
    /// How long the calls are waited for.
    pub timeout: Duration,
}

/// How far the stop is.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// Not stopping.
    Running,
    /// Waiting for the calls.
    Draining {
        /// Calls still running.
        active: usize,
        /// Seconds until they are ended.
        remaining_secs: u64,
    },
    /// Everything is shut down.
    Stopped,
}

type TrunkFn = Box<dyn Fn() -> Vec<TrunkStatus> + Send + Sync>;

/// Shared state of the control interface.
pub struct Hub {
    info: HubInfo,
    started: Instant,
    started_at: SystemTime,
    default_timeout: Duration,
    calls: Mutex<BTreeMap<String, CallInfo>>,
    events: broadcast::Sender<Event>,
    trunks: TrunkFn,
    reloader: Mutex<Option<Arc<dyn Reloader>>>,
    stop: watch::Sender<Option<StopRequest>>,
    force: watch::Sender<bool>,
    progress: watch::Sender<Progress>,
}

const MAX_REQUEST_BYTES: u64 = 64 * 1024;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(10);

impl Hub {
    /// A hub for a service that was just started. `default_timeout` is `[service]
    /// stop_timeout_secs`.
    pub fn new(
        info: HubInfo,
        default_timeout: Duration,
        trunks: impl Fn() -> Vec<TrunkStatus> + Send + Sync + 'static,
    ) -> Arc<Hub> {
        Arc::new(Hub {
            info,
            started: Instant::now(),
            started_at: SystemTime::now(),
            default_timeout,
            calls: Mutex::new(BTreeMap::new()),
            events: broadcast::channel(256).0,
            trunks: Box::new(trunks),
            reloader: Mutex::new(None),
            stop: watch::channel(None).0,
            force: watch::channel(false).0,
            progress: watch::channel(Progress::Running).0,
        })
    }

    /// Connects `reload` to something that can do it. Called by the runtime-settings code of M4a
    /// (nothing in this crate does yet).
    #[allow(dead_code)]
    pub fn set_reloader(&self, r: Arc<dyn Reloader>) {
        *self.reloader.lock().unwrap_or_else(PoisonError::into_inner) = Some(r);
    }

    fn calls_lock(&self) -> std::sync::MutexGuard<'_, BTreeMap<String, CallInfo>> {
        self.calls.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn emit(&self, e: Event) {
        // nobody subscribed: nothing to do
        let _ = self.events.send(e);
    }

    /// Feeds one call event (every event of `FaxIn`, in order).
    pub fn on_event(&self, ev: &CallEvent) {
        match ev {
            CallEvent::Started {
                call_id,
                trunk,
                peer,
                numbers,
                at,
            } => {
                let call = CallInfo {
                    call_id: call_id.clone(),
                    trunk: trunk.clone(),
                    peer: peer.clone(),
                    ani: numbers.ani.clone().unwrap_or_default(),
                    dnis: numbers.dnis.clone().unwrap_or_default(),
                    started_at: rfc3339(*at),
                    answered: false,
                    t38: false,
                    pages: 0,
                };
                self.calls_lock().insert(call_id.clone(), call.clone());
                self.emit(Event::CallStarted { call });
            }
            CallEvent::Answered { call_id, .. } => {
                if let Some(c) = self.calls_lock().get_mut(call_id) {
                    c.answered = true;
                }
                self.emit(Event::CallAnswered {
                    call_id: call_id.clone(),
                });
            }
            CallEvent::T38Established { call_id, .. } => {
                if let Some(c) = self.calls_lock().get_mut(call_id) {
                    c.t38 = true;
                }
                self.emit(Event::FaxStarted {
                    call_id: call_id.clone(),
                });
            }
            CallEvent::PageReceived { call_id, pages, .. } => {
                if let Some(c) = self.calls_lock().get_mut(call_id) {
                    c.pages = *pages;
                }
                self.emit(Event::FaxProgress {
                    call_id: call_id.clone(),
                    pages: *pages,
                });
            }
            CallEvent::Finished(r) => {
                self.calls_lock().remove(&r.call_id);
                let s = classify(r);
                self.emit(Event::CallFinished {
                    call_id: r.call_id.clone(),
                    status: if r.outcome.is_failure() {
                        "failed"
                    } else {
                        "completed"
                    }
                    .into(),
                    pages: u32::try_from(s.pages).unwrap_or(0),
                    fax_status: s.fax_status.as_str().into(),
                    hangup_code: s.hangup_code.as_str().into(),
                });
            }
        }
    }

    /// The calls in progress.
    pub fn calls(&self) -> Vec<CallInfo> {
        self.calls_lock().values().cloned().collect()
    }

    /// The state for `status`.
    pub fn status(&self) -> StatusInfo {
        let stopping = self.stop.borrow().is_some();
        StatusInfo {
            pid: self.info.pid,
            version: self.info.version.clone(),
            state: if stopping { "stopping" } else { "running" }.into(),
            started_at: rfc3339(self.started_at),
            uptime_secs: self.started.elapsed().as_secs(),
            listen: self.info.listen.clone(),
            active_calls: self.calls_lock().len(),
            trunks: (self.trunks)(),
        }
    }

    /// Asks for a stop. The first request decides the timeout; a later `force` upgrades the
    /// running wait. Returns what is in force now.
    pub fn request_stop(&self, force: bool, timeout: Option<Duration>) -> StopRequest {
        let wanted = StopRequest {
            force,
            timeout: if force {
                Duration::ZERO
            } else {
                timeout.unwrap_or(self.default_timeout)
            },
        };
        self.stop.send_if_modified(|s| {
            if s.is_none() {
                *s = Some(wanted);
                true
            } else {
                false
            }
        });
        if force {
            self.force.send_replace(true);
        }
        let mut now = self.stop.borrow().expect("set above");
        now.force |= force;
        now
    }

    /// Resolves with the first stop request.
    pub async fn stop_requested(&self) -> StopRequest {
        let mut rx = self.stop.subscribe();
        loop {
            if let Some(r) = *rx.borrow_and_update() {
                return r;
            }
            if rx.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    }

    /// Turns `true` when the wait for the calls must end now.
    pub fn force_receiver(&self) -> watch::Receiver<bool> {
        self.force.subscribe()
    }

    /// End the wait for the calls now (a second signal).
    pub fn force_now(&self) {
        self.force.send_replace(true);
    }

    /// Records how far the stop is; stop clients are told.
    pub fn set_progress(&self, p: Progress) {
        if let Progress::Draining { active, .. } = &p
            && *self.progress.borrow() == Progress::Running
        {
            self.emit(Event::Stopping { active: *active });
        }
        self.progress.send_replace(p);
    }

    /// Serves one client: reads request lines until it is done, answering each.
    pub async fn serve_connection<R, W>(self: &Arc<Self>, reader: R, mut writer: W)
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        loop {
            line.clear();
            let read = {
                let mut limited = (&mut reader).take(MAX_REQUEST_BYTES);
                tokio::time::timeout(REQUEST_TIMEOUT, limited.read_line(&mut line)).await
            };
            match read {
                Ok(Ok(0)) | Err(_) | Ok(Err(_)) => return,
                Ok(Ok(_)) => {}
            }
            if line.trim().is_empty() {
                continue;
            }
            let req: Request = match serde_json::from_str(line.trim()) {
                Ok(r) => r,
                Err(e) => {
                    let r = Reply::Error {
                        message: format!("bad request: {e}"),
                    };
                    if send(&mut writer, &r).await.is_err() {
                        return;
                    }
                    continue;
                }
            };
            let keep_open = match req {
                Request::Status => send(&mut writer, &Reply::Status(self.status())).await,
                Request::Calls => {
                    send(
                        &mut writer,
                        &Reply::Calls {
                            calls: self.calls(),
                        },
                    )
                    .await
                }
                Request::Reload => {
                    let r = self.reload().await;
                    send(&mut writer, &r).await
                }
                Request::Stop { force, timeout } => {
                    self.follow_stop(&mut writer, force, timeout.map(Duration::from_secs))
                        .await;
                    return;
                }
                Request::Subscribe => {
                    self.stream_events(&mut reader, &mut writer).await;
                    return;
                }
            };
            if keep_open.is_err() {
                return;
            }
        }
    }

    async fn reload(&self) -> Reply {
        let r = self
            .reloader
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone();
        match r {
            None => Reply::Error {
                message:
                    "reload is not available: the runtime settings are not in the database yet"
                        .into(),
            },
            Some(r) => match r.reload().await {
                Ok(message) => Reply::Reload { message },
                Err(message) => Reply::Error { message },
            },
        }
    }

    async fn follow_stop<W: AsyncWrite + Unpin>(
        &self,
        w: &mut W,
        force: bool,
        timeout: Option<Duration>,
    ) {
        let active = self.calls_lock().len();
        let req = self.request_stop(force, timeout);
        let first = Reply::Stopping {
            active,
            timeout_secs: req.timeout.as_secs(),
            force: req.force,
        };
        if send(w, &first).await.is_err() {
            return;
        }
        let mut rx = self.progress.subscribe();
        loop {
            let p = rx.borrow_and_update().clone();
            let reply = match p {
                Progress::Running => None,
                Progress::Draining {
                    active,
                    remaining_secs,
                } => Some(Reply::Waiting {
                    active,
                    remaining_secs,
                }),
                Progress::Stopped => {
                    let _ = send(w, &Reply::Stopped).await;
                    return;
                }
            };
            if let Some(r) = reply
                && send(w, &r).await.is_err()
            {
                return;
            }
            if rx.changed().await.is_err() {
                // the hub is gone: the process is exiting
                let _ = send(w, &Reply::Stopped).await;
                return;
            }
        }
    }

    async fn stream_events<R, W>(&self, reader: &mut BufReader<R>, w: &mut W)
    where
        R: AsyncRead + Unpin,
        W: AsyncWrite + Unpin,
    {
        // subscribe first, then snapshot: an event between the two shows up twice at worst
        let mut rx = self.events.subscribe();
        let first = Reply::Subscribed {
            calls: self.calls(),
        };
        if send(w, &first).await.is_err() {
            return;
        }
        let mut sink = String::new();
        loop {
            tokio::select! {
                ev = rx.recv() => match ev {
                    Ok(event) => {
                        if send(w, &Reply::Event { event }).await.is_err() {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(n)) => {
                        let r = Reply::Error { message: format!("{n} events were dropped (slow reader)") };
                        if send(w, &r).await.is_err() {
                            return;
                        }
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                },
                // the client closed the connection (or sent something we do not expect)
                n = reader.read_line(&mut sink) => {
                    if matches!(n, Ok(0) | Err(_)) {
                        return;
                    }
                    sink.clear();
                }
            }
        }
    }
}

async fn send<W: AsyncWrite + Unpin>(w: &mut W, r: &Reply) -> std::io::Result<()> {
    let mut line = r.to_line();
    line.push('\n');
    w.write_all(line.as_bytes()).await?;
    w.flush().await
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use fax_in::Numbers;
    use fax_in::event::{CallOutcome, CallResult, FailureReason, FaxReport, UdptlCounters};
    use tokio::io::{DuplexStream, duplex};

    fn hub() -> Arc<Hub> {
        Hub::new(
            HubInfo {
                pid: 4242,
                version: "9.9.9".into(),
                listen: vec!["udp 127.0.0.1:5060".into()],
            },
            Duration::from_secs(300),
            || {
                vec![TrunkStatus {
                    name: "gw".into(),
                    registration: "none".into(),
                }]
            },
        )
    }

    /// A client connected to the hub through an in-memory pipe.
    struct Client {
        w: DuplexStream,
        r: BufReader<DuplexStream>,
    }

    fn connect(h: &Arc<Hub>) -> Client {
        let (c2s_w, c2s_r) = duplex(1 << 16);
        let (s2c_w, s2c_r) = duplex(1 << 16);
        let h = h.clone();
        tokio::spawn(async move { h.serve_connection(c2s_r, s2c_w).await });
        Client {
            w: c2s_w,
            r: BufReader::new(s2c_r),
        }
    }

    impl Client {
        async fn send(&mut self, line: &str) {
            self.w
                .write_all(format!("{line}\n").as_bytes())
                .await
                .unwrap();
        }
        async fn reply(&mut self) -> Reply {
            let mut l = String::new();
            let n = tokio::time::timeout(Duration::from_secs(5), self.r.read_line(&mut l))
                .await
                .expect("a reply in time")
                .unwrap();
            assert!(n > 0, "the service closed the connection");
            serde_json::from_str(&l).unwrap_or_else(|e| panic!("{l:?}: {e}"))
        }
        async fn eof(&mut self) -> bool {
            let mut l = String::new();
            matches!(
                tokio::time::timeout(Duration::from_secs(5), self.r.read_line(&mut l)).await,
                Ok(Ok(0))
            )
        }
    }

    pub(crate) fn started(id: &str) -> CallEvent {
        CallEvent::Started {
            call_id: id.into(),
            trunk: "gw".into(),
            peer: "10.0.0.1:5060".into(),
            numbers: Numbers {
                ani: Some("0212345678".into()),
                dnis: Some("0287654321".into()),
                ..Numbers::default()
            },
            at: SystemTime::UNIX_EPOCH + Duration::from_secs(1_800_000_000),
        }
    }

    pub(crate) fn finished(id: &str, pages: u32, shutdown: bool) -> CallEvent {
        let fax = FaxReport {
            code: if shutdown { 11 } else { 0 },
            code_name: if shutdown { "aborted" } else { "ok" }.into(),
            pages,
            bit_rate: 14400,
            modem: None,
            resolution: None,
            compression: None,
            remote_ident: None,
            bad_lines_per_page: vec![],
            duration_ms: 1,
            partial: shutdown,
            tiff: None,
            pdf: None,
            store_error: None,
            udptl: UdptlCounters::default(),
            tiff_size: None,
            pdf_size: None,
            ecm: false,
            ecm_ppr_sent: 0,
            ecm_frames_lost: 0,
        };
        CallEvent::Finished(Box::new(CallResult {
            call_id: id.into(),
            trunk: "gw".into(),
            peer: "10.0.0.1:5060".into(),
            from: String::new(),
            to: String::new(),
            numbers: Numbers::default(),
            started_at: SystemTime::UNIX_EPOCH,
            answered_at: Some(SystemTime::UNIX_EPOCH),
            t38_established_at: None,
            ended_at: SystemTime::UNIX_EPOCH,
            bye_by: None,
            t38: None,
            fax: Some(fax),
            outcome: if shutdown {
                CallOutcome::Failed {
                    reason: FailureReason::Shutdown,
                }
            } else {
                CallOutcome::Completed
            },
        }))
    }

    #[tokio::test]
    async fn m07_status_and_calls_reflect_the_call_events() {
        let h = hub();
        let mut c = connect(&h);
        c.send(r#"{"cmd":"status"}"#).await;
        let Reply::Status(s) = c.reply().await else {
            panic!("status")
        };
        assert_eq!(
            (s.pid, s.version.as_str(), s.state.as_str()),
            (4242, "9.9.9", "running")
        );
        assert_eq!(s.listen, ["udp 127.0.0.1:5060"]);
        assert_eq!(s.active_calls, 0);
        assert_eq!(s.trunks[0].name, "gw");
        assert!(s.started_at.ends_with('Z'));

        h.on_event(&started("a"));
        h.on_event(&started("b"));
        h.on_event(&CallEvent::Answered {
            call_id: "a".into(),
            t38: true,
            at: SystemTime::now(),
        });
        h.on_event(&CallEvent::PageReceived {
            call_id: "a".into(),
            pages: 3,
            at: SystemTime::now(),
        });
        c.send(r#"{"cmd":"calls"}"#).await;
        let Reply::Calls { calls } = c.reply().await else {
            panic!("calls")
        };
        assert_eq!(calls.len(), 2);
        let a = calls.iter().find(|x| x.call_id == "a").unwrap();
        assert!(a.answered && !a.t38);
        assert_eq!(
            (a.pages, a.ani.as_str(), a.dnis.as_str()),
            (3, "0212345678", "0287654321")
        );
        assert_eq!(a.started_at, "2027-01-15T08:00:00.000Z");
        assert!(!calls.iter().find(|x| x.call_id == "b").unwrap().answered);

        h.on_event(&finished("a", 3, false));
        c.send(r#"{"cmd":"status"}"#).await;
        let Reply::Status(s) = c.reply().await else {
            panic!("status")
        };
        assert_eq!(s.active_calls, 1, "a is over, b is not");
    }

    #[tokio::test]
    async fn m07_bad_requests_get_an_error_and_the_connection_stays_usable() {
        let h = hub();
        let mut c = connect(&h);
        c.send("this is not json").await;
        assert!(
            matches!(c.reply().await, Reply::Error { message } if message.contains("bad request"))
        );
        c.send(r#"{"cmd":"nope"}"#).await;
        assert!(matches!(c.reply().await, Reply::Error { .. }));
        c.send("").await; // blank lines are skipped
        c.send(r#"{"cmd":"status"}"#).await;
        assert!(matches!(c.reply().await, Reply::Status(_)));
    }

    #[tokio::test]
    async fn m07_reload_without_a_reloader_says_not_available_and_with_one_calls_it() {
        struct R(Mutex<u32>);
        #[async_trait]
        impl Reloader for R {
            async fn reload(&self) -> Result<String, String> {
                let mut n = self.0.lock().unwrap();
                *n += 1;
                if *n == 1 {
                    Ok("2 settings changed".into())
                } else {
                    Err("database unreachable".into())
                }
            }
        }
        let h = hub();
        let mut c = connect(&h);
        c.send(r#"{"cmd":"reload"}"#).await;
        assert!(
            matches!(c.reply().await, Reply::Error { message } if message.contains("not available"))
        );
        h.set_reloader(Arc::new(R(Mutex::new(0))));
        c.send(r#"{"cmd":"reload"}"#).await;
        assert_eq!(
            c.reply().await,
            Reply::Reload {
                message: "2 settings changed".into()
            }
        );
        c.send(r#"{"cmd":"reload"}"#).await;
        assert_eq!(
            c.reply().await,
            Reply::Error {
                message: "database unreachable".into()
            }
        );
    }

    #[tokio::test]
    async fn m09_a_stop_request_is_announced_streamed_and_ended_with_stopped() {
        let h = hub();
        h.on_event(&started("a"));
        let mut c = connect(&h);
        c.send(r#"{"cmd":"stop","timeout":60}"#).await;
        assert_eq!(
            c.reply().await,
            Reply::Stopping {
                active: 1,
                timeout_secs: 60,
                force: false
            }
        );
        // the service side picks the request up
        let req = h.stop_requested().await;
        assert_eq!(
            req,
            StopRequest {
                force: false,
                timeout: Duration::from_secs(60)
            }
        );
        assert_eq!(h.status().state, "stopping");
        h.set_progress(Progress::Draining {
            active: 1,
            remaining_secs: 55,
        });
        assert_eq!(
            c.reply().await,
            Reply::Waiting {
                active: 1,
                remaining_secs: 55
            }
        );
        h.set_progress(Progress::Draining {
            active: 1,
            remaining_secs: 50,
        });
        assert_eq!(
            c.reply().await,
            Reply::Waiting {
                active: 1,
                remaining_secs: 50
            }
        );
        h.set_progress(Progress::Stopped);
        assert_eq!(c.reply().await, Reply::Stopped);
        assert!(c.eof().await, "the connection ends after `stopped`");
    }

    #[tokio::test]
    async fn m05_the_first_stop_decides_the_timeout_and_a_later_force_upgrades_it() {
        let h = hub();
        let mut a = connect(&h);
        a.send(r#"{"cmd":"stop"}"#).await;
        assert_eq!(
            a.reply().await,
            Reply::Stopping {
                active: 0,
                timeout_secs: 300,
                force: false
            },
            "the default is [service] stop_timeout_secs"
        );
        let force = h.force_receiver();
        assert!(!*force.borrow());
        // a second client, with another timeout: it does not change the one in force
        let mut b = connect(&h);
        b.send(r#"{"cmd":"stop","timeout":5}"#).await;
        assert_eq!(
            b.reply().await,
            Reply::Stopping {
                active: 0,
                timeout_secs: 300,
                force: false
            }
        );
        assert!(!*force.borrow());
        // and --force makes the running wait end
        let mut c = connect(&h);
        c.send(r#"{"cmd":"stop","force":true}"#).await;
        assert!(matches!(
            c.reply().await,
            Reply::Stopping { force: true, .. }
        ));
        assert!(*force.borrow(), "the wait for the calls is cut short");
        assert_eq!(h.stop_requested().await.timeout, Duration::from_secs(300));
    }

    #[tokio::test]
    async fn m07_subscribe_streams_the_calls_events_in_order_until_the_client_leaves() {
        let h = hub();
        h.on_event(&started("old"));
        let mut c = connect(&h);
        c.send(r#"{"cmd":"subscribe"}"#).await;
        let Reply::Subscribed { calls } = c.reply().await else {
            panic!("subscribed")
        };
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].call_id, "old");

        h.on_event(&started("new"));
        h.on_event(&CallEvent::PageReceived {
            call_id: "new".into(),
            pages: 1,
            at: SystemTime::now(),
        });
        h.on_event(&finished("new", 1, true));
        let mut got = Vec::new();
        for _ in 0..3 {
            let Reply::Event { event } = c.reply().await else {
                panic!("event")
            };
            got.push(event);
        }
        assert!(matches!(&got[0], Event::CallStarted { call } if call.call_id == "new"));
        assert_eq!(
            got[1],
            Event::FaxProgress {
                call_id: "new".into(),
                pages: 1
            }
        );
        assert_eq!(
            got[2],
            Event::CallFinished {
                call_id: "new".into(),
                status: "failed".into(),
                pages: 1,
                fax_status: "partial".into(),
                hangup_code: "shutdown".into()
            }
        );
        h.set_progress(Progress::Draining {
            active: 1,
            remaining_secs: 9,
        });
        assert_eq!(
            c.reply().await,
            Reply::Event {
                event: Event::Stopping { active: 1 }
            }
        );
        // the client closes: the service side lets go (the connection task ends)
        drop(c);
        tokio::time::sleep(Duration::from_millis(50)).await;
        assert_eq!(h.events.receiver_count(), 0, "the subscription is gone");
    }
}
