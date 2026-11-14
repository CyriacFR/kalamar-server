//! Places in the stands (protocol 1.1.7, `spectators.md`): spectators of a match, never players.
//!
//! - **Rooms** open places in their form (`spectators`, `spectators_who`); a member moves into the stands like a team
//!   change (`api/rooms.rs`).
//! - **Queues** and **activities** open places in their matches (`spectators = { seats, who }`): taken from the list of
//!   watchable matches (`GET /v1/spectate`) before the launch (the accept phase, the draft), or for a whole series
//!   (the next games).
//! - The **operator API** seats a referee in a given match or series.
//!
//! What the game can do decides the rest: places close when the game block is final (a spectator who arrives after the
//! start stays in the game's lobby until the end, then fails: the engine cannot seat it), and every participant's game
//! must play with spectators present (an older engine fails on the rules' letter `S`), so a place is only given when
//! every player's and spectator's launcher declares `engine.spectator_seats` and `launcher.spectate`.

use axum::Json;
use axum::extract::{Path, State};
use axum::http::StatusCode;
use rusqlite::OptionalExtension;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::auth::Authed;
use crate::config::{Config, STANDS_WHO, Stands};
use crate::error::{ApiError, ApiResult, JsonBody};
use crate::matches::reseat;
use crate::state::{AppState, Core, MState, SpectatorSeat, StandsRule};
use crate::util::new_relay_room;

/// One of the operator's referees (`[spectators] referees`).
pub fn is_referee(cfg: &Config, user: &str) -> bool {
    cfg.spectators.referees.iter().any(|r| r == user)
}

/// The label of a `who` value (rooms' enum field, lists).
pub fn who_label(w: &str) -> Value {
    match w {
        "anyone" => json!({"en": "Anyone", "fr": "Tout le monde", "es": "Cualquiera", "ja": "誰でも"}),
        "invited" => json!({"en": "Invited players", "fr": "Les invités", "es": "Los invitados", "ja": "招待された人"}),
        "friends" => json!({"en": "Friends", "fr": "Les amis", "es": "Los amigos", "ja": "フレンド"}),
        "referees" => {
            json!({"en": "Referees only", "fr": "Arbitres seulement", "es": "Solo árbitros", "ja": "審判のみ"})
        }
        other => json!({"en": other}),
    }
}

/// The places of a queue's or an activity's matches as the match rules carry them (`None` when the area is off).
pub fn rule_of(cfg: &Config, s: Option<&Stands>) -> Option<StandsRule> {
    let s = s.filter(|_| cfg.features.spectators)?;
    Some(StandsRule {
        seats: s.seats.min(kalamar_protocol::contract::MAX_SPECTATORS),
        who: s.who.clone(),
        wait_s: s.wait_s.unwrap_or(cfg.spectators.wait_s),
        open: false,
    })
}

/// The `spectators` member of a queue object: its places, or `null`.
pub fn queue_member(cfg: &Config, s: Option<&Stands>) -> Value {
    match rule_of(cfg, s) {
        Some(r) => json!({"seats": r.seats, "who": r.who, "label": who_label(&r.who), "wait_s": r.wait_s}),
        None => Value::Null,
    }
}

/// The players among `users` who refuse to be watched (when the operator lets them).
pub fn refusing(core: &Core, users: &[String]) -> Vec<String> {
    if !core.cfg.spectators.opt_out {
        return Vec::new();
    }
    users
        .iter()
        .filter(|u| {
            core.db
                .query_row("SELECT allow_spectators FROM users WHERE id = ?1", [u.as_str()], |r| {
                    r.get::<_, i64>(0)
                })
                .optional()
                .ok()
                .flatten()
                .is_some_and(|v| v == 0)
        })
        .cloned()
        .collect()
}

