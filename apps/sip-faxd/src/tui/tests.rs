//! M13: the TUI on a real SQLite store, drawn into a `TestBackend`, and fed by a real control
//! socket (hub + server on a temporary path). No sleeps: the only waits are channel receives
//! with a timeout that is never reached when the code is right.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime};

use chrono::{TimeZone, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use store::{DeliveryStatus, FaxFinal, FaxStatus, FaxStore, HangupDirection, NewDelivery, NewFax};
use tokio::sync::mpsc;

use super::app::{App, Live, Pane};
use super::i18n::Lang;
use super::{socket_loop, ui};
use crate::service::control::ControlServer;
use crate::service::hub::tests::{finished, started};
use crate::service::hub::{Hub, HubInfo};
use crate::service::protocol::{CallInfo, Event};

// ------------------------------------------------------------------ helpers

struct Env {
    _dir: tempfile::TempDir,
    store: Arc<dyn FaxStore>,
}

async fn env() -> Env {
    let dir = tempfile::tempdir().unwrap();
    let url = format!("sqlite://{}", dir.path().join("fax.db").display());
    let store = store::connect(&url).await.unwrap();
    Env { _dir: dir, store }
}

fn nf(call: &str, ani: &str, minute: i64) -> NewFax {
    NewFax {
        call_id: call.into(),
        trunk: "local".into(),
        ani: Some(ani.into()),
        ani_source: Some("from".into()),
        dnis: Some("0255".into()),
        dnis_source: Some("ruri".into()),
        started_at: Utc
            .timestamp_millis_opt(1_792_540_800_000 + minute * 60_000)
            .unwrap(),
        answered_at: None,
    }
}

fn ff(status: FaxStatus, pages: i32) -> FaxFinal {
    FaxFinal {
        ended_at: Utc.timestamp_millis_opt(1_792_540_860_000).unwrap(),
        answered_at: None,
        duration_sec: Some(60),
        hangup_direction: HangupDirection::Local,
        hangup_code: "completed".into(),
        hangup_text: None,
        sip_status: None,
        t30_code: Some(0),
        fax_status: status,
        pages,
        remote_ident: Some("0212345678".into()),
        resolution: None,
        bit_rate: None,
        compression: None,
        ecm: false,
        tiff_path: Some("faxes/a.tif".into()),
        tiff_size: Some(3),
        pdf_path: Some("faxes/a.pdf".into()),
        pdf_size: Some(3),
    }
}

async fn add(st: &Arc<dyn FaxStore>, call: &str, ani: &str, minute: i64) -> i64 {
    st.upsert_finished(&nf(call, ani, minute), &ff(FaxStatus::Received, 2))
        .await
        .unwrap()
        .0
}

fn app_of(e: &Env, lang: Lang) -> App {
    App::new(Some(e.store.clone()), crate::runtime::cli_operator(), lang)
}

fn key(code: KeyCode) -> KeyEvent {
    KeyEvent::new(code, KeyModifiers::NONE)
}

async fn press(app: &mut App, codes: &[KeyCode]) {
    for c in codes {
        app.on_key(key(*c)).await;
    }
}

async fn type_text(app: &mut App, s: &str) {
    for c in s.chars() {
        app.on_key(key(KeyCode::Char(c))).await;
    }
}

fn render(app: &App) -> String {
    let mut term = Terminal::new(TestBackend::new(130, 24)).unwrap();
    term.draw(|f| ui::draw(f, app)).unwrap();
    let buf = term.backend().buffer().clone();
    let w = buf.area.width as usize;
    buf.content()
        .chunks(w)
        .map(|row| row.iter().map(|c| c.symbol()).collect::<String>())
        .collect::<Vec<_>>()
        .join("\n")
}

/// Wide characters leave blank cells behind them: compare without any white space.
fn squash(s: &str) -> String {
    s.chars().filter(|c| !c.is_whitespace()).collect()
}

fn row_of<'a>(screen: &'a str, needle: &str) -> &'a str {
    screen
        .lines()
        .find(|l| l.contains(needle))
        .unwrap_or_else(|| panic!("{needle:?} not on the screen:\n{screen}"))
}

