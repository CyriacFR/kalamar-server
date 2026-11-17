//! The account (`api.md`, "Account"): me, settings, game capabilities, the state snapshot, export and erasure.

use axum::Json;
use axum::extract::{Path, Query, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::response::{IntoResponse, Response};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::auth::{Authed, AuthedAny, check_display_name};
use crate::error::{ApiError, ApiResult, JsonBody};
use crate::state::{AppState, Core};
use crate::util::{format_friend_code, new_id, rfc3339};

const SETTINGS: &[(&str, &str)] = &[
    ("share_presence", "presence"),
    ("allow_friend_requests", "friends"),
    ("public_profile", "stats"),
    ("show_on_leaderboards", "leaderboards"),
    // 1.1.7: a player may refuse to be watched, when the operator lets them (`[spectators] opt_out`).
    ("allow_spectators", "spectators"),
];

fn area_on(core: &Core, area: &str) -> bool {
    core.cfg.features.declared().contains(&area) && (area != "spectators" || core.cfg.spectators.opt_out)
}

/// The Me object.
pub fn me_json(core: &Core, user: &str) -> Value {
    let row = core
        .db
        .query_row(
            "SELECT display_name, created_at, friend_code, share_presence, allow_friend_requests, public_profile,
                    show_on_leaderboards, name_changed_at, banned_until, ban_reason, queue_cooldown_until, allow_spectators
             FROM users WHERE id = ?1",
            [user],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, String>(2)?,
                    [
                        r.get::<_, i64>(3)?,
                        r.get::<_, i64>(4)?,
                        r.get::<_, i64>(5)?,
                        r.get::<_, i64>(6)?,
                        r.get::<_, i64>(11)?,
                    ],
                    r.get::<_, Option<i64>>(7)?,
                    r.get::<_, Option<i64>>(8)?,
                    r.get::<_, Option<String>>(9)?,
                    r.get::<_, Option<i64>>(10)?,
                ))
            },
        )
        .optional()
        .ok()
        .flatten();
    let Some((name, created, code, flags, changed, banned, ban_reason, cooldown)) = row else {
        return Value::Null;
    };
    let logins: Vec<String> = core
        .db
        .prepare("SELECT DISTINCT provider FROM identities WHERE user_id = ?1 ORDER BY provider")
        .and_then(|mut s| s.query_map([user], |r| r.get(0)).map(|it| it.flatten().collect()))
        .unwrap_or_default();
    let mut settings = Map::new();
    for (i, (key, area)) in SETTINGS.iter().enumerate() {
        if area_on(core, area) {
            settings.insert((*key).into(), json!(flags[i] != 0));
        }
    }
    let mut restrictions = Vec::new();
    if let Some(until) = banned.filter(|u| *u > core.now) {
        restrictions.push(
            json!({"kind": "ban", "until": (until != i64::MAX).then(|| rfc3339(until)),
                                 "reason": ban_reason.map(|r| json!({"en": r}))}),
        );
    }
    if let Some(until) = cooldown.filter(|u| *u > core.now) {
        restrictions.push(json!({"kind": "queue_cooldown", "until": rfc3339(until),
                                 "reason": {"en": "Declined, missed or left queue matches"}}));
    }
    let allowed = changed
        .map(|t| t + core.cfg.limits.name_change_cooldown_s)
        .unwrap_or(0)
        .max(core.now);
    let mut me = json!({
        "id": user,
        "display_name": name,
        "created_at": rfc3339(created),
        "logins": logins,
        "settings": settings,
        "display_name_change": {"allowed_at": rfc3339(allowed)},
        "restrictions": restrictions,
    });
    if core.cfg.features.friends {
        me["friend_code"] = json!(format_friend_code(&code));
    }
    if core.cfg.features.badges {
        let (featured, title) = crate::badges::me_members(core, user);
        me["featured_badge"] = json!(featured);
        me["title"] = json!(title);
    }
    // 1.1.5: the account has a password `POST /v1/me/password` can change (never the address).
    if let Some(p) = crate::auth::password::me_member(core, user) {
        me["password"] = p;
    }
    me
}

