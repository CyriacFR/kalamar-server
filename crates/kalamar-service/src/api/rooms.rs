//! Private rooms (`api.md`, "Private rooms"): a creation form declared from the configuration, codes, passwords,
//! invitations, teams, ready, start. In memory only.

use std::collections::HashSet;
use std::time::Instant;

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use kalamar_protocol::Team;
use serde::Deserialize;
use serde_json::{Map, Value, json};
use subtle::ConstantTimeEq;

use crate::auth::Authed;
use crate::config::Config;
use crate::error::{ApiError, ApiResult, JsonBody};
use crate::matches::{MatchExtras, NewSeat, create_match};
use crate::state::{AppState, Core, MState, MatchRules, Room, RoomMember};
use crate::util::{new_code, new_id, random_index, rfc3339};

const CODE_LEN: usize = 6;

fn need(core: &Core) -> ApiResult<()> {
    if core.cfg.features.rooms {
        Ok(())
    } else {
        Err(ApiError::feature_disabled())
    }
}

/// The room declaration of `GET /v1/meta`.
pub fn declaration(c: &Config) -> Value {
    let r = &c.rooms;
    let mut options = vec![json!({
        "key": "teams", "type": "teams", "label": {"en": "Format", "fr": "Format"}, "required": true,
        "default": r.default_teams, "choices": r.teams_choices,
    })];
    let label_of =
        |id: &str, list: &[crate::config::CatalogEntry]| list.iter().find(|e| e.id == id).map(|e| json!(e.label));
    if r.modes.len() > 1 {
        let values: Vec<Value> = r
            .modes
            .iter()
            .map(|m| {
                let mut v = json!({"id": m});
                if let Some(l) = label_of(m, &c.catalog.modes) {
                    v["label"] = l;
                }
                v
            })
            .collect();
        options.push(
            json!({"key": "mode", "type": "mode", "label": {"en": "Mode", "fr": "Mode"}, "required": true,
                            "default": r.default_mode(), "values": values}),
        );
    }
    if !r.stages.is_empty() {
        let values: Vec<Value> = r
            .stages
            .iter()
            .map(|s| {
                let mut v = json!({"id": s});
                if let Some(l) = label_of(s, &c.catalog.stages) {
                    v["label"] = l;
                }
                v
            })
            .collect();
        options.push(
            json!({"key": "stage", "type": "stage", "label": {"en": "Stage", "fr": "Stage"}, "required": false,
                            "nullable": true, "null_label": {"en": "Game's choice", "fr": "Au choix du jeu"},
                            "default": null, "values": values}),
        );
    }
    if !r.loadouts.is_empty() {
        let values: Vec<Value> = r
            .loadouts
            .iter()
            .map(|l| json!({"id": l.id, "label": l.label}))
            .collect();
        options.push(json!({"key": "loadout", "type": "enum", "label": {"en": "Imposed weapon and gear", "fr": "Arme et équipement imposés"},
                            "required": false, "nullable": true, "null_label": {"en": "Free", "fr": "Libre"},
                            "default": null, "values": values}));
    }
    // 1.1.6: the drafts offered (`draft.md`); with one, the draft decides the stage (and the mode if it drafts modes).
    let drafts: Vec<Value> = r
        .drafts
        .iter()
        .filter_map(|id| {
            crate::draft::queue_member(c, Some(id))
                .as_object()
                .cloned()
                .map(Value::Object)
        })
        .collect();
    if !drafts.is_empty() {
        options.push(json!({"key": "draft", "type": "enum", "label": {"en": "Stage draft", "fr": "Choix des stages"},
                            "help": {"en": "Teams ban and pick stages before the match.", "fr": "Les équipes bannissent et choisissent les stages avant le match."},
                            "required": false, "nullable": true, "null_label": {"en": "No draft", "fr": "Sans choix"},
                            "default": null, "values": drafts}));
    }
    // 1.1.7: places in the stands (`spectators.md`), as ordinary integer and enum fields: a launcher that does not know
    // the stands still draws the form.
    let st = &r.spectators;
    let stands = c.features.spectators && st.max > 0;
    if stands {
        options.push(json!({"key": "spectators", "type": "integer",
            "label": {"en": "Places for spectators", "fr": "Places pour spectateurs", "es": "Plazas para espectadores", "ja": "観戦席の数"},
            "help": {"en": "Members who watch the match from the stands instead of playing (0 to close the stands).",
                     "fr": "Des membres regardent le match depuis la tribune au lieu de jouer (0 pour fermer la tribune)."},
            "required": false, "min": 0, "max": st.max, "default": st.default}));
        if st.who.len() > 1 {
            let values: Vec<Value> = st
                .who
                .iter()
                .map(|w| json!({"id": w, "label": crate::spectate::who_label(w)}))
                .collect();
            options.push(json!({"key": "spectators_who", "type": "enum",
                "label": {"en": "Who may watch", "fr": "Qui peut regarder", "es": "Quién puede mirar", "ja": "観戦できる人"},
                "required": false, "default": st.who[0], "values": values}));
        }
    }
    if r.password {
        options.push(
            json!({"key": "password", "type": "string", "label": {"en": "Password", "fr": "Mot de passe"},
                            "required": false, "secret": true, "min_len": 1, "max_len": 32}),
        );
    }
    let mut d = json!({
        "options": options,
        "min_players": r.min_players,
        "result_policy": r.result_policy,
        "ladder": null,
        "invites": r.invites,
        "idle_close_s": r.idle_close_s,
    });
    if stands {
        d["spectators"] = json!({
            "max": st.max, "who": st.who, "host": st.host,
            "wait_s": st.wait_s.unwrap_or(c.spectators.wait_s),
            "late": false, "opt_out": c.spectators.opt_out, "summary": c.spectators.summary,
        });
    }
    d
}