/// Configuration problems of the stands (used by `Config::validate`).
pub fn validate(c: &Config, e: &mut Vec<String>) {
    let s = &c.spectators;
    for r in &s.referees {
        if !crate::util::is_id(r) {
            e.push(format!("spectators.referees: `{r}` is not an account id"));
        }
    }
    if s.wait_s > 600 {
        e.push("spectators.wait_s: 0 to 600".into());
    }
    let rs = &c.rooms.spectators;
    if rs.max > kalamar_protocol::contract::MAX_SPECTATORS || rs.default > rs.max {
        e.push("rooms.spectators: max 0 to 2, default at most max".into());
    }
    if rs.max > 0 && (rs.who.is_empty() || rs.who.iter().any(|w| !STANDS_WHO.contains(&w.as_str()))) {
        e.push("rooms.spectators.who: anyone, invited, friends, referees (at least one)".into());
    }
    if rs.wait_s.is_some_and(|w| w > 600) {
        e.push("rooms.spectators.wait_s: 0 to 600".into());
    }
    let mut stands: Vec<(String, &Stands)> = Vec::new();
    for q in &c.queues {
        if let Some(st) = &q.spectators {
            stands.push((format!("queues.{}.spectators", q.id), st));
        }
    }
    for a in &c.activities {
        if let Some(st) = &a.spectators {
            stands.push((format!("activities.{}.spectators", a.id), st));
        }
    }
    for (n, st) in stands {
        if !(1..=kalamar_protocol::contract::MAX_SPECTATORS).contains(&st.seats) {
            e.push(format!("{n}.seats: 1 or 2"));
        }
        if !matches!(st.who.as_str(), "anyone" | "friends" | "referees") {
            e.push(format!("{n}.who: anyone, friends or referees"));
        }
        if st.wait_s.is_some_and(|w| w > 600) {
            e.push(format!("{n}.wait_s: 0 to 600"));
        }
    }
    for p in &c.result_policies {
        use kalamar_protocol::policy::{WitnessFrom, WitnessUse};
        if p.witness == WitnessUse::Unknown || p.witness_from == WitnessFrom::Unknown {
            e.push(format!(
                "result_policies.{}: witness is ignore, tiebreak or required; witness_from any or referees",
                p.id
            ));
        }
    }
}

/// The operator's configuration warnings about the stands.
pub fn warnings(c: &Config) -> Vec<String> {
    let mut w = Vec::new();
    let opened = c.rooms.spectators.max > 0
        || c.queues.iter().any(|q| q.spectators.is_some())
        || c.activities.iter().any(|a| a.spectators.is_some());
    if opened && !c.features.spectators {
        w.push("places in the stands are configured but features.spectators is off: none is offered".into());
    }
    let referees_only = c
        .result_policies
        .iter()
        .any(|p| p.witness_from == kalamar_protocol::policy::WitnessFrom::Referees);
    if referees_only && c.spectators.referees.is_empty() && c.operator_tokens.is_empty() {
        w.push(
            "a result policy counts referees' reports only, but there are no [spectators] referees and no operator token to seat one"
                .into(),
        );
    }
    w
}

/// A member name for a newcomer in the stands: unique among the match's members (theirs unchanged).
fn fresh_name(existing: &[String], display: &str) -> String {
    let mut all: Vec<&str> = existing.iter().map(String::as_str).collect();
    all.push(display);
    kalamar_protocol::contract::derive_member_names(&all)
        .pop()
        .unwrap_or_else(|| "Player".into())
}

/// Whether `user` may take a place reserved for `who` in a match of `players` (referees always may).
fn may_sit(core: &Core, user: &str, who: &str, players: &[String], referee: bool) -> bool {
    referee
        || match who {
            "anyone" => true,
            "friends" => players.iter().any(|p| core.are_friends(p, user)),
            _ => false,
        }
}

/// Every game of the match, the newcomer's included, can play with `n` spectators in `mode`: else the
/// `409 incompatible_game` naming what each one lacks (`missing`).
fn check_games(core: &Core, users: &[String], mode: &str, n: usize) -> ApiResult<()> {
    let mut players = Vec::new();
    for u in users {
        let items = crate::api::account::game_caps(core, u).spectator_missing(
            mode,
            n,
            core.cfg.spectators.verified_modes_only,
            false,
        );
        if !items.is_empty() {
            players.push(json!({"user_id": u, "missing": items}));
        }
    }
    if players.is_empty() {
        return Ok(());
    }
    Err(
        ApiError::conflict("incompatible_game", "a game of this match cannot play with spectators")
            .text(
                "A game of this match cannot play with spectators.",
                "Un jeu de ce match ne sait pas jouer avec des spectateurs.",
            )
            .details(json!({"players": players})),
    )
}

