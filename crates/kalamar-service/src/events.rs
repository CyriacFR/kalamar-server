//! The event WebSocket (`events.md`): authentication by header or single-use ticket, numbered events with a replay
//! buffer, heartbeat, resumption.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::{Duration, Instant};

use axum::Json;
use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade, rejection::WebSocketUpgradeRejection};
use axum::extract::{Query, State};
use axum::http::HeaderMap;
use axum::response::{IntoResponse, Response};
use futures_util::{SinkExt, StreamExt};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::auth::{Authed, banned_error, check_access};
use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, Core, Stream};
use crate::util::{new_id, new_token, rfc3339, token_hash};

const TICKET_TTL_S: i64 = 60;
const MAX_CLIENT_FRAME: usize = 4096;

/// `POST /v1/events/ticket`.
pub async fn ticket(State(st): State<AppState>, a: Authed) -> Json<Value> {
    let mut core = st.lock();
    let t = new_token("ket1");
    let now = core.now;
    core.live.ws_tickets.retain(|_, v| v.2 > now);
    core.live
        .ws_tickets
        .insert(token_hash(&t), (a.user, a.session, now + TICKET_TTL_S));
    Json(json!({"ticket": t, "expires_at": rfc3339(now + TICKET_TTL_S)}))
}

#[derive(Deserialize)]
pub struct EventsQuery {
    ticket: Option<String>,
    resume: Option<String>,
    last_seq: Option<String>,
}

fn bearer(h: &HeaderMap) -> Option<String> {
    let v = h.get(axum::http::header::AUTHORIZATION)?.to_str().ok()?;
    let (s, t) = v.split_once(' ')?;
    s.eq_ignore_ascii_case("bearer").then(|| t.trim().to_string())
}

struct Opened {
    stream: String,
    conn: u64,
    hello: String,
    replay: Vec<Arc<str>>,
    rx: mpsc::Receiver<Arc<str>>,
}

fn open(core: &mut Core, user: &str, session: &str, q: &EventsQuery) -> Opened {
    let (tx, rx) = mpsc::channel::<Arc<str>>(1024);
    core.live.next_conn += 1;
    let conn = core.live.next_conn;
    let now = core.now;
    let last_seq = q.last_seq.as_deref().and_then(|s| s.parse::<u64>().ok());
    let mut resumed = None;
    if let (Some(id), Some(last)) = (&q.resume, last_seq)
        && let Some(s) = core.live.streams.get_mut(id)
    {
        let complete = last <= s.seq && s.buffer.front().is_none_or(|(first, _)| *first <= last + 1);
        if s.session_id == session && s.user_id == user && complete {
            let replay: Vec<Arc<str>> = s
                .buffer
                .iter()
                .filter(|(n, _)| *n > last)
                .map(|(_, f)| f.clone())
                .collect();
            if let Some(old) = s.tx.replace(tx.clone()) {
                let _ = old.try_send(Arc::from(
                    json!({"type": "bye", "reason": "replaced"}).to_string().as_str(),
                ));
            }
            s.conn = conn;
            s.closed_at = None;
            resumed = Some((id.clone(), s.seq, replay));
        }
    }
    let (stream, last, replay, was_resumed) = match resumed {
        Some((id, seq, replay)) => (id, seq, replay, true),
        None => {
            let id = new_id("es");
            core.live.streams.insert(
                id.clone(),
                Stream {
                    id: id.clone(),
                    user_id: user.to_string(),
                    session_id: session.to_string(),
                    seq: 0,
                    buffer: VecDeque::new(),
                    tx: Some(tx),
                    conn,
                    opened_at: now,
                    closed_at: None,
                },
            );
            core.live
                .user_streams
                .entry(user.to_string())
                .or_default()
                .push(id.clone());
            (id, 0, Vec::new(), false)
        }
    };
    // Connection limit per account: the oldest connected streams make room.
    let max = core.cfg.events.max_connections_per_user;
    let mut connected: Vec<(i64, String)> = core
        .live
        .user_streams
        .get(user)
        .map(|ids| {
            ids.iter()
                .filter_map(|i| core.live.streams.get(i))
                .filter(|s| s.tx.is_some())
                .map(|s| (s.opened_at, s.id.clone()))
                .collect()
        })
        .unwrap_or_default();
    connected.sort();
    while connected.len() > max {
        let (_, old) = connected.remove(0);
        if old == stream {
            continue;
        }
        if let Some(s) = core.live.streams.get_mut(&old) {
            if let Some(tx) = s.tx.take() {
                let _ = tx.try_send(Arc::from(
                    json!({"type": "bye", "reason": "replaced"}).to_string().as_str(),
                ));
            }
            s.closed_at = Some(now);
        }
    }
    core.live.offline_since.remove(user);
    core.refresh_presence(user);
    let hello = json!({
        "type": "hello", "stream": stream, "resumed": was_resumed, "last_seq": last,
        "heartbeat_s": core.cfg.events.heartbeat_s, "resume_window_s": core.cfg.events.resume_window_s,
        "protocol": kalamar_protocol::PROTOCOL_VERSION,
    })
    .to_string();
    Opened {
        stream,
        conn,
        hello,
        replay,
        rx,
    }
}

