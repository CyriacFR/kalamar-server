//! Generic activities (protocol 1.1, `activities.md`): the reference server's two kinds, both built from the
//! configuration and shown to launchers as views of data components:
//!
//! - `tournament`: players register; when the activity is full a single-elimination bracket starts, every bracket
//!   match a best-of-N series; the next round fills as series end; a finished tournament reopens later;
//! - `duel_series`: a player asks for an opponent; the first two waiting play a best-of-N series.
//!
//! Live state is in memory (a restart reopens every activity; series and matches are in the database).

use axum::Json;
use axum::extract::{Path, State};
use serde::Deserialize;
use serde_json::{Map, Value, json};

use crate::auth::Authed;
use crate::config::{ActivityEntry, Config};
use crate::error::{ApiError, ApiResult, JsonBody};
use crate::series::Series;
use crate::state::{AppState, Core};

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct BracketMatch {
    pub a: Option<String>,
    pub b: Option<String>,
    pub series: Option<String>,
    pub winner: Option<String>,
    /// Settled (a winner, a bye, or empty).
    pub done: bool,
    /// The series was void and the winner drawn by lot (security review M8).
    #[serde(default)]
    pub drawn: bool,
}

#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct ActLive {
    pub revision: u64,
    pub state: String,
    pub edition: u32,
    pub entrants: Vec<String>,
    pub waiting: Vec<String>,
    pub rounds: Vec<Vec<BracketMatch>>,
    pub champion: Option<String>,
    pub finished_at: Option<i64>,
    /// Series of this activity, newest last (at most 50 kept).
    pub series: Vec<String>,
    /// 1.1.13: a tournament series could not start because every relay was full (`relay_capacity`): the ticker
    /// tries again every second.
    #[serde(default)]
    pub relay_wait: bool,
}

fn entry<'a>(cfg: &'a Config, id: &str) -> Option<&'a ActivityEntry> {
    cfg.activities.iter().find(|a| a.id == id)
}

fn live<'a>(core: &'a mut Core, id: &str) -> &'a mut ActLive {
    core.live.activities.entry(id.to_string()).or_insert_with(|| ActLive {
        state: "open".into(),
        edition: 1,
        ..ActLive::default()
    })
}

fn involved(a: &ActLive) -> Vec<String> {
    let mut v: Vec<String> = a.entrants.iter().chain(a.waiting.iter()).cloned().collect();
    v.sort();
    v.dedup();
    v
}

/// Bumps the revision and tells everyone involved (plus `extra`).
fn changed(core: &mut Core, id: &str, extra: &[String]) {
    let a = live(core, id);
    a.revision += 1;
    let data = serde_json::to_string(&*a).unwrap_or_default();
    let now = core.now;
    if let Err(e) = core.db.execute(
        "INSERT INTO activity_state (id, data, updated_at) VALUES (?1, ?2, ?3)
         ON CONFLICT (id) DO UPDATE SET data = ?2, updated_at = ?3",
        rusqlite::params![id, data, now],
    ) {
        tracing::error!(activity = %id, error = %e, "activity state not saved");
    }
    let a = live(core, id);
    let rev = a.revision;
    let mut users = involved(a);
    users.extend(extra.iter().cloned());
    users.sort();
    users.dedup();
    core.push_many(&users, "activity.updated", &json!({"activity_id": id, "revision": rev}));
    core.emit("activity.updated", json!({"activity_id": id, "revision": rev}));
}

fn need(core: &Core) -> ApiResult<()> {
    if core.cfg.features.activities {
        Ok(())
    } else {
        Err(ApiError::feature_disabled())
    }
}

// ---------------------------------------------------------------- tournament