/// The caller is free to watch: not in another live match, not searching.
fn free(core: &Core, user: &str, except: Option<&str>) -> ApiResult<()> {
    let busy = core
        .live
        .user_match
        .get(user)
        .filter(|m| Some(m.as_str()) != except)
        .and_then(|m| core.live.matches.get(m))
        .is_some_and(|m| !m.state.ended());
    if busy {
        return Err(ApiError::conflict("in_match", "you are in another match"));
    }
    if core.live.user_ticket.contains_key(user) {
        return Err(ApiError::conflict("already_in_queue", "leave the queue first"));
    }
    Ok(())
}

fn need(core: &Core) -> ApiResult<()> {
    if core.cfg.features.spectators {
        Ok(())
    } else {
        Err(ApiError::feature_disabled())
    }
}

/// Takes a place in the stands of a queue's or an activity's match before its launch. `operator`: seated by the
/// operator API (a referee, whoever the places are for). A game of a series seats for the whole series.
pub fn take_match(core: &mut Core, mid: &str, user: &str, operator: bool) -> ApiResult<()> {
    need(core)?;
    let referee = operator || is_referee(&core.cfg, user);
    let m = core
        .live
        .matches
        .get(mid)
        .ok_or_else(|| ApiError::not_found("no such match"))?;
    if m.spectator_of(user).is_some_and(|k| !m.spectators[k].left) {
        return Ok(());
    }
    if m.seat_of(user).is_some() {
        return Err(ApiError::conflict("in_match", "you play in this match"));
    }
    let Some(st) = m.rules.spectators.clone().filter(|_| m.origin_kind != "room") else {
        return Err(ApiError::not_found("this match has no places in the stands"));
    };
    if let Some(r) = m.game.series.clone() {
        return take_series(core, &r.id, user, operator);
    }
    if !st.open || !matches!(m.state, MState::PendingAccept | MState::Drafting) {
        return Err(ApiError::conflict(
            "match_started",
            "the match is launched: the game cannot let a spectator in any more",
        )
        .text(
            "This match has already been launched: the game cannot let a spectator in now.",
            "Ce match est déjà lancé : le jeu ne peut plus faire entrer de spectateur.",
        ));
    }
    free(core, user, None)?;
    if !core.user_exists(user) {
        return Err(ApiError::not_found("no such user"));
    }
    let active = m.spectators.iter().filter(|s| !s.left).count();
    if active >= st.seats {
        return Err(ApiError::conflict(
            "spectators_full",
            "every place in the stands is taken",
        ));
    }
    let players: Vec<String> = m.seats.iter().filter_map(|s| s.user_id.clone()).collect();
    if !may_sit(core, user, &st.who, &players, referee) {
        return Err(
            ApiError::forbidden("spectators_not_allowed", "these places are reserved").details(json!({"who": st.who})),
        );
    }
    if players.iter().any(|p| core.blocked_between(p, user)) {
        return Err(ApiError::forbidden("blocked", "you cannot watch this match"));
    }
    if !referee && !refusing(core, &players).is_empty() {
        return Err(ApiError::forbidden(
            "spectators_refused",
            "a player of this match refuses to be watched",
        ));
    }
    let mut everyone = players.clone();
    everyone.extend(
        m.spectators
            .iter()
            .filter(|s| !s.left)
            .filter_map(|s| s.user_id.clone()),
    );
    everyone.push(user.to_string());
    check_games(core, &everyone, &m.rules.mode, active + 1)?;
    let existing: Vec<String> = m
        .seats
        .iter()
        .map(|s| s.member_name.clone())
        .chain(m.spectators.iter().map(|s| s.member_name.clone()))
        .collect();
    let display = core.display_name(user);
    let seat = SpectatorSeat {
        user_id: Some(user.to_string()),
        member_name: fresh_name(&existing, &display),
        display_name: display,
        role: kalamar_protocol::Role::Join,
        referee,
        launched_at: None,
        status: None,
        status_state: None,
        status_at: None,
        report: None,
        report_at: None,
        left: false,
    };
    let now = core.now;
    let m = core
        .live
        .matches
        .get_mut(mid)
        .ok_or_else(|| ApiError::not_found("no such match"))?;
    m.spectators.retain(|s| s.user_id.as_deref() != Some(user));
    m.spectators.push(seat);
    reseat(m);
    m.updated_at = now;
    let snapshot = m.clone();
    crate::matches::save(core, &snapshot)?;
    core.live.user_match.insert(user.to_string(), mid.to_string());
    crate::matches::notify(core, mid, "match.updated");
    core.refresh_presence(user);
    tracing::info!(match_id = %mid, referee, "a place in the stands is taken");
    Ok(())
}