/// `GET /v1/events` (WebSocket).
pub async fn events(
    State(st): State<AppState>,
    headers: HeaderMap,
    Query(q): Query<EventsQuery>,
    ws: Result<WebSocketUpgrade, WebSocketUpgradeRejection>,
) -> ApiResult<Response> {
    let opened = {
        let mut core = st.lock();
        let (user, session) = if let Some(token) = bearer(&headers) {
            let a = check_access(&mut core, &token)?;
            (a.user, a.session)
        } else if let Some(t) = &q.ticket {
            let now = core.now;
            core.live
                .ws_tickets
                .remove(&token_hash(t))
                .filter(|v| v.2 > now)
                .map(|v| (v.0, v.1))
                .ok_or_else(|| ApiError::unauthorized("unauthorized", "unknown or expired ticket"))?
        } else {
            return Err(ApiError::unauthorized("unauthorized", "missing bearer token or ticket"));
        };
        if let Some((until, reason)) = core.ban_of(&user) {
            return Err(banned_error(until, reason, core.now));
        }
        let ws = ws.map_err(|_| ApiError::bad_request("bad_request", "a WebSocket upgrade is required"))?;
        let opened = open(&mut core, &user, &session, &q);
        (ws, opened)
    };
    let (ws, opened) = opened;
    let st2 = st.clone();
    Ok(ws
        .max_message_size(MAX_CLIENT_FRAME)
        .on_upgrade(move |socket| run(st2, socket, opened))
        .into_response())
}

/// Security review L-S1: frames a client may send on its event stream, per second and at once (a launcher sends a
/// `ping` now and then and an `ack` per event or batch of events).
const FRAME_RATE: f64 = 20.0;
const FRAME_BURST: f64 = 100.0;

