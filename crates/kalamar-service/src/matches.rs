//! The match engine: creation (member names, relay room, rules string, deadlines), the accept phase, launch and
//! status reports, result intake, the decision (`kalamar_protocol::decision`) and everything that follows it
//! (ratings, offenses, events, the room reopening). Time-based transitions run in [`tick`].

use kalamar_protocol::contract::{build_rules_seated, derive_member_names};
use kalamar_protocol::decision::{self, Classified, Decision, MatchSpec, Verdict};
use kalamar_protocol::policy::{Decider, Leave, Policy, WitnessFrom, WitnessUse};
use kalamar_protocol::{Role, Team};
use rusqlite::{OptionalExtension, params};
use serde_json::{Value, json};

use crate::error::{ApiError, ApiResult};
use crate::state::{Core, GameBlock, LiveMatch, MState, MatchRules, Seat, SpectatorSeat};
use crate::util::{new_id, new_relay_room, rfc3339};

/// Game states that prove a participant reached the match.
const PLAYED_STATES: &[&str] = &["in-match", "results", "finished"];
/// 1.1.7: game states of a player that say the match has started: a spectator's game started now would stay in the
/// game's lobby until the end (the engine cannot seat it), so its launch is refused (`409 match_started`).
pub const STARTED_STATES: &[&str] = &["loading-stage", "in-match", "results", "finished", "waiting-command"];

/// What a match may carry besides its rules (protocol 1.1).
#[derive(Debug, Clone, Default)]
pub struct MatchExtras {
    pub series: Option<crate::state::SeriesRef>,
    /// Reuse this relay room (contract 2 keeps one room for a session's rounds).
    pub relay_room: Option<String>,
    /// The relay (`host:port`) of the series this match is a game of: every game of a series uses the same one.
    pub relay: Option<String>,
    /// The previous round of the same session.
    pub continue_from: Option<String>,
    /// More games may follow (series): with contract 2 the game stays open.
    pub more_games: bool,
    /// 1.1.6: a stage (and mode) draft before the match is ready (`draft.md`).
    pub draft: Option<crate::draft::DraftPlan>,
    /// 1.1.7: the spectators seated from the start (`spectators.md`).
    pub spectators: Vec<NewSpectator>,
    /// 1.1.7: the spectator who hosts the game session (a room whose operator allows it), instead of a player.
    pub spectator_host: Option<usize>,
    /// The result policy of the series this match is a game of, as the series' first game had it: a reload that
    /// changes or removes it does not change the rest of the series.
    pub policy: Option<std::sync::Arc<crate::config::PolicyEntry>>,
}

pub struct NewSeat {
    pub user_id: String,
    pub team: Team,
    pub party_id: Option<String>,
    pub ticket_id: Option<String>,
}

/// 1.1.7: a spectator to seat in a new match.
#[derive(Debug, Clone)]
pub struct NewSpectator {
    pub user_id: String,
    pub referee: bool,
}

/// 1.1.7: who a caller is in a match.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Member {
    Player(usize),
    Spectator(usize),
}

/// The policy a match is decided by: the definition it was created with (`LiveMatch::policy`).
pub(crate) fn policy_of(_core: &Core, m: &LiveMatch) -> Policy {
    m.policy.policy()
}

/// The definition of the policy `id` in force now; the built-in `standard` (with a warning) when it is gone.
pub(crate) fn policy_entry(core: &Core, id: &str) -> std::sync::Arc<crate::config::PolicyEntry> {
    match core.cfg.policy(id) {
        Some(p) => std::sync::Arc::new(p.clone()),
        None => {
            tracing::warn!(policy = %id, "result policy no longer configured: the built-in standard applies");
            std::sync::Arc::new(
                crate::config::builtin_policies()
                    .into_iter()
                    .next()
                    .expect("the built-in standard policy"),
            )
        }
    }
}

/// 1.1.13: every relay that could take a new match is full (`relay.md` 4.3). Logged once per episode.
pub fn relay_capacity(core: &mut Core) -> ApiError {
    if !core.live.relays_all_full {
        core.live.relays_all_full = true;
        tracing::warn!(
            "every relay is full: new matches wait (queues) or are refused (relay_capacity) until one has room"
        );
    }
    ApiError::new(
        axum::http::StatusCode::SERVICE_UNAVAILABLE,
        "relay_capacity",
        "every relay of this server is full: try again in a moment",
    )
    .retry_after(30)
    .text(
        "Every relay of this server is full. Try again in a moment.",
        "Tous les relais de ce serveur sont pleins. Réessayez dans un instant.",
    )
}

#[allow(clippy::too_many_arguments)]
/// Creates a match from a room start (`pending_accept = false`: state `ready`) or matchmaking (`pending_accept`).
pub fn create_match(
    core: &mut Core,
    origin_kind: &'static str,
    origin_id: &str,
    mut rules: MatchRules,
    seats: Vec<NewSeat>,
    host: usize,
    pending_accept: bool,
    extras: MatchExtras,
) -> ApiResult<String> {
    if seats.len() < 2
        || seats.len() > kalamar_protocol::contract::MAX_PLAYERS
        || host >= seats.len()
        || extras.spectators.len() > kalamar_protocol::contract::MAX_SPECTATORS
        || extras.spectator_host.is_some_and(|k| k >= extras.spectators.len())
    {
        return Err(ApiError::internal("bad match size"));
    }
    // The relay: the one of the session this match continues (a series), else chosen for its players (spectators
    // have no say: `relays::pick`).
    let players: Vec<String> = seats.iter().map(|s| s.user_id.clone()).collect();
    let relay = match extras
        .relay
        .as_deref()
        .and_then(|a| core.cfg.relays.iter().find(|r| r.addr() == a).cloned())
    {
        Some(r) => r,
        None => {
            let r =
                crate::relays::choose(core, &players, seats.len() + extras.spectators.len()).map_err(|e| match e {
                    crate::relays::NoRelay::None => ApiError::internal("no relay configured"),
                    crate::relays::NoRelay::Capacity => relay_capacity(core),
                })?;
            if core.live.relays_all_full {
                core.live.relays_all_full = false;
                tracing::info!("a relay has room again: new matches start");
            }
            r
        }
    };
    // The policy as defined now (or as the series started with): the match keeps this definition to its end.
    let policy_def = extras
        .policy
        .clone()
        .filter(|p| p.id == rules.result_policy)
        .unwrap_or_else(|| policy_entry(core, &rules.result_policy));
    let policy = policy_def.policy();
    if rules.rated && rules.season.is_none() {
        rules.season = Some(crate::db::current_season(&core.db, core.now)?.0);
    }
    // Member names are unique over the whole game session: players, then spectators (1.1.7).
    let names: Vec<String> = seats
        .iter()
        .map(|s| core.display_name(&s.user_id))
        .chain(extras.spectators.iter().map(|s| core.display_name(&s.user_id)))
        .collect();
    let refs: Vec<&str> = names.iter().map(String::as_str).collect();
    let all_members = derive_member_names(&refs);
    let (members, spectator_names) = all_members.split_at(seats.len());
    // 1.1.7: the session's host is a player, or the spectator the origin chose (a room whose operator allows it).
    let host_user = match extras.spectator_host {
        Some(k) => extras.spectators[k].user_id.clone(),
        None => seats[host].user_id.clone(),
    };
    let host_name = match extras.spectator_host {
        Some(k) => spectator_names[k].clone(),
        None => members[host].clone(),
    };
    // 1.1.6: the draft, when the origin has one and there is something to draft; a drafted stage (mode) replaces the
    // origin's until the draft decides.
    let draft = extras
        .draft
        .as_ref()
        .and_then(|p| crate::draft::build(core, p, &seats, host));
    if draft.as_ref().is_some_and(|d| d.has_kind(crate::draft::Kind::Stage)) {
        rules.stage = None;
    }
    let drafting = draft.is_some() && !pending_accept;
    // A stage must be in the designated host's list (the stages of its save); else the game's draw.
    if let Some(st) = rules.stage.clone() {
        let has = |u: &str| {
            crate::api::account::game_caps(core, u)
                .stages
                .as_ref()
                .is_none_or(|l| l.iter().any(|x| x == &st))
        };
        if !has(&host_user) {
            rules.stage = None;
        }
    }
    let now = core.now;
    let id = new_id("m");
    // Contract 2 only when every participant declared it (match-contract-2.md, section 3); 1.1.7: spectators too.
    let caps: Vec<crate::api::account::GameCaps> = seats
        .iter()
        .map(|s| &s.user_id)
        .chain(extras.spectators.iter().map(|s| &s.user_id))
        .map(|u| crate::api::account::game_caps(core, u))
        .collect();
    let v2 = caps.iter().all(|c| c.contract_versions.contains(&2));
    let room = if v2 {
        extras.relay_room.clone().unwrap_or_else(new_relay_room)
    } else {
        new_relay_room()
    };
    // A relay bound to this service opens only the room codes sealed with its secret (kept in memory).
    let room = crate::relays::room_for(core, &relay, &room, now).map_err(|e| {
        tracing::error!(relay = %relay.key(), error = %e, "relay room");
        ApiError::internal("no room code for this relay")
    })?;
    let game = GameBlock {
        contract: if v2 { 2 } else { 1 },
        relay_room: v2.then(|| room.clone()),
        // 1.1.3: keep the session only when every engine says a round is faster than a relaunch.
        keep_open: v2 && extras.more_games && caps.iter().all(|c| c.rounds && c.rounds_faster),
        continue_from: if v2 { extras.continue_from.clone() } else { None },
        continue_window_s: (v2 && extras.more_games).then_some(90),
        series: extras.series.clone(),
        match_id: if v2 { id.clone() } else { room.clone() },
        relay_host: relay.host.clone(),
        relay_port: relay.port,
        players: seats.len() + extras.spectators.len(),
        rules: String::new(),
        host_name,
        timeouts: None,
    };
    // 1.1.7: the places stay open until the game block is final (the accept phase, the draft); a room's are taken in
    // the room itself, never through the match.
    if let Some(sp) = rules.spectators.as_mut() {
        sp.open = (pending_accept || drafting) && origin_kind != "room";
    }
    let spectators: Vec<SpectatorSeat> = extras
        .spectators
        .iter()
        .enumerate()
        .map(|(k, s)| SpectatorSeat {
            user_id: Some(s.user_id.clone()),
            display_name: names[seats.len() + k].clone(),
            member_name: spectator_names[k].clone(),
            role: if extras.spectator_host == Some(k) {
                Role::Host
            } else {
                Role::Join
            },
            referee: s.referee || crate::spectate::is_referee(&core.cfg, &s.user_id),
            launched_at: None,
            status: None,
            status_state: None,
            status_at: None,
            report: None,
            report_at: None,
            left: false,
        })
        .collect();
    let d = policy.deadlines;
    let mut m = LiveMatch {
        id: id.clone(),
        origin_kind,
        origin_id: origin_id.to_string(),
        state: if pending_accept {
            MState::PendingAccept
        } else {
            MState::Ready
        },
        rules,
        game,
        seats: seats
            .into_iter()
            .enumerate()
            .map(|(i, s)| Seat {
                user_id: Some(s.user_id),
                display_name: names[i].clone(),
                member_name: members[i].clone(),
                team: s.team,
                role: if i == host && extras.spectator_host.is_none() {
                    Role::Host
                } else {
                    Role::Join
                },
                party_id: s.party_id,
                ticket_id: s.ticket_id,
                accept: if pending_accept { "pending" } else { "accepted" },
                launched_at: None,
                status: None,
                status_state: None,
                status_at: None,
                saw_in_match: false,
                report: None,
                report_at: None,
            })
            .collect(),
        created_at: now,
        ready_at: None,
        accept_deadline: pending_accept.then_some(now + i64::from(d.accept_s)),
        launch_deadline: None,
        result_deadline: None,
        first_result_at: None,
        updated_at: now,
        ended_at: None,
        cancel_reason: None,
        outcome: None,
        draft,
        spectators,
        policy: policy_def,
    };
    m.game.rules = rules_string(&m);
    if !pending_accept && !drafting {
        set_ready(&mut m, &policy, now);
    }
    insert(core, &m)?;
    let users: Vec<String> = m.users();
    for u in &users {
        core.live.user_match.insert(u.clone(), id.clone());
    }
    core.live.matches.insert(id.clone(), m);
    if drafting {
        crate::draft::start(core, &id)?;
    } else {
        let kind = if pending_accept { "match.found" } else { "match.ready" };
        notify(core, &id, kind);
    }
    for u in &users {
        core.refresh_presence(u);
    }
    tracing::info!(match_id = %id, origin = origin_kind, players = users.len(), "match created");
    Ok(id)
}

