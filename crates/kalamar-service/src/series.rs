//! Series of matches (protocol 1.1, `series.md`): best of N between two fixed sides. Each game is an ordinary match;
//! when one is decided the series counts it and creates the next, reusing the relay room so engines of contract 2
//! can play the next round in the same game session (`continue_from`).

use kalamar_protocol::Team;
use kalamar_protocol::decision::Decision;
use rusqlite::{OptionalExtension, params};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::error::{ApiError, ApiResult};
use crate::matches::{MatchExtras, NewSeat, create_match};
use crate::state::{Core, LiveMatch, MatchRules, SeriesRef};
use crate::util::{new_id, new_relay_room, random_index, rfc3339};

/// A void game is replayed at most this many times; then the series is void.
pub const MAX_REPLAYS: u32 = 2;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Game {
    pub index: usize,
    pub match_id: String,
    pub state: String,
    /// The side that won (0 or 1).
    pub winner: Option<usize>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Series {
    pub id: String,
    pub origin_kind: String,
    pub origin_id: String,
    pub best_of: u32,
    /// Side 0 plays alpha, side 1 bravo.
    pub sides: [Vec<String>; 2],
    pub labels: [String; 2],
    pub wins: [u32; 2],
    pub games: Vec<Game>,
    pub state: String,
    pub winner: Option<usize>,
    pub room: String,
    /// The relay (`host:port`) of the first game, kept for the next ones.
    #[serde(default)]
    pub relay: Option<String>,
    /// The result policy as the first game was created with it, kept for the next ones while the service runs (a
    /// reload that changes or removes it does not change a series under way).
    #[serde(skip)]
    pub policy: Option<std::sync::Arc<crate::config::PolicyEntry>>,
    pub mode: String,
    pub result_policy: String,
    pub replays: u32,
    /// The host of every game (the engine's room belongs to its host).
    #[serde(default)]
    pub host: Option<String>,
    /// Keep the game session open between games (contract 2 rounds; off by default: relaunch is faster).
    #[serde(default)]
    pub keep_open: bool,
    /// Weapon and gear constraints of every game.
    #[serde(default)]
    pub loadout: Option<Value>,
    /// 1.1.6: the stage draft of the games (a `[[drafts]]` id).
    #[serde(default)]
    pub draft: Option<String>,
    /// 1.1.6, `series = "once"`: the stages of the games, in order, from the first game's draft.
    #[serde(default)]
    pub draft_order: Vec<String>,
    /// 1.1.6: the stages of the decided games, in order.
    #[serde(default)]
    pub played: Vec<String>,
    /// 1.1.7: the places in the stands of every game (`spectators.md`); `None`: none.
    #[serde(default)]
    pub stands: Option<crate::state::StandsRule>,
    /// 1.1.7: who sits in the stands: the place holds for every next game until it is left.
    #[serde(default)]
    pub spectators: Vec<Watcher>,
    /// 1.1.7: the stands changed since the last game: the next one is a new game session (`continue_from` needs the
    /// same members).
    #[serde(default)]
    pub session_changed: bool,
    pub created_at: i64,
    pub ended_at: Option<i64>,
}

/// 1.1.7: a spectator of a series.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct Watcher {
    pub user: String,
    pub referee: bool,
}

impl Series {
    pub fn needed(&self) -> u32 {
        self.best_of / 2 + 1
    }
    pub fn users(&self) -> Vec<String> {
        self.sides.iter().flatten().cloned().collect()
    }
}