fn call(id: &str) -> CallInfo {
    CallInfo {
        call_id: id.into(),
        trunk: "gw".into(),
        peer: "10.0.0.1:5060".into(),
        ani: "0212345678".into(),
        dnis: "0287654321".into(),
        started_at: "2027-01-15T08:00:00Z".into(),
        answered: false,
        t38: false,
        pages: 0,
    }
}

// ------------------------------------------------------------------ list

#[tokio::test]
async fn m13_the_list_shows_the_database_rows_newest_first_and_u_hides_read_ones() {
    let e = env().await;
    let old = add(&e.store, "c-old", "0211111111", 0).await;
    let mid = add(&e.store, "c-mid", "0222222222", 1).await;
    let new = add(&e.store, "c-new", "0233333333", 2).await;
    e.store.set_read(mid, true, "web").await.unwrap();
    let mut app = app_of(&e, Lang::En);
    app.reload().await;

    let s = render(&app);
    let order: Vec<usize> = ["0233333333", "0222222222", "0211111111"]
        .iter()
        .map(|n| s.find(n).unwrap_or_else(|| panic!("{n} missing:\n{s}")))
        .collect();
    assert!(order.windows(2).all(|w| w[0] < w[1]), "newest first:\n{s}");
    assert!(row_of(&s, "0233333333").contains("UNREAD"));
    assert!(!row_of(&s, "0222222222").contains("UNREAD"));
    assert!(row_of(&s, "0222222222").contains("read"));
    assert!(s.contains(&new.to_string()) && s.contains(&old.to_string()));

    // `u`: only the unread ones
    press(&mut app, &[KeyCode::Char('u')]).await;
    let s = render(&app);
    assert!(s.contains("0233333333") && s.contains("0211111111"), "{s}");
    assert!(!s.contains("0222222222"), "the read fax is hidden:\n{s}");
    assert!(s.contains("unread only"));

    // reverse: `u` again shows everything
    press(&mut app, &[KeyCode::Char('u')]).await;
    assert!(render(&app).contains("0222222222"));
}

#[tokio::test]
async fn m13_an_empty_database_says_so_and_the_keys_do_not_fail() {
    let e = env().await;
    let mut app = app_of(&e, Lang::En);
    app.reload().await;
    press(
        &mut app,
        &[
            KeyCode::Down,
            KeyCode::Enter,
            KeyCode::Char('r'),
            KeyCode::Char('n'),
        ],
    )
    .await;
    let s = render(&app);
    assert!(s.contains("no faxes"), "{s}");
    assert!(
        app.detail.is_none() && app.note.is_none(),
        "nothing to open or edit"
    );
    assert!(app.message.as_deref().unwrap().contains("no fax selected"));
}

#[tokio::test]
async fn m13_the_cursor_moves_with_jk_and_arrows_and_stays_inside_the_list() {
    let e = env().await;
    add(&e.store, "c1", "0211111111", 0).await;
    add(&e.store, "c2", "0222222222", 1).await;
    let mut app = app_of(&e, Lang::En);
    app.reload().await;
    assert_eq!(app.fax_sel, 0);
    press(
        &mut app,
        &[KeyCode::Char('j'), KeyCode::Down, KeyCode::Down],
    )
    .await;
    assert_eq!(app.fax_sel, 1, "not past the last row");
    press(&mut app, &[KeyCode::Char('k'), KeyCode::Up, KeyCode::Up]).await;
    assert_eq!(app.fax_sel, 0, "not before the first row");
}

// ------------------------------------------------------------------ detail