/// `GET /v1/me`.
pub async fn get_me(State(st): State<AppState>, AuthedAny(a): AuthedAny) -> Json<Value> {
    let core = st.lock();
    Json(me_json(&core, &a.user))
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PatchMe {
    display_name: Option<String>,
    #[serde(default)]
    settings: Map<String, Value>,
    /// 1.1 (`badges`): a badge id or null (present-null clears, so absent and null differ).
    #[serde(default, deserialize_with = "present")]
    featured_badge: Option<Value>,
    /// 1.1 (`badges`): a title id or null.
    #[serde(default, deserialize_with = "present")]
    title: Option<Value>,
}

/// A member that is present, even as `null`.
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Value>, D::Error> {
    <Value as serde::Deserialize>::deserialize(d).map(Some)
}

/// `PATCH /v1/me`.
pub async fn patch_me(
    State(st): State<AppState>,
    a: Authed,
    JsonBody(body): JsonBody<PatchMe>,
) -> ApiResult<Json<Value>> {
    let mut core = st.lock();
    if let Some(name) = &body.display_name {
        check_display_name(&core, name)?;
        let changed: Option<i64> =
            core.db
                .query_row("SELECT name_changed_at FROM users WHERE id = ?1", [&a.user], |r| {
                    r.get(0)
                })?;
        let current = core.display_name(&a.user);
        if &current != name {
            if let Some(t) = changed.filter(|t| core.now < t + core.cfg.limits.name_change_cooldown_s) {
                let retry = t + core.cfg.limits.name_change_cooldown_s - core.now;
                return Err(
                    ApiError::conflict("name_change_too_soon", "the display name was changed recently")
                        .retry_after(retry),
                );
            }
            core.db.execute(
                "UPDATE users SET display_name = ?1, name_changed_at = ?2 WHERE id = ?3",
                params![name, core.now, a.user],
            )?;
        }
    }
    let mut presence_changed = false;
    for (k, v) in &body.settings {
        let Some((key, area)) = SETTINGS.iter().find(|(s, _)| s == k) else {
            if k.starts_with("x-") {
                continue;
            }
            return Err(ApiError::invalid(&format!("/settings/{k}"), "unknown setting"));
        };
        if !area_on(&core, area) {
            return Err(ApiError::invalid(
                &format!("/settings/{k}"),
                "this setting's area is not offered",
            ));
        }
        let on = v
            .as_bool()
            .ok_or_else(|| ApiError::invalid(&format!("/settings/{k}"), "a boolean"))?;
        core.db.execute(
            &format!("UPDATE users SET {key} = ?1 WHERE id = ?2"),
            params![on as i64, a.user],
        )?;
        presence_changed |= *key == "share_presence";
    }
    if presence_changed {
        core.refresh_presence(&a.user);
    }
    if body.featured_badge.is_some() || body.title.is_some() {
        if !core.cfg.features.badges {
            return Err(ApiError::invalid(
                "/featured_badge",
                "badges are not offered by this server",
            ));
        }
        crate::badges::patch_me(&mut core, &a.user, body.featured_badge.as_ref(), body.title.as_ref())?;
    }
    let me = me_json(&core, &a.user);
    core.push(&a.user, "me.updated", me.clone());
    Ok(Json(me))
}

/// `PUT /v1/me/game`: stored as given (bounded), used for eligibility.
pub async fn put_game(
    State(st): State<AppState>,
    a: Authed,
    JsonBody(body): JsonBody<Map<String, Value>>,
) -> ApiResult<StatusCode> {
    let caps = GameCaps::from_json(&Value::Object(body.clone()))
        .ok_or_else(|| ApiError::invalid("/", "modes and stages: arrays of at most 256 ids"))?;
    let core = st.lock();
    core.db.execute(
        "UPDATE users SET game_caps = ?1 WHERE id = ?2",
        params![caps.to_json().to_string(), a.user],
    )?;
    Ok(StatusCode::NO_CONTENT)
}

/// What a player's game can run, as the launcher declared it.
#[derive(Debug, Clone, Default)]
pub struct GameCaps {
    pub modes: Option<Vec<String>>,
    pub stages: Option<Vec<String>>,
    pub can_select_stage: bool,
    pub max_players: Option<usize>,
    // `build` is read by nothing and not kept (security review M1: a launcher that sends the size and the
    // modification time of the locally compiled game sends an identifier of the installation; api.md, `PUT /v1/me/game`).
    /// Contract versions of `lan-auto-match` the launcher implements (1.0 bodies: `[1]`).
    pub contract_versions: Vec<u32>,
    /// 1.1 `engine.rounds`.
    pub rounds: bool,
    /// 1.1 `engine.rule_keys` (`None`: not declared).
    pub rule_keys: Option<Vec<String>>,
    /// 1.1.3 `engine.match_loadout`: the engine applies an imposed loadout to the match's copy of the save.
    pub match_loadout: bool,
    /// 1.1.3 `engine.rounds_faster`: a round in the same session starts sooner than a relaunch.
    pub rounds_faster: bool,
    /// 1.1.3 `engine.stage_list`: `all` (every listed stage loads whatever the save) or `host_save`.
    pub stage_list: Option<String>,
    /// 1.1.6 `launcher.draft`: the versions of the draft screen the launcher shows (empty: none, the server plays the
    /// player's turns).
    pub draft: Vec<u32>,
    /// 1.1.7 `engine.spectator_seats`: what the engine does with spectators (`None`: nothing declared; the 1.1
    /// boolean `engine.spectators` alone never seats anyone).
    pub spectator_seats: Option<SpectatorCaps>,
    /// 1.1.7 `launcher.spectate`: versions of the spectator seat the launcher runs and shows (`spectators.md`).
    pub spectate: Vec<u32>,
    /// 1.1.9 `launcher.relay_rtt_ms`: the round trip the launcher measured to each relay of `meta.relays`, by relay
    /// id, in milliseconds (`None`: the relay did not answer). Used to choose the relay of a match.
    pub relay_rtt: std::collections::BTreeMap<String, Option<u32>>,
}

/// 1.1.7: the engine's spectators, as the launcher declared them from the engine's own list.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SpectatorCaps {
    /// Most spectators of one match (the game has 2 places).
    pub max: usize,
    /// The modes a room can have spectators in (`None`: every mode).
    pub modes: Option<Vec<String>>,
    /// The modes actually run with spectators.
    pub verified_modes: Vec<String>,
    /// A spectator may host the game session.
    pub host: bool,
}

