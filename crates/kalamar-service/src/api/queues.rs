//! Queues (`api.md`, "Queues"): server-defined queues from the configuration, tickets, matchmaking in the ticker,
//! the accept phase hooks.

use std::collections::HashSet;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use kalamar_protocol::Team;
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

use crate::auth::Authed;
use crate::config::QueueEntry;
use crate::error::{ApiError, ApiResult};
use crate::matches::{MatchExtras, NewSeat, create_match};
use crate::matchmaking::{Cand, Params, Requirement, form};
use crate::rating::Rating;
use crate::state::{AppState, Core, LiveMatch, MatchRules, Ticket};
use crate::util::{new_id, parse_window, random_index, rfc3339, schedule_state};

fn need(core: &Core) -> ApiResult<()> {
    if core.cfg.features.queues {
        Ok(())
    } else {
        Err(ApiError::feature_disabled())
    }
}

/// Whether the queue is open now, and its next windows.
fn availability(q: &QueueEntry, now: i64) -> (bool, Vec<(i64, i64)>) {
    let windows: Vec<_> = q.schedule.iter().filter_map(parse_window).collect();
    schedule_state(&windows, now)
}

/// The pool in effect now (the rotation's current entry for `selection = rotation`).
fn current_pool(q: &QueueEntry, now: i64) -> Vec<(String, Option<String>)> {
    let entries = match (&q.rotation, q.selection.as_str()) {
        (Some(r), "rotation") if r.period_s > 0 && !r.pools.is_empty() => {
            &r.pools[(now.div_euclid(r.period_s) as usize) % r.pools.len()]
        }
        _ => &q.pool,
    };
    entries
        .iter()
        .map(|e| {
            (
                e.mode.clone(),
                if q.selection == "game" { None } else { e.stage.clone() },
            )
        })
        .collect()
}

fn pool_json(entries: &[crate::config::PoolEntry]) -> Vec<Value> {
    entries
        .iter()
        .map(|e| json!({"mode": e.mode, "stage": e.stage}))
        .collect()
}

/// `(games, placement_games)` of `user` on `ladder` this season.
fn games_on(core: &Core, user: &str, ladder: &str) -> i64 {
    let season = crate::db::current_season(&core.db, core.now)
        .map(|s| s.0)
        .unwrap_or_default();
    core.db
        .query_row(
            "SELECT games FROM ratings WHERE user_id = ?1 AND ladder = ?2 AND season_id = ?3",
            params![user, ladder, season],
            |r| r.get(0),
        )
        .optional()
        .ok()
        .flatten()
        .unwrap_or(0)
}

/// The requirements of a queue evaluated for `user` (`id`, `label`, `met`).
fn requirements(core: &Core, q: &QueueEntry, user: &str) -> Vec<(String, Value, bool)> {
    let mut out = Vec::new();
    for r in &q.requirements {
        match Requirement::parse(r) {
            Some(Requirement::PlacementDone(l)) => {
                let need = core.cfg.rating.placement_games;
                out.push((
                    r.clone(),
                    json!({"en": format!("Placement finished ({need} matches) on {l}")}),
                    games_on(core, user, &l) >= need,
                ));
            }
            Some(Requirement::MinGames(l, n)) => {
                out.push((
                    r.clone(),
                    json!({"en": format!("At least {n} matches on {l}")}),
                    games_on(core, user, &l) >= n,
                ));
            }
            None => {}
        }
    }
    // Security review M7: a rated queue without guest accounts (`[auth.guest] ranked = false`).
    if q.rated && !core.cfg.auth.guest.ranked {
        out.push((
            "account".into(),
            crate::auth::ranked_requirement(),
            crate::auth::may_play_ranked(core, user),
        ));
    }
    let caps = crate::api::account::game_caps(core, user);
    let players = q.teams[0] + q.teams[1];
    let runnable = current_pool(q, core.now)
        .iter()
        .any(|(m, s)| caps.can_run(players, m, s.as_deref()));
    out.push((
        "game-contract".into(),
        json!({"en": "Your game can run this queue's matches"}),
        runnable,
    ));
    if q.loadout.is_some() {
        out.push((
            "loadout".into(),
            json!({"en": "Your launcher can impose this queue's weapon and gear"}),
            caps.match_loadout,
        ));
    }
    // 1.1.6: a draft the operator requires a launcher to show.
    if q.draft
        .as_deref()
        .and_then(|d| core.cfg.draft(d))
        .is_some_and(|d| d.required)
    {
        out.push((
            "draft".into(),
            json!({"en": "A launcher that shows the stage draft", "fr": "Un lanceur qui affiche le choix des stages"}),
            caps.drafts(),
        ));
    }
    out
}