pub fn series_json(core: &Core, s: &Series) -> Value {
    let side = |i: usize| {
        json!({
            "team": if i == 0 { "alpha" } else { "bravo" },
            "label": {"en": s.labels[i]},
            "users": s.sides[i].iter().map(|u| core.user_json(u)).collect::<Vec<_>>(),
            "wins": s.wins[i],
        })
    };
    let team = |w: Option<usize>| w.map(|i| if i == 0 { "alpha" } else { "bravo" });
    let mut origin = json!({"kind": s.origin_kind});
    origin[format!("{}_id", s.origin_kind)] = json!(s.origin_id);
    json!({
        "id": s.id,
        "origin": origin,
        "format": {"kind": "best_of", "games": s.best_of},
        "state": s.state,
        "sides": [side(0), side(1)],
        "games": s.games.iter().map(|g| json!({"index": g.index, "match_id": g.match_id, "state": g.state, "winner": team(g.winner)})).collect::<Vec<_>>(),
        "winner": team(s.winner),
        "created_at": rfc3339(s.created_at),
        "ended_at": s.ended_at.map(rfc3339),
        // 1.1.6: the stages played, and the order a once-per-series draft gave.
        "draft": s.draft.as_ref().map(|d| json!({"preset": d, "stages": s.draft_order, "played": s.played})),
        // 1.1.7: the places in the stands and who sits there (for every next game).
        "spectators": s.stands.as_ref().map(|st| json!({"seats": st.seats, "who": st.who,
            "users": s.spectators.iter().map(|w| core.user_json(&w.user)).collect::<Vec<_>>()})),
    })
}

pub(crate) fn save(core: &Core, s: &Series) -> ApiResult<()> {
    core.db.execute(
        "INSERT INTO series (id, origin_kind, origin_id, state, data, created_at, ended_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
         ON CONFLICT (id) DO UPDATE SET state = ?4, data = ?5, ended_at = ?7",
        params![s.id, s.origin_kind, s.origin_id, s.state, serde_json::to_string(s).unwrap_or_default(), s.created_at, s.ended_at],
    )?;
    Ok(())
}

pub fn load(core: &Core, id: &str) -> ApiResult<Option<Series>> {
    if let Some(s) = core.live.series.get(id) {
        return Ok(Some(s.clone()));
    }
    let data: Option<String> = core
        .db
        .query_row("SELECT data FROM series WHERE id = ?1", [id], |r| r.get(0))
        .optional()?;
    Ok(data.and_then(|d| serde_json::from_str(&d).ok()))
}

/// `series.updated` to its players and (1.1.7) its spectators.
pub(crate) fn notify(core: &mut Core, s: &Series) {
    let v = series_json(core, s);
    let mut to = s.users();
    to.extend(s.spectators.iter().map(|w| w.user.clone()));
    core.push_many(&to, "series.updated", &v);
}

/// Starts a best-of-`best_of` series between two sides and creates its first game.
#[allow(clippy::too_many_arguments)]
pub fn start(
    core: &mut Core,
    origin_kind: &str,
    origin_id: &str,
    sides: [Vec<String>; 2],
    labels: [String; 2],
    best_of: u32,
    mode: &str,
    result_policy: &str,
    keep_open: bool,
    loadout: Option<Value>,
    draft: Option<String>,
    stands: Option<crate::state::StandsRule>,
) -> ApiResult<String> {
    let s = Series {
        id: new_id("sr"),
        origin_kind: origin_kind.to_string(),
        origin_id: origin_id.to_string(),
        best_of: best_of.max(1) | 1,
        sides,
        labels,
        wins: [0, 0],
        games: Vec::new(),
        state: "running".into(),
        winner: None,
        room: new_relay_room(),
        relay: None,
        policy: None,
        host: None,
        keep_open,
        loadout,
        draft,
        draft_order: Vec::new(),
        played: Vec::new(),
        stands,
        spectators: Vec::new(),
        session_changed: false,
        mode: mode.to_string(),
        result_policy: result_policy.to_string(),
        replays: 0,
        created_at: core.now,
        ended_at: None,
    };
    let id = s.id.clone();
    core.live.series.insert(id.clone(), s);
    next_game(core, &id)?;
    Ok(id)
}