pub(crate) fn set_ready(m: &mut LiveMatch, policy: &Policy, now: i64) {
    // 1.1.7: the game block is final: the places in the stands close (the engine cannot seat a spectator later).
    if let Some(sp) = m.rules.spectators.as_mut() {
        sp.open = false;
    }
    reseat(m);
    m.state = MState::Ready;
    m.ready_at = Some(now);
    m.accept_deadline = None;
    m.launch_deadline = Some(now + i64::from(policy.deadlines.launch_s));
    m.result_deadline = Some(now + i64::from(policy.deadlines.result_s));
    m.updated_at = now;
}

fn insert(core: &Core, m: &LiveMatch) -> ApiResult<()> {
    let tx = core.db.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO matches (id, origin_kind, origin_id, state, rules, game, season_id, created_at, ready_at,
           accept_deadline, launch_deadline, result_deadline, draft)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)",
        params![
            m.id,
            m.origin_kind,
            m.origin_id,
            m.state.as_str(),
            serde_json::to_string(&m.rules).unwrap_or_default(),
            serde_json::to_string(&m.game).unwrap_or_default(),
            m.rules.season,
            m.created_at,
            m.ready_at,
            m.accept_deadline,
            m.launch_deadline,
            m.result_deadline,
            m.draft.as_ref().and_then(|d| serde_json::to_string(d).ok())
        ],
    )?;
    for (i, s) in m.seats.iter().enumerate() {
        tx.execute(
            "INSERT INTO match_players (match_id, slot, user_id, display_name, member_name, team, role, party_id, ticket_id, accept)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![m.id, i as i64, s.user_id, s.display_name, s.member_name, s.team.as_str(), s.role.as_str(), s.party_id, s.ticket_id, s.accept],
        )?;
    }
    save_spectators(&tx, m)?;
    tx.commit()?;
    Ok(())
}

/// 1.1.7: the match's places in the stands, rewritten whole (places are taken and left before the launch).
fn save_spectators(tx: &rusqlite::Transaction<'_>, m: &LiveMatch) -> ApiResult<()> {
    tx.execute("DELETE FROM match_spectators WHERE match_id = ?1", [&m.id])?;
    for (i, s) in m.spectators.iter().enumerate() {
        tx.execute(
            "INSERT INTO match_spectators (match_id, slot, user_id, display_name, member_name, role, referee, launched_at,
               status, status_state, status_at, report, report_at, left_place)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
            params![
                m.id,
                i as i64,
                s.user_id,
                s.display_name,
                s.member_name,
                s.role.as_str(),
                s.referee as i64,
                s.launched_at,
                s.status.as_ref().map(Value::to_string),
                s.status_state,
                s.status_at,
                s.report.as_ref().map(Value::to_string),
                s.report_at,
                s.left as i64
            ],
        )?;
    }
    Ok(())
}

fn load_spectators(core: &Core, mid: &str) -> ApiResult<Vec<SpectatorSeat>> {
    let mut st = core.db.prepare(
        "SELECT user_id, display_name, member_name, role, referee, launched_at, status, status_state, status_at, report,
                report_at, left_place FROM match_spectators WHERE match_id = ?1 ORDER BY slot",
    )?;
    let rows = st
        .query_map([mid], |r| {
            let parse = |s: Option<String>| s.and_then(|s| serde_json::from_str::<Value>(&s).ok());
            Ok(SpectatorSeat {
                user_id: r.get(0)?,
                display_name: r.get(1)?,
                member_name: r.get(2)?,
                role: Role::parse(&r.get::<_, String>(3)?).unwrap_or(Role::Join),
                referee: r.get::<_, i64>(4)? != 0,
                launched_at: r.get(5)?,
                status: parse(r.get(6)?),
                status_state: r.get(7)?,
                status_at: r.get(8)?,
                report: parse(r.get(9)?),
                report_at: r.get(10)?,
                left: r.get::<_, i64>(11)? != 0,
            })
        })?
        .collect::<Result<_, _>>()?;
    Ok(rows)
}

/// Writes the match row and every seat.
pub(crate) fn save(core: &Core, m: &LiveMatch) -> ApiResult<()> {
    let tx = core.db.unchecked_transaction()?;
    tx.execute(
        "UPDATE matches SET state = ?2, ready_at = ?3, ended_at = ?4, accept_deadline = ?5, launch_deadline = ?6,
           result_deadline = ?7, first_result_at = ?8, cancel_reason = ?9, outcome = ?10, season_id = ?11,
           rules = ?12, game = ?13, draft = ?14 WHERE id = ?1",
        params![
            m.id,
            m.state.as_str(),
            m.ready_at,
            m.ended_at,
            m.accept_deadline,
            m.launch_deadline,
            m.result_deadline,
            m.first_result_at,
            m.cancel_reason,
            m.outcome.as_ref().map(Value::to_string),
            m.rules.season,
            serde_json::to_string(&m.rules).unwrap_or_default(),
            serde_json::to_string(&m.game).unwrap_or_default(),
            m.draft.as_ref().and_then(|d| serde_json::to_string(d).ok())
        ],
    )?;
    for (i, s) in m.seats.iter().enumerate() {
        tx.execute(
            "UPDATE match_players SET accept = ?3, launched_at = ?4, status = ?5, status_state = ?6, status_at = ?7,
               saw_in_match = ?8, report = ?9, report_at = ?10 WHERE match_id = ?1 AND slot = ?2",
            params![
                m.id,
                i as i64,
                s.accept,
                s.launched_at,
                s.status.as_ref().map(Value::to_string),
                s.status_state,
                s.status_at,
                s.saw_in_match as i64,
                s.report.as_ref().map(Value::to_string),
                s.report_at
            ],
        )?;
    }
    save_spectators(&tx, m)?;
    tx.commit()?;
    Ok(())
}