/// The version of the spectator seat this server assigns (`launcher.spectate`).
pub const SPECTATE_VERSION: u32 = 1;

impl GameCaps {
    pub fn from_json(v: &Value) -> Option<GameCaps> {
        let list = |k: &str| -> Option<Option<Vec<String>>> {
            match v.get(k) {
                None | Some(Value::Null) => Some(None),
                Some(Value::Array(a)) if a.len() <= 256 => {
                    let ids: Option<Vec<String>> = a
                        .iter()
                        .map(|x| {
                            // Game identifiers may be strings or numbers (1.1); both are kept as text.
                            x.as_str()
                                .map(str::to_string)
                                .or_else(|| x.as_i64().map(|n| n.to_string()))
                                .filter(|s| (1..=64).contains(&s.len()))
                        })
                        .collect();
                    ids.map(Some)
                }
                _ => None,
            }
        };
        Some(GameCaps {
            modes: list("modes")?,
            stages: list("stages")?,
            can_select_stage: v.get("can_select_stage").and_then(Value::as_bool).unwrap_or(false),
            max_players: v.get("max_players").and_then(Value::as_u64).map(|n| n.min(64) as usize),
            contract_versions: {
                let mut vs: Vec<u32> = v
                    .get("contracts")
                    .and_then(Value::as_array)
                    .into_iter()
                    .flatten()
                    .filter(|c| c.get("id").and_then(Value::as_str) == Some(kalamar_protocol::CONTRACT_ID))
                    .flat_map(|c| c.get("versions").and_then(Value::as_array).cloned().unwrap_or_default())
                    .filter_map(|x| x.as_u64().filter(|n| (1..=16).contains(n)).map(|n| n as u32))
                    .take(16)
                    .collect();
                if vs.is_empty() {
                    vs.push(1);
                }
                vs
            },
            rounds: v.pointer("/engine/rounds").and_then(Value::as_bool).unwrap_or(false),
            match_loadout: v
                .pointer("/engine/match_loadout")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            rounds_faster: v
                .pointer("/engine/rounds_faster")
                .and_then(Value::as_bool)
                .unwrap_or(false),
            rule_keys: v.pointer("/engine/rule_keys").and_then(Value::as_array).map(|a| {
                a.iter()
                    .filter_map(|x| x.as_str().filter(|s| s.len() <= 64).map(str::to_string))
                    .take(64)
                    .collect()
            }),
            stage_list: v
                .pointer("/engine/stage_list")
                .and_then(Value::as_str)
                .filter(|s| s.len() <= 32)
                .map(str::to_string),
            draft: versions(v.pointer("/launcher/draft")),
            spectator_seats: v.pointer("/engine/spectator_seats").filter(|s| s.is_object()).map(|s| {
                let ids = |k: &str| -> Option<Vec<String>> {
                    s.get(k).and_then(Value::as_array).map(|a| {
                        a.iter()
                            .filter_map(|x| x.as_str().filter(|t| (1..=64).contains(&t.len())).map(str::to_string))
                            .take(64)
                            .collect()
                    })
                };
                SpectatorCaps {
                    max: s
                        .get("max")
                        .and_then(Value::as_u64)
                        .map(|n| n.min(kalamar_protocol::contract::MAX_SPECTATORS as u64) as usize)
                        .unwrap_or(0),
                    modes: ids("modes"),
                    verified_modes: ids("verified_modes").unwrap_or_default(),
                    host: s.get("host").and_then(Value::as_bool).unwrap_or(false),
                }
            }),
            spectate: versions(v.pointer("/launcher/spectate")),
            relay_rtt: v
                .pointer("/launcher/relay_rtt_ms")
                .and_then(Value::as_object)
                .into_iter()
                .flatten()
                .filter(|(k, _)| (1..=260).contains(&k.len()))
                .filter(|(_, ms)| ms.is_null() || ms.is_u64())
                .take(16)
                // Bounded: a launcher gives up after 2 s (relay.md 4.1), and 0 ms is not a measurement.
                .map(|(k, ms)| {
                    let bounded = ms
                        .as_u64()
                        .map(|n| n.clamp(1, u64::from(crate::relays::RTT_MAX_MS)) as u32);
                    (k.clone(), bounded)
                })
                .collect(),
        })
    }
    pub fn to_json(&self) -> Value {
        json!({"modes": self.modes, "stages": self.stages, "can_select_stage": self.can_select_stage,
               "max_players": self.max_players,
               "contracts": [{"id": kalamar_protocol::CONTRACT_ID, "versions": self.contract_versions}],
               "engine": {"rounds": self.rounds, "rule_keys": self.rule_keys, "match_loadout": self.match_loadout,
                          "rounds_faster": self.rounds_faster, "stage_list": self.stage_list,
                          "spectator_seats": self.spectator_seats.as_ref().map(|s| json!({"max": s.max, "modes": s.modes,
                              "verified_modes": s.verified_modes, "host": s.host}))},
               "launcher": {"draft": self.draft, "spectate": self.spectate, "relay_rtt_ms": self.relay_rtt}})
    }
    /// 1.1.7: what this game lacks to take part in a match with `spectators` spectators in `mode` (as a player or a
    /// spectator; `spectator_host`: it would host from the stands). Empty: nothing. Items of `incompatible_game`.
    pub fn spectator_missing(
        &self,
        mode: &str,
        spectators: usize,
        verified_only: bool,
        spectator_host: bool,
    ) -> Vec<String> {
        let mut out = Vec::new();
        if !self.spectate.contains(&SPECTATE_VERSION) {
            out.push("launcher:spectate".to_string());
        }
        if !self.contract_versions.contains(&2) {
            out.push("contract:2".to_string());
        }
        match &self.spectator_seats {
            None => out.push("spectators".to_string()),
            Some(s) if s.max == 0 => out.push("spectators".to_string()),
            Some(s) => {
                if s.max < spectators {
                    out.push(format!("spectators:{spectators}"));
                }
                let listed = s.modes.as_ref().is_none_or(|m| m.iter().any(|x| x == mode));
                if !listed || (verified_only && !s.verified_modes.iter().any(|x| x == mode)) {
                    out.push(format!("spectator_mode:{mode}"));
                }
                if spectator_host && !s.host {
                    out.push("spectator_host".to_string());
                }
            }
        }
        out
    }
    /// The launcher shows draft screens of version 1 (`draft.md`).
    pub fn drafts(&self) -> bool {
        self.draft.contains(&crate::draft::VERSION)
    }
    /// Whether this game can run a match of `players` in `mode` with `stage`. Unknown capabilities are assumed.
    pub fn can_run(&self, players: usize, mode: &str, stage: Option<&str>) -> bool {
        self.max_players.is_none_or(|m| players <= m)
            && self.modes.as_ref().is_none_or(|m| m.iter().any(|x| x == mode))
            && stage
                .is_none_or(|s| self.can_select_stage && self.stages.as_ref().is_none_or(|l| l.iter().any(|x| x == s)))
    }
}

