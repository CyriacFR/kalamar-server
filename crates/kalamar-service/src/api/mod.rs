//! HTTP handlers, one module per area of `api.md`.

pub mod account;
pub mod friends;
pub mod meta;
pub mod moderation;
pub mod parties;
pub mod population;
pub mod queues;
pub mod ratings;
pub mod rooms;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult, JsonBody, RawJson};
use crate::state::AppState;

/// `GET /v1/matches/current`.
pub async fn match_current(State(st): State<AppState>, a: Authed) -> Json<Value> {
    let core = st.lock();
    let m = core
        .live
        .user_match
        .get(&a.user)
        .and_then(|id| core.live.matches.get(id))
        .map(|m| crate::matches::match_json(&core, m, Some(&a.user)))
        .unwrap_or(Value::Null);
    Json(json!({"match": m}))
}

/// `GET /v1/matches/{match_id}`: participants always; others an ended match's public view (with `stats`).
pub async fn match_get(State(st): State<AppState>, a: Authed, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let core = st.lock();
    let m = crate::matches::load(&core, &id)?.ok_or_else(|| ApiError::not_found("no such match"))?;
    if !m.involves(&a.user)
        && (!m.state.ended() || !core.cfg.features.stats || m.state == crate::state::MState::Cancelled)
    {
        return Err(ApiError::not_found("no such match"));
    }
    Ok(Json(crate::matches::match_json(&core, &m, Some(&a.user))))
}

/// `POST /v1/matches/{match_id}/accept`.
pub async fn match_accept(State(st): State<AppState>, a: Authed, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let mut core = st.lock();
    crate::matches::answer(&mut core, &id, &a.user, true).map(Json)
}

/// `POST /v1/matches/{match_id}/decline`.
pub async fn match_decline(State(st): State<AppState>, a: Authed, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let mut core = st.lock();
    crate::matches::answer(&mut core, &id, &a.user, false).map(Json)
}

/// `POST /v1/matches/{match_id}/launch`.
pub async fn match_launch(State(st): State<AppState>, a: Authed, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let mut core = st.lock();
    crate::matches::launch(&mut core, &id, &a.user).map(Json)
}

/// `POST /v1/matches/{match_id}/status`.
pub async fn match_status(
    State(st): State<AppState>,
    a: Authed,
    Path(id): Path<String>,
    RawJson(body): RawJson,
) -> ApiResult<StatusCode> {
    if !body.is_object() {
        return Err(ApiError::invalid("/", "the status file's JSON object"));
    }
    let mut core = st.lock();
    crate::matches::status(&mut core, &id, &a.user, body)?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/matches/{match_id}/result`.
pub async fn match_result(
    State(st): State<AppState>,
    a: Authed,
    Path(id): Path<String>,
    RawJson(body): RawJson,
) -> ApiResult<Json<Value>> {
    let mut core = st.lock();
    crate::matches::result(&mut core, &id, &a.user, body).map(Json)
}

#[derive(Deserialize)]
pub struct ClockBody {
    advance_s: i64,
}

/// `POST /v1/_test/clock` (servers that declare `test-clock` only).
pub async fn test_clock(State(st): State<AppState>, JsonBody(b): JsonBody<ClockBody>) -> ApiResult<Json<Value>> {
    if !st.cfg().features.test_clock || st.cfg().server.production {
        return Err(ApiError::feature_disabled());
    }
    if !(0..=7 * 86_400).contains(&b.advance_s) {
        return Err(ApiError::invalid("/advance_s", "0 to 604800"));
    }
    st.clock.advance(b.advance_s);
    crate::app::tick(&st);
    Ok(Json(json!({"time": crate::util::rfc3339(st.clock.now())})))
}