/// Settles byes and empty matches, and starts the series whose two players are known.
fn advance(core: &mut Core, e: &ActivityEntry) -> ApiResult<()> {
    let id = e.id.clone();
    loop {
        let mut todo: Option<(usize, usize, String, String)> = None;
        {
            let a = live(core, &id);
            for r in 0..a.rounds.len() {
                for k in 0..a.rounds[r].len() {
                    if r > 0 {
                        let (f1, f2) = (&a.rounds[r - 1][2 * k], &a.rounds[r - 1][2 * k + 1]);
                        let (w1, w2, d) = (f1.winner.clone(), f2.winner.clone(), f1.done && f2.done);
                        let m = &mut a.rounds[r][k];
                        m.a = w1;
                        m.b = w2;
                        if !d {
                            continue;
                        }
                    }
                    let m = &mut a.rounds[r][k];
                    if m.done {
                        continue;
                    }
                    match (&m.a, &m.b) {
                        (Some(x), Some(y)) if m.series.is_none() => {
                            todo = Some((r, k, x.clone(), y.clone()));
                        }
                        (Some(x), None) | (None, Some(x)) => {
                            m.winner = Some(x.clone());
                            m.done = true;
                        }
                        (None, None) => m.done = true,
                        _ => {}
                    }
                    if todo.is_some() {
                        break;
                    }
                }
                if todo.is_some() {
                    break;
                }
            }
        }
        let Some((r, k, x, y)) = todo else { break };
        let labels = [core.display_name(&x), core.display_name(&y)];
        let started = crate::series::start(
            core,
            "activity",
            &id,
            [vec![x], vec![y]],
            labels,
            e.best_of,
            &e.mode,
            &e.result_policy,
            e.keep_open,
            e.loadout.clone(),
            e.draft.clone(),
            crate::spectate::rule_of(&core.cfg, e.spectators.as_ref()),
        );
        let sid = match started {
            Ok(sid) => sid,
            // 1.1.13: every relay is full: the series starts once one has room (the ticker tries again).
            Err(err) if err.code == "relay_capacity" => {
                live(core, &id).relay_wait = true;
                return Ok(());
            }
            Err(err) => return Err(err),
        };
        let a = live(core, &id);
        a.rounds[r][k].series = Some(sid.clone());
        a.series.push(sid);
    }
    let now = core.now;
    let mut champion = None;
    let a = live(core, &id);
    if let Some(last) = a.rounds.last().and_then(|r| r.first())
        && last.done
        && a.state == "running"
    {
        a.state = "finished".into();
        a.champion = last.winner.clone();
        a.finished_at = Some(now);
        champion = a.champion.clone();
    }
    if let Some(c) = champion.filter(|_| core.cfg.features.badges) {
        crate::badges::on_tournament_win(core, &id, &c);
    }
    Ok(())
}

fn start_tournament(core: &mut Core, e: &ActivityEntry) -> ApiResult<()> {
    let a = live(core, &e.id);
    let n = a.entrants.len().max(2);
    let mut size = 1;
    while size < n {
        size *= 2;
    }
    let mut rounds = Vec::new();
    let mut width = size / 2;
    while width >= 1 {
        rounds.push(vec![BracketMatch::default(); width]);
        width /= 2;
    }
    for (i, u) in a.entrants.iter().enumerate() {
        let m = &mut rounds[0][i / 2];
        if i % 2 == 0 {
            m.a = Some(u.clone())
        } else {
            m.b = Some(u.clone())
        }
    }
    a.rounds = rounds;
    a.state = "running".into();
    advance(core, e)
}

/// A game of an activity's series was decided and the series goes on: refresh the view of everyone involved.
pub fn on_series_progress(core: &mut Core, s: &Series) {
    if s.origin_kind == "activity" && core.cfg.activities.iter().any(|a| a.id == s.origin_id) {
        changed(core, &s.origin_id, &s.users());
    }
}