struct Opts {
    teams: [usize; 2],
    mode: String,
    stage: Option<String>,
    password: Option<Option<String>>,
    loadout: Option<Option<String>>,
    draft: Option<Option<String>>,
    /// 1.1.7: places in the stands, and who may take them.
    spectators: usize,
    spectators_who: String,
}

/// Reads `options` against the declaration; keys absent keep `base`'s values (or the defaults).
fn read_options(c: &Config, options: &Map<String, Value>, base: Option<&Room>) -> ApiResult<Opts> {
    let r = &c.rooms;
    let mut o = Opts {
        teams: base.map(|b| b.teams).unwrap_or(r.default_teams),
        mode: base
            .map(|b| b.mode.clone())
            .unwrap_or_else(|| r.default_mode().to_string()),
        stage: base.and_then(|b| b.stage.clone()),
        password: None,
        loadout: None,
        draft: None,
        spectators: base.map(|b| b.spectators).unwrap_or(if c.features.spectators {
            r.spectators.default.min(r.spectators.max)
        } else {
            0
        }),
        spectators_who: base
            .map(|b| b.spectators_who.clone())
            .or_else(|| r.spectators.who.first().cloned())
            .unwrap_or_else(|| "anyone".into()),
    };
    let stands = c.features.spectators && r.spectators.max > 0;
    for (k, v) in options {
        match k.as_str() {
            "spectators" if stands => match v.as_u64() {
                Some(n) if n as usize <= r.spectators.max => o.spectators = n as usize,
                _ => {
                    return Err(ApiError::invalid(
                        "/options/spectators",
                        format!("0 to {} places", r.spectators.max),
                    ));
                }
            },
            "spectators_who" if stands => match v.as_str() {
                Some(w) if r.spectators.who.iter().any(|x| x == w) => o.spectators_who = w.to_string(),
                _ => {
                    return Err(ApiError::invalid(
                        "/options/spectators_who",
                        "one of the declared values",
                    ));
                }
            },
            "teams" => {
                let t: Option<[usize; 2]> = serde_json::from_value(v.clone()).ok();
                match t {
                    Some(t) if r.teams_choices.contains(&t) => o.teams = t,
                    _ => return Err(ApiError::invalid("/options/teams", "one of the declared choices")),
                }
            }
            "mode" if r.modes.len() > 1 => match v.as_str() {
                Some(m) if r.modes.iter().any(|x| x == m) => o.mode = m.to_string(),
                _ => return Err(ApiError::invalid("/options/mode", "one of the declared modes")),
            },
            "stage" if !r.stages.is_empty() => match v {
                Value::Null => o.stage = None,
                Value::String(s) if r.stages.contains(s) => o.stage = Some(s.clone()),
                _ => {
                    return Err(ApiError::invalid(
                        "/options/stage",
                        "null or one of the declared stages",
                    ));
                }
            },
            "loadout" if !r.loadouts.is_empty() => match v {
                Value::Null => o.loadout = Some(None),
                Value::String(s) if r.loadouts.iter().any(|l| &l.id == s) => o.loadout = Some(Some(s.clone())),
                _ => {
                    return Err(ApiError::invalid(
                        "/options/loadout",
                        "null or one of the declared loadouts",
                    ));
                }
            },
            "draft" if r.drafts.iter().any(|d| c.draft(d).is_some()) => match v {
                Value::Null => o.draft = Some(None),
                Value::String(s) if r.drafts.contains(s) && c.draft(s).is_some() => o.draft = Some(Some(s.clone())),
                _ => {
                    return Err(ApiError::invalid(
                        "/options/draft",
                        "null or one of the declared drafts",
                    ));
                }
            },
            "password" if r.password => match v {
                Value::Null => o.password = Some(None),
                Value::String(s) if (1..=32).contains(&s.chars().count()) => o.password = Some(Some(s.clone())),
                _ => return Err(ApiError::invalid("/options/password", "1 to 32 characters, or null")),
            },
            x if x.starts_with("x-") => {}
            _ => return Err(ApiError::invalid(&format!("/options/{k}"), "not a declared option")),
        }
    }
    Ok(o)
}

