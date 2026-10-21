//! `sip-faxd tui`: a terminal screen for the operator (M4d, spec `docs/design/m4-web.md` 7.3).
//!
//! Fax data (list, details, read flag, note) comes straight from the database, like the `fax`
//! command; calls in progress, the service status and live events come from the control socket.
//! Without the socket the screen still works on the database and says the service is not
//! running.
//!
//! * [`app`]: the state and what keys and events do to it (no I/O of its own)
//! * [`ui`]: `draw(frame, &App)`
//! * [`i18n`]: the strings, English and Traditional Chinese
//! * this file: the thin shell (terminal, key thread, socket task)

pub mod app;
pub mod i18n;
pub mod ui;

use std::path::{Path, PathBuf};
use std::process::ExitCode;
use std::time::Duration;

use tokio::sync::mpsc;

use self::app::{App, Live};
use self::i18n::Lang;
use crate::db::Db;
use crate::settings::AppConfig;

pub use crate::service::cli::format_uptime;

/// How often the status is asked for and a missing connection is retried.
const TICK: Duration = Duration::from_secs(2);

/// `sip-faxd tui`
pub fn run(cfg_path: &Path) -> ExitCode {
    crate::with_runtime(cfg_path, |cfg| async move { run_async(cfg).await })
}

async fn run_async(cfg: AppConfig) -> ExitCode {
    let base = crate::exe_dir();
    let sock = cfg.service.socket_path(&base);
    let db = Db::new(&cfg, &base);
    let store = db.store().await.ok();
    let mut app = App::new(store, crate::runtime::cli_operator(), Lang::from_env());
    app.reload().await;

    let (live_tx, mut live_rx) = mpsc::unbounded_channel::<Live>();
    let socket_task = tokio::spawn(socket_loop(sock, live_tx));
    let (key_tx, mut key_rx) = mpsc::unbounded_channel();
    std::thread::spawn(move || {
        while let Ok(ev) = crossterm::event::read() {
            if let crossterm::event::Event::Key(k) = ev
                && k.kind != crossterm::event::KeyEventKind::Release
                && key_tx.send(k).is_err()
            {
                break;
            }
        }
    });

    let mut terminal = ratatui::init();
    let mut tick = tokio::time::interval(TICK);
    let result = loop {
        if let Err(e) = terminal.draw(|f| ui::draw(f, &app)) {
            break Err(e);
        }
        if app.quit {
            break Ok(());
        }
        tokio::select! {
            Some(k) = key_rx.recv() => app.on_key(k).await,
            Some(l) = live_rx.recv() => app.apply_live(l).await,
            _ = tick.tick() => {
                if !app.has_store() && let Ok(s) = db.store().await {
                    app.set_store(s);
                }
                app.reload().await;
            }
        }
    };
    ratatui::restore();
    socket_task.abort();
    db.close().await;
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("sip-faxd: error: terminal: {e}");
            ExitCode::from(1)
        }
    }
}

/// Keeps a subscription to the control socket open (reconnecting) and asks for the status now
/// and then.
async fn socket_loop(sock: PathBuf, tx: mpsc::UnboundedSender<Live>) {
    use crate::service::control::Client;
    use crate::service::protocol::{Reply, Request};
    loop {
        if let Ok(mut c) = Client::connect(&sock).await {
            if let Ok(Reply::Subscribed { calls }) = c.call(&Request::Subscribe).await {
                if tx.send(Live::Connected(calls)).is_err() {
                    return;
                }
                let mut tick = tokio::time::interval(TICK);
                loop {
                    tokio::select! {
                        r = c.next() => match r {
                            Ok(Some(Reply::Event { event })) => {
                                if tx.send(Live::Event(event)).is_err() { return; }
                            }
                            Ok(Some(_)) => {}
                            _ => break,
                        },
                        _ = tick.tick() => {
                            if let Ok(s) = crate::service::control::fetch_status(&sock).await
                                && tx.send(Live::Status(s)).is_err()
                            {
                                return;
                            }
                        }
                    }
                }
            }
            if tx.send(Live::Down).is_err() {
                return;
            }
        } else if tx.send(Live::Down).is_err() {
            return;
        }
        tokio::time::sleep(TICK).await;
    }
}

#[cfg(test)]
mod tests;