/// Leaves a place in the stands: before the game block is final the place is free again; after it the game may still
/// wait for the spectator (`spectators_wait`), the spectator's launcher closes its game, and nothing is held against
/// anyone.
pub fn leave_match(core: &mut Core, mid: &str, user: &str) -> ApiResult<()> {
    let now = core.now;
    let m = core
        .live
        .matches
        .get_mut(mid)
        .ok_or_else(|| ApiError::not_found("no such match"))?;
    let k = m
        .spectator_of(user)
        .ok_or_else(|| ApiError::not_found("you have no place in this match's stands"))?;
    if matches!(m.state, MState::PendingAccept | MState::Drafting) {
        m.spectators.remove(k);
        reseat(m);
    } else {
        m.spectators[k].left = true;
    }
    m.updated_at = now;
    let snapshot = m.clone();
    crate::matches::save(core, &snapshot)?;
    if core.live.user_match.get(user).map(String::as_str) == Some(mid) {
        core.live.user_match.remove(user);
    }
    crate::matches::notify(core, mid, "match.updated");
    // The leaver sees the match without its place (no `you`), and its launcher lets it go.
    let gone = crate::matches::match_json(core, &snapshot, None);
    core.push(user, "match.updated", gone);
    core.refresh_presence(user);
    Ok(())
}

/// Takes a place in the stands of a series: it holds for the next games, until it is left. The next game is a new game
/// session (`continue_from` needs the same members).
pub fn take_series(core: &mut Core, sid: &str, user: &str, operator: bool) -> ApiResult<()> {
    need(core)?;
    let referee = operator || is_referee(&core.cfg, user);
    let s = core
        .live
        .series
        .get(sid)
        .cloned()
        .ok_or_else(|| ApiError::not_found("no such series"))?;
    let Some(st) = s.stands.clone() else {
        return Err(ApiError::not_found("this series has no places in the stands"));
    };
    if s.spectators.iter().any(|w| w.user == user) {
        return Ok(());
    }
    if s.users().iter().any(|u| u == user) {
        return Err(ApiError::conflict("in_match", "you play in this series"));
    }
    let current = s.games.last().map(|g| g.match_id.clone());
    free(core, user, current.as_deref())?;
    if !core.user_exists(user) {
        return Err(ApiError::not_found("no such user"));
    }
    if s.spectators.len() >= st.seats {
        return Err(ApiError::conflict(
            "spectators_full",
            "every place in the stands is taken",
        ));
    }
    let players = s.users();
    if !may_sit(core, user, &st.who, &players, referee) {
        return Err(
            ApiError::forbidden("spectators_not_allowed", "these places are reserved").details(json!({"who": st.who})),
        );
    }
    if players.iter().any(|p| core.blocked_between(p, user)) {
        return Err(ApiError::forbidden("blocked", "you cannot watch this series"));
    }
    if !referee && !refusing(core, &players).is_empty() {
        return Err(ApiError::forbidden(
            "spectators_refused",
            "a player of this series refuses to be watched",
        ));
    }
    let mut everyone = players.clone();
    everyone.extend(s.spectators.iter().map(|w| w.user.clone()));
    everyone.push(user.to_string());
    check_games(core, &everyone, &s.mode, s.spectators.len() + 1)?;
    let Some(sr) = core.live.series.get_mut(sid) else {
        return Err(ApiError::not_found("no such series"));
    };
    sr.spectators.push(crate::series::Watcher {
        user: user.to_string(),
        referee,
    });
    sr.session_changed = true;
    sr.room = new_relay_room();
    let snapshot = sr.clone();
    crate::series::save(core, &snapshot)?;
    crate::series::notify(core, &snapshot);
    // The current game too, while its places are still open (its draft).
    if let Some(mid) = current
        && core.live.matches.get(&mid).is_some_and(|m| {
            m.rules.spectators.as_ref().is_some_and(|x| x.open)
                && matches!(m.state, MState::PendingAccept | MState::Drafting)
        })
    {
        seat_in_game(core, &mid, user, referee)?;
    }
    tracing::info!(series = %sid, referee, "a place in a series' stands is taken");
    Ok(())
}