/// The match object: the participant view for a participant (a player or, 1.1.7, a spectator), else the public view
/// (ended matches only).
pub fn match_json(_core: &Core, m: &LiveMatch, viewer: Option<&str>) -> Value {
    let me = viewer.and_then(|v| m.seat_of(v));
    let watcher = viewer.and_then(|v| m.spectator_of(v));
    let participant = me.is_some() || watcher.is_some();
    let participants: Vec<Value> = m
        .seats
        .iter()
        .map(|s| {
            let user = match &s.user_id {
                Some(u) => json!({"id": u, "display_name": s.display_name}),
                None => json!({"id": null, "display_name": ""}),
            };
            if participant {
                json!({
                    "user": user, "member_name": s.member_name, "team": s.team.as_str(), "role": s.role.as_str(),
                    "accept": s.accept, "launched": s.launched_at.is_some(),
                    "status": s.status_state.as_ref().map(|st| json!({"state": st, "at": s.status_at.map(rfc3339)})),
                    "reported": s.report.is_some(),
                })
            } else {
                json!({"user": user, "team": s.team.as_str()})
            }
        })
        .collect();
    let mut origin = json!({"kind": m.origin_kind});
    let key = match m.origin_kind {
        "room" => "room_id",
        "activity" => "activity_id",
        _ => "queue_id",
    };
    origin[key] = json!(m.origin_id);
    let mut v = json!({
        "id": m.id,
        "state": m.state.as_str(),
        "origin": origin,
        "rules": m.rules,
        "participants": participants,
        "ended_at": m.ended_at.map(rfc3339),
        "outcome": m.outcome,
    });
    if let Some(r) = &m.game.series {
        v["series"] = crate::series::match_member(r);
    }
    // 1.1.6: the draft (players see their team's votes and their own part; 1.1.7: spectators see it read-only, without
    // any team's votes or hidden choices).
    if let Some(d) = &m.draft {
        let mut dv = d.view(if me.is_some() { viewer } else { None });
        if watcher.is_some() {
            dv["you"] = json!({"team": "spectator", "role": "none", "can_act": false, "choice": null});
        }
        v["draft"] = dv;
    }
    // 1.1.7: the stands. Players and spectators see every spectator (no spectator is hidden from the players).
    if participant && (!m.spectators.is_empty() || m.rules.spectators.is_some()) {
        v["spectators"] = json!(m.spectators.iter().map(|s| {
            let user = match &s.user_id {
                Some(u) => json!({"id": u, "display_name": s.display_name}),
                None => json!({"id": null, "display_name": ""}),
            };
            json!({
                "user": user, "member_name": s.member_name, "role": s.role.as_str(), "referee": s.referee,
                "launched": s.launched_at.is_some(), "left": s.left,
                "status": s.status_state.as_ref().map(|st| json!({"state": st, "at": s.status_at.map(rfc3339)})),
                "reported": s.report.is_some(),
            })
        }).collect::<Vec<_>>());
    }
    let you = match (me, watcher) {
        (Some(i), _) => {
            let s = &m.seats[i];
            Some(
                json!({"member_name": s.member_name, "team": s.team.as_str(), "role": s.role.as_str(), "seat": "player"}),
            )
        }
        (None, Some(k)) => {
            let s = &m.spectators[k];
            Some(
                json!({"member_name": s.member_name, "team": "spectator", "role": s.role.as_str(), "seat": "spectator",
                        "referee": s.referee, "left": s.left}),
            )
        }
        _ => None,
    };
    if let (Some(mut you), Some(viewer)) = (you, viewer) {
        you["user_id"] = json!(viewer);
        // 1.1.10: this participant's report is in and the match no longer holds it (it may search again).
        if is_free(m, viewer) {
            you["free"] = json!(true);
        }
        v["you"] = you;
        v["game"] = if matches!(m.state, MState::PendingAccept | MState::Drafting | MState::Cancelled) {
            Value::Null
        } else {
            let mut g = json!({
                "contract": {"id": kalamar_protocol::CONTRACT_ID, "version": m.game.contract},
                "match_id": m.game.match_id,
                "relay": {"host": m.game.relay_host, "port": m.game.relay_port,
                          "room": m.game.relay_room.clone().unwrap_or_else(|| m.game.match_id.clone())},
                "players": m.game.players,
                "rules": m.game.rules,
                "host_name": m.game.host_name,
                "timeouts": m.game.timeouts,
            });
            if m.game.contract >= 2 {
                g["keep_open"] = json!(m.game.keep_open);
                g["continue_from"] = json!(m.game.continue_from);
                if let Some(w) = m.game.continue_window_s {
                    g["continue_window_s"] = json!(w);
                }
            }
            g
        };
        v["deadlines"] = json!({
            "accept": m.accept_deadline.map(rfc3339),
            "launch": m.launch_deadline.map(rfc3339),
            "result": m.result_deadline.map(rfc3339),
        });
        v["created_at"] = json!(rfc3339(m.created_at));
        v["updated_at"] = json!(rfc3339(m.updated_at));
        v["cancel_reason"] = json!(m.cancel_reason);
    } else if let Some(o) = v.get_mut("outcome").and_then(Value::as_object_mut) {
        o.remove("players");
        o.remove("invalid_reasons");
        // 1.1.7: who watched is the participants' business, not the public's.
        o.remove("spectators");
    }
    v
}

/// Sends a `match.*` event with the participant view to the participants (players, spectators) among `users`.
pub(crate) fn notify_users(core: &mut Core, mid: &str, kind: &str, users: &[String]) {
    let Some(m) = core.live.matches.get(mid) else { return };
    let views: Vec<(String, Value)> = m
        .users()
        .into_iter()
        .filter(|u| users.contains(u))
        .map(|u| (u.clone(), match_json(core, m, Some(&u))))
        .collect();
    for (u, v) in views {
        core.push(&u, kind, v);
    }
}

/// The game's rules string of a match (`match-contract.md`): its players and teams, its mode, its stage; 1.1.7: its
/// spectators (`<name>:S` after the players, counted in `players=`) and the host's wait for them (`spectators_wait`).
pub(crate) fn rules_string(m: &LiveMatch) -> String {
    let pairs: Vec<(&str, Team)> = m.seats.iter().map(|s| (s.member_name.as_str(), s.team)).collect();
    let watchers: Vec<&str> = m.spectators.iter().map(|s| s.member_name.as_str()).collect();
    let mut r = build_rules_seated(&pairs, &watchers, &m.rules.mode);
    if let Some(stage) = &m.rules.stage {
        r.push_str(&format!(",stage={stage}"));
    }
    if !watchers.is_empty() {
        let wait = m.rules.spectators.as_ref().map(|s| s.wait_s).unwrap_or(60);
        r.push_str(&format!(",spectators_wait={wait}"));
    }
    r
}

/// 1.1.7: after the places in the stands changed (before the game block is final): the game's player count and
/// rules string follow.
pub(crate) fn reseat(m: &mut LiveMatch) {
    m.game.players = m.seats.len() + m.spectators.len();
    m.game.rules = rules_string(m);
}

/// Sends a `match.*` event with the participant view to every participant (players and spectators). 1.1.10: a
/// participant free of the match after its report (`is_free`) gets no more `match.updated` of it, so that a launcher
/// in its next match is not shown this one again; the decision (`match.completed`, `match.void`) still reaches it.
pub(crate) fn notify(core: &mut Core, mid: &str, kind: &str) {
    let Some(m) = core.live.matches.get(mid) else { return };
    let views: Vec<(String, Value)> = m
        .users()
        .into_iter()
        .filter(|u| kind != "match.updated" || !is_free(m, u))
        .map(|u| (u.clone(), match_json(core, m, Some(&u))))
        .collect();
    for (u, v) in views {
        core.push(&u, kind, v);
    }
}

pub(crate) fn notify_ended(core: &mut Core, m: &LiveMatch, kind: &str) {
    for u in m.users() {
        let v = match_json(core, m, Some(&u));
        core.push(&u, kind, v);
    }
}

/// Loads a match (live, else from the database).
pub fn load(core: &Core, mid: &str) -> ApiResult<Option<LiveMatch>> {
    if let Some(m) = core.live.matches.get(mid) {
        return Ok(Some(m.clone()));
    }
    load_db(core, mid)
}

