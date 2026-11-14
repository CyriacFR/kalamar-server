//! Friends and presence (`api.md`, "Friends and presence").

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
use crate::util::{format_friend_code, new_code, new_id, rfc3339};

fn need(core: &Core) -> ApiResult<()> {
    if core.cfg.features.friends {
        Ok(())
    } else {
        Err(ApiError::feature_disabled())
    }
}

pub fn friend_json(core: &Core, me: &str, other: &str, since: i64) -> Value {
    let mut f = json!({"kind": "friend", "user": core.user_json(other), "since": rfc3339(since)});
    if core.cfg.features.presence {
        f["presence"] = json!(core.presence_of(other));
    }
    let _ = me;
    f
}

fn since_of(core: &Core, a: &str, b: &str) -> i64 {
    let (x, y) = if a < b { (a, b) } else { (b, a) };
    core.db
        .query_row(
            "SELECT created_at FROM friendships WHERE user_a = ?1 AND user_b = ?2",
            params![x, y],
            |r| r.get(0),
        )
        .unwrap_or(core.now)
}

/// `GET /v1/friends`.
pub async fn list(State(st): State<AppState>, a: Authed) -> ApiResult<Json<Value>> {
    let core = st.lock();
    need(&core)?;
    let mut items: Vec<Value> = core
        .friends_of(&a.user)
        .iter()
        .map(|f| friend_json(&core, &a.user, f, since_of(&core, &a.user, f)))
        .collect();
    items.sort_by(|x, y| {
        x["user"]["display_name"]
            .as_str()
            .cmp(&y["user"]["display_name"].as_str())
    });
    Ok(Json(json!({"items": items})))
}

fn make_friends(core: &mut Core, a: &str, b: &str) -> ApiResult<()> {
    let max = core.cfg.limits.friends_max;
    if core.friends_of(a).len() >= max || core.friends_of(b).len() >= max {
        return Err(ApiError::conflict("friend_limit", "too many friends"));
    }
    let (x, y) = if a < b { (a, b) } else { (b, a) };
    core.db.execute(
        "INSERT OR IGNORE INTO friendships (user_a, user_b, created_at) VALUES (?1, ?2, ?3)",
        params![x, y, core.now],
    )?;
    core.db.execute(
        "DELETE FROM friend_requests WHERE (from_user = ?1 AND to_user = ?2) OR (from_user = ?2 AND to_user = ?1)",
        params![a, b],
    )?;
    let fa = friend_json(core, a, b, core.now);
    let fb = friend_json(core, b, a, core.now);
    core.push(a, "friend.added", fa);
    core.push(b, "friend.added", fb);
    Ok(())
}

fn request_json(core: &Core, id: &str, other: &str, at: i64) -> Value {
    json!({"kind": "request", "id": id, "user": core.user_json(other), "created_at": rfc3339(at)})
}

/// `GET /v1/friends/requests`.
pub async fn requests(State(st): State<AppState>, a: Authed) -> ApiResult<Json<Value>> {
    let core = st.lock();
    need(&core)?;
    let ttl = core.cfg.limits.friend_request_ttl_s;
    let load = |sql: &str| -> ApiResult<Vec<Value>> {
        let mut s = core.db.prepare(sql)?;
        let rows: Vec<(String, String, i64)> = s
            .query_map(params![a.user, core.now - ttl], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })?
            .flatten()
            .collect();
        Ok(rows.iter().map(|(id, o, at)| request_json(&core, id, o, *at)).collect())
    };
    let incoming = load(
        "SELECT id, from_user, created_at FROM friend_requests WHERE to_user = ?1 AND created_at > ?2 ORDER BY created_at",
    )?;
    let outgoing = load(
        "SELECT id, to_user, created_at FROM friend_requests WHERE from_user = ?1 AND created_at > ?2 ORDER BY created_at",
    )?;
    Ok(Json(json!({"incoming": incoming, "outgoing": outgoing})))
}

#[derive(Deserialize)]
pub struct SendBody {
    friend_code: String,
}