fn cooldown_of(core: &Core, user: &str) -> Option<i64> {
    core.db
        .query_row("SELECT queue_cooldown_until FROM users WHERE id = ?1", [user], |r| {
            r.get::<_, Option<i64>>(0)
        })
        .optional()
        .ok()
        .flatten()
        .flatten()
        .filter(|t| *t > core.now)
}

pub fn queue_json(core: &Core, q: &QueueEntry, user: &str) -> Value {
    let (open, windows) = availability(q, core.now);
    let reqs = requirements(core, q, user);
    let eligible = open && reqs.iter().all(|r| r.2) && cooldown_of(core, user).is_none();
    let rotation = match (&q.rotation, q.selection.as_str()) {
        (Some(r), "rotation") if r.period_s > 0 && !r.pools.is_empty() => {
            let start = core.now.div_euclid(r.period_s) * r.period_s;
            let idx = (core.now.div_euclid(r.period_s) as usize) % r.pools.len();
            let next = (idx + 1) % r.pools.len();
            json!([
                {"starts_at": rfc3339(start), "ends_at": rfc3339(start + r.period_s), "pool": pool_json(&r.pools[idx])},
                {"starts_at": rfc3339(start + r.period_s), "ends_at": rfc3339(start + 2 * r.period_s), "pool": pool_json(&r.pools[next])},
            ])
        }
        _ => Value::Null,
    };
    let searching: usize = core
        .live
        .tickets
        .values()
        .filter(|t| t.queue_id == q.id && t.match_id.is_none())
        .map(|t| t.members.len())
        .sum();
    json!({
        "id": q.id,
        "name": q.name,
        "description": q.description,
        "order": q.order,
        "teams": q.teams,
        "min_teams": q.min_teams(),
        "rated": q.rated,
        "ladder": q.ladder,
        "party": {"min": q.party_min, "max": q.party_max},
        "selection": q.selection,
        "pool": pool_json(&q.pool),
        "rotation": rotation,
        "result_policy": q.result_policy,
        "availability": {"open": open, "windows": windows.iter().map(|(a, b)| json!({"starts_at": rfc3339(*a), "ends_at": rfc3339(*b)})).collect::<Vec<_>>()},
        "estimated_wait_s": estimate(core, &q.id),
        "searching": if q.show_searching { json!(searching) } else { Value::Null },
        "requirements": reqs.iter().map(|(id, label, met)| json!({"id": id, "label": label, "met": met})).collect::<Vec<_>>(),
        "eligible": eligible,
        "draft": crate::draft::queue_member(&core.cfg, q.draft.as_deref()),
        // 1.1.7: places in the stands of the queue's matches, or `null`.
        "spectators": crate::spectate::queue_member(&core.cfg, q.spectators.as_ref()),
    })
}

fn estimate(core: &Core, queue: &str) -> Option<i64> {
    let w = core.live.queue_waits.get(queue)?;
    (!w.is_empty()).then(|| w.iter().sum::<i64>() / w.len() as i64)
}

pub fn ticket_json(core: &Core, t: &Ticket) -> Value {
    json!({
        "id": t.id,
        "queue_id": t.queue_id,
        "party_id": t.party_id,
        "members": t.members.iter().map(|u| core.user_json(u)).collect::<Vec<_>>(),
        "created_at": rfc3339(t.created_at),
        "state": if t.match_id.is_some() { "match_found" } else { "searching" },
        "match_id": t.match_id,
        "estimated_wait_s": estimate(core, &t.queue_id),
    })
}