#[tokio::test]
async fn m13_the_details_show_the_fields_and_the_delivery_attempts() {
    let e = env().await;
    let id = add(&e.store, "call-detail", "0212345678", 0).await;
    for (target, kind, attempt, status) in [
        ("/srv/fax/in", "folder", 1, DeliveryStatus::Done),
        (
            "https://example.com/hook",
            "webhook",
            2,
            DeliveryStatus::Failed,
        ),
    ] {
        e.store
            .add_delivery(&NewDelivery {
                fax_id: id,
                target: target.into(),
                kind: kind.into(),
                attempt,
                status,
                next_try_at: None,
            })
            .await
            .unwrap();
    }
    let other = add(&e.store, "call-other", "0299999999", -5).await;
    e.store
        .add_delivery(&NewDelivery {
            fax_id: other,
            target: "other@example.com".into(),
            kind: "email".into(),
            attempt: 1,
            status: DeliveryStatus::Done,
            next_try_at: None,
        })
        .await
        .unwrap();
    let mut app = app_of(&e, Lang::En);
    app.reload().await;
    press(&mut app, &[KeyCode::Enter]).await;

    let s = render(&app);
    for want in [
        "call-detail",
        "0212345678 (from)",
        "0255 (ruri)",
        "local",
        "received",
        "completed (local)",
        "faxes/a.pdf",
        "/srv/fax/in",
        "done",
        "https://example.com/hook",
        "failed",
    ] {
        assert!(s.contains(want), "{want:?} missing:\n{s}");
    }
    assert!(
        !s.contains("other@example.com"),
        "another fax's delivery:\n{s}"
    );
    assert!(!s.contains("call-other"));

    // Esc goes back to the list
    press(&mut app, &[KeyCode::Esc]).await;
    assert!(app.detail.is_none());
    assert!(render(&app).contains("STARTED"));
}

#[tokio::test]
async fn m13_a_fax_without_deliveries_says_none() {
    let e = env().await;
    add(&e.store, "call-x", "0212345678", 0).await;
    let mut app = app_of(&e, Lang::En);
    app.reload().await;
    press(&mut app, &[KeyCode::Enter]).await;
    let s = render(&app);
    assert!(row_of(&s, "deliveries").contains("none"), "{s}");
}

// ------------------------------------------------------------------ read

#[tokio::test]
async fn m13_r_toggles_read_in_the_database_and_on_the_screen() {
    let e = env().await;
    let id = add(&e.store, "c1", "0211111111", 0).await;
    let mut app = app_of(&e, Lang::En);
    app.reload().await;
    assert!(row_of(&render(&app), "0211111111").contains("UNREAD"));

    press(&mut app, &[KeyCode::Char('r')]).await;
    let rec = e.store.get(id).await.unwrap().unwrap();
    assert!(rec.is_read);
    let who = rec.read_changed_by.unwrap();
    assert_eq!(who, crate::runtime::cli_operator());
    assert!(who.starts_with("cli:"));
    let s = render(&app);
    assert!(!row_of(&s, "0211111111").contains("UNREAD"), "{s}");
    assert!(s.contains("marked read"));

    // reverse: back to unread
    press(&mut app, &[KeyCode::Char('r')]).await;
    assert!(!e.store.get(id).await.unwrap().unwrap().is_read);
    assert!(row_of(&render(&app), "0211111111").contains("UNREAD"));
}

#[tokio::test]
async fn m13_r_in_the_details_changes_the_opened_fax_and_the_screen() {
    let e = env().await;
    let id = add(&e.store, "c1", "0211111111", 0).await;
    let mut app = app_of(&e, Lang::En);
    app.reload().await;
    press(&mut app, &[KeyCode::Enter]).await;
    assert!(row_of(&render(&app), "UNREAD").contains("UNREAD"));
    press(&mut app, &[KeyCode::Char('r')]).await;
    assert!(e.store.get(id).await.unwrap().unwrap().is_read);
    let s = render(&app);
    assert!(!s.contains("UNREAD"), "{s}");
    assert!(
        s.lines().any(|l| l.contains("read") && l.contains("(cli:")),
        "who read it is shown:\n{s}"
    );
}

// ------------------------------------------------------------------ note

#[tokio::test]
async fn m13_n_edits_the_note_and_enter_saves_it_to_the_database() {
    let e = env().await;
    let id = add(&e.store, "c1", "0211111111", 0).await;
    let mut app = app_of(&e, Lang::En);
    app.reload().await;
    press(&mut app, &[KeyCode::Char('n')]).await;
    type_text(&mut app, "call backx").await;
    press(&mut app, &[KeyCode::Backspace]).await;
    let s = render(&app);
    assert!(s.contains("Edit note"), "the input box is drawn:\n{s}");
    assert!(s.contains("call back_"), "{s}");
    press(&mut app, &[KeyCode::Enter]).await;

    let rec = e.store.get(id).await.unwrap().unwrap();
    assert_eq!(rec.note.as_deref(), Some("call back"));
    assert_eq!(
        rec.note_updated_by.as_deref(),
        Some(crate::runtime::cli_operator().as_str())
    );
    assert!(app.note.is_none());
    assert!(app.message.as_deref().unwrap().contains("note saved"));

    // the details show it, and the next edit starts from it
    press(&mut app, &[KeyCode::Enter]).await;
    assert!(render(&app).contains("call back"));
    press(&mut app, &[KeyCode::Char('n')]).await;
    assert_eq!(app.note.as_ref().unwrap().text, "call back");
}