/// A series of an activity ended (called by the series engine).
pub fn on_series_end(core: &mut Core, s: &Series) {
    if s.origin_kind != "activity" {
        return;
    }
    let cfg = core.cfg.clone();
    let Some(e) = entry(&cfg, &s.origin_id) else { return };
    let winner = s.winner.and_then(|i| s.sides[i].first().cloned());
    {
        let a = live(core, &e.id);
        for round in a.rounds.iter_mut() {
            for m in round.iter_mut() {
                if m.series.as_deref() == Some(&s.id) {
                    m.done = true;
                    // A void series is drawn by lot. Up to 1.1.11 its first-listed player went through, and that
                    // player could void every game (an error report at once) to advance (security review M8): nothing
                    // in the reports tells who caused a void, and each side controls its own reports, so no side may
                    // profit from one.
                    m.winner = winner.clone().or_else(|| {
                        let sides: Vec<String> = [m.a.clone(), m.b.clone()].into_iter().flatten().collect();
                        m.drawn = !sides.is_empty();
                        (!sides.is_empty()).then(|| sides[crate::util::random_index(sides.len())].clone())
                    });
                    if m.drawn {
                        tracing::info!(activity = %e.id, series = %s.id, "void series: its winner was drawn by lot");
                    }
                }
            }
        }
    }
    if e.kind == "tournament"
        && let Err(err) = advance(core, e)
    {
        tracing::error!(activity = %e.id, error = %err.message, "tournament advance failed");
    }
    changed(core, &e.id, &s.users());
}

/// Reopens finished tournaments after their pause (ticker).
pub fn tick(core: &mut Core) {
    if !core.cfg.features.activities {
        return;
    }
    let cfg = core.cfg.clone();
    for e in &cfg.activities {
        if e.kind == "tournament" && std::mem::take(&mut live(core, &e.id).relay_wait) {
            if let Err(err) = advance(core, e) {
                tracing::error!(activity = %e.id, error = %err.message, "tournament advance failed");
            }
            // the players hear of it once the series started, not every second while every relay is full
            if !live(core, &e.id).relay_wait {
                changed(core, &e.id, &[]);
            }
        }
        let now = core.now;
        let a = live(core, &e.id);
        if a.state == "finished" && a.finished_at.is_some_and(|t| now - t >= e.reopen_after_s) {
            let edition = a.edition + 1;
            let rev = a.revision;
            *a = ActLive {
                state: "open".into(),
                edition,
                revision: rev,
                ..ActLive::default()
            };
            changed(core, &e.id, &[]);
        }
    }
}

// ---------------------------------------------------------------- views

fn text(s: &crate::config::Text) -> Value {
    json!(s)
}

fn en(s: &str) -> Value {
    json!({"en": s})
}

fn action(id: &str, label: &str, style: &str, enabled: bool, why: Option<&str>) -> Value {
    let mut a = json!({"id": id, "label": en(label), "style": style, "enabled": enabled});
    if let Some(w) = why {
        a["disabled_reason"] = en(w);
    }
    a
}

fn summary(core: &mut Core, e: &ActivityEntry, user: &str) -> Value {
    let a = live(core, &e.id).clone();
    let (count, capacity, joined, join) = if e.kind == "tournament" {
        (
            a.entrants.len(),
            Some(e.size),
            a.entrants.iter().any(|u| u == user),
            (a.state == "open").then(|| json!({"action": "register", "label": en("Register")})),
        )
    } else {
        (
            a.waiting.len(),
            None,
            a.waiting.iter().any(|u| u == user),
            Some(json!({"action": "find", "label": en("Find an opponent")})),
        )
    };
    json!({
        "id": e.id,
        "kind": text(&e.kind_label()),
        "title": text(&e.title),
        "subtitle": text(&e.subtitle),
        "icon": e.icon,
        "color": e.color,
        "order": e.order,
        "state": a.state,
        "participants": {"count": count, "capacity": capacity},
        "join": join,
        "you": {"joined": joined},
        "revision": a.revision,
        // 1.1.7: places in the stands of the activity's series, or `null`.
        "spectators": crate::spectate::queue_member(&core.cfg, e.spectators.as_ref()),
    })
}