async fn run(st: AppState, socket: WebSocket, o: Opened) {
    let Opened {
        stream,
        conn,
        hello,
        replay,
        mut rx,
    } = o;
    let (mut sink, mut source) = socket.split();
    let hb = Duration::from_secs(st.cfg().events.heartbeat_s);
    let mut ok = sink.send(Message::Text(hello.into())).await.is_ok();
    for f in replay {
        if !ok {
            break;
        }
        ok = sink.send(Message::Text(f.to_string().into())).await.is_ok();
    }
    let mut ping = tokio::time::interval(hb);
    ping.tick().await;
    let mut last_rx = Instant::now();
    // Security review L-S1: the client's own frames are limited per connection (each `ack` takes the service's
    // lock; one connection sent 44 772 a second). Beyond the budget the connection is closed (1008): a launcher
    // reconnects and resumes, a flood stops.
    let (mut budget, mut refilled) = (FRAME_BURST, Instant::now());
    loop {
        // `ok` only guards the start (hello and replay sent); every failure below breaks out.
        if !ok {
            break;
        }
        tokio::select! {
            frame = rx.recv() => {
                let Some(frame) = frame else { break };
                // Control frames carry no `seq`; a `bye` ends the connection after it is sent.
                let bye = !frame.contains("\"seq\"") && frame.contains("\"type\":\"bye\"");
                if sink.send(Message::Text(frame.to_string().into())).await.is_err() || bye {
                    break;
                }
            }
            msg = source.next() => {
                let Some(Ok(msg)) = msg else { break };
                last_rx = Instant::now();
                budget = (budget + last_rx.duration_since(refilled).as_secs_f64() * FRAME_RATE).min(FRAME_BURST);
                refilled = last_rx;
                if budget < 1.0 {
                    let close = CloseFrame { code: 1008, reason: "too many frames".into() };
                    let _ = sink.send(Message::Close(Some(close))).await;
                    tracing::info!("event stream closed: the client sent too many frames");
                    break;
                }
                budget -= 1.0;
                match msg {
                    Message::Text(t) => {
                        let v: Value = serde_json::from_str(t.as_str()).unwrap_or(Value::Null);
                        match v.get("type").and_then(Value::as_str) {
                            Some("ping") => {
                                let pong = json!({"type": "pong", "t": v.get("t").cloned().unwrap_or(Value::Null)}).to_string();
                                if sink.send(Message::Text(pong.into())).await.is_err() { break; }
                            }
                            Some("ack") => {
                                if let Some(seq) = v.get("seq").and_then(Value::as_u64) {
                                    let mut core = st.lock();
                                    if let Some(s) = core.live.streams.get_mut(&stream) {
                                        while s.buffer.front().is_some_and(|(n, _)| *n <= seq) {
                                            s.buffer.pop_front();
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
            _ = ping.tick() => {
                if last_rx.elapsed() > hb * 3 {
                    break;
                }
                let p = json!({"type": "ping", "t": st.clock.now()}).to_string();
                if sink.send(Message::Text(p.into())).await.is_err() { break; }
            }
        }
    }
    let _ = sink.close().await;
    let mut core = st.lock();
    let now = core.now;
    let user = match core.live.streams.get_mut(&stream) {
        Some(s) if s.conn == conn => {
            s.tx = None;
            s.closed_at = Some(now);
            Some(s.user_id.clone())
        }
        _ => None,
    };
    if let Some(u) = user
        && !core.is_connected(&u)
    {
        core.live.offline_since.insert(u, now);
    }
}

/// Forgets closed streams past the resume window and announces players gone offline after the grace period.
pub fn sweep(core: &mut Core) {
    let now = core.now;
    let window = core.cfg.events.resume_window_s;
    let stale: Vec<(String, String)> = core
        .live
        .streams
        .values()
        .filter(|s| s.tx.is_none() && s.closed_at.is_some_and(|t| now - t > window))
        .map(|s| (s.id.clone(), s.user_id.clone()))
        .collect();
    for (id, user) in stale {
        core.live.streams.remove(&id);
        if let Some(v) = core.live.user_streams.get_mut(&user) {
            v.retain(|x| x != &id);
            if v.is_empty() {
                core.live.user_streams.remove(&user);
            }
        }
    }
    let gone: Vec<String> = core
        .live
        .offline_since
        .iter()
        .filter(|(_, t)| now - **t >= crate::state::PRESENCE_GRACE_S)
        .map(|(u, _)| u.clone())
        .collect();
    for u in gone {
        core.refresh_presence(&u);
        if core.presence_of(&u) == "offline" {
            core.live.offline_since.remove(&u);
        }
    }
    core.live.ws_tickets.retain(|_, v| v.2 > now);
}
