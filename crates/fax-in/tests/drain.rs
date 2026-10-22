//! M4d at the controller: the stop sequence (`FaxIn::drain` then `shutdown`), deterministic.
//! The controller runs on a detached sip-core endpoint under paused time; the test is the
//! gateway and the clock. Nothing waits for real.
//!
//! * M04: after `drain` starts, a new INVITE is answered 503, the running call is left alone
//!   and the drain ends the moment that call ends; its fax is complete (`received`).
//! * M05: when `timeout` passes (or `force` is set) the call is ended by `shutdown`; the pages
//!   that were confirmed are kept, the result is `partial` with `hangup_code = shutdown`.

mod common;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use common::*;
use fax_in::event::{CallOutcome, FailureReason, FaxReport, UdptlCounters};
use fax_in::session::{FaxSession, FaxSessionFactory, SessionContext, SessionOutcome, cancelled};
use fax_in::{CallEvent, CallResult, DrainProgress, FaxInConfig, HangupCode, classify};
use sip_core::message::SipMessage;
use store::FaxStatus;
use tokio::sync::{Notify, watch};
use tokio::time::Instant;

/// Confirms `pages` pages at once, then either finishes when `finish` is notified (a fax that
/// ends by itself) or waits for the call to end underneath it.
struct Scripted {
    pages: u32,
    finish: Option<Arc<Notify>>,
}

fn report(pages: u32, code: u8, partial: bool) -> FaxReport {
    FaxReport {
        code,
        code_name: if code == 0 { "ok" } else { "aborted" }.into(),
        pages,
        bit_rate: 14_400,
        modem: Some("v17".into()),
        resolution: Some("fine".into()),
        compression: Some("mmr".into()),
        remote_ident: Some("0212345678".into()),
        bad_lines_per_page: vec![0; pages as usize],
        duration_ms: 1000,
        partial,
        tiff: Some("/d/faxes/x.tif".into()),
        pdf: Some("/d/faxes/x.pdf".into()),
        store_error: None,
        udptl: UdptlCounters::default(),
        tiff_size: Some(1),
        pdf_size: Some(1),
        ecm: false,
        ecm_ppr_sent: 0,
        ecm_frames_lost: 0,
    }
}