/// The versions a launcher lists for one of its capabilities (`launcher.draft`, `launcher.spectate`).
fn versions(v: Option<&Value>) -> Vec<u32> {
    v.and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_u64().filter(|n| (1..=16).contains(n)).map(|n| n as u32))
        .take(16)
        .collect()
}

pub fn game_caps(core: &Core, user: &str) -> GameCaps {
    core.db
        .query_row("SELECT game_caps FROM users WHERE id = ?1", [user], |r| {
            r.get::<_, Option<String>>(0)
        })
        .optional()
        .ok()
        .flatten()
        .flatten()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| GameCaps::from_json(&v))
        .unwrap_or_default()
}

/// `GET /v1/state`.
pub async fn get_state(State(st): State<AppState>, AuthedAny(a): AuthedAny) -> Json<Value> {
    let core = st.lock();
    let f = &core.cfg.features;
    let mut s = json!({"me": me_json(&core, &a.user)});
    if f.parties {
        s["party"] = crate::api::parties::party_of_json(&core, &a.user);
        s["party_invites"] = json!(crate::api::parties::invites_for(&core, &a.user));
    }
    if f.rooms {
        s["room"] = crate::api::rooms::room_of_json(&core, &a.user);
        s["room_invites"] = json!(crate::api::rooms::invites_for(&core, &a.user));
    }
    if f.queues {
        s["ticket"] = crate::api::queues::ticket_of_json(&core, &a.user);
    }
    s["match"] = core
        .live
        .user_match
        .get(&a.user)
        .and_then(|id| core.live.matches.get(id))
        .map(|m| crate::matches::match_json(&core, m, Some(&a.user)))
        .unwrap_or(Value::Null);
    Json(s)
}