pub fn room_json(core: &Core, r: &Room) -> Value {
    let members: Vec<Value> = r
        .members
        .iter()
        .map(|m| {
            // 1.1.7: a member in the stands has the team `spectator` (an open enum: launchers that do not know it show
            // the member in no team).
            let team = if m.spectator { "spectator" } else { m.team.as_str() };
            json!({"user": core.user_json(&m.user_id), "team": team, "ready": m.ready, "joined_at": rfc3339(m.joined_at)})
        })
        .collect();
    let mut options =
        json!({"teams": r.teams, "mode": r.mode, "stage": r.stage, "loadout": r.loadout, "draft": r.draft});
    let stands = core.cfg.features.spectators && core.cfg.rooms.spectators.max > 0;
    if stands {
        options["spectators"] = json!(r.spectators);
        options["spectators_who"] = json!(r.spectators_who);
    }
    let mut v = json!({
        "id": r.id,
        "code": r.code,
        "owner": r.owner,
        "options": options,
        "has_password": r.password.is_some(),
        "state": if r.match_id.is_some() { "in_match" } else { "open" },
        "members": members,
        "match_id": r.match_id,
        "created_at": rfc3339(r.created_at),
        "closes_at": rfc3339(r.last_activity + core.cfg.rooms.idle_close_s),
    });
    if stands {
        let taken = r.members.iter().filter(|m| m.spectator).count();
        v["spectators"] = json!({"seats": r.spectators, "taken": taken, "who": r.spectators_who,
                                 "host": core.cfg.rooms.spectators.host});
    }
    v
}

/// 1.1.7: whether `user`, a member of `r`, may sit in its stands (`who`); the operator's referees always may.
fn may_watch(core: &Core, r: &Room, user: &str) -> bool {
    if crate::spectate::is_referee(&core.cfg, user) {
        return true;
    }
    let invited = r.members.iter().any(|m| m.user_id == user && m.invited)
        || core
            .live
            .room_invites
            .get(&(r.id.clone(), user.to_string()))
            .is_some_and(|(_, exp)| *exp > core.now);
    match r.spectators_who.as_str() {
        "anyone" => true,
        "invited" => invited || r.owner == user,
        "friends" => r.owner == user || core.are_friends(&r.owner, user),
        _ => false,
    }
}

/// 1.1.7: a member moves into the stands (or joins there): a free place, `who`, and a game and launcher that can watch
/// (and every player's: a match with spectators needs every participant's game to know them).
fn check_watch(core: &Core, r: &Room, user: &str) -> ApiResult<()> {
    if !core.cfg.features.spectators || r.spectators == 0 {
        return Err(ApiError::conflict(
            "spectators_closed",
            "this room has no places in the stands",
        ));
    }
    if r.members.iter().filter(|m| m.spectator && m.user_id != user).count() >= r.spectators {
        return Err(ApiError::conflict(
            "spectators_full",
            "every place in the stands is taken",
        ));
    }
    if !may_watch(core, r, user) {
        return Err(
            ApiError::forbidden("spectators_not_allowed", "the room's stands are reserved")
                .details(json!({"who": r.spectators_who})),
        );
    }
    let n = r.members.iter().filter(|m| m.spectator && m.user_id != user).count() + 1;
    let host_spectator = core.cfg.rooms.spectators.host && r.owner == user;
    let mut players = Vec::new();
    for m in &r.members {
        let watch = m.user_id == user || m.spectator;
        let missing = crate::api::account::game_caps(core, &m.user_id).spectator_missing(
            &r.mode,
            n,
            core.cfg.spectators.verified_modes_only,
            watch && host_spectator && m.user_id == r.owner,
        );
        if !missing.is_empty() {
            players.push(json!({"user_id": m.user_id, "missing": missing}));
        }
    }
    if !r.members.iter().any(|m| m.user_id == user) {
        let missing = crate::api::account::game_caps(core, user).spectator_missing(
            &r.mode,
            n,
            core.cfg.spectators.verified_modes_only,
            false,
        );
        if !missing.is_empty() {
            players.push(json!({"user_id": user, "missing": missing}));
        }
    }
    if !players.is_empty() {
        return Err(
            ApiError::conflict("incompatible_game", "a game in this room cannot play with spectators")
                .text(
                    "A launcher or game in this room cannot play with spectators.",
                    "Un lanceur ou un jeu de ce salon ne sait pas jouer avec des spectateurs.",
                )
                .details(json!({"players": players})),
        );
    }
    Ok(())
}

pub fn room_of_json(core: &Core, user: &str) -> Value {
    core.live
        .user_room
        .get(user)
        .and_then(|id| core.live.rooms.get(id))
        .map(|r| room_json(core, r))
        .unwrap_or(Value::Null)
}

pub fn invites_for(core: &Core, user: &str) -> Vec<Value> {
    core.live
        .room_invites
        .iter()
        .filter(|((_, u), (_, exp))| u == user && *exp > core.now)
        .filter_map(|((rid, _), (from, exp))| {
            let r = core.live.rooms.get(rid)?;
            Some(json!({"room_id": rid, "code": r.code, "from": core.user_json(from), "expires_at": rfc3339(*exp)}))
        })
        .collect()
}

fn broadcast(core: &mut Core, room_id: &str) {
    let Some(r) = core.live.rooms.get(room_id) else { return };
    let data = room_json(core, r);
    let members: Vec<String> = r.members.iter().map(|m| m.user_id.clone()).collect();
    core.push_many(&members, "room.updated", &data);
}