fn next_game(core: &mut Core, id: &str) -> ApiResult<()> {
    let s = core
        .live
        .series
        .get(id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("no such series"))?;
    let mut seats = Vec::new();
    for (side, team) in [(0usize, Team::Alpha), (1, Team::Bravo)] {
        for u in &s.sides[side] {
            seats.push(NewSeat {
                user_id: u.clone(),
                team,
                party_id: None,
                ticket_id: None,
            });
        }
    }
    // The same host for every game of the series; drawn once.
    let host = s
        .host
        .as_ref()
        .and_then(|h| seats.iter().position(|x| &x.user_id == h))
        .unwrap_or_else(|| random_index(seats.len()));
    let host_user = seats[host].user_id.clone();
    let rules = MatchRules {
        teams: [s.sides[0].len(), s.sides[1].len()],
        mode: s.mode.clone(),
        stage: None,
        options: json!({}),
        rated: false,
        ladder: None,
        season: None,
        result_policy: s.result_policy.clone(),
        loadout: s.loadout.clone(),
        spectators: s.stands.clone(),
    };
    let index = s.games.len() + 1;
    // 1.1.6: the draft of this game: before every game (stages played excluded, the previous game's winner and
    // loser known), or once before the first game whose picks give every game's stage.
    let mut rules = rules;
    let decided: Vec<&Game> = s.games.iter().filter(|g| g.state == "completed").collect();
    let preset = s.draft.as_ref().and_then(|d| core.cfg.draft(d));
    let draft = match preset {
        Some(p) if p.series == "once" && decided.is_empty() && s.draft_order.is_empty() => {
            Some(crate::draft::DraftPlan {
                preset: p.id.clone(),
                first_game: true,
                once: true,
                ..Default::default()
            })
        }
        Some(p) if p.series == "once" => {
            rules.stage = s.draft_order.get(s.played.len()).cloned().or_else(|| {
                let left: Vec<&String> = s.draft_order.iter().filter(|x| !s.played.contains(x)).collect();
                (!left.is_empty()).then(|| left[random_index(left.len())].clone())
            });
            None
        }
        Some(p) => Some(crate::draft::DraftPlan {
            preset: p.id.clone(),
            played: s.played.clone(),
            previous_winner: decided
                .last()
                .and_then(|g| g.winner)
                .map(|w| if w == 0 { Team::Alpha } else { Team::Bravo }),
            first_game: decided.is_empty(),
            ..Default::default()
        }),
        None => None,
    };
    // 1.1.7: the spectators of the series watch this game too; a change in the stands starts a new game session.
    let fresh = s.session_changed;
    let extras = MatchExtras {
        draft,
        series: Some(SeriesRef {
            id: s.id.clone(),
            index,
            best_of: s.best_of,
        }),
        relay_room: s.keep_open.then(|| s.room.clone()),
        relay: s.relay.clone(),
        continue_from: if s.keep_open && !fresh {
            s.games.last().map(|g| g.match_id.clone())
        } else {
            None
        },
        more_games: s.keep_open,
        spectators: s
            .spectators
            .iter()
            .map(|w| crate::matches::NewSpectator {
                user_id: w.user.clone(),
                referee: w.referee,
            })
            .collect(),
        spectator_host: None,
        policy: s.policy.clone(),
    };
    let kind: &'static str = match s.origin_kind.as_str() {
        "room" => "room",
        "queue" => "queue",
        _ => "activity",
    };
    let mid = create_match(core, kind, &s.origin_id, rules, seats, host, false, extras)?;
    let (relay, room, policy) = core
        .live
        .matches
        .get(&mid)
        .map(|m| {
            (
                Some(format!("{}:{}", m.game.relay_host, m.game.relay_port)),
                m.game.relay_room.clone(),
                Some(m.policy.clone()),
            )
        })
        .unwrap_or_default();
    if let Some(s) = core.live.series.get_mut(id) {
        s.relay = relay.or(s.relay.take());
        if s.policy.is_none() {
            s.policy = policy;
        }
        // The room as the relay knows it (sealed for a bound relay): the session's next rounds reuse it as it is.
        if let Some(room) = room {
            s.room = room;
        }
        s.games.push(Game {
            index,
            match_id: mid,
            state: "ready".into(),
            winner: None,
        });
        s.host = Some(host_user);
        s.session_changed = false;
    }
    let s = core.live.series[id].clone();
    save(core, &s)?;
    notify(core, &s);
    Ok(())
}

