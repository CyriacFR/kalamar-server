//! The operator API (protocol 1.1, `operators.md` section 2): bearer tokens of the operator's tools (a Discord bot,
//! a statistics site, a referee), configured as SHA-256 hashes with scopes. Never used by launchers.

use axum::Json;
use axum::extract::{FromRequestParts, Path, Query, State};
use axum::http::StatusCode;
use axum::http::request::Parts;
use kalamar_protocol::Team;
use rusqlite::params;
use serde::Deserialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;

use crate::error::{ApiError, ApiResult, JsonBody};
use crate::state::AppState;
use crate::util::{hex, parse_rfc3339, rfc3339};

/// An authenticated operator tool.
pub struct Operator {
    pub name: String,
    pub scopes: Vec<String>,
}

impl Operator {
    pub(crate) fn need(&self, scope: &str) -> ApiResult<()> {
        if self.scopes.iter().any(|s| s == scope) {
            Ok(())
        } else {
            Err(ApiError::forbidden(
                "forbidden",
                format!("this token lacks the `{scope}` scope"),
            ))
        }
    }
}

impl FromRequestParts<AppState> for Operator {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> ApiResult<Self> {
        if st.cfg().operator_tokens.is_empty() {
            return Err(ApiError::feature_disabled());
        }
        let token = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer ").or_else(|| v.strip_prefix("bearer ")))
            .ok_or_else(|| ApiError::unauthorized("unauthorized", "missing operator token"))?;
        let hash = hex(&Sha256::digest(token.trim().as_bytes()));
        st.cfg()
            .operator_tokens
            .iter()
            .find(|t| bool::from(t.sha256.to_lowercase().as_bytes().ct_eq(hash.as_bytes())))
            .map(|t| Operator {
                name: t.name.clone(),
                scopes: t.scopes.clone(),
            })
            .ok_or_else(|| ApiError::unauthorized("unauthorized", "unknown operator token"))
    }
}

#[derive(Deserialize)]
pub struct ListQuery {
    state: Option<String>,
    cursor: Option<String>,
    limit: Option<String>,
}

fn page(q: &ListQuery) -> ApiResult<(i64, i64)> {
    let limit = q
        .limit
        .as_deref()
        .map(|l| l.parse::<i64>().ok().filter(|n| (1..=100).contains(n)))
        .unwrap_or(Some(20));
    let offset = q
        .cursor
        .as_deref()
        .map(|c| c.strip_prefix('o').and_then(|n| n.parse::<i64>().ok()))
        .unwrap_or(Some(0));
    match (limit, offset) {
        (Some(l), Some(o)) if o >= 0 => Ok((o, l)),
        (None, _) => Err(ApiError::invalid("/limit", "1 to 100")),
        _ => Err(ApiError::bad_request("bad_cursor", "unknown cursor")),
    }
}

/// `GET /v1/operator/matches` (read).
pub async fn matches(State(st): State<AppState>, op: Operator, Query(q): Query<ListQuery>) -> ApiResult<Json<Value>> {
    op.need("read")?;
    let (offset, limit) = page(&q)?;
    let core = st.lock();
    let mut s = core.db.prepare(
        "SELECT id, state, origin_kind, origin_id, created_at, ended_at FROM matches
         WHERE (?1 IS NULL OR state = ?1) ORDER BY created_at DESC, id DESC LIMIT ?2 OFFSET ?3",
    )?;
    let rows: Vec<Value> = s
        .query_map(params![q.state, limit + 1, offset], |r| {
            Ok(json!({"id": r.get::<_, String>(0)?, "state": r.get::<_, String>(1)?,
                      "origin": {"kind": r.get::<_, String>(2)?, "id": r.get::<_, String>(3)?},
                      "created_at": rfc3339(r.get(4)?), "ended_at": r.get::<_, Option<i64>>(5)?.map(rfc3339)}))
        })?
        .flatten()
        .collect();
    let more = rows.len() as i64 > limit;
    let items: Vec<Value> = rows.into_iter().take(limit as usize).collect();
    Ok(Json(
        json!({"items": items, "next_cursor": more.then(|| format!("o{}", offset + limit))}),
    ))
}

