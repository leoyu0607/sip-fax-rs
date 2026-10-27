//! `GET /api/v1/events` (spec 4, V01-V06): the WebSocket that pushes [`crate::events`].
//!
//! On connect the server sends `{"type": "hello", "seq": <last>}`, then either the events after
//! `?since=` or `{"type": "resync", "seq": <last>}`, then every new event. It pings every
//! [`AppState::ws_ping`] and closes a connection that has not answered for twice that, one that
//! falls [`crate::events::LAG`] events behind (4008), and every connection of an account whose
//! sessions ended (4001).

use std::sync::Arc;
use std::time::Duration;

use accounts::Capability;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Query, State};
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use axum::routing::get;
use axum::{Router, extract::ws::rejection::WebSocketUpgradeRejection};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::broadcast::error::RecvError;
use tokio::time::Instant;

use crate::error::ApiError;
use crate::events::{Backlog, EventBus};
use crate::session::Session;
use crate::state::AppState;

/// The ping interval of production.
pub const PING_EVERY: Duration = Duration::from_secs(30);
/// A frame that cannot be written in this time ends the connection.
pub const SEND_TIMEOUT: Duration = Duration::from_secs(10);
/// The largest frame a client may send (it has nothing to say but pongs).
pub const MAX_CLIENT_FRAME: usize = 4096;

/// Close code: the account's sessions ended.
pub const CLOSE_REVOKED: u16 = 4001;
/// Close code: no pong in time.
pub const CLOSE_NO_PONG: u16 = 4002;
/// Close code: the client did not keep up.
pub const CLOSE_TOO_SLOW: u16 = 4008;

/// The routes.
pub fn routes() -> Router<AppState> {
    Router::new().route("/events", get(events))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EventsQuery {
    since: Option<u64>,
}

/// A browser always sends `Origin` with a WebSocket handshake: it has to name this server.
/// (The session cookie is `SameSite=Strict` as well; this is the second lock.)
fn same_origin(h: &HeaderMap) -> Result<(), ApiError> {
    let Some(origin) = h.get(header::ORIGIN) else {
        return Ok(());
    };
    let host = h.get(header::HOST).and_then(|v| v.to_str().ok());
    let authority = origin
        .to_str()
        .ok()
        .and_then(|o| o.split_once("://"))
        .map(|(_, a)| a);
    match (authority, host) {
        (Some(a), Some(h)) if a.eq_ignore_ascii_case(h) => Ok(()),
        _ => Err(ApiError::new(
            StatusCode::FORBIDDEN,
            "origin",
            "the WebSocket must be opened from this server's pages",
        )),
    }
}

async fn events(
    State(st): State<AppState>,
    s: Session,
    headers: HeaderMap,
    q: Result<Query<EventsQuery>, axum::extract::rejection::QueryRejection>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> Result<Response, ApiError> {
    s.need(Capability::ViewFaxes)?;
    same_origin(&headers)?;
    let Query(q) = q.map_err(|_| ApiError::bad_request("`since` must be a sequence number"))?;
    let ws = ws.map_err(|_| ApiError::bad_request("a WebSocket upgrade is expected"))?;
    let user = s.0.user.id;
    let bus = st.events.clone();
    let ping = st.ws_ping;
    Ok(ws
        .max_message_size(MAX_CLIENT_FRAME)
        .max_frame_size(MAX_CLIENT_FRAME)
        .on_upgrade(move |sock| connection(sock, bus, user, q.since, ping)))
}

async fn send(sock: &mut WebSocket, m: Message) -> bool {
    matches!(
        tokio::time::timeout(SEND_TIMEOUT, sock.send(m)).await,
        Ok(Ok(()))
    )
}

async fn close(sock: &mut WebSocket, code: u16, reason: &'static str) {
    let _ = send(
        sock,
        Message::Close(Some(CloseFrame {
            code,
            reason: reason.into(),
        })),
    )
    .await;
}

async fn connection(
    mut sock: WebSocket,
    bus: Arc<EventBus>,
    user: i64,
    since: Option<u64>,
    ping_every: Duration,
) {
    // revocations first: one that comes while the backlog is sent is not missed
    let mut revoked = bus.revocations();
    let (mut live, backlog, last) = bus.attach(since);
    let hello = json!({"type": "hello", "seq": last}).to_string();
    if !send(&mut sock, Message::Text(hello.into())).await {
        return;
    }
    match backlog {
        Backlog::Events(v) => {
            for e in v {
                if !send(&mut sock, Message::Text(e.json.clone().into())).await {
                    return;
                }
            }
        }
        Backlog::Resync => {
            let m = json!({"type": "resync", "seq": last}).to_string();
            if !send(&mut sock, Message::Text(m.into())).await {
                return;
            }
        }
    }
    let mut last_pong = Instant::now();
    let mut ping = tokio::time::interval_at(Instant::now() + ping_every, ping_every);
    loop {
        tokio::select! {
            m = sock.recv() => match m {
                Some(Ok(Message::Pong(_))) => last_pong = Instant::now(),
                // a ping is answered by the library; text from the client means nothing
                Some(Ok(Message::Ping(_) | Message::Text(_) | Message::Binary(_))) => {}
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => return,
            },
            e = live.recv() => match e {
                Ok(e) => {
                    if !send(&mut sock, Message::Text(e.json.clone().into())).await {
                        return;
                    }
                }
                Err(RecvError::Lagged(_)) => {
                    close(&mut sock, CLOSE_TOO_SLOW, "too slow: reconnect with since").await;
                    return;
                }
                Err(RecvError::Closed) => return,
            },
            r = revoked.recv() => match r {
                Ok(id) if id != user => {}
                // (lagged: one of the missed ones may have been this account)
                Ok(_) | Err(RecvError::Lagged(_)) => {
                    close(&mut sock, CLOSE_REVOKED, "the session ended").await;
                    return;
                }
                Err(RecvError::Closed) => return,
            },
            _ = ping.tick() => {
                if last_pong.elapsed() > ping_every * 2 {
                    close(&mut sock, CLOSE_NO_PONG, "no pong").await;
                    return;
                }
                if !send(&mut sock, Message::Ping(Vec::new().into())).await {
                    return;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