#[allow(clippy::type_complexity)]
fn load_db(core: &Core, mid: &str) -> ApiResult<Option<LiveMatch>> {
    let row: Option<(String, String, String, String, String, i64, Option<i64>, Option<i64>, Option<i64>, Option<i64>, Option<i64>, Option<i64>, Option<String>, Option<String>, Option<String>)> = core
        .db
        .query_row(
            "SELECT origin_kind, origin_id, state, rules, game, created_at, ready_at, ended_at, accept_deadline,
                    launch_deadline, result_deadline, first_result_at, cancel_reason, outcome, draft FROM matches WHERE id = ?1",
            [mid],
            |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?, r.get(7)?, r.get(8)?,
                    r.get(9)?, r.get(10)?, r.get(11)?, r.get(12)?, r.get(13)?, r.get(14)?))
            },
        )
        .optional()?;
    let Some((ok, oid, state, rules, game, created, ready, ended, acc, launch, result, first, cancel, outcome, draft)) =
        row
    else {
        return Ok(None);
    };
    let mut st = core.db.prepare(
        "SELECT user_id, display_name, member_name, team, role, party_id, ticket_id, accept, launched_at, status,
                status_state, status_at, saw_in_match, report, report_at FROM match_players WHERE match_id = ?1 ORDER BY slot",
    )?;
    let seats: Vec<Seat> = st
        .query_map([mid], |r| {
            let parse = |s: Option<String>| s.and_then(|s| serde_json::from_str::<Value>(&s).ok());
            Ok(Seat {
                user_id: r.get(0)?,
                display_name: r.get(1)?,
                member_name: r.get(2)?,
                team: Team::parse(&r.get::<_, String>(3)?).unwrap_or(Team::Alpha),
                role: Role::parse(&r.get::<_, String>(4)?).unwrap_or(Role::Join),
                party_id: r.get(5)?,
                ticket_id: r.get(6)?,
                accept: match r.get::<_, String>(7)?.as_str() {
                    "accepted" => "accepted",
                    "declined" => "declined",
                    _ => "pending",
                },
                launched_at: r.get(8)?,
                status: parse(r.get(9)?),
                status_state: r.get(10)?,
                status_at: r.get(11)?,
                saw_in_match: r.get::<_, i64>(12)? != 0,
                report: parse(r.get(13)?),
                report_at: r.get(14)?,
            })
        })?
        .collect::<Result<_, _>>()?;
    let rules: MatchRules = serde_json::from_str(&rules).map_err(|_| ApiError::internal("stored rules unreadable"))?;
    let game: GameBlock = serde_json::from_str(&game).map_err(|_| ApiError::internal("stored game unreadable"))?;
    let spectators = load_spectators(core, mid)?;
    // After a restart the definition is the configuration's (a policy removed meanwhile: the built-in standard).
    let policy = match core.live.matches.get(mid) {
        Some(live) => live.policy.clone(),
        None => policy_entry(core, &rules.result_policy),
    };
    Ok(Some(LiveMatch {
        policy,
        spectators,
        id: mid.to_string(),
        origin_kind: match ok.as_str() {
            "room" => "room",
            "activity" => "activity",
            _ => "queue",
        },
        origin_id: oid,
        state: MState::parse(&state).unwrap_or(MState::Void),
        rules,
        game,
        seats,
        created_at: created,
        ready_at: ready,
        accept_deadline: acc,
        launch_deadline: launch,
        result_deadline: result,
        first_result_at: first,
        updated_at: ended.or(ready).unwrap_or(created),
        ended_at: ended,
        cancel_reason: cancel,
        outcome: outcome.and_then(|s| serde_json::from_str(&s).ok()),
        draft: draft.and_then(|s| serde_json::from_str(&s).ok()),
    }))
}

/// Loads the matches that were running when the service stopped; pending accept phases are canceled (tickets
/// lived in memory).
pub fn restore(core: &mut Core) -> ApiResult<()> {
    let ids: Vec<String> = {
        let mut st = core
            .db
            .prepare("SELECT id FROM matches WHERE state IN ('pending_accept', 'drafting', 'ready', 'running')")?;
        st.query_map([], |r| r.get(0))?.flatten().collect()
    };
    for id in ids {
        let Some(mut m) = load_db(core, &id)? else { continue };
        if m.state == MState::PendingAccept {
            m.state = MState::Cancelled;
            m.cancel_reason = Some("server_restart".into());
            m.ended_at = Some(core.now);
            save(core, &m)?;
            continue;
        }
        // 1.1.10: a participant free after its report stays free.
        for s in &m.seats {
            if let Some(u) = &s.user_id
                && !s.report.as_ref().is_some_and(|r| frees_reporter(&m, r))
            {
                core.live.user_match.insert(u.clone(), id.clone());
            }
        }
        for s in m.spectators.iter().filter(|s| !s.left) {
            if let Some(u) = &s.user_id
                && !s.report.as_ref().is_some_and(|r| frees_reporter(&m, r))
            {
                core.live.user_match.insert(u.clone(), id.clone());
            }
        }
        core.live.matches.insert(id, m);
    }
    Ok(())
}

/// Distinguishes "ended" from "unknown" for a match not live.
fn not_live(core: &Core, mid: &str, user: &str) -> ApiError {
    match load_db(core, mid) {
        Ok(Some(m)) if m.involves(user) => ApiError::conflict("match_state", "this match has ended"),
        _ => ApiError::not_found("no such match"),
    }
}

/// The caller's part in a live match: a player or (1.1.7) a spectator.
pub(crate) fn live_member(core: &Core, mid: &str, user: &str) -> ApiResult<Member> {
    match core.live.matches.get(mid) {
        Some(m) => match (m.seat_of(user), m.spectator_of(user)) {
            (Some(i), _) => Ok(Member::Player(i)),
            (None, Some(k)) => Ok(Member::Spectator(k)),
            _ => Err(ApiError::not_found("no such match")),
        },
        None => Err(not_live(core, mid, user)),
    }
}

/// `accept` (`true`) or `decline` a found match.
pub fn answer(core: &mut Core, mid: &str, user: &str, accept: bool) -> ApiResult<Value> {
    let i = match live_member(core, mid, user)? {
        Member::Player(i) => i,
        // 1.1.7: a spectator has nothing to accept; declining leaves its place.
        Member::Spectator(_) if accept => return Ok(match_json(core, &core.live.matches[mid], Some(user))),
        Member::Spectator(_) => {
            crate::spectate::leave_match(core, mid, user)?;
            let m = load(core, mid)?.ok_or_else(|| ApiError::not_found("no such match"))?;
            return Ok(match_json(core, &m, Some(user)));
        }
    };
    let state = core.live.matches[mid].state;
    // 1.1.6: declining during the draft abandons the match.
    if state == MState::Drafting && !accept {
        crate::draft::abandon(core, mid, user)?;
        let m = load(core, mid)?.ok_or_else(|| ApiError::not_found("no such match"))?;
        return Ok(match_json(core, &m, Some(user)));
    }
    if state != MState::PendingAccept {
        let same = core.live.matches[mid].seats[i].accept == "accepted" && accept;
        if same {
            return Ok(match_json(core, &core.live.matches[mid], Some(user)));
        }
        return Err(ApiError::conflict(
            "match_state",
            "this match is not waiting for answers",
        ));
    }
    let now = core.now;
    let policy = policy_of(core, &core.live.matches[mid]);
    let all = {
        let m = core
            .live
            .matches
            .get_mut(mid)
            .ok_or_else(|| ApiError::not_found("no such match"))?;
        m.seats[i].accept = if accept { "accepted" } else { "declined" };
        m.updated_at = now;
        m.seats.iter().all(|s| s.accept == "accepted")
    };
    if !accept {
        let m = core.live.matches[mid].clone();
        save(core, &m)?;
        cancel_pending(core, mid, "declined")?;
        let m = load(core, mid)?.ok_or_else(|| ApiError::not_found("no such match"))?;
        return Ok(match_json(core, &m, Some(user)));
    }
    if all && core.live.matches[mid].draft.is_some() {
        // 1.1.6: the draft starts; the tickets are kept aside in case a player abandons it.
        let tickets = crate::api::queues::tickets_of(core, mid);
        core.live.draft_tickets.insert(mid.to_string(), tickets);
        crate::api::queues::on_match_ready(core, mid);
        crate::draft::start(core, mid)?;
        let users: Vec<String> = core
            .live
            .matches
            .get(mid)
            .map(|m| m.seats.iter().filter_map(|s| s.user_id.clone()).collect())
            .unwrap_or_default();
        for u in users {
            core.refresh_presence(&u);
        }
    } else if all {
        if let Some(m) = core.live.matches.get_mut(mid) {
            set_ready(m, &policy, now);
        }
        crate::api::queues::on_match_ready(core, mid);
        let m = core.live.matches[mid].clone();
        save(core, &m)?;
        notify(core, mid, "match.ready");
        for u in m.seats.iter().filter_map(|s| s.user_id.clone()) {
            core.refresh_presence(&u);
        }
    } else {
        let m = core.live.matches[mid].clone();
        save(core, &m)?;
        notify(core, mid, "match.updated");
    }
    Ok(match_json(core, &core.live.matches[mid], Some(user)))
}

