//! Drawing: `draw(frame, &App)` turns the state into a screen. No I/O.

use chrono::{DateTime, Utc};
use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Row, Table, TableState, Wrap};
use store::FaxRecord;

use super::app::{App, Detail, Pane};
use super::i18n::{Key, t};
use crate::service::protocol::CallInfo;

fn ts(at: DateTime<Utc>) -> String {
    at.format("%Y-%m-%d %H:%M:%S").to_string()
}

fn opt(v: &Option<String>) -> &str {
    v.as_deref().filter(|s| !s.is_empty()).unwrap_or("-")
}

/// Draws the whole screen.
pub fn draw(frame: &mut Frame, app: &App) {
    let [head, body, foot] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    draw_header(frame, app, head);
    match app.pane {
        Pane::Faxes => match &app.detail {
            Some(d) => draw_detail(frame, app, d, body),
            None => draw_faxes(frame, app, body),
        },
        Pane::Calls => draw_calls(frame, app, body),
        Pane::Status => draw_status(frame, app, body),
    }
    draw_footer(frame, app, foot);
    if app.note.is_some() {
        draw_note(frame, app, body);
    }
}

fn draw_header(frame: &mut Frame, app: &App, area: Rect) {
    let l = app.lang;
    let tab = |pane: Pane, key: Key, extra: String| {
        let text = format!(" {}{extra} ", t(l, key));
        if app.pane == pane {
            Span::styled(text, Style::new().add_modifier(Modifier::REVERSED))
        } else {
            Span::raw(text)
        }
    };
    let mut spans = vec![
        tab(Pane::Faxes, Key::TabFaxes, String::new()),
        tab(
            Pane::Calls,
            Key::TabCalls,
            format!(" ({})", app.calls.len()),
        ),
        tab(Pane::Status, Key::TabStatus, String::new()),
        Span::raw("  "),
    ];
    spans.push(Span::raw(if app.live {
        match app.status.as_ref().map(|s| s.state.as_str()) {
            Some("stopping") => t(l, Key::ServiceStopping),
            _ => t(l, Key::ServiceRunning),
        }
    } else {
        t(l, Key::ServiceNotRunning)
    }));
    if !app.has_store() {
        spans.push(Span::raw(format!("  [{}]", t(l, Key::DbUnavailable))));
    }
    if app.unread_only {
        spans.push(Span::raw(format!("  [{}]", t(l, Key::UnreadOnly))));
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

fn draw_footer(frame: &mut Frame, app: &App, area: Rect) {
    let l = app.lang;
    let help = if app.note.is_some() {
        t(l, Key::FooterNote)
    } else {
        match (app.pane, app.detail.is_some()) {
            (Pane::Faxes, false) => t(l, Key::FooterList),
            (Pane::Faxes, true) => t(l, Key::FooterDetail),
            (Pane::Calls, _) => t(l, Key::FooterCalls),
            (Pane::Status, _) => t(l, Key::FooterStatus),
        }
    };
    let text = match &app.message {
        Some(m) => format!("{m}  |  {help}"),
        None => help.to_string(),
    };
    frame.render_widget(Paragraph::new(text), area);
}

fn state_label(app: &App, r: &FaxRecord) -> &'static str {
    t(app.lang, if r.is_read { Key::Read } else { Key::Unread })
}

fn draw_faxes(frame: &mut Frame, app: &App, area: Rect) {
    let l = app.lang;
    if app.faxes.is_empty() {
        frame.render_widget(Paragraph::new(t(l, Key::NoFaxes)), area);
        return;
    }
    let head = Row::new([
        t(l, Key::ColId),
        t(l, Key::ColStarted),
        t(l, Key::ColTrunk),
        t(l, Key::ColAni),
        t(l, Key::ColDnis),
        t(l, Key::ColStatus),
        t(l, Key::ColPages),
        t(l, Key::ColState),
    ])
    .style(Style::new().add_modifier(Modifier::BOLD));
    let rows = app.faxes.iter().map(|r| {
        Row::new([
            r.id.to_string(),
            ts(r.started_at),
            r.trunk.clone(),
            opt(&r.ani).to_string(),
            opt(&r.dnis).to_string(),
            r.fax_status.to_string(),
            r.pages.to_string(),
            state_label(app, r).to_string(),
        ])
        .style(if r.is_read {
            Style::new()
        } else {
            Style::new().add_modifier(Modifier::BOLD)
        })
    });
    let table = Table::new(
        rows,
        [
            Constraint::Length(7),
            Constraint::Length(19),
            Constraint::Length(12),
            Constraint::Length(14),
            Constraint::Length(14),
            Constraint::Length(10),
            Constraint::Length(4),
            Constraint::Length(8),
        ],
    )
    .header(head)
    .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
    .highlight_symbol("> ");
    let mut state = TableState::default().with_selected(Some(app.fax_sel));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_detail(frame: &mut Frame, app: &App, d: &Detail, area: Rect) {
    let l = app.lang;
    let r = &d.fax;
    let kv = |k: Key, v: String| Line::from(format!("{:<16}{v}", t(l, k)));
    let kvs = |k: &str, v: String| Line::from(format!("{k:<16}{v}"));
    let mut lines = vec![
        kvs("id", r.id.to_string()),
        kv(Key::FCallId, r.call_id.clone()),
        kv(Key::ColTrunk, r.trunk.clone()),
        kvs("ani", format!("{} ({})", opt(&r.ani), opt(&r.ani_source))),
        kvs(
            "dnis",
            format!("{} ({})", opt(&r.dnis), opt(&r.dnis_source)),
        ),
        kv(Key::ColStarted, ts(r.started_at)),
        kv(Key::FEnded, r.ended_at.map_or("-".into(), ts)),
        kv(
            Key::FDuration,
            r.duration_sec.map_or("-".into(), |s| format!("{s} s")),
        ),
        kv(Key::ColStatus, r.fax_status.to_string()),
        kv(Key::ColPages, r.pages.to_string()),
        kv(
            Key::FHangup,
            format!(
                "{} ({})",
                r.hangup_code.as_deref().unwrap_or("-"),
                r.hangup_direction
                    .map_or("-".to_string(), |h| h.to_string())
            ),
        ),
        kv(Key::FRemoteIdent, opt(&r.remote_ident).to_string()),
        kv(
            Key::FFiles,
            format!("tiff {}  pdf {}", opt(&r.tiff_path), opt(&r.pdf_path)),
        ),
        kv(
            Key::FPurged,
            r.files_purged_at.map_or(t(l, Key::No).to_string(), ts),
        ),
        kv(
            Key::FRead,
            match (r.is_read, &r.read_changed_by) {
                (true, Some(b)) => format!("{} ({b})", t(l, Key::Read)),
                (true, None) => t(l, Key::Read).to_string(),
                (false, _) => t(l, Key::Unread).to_string(),
            },
        ),
        kv(
            Key::FNote,
            match &r.note {
                Some(n) if !n.is_empty() => format!(
                    "{n} ({}, {})",
                    opt(&r.note_updated_by),
                    r.note_updated_at.map_or("-".into(), ts)
                ),
                _ => "-".into(),
            },
        ),
    ];
    if d.deliveries.is_empty() {
        lines.push(kv(Key::FDeliveries, t(l, Key::NoDeliveries).to_string()));
    } else {
        lines.push(Line::from(format!("{}:", t(l, Key::FDeliveries))));
        for a in &d.deliveries {
            lines.push(Line::from(format!(
                "  #{:<5} {} {:<9} {} {} {}",
                a.id,
                a.attempt,
                a.status,
                a.kind,
                a.target,
                a.last_error
                    .as_deref()
                    .map(|e| format!("({e})"))
                    .unwrap_or_default()
            )));
        }
    }
    let block =
        Block::new()
            .borders(Borders::ALL)
            .title(format!(" {} #{} ", t(l, Key::DetailTitle), r.id));
    frame.render_widget(
        Paragraph::new(lines)
            .block(block)
            .wrap(Wrap { trim: false }),
        area,
    );
}

fn call_row(app: &App, c: &CallInfo) -> Row<'static> {
    let l = app.lang;
    let yn = |b: bool| t(l, if b { Key::Yes } else { Key::No });
    Row::new([
        c.call_id.clone(),
        c.trunk.clone(),
        c.ani.clone(),
        c.dnis.clone(),
        c.started_at.clone(),
        yn(c.answered).to_string(),
        yn(c.t38).to_string(),
        c.pages.to_string(),
    ])
}

fn draw_calls(frame: &mut Frame, app: &App, area: Rect) {
    let l = app.lang;
    if app.calls.is_empty() {
        frame.render_widget(Paragraph::new(t(l, Key::NoCalls)), area);
        return;
    }
    let head = Row::new([
        t(l, Key::FCallId),
        t(l, Key::ColTrunk),
        t(l, Key::ColAni),
        t(l, Key::ColDnis),
        t(l, Key::ColStarted),
        t(l, Key::ColAnswered),
        t(l, Key::ColT38),
        t(l, Key::ColPages),
    ])
    .style(Style::new().add_modifier(Modifier::BOLD));
    let table = Table::new(
        app.calls.iter().map(|c| call_row(app, c)),
        [
            Constraint::Length(22),
            Constraint::Length(10),
            Constraint::Length(12),
            Constraint::Length(12),
            Constraint::Length(21),
            Constraint::Length(9),
            Constraint::Length(5),
            Constraint::Length(4),
        ],
    )
    .header(head)
    .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
    .highlight_symbol("> ");
    let mut state = TableState::default().with_selected(Some(app.call_sel));
    frame.render_stateful_widget(table, area, &mut state);
}

fn draw_status(frame: &mut Frame, app: &App, area: Rect) {
    let l = app.lang;
    let lines: Vec<Line> = match (&app.status, app.live) {
        (Some(s), true) => {
            let kv = |k: Key, v: String| Line::from(format!("{:<12}{v}", t(l, k)));
            let mut v = vec![
                Line::from(if s.state == "stopping" {
                    t(l, Key::ServiceStopping)
                } else {
                    t(l, Key::ServiceRunning)
                }),
                kv(Key::SPid, s.pid.to_string()),
                kv(Key::SVersion, s.version.clone()),
                kv(Key::SStarted, s.started_at.clone()),
                kv(Key::SUptime, super::format_uptime(s.uptime_secs)),
                kv(Key::SListen, s.listen.join(", ")),
                kv(Key::SCalls, app.calls.len().to_string()),
            ];
            if s.trunks.is_empty() {
                v.push(kv(Key::STrunks, t(l, Key::Nothing).to_string()));
            } else {
                v.push(Line::from(format!("{}:", t(l, Key::STrunks))));
                for tr in &s.trunks {
                    v.push(Line::from(format!("  {}: {}", tr.name, tr.registration)));
                }
            }
            v
        }
        _ => vec![Line::from(t(l, Key::ServiceNotRunning))],
    };
    frame.render_widget(Paragraph::new(lines), area);
}

fn draw_note(frame: &mut Frame, app: &App, area: Rect) {
    let Some(edit) = &app.note else { return };
    let w = area.width.saturating_sub(4).max(10);
    let rect = Rect {
        x: area.x + 2.min(area.width.saturating_sub(1)),
        y: area.y + area.height.saturating_sub(3) / 2,
        width: w.min(area.width),
        height: 3.min(area.height),
    };
    frame.render_widget(Clear, rect);
    let block = Block::new().borders(Borders::ALL).title(format!(
        " {} #{} ",
        t(app.lang, Key::NoteEditTitle),
        edit.fax_id
    ));
    frame.render_widget(Paragraph::new(format!("{}_", edit.text)).block(block), rect);
}
