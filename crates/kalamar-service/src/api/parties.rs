//! Parties (`api.md`, "Parties"): groups that queue together. In memory only.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::Authed;
use crate::error::{ApiError, ApiResult, JsonBody};
use crate::state::{AppState, Core, MState, Party};
use crate::util::{new_id, rfc3339};

fn need(core: &Core) -> ApiResult<()> {
    if core.cfg.features.parties {
        Ok(())
    } else {
        Err(ApiError::feature_disabled())
    }
}

pub fn party_json(core: &Core, p: &Party) -> Value {
    let members: Vec<Value> = p
        .members
        .iter()
        .map(|(u, at)| json!({"user": core.user_json(u), "joined_at": rfc3339(*at)}))
        .collect();
    let invites: Vec<Value> = core
        .live
        .party_invites
        .iter()
        .filter(|((pid, _), (_, exp))| pid == &p.id && *exp > core.now)
        .map(|((_, u), (_, exp))| json!({"user": core.user_json(u), "expires_at": rfc3339(*exp)}))
        .collect();
    let ticket = p
        .members
        .first()
        .and_then(|(u, _)| core.live.user_ticket.get(u))
        .and_then(|t| core.live.tickets.get(t))
        .filter(|t| t.party_id.as_deref() == Some(&p.id))
        .map(|t| crate::api::queues::ticket_json(core, t))
        .unwrap_or(Value::Null);
    json!({"id": p.id, "leader": p.leader, "members": members, "invites": invites,
           "max_size": core.cfg.limits.party_size, "ticket": ticket})
}

pub fn party_of_json(core: &Core, user: &str) -> Value {
    core.live
        .user_party
        .get(user)
        .and_then(|id| core.live.parties.get(id))
        .map(|p| party_json(core, p))
        .unwrap_or(Value::Null)
}

pub fn invites_for(core: &Core, user: &str) -> Vec<Value> {
    core.live
        .party_invites
        .iter()
        .filter(|((_, u), (_, exp))| u == user && *exp > core.now)
        .map(|((pid, _), (from, exp))| json!({"party_id": pid, "from": core.user_json(from), "expires_at": rfc3339(*exp)}))
        .collect()
}

fn broadcast(core: &mut Core, party_id: &str) {
    let Some(p) = core.live.parties.get(party_id) else {
        return;
    };
    let data = party_json(core, p);
    let members: Vec<String> = p.members.iter().map(|(u, _)| u.clone()).collect();
    core.push_many(&members, "party.updated", &data);
}

fn busy(core: &Core, party_id: &str) -> bool {
    core.live.parties.get(party_id).is_some_and(|p| {
        p.members.iter().any(|(u, _)| {
            core.live
                .user_match
                .get(u)
                .and_then(|m| core.live.matches.get(m))
                .is_some_and(|m| !m.state.ended() && m.state != MState::Cancelled)
        })
    })
}

/// Removes `user` from their party (leave, kick, erasure): cancels the party's ticket, passes leadership on,
/// removes an empty party.
pub fn remove_from_party(core: &mut Core, user: &str, reason: &str) {
    let Some(pid) = core.live.user_party.remove(user) else {
        return;
    };
    crate::api::queues::cancel_party_ticket(core, &pid, "party_changed");
    let empty = match core.live.parties.get_mut(&pid) {
        Some(p) => {
            p.members.retain(|(u, _)| u != user);
            if p.leader == user
                && let Some((next, _)) = p.members.first()
            {
                p.leader = next.clone();
            }
            p.members.is_empty()
        }
        None => true,
    };
    core.push(user, "party.closed", json!({"party_id": pid, "reason": reason}));
    if empty {
        core.live.parties.remove(&pid);
        core.live.party_invites.retain(|(p, _), _| p != &pid);
    } else {
        broadcast(core, &pid);
    }
}

fn my_party(core: &Core, user: &str) -> ApiResult<String> {
    core.live
        .user_party
        .get(user)
        .cloned()
        .ok_or_else(|| ApiError::not_found("you are not in a party"))
}