fn close_room(core: &mut Core, room_id: &str, reason: &str) {
    if let Some(r) = core.live.rooms.remove(room_id) {
        core.live.room_codes.remove(&r.code);
        core.live.room_invites.retain(|(x, _), _| x != room_id);
        for m in &r.members {
            core.live.user_room.remove(&m.user_id);
            core.push(&m.user_id, "room.closed", json!({"room_id": room_id, "reason": reason}));
        }
    }
}

/// Removes `user` from their room (leave, kick, erasure); ownership passes on; an empty room closes.
pub fn remove_from_room(core: &mut Core, user: &str, reason: &str) {
    let Some(rid) = core.live.user_room.remove(user) else {
        return;
    };
    let now = core.now;
    let empty = match core.live.rooms.get_mut(&rid) {
        Some(r) => {
            r.members.retain(|m| m.user_id != user);
            if r.owner == user
                && let Some(m) = r.members.first()
            {
                r.owner = m.user_id.clone();
            }
            r.last_activity = now;
            r.members.is_empty()
        }
        None => true,
    };
    core.push(user, "room.closed", json!({"room_id": rid, "reason": reason}));
    if empty {
        close_room(core, &rid, "empty");
    } else {
        broadcast(core, &rid);
    }
}

/// Called when a room's match ended: the room opens again with every `ready` reset.
pub fn on_match_end(core: &mut Core, room_id: &str) {
    let now = core.now;
    if let Some(r) = core.live.rooms.get_mut(room_id) {
        r.match_id = None;
        r.last_activity = now;
        for m in &mut r.members {
            m.ready = false;
        }
    }
    broadcast(core, room_id);
}

/// Closes idle rooms (called by the ticker).
pub fn sweep(core: &mut Core) {
    let idle = core.cfg.rooms.idle_close_s;
    let now = core.now;
    let stale: Vec<String> = core
        .live
        .rooms
        .values()
        .filter(|r| r.match_id.is_none() && now - r.last_activity >= idle)
        .map(|r| r.id.clone())
        .collect();
    for id in stale {
        close_room(core, &id, "idle");
    }
    core.live.room_invites.retain(|_, (_, exp)| *exp > now);
}

fn in_active_match(core: &Core, user: &str) -> bool {
    core.live
        .user_match
        .get(user)
        .and_then(|m| core.live.matches.get(m))
        .is_some_and(|m| !m.state.ended())
}

#[derive(Deserialize)]
pub struct OptionsBody {
    #[serde(default)]
    options: Map<String, Value>,
}

/// `POST /v1/rooms`.
pub async fn create(
    State(st): State<AppState>,
    a: Authed,
    JsonBody(b): JsonBody<OptionsBody>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let mut core = st.lock();
    need(&core)?;
    if core.live.user_room.contains_key(&a.user) {
        return Err(ApiError::conflict("already_in_room", "leave your room first"));
    }
    if in_active_match(&core, &a.user) {
        return Err(ApiError::conflict("in_match", "you are in a match"));
    }
    if core.live.rooms.len() >= core.cfg.limits.max_rooms {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "too many rooms",
        ));
    }
    let o = read_options(&core.cfg, &b.options, None)?;
    let mut code = new_code(CODE_LEN);
    while core.live.room_codes.contains_key(&code) {
        code = new_code(CODE_LEN);
    }
    let id = new_id("r");
    let now = core.now;
    let room = Room {
        id: id.clone(),
        code: code.clone(),
        owner: a.user.clone(),
        teams: o.teams,
        mode: o.mode,
        stage: o.stage,
        password: o.password.flatten(),
        loadout: o.loadout.flatten(),
        draft: o.draft.flatten(),
        spectators: o.spectators,
        spectators_who: o.spectators_who,
        members: vec![RoomMember {
            user_id: a.user.clone(),
            team: Team::Alpha,
            ready: false,
            joined_at: now,
            spectator: false,
            invited: false,
        }],
        kicked: HashSet::new(),
        match_id: None,
        created_at: now,
        last_activity: now,
    };
    core.live.rooms.insert(id.clone(), room);
    core.live.room_codes.insert(code, id.clone());
    core.live.user_room.insert(a.user.clone(), id.clone());
    Ok((StatusCode::CREATED, Json(room_json(&core, &core.live.rooms[&id]))))
}

/// `GET /v1/rooms/current`.
pub async fn current(State(st): State<AppState>, a: Authed) -> ApiResult<Json<Value>> {
    let core = st.lock();
    need(&core)?;
    Ok(Json(json!({"room": room_of_json(&core, &a.user)})))
}

fn member_room<'a>(core: &'a Core, user: &str, room_id: &str) -> ApiResult<&'a Room> {
    core.live
        .rooms
        .get(room_id)
        .filter(|r| r.members.iter().any(|m| m.user_id == user))
        .ok_or_else(|| ApiError::not_found("no such room"))
}

/// `GET /v1/rooms/{room_id}`.
pub async fn get(State(st): State<AppState>, a: Authed, Path(rid): Path<String>) -> ApiResult<Json<Value>> {
    let core = st.lock();
    need(&core)?;
    let r = member_room(&core, &a.user, &rid)?;
    Ok(Json(room_json(&core, r)))
}