/// Ends an accept phase: decliners or non-answerers get an offense, accepted tickets go back to searching.
pub fn cancel_pending(core: &mut Core, mid: &str, reason: &str) -> ApiResult<()> {
    let Some(mut m) = core.live.matches.remove(mid) else {
        return Ok(());
    };
    m.state = MState::Cancelled;
    m.cancel_reason = Some(reason.to_string());
    m.ended_at = Some(core.now);
    m.updated_at = core.now;
    save(core, &m)?;
    let offence = if reason == "declined" {
        "decline"
    } else {
        "accept_timeout"
    };
    let mut failed_tickets = std::collections::HashSet::new();
    for s in &m.seats {
        if s.accept != "accepted" {
            if let Some(t) = &s.ticket_id {
                failed_tickets.insert(t.clone());
            }
            if let Some(u) = &s.user_id {
                if reason == "declined" && s.accept == "pending" {
                    continue;
                }
                apply_offence(core, u, offence, Some(mid))?;
            }
        }
    }
    for u in m.users() {
        if core.live.user_match.get(&u) == Some(&m.id) {
            core.live.user_match.remove(&u);
        }
    }
    crate::api::queues::on_match_cancelled(core, &m, &failed_tickets);
    notify_ended(core, &m, "match.cancelled");
    for u in m.users() {
        core.refresh_presence(&u);
    }
    Ok(())
}

/// Records an offense and starts the cooldown the penalty ladder gives.
pub fn apply_offence(core: &mut Core, user: &str, kind: &str, match_id: Option<&str>) -> ApiResult<()> {
    let p = &core.cfg.penalties;
    if !core.cfg.features.queues || !p.counted.iter().any(|k| k == kind) {
        return Ok(());
    }
    let since = core.now - p.window_s;
    let count: i64 = core.db.query_row(
        "SELECT COUNT(*) FROM offences WHERE user_id = ?1 AND at > ?2",
        params![user, since],
        |r| r.get(0),
    )?;
    let idx = (count as usize).min(p.cooldowns_s.len() - 1);
    let cooldown = p.cooldowns_s[idx];
    let until = (cooldown > 0).then_some(core.now + cooldown);
    core.db.execute(
        "INSERT INTO offences (user_id, kind, match_id, at, cooldown_until) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![user, kind, match_id, core.now, until],
    )?;
    if let Some(until) = until {
        core.db.execute(
            "UPDATE users SET queue_cooldown_until = MAX(COALESCE(queue_cooldown_until, 0), ?1) WHERE id = ?2",
            params![until, user],
        )?;
        crate::api::queues::cancel_ticket_of(core, user, "cancelled");
        core.push(
            user,
            "queue.cooldown",
            json!({"until": rfc3339(until), "offence": kind}),
        );
        let me = crate::api::account::me_json(core, user);
        core.push(user, "me.updated", me);
    }
    Ok(())
}

/// Error codes of a report that count as leaving a queue match (the offense `left`).
pub(crate) const LEAVING_ERRORS: &[&str] = &["left-by-player", "launcher-cancelled"];

/// The terminal report of `user` (a player's or a spectator's), if any.
fn report_of<'a>(m: &'a LiveMatch, user: &str) -> Option<&'a Value> {
    match (m.seat_of(user), m.spectator_of(user)) {
        (Some(i), _) => m.seats[i].report.as_ref(),
        (None, Some(k)) => m.spectators[k].report.as_ref(),
        _ => None,
    }
}

/// 1.1.10 (`api.md`, "Lifecycle"): whether a participant whose terminal report is `report` is free of this match while
/// it waits for the others' reports: it may search, join a room or play another match, and the decision still applies
/// to it (it gets `match.completed` or `match.void`). Without it, when one player of a 1v1 vanishes
/// with its launcher (a computer that crashed), the match waits for its result deadline with the other player held
/// (`409 in_match`). Not within a series
/// (its next game keeps the same players together), and not for a report that counts as leaving a queue match (the
/// offense `left` comes with the decision, which holds that player until then).
pub(crate) fn frees_reporter(m: &LiveMatch, report: &Value) -> bool {
    let leaving = m.origin_kind == "queue"
        && report
            .get("error")
            .and_then(Value::as_str)
            .is_some_and(|e| LEAVING_ERRORS.contains(&e));
    m.game.series.is_none() && !leaving && matches!(m.state, MState::Ready | MState::Running)
}

/// Whether `user` has reported and is free of the match (`frees_reporter`).
pub(crate) fn is_free(m: &LiveMatch, user: &str) -> bool {
    report_of(m, user).is_some_and(|r| frees_reporter(m, r))
}

/// 1.1.10: `user`'s report is in; the match stops holding it when `frees_reporter` says so.
fn free_reporter(core: &mut Core, mid: &str, user: &str) {
    let free = core.live.matches.get(mid).is_some_and(|m| is_free(m, user));
    if free && core.live.user_match.get(user).map(String::as_str) == Some(mid) {
        core.live.user_match.remove(user);
        core.refresh_presence(user);
    }
}

/// 1.1.7: whether a player's game has started the match (its stage loads or later).
pub(crate) fn started(m: &LiveMatch) -> bool {
    m.seats
        .iter()
        .any(|s| s.status_state.as_deref().is_some_and(|st| STARTED_STATES.contains(&st)))
}

/// 1.1.7: `POST /v1/matches/{id}/launch` by a spectator: refused once a player's game started the match (the engine
/// leaves a late spectator in its lobby until the end, then fails with `host-left`); a spectator launches before it
/// starts its game.
fn launch_spectator(core: &mut Core, mid: &str, user: &str, k: usize) -> ApiResult<Value> {
    let now = core.now;
    let m = core
        .live
        .matches
        .get_mut(mid)
        .ok_or_else(|| ApiError::not_found("no such match"))?;
    if !matches!(m.state, MState::Ready | MState::Running) || m.spectators[k].left {
        return Err(ApiError::conflict("match_state", "this match cannot be launched now"));
    }
    if m.spectators[k].launched_at.is_none() {
        if started(m) {
            return Err(ApiError::conflict(
                "match_started",
                "the match has started: the game cannot let a spectator in any more",
            )
            .text(
                "The match has already started: the game cannot let a spectator in now.",
                "Le match a déjà commencé : le jeu ne peut plus faire entrer un spectateur.",
            ));
        }
        m.spectators[k].launched_at = Some(now);
        m.updated_at = now;
        let snapshot = m.clone();
        save(core, &snapshot)?;
        notify(core, mid, "match.updated");
    }
    Ok(match_json(core, &core.live.matches[mid], Some(user)))
}

/// 1.1.7: a spectator's mirrored status (kept, shown to the others; it never proves anything about the players).
fn status_spectator(core: &mut Core, mid: &str, k: usize, state: String, body: Value) -> ApiResult<()> {
    let now = core.now;
    let m = core
        .live
        .matches
        .get_mut(mid)
        .ok_or_else(|| ApiError::not_found("no such match"))?;
    if !matches!(m.state, MState::Ready | MState::Running) {
        return Err(ApiError::conflict("match_state", "this match is not running"));
    }
    let seat = &mut m.spectators[k];
    let changed = seat.status_state.as_deref() != Some(state.as_str());
    seat.status_state = Some(state);
    seat.status_at = Some(now);
    seat.status = Some(body);
    seat.launched_at.get_or_insert(now);
    m.updated_at = now;
    let snapshot = m.clone();
    save(core, &snapshot)?;
    if changed {
        notify(core, mid, "match.updated");
    }
    Ok(())
}

/// 1.1.7: a spectator's report: kept as a witness report (`spectators.md`), never a player's.
fn result_spectator(core: &mut Core, mid: &str, user: &str, k: usize, body: Value) -> ApiResult<Value> {
    let now = core.now;
    let m = core
        .live
        .matches
        .get_mut(mid)
        .ok_or_else(|| ApiError::not_found("no such match"))?;
    if !matches!(m.state, MState::Ready | MState::Running) {
        return Err(ApiError::conflict("match_state", "this match is not running"));
    }
    if let Some(prev) = &m.spectators[k].report {
        if prev == &body {
            return Ok(
                json!({"match": match_json(core, &core.live.matches[mid], Some(user)), "report": {"accepted": true, "verdict": null}}),
            );
        }
        return Err(ApiError::conflict(
            "already_reported",
            "a different report was already received",
        ));
    }
    m.spectators[k].report = Some(body);
    m.spectators[k].report_at = Some(now);
    m.spectators[k].launched_at.get_or_insert(now);
    m.updated_at = now;
    let snapshot = m.clone();
    save(core, &snapshot)?;
    free_reporter(core, mid, user);
    notify(core, mid, "match.updated");
    // A free reporter gets this one update (its view says `you.free`); the match's later updates skip it.
    if core.live.matches.get(mid).is_some_and(|m| is_free(m, user)) {
        notify_users(core, mid, "match.updated", &[user.to_string()]);
    }
    maybe_decide(core, mid)?;
    let m = load(core, mid)?.ok_or_else(|| ApiError::not_found("no such match"))?;
    let verdict = m.outcome.as_ref().and_then(|o| {
        o["spectators"]
            .as_array()?
            .iter()
            .find(|p| p["user_id"] == json!(user))
            .map(|p| p["verdict"].clone())
    });
    Ok(
        json!({"match": match_json(core, &m, Some(user)), "report": {"accepted": true, "verdict": verdict, "witness": true}}),
    )
}