fn leader_party(core: &Core, user: &str) -> ApiResult<String> {
    let pid = my_party(core, user)?;
    if core.live.parties.get(&pid).is_some_and(|p| p.leader == user) {
        Ok(pid)
    } else {
        Err(ApiError::forbidden("not_leader", "only the party leader can do this"))
    }
}

/// `GET /v1/party`.
pub async fn get(State(st): State<AppState>, a: Authed) -> ApiResult<Json<Value>> {
    let core = st.lock();
    need(&core)?;
    Ok(Json(json!({"party": party_of_json(&core, &a.user)})))
}

/// `POST /v1/party`.
pub async fn create(State(st): State<AppState>, a: Authed) -> ApiResult<(StatusCode, Json<Value>)> {
    let mut core = st.lock();
    need(&core)?;
    if core.live.user_party.contains_key(&a.user) {
        return Err(ApiError::conflict("already_in_party", "you are already in a party"));
    }
    crate::api::queues::cancel_ticket_of(&mut core, &a.user, "party_changed");
    let id = new_id("p");
    let p = Party {
        id: id.clone(),
        leader: a.user.clone(),
        members: vec![(a.user.clone(), core.now)],
        created_at: core.now,
    };
    core.live.parties.insert(id.clone(), p);
    core.live.user_party.insert(a.user.clone(), id.clone());
    let data = party_json(&core, &core.live.parties[&id]);
    Ok((StatusCode::CREATED, Json(data)))
}

#[derive(Deserialize)]
pub struct UserBody {
    user_id: String,
}

/// `POST /v1/party/invites`.
pub async fn invite(
    State(st): State<AppState>,
    a: Authed,
    JsonBody(b): JsonBody<UserBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let mut core = st.lock();
    need(&core)?;
    let pid = leader_party(&core, &a.user)?;
    if busy(&core, &pid) {
        return Err(ApiError::conflict("party_busy", "the party is in a match"));
    }
    if b.user_id == a.user || !core.user_exists(&b.user_id) {
        return Err(ApiError::not_found("no such user"));
    }
    let p = &core.live.parties[&pid];
    if p.members.iter().any(|(u, _)| u == &b.user_id) {
        return Err(ApiError::conflict(
            "already_in_party",
            "this player is already in the party",
        ));
    }
    let pending = core.live.party_invites.keys().filter(|(x, _)| x == &pid).count();
    if p.members.len() + pending >= core.cfg.limits.party_size {
        return Err(ApiError::conflict("party_full", "the party is full"));
    }
    if core.blocked_between(&a.user, &b.user_id) {
        return Err(ApiError::forbidden(
            "blocked",
            "this player does not accept your invitations",
        ));
    }
    if core.cfg.rooms.invites_friends_only && core.cfg.features.friends && !core.are_friends(&a.user, &b.user_id) {
        return Err(ApiError::forbidden("not_friends", "you can only invite friends"));
    }
    let exp = core.now + core.cfg.limits.party_invite_ttl_s;
    core.live
        .party_invites
        .insert((pid.clone(), b.user_id.clone()), (a.user.clone(), exp));
    let from = core.user_json(&a.user);
    core.push(
        &b.user_id,
        "party.invite",
        json!({"party_id": pid, "from": from, "expires_at": rfc3339(exp)}),
    );
    broadcast(&mut core, &pid);
    let data = party_json(&core, &core.live.parties[&pid]);
    Ok((StatusCode::CREATED, Json(data)))
}

/// `DELETE /v1/party/invites/{user_id}`.
pub async fn revoke(State(st): State<AppState>, a: Authed, Path(user): Path<String>) -> ApiResult<Json<Value>> {
    let mut core = st.lock();
    need(&core)?;
    let pid = leader_party(&core, &a.user)?;
    if core.live.party_invites.remove(&(pid.clone(), user)).is_none() {
        return Err(ApiError::not_found("no such invitation"));
    }
    broadcast(&mut core, &pid);
    Ok(Json(party_json(&core, &core.live.parties[&pid])))
}

/// `GET /v1/party/invites`.
pub async fn incoming(State(st): State<AppState>, a: Authed) -> ApiResult<Json<Value>> {
    let core = st.lock();
    need(&core)?;
    Ok(Json(json!({"items": invites_for(&core, &a.user)})))
}