fn view(core: &mut Core, e: &ActivityEntry, user: &str) -> Value {
    let a = live(core, &e.id).clone();
    let mut sections = vec![json!({"type": "text", "style": "body", "text": text(&e.description)})];
    let mut actions = Vec::new();
    let name = |core: &Core, u: &Option<String>| u.as_ref().map(|u| core.display_name(u)).unwrap_or_else(|| "?".into());
    if e.kind == "tournament" {
        sections.push(json!({"type": "fields", "items": [
            {"label": en("Format"), "value": en(&format!("Single elimination, best of {}", e.best_of))},
            {"label": en("Players"), "value": format!("{} / {}", a.entrants.len(), e.size)},
            {"label": en("Edition"), "value": a.edition},
        ]}));
        if let Some(c) = &a.champion {
            sections.push(json!({"type": "notice", "level": "success", "text": en(&format!("Champion: {}", core.display_name(c)))}));
        }
        let joined = a.entrants.iter().any(|u| u == user);
        if a.state == "open" {
            sections.push(json!({"type": "list", "items": a.entrants.iter().map(|u| json!({"title": en(&core.display_name(u)), "user": core.user_json(u), "icon": "user"})).collect::<Vec<_>>(),
                                 "empty": en("Nobody registered yet.")}));
            actions.push(action(
                "register",
                "Register",
                "primary",
                !joined && a.entrants.len() < e.size,
                joined.then_some("You are registered."),
            ));
            actions.push(action(
                "unregister",
                "Leave",
                "secondary",
                joined,
                (!joined).then_some("You are not registered."),
            ));
        }
        if !a.rounds.is_empty() {
            let total = a.rounds.len();
            let rounds: Vec<Value> = a
                .rounds
                .iter()
                .enumerate()
                .map(|(r, ms)| {
                    let title = match total - r {
                        1 => "Final".to_string(),
                        2 => "Semi-finals".to_string(),
                        3 => "Quarter-finals".to_string(),
                        n => format!("Round of {}", 1 << n),
                    };
                    let matches: Vec<Value> = ms
                        .iter()
                        .enumerate()
                        .map(|(k, m)| {
                            let s = m.series.as_ref().and_then(|id| crate::series::load(core, id).ok().flatten());
                            let score = |i: usize| s.as_ref().map(|s| s.wins[i]);
                            let slot = |u: &Option<String>, i: usize| {
                                let mut v = json!({"label": en(&name(core, u)), "winner": m.winner.is_some() && m.winner == *u, "score": score(i)});
                                if let Some(u) = u {
                                    v["user"] = core.user_json(u);
                                }
                                v
                            };
                            json!({"id": format!("r{r}m{k}"), "state": if m.done { "completed" } else if m.series.is_some() { "running" } else { "waiting" },
                                   "series_id": m.series, "slots": [slot(&m.a, 0), slot(&m.b, 1)]})
                        })
                        .collect();
                    json!({"title": en(&title), "matches": matches})
                })
                .collect();
            sections.push(json!({"type": "bracket", "rounds": rounds}));
        }
    } else {
        let waiting = a.waiting.iter().any(|u| u == user);
        sections.push(json!({"type": "fields", "items": [
            {"label": en("Format"), "value": en(&format!("Best of {}", e.best_of))},
            {"label": en("Waiting"), "value": a.waiting.len()},
        ]}));
        actions.push(action(
            "find",
            "Find an opponent",
            "primary",
            !waiting,
            waiting.then_some("You are already waiting."),
        ));
        actions.push(action(
            "leave",
            "Stop waiting",
            "secondary",
            waiting,
            (!waiting).then_some("You are not waiting."),
        ));
    }
    // The caller's own and the recent series.
    let mut items = Vec::new();
    for sid in a.series.iter().rev().take(10) {
        if let Ok(Some(s)) = crate::series::load(core, sid) {
            if s.users().iter().any(|u| u == user) && s.state == "running" {
                sections.push(json!({"type": "series", "series_id": s.id, "title": en("Your series")}));
            }
            items.push(json!({"title": en(&format!("{} vs {}", s.labels[0], s.labels[1])),
                              "trailing": format!("{} - {}", s.wins[0], s.wins[1]),
                              "subtitle": en(&s.state), "icon": "swords"}));
        }
    }
    if !items.is_empty() {
        sections.push(json!({"type": "group", "title": en("Series"), "children": [{"type": "list", "items": items}]}));
    }
    json!({"id": e.id, "revision": a.revision, "title": text(&e.title), "state": a.state, "sections": sections, "actions": actions})
}

// ---------------------------------------------------------------- endpoints