fn owner_open(core: &Core, user: &str, rid: &str) -> ApiResult<()> {
    let r = member_room(core, user, rid)?;
    if r.owner != user {
        return Err(ApiError::forbidden("not_owner", "only the room owner can do this"));
    }
    if r.match_id.is_some() {
        return Err(ApiError::conflict("room_state", "the room is in a match"));
    }
    Ok(())
}

/// `PATCH /v1/rooms/{room_id}`.
pub async fn patch(
    State(st): State<AppState>,
    a: Authed,
    Path(rid): Path<String>,
    JsonBody(b): JsonBody<OptionsBody>,
) -> ApiResult<Json<Value>> {
    let mut core = st.lock();
    need(&core)?;
    owner_open(&core, &a.user, &rid)?;
    let o = read_options(&core.cfg, &b.options, core.live.rooms.get(&rid))?;
    let now = core.now;
    let r = core
        .live
        .rooms
        .get_mut(&rid)
        .ok_or_else(|| ApiError::not_found("no such room"))?;
    for (i, t) in [Team::Alpha, Team::Bravo].iter().enumerate() {
        if r.members.iter().filter(|m| m.team == *t && !m.spectator).count() > o.teams[i] {
            return Err(ApiError::invalid(
                "/options/teams",
                "the current members do not fit these teams",
            ));
        }
    }
    if r.members.iter().filter(|m| m.spectator).count() > o.spectators {
        return Err(ApiError::invalid(
            "/options/spectators",
            "the members in the stands do not fit these places",
        ));
    }
    r.teams = o.teams;
    r.mode = o.mode;
    r.stage = o.stage;
    r.spectators = o.spectators;
    r.spectators_who = o.spectators_who;
    if let Some(p) = o.password {
        r.password = p;
    }
    if let Some(l) = o.loadout {
        r.loadout = l;
    }
    if let Some(d) = o.draft {
        r.draft = d;
    }
    r.last_activity = now;
    for m in &mut r.members {
        m.ready = false;
    }
    broadcast(&mut core, &rid);
    Ok(Json(room_json(&core, &core.live.rooms[&rid])))
}

#[derive(Deserialize)]
pub struct JoinBody {
    code: String,
    #[serde(default)]
    password: Option<String>,
    /// 1.1.7: `spectator` to join in the stands (a launcher that runs the spectator seat); else a team.
    #[serde(default)]
    seat: Option<String>,
}

/// `POST /v1/rooms/join`.
pub async fn join(State(st): State<AppState>, a: Authed, JsonBody(b): JsonBody<JoinBody>) -> ApiResult<Json<Value>> {
    {
        let core = st.lock();
        need(&core)?;
    }
    if let Err(retry) = st.limits.room_join.take(&a.user, Instant::now()) {
        return Err(ApiError::rate_limited(retry));
    }
    let mut core = st.lock();
    if core.live.user_room.contains_key(&a.user) {
        return Err(ApiError::conflict("already_in_room", "leave your room first"));
    }
    if in_active_match(&core, &a.user) {
        return Err(ApiError::conflict("in_match", "you are in a match"));
    }
    let code = kalamar_protocol::contract::normalize_code(&b.code);
    let rid = core
        .live
        .room_codes
        .get(&code)
        .cloned()
        .ok_or_else(|| ApiError::not_found("no room has this code"))?;
    let invited = core
        .live
        .room_invites
        .get(&(rid.clone(), a.user.clone()))
        .is_some_and(|(_, exp)| *exp > core.now);
    let now = core.now;
    let r = core
        .live
        .rooms
        .get(&rid)
        .ok_or_else(|| ApiError::not_found("no room has this code"))?;
    if r.kicked.contains(&a.user) {
        return Err(ApiError::forbidden("kicked", "you were removed from this room"));
    }
    if let Some(pw) = &r.password {
        let given = b.password.unwrap_or_default();
        if !invited && !bool::from(pw.as_bytes().ct_eq(given.as_bytes())) {
            return Err(ApiError::forbidden("wrong_password", "wrong password"));
        }
    }
    if r.match_id.is_some() {
        return Err(ApiError::conflict("room_state", "the room is in a match"));
    }
    let count = |t: Team| r.members.iter().filter(|m| m.team == t && !m.spectator).count();
    let (na, nb) = (count(Team::Alpha), count(Team::Bravo));
    // 1.1.7: only a launcher that asks for it joins in the stands; one that does not know them never ends up there.
    let watch = b.seat.as_deref() == Some("spectator");
    if let Some(s) = b.seat.as_deref().filter(|s| !matches!(*s, "spectator" | "player")) {
        return Err(ApiError::invalid("/seat", format!("`{s}`: spectator or player")));
    }
    let team = if watch {
        check_watch(&core, r, &a.user)?;
        Team::Alpha
    } else if na < r.teams[0] && (na <= nb || nb >= r.teams[1]) {
        Team::Alpha
    } else if nb < r.teams[1] {
        Team::Bravo
    } else {
        return Err(ApiError::conflict("room_full", "the room is full"));
    };
    if r.members.iter().any(|m| core.blocked_between(&m.user_id, &a.user)) {
        return Err(ApiError::forbidden("blocked", "you cannot join this room"));
    }
    let r = core
        .live
        .rooms
        .get_mut(&rid)
        .ok_or_else(|| ApiError::not_found("no such room"))?;
    r.members.push(RoomMember {
        user_id: a.user.clone(),
        team,
        ready: false,
        joined_at: now,
        spectator: watch,
        invited,
    });
    r.last_activity = now;
    core.live.room_invites.remove(&(rid.clone(), a.user.clone()));
    core.live.user_room.insert(a.user.clone(), rid.clone());
    broadcast(&mut core, &rid);
    Ok(Json(room_json(&core, &core.live.rooms[&rid])))
}