/// `POST /v1/matches/{id}/launch`.
pub fn launch(core: &mut Core, mid: &str, user: &str) -> ApiResult<Value> {
    let i = match live_member(core, mid, user)? {
        Member::Player(i) => i,
        Member::Spectator(k) => return launch_spectator(core, mid, user, k),
    };
    let now = core.now;
    let m = core
        .live
        .matches
        .get_mut(mid)
        .ok_or_else(|| ApiError::not_found("no such match"))?;
    if !matches!(m.state, MState::Ready | MState::Running) {
        return Err(ApiError::conflict("match_state", "this match cannot be launched now"));
    }
    if m.seats[i].launched_at.is_none() {
        m.seats[i].launched_at = Some(now);
        m.state = MState::Running;
        m.updated_at = now;
        let snapshot = m.clone();
        save(core, &snapshot)?;
        notify(core, mid, "match.updated");
    }
    Ok(match_json(core, &core.live.matches[mid], Some(user)))
}

/// `POST /v1/matches/{id}/status`.
pub fn status(core: &mut Core, mid: &str, user: &str, body: Value) -> ApiResult<()> {
    let member = live_member(core, mid, user)?;
    let state = body
        .get("state")
        .and_then(Value::as_str)
        .filter(|s| (1..=32).contains(&s.len()))
        .map(str::to_string)
        .ok_or_else(|| ApiError::invalid("/state", "the status file's state (a string)"))?;
    let i = match member {
        Member::Player(i) => i,
        Member::Spectator(k) => return status_spectator(core, mid, k, state, body),
    };
    let now = core.now;
    let m = core
        .live
        .matches
        .get_mut(mid)
        .ok_or_else(|| ApiError::not_found("no such match"))?;
    if !matches!(m.state, MState::Ready | MState::Running) {
        return Err(ApiError::conflict("match_state", "this match is not running"));
    }
    let seat = &mut m.seats[i];
    let changed = seat.status_state.as_deref() != Some(state.as_str());
    seat.saw_in_match |= PLAYED_STATES.contains(&state.as_str());
    seat.status_state = Some(state);
    seat.status_at = Some(now);
    seat.status = Some(body);
    if seat.launched_at.is_none() {
        seat.launched_at = Some(now);
        m.state = MState::Running;
    }
    m.updated_at = now;
    let snapshot = m.clone();
    save(core, &snapshot)?;
    if changed {
        notify(core, mid, "match.updated");
    }
    Ok(())
}

fn spec_of(m: &LiveMatch) -> MatchSpec {
    MatchSpec {
        game_match_id: m.game.match_id.clone(),
        mode: m.rules.mode.clone(),
        stage: m.rules.stage.clone(),
        rated: m.rules.rated,
        loadout: m.rules.loadout.clone(),
        seats: m
            .seats
            .iter()
            .map(|s| decision::Seat {
                member_name: s.member_name.clone(),
                team: s.team,
                role: s.role,
                launched: s.launched_at.is_some() || s.report.is_some(),
                saw_in_match: s.saw_in_match,
                report: s.report.clone(),
                reported_at: s.report_at,
            })
            .collect(),
        // 1.1.7: the spectators' reports are witness reports.
        witnesses: m
            .spectators
            .iter()
            .map(|s| decision::Witness {
                member_name: s.member_name.clone(),
                role: s.role,
                referee: s.referee,
                launched: s.launched_at.is_some() || s.report.is_some(),
                report: s.report.clone(),
            })
            .collect(),
    }
}

/// `POST /v1/matches/{id}/result`.
pub fn result(core: &mut Core, mid: &str, user: &str, body: Value) -> ApiResult<Value> {
    let i = match live_member(core, mid, user)? {
        Member::Player(i) => i,
        Member::Spectator(k) => return result_spectator(core, mid, user, k, body),
    };
    let now = core.now;
    let m = core
        .live
        .matches
        .get_mut(mid)
        .ok_or_else(|| ApiError::not_found("no such match"))?;
    if !matches!(m.state, MState::Ready | MState::Running) {
        return Err(ApiError::conflict("match_state", "this match is not running"));
    }
    if let Some(prev) = &m.seats[i].report {
        if prev == &body {
            return Ok(
                json!({"match": match_json(core, &core.live.matches[mid], Some(user)), "report": {"accepted": true, "verdict": null}}),
            );
        }
        return Err(ApiError::conflict(
            "already_reported",
            "a different report was already received",
        ));
    }
    m.seats[i].report = Some(body);
    m.seats[i].report_at = Some(now);
    if m.seats[i].launched_at.is_none() {
        m.seats[i].launched_at = Some(now);
    }
    m.state = MState::Running;
    m.updated_at = now;
    let spec = spec_of(m);
    if let Classified::Result(_) = decision::classify(m.seats[i].report.as_ref().unwrap_or(&Value::Null), &spec, i) {
        m.first_result_at.get_or_insert(now);
    }
    let snapshot = m.clone();
    save(core, &snapshot)?;
    free_reporter(core, mid, user);
    notify(core, mid, "match.updated");
    // A free reporter gets this one update (its view says `you.free`); the match's later updates skip it.
    if core.live.matches.get(mid).is_some_and(|m| is_free(m, user)) {
        notify_users(core, mid, "match.updated", &[user.to_string()]);
    }
    maybe_decide(core, mid)?;
    let m = load(core, mid)?.ok_or_else(|| ApiError::not_found("no such match"))?;
    let verdict = m.outcome.as_ref().and_then(|o| {
        o["players"]
            .as_array()?
            .iter()
            .find(|p| p["user_id"] == json!(user))
            .map(|p| p["verdict"].clone())
    });
    Ok(json!({"match": match_json(core, &m, Some(user)), "report": {"accepted": true, "verdict": verdict}}))
}

/// Decides the match if its conditions are met (every report in, grace or result deadline passed), or voids it as
/// a no-show at the launch deadline.
pub fn maybe_decide(core: &mut Core, mid: &str) -> ApiResult<()> {
    let Some(m) = core.live.matches.get(mid) else {
        return Ok(());
    };
    let policy = policy_of(core, m);
    let now = core.now;
    // A match waiting for a referee gets the automatic decision when the review deadline passes.
    if m.state == MState::UnderReview {
        if m.result_deadline.is_some_and(|t| now >= t) {
            let spec = spec_of(m);
            let d = decision::decide(&policy, &spec).unwrap_or_else(|| decision::decide_no_show(&policy, &spec));
            return finish(core, mid, d);
        }
        return Ok(());
    }
    if !matches!(m.state, MState::Ready | MState::Running) {
        return Ok(());
    }
    // 1.1.7: when the policy uses witnesses, the decision also waits for the counted spectators who launched (the
    // grace period and the deadline still bound it); spectators never hold a decision otherwise.
    let waiting_witness = policy.witness != WitnessUse::Ignore
        && m.spectators.iter().any(|s| {
            (policy.witness_from == WitnessFrom::Any || s.referee)
                && s.launched_at.is_some()
                && !s.left
                && s.report.is_none()
        });
    let all_in = m.seats.iter().all(|s| s.report.is_some()) && !waiting_witness;
    let grace_over = m
        .first_result_at
        .is_some_and(|t| now >= t + i64::from(policy.deadlines.grace_s));
    let deadline = m.result_deadline.is_some_and(|t| now >= t);
    // 1.1.11: a match of two players that one left in play (`left-by-player` with `in_play`), under a policy whose
    // `leave` is a forfeit, is decided `grace_s` after the departure even when the other player never reports (its
    // game ended with the match: nothing else can come; its launcher may be gone). With more players the
    // others' games may play on and record the match: they are waited for as before.
    let left_over = policy.leave != Leave::Void
        && m.seats.len() == 2
        && m.seats.iter().any(|s| {
            s.report.as_ref().is_some_and(decision::left_in_play)
                && s.report_at
                    .is_some_and(|t| now >= t + i64::from(policy.deadlines.grace_s))
        });
    let no_show = m.launch_deadline.is_some_and(|t| now >= t)
        && m.seats.iter().any(|s| s.launched_at.is_none() && s.report.is_none());
    let spec = spec_of(m);
    let d = if all_in || grace_over || deadline || left_over {
        decision::decide(&policy, &spec)
    } else if no_show {
        Some(decision::decide_no_show(&policy, &spec))
    } else {
        return Ok(());
    };
    let d = d.unwrap_or_else(|| decision::decide_no_show(&policy, &spec));
    let referee = match policy.decider {
        Decider::Referee => true,
        Decider::AutoThenReferee => !d.counted,
        _ => false,
    };
    if referee {
        return send_to_review(core, mid);
    }
    finish(core, mid, d)
}