pub fn ticket_of_json(core: &Core, user: &str) -> Value {
    core.live
        .user_ticket
        .get(user)
        .and_then(|id| core.live.tickets.get(id))
        .map(|t| ticket_json(core, t))
        .unwrap_or(Value::Null)
}

fn end_ticket(core: &mut Core, ticket_id: &str, reason: &str) {
    let Some(t) = core.live.tickets.remove(ticket_id) else {
        return;
    };
    for u in &t.members {
        if core.live.user_ticket.get(u).map(String::as_str) == Some(ticket_id) {
            core.live.user_ticket.remove(u);
        }
        core.push(u, "queue.ticket", json!({"ticket": null, "reason": reason}));
    }
    for u in &t.members {
        core.refresh_presence(u);
    }
}

/// Cancels `user`'s searching ticket (for everyone on it). A ticket with a found match is left alone.
pub fn cancel_ticket_of(core: &mut Core, user: &str, reason: &str) {
    let Some(tid) = core.live.user_ticket.get(user).cloned() else {
        return;
    };
    if core.live.tickets.get(&tid).is_some_and(|t| t.match_id.is_none()) {
        end_ticket(core, &tid, reason);
    }
}

pub fn cancel_party_ticket(core: &mut Core, party_id: &str, reason: &str) {
    let ids: Vec<String> = core
        .live
        .tickets
        .values()
        .filter(|t| t.party_id.as_deref() == Some(party_id) && t.match_id.is_none())
        .map(|t| t.id.clone())
        .collect();
    for id in ids {
        end_ticket(core, &id, reason);
    }
}

/// The live tickets of a match's players (kept aside while its draft runs).
pub fn tickets_of(core: &Core, mid: &str) -> Vec<Ticket> {
    let Some(m) = core.live.matches.get(mid) else {
        return Vec::new();
    };
    let ids: HashSet<&String> = m.seats.iter().filter_map(|s| s.ticket_id.as_ref()).collect();
    ids.into_iter()
        .filter_map(|t| core.live.tickets.get(t).cloned())
        .collect()
}

/// A draft was abandoned: the tickets of the other players search again with their original creation time (their
/// priority); the leaver's ticket (its whole party) ends.
pub fn requeue(core: &mut Core, tickets: Vec<Ticket>, leaver: &str) {
    for mut t in tickets {
        if t.members.iter().any(|u| u == leaver)
            || t.members
                .iter()
                .any(|u| core.live.user_ticket.contains_key(u) || core.live.user_match.contains_key(u))
            || core.cfg.queue(&t.queue_id).is_none()
        {
            continue;
        }
        t.match_id = None;
        let id = t.id.clone();
        for u in &t.members {
            core.live.user_ticket.insert(u.clone(), id.clone());
        }
        let members = t.members.clone();
        core.live.tickets.insert(id.clone(), t);
        let data = json!({"ticket": ticket_json(core, &core.live.tickets[&id]), "reason": "requeued"});
        core.push_many(&members, "queue.ticket", &data);
        for u in &members {
            core.refresh_presence(u);
        }
    }
}

/// Every participant accepted: the tickets end, and their waits feed the estimate.
pub fn on_match_ready(core: &mut Core, mid: &str) {
    let Some(m) = core.live.matches.get(mid) else { return };
    let tickets: HashSet<String> = m.seats.iter().filter_map(|s| s.ticket_id.clone()).collect();
    for tid in tickets {
        if let Some(t) = core.live.tickets.get(&tid) {
            let wait = core.now - t.created_at;
            let q = t.queue_id.clone();
            let w = core.live.queue_waits.entry(q).or_default();
            w.push_back(wait);
            while w.len() > 20 {
                w.pop_front();
            }
        }
        end_ticket(core, &tid, "matched");
    }
}