/// `POST /v1/rooms/{room_id}/leave`.
pub async fn leave(State(st): State<AppState>, a: Authed, Path(rid): Path<String>) -> ApiResult<StatusCode> {
    let mut core = st.lock();
    need(&core)?;
    member_room(&core, &a.user, &rid)?;
    remove_from_room(&mut core, &a.user, "left");
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct UserBody {
    user_id: String,
}

/// `POST /v1/rooms/{room_id}/invites`.
pub async fn invite(
    State(st): State<AppState>,
    a: Authed,
    Path(rid): Path<String>,
    JsonBody(b): JsonBody<UserBody>,
) -> ApiResult<StatusCode> {
    let mut core = st.lock();
    need(&core)?;
    if !core.cfg.rooms.invites {
        return Err(ApiError::feature_disabled());
    }
    let code = member_room(&core, &a.user, &rid)?.code.clone();
    if b.user_id == a.user || !core.user_exists(&b.user_id) {
        return Err(ApiError::not_found("no such user"));
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
    let exp = core.now + core.cfg.limits.room_invite_ttl_s;
    core.live
        .room_invites
        .insert((rid.clone(), b.user_id.clone()), (a.user.clone(), exp));
    let from = core.user_json(&a.user);
    core.push(
        &b.user_id,
        "room.invite",
        json!({"room_id": rid, "code": code, "from": from, "expires_at": rfc3339(exp)}),
    );
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/rooms/{room_id}/kick`.
pub async fn kick(
    State(st): State<AppState>,
    a: Authed,
    Path(rid): Path<String>,
    JsonBody(b): JsonBody<UserBody>,
) -> ApiResult<Json<Value>> {
    let mut core = st.lock();
    need(&core)?;
    let r = member_room(&core, &a.user, &rid)?;
    if r.owner != a.user {
        return Err(ApiError::forbidden("not_owner", "only the room owner can do this"));
    }
    if b.user_id == a.user || !r.members.iter().any(|m| m.user_id == b.user_id) {
        return Err(ApiError::not_found("not a member of this room"));
    }
    if let Some(r) = core.live.rooms.get_mut(&rid) {
        r.kicked.insert(b.user_id.clone());
    }
    remove_from_room(&mut core, &b.user_id, "kicked");
    Ok(Json(room_json(&core, &core.live.rooms[&rid])))
}

/// `POST /v1/rooms/{room_id}/owner`.
pub async fn owner(
    State(st): State<AppState>,
    a: Authed,
    Path(rid): Path<String>,
    JsonBody(b): JsonBody<UserBody>,
) -> ApiResult<Json<Value>> {
    let mut core = st.lock();
    need(&core)?;
    let r = member_room(&core, &a.user, &rid)?;
    if r.owner != a.user {
        return Err(ApiError::forbidden("not_owner", "only the room owner can do this"));
    }
    if !r.members.iter().any(|m| m.user_id == b.user_id) {
        return Err(ApiError::not_found("not a member of this room"));
    }
    if let Some(r) = core.live.rooms.get_mut(&rid) {
        r.owner = b.user_id.clone();
    }
    broadcast(&mut core, &rid);
    Ok(Json(room_json(&core, &core.live.rooms[&rid])))
}

#[derive(Deserialize)]
pub struct TeamBody {
    team: String,
}

/// `PUT /v1/rooms/{room_id}/members/{user_id}`.
pub async fn set_team(
    State(st): State<AppState>,
    a: Authed,
    Path((rid, user)): Path<(String, String)>,
    JsonBody(b): JsonBody<TeamBody>,
) -> ApiResult<Json<Value>> {
    let mut core = st.lock();
    need(&core)?;
    // 1.1.7: `spectator` moves the member into the stands (and a team back out of them).
    let watch = b.team == "spectator";
    let team = if watch {
        None
    } else {
        Some(Team::parse(&b.team).ok_or_else(|| ApiError::invalid("/team", "alpha, bravo or spectator"))?)
    };
    let r = member_room(&core, &a.user, &rid)?;
    if user != a.user && r.owner != a.user {
        return Err(ApiError::forbidden(
            "not_owner",
            "only the room owner can move other players",
        ));
    }
    if r.match_id.is_some() {
        return Err(ApiError::conflict("room_state", "the room is in a match"));
    }
    let Some(cur) = r.members.iter().find(|m| m.user_id == user) else {
        return Err(ApiError::not_found("not a member of this room"));
    };
    match team {
        None if !cur.spectator => check_watch(&core, r, &user)?,
        Some(t) if cur.team != t || cur.spectator => {
            let idx = if t == Team::Alpha { 0 } else { 1 };
            if r.members.iter().filter(|m| m.team == t && !m.spectator).count() >= r.teams[idx] {
                return Err(ApiError::conflict("team_full", "this team is full"));
            }
        }
        _ => {}
    }
    let now = core.now;
    if let Some(r) = core.live.rooms.get_mut(&rid) {
        r.last_activity = now;
        if let Some(m) = r.members.iter_mut().find(|m| m.user_id == user) {
            let changed = match team {
                None => !m.spectator,
                Some(t) => m.spectator || m.team != t,
            };
            if changed {
                if let Some(t) = team {
                    m.team = t;
                }
                m.spectator = watch;
                m.ready = false;
            }
        }
    }
    broadcast(&mut core, &rid);
    Ok(Json(room_json(&core, &core.live.rooms[&rid])))
}

#[derive(Deserialize)]
pub struct ReadyBody {
    ready: bool,
}

/// `POST /v1/rooms/{room_id}/ready`.
pub async fn ready(
    State(st): State<AppState>,
    a: Authed,
    Path(rid): Path<String>,
    JsonBody(b): JsonBody<ReadyBody>,
) -> ApiResult<Json<Value>> {
    let mut core = st.lock();
    need(&core)?;
    let r = member_room(&core, &a.user, &rid)?;
    if r.match_id.is_some() {
        return Err(ApiError::conflict("room_state", "the room is in a match"));
    }
    let now = core.now;
    if let Some(r) = core.live.rooms.get_mut(&rid) {
        r.last_activity = now;
        if let Some(m) = r.members.iter_mut().find(|m| m.user_id == a.user) {
            m.ready = b.ready;
        }
    }
    broadcast(&mut core, &rid);
    Ok(Json(room_json(&core, &core.live.rooms[&rid])))
}

/// `POST /v1/rooms/{room_id}/start`.
pub async fn start(
    State(st): State<AppState>,
    a: Authed,
    Path(rid): Path<String>,
) -> ApiResult<(StatusCode, Json<Value>)> {
    let mut core = st.lock();
    need(&core)?;
    owner_open(&core, &a.user, &rid)?;
    let r = &core.live.rooms[&rid];
    if r.members.iter().any(|m| m.user_id != a.user && !m.ready) {
        return Err(ApiError::conflict("not_ready", "every player must be ready"));
    }
    // 1.1.7: spectators never count as players: the minimum and both teams are the players'.
    let count = |t: Team| r.members.iter().filter(|m| m.team == t && !m.spectator).count();
    let (na, nb) = (count(Team::Alpha), count(Team::Bravo));
    if na + nb < core.cfg.rooms.min_players || na == 0 || nb == 0 {
        return Err(ApiError::conflict(
            "not_enough_players",
            "not enough players, or a team is empty",
        ));
    }
    // Security review M4: each start mints a room code of the relay (sealed codes are valid 24 to 48 hours) and stores
    // a match; two accounts started 90 in a second. Limited per owner (`rate_limits.room_starts_per_hour`, 0: none).
    if core.cfg.rate_limits.room_starts_per_hour > 0.0
        && let Err(retry) = st.limits.room_starts.take(&a.user, std::time::Instant::now())
    {
        return Err(ApiError::rate_limited(retry));
    }
    if let Some(m) = r.members.iter().find(|m| in_active_match(&core, &m.user_id)) {
        return Err(ApiError::conflict(
            "in_match",
            format!("{} is in another match", core.display_name(&m.user_id)),
        ));
    }
    let players = na + nb;
    let watchers: Vec<&RoomMember> = r.members.iter().filter(|m| m.spectator).collect();
    // The host: the owner unless configured otherwise; an owner in the stands hosts only when the operator allows it.
    let owner_watches = watchers.iter().any(|m| m.user_id == r.owner);
    let spectator_host = (owner_watches && core.cfg.rooms.spectators.host && core.cfg.rooms.host != "random")
        .then(|| watchers.iter().position(|m| m.user_id == r.owner))
        .flatten();
    if !watchers.is_empty() {
        if !core.cfg.features.spectators || watchers.len() > r.spectators {
            return Err(ApiError::conflict(
                "spectators_closed",
                "this room has no such places in the stands",
            ));
        }
        // Every member's game must play with spectators present (an older engine fails on the letter S): a clear
        // refusal naming what is missing, never a match bound to fail.
        let mut missing = Vec::new();
        for m in &r.members {
            let host_here = spectator_host.is_some() && m.user_id == r.owner;
            let items = crate::api::account::game_caps(&core, &m.user_id).spectator_missing(
                &r.mode,
                watchers.len(),
                core.cfg.spectators.verified_modes_only,
                host_here,
            );
            if !items.is_empty() {
                missing.push(json!({"user_id": m.user_id, "missing": items}));
            }
        }
        if !missing.is_empty() {
            return Err(ApiError::conflict(
                "incompatible_game",
                "a member's game or launcher cannot play with spectators",
            )
            .text(
                "A member's game or launcher cannot play with spectators: they come back to play, or leave the stands.",
                "Le jeu ou le lanceur d'un membre ne sait pas jouer avec des spectateurs : revenez jouer ou quittez la tribune.",
            )
            .details(json!({"players": missing})));
        }
        // A player who refuses to be watched (when the operator lets them), unless every spectator is a referee.
        let refusing = crate::spectate::refusing(
            &core,
            &r.members
                .iter()
                .filter(|m| !m.spectator)
                .map(|m| m.user_id.clone())
                .collect::<Vec<_>>(),
        );
        let all_referees = watchers
            .iter()
            .all(|m| crate::spectate::is_referee(&core.cfg, &m.user_id));
        if !refusing.is_empty() && !all_referees {
            return Err(
                ApiError::conflict("spectators_refused", "a player of this room refuses to be watched")
                    .text(
                        "A player of this room does not want to be watched.",
                        "Un joueur de ce salon ne souhaite pas être regardé.",
                    )
                    .details(json!({"players": refusing.iter().map(|u| json!({"user_id": u})).collect::<Vec<_>>()})),
            );
        }
    }
    let loadout = r
        .loadout
        .as_ref()
        .and_then(|id| core.cfg.rooms.loadouts.iter().find(|l| &l.id == id))
        .map(|l| l.loadout.clone());
    let draft = r.draft.as_ref().and_then(|id| core.cfg.draft(id));
    // Spectators neither draft nor carry a loadout (their game is checked above).
    for m in r.members.iter().filter(|m| !m.spectator) {
        if draft.is_some_and(|d| d.required) && !crate::api::account::game_caps(&core, &m.user_id).drafts() {
            return Err(ApiError::conflict(
                "incompatible_game",
                format!(
                    "{}'s launcher cannot show the stage draft",
                    core.display_name(&m.user_id)
                ),
            )
            .details(json!({"players": [{"user_id": m.user_id, "missing": ["launcher:draft"]}]})));
        }
        if loadout.is_some() && !crate::api::account::game_caps(&core, &m.user_id).match_loadout {
            return Err(ApiError::conflict(
                "incompatible_game",
                format!(
                    "{}'s launcher cannot impose a weapon and gear",
                    core.display_name(&m.user_id)
                ),
            )
            .details(json!({"players": [{"user_id": m.user_id, "missing": ["loadout:weapon"]}]})));
        }
        if !crate::api::account::game_caps(&core, &m.user_id).can_run(players, &r.mode, r.stage.as_deref()) {
            return Err(ApiError::conflict(
                "incompatible_game",
                format!("{}'s game cannot run these rules", core.display_name(&m.user_id)),
            ));
        }
    }
    // Alpha first, then bravo, each in joining order; the host is the owner unless configured otherwise.
    let mut seats: Vec<NewSeat> = Vec::new();
    for t in [Team::Alpha, Team::Bravo] {
        for m in r.members.iter().filter(|m| m.team == t && !m.spectator) {
            seats.push(NewSeat {
                user_id: m.user_id.clone(),
                team: t,
                party_id: None,
                ticket_id: None,
            });
        }
    }
    let host = if core.cfg.rooms.host == "random" {
        random_index(seats.len())
    } else {
        seats.iter().position(|s| s.user_id == r.owner).unwrap_or(0)
    };
    // 1.1.7: the places are taken in the room, so the match's are closed from the start (a room match is ready at
    // once): nobody joins the stands after the launch.
    let stands = (!watchers.is_empty() || r.spectators > 0)
        .then(|| crate::state::StandsRule {
            seats: r.spectators.max(watchers.len()),
            who: r.spectators_who.clone(),
            wait_s: core.cfg.rooms.spectators.wait_s.unwrap_or(core.cfg.spectators.wait_s),
            open: false,
        })
        .filter(|_| core.cfg.features.spectators);
    let new_watchers: Vec<crate::matches::NewSpectator> = watchers
        .iter()
        .map(|m| crate::matches::NewSpectator {
            user_id: m.user_id.clone(),
            referee: crate::spectate::is_referee(&core.cfg, &m.user_id),
        })
        .collect();
    let rules = MatchRules {
        teams: [na, nb],
        mode: r.mode.clone(),
        stage: r.stage.clone(),
        options: json!({}),
        rated: false,
        ladder: None,
        season: None,
        result_policy: core.cfg.rooms.result_policy.clone(),
        loadout,
        spectators: stands,
    };
    let extras = MatchExtras {
        draft: r.draft.clone().map(|preset| crate::draft::DraftPlan {
            preset,
            stages: core.cfg.rooms.stages.clone(),
            modes: core.cfg.rooms.modes.clone(),
            ..Default::default()
        }),
        spectators: new_watchers,
        spectator_host,
        ..MatchExtras::default()
    };
    let mid = create_match(&mut core, "room", &rid, rules, seats, host, false, extras)?;
    let now = core.now;
    if let Some(r) = core.live.rooms.get_mut(&rid) {
        r.match_id = Some(mid.clone());
        r.last_activity = now;
    }
    broadcast(&mut core, &rid);
    let m = &core.live.matches[&mid];
    debug_assert!(matches!(m.state, MState::Ready | MState::Drafting));
    Ok((
        StatusCode::CREATED,
        Json(crate::matches::match_json(&core, m, Some(&a.user))),
    ))
}
