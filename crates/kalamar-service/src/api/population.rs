//! `GET /v1/population` (area `population`, protocol 1.1.4): how many players are online, in a match and searching,
//! as three numbers. Public, cached for [`CACHE_S`] seconds, limited per address; no identity, no detail per queue or
//! room.

use std::collections::HashSet;
use std::time::Instant;

use axum::extract::State;
use axum::http::{HeaderValue, header};
use axum::response::{IntoResponse, Response};
use serde_json::{Value, json};

use crate::auth::ip_of;
use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, Core, MState};
use crate::util::rfc3339;

/// An account counts as online for this long after its last authenticated request.
pub const ACTIVITY_WINDOW_S: i64 = 120;
/// How long a computed answer is served again.
pub const CACHE_S: i64 = 10;

/// The three numbers, now.
pub fn compute(core: &Core) -> Value {
    let now = core.now;
    let mut online: HashSet<&str> = core
        .live
        .last_seen
        .iter()
        .filter(|(_, t)| now - **t <= ACTIVITY_WINDOW_S)
        .map(|(u, _)| u.as_str())
        .collect();
    online.extend(
        core.live
            .streams
            .values()
            .filter(|s| s.tx.is_some())
            .map(|s| s.user_id.as_str()),
    );
    // From the moment everyone accepted until the reports decide (the presence `in_match` of the same players).
    let in_match: HashSet<&str> = core
        .live
        .matches
        .values()
        .filter(|m| matches!(m.state, MState::Drafting | MState::Ready | MState::Running))
        .flat_map(|m| m.seats.iter().filter_map(|s| s.user_id.as_deref()))
        .collect();
    let searching: HashSet<&str> = core
        .live
        .tickets
        .values()
        .flat_map(|t| t.members.iter().map(String::as_str))
        .collect();
    json!({
        "online": online.len(),
        "in_match": in_match.len(),
        "searching": searching.len(),
        "updated_at": rfc3339(now),
    })
}

/// `GET /v1/population`: `404 not_found` when the area is not declared (as on a server that never heard of it).
pub async fn get(State(st): State<AppState>, ext: axum::http::Extensions) -> ApiResult<Response> {
    if !st.cfg().features.population {
        return Err(ApiError::not_found("no such endpoint"));
    }
    let ip = ip_of(&ext);
    st.limits
        .population
        .take(&ip, Instant::now())
        .map_err(ApiError::rate_limited)?;
    let body = {
        let mut core = st.lock();
        let now = core.now;
        match &core.live.population {
            Some((at, v)) if now - at < CACHE_S && now >= *at => v.clone(),
            _ => {
                let v = compute(&core);
                core.live.population = Some((now, v.clone()));
                v
            }
        }
    };
    let mut resp = axum::Json(body).into_response();
    resp.headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("max-age=10"));
    Ok(resp)
}