/// `GET /v1/users/{user_id}`.
pub async fn get_user(State(st): State<AppState>, a: Authed, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let core = st.lock();
    if !core.user_exists(&id) {
        return Err(ApiError::not_found("no such user"));
    }
    let mut u = core.user_json(&id);
    if core.cfg.features.friends {
        u["friend"] = json!(core.are_friends(&a.user, &id));
    }
    if core.cfg.features.moderation {
        let blocked = core
            .db
            .query_row(
                "SELECT 1 FROM blocks WHERE blocker = ?1 AND blocked = ?2",
                params![a.user, id],
                |_| Ok(()),
            )
            .optional()?
            .is_some();
        u["blocked"] = json!(blocked);
    }
    Ok(Json(u))
}

fn rows(core: &Core, sql: &str, user: &str, cols: &[&str]) -> rusqlite::Result<Vec<Value>> {
    let mut st = core.db.prepare(sql)?;
    let n = cols.len();
    let out = st
        .query_map([user], |r| {
            let mut m = Map::new();
            for (i, c) in cols.iter().enumerate().take(n) {
                let v: rusqlite::types::Value = r.get(i)?;
                let j = match v {
                    rusqlite::types::Value::Null => Value::Null,
                    rusqlite::types::Value::Integer(x) if c.ends_with("_at") || *c == "until" => json!(rfc3339(x)),
                    rusqlite::types::Value::Integer(x) => json!(x),
                    rusqlite::types::Value::Real(x) => json!(x),
                    rusqlite::types::Value::Text(t) if c.ends_with("_json") => {
                        serde_json::from_str(&t).unwrap_or(Value::Null)
                    }
                    rusqlite::types::Value::Text(t) => json!(t),
                    rusqlite::types::Value::Blob(_) => Value::Null,
                };
                m.insert(c.trim_end_matches("_json").to_string(), j);
            }
            Ok(Value::Object(m))
        })?
        .collect::<Result<Vec<_>, _>>()?;
    Ok(out)
}

