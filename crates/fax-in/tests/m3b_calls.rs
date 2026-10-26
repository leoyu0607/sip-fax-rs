//! M3b at the controller level: a real `FaxIn` on localhost sockets with a sip-core UAC as the
//! gateway, the events fed to a [`Recorder`] on a real SQLite store. Covers the receiving state
//! with climbing pages (the S02 flow, driven by the controller; I01 proper is in the SpanDSP suite), ANI / DNIS from the INVITE, who sent the BYE, and C04 for
//! sources outside every trunk. The SpanDSP versions are in `tools/fax-sim/tests/e2e_m3b.rs`.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use fax_in::event::{ByeBy, CallEvent, CallOutcome, CallResult, FaxReport, UdptlCounters};
use fax_in::recorder::{Recorder, RecorderConfig, StoreSink};
use fax_in::session::{FaxSession, FaxSessionFactory, SessionContext, SessionOutcome, cancelled};
use fax_in::{FaxIn, FaxInConfig, StartOptions};
use sip_core::endpoint::{DialogHandle, Endpoint, EndpointConfig, InviteOptions};
use sip_core::message::SipUri;
use sip_core::sdp::{SessionDescription, parse_sdp};
use store::{FaxStatus, FaxStore, HangupDirection};
use tokio::sync::{Notify, mpsc};

const T38_SDP: &str = "v=0\r\no=gw 100 100 IN IP4 127.0.0.1\r\ns=-\r\nc=IN IP4 127.0.0.1\r\nt=0 0\r\nm=image 5004 udptl t38\r\na=T38FaxVersion:0\r\na=T38MaxBitRate:14400\r\na=T38FaxRateManagement:transferredTCF\r\na=T38FaxMaxBuffer:1800\r\na=T38FaxMaxDatagram:200\r\na=T38FaxUdpEC:t38UDPRedundancy\r\n";

fn sdp(t: &str) -> SessionDescription {
    parse_sdp(t.as_bytes()).unwrap()
}

/// A session that confirms pages one at a time on request, then ends the way the test wants.
struct Scripted {
    /// Pages to confirm, released one per `Notify` permit.
    pages: u32,
    step: Arc<Notify>,
    /// `true`: finish by itself (we send the BYE); `false`: wait for the peer's BYE.
    complete: bool,
}

impl FaxSession for Scripted {
    fn run(
        self: Box<Self>,
        mut ctx: SessionContext,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = SessionOutcome> + Send>> {
        Box::pin(async move {
            let mut seen = 0;
            for n in 1..=self.pages {
                self.step.notified().await;
                seen = n;
                if let Some(p) = &ctx.progress {
                    p.send(n).unwrap();
                }
            }
            let rep = FaxReport {
                code: if self.complete { 0 } else { 11 },
                code_name: if self.complete { "ok" } else { "aborted" }.into(),
                pages: seen,
                bit_rate: 14_400,
                modem: Some("v17".into()),
                resolution: Some("fine".into()),
                compression: Some("mmr".into()),
                remote_ident: Some("0212345678".into()),
                bad_lines_per_page: vec![0; seen as usize],
                duration_ms: 1000,
                partial: !self.complete && seen > 0,
                tiff: Some("/d/faxes/2026/10/21/x.tif".into()),
                pdf: Some("/d/faxes/2026/10/21/x.pdf".into()),
                store_error: None,
                udptl: UdptlCounters::default(),
                media: Default::default(),
                rtp: None,
                tiff_size: Some(111),
                pdf_size: Some(222),
                ecm: true,
                ecm_ppr_sent: 1,
                ecm_frames_lost: 0,
            };
            *ctx.report.lock().unwrap() = Some(rep);
            if self.complete {
                SessionOutcome::Completed
            } else {
                cancelled(&mut ctx.cancel).await;
                SessionOutcome::Aborted
            }
        })
    }
}

struct Factory {
    pages: u32,
    step: Arc<Notify>,
    complete: bool,
}

impl FaxSessionFactory for Factory {
    fn create(&self, _: &SessionContext) -> Box<dyn FaxSession> {
        Box::new(Scripted {
            pages: self.pages,
            step: self.step.clone(),
            complete: self.complete,
        })
    }
}

fn config(allowed: &str, unknown: &str, trunk_extra: &str) -> FaxInConfig {
    FaxInConfig::parse(&format!(
        "[sip]\nbind = \"127.0.0.1:0\"\nunknown_source = \"{unknown}\"\n[capture]\nenabled = false\n\
         [[trunks]]\nname = \"local\"\nallowed_sources = [\"{allowed}\"]\n{trunk_extra}\n"
    ))
    .unwrap()
}

struct Rig {
    fx: FaxIn,
    store: Arc<dyn FaxStore>,
    rec: Arc<Recorder>,
    events: tokio::task::JoinHandle<()>,
    results: mpsc::UnboundedReceiver<CallResult>,
    step: Arc<Notify>,
}

async fn rig(cfg: FaxInConfig, pages: u32, complete: bool) -> Rig {
    let store = store::connect("sqlite::memory:").await.unwrap();
    let rec = Arc::new(Recorder::start(
        Arc::new(StoreSink(store.clone())),
        RecorderConfig::new("/d".into()),
    ));
    let step = Arc::new(Notify::new());
    let (fx, mut ev) = FaxIn::start(
        cfg,
        StartOptions {
            session_factory: Some(Arc::new(Factory {
                pages,
                step: step.clone(),
                complete,
            })),
            base_dir: None,
        },
    )
    .await
    .unwrap();
    let (rtx, results) = mpsc::unbounded_channel();
    let r2 = rec.clone();
    // what sip-faxd's event loop does
    let events = tokio::spawn(async move {
        while let Some(e) = ev.recv().await {
            r2.record(&e);
            if let CallEvent::Finished(r) = e {
                let _ = rtx.send(*r);
            }
        }
    });
    Rig {
        fx,
        store,
        rec,
        events,
        results,
        step,
    }
}

impl Rig {
    fn target(&self) -> SocketAddr {
        self.fx.udp_addr().unwrap()
    }