#[tokio::test]
async fn m13_esc_cancels_the_note_and_q_inside_the_input_is_text() {
    let e = env().await;
    let id = add(&e.store, "c1", "0211111111", 0).await;
    let mut app = app_of(&e, Lang::En);
    app.reload().await;
    press(&mut app, &[KeyCode::Char('n')]).await;
    type_text(&mut app, "quit").await;
    assert!(!app.quit, "q is a letter while typing");
    press(&mut app, &[KeyCode::Esc]).await;
    assert!(app.note.is_none());
    assert_eq!(
        e.store.get(id).await.unwrap().unwrap().note,
        None,
        "nothing saved"
    );
}

#[tokio::test]
async fn m13_a_concurrent_note_edit_is_refused_and_the_other_edit_stays() {
    let e = env().await;
    let id = add(&e.store, "c1", "0211111111", 0).await;
    let mut app = app_of(&e, Lang::En);
    app.reload().await;
    press(&mut app, &[KeyCode::Char('n')]).await;
    type_text(&mut app, "mine").await;
    // somebody else saves a note while this one is typed
    e.store
        .set_note(id, "theirs", "web:alice", None)
        .await
        .unwrap();
    press(&mut app, &[KeyCode::Enter]).await;

    let rec = e.store.get(id).await.unwrap().unwrap();
    assert_eq!(
        rec.note.as_deref(),
        Some("theirs"),
        "the other edit is kept"
    );
    assert_eq!(rec.note_updated_by.as_deref(), Some("web:alice"));
    let s = render(&app);
    assert!(s.contains("changed by someone else"), "{s}");
    assert!(app.note.is_none(), "the input is closed");
    // reloaded: a retry now starts from the new note and succeeds (reverse assertion)
    press(&mut app, &[KeyCode::Char('n')]).await;
    assert_eq!(app.note.as_ref().unwrap().text, "theirs");
    type_text(&mut app, "+mine").await;
    press(&mut app, &[KeyCode::Enter]).await;
    assert_eq!(
        e.store.get(id).await.unwrap().unwrap().note.as_deref(),
        Some("theirs+mine")
    );
}

// ------------------------------------------------------------------ panes

#[tokio::test]
async fn m13_tab_goes_through_the_panes_and_q_quits() {
    let e = env().await;
    let mut app = app_of(&e, Lang::En);
    assert_eq!(app.pane, Pane::Faxes);
    press(&mut app, &[KeyCode::Tab]).await;
    assert_eq!(app.pane, Pane::Calls);
    assert!(render(&app).contains("no calls in progress"));
    press(&mut app, &[KeyCode::Tab]).await;
    assert_eq!(app.pane, Pane::Status);
    press(&mut app, &[KeyCode::Tab]).await;
    assert_eq!(app.pane, Pane::Faxes);
    press(&mut app, &[KeyCode::BackTab]).await;
    assert_eq!(app.pane, Pane::Status);
    assert!(!app.quit);
    press(&mut app, &[KeyCode::Char('q')]).await;
    assert!(app.quit);
    // Ctrl+C quits as well
    let mut app = app_of(&e, Lang::En);
    app.on_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL))
        .await;
    assert!(app.quit);
}

// ------------------------------------------------------------------ events