/// Seats a series' spectator in its game that is not launched yet (its draft).
fn seat_in_game(core: &mut Core, mid: &str, user: &str, referee: bool) -> ApiResult<()> {
    let display = core.display_name(user);
    let now = core.now;
    let Some(m) = core.live.matches.get_mut(mid) else {
        return Ok(());
    };
    if m.spectator_of(user).is_some() {
        return Ok(());
    }
    let existing: Vec<String> = m
        .seats
        .iter()
        .map(|s| s.member_name.clone())
        .chain(m.spectators.iter().map(|s| s.member_name.clone()))
        .collect();
    m.spectators.push(SpectatorSeat {
        user_id: Some(user.to_string()),
        member_name: fresh_name(&existing, &display),
        display_name: display,
        role: kalamar_protocol::Role::Join,
        referee,
        launched_at: None,
        status: None,
        status_state: None,
        status_at: None,
        report: None,
        report_at: None,
        left: false,
    });
    reseat(m);
    m.updated_at = now;
    let snapshot = m.clone();
    crate::matches::save(core, &snapshot)?;
    core.live.user_match.insert(user.to_string(), mid.to_string());
    crate::matches::notify(core, mid, "match.updated");
    core.refresh_presence(user);
    Ok(())
}

/// Leaves a series' stands (and the current game's, when the spectator has a place there).
pub fn leave_series(core: &mut Core, sid: &str, user: &str) -> ApiResult<()> {
    let Some(s) = core.live.series.get_mut(sid) else {
        return Err(ApiError::not_found("no such series"));
    };
    if !s.spectators.iter().any(|w| w.user == user) {
        return Err(ApiError::not_found("you have no place in this series' stands"));
    }
    s.spectators.retain(|w| w.user != user);
    s.session_changed = true;
    s.room = new_relay_room();
    let snapshot = s.clone();
    crate::series::save(core, &snapshot)?;
    crate::series::notify(core, &snapshot);
    core.push(user, "series.updated", crate::series::series_json(core, &snapshot));
    if let Some(mid) = snapshot.games.last().map(|g| g.match_id.clone())
        && core
            .live
            .matches
            .get(&mid)
            .is_some_and(|m| m.spectator_of(user).is_some())
    {
        leave_match(core, &mid, user)?;
    }
    Ok(())
}

// ---------------------------------------------------------------- the list of watchable matches

/// Whether `user` sees a match of `players` whose places are for `who` in the list.
fn visible(core: &Core, user: &str, who: &str, players: &[String]) -> bool {
    let referee = is_referee(&core.cfg, user);
    if !referee && !refusing(core, players).is_empty() {
        return false;
    }
    if players.iter().any(|p| core.blocked_between(p, user)) {
        return false;
    }
    may_sit(core, user, who, players, referee)
}

fn team_users(core: &Core, seats: &[(String, kalamar_protocol::Team)]) -> Value {
    let pick = |t: kalamar_protocol::Team| -> Vec<Value> {
        seats
            .iter()
            .filter(|(_, x)| *x == t)
            .map(|(u, _)| core.user_json(u))
            .collect()
    };
    json!({"alpha": pick(kalamar_protocol::Team::Alpha), "bravo": pick(kalamar_protocol::Team::Bravo)})
}

/// The caller's own game: what it lacks to watch `mode` (the others' games are checked when the place is taken).
fn own_missing(core: &Core, user: &str, mode: &str, n: usize) -> Vec<String> {
    crate::api::account::game_caps(core, user).spectator_missing(
        mode,
        n,
        core.cfg.spectators.verified_modes_only,
        false,
    )
}

