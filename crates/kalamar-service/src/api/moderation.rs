//! Moderation (`api.md`, "Moderation"): reports and blocks. Bans are applied by the operator with the admin command
//! line and checked on every request.

use std::time::Instant;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult, JsonBody};
use crate::state::{AppState, Core};
use crate::util::{new_id, rfc3339};

fn need(core: &Core) -> ApiResult<()> {
    if core.cfg.features.moderation {
        Ok(())
    } else {
        Err(ApiError::feature_disabled())
    }
}

#[derive(Deserialize)]
pub struct ReportBody {
    user_id: String,
    #[serde(default)]
    match_id: Option<String>,
    category: String,
    #[serde(default)]
    comment: String,
}

/// `POST /v1/reports`.
pub async fn report(
    State(st): State<AppState>,
    a: Authed,
    JsonBody(b): JsonBody<ReportBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    {
        let core = st.lock();
        need(&core)?;
    }
    if let Err(retry) = st.limits.reports.take(&a.user, Instant::now()) {
        return Err(ApiError::rate_limited(retry));
    }
    let core = st.lock();
    if !core.cfg.moderation.report_categories.iter().any(|c| c.id == b.category) {
        return Err(ApiError::invalid(
            "/category",
            "not one of moderation.report_categories",
        ));
    }
    if b.comment.chars().count() > 1000 {
        return Err(ApiError::invalid("/comment", "at most 1000 characters"));
    }
    if b.user_id == a.user || !core.user_exists(&b.user_id) {
        return Err(ApiError::not_found("no such user"));
    }
    if let Some(m) = &b.match_id {
        let known = core
            .db
            .query_row("SELECT 1 FROM matches WHERE id = ?1", [m], |_| Ok(()))
            .optional()?
            .is_some();
        if !known {
            return Err(ApiError::not_found("no such match"));
        }
    }
    let id = new_id("rp");
    core.db.execute(
        "INSERT INTO reports (id, reporter, target, match_id, category, comment, created_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![id, a.user, b.user_id, b.match_id, b.category, b.comment, core.now],
    )?;
    tracing::info!(report = %id, category = %b.category, "report filed");
    core.emit(
        "report.created",
        json!({"id": id, "category": b.category, "target": b.user_id, "match_id": b.match_id}),
    );
    Ok((
        StatusCode::CREATED,
        Json(json!({"id": id, "created_at": rfc3339(core.now)})),
    ))
}

/// `GET /v1/blocks`.
pub async fn blocks(State(st): State<AppState>, a: Authed) -> ApiResult<Json<Value>> {
    let core = st.lock();
    need(&core)?;
    let mut s = core
        .db
        .prepare("SELECT blocked, created_at FROM blocks WHERE blocker = ?1 ORDER BY created_at")?;
    let rows: Vec<(String, i64)> = s
        .query_map([&a.user], |r| Ok((r.get(0)?, r.get(1)?)))?
        .flatten()
        .collect();
    let items: Vec<Value> = rows
        .iter()
        .map(|(u, at)| json!({"user": core.user_json(u), "since": rfc3339(*at)}))
        .collect();
    Ok(Json(json!({"items": items})))
}

/// `PUT /v1/blocks/{user_id}`: also ends a friendship and pending requests between the two.
pub async fn block(State(st): State<AppState>, a: Authed, Path(other): Path<String>) -> ApiResult<StatusCode> {
    let mut core = st.lock();
    need(&core)?;
    if other == a.user || !core.user_exists(&other) {
        return Err(ApiError::not_found("no such user"));
    }
    core.db.execute(
        "INSERT OR IGNORE INTO blocks (blocker, blocked, created_at) VALUES (?1, ?2, ?3)",
        params![a.user, other, core.now],
    )?;
    core.db.execute(
        "DELETE FROM friend_requests WHERE (from_user = ?1 AND to_user = ?2) OR (from_user = ?2 AND to_user = ?1)",
        params![a.user, other],
    )?;
    crate::api::friends::unfriend(&mut core, &a.user, &other)?;
    core.live
        .party_invites
        .retain(|(_, u), (from, _)| !((u == &a.user && from == &other) || (u == &other && from == &a.user)));
    core.live
        .room_invites
        .retain(|(_, u), (from, _)| !((u == &a.user && from == &other) || (u == &other && from == &a.user)));
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v1/blocks/{user_id}`.
pub async fn unblock(State(st): State<AppState>, a: Authed, Path(other): Path<String>) -> ApiResult<StatusCode> {
    let core = st.lock();
    need(&core)?;
    let n = core.db.execute(
        "DELETE FROM blocks WHERE blocker = ?1 AND blocked = ?2",
        params![a.user, other],
    )?;
    if n == 0 {
        Err(ApiError::not_found("not blocked"))
    } else {
        Ok(StatusCode::NO_CONTENT)
    }
}