#[tokio::test]
async fn m13_events_update_the_calls_pane_and_the_fax_list() {
    let e = env().await;
    let mut app = app_of(&e, Lang::En);
    app.reload().await;
    press(&mut app, &[KeyCode::Tab]).await;

    app.apply_live(Live::Event(Event::CallStarted {
        call: call("call-1"),
    }))
    .await;
    let s = render(&app);
    assert!(
        s.contains("Calls (1)") && s.contains("call-1") && s.contains("0212345678"),
        "{s}"
    );
    assert!(row_of(&s, "call-1").contains("no"), "not answered yet");

    app.apply_live(Live::Event(Event::CallAnswered {
        call_id: "call-1".into(),
    }))
    .await;
    app.apply_live(Live::Event(Event::FaxStarted {
        call_id: "call-1".into(),
    }))
    .await;
    app.apply_live(Live::Event(Event::FaxProgress {
        call_id: "call-1".into(),
        pages: 3,
    }))
    .await;
    let c = &app.calls[0];
    assert!(c.answered && c.t38 && c.pages == 3);
    let s = render(&app);
    assert!(row_of(&s, "call-1").contains("yes"), "{s}");
    assert!(row_of(&s, "call-1").trim_end().ends_with('3'), "{s}");

    // the recorder writes the row, then the call ends: the call goes, the fax appears
    add(&e.store, "call-1", "0212345678", 0).await;
    app.apply_live(Live::Event(Event::CallFinished {
        call_id: "call-1".into(),
        status: "completed".into(),
        pages: 3,
        fax_status: "received".into(),
        hangup_code: "completed".into(),
    }))
    .await;
    assert!(app.calls.is_empty());
    let s = render(&app);
    assert!(
        s.contains("Calls (0)") && s.contains("no calls in progress"),
        "{s}"
    );
    press(&mut app, &[KeyCode::Tab, KeyCode::Tab]).await;
    let s = render(&app);
    assert!(row_of(&s, "0212345678").contains("received"), "{s}");
}

#[tokio::test]
async fn m13_an_event_for_an_unknown_call_changes_nothing() {
    let e = env().await;
    let mut app = app_of(&e, Lang::En);
    app.apply_live(Live::Event(Event::FaxProgress {
        call_id: "ghost".into(),
        pages: 9,
    }))
    .await;
    app.apply_live(Live::Event(Event::CallAnswered {
        call_id: "ghost".into(),
    }))
    .await;
    assert!(app.calls.is_empty());
}

fn short_dir() -> tempfile::TempDir {
    // unix socket paths are limited to about 100 bytes
    tempfile::Builder::new()
        .prefix("sf")
        .tempdir_in("/tmp")
        .unwrap()
}

fn hub() -> Arc<Hub> {
    Hub::new(
        HubInfo {
            pid: 4242,
            version: "9.9.9".into(),
            listen: vec!["udp 127.0.0.1:5060".into()],
        },
        Duration::from_secs(300),
        Vec::new,
    )
}

/// Applies live messages until `done` holds.
async fn pump(app: &mut App, rx: &mut mpsc::UnboundedReceiver<Live>, done: impl Fn(&App) -> bool) {
    while !done(app) {
        let l = tokio::time::timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("a live message in time")
            .expect("the socket task is alive");
        app.apply_live(l).await;
    }
}

#[tokio::test]
async fn m13_a_real_control_socket_feeds_the_calls_pane_the_status_and_the_list() {
    let e = env().await;
    let dir = short_dir();
    let sock: PathBuf = dir.path().join("c.sock");
    let hub = hub();
    let server = ControlServer::bind(&sock, hub.clone()).unwrap();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(socket_loop(sock.clone(), tx));
    let mut app = app_of(&e, Lang::En);

    // connected: the service is running, with its status
    pump(&mut app, &mut rx, |a| a.live && a.status.is_some()).await;
    let s = render(&app);
    assert!(
        s.contains("service running") && !s.contains("not running"),
        "{s}"
    );
    press(&mut app, &[KeyCode::Tab, KeyCode::Tab]).await;
    let s = render(&app);
    assert!(
        s.contains("9.9.9") && s.contains("udp 127.0.0.1:5060") && s.contains("4242"),
        "{s}"
    );
    press(&mut app, &[KeyCode::Tab, KeyCode::Tab]).await;
    assert_eq!(app.pane, Pane::Calls);

    // a call starts, makes progress, ends
    hub.on_event(&started("sock-1"));
    pump(&mut app, &mut rx, |a| a.calls.len() == 1).await;
    let s = render(&app);
    assert!(s.contains("Calls (1)") && s.contains("sock-1"), "{s}");
    hub.on_event(&fax_in::CallEvent::Answered {
        call_id: "sock-1".into(),
        t38: true,
        at: SystemTime::now(),
    });
    hub.on_event(&fax_in::CallEvent::PageReceived {
        call_id: "sock-1".into(),
        pages: 2,
        at: SystemTime::now(),
    });
    pump(&mut app, &mut rx, |a| {
        a.calls.first().is_some_and(|c| c.pages == 2 && c.answered)
    })
    .await;

    add(&e.store, "sock-1", "0299990000", 0).await;
    hub.on_event(&finished("sock-1", 2, false));
    pump(&mut app, &mut rx, |a| {
        a.calls.is_empty() && !a.faxes.is_empty()
    })
    .await;
    let s = render(&app);
    assert!(s.contains("no calls in progress"), "{s}");
    press(&mut app, &[KeyCode::Tab, KeyCode::Tab]).await;
    assert!(render(&app).contains("0299990000"));

    // the service goes away: not running, the list stays
    server.shutdown().await;
    pump(&mut app, &mut rx, |a| !a.live).await;
    let s = render(&app);
    assert!(s.contains("service not running"), "{s}");
    assert!(s.contains("0299990000"));
    task.abort();
}