/// `GET /v1/operator/matches/{id}` (read): the match with every report.
pub async fn match_detail(State(st): State<AppState>, op: Operator, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    op.need("read")?;
    let core = st.lock();
    let m = crate::matches::load(&core, &id)?.ok_or_else(|| ApiError::not_found("no such match"))?;
    let mut v = crate::matches::match_json(&core, &m, None);
    v["reports"] =
        json!(m.seats.iter().map(|s| json!({
        "user_id": s.user_id, "member_name": s.member_name, "team": s.team.as_str(), "role": s.role.as_str(),
        "launched_at": s.launched_at.map(rfc3339), "status": s.status_state, "report": s.report,
    })).collect::<Vec<_>>());
    Ok(Json(v))
}

#[derive(Deserialize)]
pub struct DecisionBody {
    counted: bool,
    #[serde(default)]
    winner: Option<String>,
    #[serde(default)]
    rated: bool,
    #[serde(default)]
    reason: Option<Value>,
}

/// `POST /v1/operator/matches/{id}/decision` (results).
pub async fn decide(
    State(st): State<AppState>,
    op: Operator,
    Path(id): Path<String>,
    JsonBody(b): JsonBody<DecisionBody>,
) -> ApiResult<Json<Value>> {
    op.need("results")?;
    let winner = match b.winner.as_deref() {
        None => None,
        Some(w) => Some(Team::parse(w).ok_or_else(|| ApiError::invalid("/winner", "alpha, bravo or null"))?),
    };
    let mut core = st.lock();
    crate::matches::referee_decide(&mut core, &id, b.counted, winner, b.rated)?;
    let now = core.now;
    crate::db::audit(
        &core.db,
        now,
        "referee-decision",
        Some(&id),
        Some(&format!("{} by {}", b.reason.unwrap_or(Value::Null), op.name)),
    )?;
    let m = crate::matches::load(&core, &id)?.ok_or_else(|| ApiError::not_found("no such match"))?;
    Ok(Json(crate::matches::match_json(&core, &m, None)))
}

/// `GET /v1/operator/reports` (moderation).
pub async fn reports(State(st): State<AppState>, op: Operator, Query(q): Query<ListQuery>) -> ApiResult<Json<Value>> {
    op.need("moderation")?;
    let (offset, limit) = page(&q)?;
    let core = st.lock();
    let status = q.state.unwrap_or_else(|| "open".into());
    let mut s = core.db.prepare(
        "SELECT id, reporter, target, match_id, category, comment, created_at, status FROM reports
         WHERE status = ?1 ORDER BY created_at LIMIT ?2 OFFSET ?3",
    )?;
    let items: Vec<Value> = s
        .query_map(params![status, limit, offset], |r| {
            Ok(json!({"id": r.get::<_, String>(0)?, "reporter": r.get::<_, Option<String>>(1)?, "target": r.get::<_, Option<String>>(2)?,
                      "match_id": r.get::<_, Option<String>>(3)?, "category": r.get::<_, String>(4)?, "comment": r.get::<_, String>(5)?,
                      "created_at": rfc3339(r.get(6)?), "status": r.get::<_, String>(7)?}))
        })?
        .flatten()
        .collect();
    Ok(Json(json!({"items": items, "next_cursor": null})))
}

#[derive(Deserialize, Default)]
pub struct NoteBody {
    #[serde(default)]
    note: String,
}