/// Puts a match `under_review` until a referee decides or the review deadline passes; its players are free.
fn send_to_review(core: &mut Core, mid: &str) -> ApiResult<()> {
    let review = core.live.matches[mid].policy.review_deadline_s;
    let now = core.now;
    let Some(m) = core.live.matches.get_mut(mid) else {
        return Ok(());
    };
    m.state = MState::UnderReview;
    m.result_deadline = Some(now + i64::from(review));
    m.updated_at = now;
    let m = m.clone();
    save(core, &m)?;
    for u in m.users() {
        if core.live.user_match.get(&u) == Some(&m.id) {
            core.live.user_match.remove(&u);
        }
    }
    notify(core, mid, "match.updated");
    for u in m.users() {
        core.refresh_presence(&u);
    }
    tracing::info!(match_id = %mid, "match under review");
    Ok(())
}

/// A referee's decision through the operator API: on a match not decided yet (ready, running or under review).
pub fn referee_decide(core: &mut Core, mid: &str, counted: bool, winner: Option<Team>, rated: bool) -> ApiResult<()> {
    let m = core
        .live
        .matches
        .get(mid)
        .ok_or_else(|| ApiError::conflict("match_state", "only a match not yet decided can be decided"))?;
    if !matches!(m.state, MState::Ready | MState::Running | MState::UnderReview) {
        return Err(ApiError::conflict(
            "match_state",
            "this match is not waiting for a decision",
        ));
    }
    let policy = policy_of(core, m);
    let spec = spec_of(m);
    let mut d = decision::decide(&policy, &spec).unwrap_or_else(|| decision::decide_no_show(&policy, &spec));
    d.decision = "admin";
    d.counted = counted;
    d.winner = if counted { winner } else { None };
    d.rated = counted && rated && spec.rated && d.winner.is_some();
    d.unrated_reason = None;
    if !counted {
        d.record = None;
    }
    finish(core, mid, d)
}

fn summary(d: &Decision, expected: usize) -> Value {
    let en = match d.decision {
        "agreed" => format!("Result confirmed by {} of {} reports.", d.agreeing, expected),
        "majority" => format!(
            "Result decided by a majority of {} of {} reports; the others disagreed.",
            d.agreeing, expected
        ),
        "single" => "Result accepted from one report; no other report contradicted it.".to_string(),
        "forfeit" => match d.forfeit.as_ref().map(|f| f.reason) {
            Some("left") => concat!(
                "A team lost by forfeit: one of its players left the match while it was being played ",
                "(their own launcher reported it)."
            )
            .to_string(),
            Some("loadout") => {
                "A team lost by forfeit: one of its players did not play the required weapon or gear.".to_string()
            }
            _ => "A team lost by forfeit: one of its players disconnected.".to_string(),
        },
        "aborted" => "No result: the game did not produce a match record.".to_string(),
        "invalid_reports" => {
            let first = d.invalid_reasons.iter().flatten().next().cloned().unwrap_or_default();
            format!("No result: the games produced a record, but every report failed the server's checks ({first}).")
        }
        "loadout_violation" => "No result: a player did not play the required weapon or gear.".to_string(),
        "disagreement" => "No result: the reports contradict each other.".to_string(),
        "insufficient_reports" => "No result: not enough players confirmed it.".to_string(),
        "no_reports" => "No result: no report arrived.".to_string(),
        "no_show" => "No result: a player did not launch the game in time.".to_string(),
        "witnessed" => "The players' reports disagreed; the spectator's report decided between them.".to_string(),
        "witness_disagreement" => {
            "No result: the spectator's report contradicts the players' (the policy requires a witness).".to_string()
        }
        other => format!("Decision: {other}."),
    };
    // 1.1.8: reports that agree on the outcome but whose counters differ beyond the tolerance (`stats: flag`).
    let flagged = d.counted && d.stats.flagged;
    let mut en = en;
    if flagged {
        en.push_str(
            " The reports' detailed counters differ more than two games normally do; the server's operator is told.",
        );
    }
    let mut text = json!({"en": en});
    if let Some(fr) = match d.decision {
        "forfeit" if d.forfeit.as_ref().is_some_and(|f| f.reason == "left") => Some(
            "Une équipe perd par forfait : un de ses joueurs a quitté le match en cours (son propre lanceur l'a signalé).",
        ),
        "witnessed" => Some("Les rapports des joueurs divergeaient ; celui du spectateur a tranché."),
        "witness_disagreement" => Some("Pas de résultat : le rapport du spectateur contredit celui des joueurs."),
        _ => None,
    } {
        let mut fr = fr.to_string();
        if flagged {
            fr.push_str(" Les compteurs détaillés des rapports diffèrent plus que d'ordinaire ; l'opérateur du serveur est prévenu.");
        }
        text["fr"] = json!(fr);
    }
    text
}

/// 1.1.8: `outcome.stats_check`: what the policy's `stats` made of the agreeing reports' counters, the gaps beyond the
/// tolerance (a player's by `user_id`, so an erased account's are anonymized with the rest), and whose report is the
/// record (its counters are the statistics recorded).
fn stats_check_json(m: &LiveMatch, d: &Decision) -> Value {
    let uid = |i: usize| m.seats.get(i).and_then(|s| s.user_id.clone());
    let gaps: Vec<Value> = d
        .stats
        .gaps
        .iter()
        .map(|g| {
            let mut v = json!({"field": g.field, "values": g.values});
            if let Some(t) = g.team {
                v["team"] = json!(t.as_str());
            }
            if let Some(i) = g.seat {
                v["user_id"] = json!(uid(i));
            }
            v
        })
        .collect();
    json!({
        "check": d.stats.check, "differ": d.stats.differ, "flagged": d.stats.flagged, "gaps": gaps,
        "record_from": d.stats.record_seat.map(|i| json!({"user_id": uid(i)})),
    })
}