/// `GET /v1/activities`.
pub async fn list(State(st): State<AppState>, a: Authed) -> ApiResult<Json<Value>> {
    let mut core = st.lock();
    need(&core)?;
    let cfg = core.cfg.clone();
    let items: Vec<Value> = cfg.activities.iter().map(|e| summary(&mut core, e, &a.user)).collect();
    Ok(Json(json!({"items": items})))
}

/// `GET /v1/activities/{activity_id}`.
pub async fn get(State(st): State<AppState>, a: Authed, Path(id): Path<String>) -> ApiResult<Json<Value>> {
    let mut core = st.lock();
    need(&core)?;
    let cfg = core.cfg.clone();
    let e = entry(&cfg, &id).ok_or_else(|| ApiError::not_found("no such activity"))?;
    Ok(Json(view(&mut core, e, &a.user)))
}

#[derive(Deserialize, Default)]
pub struct ActionBody {
    #[serde(default)]
    values: Map<String, Value>,
    #[serde(default)]
    revision: Option<u64>,
}

fn unavailable(m: &str) -> ApiError {
    ApiError::conflict("action_unavailable", m)
}

/// `POST /v1/activities/{activity_id}/actions/{action_id}`.
pub async fn act(
    State(st): State<AppState>,
    a: Authed,
    Path((id, action)): Path<(String, String)>,
    JsonBody(body): JsonBody<ActionBody>,
) -> ApiResult<Json<Value>> {
    let mut core = st.lock();
    need(&core)?;
    let cfg = core.cfg.clone();
    let e = entry(&cfg, &id).ok_or_else(|| ApiError::not_found("no such activity"))?;
    let _ = &body.values;
    if body.revision.is_some_and(|r| r != live(&mut core, &id).revision) {
        return Err(ApiError::conflict("stale_view", "the view changed: fetch it again"));
    }
    let busy = core
        .live
        .user_match
        .get(&a.user)
        .and_then(|m| core.live.matches.get(m))
        .is_some_and(|m| !m.state.ended());
    let mut series_id = None;
    // An activity with an imposed loadout only takes players whose engine applies it (1.1.3).
    if e.loadout.is_some()
        && matches!(action.as_str(), "register" | "find")
        && !crate::api::account::game_caps(&core, &a.user).match_loadout
    {
        return Err(ApiError::forbidden(
            "not_eligible",
            "your launcher cannot impose this activity's weapon and gear",
        )
        .details(
            json!({"requirements": [{"id": "loadout", "label": {"en": "A launcher that imposes weapon and gear"}}]}),
        ));
    }
    // 1.1.6: an activity whose draft requires a launcher that shows it.
    if e.draft
        .as_deref()
        .and_then(|d| cfg.draft(d))
        .is_some_and(|d| d.required)
        && matches!(action.as_str(), "register" | "find")
        && !crate::api::account::game_caps(&core, &a.user).drafts()
    {
        return Err(
            ApiError::forbidden("not_eligible", "your launcher cannot show this activity's stage draft").details(
                json!({"requirements": [{"id": "draft", "label": {"en": "A launcher that shows the stage draft"}}]}),
            ),
        );
    }
    // Security review M7: a tournament without guest accounts (`[auth.guest] ranked = false`).
    if e.kind == "tournament" && action == "register" && !crate::auth::may_play_ranked(&core, &a.user) {
        return Err(
            ApiError::forbidden("not_eligible", "guest accounts cannot enter this server's tournaments")
                .details(json!({"requirements": [{"id": "account", "label": crate::auth::ranked_requirement()}]})),
        );
    }
    let notice = match (e.kind.as_str(), action.as_str()) {
        ("tournament", "register") => {
            let l = live(&mut core, &id);
            if l.state != "open" {
                return Err(unavailable("registration is closed"));
            }
            if l.entrants.contains(&a.user) {
                return Err(unavailable("you are registered"));
            }
            if l.entrants.len() >= e.size {
                return Err(unavailable("the tournament is full"));
            }
            l.entrants.push(a.user.clone());
            let full = l.entrants.len() >= e.size;
            if full {
                start_tournament(&mut core, e)?;
            }
            if full {
                "Registered. The tournament starts now."
            } else {
                "Registered."
            }
        }
        ("tournament", "unregister") => {
            let l = live(&mut core, &id);
            if l.state != "open" || !l.entrants.contains(&a.user) {
                return Err(unavailable("you are not registered, or it started"));
            }
            l.entrants.retain(|u| u != &a.user);
            "You left the tournament."
        }
        ("duel_series", "find") => {
            if busy {
                return Err(ApiError::conflict("in_match", "you are in a match"));
            }
            let l = live(&mut core, &id);
            if l.waiting.contains(&a.user) {
                return Err(unavailable("you are already waiting"));
            }
            l.waiting.push(a.user.clone());
            if l.waiting.len() >= 2 {
                let x = l.waiting.remove(0);
                let y = l.waiting.remove(0);
                let labels = [core.display_name(&x), core.display_name(&y)];
                let sid = crate::series::start(
                    &mut core,
                    "activity",
                    &id,
                    [vec![x.clone()], vec![y.clone()]],
                    labels,
                    e.best_of,
                    &e.mode,
                    &e.result_policy,
                    e.keep_open,
                    e.loadout.clone(),
                    e.draft.clone(),
                    crate::spectate::rule_of(&cfg, e.spectators.as_ref()),
                )
                .inspect_err(|_| {
                    // 1.1.13 (`relay_capacity`, every relay full) or another failure: both keep waiting, first in line.
                    let l = live(&mut core, &id);
                    l.waiting.insert(0, y.clone());
                    l.waiting.insert(0, x.clone());
                })?;
                let l = live(&mut core, &id);
                l.series.push(sid.clone());
                if l.series.len() > 50 {
                    l.series.remove(0);
                }
                series_id = Some(sid);
                changed(&mut core, &id, &[x, y]);
                "Opponent found: the series starts."
            } else {
                "Waiting for an opponent."
            }
        }
        ("duel_series", "leave") => {
            let l = live(&mut core, &id);
            if !l.waiting.contains(&a.user) {
                return Err(unavailable("you are not waiting"));
            }
            l.waiting.retain(|u| u != &a.user);
            "You stopped waiting."
        }
        _ => return Err(ApiError::not_found("no such action")),
    };
    changed(&mut core, &id, std::slice::from_ref(&a.user));
    let v = view(&mut core, e, &a.user);
    Ok(Json(
        json!({"view": v, "notice": {"level": "success", "text": en(notice)}, "series_id": series_id, "match_id": null}),
    ))
}