/// `POST /v1/operator/reports/{id}/resolve` (moderation).
pub async fn resolve(
    State(st): State<AppState>,
    op: Operator,
    Path(id): Path<String>,
    JsonBody(b): JsonBody<NoteBody>,
) -> ApiResult<StatusCode> {
    op.need("moderation")?;
    let core = st.lock();
    let n = core.db.execute(
        "UPDATE reports SET status = 'resolved', resolution = ?1, resolved_at = ?2 WHERE id = ?3",
        params![b.note.chars().take(1000).collect::<String>(), core.now, id],
    )?;
    if n == 0 {
        return Err(ApiError::not_found("no such report"));
    }
    crate::db::audit(&core.db, core.now, "resolve-report", Some(&id), Some(&op.name))?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct BanBody {
    #[serde(default)]
    until: Option<String>,
    #[serde(default)]
    reason: Option<Value>,
}

/// `POST /v1/operator/users/{id}/ban` (moderation): `until` RFC 3339 or null (permanent).
pub async fn ban(
    State(st): State<AppState>,
    op: Operator,
    Path(id): Path<String>,
    JsonBody(b): JsonBody<BanBody>,
) -> ApiResult<StatusCode> {
    op.need("moderation")?;
    let until = match &b.until {
        None => i64::MAX,
        Some(t) => parse_rfc3339(t).ok_or_else(|| ApiError::invalid("/until", "RFC 3339 or null"))?,
    };
    let reason = match &b.reason {
        Some(Value::String(s)) => s.clone(),
        Some(Value::Object(m)) => m.get("en").and_then(Value::as_str).unwrap_or_default().to_string(),
        _ => String::new(),
    };
    let core = st.lock();
    let n = core.db.execute(
        "UPDATE users SET banned_until = ?1, ban_reason = ?2 WHERE id = ?3",
        params![until, reason, id],
    )?;
    if n == 0 {
        return Err(ApiError::not_found("no such user"));
    }
    crate::db::audit(&core.db, core.now, "ban", Some(&id), Some(&op.name))?;
    core.emit(
        "user.banned",
        json!({"user_id": id, "until": (until != i64::MAX).then(|| rfc3339(until))}),
    );
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/operator/users/{id}/unban` (moderation).
pub async fn unban(State(st): State<AppState>, op: Operator, Path(id): Path<String>) -> ApiResult<StatusCode> {
    op.need("moderation")?;
    let core = st.lock();
    let n = core.db.execute(
        "UPDATE users SET banned_until = NULL, ban_reason = NULL WHERE id = ?1",
        [&id],
    )?;
    if n == 0 {
        return Err(ApiError::not_found("no such user"));
    }
    crate::db::audit(&core.db, core.now, "unban", Some(&id), Some(&op.name))?;
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /v1/operator/ladders/{id}/leaderboard` (read): for statistics sites (players who left leaderboards are out).
pub async fn leaderboard(
    State(st): State<AppState>,
    op: Operator,
    Path(ladder): Path<String>,
    Query(q): Query<ListQuery>,
) -> ApiResult<Json<Value>> {
    op.need("read")?;
    let (offset, limit) = page(&q)?;
    let core = st.lock();
    if core.cfg.ladder(&ladder).is_none() {
        return Err(ApiError::not_found("no such ladder"));
    }
    let season = crate::db::current_season(&core.db, core.now)?.0;
    let p = &core.cfg.rating;
    let mut s = core.db.prepare(
        "SELECT r.user_id, u.display_name, r.rating - ?4 * r.deviation, r.games, r.wins, r.losses FROM ratings r
         JOIN users u ON u.id = r.user_id WHERE r.ladder = ?1 AND r.season_id = ?2 AND r.games >= ?3
         AND u.show_on_leaderboards = 1 ORDER BY 3 DESC, r.user_id LIMIT ?5 OFFSET ?6",
    )?;
    let items: Vec<Value> = s
        .query_map(
            params![ladder, season, p.placement_games, p.display_deviations, limit, offset],
            |r| {
                Ok(
                    json!({"user_id": r.get::<_, String>(0)?, "display_name": r.get::<_, String>(1)?,
                      "rating": r.get::<_, f64>(2)?.round() as i64, "games": r.get::<_, i64>(3)?,
                      "wins": r.get::<_, i64>(4)?, "losses": r.get::<_, i64>(5)?}),
                )
            },
        )?
        .flatten()
        .collect();
    Ok(Json(json!({"ladder": ladder, "season": season, "items": items})))
}

/// The configuration lines of a new operator token (printed by `admin operator-token`).
pub fn token_line(name: &str, token: &str, scopes: &[String]) -> String {
    let hash = hex(&Sha256::digest(token.as_bytes()));
    format!("[[operator_tokens]]\nname = \"{name}\"\nsha256 = \"{hash}\"\nscopes = {scopes:?}\n")
}