/// An accept phase failed: tickets in `failed` end, the others search again with their original priority.
pub fn on_match_cancelled(core: &mut Core, m: &LiveMatch, failed: &HashSet<String>) {
    let tickets: HashSet<String> = m.seats.iter().filter_map(|s| s.ticket_id.clone()).collect();
    for tid in tickets {
        if failed.contains(&tid) {
            end_ticket(core, &tid, "cancelled");
        } else if let Some(t) = core.live.tickets.get_mut(&tid) {
            t.match_id = None;
            let members = t.members.clone();
            let data = json!({"ticket": ticket_json(core, &core.live.tickets[&tid]), "reason": "requeued"});
            core.push_many(&members, "queue.ticket", &data);
        }
    }
}

/// `GET /v1/queues`.
pub async fn list(State(st): State<AppState>, a: Authed) -> ApiResult<Json<Value>> {
    let core = st.lock();
    need(&core)?;
    let items: Vec<Value> = core.cfg.queues.iter().map(|q| queue_json(&core, q, &a.user)).collect();
    Ok(Json(json!({"items": items})))
}

/// `GET /v1/queues/{queue_id}`.
pub async fn get(State(st): State<AppState>, a: Authed, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let core = st.lock();
    need(&core)?;
    let q = core
        .cfg
        .queue(&id)
        .ok_or_else(|| ApiError::not_found("no such queue"))?;
    Ok(Json(queue_json(&core, q, &a.user)))
}

/// `POST /v1/queues/{queue_id}/tickets`.
pub async fn join(
    State(st): State<AppState>,
    a: Authed,
    Path(id): Path<String>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let mut core = st.lock();
    need(&core)?;
    let cfg = core.cfg.clone();
    let q = cfg.queue(&id).ok_or_else(|| ApiError::not_found("no such queue"))?;
    if !availability(q, core.now).0 {
        return Err(ApiError::conflict("queue_closed", "this queue is closed now"));
    }
    let party = core.live.user_party.get(&a.user).and_then(|p| core.live.parties.get(p));
    let (party_id, members): (Option<String>, Vec<String>) = match party {
        Some(p) if p.members.len() > 1 => {
            if p.leader != a.user {
                return Err(ApiError::forbidden(
                    "not_leader",
                    "the party leader joins queues for the party",
                ));
            }
            (Some(p.id.clone()), p.members.iter().map(|(u, _)| u.clone()).collect())
        }
        _ => (None, vec![a.user.clone()]),
    };
    if members.len() < q.party_min || members.len() > q.party_max {
        return Err(ApiError::conflict(
            "party_size",
            format!("this queue takes parties of {} to {}", q.party_min, q.party_max),
        ));
    }
    for u in &members {
        if core.live.user_ticket.contains_key(u) {
            return Err(ApiError::conflict("already_in_queue", "already searching"));
        }
        if core
            .live
            .user_match
            .get(u)
            .and_then(|m| core.live.matches.get(m))
            .is_some_and(|m| !m.state.ended())
        {
            return Err(ApiError::conflict("in_match", "a player is in a match"));
        }
        if let Some(until) = cooldown_of(&core, u) {
            return Err(
                ApiError::forbidden("queue_cooldown", "a queue cooldown is running").retry_after(until - core.now)
            );
        }
        let unmet: Vec<Value> = requirements(&core, q, u)
            .into_iter()
            .filter(|r| !r.2)
            .map(|r| json!({"id": r.0, "label": r.1}))
            .collect();
        if !unmet.is_empty() {
            return Err(ApiError::forbidden("not_eligible", "an entry requirement is not met")
                .details(json!({"requirements": unmet})));
        }
        if let Some((until, reason)) = core.ban_of(u) {
            return Err(crate::auth::banned_error(until, reason, core.now));
        }
    }
    let tid = new_id("t");
    let t = Ticket {
        id: tid.clone(),
        queue_id: id.clone(),
        party_id,
        members: members.clone(),
        created_at: core.now,
        match_id: None,
    };
    core.live.tickets.insert(tid.clone(), t);
    for u in &members {
        core.live.user_ticket.insert(u.clone(), tid.clone());
    }
    let data = json!({"ticket": ticket_json(&core, &core.live.tickets[&tid]), "reason": null});
    core.push_many(&members, "queue.ticket", &data);
    for u in &members {
        core.refresh_presence(u);
    }
    let v = ticket_json(&core, &core.live.tickets[&tid]);
    Ok((StatusCode::CREATED, Json(v)))
}