/// Configuration problems of the activities (used by `Config::validate`).
pub fn validate(c: &Config, e: &mut Vec<String>) {
    let mut ids = std::collections::HashSet::new();
    for a in &c.activities {
        let n = format!("activities.{}", a.id);
        if !ids.insert(a.id.as_str()) || !crate::util::is_id(&a.id) {
            e.push(format!("{n}: duplicate or bad id"));
        }
        if !matches!(a.kind.as_str(), "tournament" | "duel_series") {
            e.push(format!("{n}.kind: tournament or duel_series"));
        }
        if !(2..=64).contains(&a.size) {
            e.push(format!("{n}.size: 2 to 64"));
        }
        if a.best_of == 0 || a.best_of % 2 == 0 || a.best_of > 9 {
            e.push(format!("{n}.best_of: an odd number from 1 to 9"));
        }
        if c.policy(&a.result_policy).is_none() {
            e.push(format!("{n}.result_policy: unknown policy"));
        }
    }
    if c.features.activities && c.activities.is_empty() {
        e.push("features.activities needs at least one [[activities]]".into());
    }
    if c.features.activities && !c.features.series {
        e.push("features.activities (the reference kinds) needs features.series".into());
    }
}

/// Restores the activities' live state saved before a restart (series and matches restore themselves).
pub fn restore(core: &mut Core) -> rusqlite::Result<()> {
    let rows: Vec<(String, String)> = {
        let mut st = core.db.prepare("SELECT id, data FROM activity_state")?;
        st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.flatten().collect()
    };
    for (id, data) in rows {
        if core.cfg.activities.iter().any(|a| a.id == id)
            && let Ok(a) = serde_json::from_str::<ActLive>(&data)
        {
            core.live.activities.insert(id, a);
        }
    }
    Ok(())
}
