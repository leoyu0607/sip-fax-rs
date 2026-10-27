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
        media: Default::default(),
        rtp: None,
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
        audio: None,
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
    assert!(matches!(c.reply().await, Reply::Error { message } if message.contains("bad request")));
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