    async fn invite(&self, extra: Vec<(String, String)>) -> (Endpoint, DialogHandle) {
        let gw = Endpoint::bind(EndpointConfig::new().udp("127.0.0.1:0".parse().unwrap()))
            .await
            .unwrap()
            .0;
        let addr = self.target();
        let mut o = InviteOptions::new(
            addr,
            SipUri::parse(&format!("sip:0255501234@{addr}")).unwrap(),
            SipUri::parse("sip:0933111222@127.0.0.1").unwrap(),
            SipUri::parse(&format!("sip:0255501234@{addr}")).unwrap(),
            sdp(T38_SDP),
        );
        o.headers = extra;
        let dlg = gw.invite(o).await.unwrap();
        (gw, dlg)
    }

    /// Waits until the database shows `pages` pages for the call.
    async fn wait_pages(&self, call_id: &str, pages: i32) {
        for _ in 0..400 {
            self.rec.flush(Duration::from_secs(5)).await;
            if let Some(r) = self.store.get_by_call_id(call_id).await.unwrap()
                && r.pages == pages
            {
                return;
            }
            tokio::task::yield_now().await;
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        panic!("the database never showed {pages} page(s) for {call_id}");
    }

    async fn result(&mut self) -> CallResult {
        tokio::time::timeout(Duration::from_secs(10), self.results.recv())
            .await
            .expect("a call result")
            .unwrap()
    }

    async fn stop(self) {
        self.fx.shutdown().await;
        let _ = self.events.await;
        self.rec.shutdown(Duration::from_secs(5)).await;
    }
}

#[tokio::test]
async fn s02_controller_the_database_shows_receiving_with_climbing_pages_then_the_final_state() {
    let mut r = rig(config("127.0.0.0/8", "drop", ""), 3, false).await;
    let (_gw, dlg) = r.invite(vec![]).await;
    let call_id = dlg.id().call_id.clone();
    // answered: the row exists as `receiving` with zero pages and the numbers of the INVITE
    r.wait_pages(&call_id, 0).await;
    let row = r.store.get_by_call_id(&call_id).await.unwrap().unwrap();
    assert_eq!(row.fax_status, FaxStatus::Receiving);
    assert_eq!(row.trunk, "local");
    assert_eq!(
        (row.ani.as_deref(), row.ani_source.as_deref()),
        (Some("0933111222"), Some("from"))
    );
    assert_eq!(
        (row.dnis.as_deref(), row.dnis_source.as_deref()),
        (Some("0255501234"), Some("ruri"))
    );
    assert!(row.ended_at.is_none());
    for n in 1..=3 {
        r.step.notify_one();
        r.wait_pages(&call_id, n).await;
        let row = r.store.get_by_call_id(&call_id).await.unwrap().unwrap();
        assert_eq!(
            row.fax_status,
            FaxStatus::Receiving,
            "still receiving after page {n}"
        );
        assert_eq!(row.pages, n);
    }
    // the gateway hangs up after page 3 but before T.30 finished
    assert_eq!(dlg.bye().await.unwrap().code(), 200);
    let res = r.result().await;
    assert_eq!(res.bye_by, Some(ByeBy::Remote));
    assert!(matches!(res.outcome, CallOutcome::Failed { .. }));
    r.rec.flush(Duration::from_secs(5)).await;
    let row = r.store.get_by_call_id(&call_id).await.unwrap().unwrap();
    assert_eq!(row.fax_status, FaxStatus::Partial);
    assert_eq!(row.pages, 3);
    assert_eq!(row.hangup_code.as_deref(), Some("peer_hangup"));
    assert_eq!(row.hangup_direction, Some(HangupDirection::Remote));
    assert_eq!(row.t30_code, Some(11));
    assert!(row.ecm, "ECM negotiated by the session reaches the row");
    assert!(row.ended_at.is_some() && row.answered_at.is_some());
    r.stop().await;
}

#[tokio::test]
async fn s02_controller_a_session_that_completes_ends_with_our_bye_and_received() {
    let mut r = rig(config("127.0.0.0/8", "drop", ""), 2, true).await;
    let (_gw, dlg) = r.invite(vec![]).await;
    let call_id = dlg.id().call_id.clone();
    r.step.notify_one();
    r.step.notify_one();
    let res = r.result().await;
    assert_eq!(res.outcome, CallOutcome::Completed);
    assert_eq!(res.bye_by, Some(ByeBy::Local));
    r.rec.flush(Duration::from_secs(5)).await;
    let row = r.store.get_by_call_id(&call_id).await.unwrap().unwrap();
    assert_eq!(row.fax_status, FaxStatus::Received);
    assert_eq!(row.pages, 2);
    assert_eq!(row.hangup_code.as_deref(), Some("completed"));
    assert_eq!(row.hangup_direction, Some(HangupDirection::Local));
    assert_eq!(row.tiff_path.as_deref(), Some("faxes/2026/10/21/x.tif"));
    assert_eq!((row.tiff_size, row.pdf_size), (Some(111), Some(222)));
    r.stop().await;
}

#[tokio::test]
async fn c02_the_trunk_config_decides_where_the_numbers_come_from() {
    let extra = "[trunks.numbers]\nani_sources = [\"pai\", \"from\"]\nstrip_plus = true\ndnis_strip_prefix = \"02\"\n";
    let mut r = rig(config("127.0.0.0/8", "drop", extra), 0, false).await;
    let (_gw, dlg) = r
        .invite(vec![(
            "P-Asserted-Identity".into(),
            "<sip:+886223456789@carrier.example>".into(),
        )])
        .await;
    let call_id = dlg.id().call_id.clone();
    r.wait_pages(&call_id, 0).await;
    let row = r.store.get_by_call_id(&call_id).await.unwrap().unwrap();
    assert_eq!(
        (row.ani.as_deref(), row.ani_source.as_deref()),
        (Some("886223456789"), Some("pai"))
    );
    assert_eq!(
        (row.dnis.as_deref(), row.dnis_source.as_deref()),
        (Some("55501234"), Some("ruri"))
    );
    dlg.bye().await.unwrap();
    let res = r.result().await;
    assert_eq!(res.numbers.ani.as_deref(), Some("886223456789"));
    r.stop().await;
}

#[tokio::test]
async fn c02_a_hidden_number_is_stored_as_anonymous() {
    let mut r = rig(config("127.0.0.0/8", "drop", ""), 0, false).await;
    let (_gw, dlg) = r.invite(vec![("Privacy".into(), "id".into())]).await;
    let call_id = dlg.id().call_id.clone();
    r.wait_pages(&call_id, 0).await;
    let row = r.store.get_by_call_id(&call_id).await.unwrap().unwrap();
    assert_eq!(
        (row.ani.clone(), row.ani_source.as_deref()),
        (None, Some("anonymous"))
    );
    dlg.bye().await.unwrap();
    r.result().await;
    r.stop().await;
}

#[tokio::test]
async fn c04_unknown_source_log_mode_is_handled_and_recorded_under_the_unknown_trunk() {
    let mut r = rig(config("10.9.9.0/24", "log", ""), 0, false).await;
    let (_gw, dlg) = r.invite(vec![]).await;
    let call_id = dlg.id().call_id.clone();
    r.wait_pages(&call_id, 0).await;
    let row = r.store.get_by_call_id(&call_id).await.unwrap().unwrap();
    assert_eq!(row.trunk, "unknown");
    dlg.bye().await.unwrap();
    r.result().await;
    r.stop().await;
}

#[tokio::test]
async fn c04_a_source_outside_every_trunk_is_refused_and_not_stored() {
    // the trunk only allows 10.9.9.0/24; the gateway comes from 127.0.0.1 and gets a 403
    let mut r = rig(config("10.9.9.0/24", "reject", ""), 0, false).await;
    let gw = Endpoint::bind(EndpointConfig::new().udp("127.0.0.1:0".parse().unwrap()))
        .await
        .unwrap()
        .0;
    let addr = r.target();
    let o = InviteOptions::new(
        addr,
        SipUri::parse(&format!("sip:0255501234@{addr}")).unwrap(),
        SipUri::parse("sip:0933111222@127.0.0.1").unwrap(),
        SipUri::parse(&format!("sip:0255501234@{addr}")).unwrap(),
        sdp(T38_SDP),
    );
    assert!(gw.invite(o).await.is_err(), "the INVITE is refused");
    // the transport refuses it before any call exists: no event, nothing for the recorder
    r.rec.flush(Duration::from_secs(5)).await;
    assert!(
        r.results.try_recv().is_err(),
        "no call result for a refused source"
    );
    let all = r
        .store
        .list(&store::ListFilter::default(), store::PageRequest::default())
        .await
        .unwrap();
    assert!(all.items.is_empty(), "{:?}", all.items);
    r.stop().await;
}