/// `POST /v1/parties/{party_id}/join`.
pub async fn join(State(st): State<AppState>, a: Authed, Path(pid): Path<String>) -> ApiResult<Json<Value>> {
    let mut core = st.lock();
    need(&core)?;
    let key = (pid.clone(), a.user.clone());
    if !core
        .live
        .party_invites
        .get(&key)
        .is_some_and(|(_, exp)| *exp > core.now)
        || !core.live.parties.contains_key(&pid)
    {
        return Err(ApiError::not_found("no invitation to this party"));
    }
    if core.live.user_party.contains_key(&a.user) {
        return Err(ApiError::conflict("already_in_party", "leave your party first"));
    }
    if busy(&core, &pid) {
        return Err(ApiError::conflict("party_busy", "the party is in a match"));
    }
    if core
        .live
        .user_match
        .get(&a.user)
        .and_then(|m| core.live.matches.get(m))
        .is_some_and(|m| !m.state.ended())
    {
        return Err(ApiError::conflict("in_match", "you are in a match"));
    }
    if core.live.parties[&pid].members.len() >= core.cfg.limits.party_size {
        return Err(ApiError::conflict("party_full", "the party is full"));
    }
    core.live.party_invites.remove(&key);
    crate::api::queues::cancel_ticket_of(&mut core, &a.user, "party_changed");
    crate::api::queues::cancel_party_ticket(&mut core, &pid, "party_changed");
    let now = core.now;
    if let Some(p) = core.live.parties.get_mut(&pid) {
        p.members.push((a.user.clone(), now));
    }
    core.live.user_party.insert(a.user.clone(), pid.clone());
    broadcast(&mut core, &pid);
    Ok(Json(party_json(&core, &core.live.parties[&pid])))
}

/// `POST /v1/parties/{party_id}/decline`.
pub async fn decline(State(st): State<AppState>, a: Authed, Path(pid): Path<String>) -> ApiResult<StatusCode> {
    let mut core = st.lock();
    need(&core)?;
    if core.live.party_invites.remove(&(pid.clone(), a.user.clone())).is_none() {
        return Err(ApiError::not_found("no invitation to this party"));
    }
    broadcast(&mut core, &pid);
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/party/leave`.
pub async fn leave(State(st): State<AppState>, a: Authed) -> ApiResult<StatusCode> {
    let mut core = st.lock();
    need(&core)?;
    let pid = my_party(&core, &a.user)?;
    if busy(&core, &pid) {
        return Err(ApiError::conflict("party_busy", "the party is in a match"));
    }
    remove_from_party(&mut core, &a.user, "left");
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/party/kick`.
pub async fn kick(State(st): State<AppState>, a: Authed, JsonBody(b): JsonBody<UserBody>) -> ApiResult<Json<Value>> {
    let mut core = st.lock();
    need(&core)?;
    let pid = leader_party(&core, &a.user)?;
    if b.user_id == a.user || core.live.user_party.get(&b.user_id) != Some(&pid) {
        return Err(ApiError::not_found("not a member of your party"));
    }
    if busy(&core, &pid) {
        return Err(ApiError::conflict("party_busy", "the party is in a match"));
    }
    remove_from_party(&mut core, &b.user_id, "kicked");
    Ok(Json(party_json(&core, &core.live.parties[&pid])))
}

/// `POST /v1/party/leader`.
pub async fn leader(State(st): State<AppState>, a: Authed, JsonBody(b): JsonBody<UserBody>) -> ApiResult<Json<Value>> {
    let mut core = st.lock();
    need(&core)?;
    let pid = leader_party(&core, &a.user)?;
    if core.live.user_party.get(&b.user_id) != Some(&pid) {
        return Err(ApiError::not_found("not a member of your party"));
    }
    if let Some(p) = core.live.parties.get_mut(&pid) {
        p.leader = b.user_id.clone();
    }
    broadcast(&mut core, &pid);
    Ok(Json(party_json(&core, &core.live.parties[&pid])))
}