impl FaxSession for Scripted {
    fn run(
        self: Box<Self>,
        mut ctx: SessionContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = SessionOutcome> + Send>> {
        Box::pin(async move {
            for n in 1..=self.pages {
                if let Some(p) = &ctx.progress {
                    p.send(n).unwrap();
                }
            }
            match &self.finish {
                Some(f) => {
                    f.notified().await;
                    *ctx.report.lock().unwrap() = Some(report(self.pages, 0, false));
                    SessionOutcome::Completed
                }
                None => {
                    cancelled(&mut ctx.cancel).await;
                    // what the real session does when it is cancelled: write what it has
                    *ctx.report.lock().unwrap() = Some(report(self.pages, 11, self.pages > 0));
                    SessionOutcome::Aborted
                }
            }
        })
    }
}

struct Factory {
    pages: u32,
    finish: Option<Arc<Notify>>,
}

impl FaxSessionFactory for Factory {
    fn create(&self, _: &SessionContext) -> Box<dyn FaxSession> {
        Box::new(Scripted {
            pages: self.pages,
            finish: self.finish.clone(),
        })
    }
}

async fn rig(pages: u32, finish: Option<Arc<Notify>>) -> W {
    let cfg = FaxInConfig::parse(&base_config("")).unwrap();
    W::with_factory(cfg, Some(Arc::new(Factory { pages, finish }))).await
}

/// One answered, ACKed T.38 call with the session running.
async fn answered(w: &W, call_id: &str) -> String {
    w.call(call_id, T38_SDP, Some(100)).await
}

async fn invite_refused(w: &W, call_id: &str) {
    w.feed(Req::invite(call_id, T38_SDP).build()).await;
}

fn only_result(w: &mut W) -> CallResult {
    let mut r = w.results();
    assert_eq!(r.len(), 1, "exactly one call result: {r:?}");
    r.remove(0)
}

fn no_progress(_: DrainProgress) {}

fn force_never() -> (watch::Sender<bool>, watch::Receiver<bool>) {
    watch::channel(false)
}

#[tokio::test(start_paused = true)]
async fn m04_drain_without_calls_returns_at_once_and_still_refuses_new_invites() {
    let mut w = rig(0, None).await;
    let fx = w.fx.as_ref().unwrap();
    assert!(!fx.is_draining());
    let (_f, rx) = force_never();
    let t0 = Instant::now();
    let left = fx
        .drain(
            Duration::from_secs(300),
            rx,
            Duration::from_secs(5),
            no_progress,
        )
        .await;
    assert_eq!(left, 0);
    assert_eq!(Instant::now(), t0, "nothing to wait for: no time passes");
    assert!(fx.is_draining());
    invite_refused(&w, "late@gw").await;
    assert_eq!(
        w.responses(503, "INVITE").len(),
        1,
        "{:?}",
        w.status_lines()
    );
    assert!(
        w.responses(200, "INVITE").is_empty(),
        "the INVITE was not taken: {:?}",
        w.status_lines()
    );
    assert!(w.events().is_empty(), "a refused INVITE is not a call");
    w.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn m04_a_running_fax_is_waited_for_new_invites_get_503_and_the_fax_is_complete() {
    let finish = Arc::new(Notify::new());
    let mut w = rig(2, Some(finish.clone())).await;
    let tag = answered(&w, "running@gw").await;
    assert_eq!(w.fx.as_ref().unwrap().active_calls(), 1);

    let seen: Arc<Mutex<Vec<DrainProgress>>> = Arc::default();
    let (_f, rx) = force_never();
    let fx = w.fx.take().unwrap();
    let t0 = Instant::now();
    let s2 = seen.clone();
    let drain = tokio::spawn(async move {
        let left = fx
            .drain(
                Duration::from_secs(300),
                rx,
                Duration::from_secs(5),
                move |p| s2.lock().unwrap().push(p),
            )
            .await;
        (fx, left, Instant::now())
    });

    // 12 s in: still waiting, progress was reported at 5 s and 10 s
    tokio::time::sleep(Duration::from_secs(12)).await;
    settle().await;
    assert!(!drain.is_finished(), "the call is still running");
    {
        let p = seen.lock().unwrap().clone();
        assert_eq!(p.len(), 2, "{p:?}");
        assert!(p.iter().all(|x| x.active == 1));
        assert_eq!(p[0].remaining, Duration::from_secs(295));
        assert_eq!(p[1].remaining, Duration::from_secs(290));
    }
    // a new INVITE during the drain: 503, and the running call is untouched
    invite_refused(&w, "late@gw").await;
    let refused = w.responses(503, "INVITE");
    assert_eq!(refused.len(), 1, "{:?}", w.status_lines());
    assert_eq!(refused[0].1.headers.call_id(), Some("late@gw"));
    assert!(
        w.requests("BYE").is_empty(),
        "the running call was not hung up"
    );

    // the fax finishes by itself at 20 s: the drain ends right then, with nothing left
    tokio::time::sleep(Duration::from_secs(8)).await;
    finish.notify_one();
    settle().await;
    // the call ends with our BYE; the gateway answers it
    let bye = w.requests("BYE");
    assert_eq!(bye.len(), 1, "our BYE");
    w.feed_resp(resp_to(&bye[0].1, 200, GW_TAG, None)).await;
    let (fx, left, ended) = drain.await.unwrap();
    assert_eq!(left, 0);
    assert_eq!(ended - t0, Duration::from_secs(20));
    w.fx = Some(fx);
    settle().await;
    let r = only_result(&mut w);
    assert_eq!(r.call_id, "running@gw");
    assert_eq!(r.outcome, CallOutcome::Completed);
    let s = classify(&r);
    assert_eq!(s.fax_status, FaxStatus::Received);
    assert_eq!(s.pages, 2);
    let _ = tag;
    w.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn m05_when_the_timeout_passes_the_call_is_ended_and_the_pages_are_kept() {
    let mut w = rig(2, None).await;
    answered(&w, "slow@gw").await;
    let fx = w.fx.take().unwrap();
    let (_f, rx) = force_never();
    let t0 = Instant::now();
    let left = fx
        .drain(
            Duration::from_secs(300),
            rx,
            Duration::from_secs(5),
            no_progress,
        )
        .await;
    assert_eq!(left, 1, "the call is still there when the time is up");
    assert_eq!(Instant::now() - t0, Duration::from_secs(300));
    assert!(
        w.events()
            .iter()
            .all(|e| !matches!(e, CallEvent::Finished(_)))
    );
    // the stop: calls are ended, pages written, result is partial / shutdown
    fx.shutdown().await;
    let r = only_result(&mut w);
    assert_eq!(
        r.outcome,
        CallOutcome::Failed {
            reason: FailureReason::Shutdown
        }
    );
    let s = classify(&r);
    assert_eq!(s.fax_status, FaxStatus::Partial);
    assert_eq!(s.hangup_code, HangupCode::Shutdown);
    assert_eq!(s.hangup_code.as_str(), "shutdown");
    assert_eq!(s.pages, 2);
    assert!(r.fax.as_ref().unwrap().partial);
    assert!(!w.requests("BYE").is_empty(), "we hung up");
}

#[tokio::test(start_paused = true)]
async fn m05_force_ends_the_wait_at_once_even_in_the_middle_of_it() {
    let mut w = rig(1, None).await;
    answered(&w, "forced@gw").await;
    let fx = w.fx.take().unwrap();
    let (force, rx) = force_never();
    let t0 = Instant::now();
    let drain = tokio::spawn(async move {
        let left = fx
            .drain(
                Duration::from_secs(300),
                rx,
                Duration::from_secs(5),
                no_progress,
            )
            .await;
        (fx, left, Instant::now())
    });
    tokio::time::sleep(Duration::from_secs(7)).await;
    settle().await;
    assert!(!drain.is_finished());
    force.send(true).unwrap();
    let (fx, left, ended) = drain.await.unwrap();
    assert_eq!(left, 1);
    assert_eq!(ended - t0, Duration::from_secs(7), "not a second later");
    fx.shutdown().await;
    let s = classify(&only_result(&mut w));
    assert_eq!(
        (s.fax_status, s.hangup_code, s.pages),
        (FaxStatus::Partial, HangupCode::Shutdown, 1)
    );
}

#[tokio::test(start_paused = true)]
async fn m05_a_force_that_is_set_before_the_drain_starts_does_not_wait_at_all() {
    let mut w = rig(0, None).await;
    answered(&w, "c@gw").await;
    let fx = w.fx.take().unwrap();
    let (_force, rx) = watch::channel(true);
    let t0 = Instant::now();
    let left = fx
        .drain(
            Duration::from_secs(300),
            rx,
            Duration::from_secs(5),
            no_progress,
        )
        .await;
    assert_eq!((left, Instant::now()), (1, t0));
    fx.shutdown().await;
    // no page: failed, not partial
    let s = classify(&only_result(&mut w));
    assert_eq!(
        (s.fax_status, s.hangup_code, s.pages),
        (FaxStatus::Failed, HangupCode::Shutdown, 0)
    );
}

#[tokio::test(start_paused = true)]
async fn m05_a_dropped_force_sender_does_not_end_the_wait() {
    let mut w = rig(1, None).await;
    let tag = answered(&w, "c@gw").await;
    let fx = w.fx.take().unwrap();
    let (force, rx) = force_never();
    drop(force);
    let drain = tokio::spawn(async move {
        let left = fx
            .drain(
                Duration::from_secs(60),
                rx,
                Duration::from_secs(5),
                no_progress,
            )
            .await;
        (fx, left)
    });
    tokio::time::sleep(Duration::from_secs(30)).await;
    settle().await;
    assert!(!drain.is_finished(), "30 s of 60 s have passed");
    // the gateway hangs up: the wait ends with nothing left
    w.feed_from(
        SipMessage::Request(Req::in_dialog("BYE", "c@gw", "bye", 2, &tag).build()),
        GW,
    )
    .await;
    let (fx, left) = drain.await.unwrap();
    assert_eq!(left, 0);
    w.fx = Some(fx);
    w.shutdown().await;
}