/// Applies a decision: outcome, seats, ratings, offenses, events, room reopening.
fn finish(core: &mut Core, mid: &str, d: Decision) -> ApiResult<()> {
    let Some(mut m) = core.live.matches.remove(mid) else {
        return Ok(());
    };
    let now = core.now;
    m.state = if d.counted { MState::Completed } else { MState::Void };
    m.ended_at = Some(now);
    m.updated_at = now;
    let record = d.record.as_ref().map(|r| {
        let mut v = r.public_json();
        if let Some(players) = v.get_mut("players").and_then(Value::as_array_mut) {
            for p in players {
                let uid = m
                    .seats
                    .iter()
                    .find(|s| p["name"] == json!(s.member_name))
                    .and_then(|s| s.user_id.clone());
                p["user_id"] = json!(uid);
            }
        }
        v
    });
    let rating_changes = if d.rated {
        crate::api::ratings::apply_match(core, &m, &d)?
    } else {
        Vec::new()
    };
    let players: Vec<Value> = m
        .seats
        .iter()
        .zip(&d.verdicts)
        .map(|(s, v)| json!({"user_id": s.user_id, "verdict": v.as_str()}))
        .collect();
    // 1.1.2: why reports were refused (participants see it; the log names the check, not the player).
    let invalid_reasons: Vec<Value> = m
        .seats
        .iter()
        .zip(&d.invalid_reasons)
        .filter_map(|(s, r)| {
            let r = r.as_ref()?;
            let check = r
                .split_once(':')
                .map(|(c, _)| c)
                .filter(|c| c.len() <= 4 && c.starts_with('V'))
                .unwrap_or("format");
            tracing::warn!(match_id = %m.id, check, reason = %r, "invalid result report");
            Some(json!({"user_id": s.user_id, "check": check, "text": {"en": r}}))
        })
        .collect();
    let mut outcome = json!({
        "decision": d.decision,
        "counted": d.counted,
        "winner": d.winner.map(Team::as_str),
        "rated": d.rated,
        "unrated_reason": d.unrated_reason,
        "summary": summary(&d, m.seats.len()),
        "record": record,
        "reports": {"expected": m.seats.len(), "results": d.results, "agreeing": d.agreeing, "errors": d.errors, "invalid": d.invalid},
        "players": players,
        "rating_changes": rating_changes,
        "rematch_id": null,
        "invalid_reasons": invalid_reasons,
    });
    // 1.1.7: the spectators' reports are witnesses: their verdicts, apart from the players'.
    if !m.spectators.is_empty() {
        let policy = policy_of(core, &m);
        let counted = |s: &SpectatorSeat| {
            policy.witness != WitnessUse::Ignore && (policy.witness_from == WitnessFrom::Any || s.referee)
        };
        outcome["spectators"] = json!(
            m.spectators
                .iter()
                .enumerate()
                .map(|(k, s)| {
                    let mut w = json!({"user_id": s.user_id, "referee": s.referee, "counted": counted(s),
                               "verdict": d.witnesses.get(k).map(|v| v.as_str()).unwrap_or("absent")});
                    if let Some(Some(why)) = d.witness_reasons.get(k) {
                        w["text"] = json!({"en": why});
                    }
                    w
                })
                .collect::<Vec<_>>()
        );
        outcome["reports"]["witnesses"] = json!(m.spectators.len());
        outcome["witnessed"] = json!(d.witnessed);
    }
    // 1.1.11: why a team lost by forfeit: `left` (a player's own launcher reported its departure in play),
    // `disconnected`, `loadout`; the players it was about, by account (`{"user_id"}`, so an erased account's is
    // anonymized with the rest).
    if let Some(f) = &d.forfeit {
        let players: Vec<Value> = f
            .seats
            .iter()
            .map(|&i| json!({"user_id": m.seats.get(i).and_then(|s| s.user_id.clone())}))
            .collect();
        outcome["forfeit"] = json!({"reason": f.reason, "team": f.team.as_str(), "players": players});
    }
    // 1.1.8: the counters of the reports that agree on the outcome (informative), and whose report is the record.
    if d.counted && d.record.is_some() {
        outcome["stats_check"] = stats_check_json(&m, &d);
        if d.stats.flagged {
            // The log names the fields and seats, not the players.
            let gaps: Vec<String> = d
                .stats
                .gaps
                .iter()
                .map(|g| match (g.team, g.seat) {
                    (Some(t), _) => format!("{}.{}", t.as_str(), g.field),
                    (_, Some(i)) => format!("seat{i}.{}", g.field),
                    _ => g.field.to_string(),
                })
                .collect();
            tracing::warn!(match_id = %m.id, gaps = %gaps.join(","), decision = d.decision,
                "result reports agree on the outcome but their counters differ beyond the tolerance");
        }
    }
    m.outcome = Some(outcome);
    save(core, &m)?;
    // Per-seat results, statistics and verdicts for history and stats.
    for (i, s) in m.seats.iter().enumerate() {
        let result = match d.winner {
            Some(w) if d.counted => {
                if s.team == w {
                    "win"
                } else {
                    "loss"
                }
            }
            _ => "none",
        };
        let stats = d
            .record
            .as_ref()
            .and_then(|r| r.player(&s.member_name))
            .map(|p| serde_json::to_string(p).unwrap_or_default());
        let delta = rating_changes
            .iter()
            .find(|c| c["user_id"] == json!(s.user_id))
            .and_then(|c| c["delta"].as_f64());
        core.db.execute(
            "UPDATE match_players SET verdict = ?3, result = ?4, stats = ?5, rating_delta = ?6 WHERE match_id = ?1 AND slot = ?2",
            params![m.id, i as i64, d.verdicts[i].as_str(), result, stats, delta],
        )?;
    }
    // Offenses: queue matches only.
    if m.origin_kind == "queue" {
        for (i, s) in m.seats.iter().enumerate() {
            let Some(u) = &s.user_id else { continue };
            let left = s
                .report
                .as_ref()
                .and_then(|r| r.get("error"))
                .and_then(Value::as_str)
                .is_some_and(|e| LEAVING_ERRORS.contains(&e));
            if d.verdicts[i] == Verdict::NoShow {
                apply_offence(core, u, "no_show", Some(&m.id))?;
            } else if left {
                apply_offence(core, u, "left", Some(&m.id))?;
            }
        }
    }
    // 1.1.7: the spectators' verdicts, for their own history (never an offense, never a rating).
    for (k, _) in m.spectators.iter().enumerate() {
        core.db.execute(
            "UPDATE match_spectators SET verdict = ?3 WHERE match_id = ?1 AND slot = ?2",
            params![
                m.id,
                k as i64,
                d.witnesses.get(k).map(|v| v.as_str()).unwrap_or("absent")
            ],
        )?;
    }
    for u in m.users() {
        if core.live.user_match.get(&u) == Some(&m.id) {
            core.live.user_match.remove(&u);
        }
    }
    // 1.1.1: the host's save lacked the stage: draw again (the game's draw) instead of voiding the evening.
    let mut replacement = None;
    let stage_missing = m
        .seats
        .iter()
        .any(|s| s.report.as_ref().and_then(|r| r.get("error")).and_then(Value::as_str) == Some("stage-unavailable"));
    if !d.counted && stage_missing && m.game.series.is_none() && m.seats.iter().all(|s| s.user_id.is_some()) {
        let seats: Vec<NewSeat> = m
            .seats
            .iter()
            .map(|s| NewSeat {
                user_id: s.user_id.clone().unwrap_or_default(),
                team: s.team,
                party_id: s.party_id.clone(),
                ticket_id: None,
            })
            .collect();
        let host = m.seats.iter().position(|s| s.role == Role::Host).unwrap_or(0);
        let rules = MatchRules {
            stage: None,
            ..m.rules.clone()
        };
        // 1.1.7: the same spectators watch the replacement (they had their place before the launch).
        let watchers: Vec<&SpectatorSeat> = m.spectators.iter().filter(|s| !s.left && s.user_id.is_some()).collect();
        let extras = MatchExtras {
            spectators: watchers
                .iter()
                .map(|s| NewSpectator {
                    user_id: s.user_id.clone().unwrap_or_default(),
                    referee: s.referee,
                })
                .collect(),
            spectator_host: watchers.iter().position(|s| s.role == Role::Host),
            relay: Some(format!("{}:{}", m.game.relay_host, m.game.relay_port)),
            policy: Some(m.policy.clone()),
            ..MatchExtras::default()
        };
        let host = if extras.spectator_host.is_some() { 0 } else { host };
        match create_match(core, m.origin_kind, &m.origin_id, rules, seats, host, false, extras) {
            Ok(id) => {
                if let Some(o) = m.outcome.as_mut() {
                    o["rematch_id"] = json!(id);
                }
                save(core, &m)?;
                replacement = Some(id);
            }
            Err(e) => tracing::error!(match_id = %m.id, error = %e.message, "redraw failed"),
        }
    }
    let kind = if d.counted { "match.completed" } else { "match.void" };
    notify_ended(core, &m, kind);
    core.emit(kind, match_json(core, &m, None));
    if !rating_changes.is_empty() {
        crate::api::ratings::notify_changes(core, &rating_changes);
    }
    if m.origin_kind == "room" {
        match &replacement {
            Some(id) => {
                if let Some(r) = core.live.rooms.get_mut(&m.origin_id) {
                    r.match_id = Some(id.clone());
                }
            }
            None => crate::api::rooms::on_match_end(core, &m.origin_id),
        }
    }
    for u in m.users() {
        core.refresh_presence(&u);
    }
    tracing::info!(match_id = %m.id, state = m.state.as_str(), decision = d.decision, rated = d.rated, "match decided");
    if core.cfg.features.badges {
        crate::badges::on_match_end(core, &m, &d);
    }
    if m.game.series.is_some() {
        crate::series::on_game_end(core, &m, &d)?;
    }
    Ok(())
}

/// Time-based transitions: accept deadlines, launch deadlines, result deadlines and grace periods.
pub fn tick(core: &mut Core) {
    let now = core.now;
    let ids: Vec<(String, MState, Option<i64>)> = core
        .live
        .matches
        .values()
        .map(|m| (m.id.clone(), m.state, m.accept_deadline))
        .collect();
    for (id, state, accept) in ids {
        let r = if state == MState::PendingAccept {
            if accept.is_some_and(|t| now >= t) {
                cancel_pending(core, &id, "accept_timeout")
            } else {
                Ok(())
            }
        } else {
            maybe_decide(core, &id)
        };
        if let Err(e) = r {
            tracing::error!(match_id = %id, error = %e.message, "match transition failed");
        }
    }
}

/// Deletes raw reports and statuses of matches ended before the retention period (the decision is kept).
pub fn purge_raw(core: &Core) -> rusqlite::Result<usize> {
    let before = core.now - core.cfg.retention.raw_reports_days * 86_400;
    let players = core.db.execute(
        "UPDATE match_players SET report = NULL, status = NULL
         WHERE (report IS NOT NULL OR status IS NOT NULL)
           AND match_id IN (SELECT id FROM matches WHERE ended_at IS NOT NULL AND ended_at < ?1)",
        [before],
    )?;
    // 1.1.7: the spectators' witness reports and statuses go with the players'.
    let watchers = core.db.execute(
        "UPDATE match_spectators SET report = NULL, status = NULL
         WHERE (report IS NOT NULL OR status IS NOT NULL)
           AND match_id IN (SELECT id FROM matches WHERE ended_at IS NOT NULL AND ended_at < ?1)",
        [before],
    )?;
    Ok(players + watchers)
}