fn items_for(core: &Core, user: &str) -> Vec<Value> {
    let mut out: Vec<(i64, Value)> = Vec::new();
    for m in core.live.matches.values() {
        let Some(st) = m.rules.spectators.as_ref() else {
            continue;
        };
        if m.origin_kind == "room"
            || m.game.series.is_some()
            || !st.open
            || !matches!(m.state, MState::PendingAccept | MState::Drafting)
            || m.involves(user)
        {
            continue;
        }
        let players: Vec<String> = m.seats.iter().filter_map(|s| s.user_id.clone()).collect();
        if !visible(core, user, &st.who, &players) {
            continue;
        }
        let taken = m.spectators.iter().filter(|s| !s.left).count();
        let missing = own_missing(core, user, &m.rules.mode, taken + 1);
        let why = if taken >= st.seats {
            Some("spectators_full")
        } else if !missing.is_empty() {
            Some("incompatible_game")
        } else {
            None
        };
        let mut origin = json!({"kind": m.origin_kind});
        origin[if m.origin_kind == "activity" {
            "activity_id"
        } else {
            "queue_id"
        }] = json!(m.origin_id);
        let seats: Vec<(String, kalamar_protocol::Team)> = m
            .seats
            .iter()
            .filter_map(|s| s.user_id.clone().map(|u| (u, s.team)))
            .collect();
        out.push((
            m.created_at,
            json!({
                "kind": "match", "match_id": m.id, "series_id": null, "origin": origin, "state": m.state.as_str(),
                "rules": {"mode": m.rules.mode, "stage": m.rules.stage, "teams": m.rules.teams},
                "teams": team_users(core, &seats),
                "places": {"seats": st.seats, "taken": taken, "who": st.who, "label": who_label(&st.who)},
                "series": null, "draft": m.draft.is_some(),
                "can_take": why.is_none(), "why": why, "missing": missing,
            }),
        ));
    }
    for s in core.live.series.values() {
        let Some(st) = s.stands.as_ref() else { continue };
        let players = s.users();
        if s.state != "running"
            || players.iter().any(|u| u == user)
            || s.spectators.iter().any(|w| w.user == user)
            || !visible(core, user, &st.who, &players)
        {
            continue;
        }
        let taken = s.spectators.len();
        let missing = own_missing(core, user, &s.mode, taken + 1);
        let why = if taken >= st.seats {
            Some("spectators_full")
        } else if !missing.is_empty() {
            Some("incompatible_game")
        } else {
            None
        };
        let mut origin = json!({"kind": s.origin_kind});
        origin[format!("{}_id", s.origin_kind)] = json!(s.origin_id);
        let seats: Vec<(String, kalamar_protocol::Team)> = s.sides[0]
            .iter()
            .map(|u| (u.clone(), kalamar_protocol::Team::Alpha))
            .chain(s.sides[1].iter().map(|u| (u.clone(), kalamar_protocol::Team::Bravo)))
            .collect();
        out.push((
            s.created_at,
            json!({
                "kind": "series", "match_id": s.games.last().map(|g| g.match_id.clone()), "series_id": s.id,
                "origin": origin, "state": "running",
                "rules": {"mode": s.mode, "stage": null, "teams": [s.sides[0].len(), s.sides[1].len()]},
                "teams": team_users(core, &seats),
                "places": {"seats": st.seats, "taken": taken, "who": st.who, "label": who_label(&st.who)},
                "series": {"index": s.games.len() + 1, "format": {"kind": "best_of", "games": s.best_of},
                           "wins": s.wins, "labels": s.labels},
                "draft": s.draft.is_some(),
                "can_take": why.is_none(), "why": why, "missing": missing,
            }),
        ));
    }
    out.sort_by_key(|(t, _)| *t);
    out.into_iter().map(|(_, v)| v).take(100).collect()
}

fn limited(st: &AppState, user: &str) -> ApiResult<()> {
    match st.limits.spectate.take(&user.to_string(), std::time::Instant::now()) {
        Ok(()) => Ok(()),
        Err(retry) => Err(ApiError::rate_limited(retry)),
    }
}

/// `GET /v1/spectate`: the matches and series the caller may watch, before their launch.
pub async fn list(State(st): State<AppState>, a: Authed) -> ApiResult<Json<Value>> {
    need(&st.lock())?;
    limited(&st, &a.user)?;
    let core = st.lock();
    Ok(Json(json!({"items": items_for(&core, &a.user)})))
}