/// A game of a series was decided: count it, then create the next game or settle the series.
pub fn on_game_end(core: &mut Core, m: &LiveMatch, d: &Decision) -> ApiResult<()> {
    let Some(r) = &m.game.series else { return Ok(()) };
    let Some(mut s) = core.live.series.get(&r.id).cloned() else {
        return Ok(());
    };
    let side = d
        .winner
        .filter(|_| d.counted)
        .map(|t| if t == Team::Alpha { 0 } else { 1 });
    if let Some(g) = s.games.iter_mut().find(|g| g.match_id == m.id) {
        g.state = if d.counted { "completed".into() } else { "void".into() };
        g.winner = side;
    }
    match side {
        Some(i) => s.wins[i] += 1,
        None => s.replays += 1,
    }
    if d.counted {
        let stage = d
            .record
            .as_ref()
            .and_then(|r| {
                r.public_json()
                    .get("stage_scene")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .or_else(|| m.rules.stage.clone());
        if let Some(st) = stage {
            s.played.push(st);
        }
    }
    settle(core, s)
}

/// A game of a series was canceled during its draft (a player abandoned it): it is replayed like a void game.
pub fn on_game_cancelled(core: &mut Core, m: &LiveMatch) -> ApiResult<()> {
    let Some(r) = &m.game.series else { return Ok(()) };
    let Some(mut s) = core.live.series.get(&r.id).cloned() else {
        return Ok(());
    };
    if let Some(g) = s.games.iter_mut().find(|g| g.match_id == m.id) {
        g.state = "void".into();
    }
    s.replays += 1;
    settle(core, s)
}

/// 1.1.6: the once-per-series draft is over: its picks are the games' stages.
pub fn on_draft_done(core: &mut Core, id: &str, order: Vec<String>) -> ApiResult<()> {
    if let Some(s) = core.live.series.get_mut(id) {
        s.draft_order = order;
        let s = s.clone();
        save(core, &s)?;
        notify(core, &s);
    }
    Ok(())
}

/// After a game: the series is settled (won, or void after too many replays), or its next game is created.
fn settle(core: &mut Core, mut s: Series) -> ApiResult<()> {
    let settled = s.wins.iter().position(|w| *w >= s.needed());
    let decided = s.wins[0] + s.wins[1];
    if let Some(i) = settled {
        s.state = "completed".into();
        s.winner = Some(i);
    } else if s.replays > MAX_REPLAYS || decided >= s.best_of {
        s.state = "void".into();
    }
    if s.state != "running" {
        s.ended_at = Some(core.now);
        core.live.series.remove(&s.id);
        save(core, &s)?;
        notify(core, &s);
        core.emit("series.completed", series_json(core, &s));
        if core.cfg.features.badges {
            crate::badges::on_series_end(core, &s);
        }
        crate::activities::on_series_end(core, &s);
        return Ok(());
    }
    core.live.series.insert(s.id.clone(), s.clone());
    save(core, &s)?;
    next_game(core, &s.id)?;
    // 1.1.2: an activity's view shows the score after every decided game, not only at the end.
    crate::activities::on_series_progress(core, &s);
    Ok(())
}

/// `GET /v1/series/{series_id}`.
pub async fn get(
    axum::extract::State(st): axum::extract::State<crate::state::AppState>,
    a: crate::auth::Authed,
    axum::extract::Path(id): axum::extract::Path<String>,
) -> ApiResult<axum::Json<Value>> {
    let core = st.lock();
    if !core.cfg.features.series {
        return Err(ApiError::feature_disabled());
    }
    let s = load(&core, &id)?.ok_or_else(|| ApiError::not_found("no such series"))?;
    let member = s.users().contains(&a.user) || s.spectators.iter().any(|w| w.user == a.user);
    if !member && (s.state == "running" || !core.cfg.features.stats) {
        return Err(ApiError::not_found("no such series"));
    }
    Ok(axum::Json(series_json(&core, &s)))
}

/// The series a match belongs to, for the match object.
pub fn match_member(r: &SeriesRef) -> Value {
    json!({"id": r.id, "index": r.index, "format": {"kind": "best_of", "games": r.best_of}})
}

/// Restores the running series after a restart (their matches are restored by the match engine).
pub fn restore(core: &mut Core) -> rusqlite::Result<()> {
    let rows: Vec<String> = {
        let mut st = core.db.prepare("SELECT data FROM series WHERE state = 'running'")?;
        st.query_map([], |r| r.get(0))?.flatten().collect()
    };
    for data in rows {
        if let Ok(s) = serde_json::from_str::<Series>(&data) {
            core.live.series.insert(s.id.clone(), s);
        }
    }
    Ok(())
}