/// `POST /v1/friends/requests`.
pub async fn send(
    State(st): State<AppState>,
    a: Authed,
    JsonBody(body): JsonBody<SendBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    if let Err(retry) = st.limits.friend_requests.take(&a.user, Instant::now()) {
        return Err(ApiError::rate_limited(retry));
    }
    let mut core = st.lock();
    need(&core)?;
    let code = kalamar_protocol::contract::normalize_code(&body.friend_code);
    if code.len() != 12 {
        return Err(ApiError::not_found("no player has this friend code"));
    }
    let target: Option<(String, i64)> = core
        .db
        .query_row(
            "SELECT id, allow_friend_requests FROM users WHERE friend_code = ?1",
            [&code],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((target, allow)) = target else {
        return Err(ApiError::not_found("no player has this friend code"));
    };
    if target == a.user {
        return Err(ApiError::invalid("/friend_code", "this is your own friend code"));
    }
    if core.are_friends(&a.user, &target) {
        return Err(ApiError::conflict("already_friends", "you are already friends"));
    }
    if core.blocked_between(&a.user, &target) || allow == 0 {
        return Err(ApiError::forbidden(
            "blocked",
            "this player does not accept your request",
        ));
    }
    let ttl = core.cfg.limits.friend_request_ttl_s;
    core.db
        .execute("DELETE FROM friend_requests WHERE created_at <= ?1", [core.now - ttl])?;
    // The other player already asked: it becomes a friendship.
    if core
        .db
        .query_row(
            "SELECT 1 FROM friend_requests WHERE from_user = ?1 AND to_user = ?2",
            params![target, a.user],
            |_| Ok(()),
        )
        .optional()?
        .is_some()
    {
        make_friends(&mut core, &a.user, &target)?;
        let f = friend_json(&core, &a.user, &target, core.now);
        return Ok((StatusCode::OK, Json(f)));
    }
    if core
        .db
        .query_row(
            "SELECT 1 FROM friend_requests WHERE from_user = ?1 AND to_user = ?2",
            params![a.user, target],
            |_| Ok(()),
        )
        .optional()?
        .is_some()
    {
        return Err(ApiError::conflict("request_exists", "you already sent a request"));
    }
    let pending: i64 = core.db.query_row(
        "SELECT COUNT(*) FROM friend_requests WHERE from_user = ?1",
        [&a.user],
        |r| r.get(0),
    )?;
    if pending as usize >= core.cfg.limits.friends_max {
        return Err(ApiError::conflict("friend_limit", "too many pending requests"));
    }
    let id = new_id("fr");
    core.db.execute(
        "INSERT INTO friend_requests (id, from_user, to_user, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![id, a.user, target, core.now],
    )?;
    let incoming = request_json(&core, &id, &a.user, core.now);
    core.push(&target, "friend.request", incoming);
    Ok((StatusCode::CREATED, Json(request_json(&core, &id, &target, core.now))))
}

fn request_row(core: &Core, id: &str) -> ApiResult<(String, String)> {
    core.db
        .query_row(
            "SELECT from_user, to_user FROM friend_requests WHERE id = ?1 AND created_at > ?2",
            params![id, core.now - core.cfg.limits.friend_request_ttl_s],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?
        .ok_or_else(|| ApiError::not_found("no such request"))
}

/// `POST /v1/friends/requests/{id}/accept`.
pub async fn accept(State(st): State<AppState>, a: Authed, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let mut core = st.lock();
    need(&core)?;
    let (from, to) = request_row(&core, &id)?;
    if to != a.user {
        return Err(ApiError::not_found("no such request"));
    }
    make_friends(&mut core, &a.user, &from)?;
    core.refresh_presence(&a.user);
    core.refresh_presence(&from);
    Ok(Json(friend_json(&core, &a.user, &from, core.now)))
}

/// `POST /v1/friends/requests/{id}/decline`.
pub async fn decline(State(st): State<AppState>, a: Authed, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let core = st.lock();
    need(&core)?;
    let (_, to) = request_row(&core, &id)?;
    if to != a.user {
        return Err(ApiError::not_found("no such request"));
    }
    core.db.execute("DELETE FROM friend_requests WHERE id = ?1", [&id])?;
    Ok(StatusCode::NO_CONTENT)
}

/// `DELETE /v1/friends/requests/{id}` (cancel an outgoing request).
pub async fn cancel(State(st): State<AppState>, a: Authed, Path(id): Path<String>) -> ApiResult<StatusCode> {
    let core = st.lock();
    need(&core)?;
    let (from, _) = request_row(&core, &id)?;
    if from != a.user {
        return Err(ApiError::not_found("no such request"));
    }
    core.db.execute("DELETE FROM friend_requests WHERE id = ?1", [&id])?;
    Ok(StatusCode::NO_CONTENT)
}

pub fn unfriend(core: &mut Core, a: &str, b: &str) -> ApiResult<bool> {
    let (x, y) = if a < b { (a, b) } else { (b, a) };
    let n = core.db.execute(
        "DELETE FROM friendships WHERE user_a = ?1 AND user_b = ?2",
        params![x, y],
    )?;
    if n > 0 {
        core.push(a, "friend.removed", json!({"user_id": b}));
        core.push(b, "friend.removed", json!({"user_id": a}));
    }
    Ok(n > 0)
}

/// `DELETE /v1/friends/{user_id}`.
pub async fn remove(State(st): State<AppState>, a: Authed, Path(other): Path<String>) -> ApiResult<StatusCode> {
    let mut core = st.lock();
    need(&core)?;
    if unfriend(&mut core, &a.user, &other)? {
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(ApiError::not_found("not a friend"))
    }
}

/// `POST /v1/me/friend-code`.
pub async fn rotate_code(State(st): State<AppState>, a: Authed) -> ApiResult<Json<Value>> {
    let core = st.lock();
    need(&core)?;
    let mut code = new_code(12);
    while core
        .db
        .query_row("SELECT 1 FROM users WHERE friend_code = ?1", [&code], |_| Ok(()))
        .optional()?
        .is_some()
    {
        code = new_code(12);
    }
    core.db
        .execute("UPDATE users SET friend_code = ?1 WHERE id = ?2", params![code, a.user])?;
    Ok(Json(json!({"friend_code": format_friend_code(&code)})))
}