/// `POST /v1/matches/{match_id}/spectate`: takes a place in the match's stands.
pub async fn take(State(st): State<AppState>, a: Authed, Path(mid): Path<String>) -> ApiResult<Json<Value>> {
    need(&st.lock())?;
    limited(&st, &a.user)?;
    let mut core = st.lock();
    take_match(&mut core, &mid, &a.user, false)?;
    let m = crate::matches::load(&core, &mid)?.ok_or_else(|| ApiError::not_found("no such match"))?;
    Ok(Json(crate::matches::match_json(&core, &m, Some(&a.user))))
}

/// `DELETE /v1/matches/{match_id}/spectate`: leaves the place.
pub async fn leave(State(st): State<AppState>, a: Authed, Path(mid): Path<String>) -> ApiResult<StatusCode> {
    need(&st.lock())?;
    limited(&st, &a.user)?;
    let mut core = st.lock();
    if !core.live.matches.contains_key(&mid) {
        return Err(ApiError::not_found("no such match"));
    }
    if let Some(sid) = core.live.matches[&mid].game.series.clone().map(|r| r.id)
        && core
            .live
            .series
            .get(&sid)
            .is_some_and(|s| s.spectators.iter().any(|w| w.user == a.user))
    {
        leave_series(&mut core, &sid, &a.user)?;
    } else {
        leave_match(&mut core, &mid, &a.user)?;
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/series/{series_id}/spectate`.
pub async fn take_series_ep(State(st): State<AppState>, a: Authed, Path(sid): Path<String>) -> ApiResult<Json<Value>> {
    need(&st.lock())?;
    limited(&st, &a.user)?;
    let mut core = st.lock();
    take_series(&mut core, &sid, &a.user, false)?;
    let s = crate::series::load(&core, &sid)?.ok_or_else(|| ApiError::not_found("no such series"))?;
    Ok(Json(crate::series::series_json(&core, &s)))
}

/// `DELETE /v1/series/{series_id}/spectate`.
pub async fn leave_series_ep(State(st): State<AppState>, a: Authed, Path(sid): Path<String>) -> ApiResult<StatusCode> {
    need(&st.lock())?;
    limited(&st, &a.user)?;
    let mut core = st.lock();
    leave_series(&mut core, &sid, &a.user)?;
    Ok(StatusCode::NO_CONTENT)
}

#[derive(Deserialize)]
pub struct SeatBody {
    user_id: String,
}

/// `POST /v1/operator/matches/{match_id}/spectators` (scope `results`): seats a referee in a match not launched yet.
pub async fn operator_match(
    State(st): State<AppState>,
    op: crate::operator::Operator,
    Path(mid): Path<String>,
    JsonBody(b): JsonBody<SeatBody>,
) -> ApiResult<StatusCode> {
    op.need("results")?;
    let mut core = st.lock();
    take_match(&mut core, &mid, &b.user_id, true)?;
    let now = core.now;
    crate::db::audit(
        &core.db,
        now,
        "referee-seat",
        Some(&mid),
        Some(&format!("{} by {}", b.user_id, op.name)),
    )?;
    Ok(StatusCode::NO_CONTENT)
}

/// `POST /v1/operator/series/{series_id}/spectators` (scope `results`): seats a referee for a series' next games.
pub async fn operator_series(
    State(st): State<AppState>,
    op: crate::operator::Operator,
    Path(sid): Path<String>,
    JsonBody(b): JsonBody<SeatBody>,
) -> ApiResult<StatusCode> {
    op.need("results")?;
    let mut core = st.lock();
    take_series(&mut core, &sid, &b.user_id, true)?;
    let now = core.now;
    crate::db::audit(
        &core.db,
        now,
        "referee-seat",
        Some(&sid),
        Some(&format!("{} by {}", b.user_id, op.name)),
    )?;
    Ok(StatusCode::NO_CONTENT)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn newcomers_get_a_name_of_their_own() {
        let existing = vec!["Camille".to_string(), "Lou".to_string()];
        assert_eq!(fresh_name(&existing, "Lou"), "Lou2");
        assert_eq!(fresh_name(&existing, "Ana, the: great"), "Ana the gr");
        assert_eq!(fresh_name(&[], "Zed"), "Zed");
    }
}