// ------------------------------------------------------------------ no socket

#[tokio::test]
async fn m13_without_the_socket_the_screen_says_not_running_and_the_database_still_works() {
    let e = env().await;
    let id = add(&e.store, "c1", "0211111111", 0).await;
    let dir = short_dir();
    let (tx, mut rx) = mpsc::unbounded_channel();
    let task = tokio::spawn(socket_loop(dir.path().join("none.sock"), tx));
    let mut app = app_of(&e, Lang::En);
    let first = tokio::time::timeout(Duration::from_secs(10), rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(first, Live::Down), "{first:?}");
    app.apply_live(first).await;
    task.abort();
    assert!(!app.live);
    app.reload().await;
    let s = render(&app);
    assert!(s.contains("service not running"), "{s}");
    assert!(
        s.contains("0211111111"),
        "the list is from the database:\n{s}"
    );
    press(&mut app, &[KeyCode::Tab, KeyCode::Tab]).await;
    assert!(render(&app).contains("service not running"));
    press(&mut app, &[KeyCode::Tab, KeyCode::Char('r')]).await;
    assert!(
        e.store.get(id).await.unwrap().unwrap().is_read,
        "read toggle works"
    );
}

#[tokio::test]
async fn m13_without_a_database_the_screen_says_so_and_the_keys_are_harmless() {
    let mut app = App::new(None, "cli:t".into(), Lang::En);
    app.reload().await;
    press(
        &mut app,
        &[KeyCode::Char('r'), KeyCode::Char('n'), KeyCode::Enter],
    )
    .await;
    let s = render(&app);
    assert!(s.contains("database not reachable"), "{s}");
    assert!(s.contains("service not running"));
    // reverse: with a store the notice is gone
    let e = env().await;
    app.set_store(e.store.clone());
    assert!(!render(&app).contains("database not reachable"));
}

// ------------------------------------------------------------------ language

#[tokio::test]
async fn m13_zh_tw_shows_chinese_labels_and_english_does_not() {
    let e = env().await;
    add(&e.store, "c1", "0211111111", 0).await;
    let mut zh = app_of(&e, Lang::from_lang_var(Some("zh_TW.UTF-8")));
    zh.reload().await;
    let s = squash(&render(&zh));
    for want in ["傳真", "通話", "狀態", "未讀", "服務未執行", "切換已讀"] {
        assert!(s.contains(want), "{want} missing:\n{s}");
    }
    assert!(!s.contains("UNREAD") && !s.contains("notrunning"));
    press(&mut zh, &[KeyCode::Enter]).await;
    let s = squash(&render(&zh));
    assert!(s.contains("掛斷原因") && s.contains("投遞"), "{s}");

    let mut en = app_of(&e, Lang::from_lang_var(Some("en_US.UTF-8")));
    en.reload().await;
    let s = render(&en);
    assert!(s.contains("Faxes") && s.contains("UNREAD") && s.contains("service not running"));
    assert!(!squash(&s).contains("傳真"), "{s}");
}

#[tokio::test]
async fn m13_the_status_helper_of_a_missing_service_is_an_error_not_a_panic() {
    let r = crate::service::control::fetch_status(Path::new("/nonexistent/x.sock")).await;
    assert!(r.is_err());
}