/// `GET /v1/tickets/current`.
pub async fn current(State(st): State<AppState>, a: Authed) -> ApiResult<Json<Value>> {
    let core = st.lock();
    need(&core)?;
    Ok(Json(json!({"ticket": ticket_of_json(&core, &a.user)})))
}

/// `DELETE /v1/tickets/current`.
pub async fn cancel(State(st): State<AppState>, a: Authed) -> ApiResult<StatusCode> {
    let mut core = st.lock();
    need(&core)?;
    let tid = core
        .live
        .user_ticket
        .get(&a.user)
        .cloned()
        .ok_or_else(|| ApiError::not_found("not searching"))?;
    if core.live.tickets.get(&tid).is_some_and(|t| t.match_id.is_some()) {
        return Err(ApiError::conflict(
            "match_found",
            "a match was found: accept or decline it",
        ));
    }
    end_ticket(&mut core, &tid, "cancelled");
    Ok(StatusCode::NO_CONTENT)
}

pub(crate) fn rating_for(core: &Core, user: &str, ladder: Option<&str>, season: &str) -> Rating {
    let initial = Rating::initial(&core.cfg.rating);
    let Some(l) = ladder else { return initial };
    core.db
        .query_row(
            "SELECT rating, deviation, volatility, games FROM ratings WHERE user_id = ?1 AND ladder = ?2 AND season_id = ?3",
            params![user, l, season],
            |r| Ok(Rating { rating: r.get(0)?, deviation: r.get(1)?, volatility: r.get(2)?, games: r.get(3)? }),
        )
        .optional()
        .ok()
        .flatten()
        .unwrap_or(initial)
}