/// Everything held about `user` (`api.md`, `GET /v1/me/export`).
pub fn export_user(core: &Core, user: &str) -> ApiResult<Value> {
    Ok(json!({
        "format": "kalamar-export-1",
        "exported_at": rfc3339(core.now),
        "server": core.cfg.server.name,
        "account": rows(core, "SELECT id, display_name, created_at, name_changed_at, friend_code, share_presence,
            allow_friend_requests, public_profile, show_on_leaderboards, banned_until, ban_reason, queue_cooldown_until,
            game_caps, host_score FROM users WHERE id = ?1", user,
            &["id", "display_name", "created_at", "name_changed_at", "friend_code", "share_presence",
              "allow_friend_requests", "public_profile", "show_on_leaderboards", "banned_until", "ban_reason",
              "queue_cooldown_until", "game_caps_json", "host_score"])?.into_iter().next(),
        "logins": rows(core, "SELECT provider, created_at FROM identities WHERE user_id = ?1", user, &["provider", "created_at"])?,
        "logins_note": "the server stores only a keyed hash of each login identity, never the provider's user id or your e-mail address",
        "password_login": crate::auth::password::export_member(core, user),
        "sessions": rows(core, "SELECT created_at, last_used_at, access_expires, refresh_expires FROM sessions WHERE user_id = ?1",
            user, &["created_at", "last_used_at", "access_expires_at", "refresh_expires_at"])?,
        "friends": rows(core, "SELECT CASE WHEN user_a = ?1 THEN user_b ELSE user_a END, created_at FROM friendships
            WHERE user_a = ?1 OR user_b = ?1", user, &["user_id", "created_at"])?,
        "friend_requests": rows(core, "SELECT id, from_user, to_user, created_at FROM friend_requests
            WHERE from_user = ?1 OR to_user = ?1", user, &["id", "from", "to", "created_at"])?,
        "blocks": rows(core, "SELECT blocked, created_at FROM blocks WHERE blocker = ?1", user, &["user_id", "created_at"])?,
        "ratings": rows(core, "SELECT ladder, season_id, rating, deviation, volatility, games, wins, losses, last_delta,
            last_match, last_at FROM ratings WHERE user_id = ?1", user,
            &["ladder", "season", "rating", "deviation", "volatility", "games", "wins", "losses", "last_delta", "last_match", "last_at"])?,
        "matches": rows(core, "SELECT p.match_id, m.state, m.origin_kind, m.origin_id, m.rules, m.created_at, m.ended_at,
            p.display_name, p.member_name, p.team, p.role, p.accept, p.launched_at, p.status, p.report, p.report_at,
            p.verdict, p.result, p.stats, p.rating_delta FROM match_players p JOIN matches m ON m.id = p.match_id
            WHERE p.user_id = ?1 ORDER BY m.created_at", user,
            &["match_id", "state", "origin_kind", "origin_id", "rules_json", "created_at", "ended_at", "display_name",
              "member_name", "team", "role", "accept", "launched_at", "status_json", "report_json", "reported_at",
              "verdict", "result", "stats_json", "rating_delta"])?,
        // 1.1.7: the matches the account watched from the stands, with its witness reports.
        "matches_watched": rows(core, "SELECT s.match_id, m.state, m.origin_kind, m.origin_id, m.created_at, m.ended_at,
            s.display_name, s.member_name, s.role, s.referee, s.launched_at, s.status, s.report, s.report_at, s.left_place,
            s.verdict FROM match_spectators s JOIN matches m ON m.id = s.match_id WHERE s.user_id = ?1 ORDER BY m.created_at", user,
            &["match_id", "state", "origin_kind", "origin_id", "created_at", "ended_at", "display_name", "member_name",
              "role", "referee", "launched_at", "status_json", "report_json", "reported_at", "left", "verdict"])?,
        "offences": rows(core, "SELECT kind, match_id, at, cooldown_until FROM offences WHERE user_id = ?1", user,
            &["kind", "match_id", "created_at", "until"])?,
        "reports_filed": rows(core, "SELECT id, target, match_id, category, comment, created_at, status FROM reports
            WHERE reporter = ?1", user, &["id", "target", "match_id", "category", "comment", "created_at", "status"])?,
    }))
}

/// `GET /v1/me/export`.
pub async fn export(State(st): State<AppState>, AuthedAny(a): AuthedAny) -> ApiResult<Response> {
    let core = st.lock();
    let doc = export_user(&core, &a.user)?;
    let mut resp = Json(doc).into_response();
    resp.headers_mut().insert(
        header::CONTENT_DISPOSITION,
        HeaderValue::from_static("attachment; filename=\"kalamar-export.json\""),
    );
    Ok(resp)
}

/// Erases an account (API and admin command line): live state, then the database.
pub fn erase_user(core: &mut Core, user: &str) -> ApiResult<()> {
    if !core.user_exists(user) {
        return Err(ApiError::not_found("no such user"));
    }
    // Live state first.
    crate::api::queues::cancel_ticket_of(core, user, "cancelled");
    crate::api::parties::remove_from_party(core, user, "left");
    crate::api::rooms::remove_from_room(core, user, "kicked");
    if let Some(mid) = core.live.user_match.remove(user)
        && let Some(m) = core.live.matches.get_mut(&mid)
    {
        for s in m.seats.iter_mut().filter(|s| s.user_id.as_deref() == Some(user)) {
            s.user_id = None;
            s.display_name.clear();
        }
        for s in m.spectators.iter_mut().filter(|s| s.user_id.as_deref() == Some(user)) {
            s.user_id = None;
            s.display_name.clear();
        }
        if let Some(d) = m.draft.as_mut() {
            d.forget(user);
        }
    }
    core.live
        .party_invites
        .retain(|(_, u), (from, _)| u != user && from != user);
    core.live
        .room_invites
        .retain(|(_, u), (from, _)| u != user && from != user);
    core.push(user, "session.revoked", json!({"reason": "erased"}));
    core.close_streams(user, None, "session_revoked");
    core.live.last_seen.remove(user);
    core.live.known_ips.remove(user);
    let friends = core.friends_of(user);
    // Keep a running ban on the login identity, as the privacy notice says.
    let tx = core.db.unchecked_transaction()?;
    tx.execute(
        "INSERT OR REPLACE INTO identity_bans (provider, subject_hash, until, reason)
         SELECT i.provider, i.subject_hash, u.banned_until, u.ban_reason FROM identities i JOIN users u ON u.id = i.user_id
         WHERE i.user_id = ?1 AND u.banned_until > ?2",
        params![user, core.now],
    )?;
    hold_identity(core, &tx, user)?;
    // Anonymize the account's lines in other players' matches.
    let match_ids: Vec<String> = {
        let mut s = tx.prepare(
            "SELECT match_id FROM match_players WHERE user_id = ?1 UNION SELECT match_id FROM match_spectators WHERE user_id = ?1",
        )?;
        s.query_map([user], |r| r.get(0))?.flatten().collect()
    };
    for mid in &match_ids {
        let outcome: Option<String> = tx.query_row("SELECT outcome FROM matches WHERE id = ?1", [mid], |r| r.get(0))?;
        if let Some(mut o) = outcome.and_then(|s| serde_json::from_str::<Value>(&s).ok()) {
            anonymize(&mut o, user);
            tx.execute(
                "UPDATE matches SET outcome = ?1 WHERE id = ?2",
                params![o.to_string(), mid],
            )?;
        }
        // 1.1.6: the account's place in the match's draft (team lists, captain, votes).
        let draft: Option<String> = tx.query_row("SELECT draft FROM matches WHERE id = ?1", [mid], |r| r.get(0))?;
        if let Some(mut d) = draft.and_then(|s| serde_json::from_str::<crate::draft::Draft>(&s).ok()) {
            d.forget(user);
            tx.execute(
                "UPDATE matches SET draft = ?1 WHERE id = ?2",
                params![serde_json::to_string(&d).unwrap_or_default(), mid],
            )?;
        }
    }
    tx.execute(
        "UPDATE match_players SET user_id = NULL, display_name = '', member_name = '', report = NULL, status = NULL
         WHERE user_id = ?1",
        [user],
    )?;
    // 1.1.7: the places the account had in the stands.
    tx.execute(
        "UPDATE match_spectators SET user_id = NULL, display_name = '', member_name = '', report = NULL, status = NULL
         WHERE user_id = ?1",
        [user],
    )?;
    // One-time codes for the account's address (sign-up and decoy codes carry no account id).
    tx.execute(
        "DELETE FROM password_codes WHERE subject_hash IN
         (SELECT subject_hash FROM identities WHERE user_id = ?1 AND provider = 'password')",
        [user],
    )?;
    tx.execute("DELETE FROM users WHERE id = ?1", [user])?;
    tx.commit()?;
    crate::db::audit(&core.db, core.now, "erase", Some(user), None)?;
    for f in friends {
        core.push(&f, "friend.removed", json!({"user_id": user}));
    }
    Ok(())
}

/// Security review M6: an erased account's running cooldown, the offenses of the penalty window and the open reports
/// about it stay with its login identities (their keyed hashes, in `identity_holds`) for as long as they run, the
/// reports for `retention.erased_hold_days` at most: erasing an account and signing up again with the same login no
/// longer clears them. Nothing is held for a guest account (its identity can never log in again).
fn hold_identity(core: &Core, tx: &rusqlite::Transaction<'_>, user: &str) -> rusqlite::Result<()> {
    let now = core.now;
    let window = core.cfg.penalties.window_s;
    let cooldown: Option<i64> = tx
        .query_row("SELECT queue_cooldown_until FROM users WHERE id = ?1", [user], |r| {
            r.get(0)
        })
        .optional()?
        .flatten()
        .filter(|t| *t > now);
    let offences: Vec<(String, i64, Option<i64>)> = {
        let mut s =
            tx.prepare("SELECT kind, at, cooldown_until FROM offences WHERE user_id = ?1 AND at > ?2 ORDER BY at")?;
        s.query_map(params![user, now - window], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<rusqlite::Result<_>>()?
    };
    let open_reports: i64 = tx.query_row(
        "SELECT COUNT(*) FROM reports WHERE target = ?1 AND status = 'open'",
        [user],
        |r| r.get(0),
    )?;
    let mut until = cooldown.unwrap_or(0);
    if let Some(last) = offences.iter().map(|o| o.1).max() {
        until = until.max(last + window);
    }
    if open_reports > 0 {
        until = until.max(now + core.cfg.retention.erased_hold_days * 86_400);
    }
    if until <= now {
        return Ok(());
    }
    let hold = new_id("h");
    let offences_json = serde_json::to_string(&offences).unwrap_or_else(|_| "[]".into());
    let held = tx.execute(
        "INSERT OR REPLACE INTO identity_holds (provider, subject_hash, hold_id, until, cooldown_until, offences, created_at)
         SELECT provider, subject_hash, ?2, ?3, ?4, ?5, ?6 FROM identities WHERE user_id = ?1 AND provider != 'guest'",
        params![user, hold, until, cooldown, offences_json, now],
    )?;
    if held > 0 {
        tx.execute(
            "UPDATE reports SET target_hold = ?2 WHERE target = ?1 AND status = 'open'",
            params![user, hold],
        )?;
    }
    Ok(())
}

/// A new account whose login identity carries a hold (an erased account's, [`hold_identity`]) takes it over: the
/// cooldown, the offenses, the open reports. The hold is then gone (a later erasure makes a new one).
pub fn take_over_hold(
    tx: &rusqlite::Transaction<'_>,
    now: i64,
    provider: &str,
    hash: &[u8],
    user: &str,
) -> rusqlite::Result<()> {
    let hold: Option<(String, Option<i64>, String)> = tx
        .query_row(
            "SELECT hold_id, cooldown_until, offences FROM identity_holds WHERE provider = ?1 AND subject_hash = ?2 AND until > ?3",
            params![provider, hash, now],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let Some((hold, cooldown, offences)) = hold else {
        return Ok(());
    };
    if let Some(until) = cooldown.filter(|t| *t > now) {
        tx.execute(
            "UPDATE users SET queue_cooldown_until = ?1 WHERE id = ?2",
            params![until, user],
        )?;
    }
    let offences: Vec<(String, i64, Option<i64>)> = serde_json::from_str(&offences).unwrap_or_default();
    for (kind, at, until) in offences {
        tx.execute(
            "INSERT INTO offences (user_id, kind, match_id, at, cooldown_until) VALUES (?1, ?2, NULL, ?3, ?4)",
            params![user, kind, at, until],
        )?;
    }
    tx.execute(
        "UPDATE reports SET target = ?1, target_hold = NULL WHERE target_hold = ?2",
        params![user, hold],
    )?;
    tx.execute("DELETE FROM identity_holds WHERE hold_id = ?1", [&hold])?;
    Ok(())
}

fn anonymize(v: &mut Value, user: &str) {
    match v {
        Value::Object(m) => {
            if m.get("user_id").and_then(Value::as_str) == Some(user) {
                m.insert("user_id".into(), Value::Null);
                if m.contains_key("name") {
                    m.insert("name".into(), json!(""));
                }
            }
            for x in m.values_mut() {
                anonymize(x, user);
            }
        }
        Value::Array(a) => a.iter_mut().for_each(|x| anonymize(x, user)),
        _ => {}
    }
}

#[derive(Deserialize)]
pub struct EraseQuery {
    confirm: Option<String>,
}

/// `DELETE /v1/me?confirm=erase`.
pub async fn erase(
    State(st): State<AppState>,
    AuthedAny(a): AuthedAny,
    Query(q): Query<EraseQuery>,
) -> ApiResult<StatusCode> {
    if q.confirm.as_deref() != Some("erase") {
        return Err(ApiError::invalid("/confirm", "add ?confirm=erase"));
    }
    let mut core = st.lock();
    erase_user(&mut core, &a.user)?;
    Ok(StatusCode::NO_CONTENT)
}