/// Matchmaking for every open queue (called by the ticker), and closing tickets of queues that closed.
pub fn tick(core: &mut Core) {
    if !core.cfg.features.queues {
        return;
    }
    let cfg = core.cfg.clone();
    let season = crate::db::current_season(&core.db, core.now)
        .map(|s| s.0)
        .unwrap_or_default();
    for q in &cfg.queues {
        if !availability(q, core.now).0 {
            let ids: Vec<String> = core
                .live
                .tickets
                .values()
                .filter(|t| t.queue_id == q.id && t.match_id.is_none())
                .map(|t| t.id.clone())
                .collect();
            for id in ids {
                end_ticket(core, &id, "queue_closed");
            }
            continue;
        }
        let ladder = q.balance_ladder.as_deref().or(q.ladder.as_deref());
        for _ in 0..50 {
            let tickets: Vec<(String, Vec<String>, Option<String>, i64)> = {
                let mut v: Vec<_> = core
                    .live
                    .tickets
                    .values()
                    .filter(|t| t.queue_id == q.id && t.match_id.is_none())
                    .map(|t| (t.id.clone(), t.members.clone(), t.party_id.clone(), t.created_at))
                    .collect();
                v.sort_by(|a, b| (a.3, &a.0).cmp(&(b.3, &b.0)));
                v
            };
            if tickets.is_empty() {
                break;
            }
            let cands: Vec<Cand> = tickets
                .iter()
                .map(|(_, members, _, at)| {
                    let rs: Vec<Rating> = members.iter().map(|u| rating_for(core, u, ladder, &season)).collect();
                    let n = rs.len().max(1) as f64;
                    let mean = Rating {
                        rating: rs.iter().map(|r| r.rating).sum::<f64>() / n,
                        deviation: (rs.iter().map(|r| r.deviation * r.deviation).sum::<f64>() / n).sqrt(),
                        volatility: cfg.rating.initial_volatility,
                        games: 0,
                    };
                    Cand {
                        size: members.len(),
                        rating: mean,
                        created_at: *at,
                    }
                })
                .collect();
            let p = Params {
                teams: q.teams,
                min_teams: q.min_teams(),
                shrink_after_s: q.shrink_after_s,
                search: &q.search,
                center: cfg.rating.initial_rating,
            };
            let Some(f) = form(&cands, &p, core.now) else { break };
            let mut seats = Vec::new();
            for team in [Team::Alpha, Team::Bravo] {
                for (k, &i) in f.picked.iter().enumerate() {
                    if (team == Team::Alpha) == f.alpha[k] {
                        let (tid, members, party, _) = &tickets[i];
                        for u in members {
                            seats.push(NewSeat {
                                user_id: u.clone(),
                                team,
                                party_id: party.clone(),
                                ticket_id: Some(tid.clone()),
                            });
                        }
                    }
                }
            }
            let pool = current_pool(q, core.now);
            let runnable: Vec<&(String, Option<String>)> = pool
                .iter()
                .filter(|(m, s)| {
                    seats
                        .iter()
                        .all(|x| crate::api::account::game_caps(core, &x.user_id).can_run(seats.len(), m, s.as_deref()))
                })
                .collect();
            let choice = if runnable.is_empty() {
                pool.first()
            } else if q.selection == "fixed" {
                runnable.first().copied()
            } else {
                Some(runnable[random_index(runnable.len())])
            };
            let Some((mode, stage)) = choice.cloned() else { break };
            // 1.1.6: the draft's pools come from the queue's pool: the stages of the chosen mode (or of every runnable
            // entry when the modes are drafted too), and the combinations when every entry names a stage.
            let draft = q.draft.as_ref().filter(|d| cfg.draft(d).is_some()).map(|d| {
                let modes_drafted = cfg.draft(d).is_some_and(|p| {
                    p.turns
                        .iter()
                        .chain(p.first_game_turns.iter().flatten())
                        .any(|t| crate::draft::parse_turn(t).is_ok_and(|s| s.kind() == crate::draft::Kind::Mode))
                });
                let entries: Vec<&(String, Option<String>)> = if runnable.is_empty() {
                    pool.iter().collect()
                } else {
                    runnable.clone()
                };
                let mut modes: Vec<String> = Vec::new();
                let mut stages: Vec<String> = Vec::new();
                for (m, s) in &entries {
                    if !modes.contains(m) {
                        modes.push(m.clone());
                    }
                    if let Some(s) = s.as_ref().filter(|_| modes_drafted || *m == mode)
                        && !stages.contains(s)
                    {
                        stages.push(s.clone());
                    }
                }
                let pairs = if modes_drafted && entries.iter().all(|(_, s)| s.is_some()) {
                    entries
                        .iter()
                        .filter_map(|(m, s)| s.clone().map(|s| (m.clone(), s)))
                        .collect()
                } else {
                    Vec::new()
                };
                crate::draft::DraftPlan {
                    preset: d.clone(),
                    stages,
                    modes,
                    pairs,
                    ladder: q.balance_ladder.clone().or(q.ladder.clone()),
                    ..Default::default()
                }
            });
            let rules = MatchRules {
                teams: f.teams,
                mode,
                stage,
                options: json!({}),
                rated: q.rated,
                ladder: q.ladder.clone(),
                season: q.rated.then(|| season.clone()),
                result_policy: q.result_policy.clone(),
                loadout: q.loadout.clone(),
                spectators: crate::spectate::rule_of(&cfg, q.spectators.as_ref()),
            };
            let host = random_index(seats.len());
            let extras = MatchExtras {
                draft,
                ..MatchExtras::default()
            };
            match create_match(core, "queue", &q.id, rules, seats, host, true, extras) {
                Ok(mid) => {
                    for &i in &f.picked {
                        if let Some(t) = core.live.tickets.get_mut(&tickets[i].0) {
                            t.match_id = Some(mid.clone());
                        }
                    }
                    for &i in &f.picked {
                        let tid = &tickets[i].0;
                        let data = json!({"ticket": ticket_json(core, &core.live.tickets[tid]), "reason": null});
                        let members = tickets[i].1.clone();
                        core.push_many(&members, "queue.ticket", &data);
                    }
                }
                // 1.1.13: every relay is full: the tickets wait (logged once, `relay_capacity`).
                Err(e) if e.code == "relay_capacity" => break,
                Err(e) => {
                    tracing::error!(queue = %q.id, error = %e.message, "match creation failed");
                    break;
                }
            }
        }
    }
}
