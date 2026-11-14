//! Stage and mode drafts, "pick and ban" (protocol 1.1.6, `draft.md`): a phase between "every player accepted" (or a
//! room start) and the launch, driven entirely by the server. The match carries the draft's state (`draft` of the
//! match object: the options with their state, the turns, the current turn, who may act); launchers render it and
//! send one player's action (`POST /v1/matches/{id}/draft`); the server resolves turns, plays the turns of teams
//! whose launchers cannot draft or that let their time run out, and finally writes the result into the match's rules
//! (`rules.stage`, `rules.mode`) before it becomes `ready`. Nothing about the draft is needed to play the match.
//!
//! The engine ([`Draft`]) is pure: time, the players online and the random draws come from a [`Ctx`]. The functions
//! after it bind it to the service (building a draft from a queue, a room or a series, the endpoint, the ticker,
//! completion and abandonment).

use std::collections::{BTreeMap, HashMap, HashSet};

use axum::Json;
use axum::extract::{Path, State};
use kalamar_protocol::Team;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::auth::Authed;
use crate::config::{Config, Text};
use crate::error::{ApiError, ApiResult, JsonBody};
use crate::matches::NewSeat;
use crate::state::{AppState, Core, MState};
use crate::util::{random_index, rfc3339};

/// The version of the draft object and of the launcher capability `launcher.draft`.
pub const VERSION: u32 = 1;
/// The offense recorded for a player who let a turn run out (counted only when `penalties.counted` lists it).
pub const OFFENCE: &str = "draft_timeout";
/// At most this many options and turns in one draft.
pub const MAX_OPTIONS: usize = 128;
pub const MAX_TURNS: usize = 64;

// ---------------------------------------------------------------------------------------------- turns as configured

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Kind {
    Stage,
    Mode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Action {
    Ban,
    Pick,
    Random,
    Last,
}

/// A team named in a turn, relative to the draft's `first` team or to the previous game of a series.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TeamRef {
    First,
    Second,
    Alpha,
    Bravo,
    Both,
    Winner,
    Loser,
}

/// One configured turn token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spec {
    Turn {
        action: Action,
        team: Option<TeamRef>,
        count: usize,
        kind: Kind,
    },
    /// Alternate single bans from `first` until one option is left, which is then played (`last`).
    Strike { kind: Kind },
}

impl Spec {
    pub fn kind(&self) -> Kind {
        match self {
            Spec::Turn { kind, .. } | Spec::Strike { kind } => *kind,
        }
    }
    /// The turn settles an option of its kind (a pick, a draw, the last one, a strike).
    fn resolves(&self) -> bool {
        !matches!(
            self,
            Spec::Turn {
                action: Action::Ban,
                ..
            }
        )
    }
}

/// Parses `ban:<team>:<n>`, `pick:<team>:<n>`, `random[:<n>]`, `last`, `strike`, each with an optional `@stage` or
/// `@mode`.
pub fn parse_turn(s: &str) -> Result<Spec, String> {
    let (body, kind) = match s.split_once('@') {
        None => (s, Kind::Stage),
        Some((b, "stage")) => (b, Kind::Stage),
        Some((b, "mode")) => (b, Kind::Mode),
        Some(_) => return Err(format!("`{s}`: after @ comes stage or mode")),
    };
    let parts: Vec<&str> = body.split(':').collect();
    let count = |p: Option<&&str>| -> Result<usize, String> {
        match p {
            None => Ok(1),
            Some(n) => n
                .parse::<usize>()
                .ok()
                .filter(|n| (1..=16).contains(n))
                .ok_or_else(|| format!("`{s}`: a count from 1 to 16")),
        }
    };
    let team = |t: &str| -> Result<TeamRef, String> {
        Ok(match t {
            "first" => TeamRef::First,
            "second" => TeamRef::Second,
            "alpha" => TeamRef::Alpha,
            "bravo" => TeamRef::Bravo,
            "both" => TeamRef::Both,
            "winner" => TeamRef::Winner,
            "loser" => TeamRef::Loser,
            _ => {
                return Err(format!(
                    "`{s}`: the team is first, second, alpha, bravo, both, winner or loser"
                ));
            }
        })
    };
    match parts.as_slice() {
        [a @ ("ban" | "pick"), t, rest @ ..] if rest.len() <= 1 => Ok(Spec::Turn {
            action: if *a == "ban" { Action::Ban } else { Action::Pick },
            team: Some(team(t)?),
            count: count(rest.first())?,
            kind,
        }),
        ["random", rest @ ..] if rest.len() <= 1 => Ok(Spec::Turn {
            action: Action::Random,
            team: None,
            count: count(rest.first())?,
            kind,
        }),
        ["last"] => Ok(Spec::Turn {
            action: Action::Last,
            team: None,
            count: 1,
            kind,
        }),
        ["strike"] => Ok(Spec::Strike { kind }),
        _ => Err(format!(
            "`{s}`: ban:<team>:<n>, pick:<team>:<n>, random[:<n>], last or strike, optionally @stage or @mode"
        )),
    }
}

// ---------------------------------------------------------------------------------------------- the draft's state

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum OState {
    Available,
    Banned,
    Picked,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TState {
    Pending,
    Current,
    Done,
    Skipped,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DState {
    /// Planned, the match waits for every player's answer.
    Waiting,
    Running,
    Done,
    Cancelled,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Decider {
    Captain,
    Vote,
    Any,
}

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Opt {
    pub id: String,
    pub kind: Kind,
    pub state: OState,
    #[serde(default)]
    pub by: Option<Team>,
    #[serde(default)]
    pub turn: Option<usize>,
    /// Why an option is unavailable: `game` (a player's game cannot load it), `played` (already played in the
    /// series), `combination` (not offered with the mode or stage chosen).
    #[serde(default)]
    pub reason: Option<String>,
    /// Picks in order (1 = the match's own).
    #[serde(default)]
    pub order: Option<u32>,
}

/// What a team (or the server, `team: None`) chose in a turn, and how: `players` (the captain or a player),
/// `vote`, `timeout` (the server, the time ran out), `server` (nobody of the team can draft, or a draw).
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
pub struct Choice {
    pub team: Option<Team>,
    pub ids: Vec<String>,
    pub by: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Turn {
    pub action: Action,
    pub kind: Kind,
    pub teams: Vec<Team>,
    /// The count asked by the configuration; `eff` the count in effect once the turn started (never banning the
    /// last option).
    pub count: usize,
    #[serde(default)]
    pub eff: usize,
    pub state: TState,
    #[serde(default)]
    pub started_at: Option<i64>,
    #[serde(default)]
    pub deadline: Option<i64>,
    #[serde(default)]
    pub ended_at: Option<i64>,
    /// The resolved choices (public once the turn is over).
    #[serde(default)]
    pub results: Vec<Choice>,
    /// A team's final choice while the other team of the same turn has not chosen yet (hidden from it).
    #[serde(default)]
    pub staged: [Option<(Vec<String>, String)>; 2],
    /// Votes of the current turn, by player.
    #[serde(default)]
    pub votes: BTreeMap<String, Vec<String>>,
}

/// One team in the draft.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Side {
    /// The team's players, in seat order.
    pub members: Vec<String>,
    /// Those whose launcher shows the draft (`launcher.draft`); nobody: the server plays the team's turns.
    pub capable: Vec<String>,
    /// The captain chosen when the draft was planned (party leader, else the first capable player).
    pub captain_pref: Option<String>,
    /// The captain of the current turn (the next connected capable player when the chosen one is offline).
    pub captain: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Draft {
    pub version: u32,
    pub preset: String,
    pub title: Text,
    #[serde(default)]
    pub summary: Text,
    pub decider: Decider,
    /// Ties of a vote: `captain` or `random`.
    pub tiebreak: String,
    /// A timed-out or automatic team turn: `random` or `skip`.
    pub timeout: String,
    pub turn_s: u32,
    pub state: DState,
    pub first: Option<Team>,
    pub options: Vec<Opt>,
    pub turns: Vec<Turn>,
    pub current: Option<usize>,
    pub sides: [Side; 2],
    /// Allowed (mode, stage) combinations; empty: any.
    #[serde(default)]
    pub pairs: Vec<(String, String)>,
    #[serde(default)]
    pub next_order: u32,
    /// Players who let a turn run out and were not yet reported (for the `draft_timeout` offense).
    #[serde(default)]
    pub idle: Vec<String>,
    /// The picks are the stages of every game of a series, in order (`series = "once"`).
    #[serde(default)]
    pub once: bool,
}

/// What the engine needs from outside: the time, who is connected, and a uniform draw in `0..n`.
pub struct Ctx<'a> {
    pub now: i64,
    pub online: &'a HashSet<String>,
    pub rng: &'a mut dyn FnMut(usize) -> usize,
}

/// Why an action is refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ActError {
    /// 403 `not_your_turn`.
    NotYourTurn(&'static str),
    /// 409 `draft_turn`: the turn is not open for this team (over, or another one).
    Stale(&'static str),
    /// 422 `validation_failed` on `/choices`.
    Invalid(String),
    /// 409 `match_state`: no draft is running.
    NotRunning,
}

/// What an accepted action changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Acted {
    /// Something changed (an identical repeat changes nothing).
    pub changed: bool,
    /// Everyone sees the change; `false`: only the actor's team (a vote that did not settle the team's choice).
    pub public: bool,
}

pub fn ti(t: Team) -> usize {
    match t {
        Team::Alpha => 0,
        Team::Bravo => 1,
    }
}

const TEAMS: [Team; 2] = [Team::Alpha, Team::Bravo];

impl Draft {
    pub fn team_of(&self, user: &str) -> Option<Team> {
        TEAMS
            .into_iter()
            .find(|t| self.sides[ti(*t)].members.iter().any(|u| u == user))
    }

    fn avail(&self, kind: Kind) -> Vec<usize> {
        self.options
            .iter()
            .enumerate()
            .filter(|(_, o)| o.kind == kind && o.state == OState::Available)
            .map(|(i, _)| i)
            .collect()
    }

    pub fn has_kind(&self, kind: Kind) -> bool {
        self.options.iter().any(|o| o.kind == kind)
    }

    /// `n` distinct available options of `kind`, drawn at random.
    fn draw(&self, kind: Kind, n: usize, rng: &mut dyn FnMut(usize) -> usize) -> Vec<String> {
        let mut pool = self.avail(kind);
        let mut out = Vec::new();
        while out.len() < n && !pool.is_empty() {
            let k = rng(pool.len()).min(pool.len() - 1);
            out.push(self.options[pool.remove(k)].id.clone());
        }
        out
    }

    /// The picks of `kind`, in order.
    pub fn picked(&self, kind: Kind) -> Vec<String> {
        let mut v: Vec<&Opt> = self
            .options
            .iter()
            .filter(|o| o.kind == kind && o.state == OState::Picked)
            .collect();
        v.sort_by_key(|o| o.order.unwrap_or(u32::MAX));
        v.into_iter().map(|o| o.id.clone()).collect()
    }

    pub fn result_stage(&self) -> Option<String> {
        self.picked(Kind::Stage).into_iter().next()
    }

    pub fn result_mode(&self) -> Option<String> {
        self.picked(Kind::Mode).into_iter().next()
    }

    /// Starts a planned draft.
    pub fn begin(&mut self, cx: &mut Ctx) {
        if self.state != DState::Waiting {
            return;
        }
        self.state = DState::Running;
        self.advance(cx);
    }

    /// Starts turns until one waits for players, or closes the draft.
    fn advance(&mut self, cx: &mut Ctx) {
        while let Some(i) = self.turns.iter().position(|t| t.state == TState::Pending) {
            if self.start_turn(i, cx) {
                self.current = Some(i);
                return;
            }
        }
        self.close(cx);
    }

    /// Who may act for `team` in its turns now: the captain, or every capable player (vote, any).
    fn actors(&self, team: Team) -> Vec<String> {
        let s = &self.sides[ti(team)];
        match self.decider {
            Decider::Captain => s.captain.iter().cloned().collect(),
            Decider::Vote | Decider::Any => s.capable.clone(),
        }
    }

    fn refresh_captain(&mut self, team: Team, online: &HashSet<String>) {
        let captain_decides = self.decider == Decider::Captain;
        let s = &mut self.sides[ti(team)];
        let pref = s.captain_pref.clone();
        s.captain = match pref {
            Some(p) if !captain_decides || online.contains(&p) => Some(p),
            pref => s.capable.iter().find(|u| online.contains(*u)).cloned().or(pref),
        };
    }

    /// What the server chooses for a team that cannot or did not act.
    fn auto_choice(&self, kind: Kind, eff: usize, rng: &mut dyn FnMut(usize) -> usize) -> Vec<String> {
        if self.timeout == "skip" {
            Vec::new()
        } else {
            self.draw(kind, eff, rng)
        }
    }

    /// Starts turn `i`; `true` when it waits for players.
    fn start_turn(&mut self, i: usize, cx: &mut Ctx) -> bool {
        let (action, kind, count, teams) = {
            let t = &self.turns[i];
            (t.action, t.kind, t.count, t.teams.clone())
        };
        let avail = self.avail(kind).len();
        let n = teams.len().max(1);
        let eff = match action {
            Action::Ban => count.min(avail.saturating_sub(1) / n),
            Action::Pick => count.min(avail / n),
            Action::Random => count.min(avail),
            Action::Last => usize::from(avail > 0),
        };
        {
            let t = &mut self.turns[i];
            t.eff = eff;
            t.started_at = Some(cx.now);
        }
        if eff == 0 {
            let t = &mut self.turns[i];
            t.state = TState::Skipped;
            t.ended_at = Some(cx.now);
            return false;
        }
        if matches!(action, Action::Random | Action::Last) {
            let ids = self.draw(kind, eff, cx.rng);
            self.turns[i].results.push(Choice {
                team: None,
                ids,
                by: "server".into(),
            });
            self.apply_turn(i, cx);
            return false;
        }
        for &team in &teams {
            self.refresh_captain(team, cx.online);
            if self.actors(team).is_empty() {
                let ids = self.auto_choice(kind, eff, cx.rng);
                self.turns[i].staged[ti(team)] = Some((ids, "server".into()));
            }
        }
        let t = &mut self.turns[i];
        if teams.iter().all(|x| t.staged[ti(*x)].is_some()) {
            self.apply_turn(i, cx);
            return false;
        }
        t.state = TState::Current;
        t.deadline = Some(cx.now + i64::from(self.turn_s.max(1)));
        true
    }

    fn mark(&mut self, k: usize, action: Action, team: Option<Team>, turn: usize) {
        let o = &mut self.options[k];
        o.by = team;
        o.turn = Some(turn);
        if action == Action::Ban {
            o.state = OState::Banned;
        } else {
            self.next_order += 1;
            o.state = OState::Picked;
            o.order = Some(self.next_order);
        }
    }

    /// Applies the choices of turn `i` (staged team choices in the turn's team order, then the server's) and ends it.
    fn apply_turn(&mut self, i: usize, cx: &mut Ctx) {
        let (action, kind, teams) = {
            let t = &self.turns[i];
            (t.action, t.kind, t.teams.clone())
        };
        let mut pending: Vec<Choice> = Vec::new();
        for team in &teams {
            if let Some((ids, by)) = self.turns[i].staged[ti(*team)].take() {
                pending.push(Choice {
                    team: Some(*team),
                    ids,
                    by,
                });
            }
        }
        pending.append(&mut self.turns[i].results);
        let mut results = Vec::new();
        for c in pending {
            let mut applied = Vec::new();
            for id in &c.ids {
                let k = self.options.iter().position(|o| o.kind == kind && &o.id == id);
                match k {
                    Some(k) if self.options[k].state == OState::Available => {
                        self.mark(k, action, c.team, i);
                        applied.push(id.clone());
                    }
                    // Picked by the other team in the same turn: a draw instead.
                    _ if action != Action::Ban => {
                        if let Some(x) = self.draw(kind, 1, cx.rng).into_iter().next()
                            && let Some(k) = self.options.iter().position(|o| o.kind == kind && o.id == x)
                        {
                            self.mark(k, action, c.team, i);
                            applied.push(x);
                        }
                    }
                    _ => {}
                }
            }
            results.push(Choice {
                team: c.team,
                ids: applied,
                by: c.by,
            });
        }
        self.apply_pairs();
        let t = &mut self.turns[i];
        t.results = results;
        t.state = TState::Done;
        t.ended_at = Some(cx.now);
        t.deadline = None;
        t.votes.clear();
        if self.current == Some(i) {
            self.current = None;
        }
    }

    /// With allowed (mode, stage) combinations: what the picks so far exclude.
    fn apply_pairs(&mut self) {
        if self.pairs.is_empty() {
            return;
        }
        let modes = self.picked(Kind::Mode);
        let stages = self.picked(Kind::Stage);
        let pairs = self.pairs.clone();
        for o in &mut self.options {
            if o.state != OState::Available {
                continue;
            }
            let ok = match o.kind {
                Kind::Stage => modes.is_empty() || pairs.iter().any(|(m, s)| s == &o.id && modes.contains(m)),
                Kind::Mode => stages.is_empty() || pairs.iter().any(|(m, s)| m == &o.id && stages.contains(s)),
            };
            if !ok {
                o.state = OState::Unavailable;
                o.reason = Some("combination".into());
            }
        }
    }

    /// Every turn played: a kind without a pick gets a draw among what remains.
    fn close(&mut self, cx: &mut Ctx) {
        for kind in [Kind::Mode, Kind::Stage] {
            if self.has_kind(kind) && self.picked(kind).is_empty() && !self.avail(kind).is_empty() {
                let ids = self.draw(kind, 1, cx.rng);
                self.turns.push(Turn {
                    action: Action::Random,
                    kind,
                    teams: Vec::new(),
                    count: 1,
                    eff: 1,
                    state: TState::Pending,
                    started_at: Some(cx.now),
                    deadline: None,
                    ended_at: None,
                    results: vec![Choice {
                        team: None,
                        ids,
                        by: "server".into(),
                    }],
                    staged: [None, None],
                    votes: BTreeMap::new(),
                });
                let i = self.turns.len() - 1;
                self.apply_turn(i, cx);
            }
        }
        self.current = None;
        self.state = DState::Done;
    }

    /// The team's choice from its votes: most votes first; ties by the captain's vote (`tiebreak = captain`), then
    /// at random.
    fn tally(&self, team: Team, eff: usize, rng: &mut dyn FnMut(usize) -> usize, i: usize) -> Vec<String> {
        let s = &self.sides[ti(team)];
        let votes = &self.turns[i].votes;
        let mut count: HashMap<&str, usize> = HashMap::new();
        for u in &s.capable {
            for id in votes.get(u).into_iter().flatten() {
                *count.entry(id.as_str()).or_default() += 1;
            }
        }
        let captain: HashSet<&str> = s
            .captain
            .as_ref()
            .and_then(|c| votes.get(c))
            .map(|v| v.iter().map(String::as_str).collect())
            .unwrap_or_default();
        let mut ids: Vec<(&str, usize, bool, usize)> = count
            .into_iter()
            .map(|(id, n)| (id, n, self.tiebreak == "captain" && captain.contains(id), rng(1 << 30)))
            .collect();
        ids.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)).then(a.3.cmp(&b.3)).then(a.0.cmp(b.0)));
        ids.into_iter().take(eff).map(|x| x.0.to_string()).collect()
    }

    /// The team's vote is settled: everyone voted, or exactly `eff` options have a strict majority.
    fn vote_settled(&self, team: Team, i: usize) -> bool {
        let s = &self.sides[ti(team)];
        let t = &self.turns[i];
        let cast = s.capable.iter().filter(|u| t.votes.contains_key(*u)).count();
        if cast == s.capable.len() {
            return true;
        }
        let mut count: HashMap<&str, usize> = HashMap::new();
        for u in &s.capable {
            for id in t.votes.get(u).into_iter().flatten() {
                *count.entry(id.as_str()).or_default() += 1;
            }
        }
        count.values().filter(|n| **n * 2 > s.capable.len()).count() == t.eff
    }

    /// One player's action on turn `turn`.
    pub fn act(&mut self, user: &str, turn: usize, ids: &[String], cx: &mut Ctx) -> Result<Acted, ActError> {
        let same = |a: &[String], b: &[String]| {
            let (mut a, mut b) = (a.to_vec(), b.to_vec());
            a.sort();
            b.sort();
            a == b
        };
        let team = self
            .team_of(user)
            .ok_or(ActError::NotYourTurn("you are not a player of this match"))?;
        let unchanged = Acted {
            changed: false,
            public: false,
        };
        let Some(t) = self.turns.get(turn) else {
            return Err(ActError::Stale("there is no such turn"));
        };
        // A repeat of an action already resolved is answered as a success (idempotence).
        let repeat = t
            .results
            .iter()
            .any(|c| c.team == Some(team) && same(&c.ids, ids) && (c.by == "players" || c.by == "vote"))
            && (t.staged[ti(team)].is_none());
        if self.state != DState::Running || self.current != Some(turn) {
            if repeat || t.staged[ti(team)].as_ref().is_some_and(|(s, _)| same(s, ids)) {
                return Ok(unchanged);
            }
            return Err(if self.state == DState::Running || self.state == DState::Waiting {
                ActError::Stale("this turn is not open")
            } else {
                ActError::NotRunning
            });
        }
        if !t.teams.contains(&team) {
            return Err(ActError::NotYourTurn("this turn is the other team's"));
        }
        let actor = match self.decider {
            Decider::Captain => self.sides[ti(team)].captain.as_deref() == Some(user),
            Decider::Vote | Decider::Any => self.sides[ti(team)].capable.iter().any(|u| u == user),
        };
        if !actor {
            return Err(ActError::NotYourTurn(match self.decider {
                Decider::Captain => "your team's captain chooses",
                _ => "your launcher did not declare the draft",
            }));
        }
        if let Some((s, _)) = &t.staged[ti(team)] {
            return if same(s, ids) {
                Ok(unchanged)
            } else {
                Err(ActError::Stale("your team has chosen for this turn"))
            };
        }
        let mut seen = HashSet::new();
        let avail: HashSet<&str> = self
            .options
            .iter()
            .filter(|o| o.kind == t.kind && o.state == OState::Available)
            .map(|o| o.id.as_str())
            .collect();
        if ids.len() != t.eff {
            return Err(ActError::Invalid(format!("choose exactly {} option(s)", t.eff)));
        }
        for id in ids {
            if !seen.insert(id.as_str()) {
                return Err(ActError::Invalid("an option is given twice".into()));
            }
            if !avail.contains(id.as_str()) {
                return Err(ActError::Invalid(format!(
                    "`{id}` is not an available option of this turn"
                )));
            }
        }
        let teams = t.teams.clone();
        let mut public = true;
        if self.decider == Decider::Vote {
            if self.turns[turn].votes.get(user).is_some_and(|v| same(v, ids)) {
                return Ok(unchanged);
            }
            self.turns[turn].votes.insert(user.to_string(), ids.to_vec());
            if self.vote_settled(team, turn) {
                let eff = self.turns[turn].eff;
                let choice = self.tally(team, eff, cx.rng, turn);
                self.turns[turn].staged[ti(team)] = Some((choice, "vote".into()));
            } else {
                public = false;
            }
        } else {
            self.turns[turn].staged[ti(team)] = Some((ids.to_vec(), "players".into()));
        }
        if teams.iter().all(|x| self.turns[turn].staged[ti(*x)].is_some()) {
            self.apply_turn(turn, cx);
            self.advance(cx);
        }
        Ok(Acted { changed: true, public })
    }

    /// The deadline of the current turn passed: the server acts for every team that has not (a vote's tally when
    /// some voted). `true` when something changed.
    pub fn tick(&mut self, cx: &mut Ctx) -> bool {
        if self.state != DState::Running {
            return false;
        }
        let Some(i) = self.current else {
            return false;
        };
        if self.turns[i].deadline.is_none_or(|d| cx.now < d) {
            return false;
        }
        let (kind, eff, teams) = {
            let t = &self.turns[i];
            (t.kind, t.eff, t.teams.clone())
        };
        for team in teams {
            if self.turns[i].staged[ti(team)].is_some() {
                continue;
            }
            let voters: Vec<String> = self.sides[ti(team)].capable.clone();
            let voted: Vec<&String> = voters.iter().filter(|u| self.turns[i].votes.contains_key(*u)).collect();
            if self.decider == Decider::Vote && !voted.is_empty() {
                let idle: Vec<String> = voters
                    .iter()
                    .filter(|u| !self.turns[i].votes.contains_key(*u))
                    .cloned()
                    .collect();
                self.idle.extend(idle);
                let choice = self.tally(team, eff, cx.rng, i);
                self.turns[i].staged[ti(team)] = Some((choice, "vote".into()));
            } else {
                let idle = self.actors(team);
                self.idle.extend(idle);
                let ids = self.auto_choice(kind, eff, cx.rng);
                self.turns[i].staged[ti(team)] = Some((ids, "timeout".into()));
            }
        }
        self.apply_turn(i, cx);
        self.advance(cx);
        true
    }

    /// An erased account leaves no trace in the draft (its place in the team, the captaincy, its votes); its team's
    /// choices stay.
    pub fn forget(&mut self, user: &str) {
        for s in &mut self.sides {
            s.members.retain(|u| u != user);
            s.capable.retain(|u| u != user);
            if s.captain_pref.as_deref() == Some(user) {
                s.captain_pref = s.capable.first().cloned();
            }
            if s.captain.as_deref() == Some(user) {
                s.captain = s.captain_pref.clone();
            }
        }
        for t in &mut self.turns {
            t.votes.remove(user);
        }
        self.idle.retain(|u| u != user);
    }

    /// Ends a draft that will not finish (the match was canceled).
    pub fn cancel(&mut self, now: i64) {
        if matches!(self.state, DState::Done | DState::Cancelled) {
            return;
        }
        self.state = DState::Cancelled;
        if let Some(i) = self.current.take() {
            let t = &mut self.turns[i];
            t.state = TState::Skipped;
            t.ended_at = Some(now);
            t.deadline = None;
            t.staged = [None, None];
            t.votes.clear();
        }
    }

    /// The draft object of the match (`draft.md`): `viewer` a participant (their team's votes, `you`), or `None`
    /// for the public view (history, operators, webhooks).
    pub fn view(&self, viewer: Option<&str>) -> Value {
        let mine = viewer.and_then(|v| self.team_of(v));
        let opts: Vec<Value> = self
            .options
            .iter()
            .map(|o| {
                json!({"id": o.id, "kind": o.kind, "state": o.state, "by": o.by.map(Team::as_str), "turn": o.turn,
                       "order": o.order, "reason": o.reason})
            })
            .collect();
        let turns: Vec<Value> = self
            .turns
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let acted: Vec<&str> = t
                    .teams
                    .iter()
                    .filter(|x| t.staged[ti(**x)].is_some())
                    .map(|x| x.as_str())
                    .collect();
                json!({
                    "index": i, "action": t.action, "kind": t.kind,
                    "teams": t.teams.iter().map(|x| x.as_str()).collect::<Vec<_>>(),
                    "count": if t.state == TState::Pending { t.count } else { t.eff },
                    "state": t.state,
                    "started_at": t.started_at.map(rfc3339), "deadline": t.deadline.map(rfc3339),
                    "ended_at": t.ended_at.map(rfc3339),
                    "choices": t.results.iter().map(|c| json!({"team": c.team.map(Team::as_str), "ids": c.ids, "by": c.by})).collect::<Vec<_>>(),
                    "acted": acted,
                })
            })
            .collect();
        let cur = self.current.map(|i| &self.turns[i]);
        let side = |team: Team| {
            let s = &self.sides[ti(team)];
            let mut v = json!({
                "players": s.members, "actors": self.actors(team), "captain": s.captain,
                "auto": s.capable.is_empty(),
            });
            if mine == Some(team) && self.decider == Decider::Vote {
                let votes: Vec<Value> = cur
                    .map(|t| {
                        s.capable
                            .iter()
                            .filter_map(|u| t.votes.get(u).map(|ids| json!({"user_id": u, "ids": ids})))
                            .collect()
                    })
                    .unwrap_or_default();
                v["votes"] = json!(votes);
            }
            v
        };
        let picked_stages = self.picked(Kind::Stage);
        let mut d = json!({
            "version": self.version,
            "state": self.state,
            "preset": self.preset,
            "title": self.title,
            "summary": self.summary,
            "decider": self.decider,
            "tiebreak": self.tiebreak,
            "turn_s": self.turn_s,
            "first": self.first.map(Team::as_str),
            "options": opts,
            "turns": turns,
            "current": self.current,
            "teams": {"alpha": side(Team::Alpha), "bravo": side(Team::Bravo)},
            "result": {
                "mode": self.result_mode(),
                "stage": picked_stages.first(),
                "stages": if self.once { json!(picked_stages) } else { json!(picked_stages.iter().take(1).collect::<Vec<_>>()) },
            },
        });
        if let (Some(team), Some(user)) = (mine, viewer) {
            let s = &self.sides[ti(team)];
            let role = match self.decider {
                Decider::Captain if s.captain.as_deref() == Some(user) => "captain",
                Decider::Vote if s.capable.iter().any(|u| u == user) => "voter",
                Decider::Any if s.capable.iter().any(|u| u == user) => "any",
                _ => "none",
            };
            let (can_act, choice) = match cur {
                Some(t) if t.teams.contains(&team) && self.state == DState::Running => {
                    let staged = t.staged[ti(team)].as_ref().map(|(ids, _)| ids.clone());
                    let vote = t.votes.get(user).cloned();
                    (role != "none" && staged.is_none(), vote.or(staged))
                }
                _ => (false, None),
            };
            d["you"] = json!({"team": team.as_str(), "role": role, "can_act": can_act, "choice": choice});
        }
        d
    }
}

// ---------------------------------------------------------------------------------------------- planning

/// What an origin (queue, room, series) gives the draft of one match.
#[derive(Debug, Clone, Default)]
pub struct DraftPlan {
    /// The `[[drafts]]` id.
    pub preset: String,
    /// The origin's stage pool (used when the preset has none).
    pub stages: Vec<String>,
    /// The origin's mode pool (used when the preset has none and it drafts modes).
    pub modes: Vec<String>,
    /// Allowed (mode, stage) combinations of the origin; empty: any.
    pub pairs: Vec<(String, String)>,
    /// Stages already played in the series.
    pub played: Vec<String>,
    /// Series: the winner of the previous decided game (`None` for the first game, or a draw).
    pub previous_winner: Option<Team>,
    /// Series: no game decided yet (`first_game_turns`).
    pub first_game: bool,
    /// The ladder whose ratings say who is `higher_rated`.
    pub ladder: Option<String>,
    /// Series `once`: the picks are every game's stage.
    pub once: bool,
}

/// The ratings of a team on a ladder (mean), for `first = higher_rated / lower_rated`.
fn team_rating(core: &Core, users: &[String], ladder: &str) -> f64 {
    let season = crate::db::current_season(&core.db, core.now)
        .map(|s| s.0)
        .unwrap_or_default();
    let rs: Vec<f64> = users
        .iter()
        .map(|u| crate::api::queues::rating_for(core, u, Some(ladder), &season).rating)
        .collect();
    rs.iter().sum::<f64>() / rs.len().max(1) as f64
}

/// Expands the configured turns for this match.
fn expand(specs: &[Spec], first: Team, winner: Option<Team>, avail: &HashMap<Kind, usize>) -> Vec<Turn> {
    let second = first.other();
    let loser_t = winner.map(Team::other).unwrap_or(first);
    let winner_t = winner.unwrap_or(second);
    let mk = |action: Action, kind: Kind, teams: Vec<Team>, count: usize| Turn {
        action,
        kind,
        teams,
        count,
        eff: 0,
        state: TState::Pending,
        started_at: None,
        deadline: None,
        ended_at: None,
        results: Vec::new(),
        staged: [None, None],
        votes: BTreeMap::new(),
    };
    let mut out = Vec::new();
    for s in specs {
        match s {
            Spec::Turn {
                action,
                team,
                count,
                kind,
            } => {
                let teams = match team {
                    None => Vec::new(),
                    Some(TeamRef::First) => vec![first],
                    Some(TeamRef::Second) => vec![second],
                    Some(TeamRef::Alpha) => vec![Team::Alpha],
                    Some(TeamRef::Bravo) => vec![Team::Bravo],
                    Some(TeamRef::Both) => vec![first, second],
                    Some(TeamRef::Winner) => vec![winner_t],
                    Some(TeamRef::Loser) => vec![loser_t],
                };
                out.push(mk(*action, *kind, teams, *count));
            }
            Spec::Strike { kind } => {
                let n = avail.get(kind).copied().unwrap_or(0);
                for k in 0..n.saturating_sub(1) {
                    out.push(mk(Action::Ban, *kind, vec![if k % 2 == 0 { first } else { second }], 1));
                }
                out.push(mk(Action::Last, *kind, Vec::new(), 1));
            }
        }
        if out.len() >= MAX_TURNS {
            out.truncate(MAX_TURNS);
            break;
        }
    }
    out
}

/// Plans the draft of a match with these seats (`host`: the index of the host), or `None` when there is nothing to
/// draft (the area is off, the preset unknown, or no option the players' games can load).
pub fn build(core: &Core, plan: &DraftPlan, seats: &[NewSeat], host: usize) -> Option<Draft> {
    let p = core.cfg.draft(&plan.preset)?;
    let caps: Vec<crate::api::account::GameCaps> = seats
        .iter()
        .map(|s| crate::api::account::game_caps(core, &s.user_id))
        .collect();
    let specs: Vec<Spec> = match (&p.first_game_turns, plan.first_game) {
        (Some(f), true) => f,
        _ => &p.turns,
    }
    .iter()
    .filter_map(|t| parse_turn(t).ok())
    .collect();
    let mut options: Vec<Opt> = Vec::new();
    let mut seen: HashSet<(Kind, String)> = HashSet::new();
    let mut push = |options: &mut Vec<Opt>, kind: Kind, id: &str, reason: Option<&str>| {
        if options.len() < MAX_OPTIONS && seen.insert((kind, id.to_string())) {
            options.push(Opt {
                id: id.to_string(),
                kind,
                state: if reason.is_some() {
                    OState::Unavailable
                } else {
                    OState::Available
                },
                by: None,
                turn: None,
                reason: reason.map(str::to_string),
                order: None,
            });
        }
    };
    // Modes, when the turns draft them: only those every player's game runs.
    if specs.iter().any(|s| s.kind() == Kind::Mode) {
        let pool = if p.modes.is_empty() { &plan.modes } else { &p.modes };
        for m in pool {
            let ok = caps.iter().all(|c| c.modes.as_ref().is_none_or(|l| l.contains(m)));
            push(&mut options, Kind::Mode, m, (!ok).then_some("game"));
        }
    }
    // Stages: only when the host's game can be told one (a match of contract 2, every player declared it, and a host
    // engine that takes the rule key `stage`); a stage every game involved can load (the host's list for engines that
    // read stages from the host's save).
    let host_caps = caps.get(host);
    let contract_2 = caps.iter().all(|c| c.contract_versions.contains(&2));
    if contract_2
        && host_caps
            .is_some_and(|c| c.can_select_stage && c.rule_keys.as_ref().is_none_or(|k| k.iter().any(|x| x == "stage")))
    {
        let loadable = |s: &str| {
            caps.iter().enumerate().all(|(i, c)| {
                (i != host && c.stage_list.as_deref() == Some("host_save"))
                    || c.stages.as_ref().is_none_or(|l| l.iter().any(|x| x == s))
            })
        };
        let pool: Vec<String> = if !p.stages.is_empty() {
            p.stages.clone()
        } else if !plan.stages.is_empty() {
            plan.stages.clone()
        } else {
            host_caps
                .and_then(|c| c.stages.clone())
                .or_else(|| caps.iter().find_map(|c| c.stages.clone()))
                .unwrap_or_default()
                .into_iter()
                .filter(|s| loadable(s))
                .collect()
        };
        for s in &pool {
            let reason = if !loadable(s) {
                Some("game")
            } else if p.exclude_played && plan.played.contains(s) {
                Some("played")
            } else {
                None
            };
            push(&mut options, Kind::Stage, s, reason);
        }
    }
    let avail = |k: Kind| {
        options
            .iter()
            .filter(|o| o.kind == k && o.state == OState::Available)
            .count()
    };
    if avail(Kind::Stage) == 0 && avail(Kind::Mode) == 0 {
        return None;
    }
    // Who is first.
    let teams_users = |t: Team| -> Vec<String> {
        seats
            .iter()
            .filter(|s| s.team == t)
            .map(|s| s.user_id.clone())
            .collect()
    };
    let coin = || if random_index(2) == 0 { Team::Alpha } else { Team::Bravo };
    let first = match p.first.as_str() {
        "alpha" => Team::Alpha,
        "bravo" => Team::Bravo,
        "loser" => plan.previous_winner.map(Team::other).unwrap_or_else(coin),
        "winner" => plan.previous_winner.unwrap_or_else(coin),
        r @ ("higher_rated" | "lower_rated") => match &plan.ladder {
            Some(l) => {
                let (a, b) = (
                    team_rating(core, &teams_users(Team::Alpha), l),
                    team_rating(core, &teams_users(Team::Bravo), l),
                );
                if (a - b).abs() < f64::EPSILON {
                    coin()
                } else if (a > b) == (r == "higher_rated") {
                    Team::Alpha
                } else {
                    Team::Bravo
                }
            }
            None => coin(),
        },
        _ => coin(),
    };
    let counts: HashMap<Kind, usize> = [(Kind::Stage, avail(Kind::Stage)), (Kind::Mode, avail(Kind::Mode))].into();
    let mut turns = expand(&specs, first, plan.previous_winner, &counts);
    // A kind with options and no turn that settles it gets a draw at the end (visible as its own turn).
    for kind in [Kind::Mode, Kind::Stage] {
        if counts[&kind] > 0 && !specs.iter().any(|s| s.kind() == kind && s.resolves()) && turns.len() < MAX_TURNS {
            let mut t = expand(
                &[Spec::Turn {
                    action: Action::Random,
                    team: None,
                    count: 1,
                    kind,
                }],
                first,
                None,
                &counts,
            );
            turns.append(&mut t);
        }
    }
    // Sides: the capable players, the captain (the leader of the team's largest party, else the first capable).
    let mut sides: [Side; 2] = Default::default();
    for t in TEAMS {
        let members = teams_users(t);
        let capable: Vec<String> = seats
            .iter()
            .enumerate()
            .filter(|(i, s)| s.team == t && caps[*i].drafts())
            .map(|(_, s)| s.user_id.clone())
            .collect();
        let mut parties: HashMap<&str, usize> = HashMap::new();
        for s in seats.iter().filter(|s| s.team == t) {
            if let Some(pid) = &s.party_id {
                *parties.entry(pid.as_str()).or_default() += 1;
            }
        }
        let mut by_size: Vec<(&str, usize)> = parties.into_iter().collect();
        by_size.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(b.0)));
        let leader = by_size
            .iter()
            .filter_map(|(pid, _)| core.live.parties.get(*pid).map(|p| p.leader.clone()))
            .find(|l| capable.contains(l));
        let captain = leader.or_else(|| capable.first().cloned());
        sides[ti(t)] = Side {
            members,
            capable,
            captain_pref: captain.clone(),
            captain,
        };
    }
    Some(Draft {
        version: VERSION,
        preset: p.id.clone(),
        title: p.label.clone(),
        summary: p.summary.clone(),
        decider: match p.decider.as_str() {
            "vote" => Decider::Vote,
            "any" => Decider::Any,
            _ => Decider::Captain,
        },
        tiebreak: p.tiebreak.clone(),
        timeout: p.timeout.clone(),
        turn_s: p.turn_s,
        state: DState::Waiting,
        first: Some(first),
        options,
        turns,
        current: None,
        sides,
        pairs: plan.pairs.clone(),
        next_order: 0,
        idle: Vec::new(),
        once: plan.once,
    })
}

// ---------------------------------------------------------------------------------------------- the service

/// Runs `f` on the draft of match `mid` with the service's clock, the players connected now and a secure draw.
fn with_draft<R>(core: &mut Core, mid: &str, f: impl FnOnce(&mut Draft, &mut Ctx) -> R) -> Option<R> {
    let users: Vec<String> = core
        .live
        .matches
        .get(mid)?
        .seats
        .iter()
        .filter_map(|s| s.user_id.clone())
        .collect();
    let online: HashSet<String> = users.into_iter().filter(|u| core.is_connected(u)).collect();
    let now = core.now;
    let d = core.live.matches.get_mut(mid)?.draft.as_mut()?;
    let mut rng = random_index;
    let mut cx = Ctx {
        now,
        online: &online,
        rng: &mut rng,
    };
    Some(f(d, &mut cx))
}

/// Starts the draft of a match whose players are all in (every one accepted, or a room start).
pub fn start(core: &mut Core, mid: &str) -> ApiResult<()> {
    let now = core.now;
    if let Some(m) = core.live.matches.get_mut(mid) {
        m.state = MState::Drafting;
        m.updated_at = now;
    }
    with_draft(core, mid, |d, cx| d.begin(cx));
    after(core, mid, None)
}

/// After a change: penalties for timed-out players, then either the end of the draft or a save and an event
/// (`only`: tell only this team, a vote's progress).
fn after(core: &mut Core, mid: &str, only: Option<Team>) -> ApiResult<()> {
    let Some(m) = core.live.matches.get_mut(mid) else {
        return Ok(());
    };
    m.updated_at = core.now;
    let queue = m.origin_kind == "queue";
    let (idle, done) = match m.draft.as_mut() {
        Some(d) => (std::mem::take(&mut d.idle), d.state == DState::Done),
        None => (Vec::new(), true),
    };
    if queue {
        for u in idle {
            crate::matches::apply_offence(core, &u, OFFENCE, Some(mid))?;
        }
    }
    if done {
        return complete(core, mid);
    }
    let m = core.live.matches[mid].clone();
    crate::matches::save(core, &m)?;
    match only.and_then(|t| m.draft.as_ref().map(|d| d.sides[ti(t)].members.clone())) {
        Some(team) => crate::matches::notify_users(core, mid, "match.updated", &team),
        None => crate::matches::notify(core, mid, "match.updated"),
    }
    Ok(())
}

/// The draft is over: its result goes into the rules (and the game's rules string), the match becomes `ready`.
fn complete(core: &mut Core, mid: &str) -> ApiResult<()> {
    let now = core.now;
    let policy = {
        let Some(m) = core.live.matches.get(mid) else {
            return Ok(());
        };
        crate::matches::policy_of(core, m)
    };
    let Some(m) = core.live.matches.get_mut(mid) else {
        return Ok(());
    };
    let mut order = None;
    if let Some(d) = &m.draft {
        if let Some(mode) = d.result_mode() {
            m.rules.mode = mode;
        }
        if let Some(stage) = d.result_stage() {
            m.rules.stage = Some(stage);
        }
        if d.once {
            order = Some(d.picked(Kind::Stage));
        }
    }
    let rules = crate::matches::rules_string(m);
    m.game.rules = rules;
    crate::matches::set_ready(m, &policy, now);
    let m = m.clone();
    crate::matches::save(core, &m)?;
    core.live.draft_tickets.remove(mid);
    crate::matches::notify(core, mid, "match.ready");
    core.emit("match.drafted", crate::matches::match_json(core, &m, None));
    for u in m.seats.iter().filter_map(|s| s.user_id.clone()) {
        core.refresh_presence(&u);
    }
    if let (Some(order), Some(r)) = (order, &m.game.series) {
        crate::series::on_draft_done(core, &r.id, order)?;
    }
    tracing::info!(match_id = %mid, stage = ?m.rules.stage, "draft done");
    Ok(())
}

/// The ticker: turns whose time ran out.
pub fn tick(core: &mut Core) {
    let ids: Vec<String> = core
        .live
        .matches
        .values()
        .filter(|m| m.state == MState::Drafting)
        .map(|m| m.id.clone())
        .collect();
    for id in ids {
        if with_draft(core, &id, |d, cx| d.tick(cx)) == Some(true)
            && let Err(e) = after(core, &id, None)
        {
            tracing::error!(match_id = %id, error = %e.message, "draft transition failed");
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActBody {
    turn: usize,
    #[serde(default)]
    choices: Vec<String>,
}

/// `POST /v1/matches/{match_id}/draft`: one player's ban, pick or vote on the current turn.
pub async fn post(
    State(st): State<AppState>,
    a: Authed,
    Path(mid): Path<String>,
    JsonBody(b): JsonBody<ActBody>,
) -> ApiResult<Json<Value>> {
    if !st.cfg().features.draft {
        return Err(ApiError::feature_disabled());
    }
    if let Err(retry) = st.limits.draft.take(&a.user, std::time::Instant::now()) {
        return Err(ApiError::rate_limited(retry));
    }
    let mut core = st.lock();
    act(&mut core, &mid, &a.user, b.turn, &b.choices)?;
    let m = crate::matches::load(&core, &mid)?.ok_or_else(|| ApiError::not_found("no such match"))?;
    Ok(Json(crate::matches::match_json(&core, &m, Some(&a.user))))
}

pub fn act(core: &mut Core, mid: &str, user: &str, turn: usize, ids: &[String]) -> ApiResult<()> {
    // 1.1.7: a spectator sees the draft read-only.
    if core
        .live
        .matches
        .get(mid)
        .is_some_and(|m| m.spectator_of(user).is_some())
    {
        return Err(ApiError::forbidden("not_your_turn", "spectators watch the draft").text(
            "Spectators watch the draft; only the teams choose.",
            "Les spectateurs regardent le choix ; seules les équipes choisissent.",
        ));
    }
    let m = match core.live.matches.get(mid) {
        Some(m) if m.seat_of(user).is_some() => m,
        _ => {
            // An ended match the player was in: its draft can no longer change.
            return match crate::matches::load(core, mid)? {
                Some(m) if m.seat_of(user).is_some() => Err(ApiError::conflict("match_state", "this match has ended")),
                _ => Err(ApiError::not_found("no such match")),
            };
        }
    };
    if m.draft.is_none() {
        return Err(ApiError::conflict("match_state", "this match has no draft"));
    }
    if ids.len() > 16 || ids.iter().any(|i| i.is_empty() || i.len() > 64) {
        return Err(ApiError::invalid(
            "/choices",
            "at most 16 option ids of 1 to 64 characters",
        ));
    }
    let team = m.draft.as_ref().and_then(|d| d.team_of(user));
    let r = with_draft(core, mid, |d, cx| d.act(user, turn, ids, cx))
        .ok_or_else(|| ApiError::not_found("no such match"))?;
    match r {
        Ok(Acted { changed: false, .. }) => Ok(()),
        Ok(Acted { public, .. }) => after(core, mid, if public { None } else { team }),
        Err(ActError::NotYourTurn(m)) => {
            let (en, fr) = not_your_turn_text(m);
            Err(ApiError::forbidden("not_your_turn", m).text(en, fr))
        }
        Err(ActError::Stale(m)) => Err(ApiError::conflict("draft_turn", m)),
        Err(ActError::Invalid(m)) => Err(ApiError::invalid("/choices", m)),
        Err(ActError::NotRunning) => Err(ApiError::conflict("match_state", "the draft is over")),
    }
}

/// The player's text (English, French) for a refused draft action, from the `NotYourTurn` message.
fn not_your_turn_text(m: &str) -> (&'static str, &'static str) {
    match m {
        "your team's captain chooses" => (
            "Your team's captain chooses.",
            "C'est le capitaine de votre équipe qui choisit.",
        ),
        "this turn is the other team's" => ("It is the other team's turn.", "C'est le tour de l'autre équipe."),
        _ => ("You cannot act on this turn.", "Vous ne pouvez pas agir sur ce tour."),
    }
}

/// A player abandons a match during its draft (`POST /v1/matches/{id}/decline`): the match is canceled
/// (`cancel_reason: left`), the player gets the `left` offense for a queue match, the others' tickets search again
/// with their original priority, a room opens again, a series replays the game.
pub fn abandon(core: &mut Core, mid: &str, user: &str) -> ApiResult<()> {
    let Some(mut m) = core.live.matches.remove(mid) else {
        return Ok(());
    };
    let now = core.now;
    m.state = MState::Cancelled;
    m.cancel_reason = Some("left".into());
    m.ended_at = Some(now);
    m.updated_at = now;
    if let Some(d) = m.draft.as_mut() {
        d.cancel(now);
    }
    crate::matches::save(core, &m)?;
    for u in m.users() {
        if core.live.user_match.get(&u) == Some(&m.id) {
            core.live.user_match.remove(&u);
        }
    }
    if m.origin_kind == "queue" {
        crate::matches::apply_offence(core, user, "left", Some(mid))?;
    }
    if let Some(tickets) = core.live.draft_tickets.remove(mid) {
        crate::api::queues::requeue(core, tickets, user);
    }
    crate::matches::notify_ended(core, &m, "match.cancelled");
    if m.origin_kind == "room" {
        crate::api::rooms::on_match_end(core, &m.origin_id);
    }
    for u in m.seats.iter().filter_map(|s| s.user_id.clone()) {
        core.refresh_presence(&u);
    }
    tracing::info!(match_id = %mid, "match abandoned during its draft");
    if m.game.series.is_some() {
        crate::series::on_game_cancelled(core, &m)?;
    }
    Ok(())
}

// ---------------------------------------------------------------------------------------------- declarations

/// The draft a queue's matches have, for the queue object (`draft.md`, section 2): `null` when none.
pub fn queue_member(cfg: &Config, id: Option<&str>) -> Value {
    match id.and_then(|i| cfg.draft(i)) {
        Some(p) => json!({
            "id": p.id, "label": p.label, "summary": p.summary, "required": p.required, "turn_s": p.turn_s,
            "pool": {"stages": p.stages, "modes": p.modes},
        }),
        None => Value::Null,
    }
}

/// Every problem with the drafts and their references.
pub fn validate(c: &Config, e: &mut Vec<String>) {
    let mut ids = HashSet::new();
    for d in &c.drafts {
        let n = format!("drafts.{}", d.id);
        if !ids.insert(d.id.as_str()) || !crate::util::is_id(&d.id) {
            e.push(format!("{n}: duplicate or bad id"));
        }
        let check = |turns: &[String], what: &str, e: &mut Vec<String>| {
            if turns.is_empty() || turns.len() > MAX_TURNS {
                e.push(format!("{n}.{what}: 1 to {MAX_TURNS} turns"));
            }
            for t in turns {
                if let Err(m) = parse_turn(t) {
                    e.push(format!("{n}.{what}: {m}"));
                }
            }
        };
        check(&d.turns, "turns", e);
        if let Some(f) = &d.first_game_turns {
            check(f, "first_game_turns", e);
        }

        if !matches!(
            d.first.as_str(),
            "random" | "higher_rated" | "lower_rated" | "loser" | "winner" | "alpha" | "bravo"
        ) {
            e.push(format!(
                "{n}.first: random, higher_rated, lower_rated, loser, winner, alpha or bravo"
            ));
        }
        if !matches!(d.decider.as_str(), "captain" | "vote" | "any") {
            e.push(format!("{n}.decider: captain, vote or any"));
        }
        if !matches!(d.tiebreak.as_str(), "captain" | "random") {
            e.push(format!("{n}.tiebreak: captain or random"));
        }
        if !matches!(d.timeout.as_str(), "random" | "skip") {
            e.push(format!("{n}.timeout: random or skip"));
        }
        if !matches!(d.series.as_str(), "per_game" | "once") {
            e.push(format!("{n}.series: per_game or once"));
        }
        if !(5..=600).contains(&d.turn_s) {
            e.push(format!("{n}.turn_s: 5 to 600"));
        }
        if d.stages.len() > MAX_OPTIONS || d.modes.len() > 32 {
            e.push(format!("{n}: at most {MAX_OPTIONS} stages and 32 modes"));
        }
        if d.stages
            .iter()
            .chain(d.modes.iter())
            .any(|s| s.is_empty() || s.len() > 64)
        {
            e.push(format!("{n}: stage and mode ids are 1 to 64 characters"));
        }
        if d.label.is_empty() {
            e.push(format!("{n}.label: a name (localized text)"));
        }
    }
    let known = |id: &str| c.drafts.iter().any(|d| d.id == id);
    let mut refs: Vec<(String, &str)> = Vec::new();
    for q in &c.queues {
        if let Some(d) = &q.draft {
            refs.push((format!("queues.{}.draft", q.id), d));
        }
    }
    for d in &c.rooms.drafts {
        refs.push(("rooms.drafts".into(), d));
    }
    for a in &c.activities {
        if let Some(d) = &a.draft {
            refs.push((format!("activities.{}.draft", a.id), d));
        }
    }
    for (what, id) in &refs {
        if !known(id) {
            e.push(format!("{what}: no [[drafts]] has the id `{id}`"));
        }
    }
    if !refs.is_empty() && !c.features.draft {
        e.push("drafts are named but features.draft is off".into());
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic draw: always the first index.
    fn first(_: usize) -> usize {
        0
    }

    fn opts(kind: Kind, ids: &[&str]) -> Vec<Opt> {
        ids.iter()
            .map(|id| Opt {
                id: id.to_string(),
                kind,
                state: OState::Available,
                by: None,
                turn: None,
                reason: None,
                order: None,
            })
            .collect()
    }

    fn draft(specs: &[&str], decider: Decider, stages: &[&str], alpha: &[&str], bravo: &[&str]) -> Draft {
        let specs: Vec<Spec> = specs.iter().map(|s| parse_turn(s).unwrap()).collect();
        let options = opts(Kind::Stage, stages);
        let counts: HashMap<Kind, usize> = [(Kind::Stage, stages.len()), (Kind::Mode, 0)].into();
        let side = |m: &[&str]| Side {
            members: m.iter().map(|s| s.to_string()).collect(),
            capable: m.iter().map(|s| s.to_string()).collect(),
            captain_pref: m.first().map(|s| s.to_string()),
            captain: m.first().map(|s| s.to_string()),
        };
        Draft {
            version: VERSION,
            preset: "t".into(),
            title: crate::config::en("Test"),
            summary: Text::new(),
            decider,
            tiebreak: "captain".into(),
            timeout: "random".into(),
            turn_s: 20,
            state: DState::Waiting,
            first: Some(Team::Alpha),
            options,
            turns: expand(&specs, Team::Alpha, None, &counts),
            current: None,
            sides: [side(alpha), side(bravo)],
            pairs: Vec::new(),
            next_order: 0,
            idle: Vec::new(),
            once: false,
        }
    }

    fn online(users: &[&str]) -> HashSet<String> {
        users.iter().map(|s| s.to_string()).collect()
    }

    fn ids(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn turn_tokens_parse() {
        assert_eq!(
            parse_turn("ban:first:1").unwrap(),
            Spec::Turn {
                action: Action::Ban,
                team: Some(TeamRef::First),
                count: 1,
                kind: Kind::Stage
            }
        );
        assert_eq!(
            parse_turn("pick:loser:2@mode").unwrap(),
            Spec::Turn {
                action: Action::Pick,
                team: Some(TeamRef::Loser),
                count: 2,
                kind: Kind::Mode
            }
        );
        assert_eq!(parse_turn("random").unwrap().kind(), Kind::Stage);
        assert!(matches!(parse_turn("random:3"), Ok(Spec::Turn { count: 3, .. })));
        assert_eq!(parse_turn("strike@mode").unwrap(), Spec::Strike { kind: Kind::Mode });
        for bad in [
            "ban",
            "ban:third:1",
            "pick:first:0",
            "pick:first:17",
            "random:x",
            "last:2",
            "ban:first:1@map",
            "veto",
        ] {
            assert!(parse_turn(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn ban_ban_random_with_captains() {
        let mut d = draft(
            &["ban:alpha:1", "ban:bravo:1", "random"],
            Decider::Captain,
            &["s1", "s2", "s3", "s4"],
            &["a1", "a2"],
            &["b1"],
        );
        let on = online(&["a1", "a2", "b1"]);
        let mut rng = first;
        let mut cx = Ctx {
            now: 100,
            online: &on,
            rng: &mut rng,
        };
        d.begin(&mut cx);
        assert_eq!(d.current, Some(0));
        assert_eq!(d.turns[0].deadline, Some(120));
        // Only alpha's captain acts on alpha's turn.
        assert_eq!(
            d.act("b1", 0, &ids(&["s1"]), &mut cx),
            Err(ActError::NotYourTurn("this turn is the other team's"))
        );
        assert_eq!(
            d.act("a2", 0, &ids(&["s1"]), &mut cx),
            Err(ActError::NotYourTurn("your team's captain chooses"))
        );
        assert!(matches!(
            d.act("a1", 0, &ids(&["s9"]), &mut cx),
            Err(ActError::Invalid(_))
        ));
        assert!(matches!(
            d.act("a1", 0, &ids(&["s1", "s2"]), &mut cx),
            Err(ActError::Invalid(_))
        ));
        assert_eq!(
            d.act("a1", 0, &ids(&["s1"]), &mut cx),
            Ok(Acted {
                changed: true,
                public: true
            })
        );
        // Idempotent repeat; another choice on a closed turn is stale.
        assert_eq!(
            d.act("a1", 0, &ids(&["s1"]), &mut cx),
            Ok(Acted {
                changed: false,
                public: false
            })
        );
        assert_eq!(
            d.act("a1", 0, &ids(&["s2"]), &mut cx),
            Err(ActError::Stale("this turn is not open"))
        );
        assert_eq!(d.current, Some(1));
        // s1 is banned: bravo cannot ban it again.
        assert!(matches!(
            d.act("b1", 1, &ids(&["s1"]), &mut cx),
            Err(ActError::Invalid(_))
        ));
        d.act("b1", 1, &ids(&["s4"]), &mut cx).unwrap();
        assert_eq!(d.state, DState::Done);
        assert_eq!(
            d.result_stage().as_deref(),
            Some("s2"),
            "the draw among s2 and s3 (first index)"
        );
        assert_eq!(d.turns[2].results[0].by, "server");
        let v = d.view(Some("a1"));
        assert_eq!(v["options"][0]["state"], "banned");
        assert_eq!(v["options"][0]["by"], "alpha");
        assert_eq!(v["result"]["stage"], "s2");
        assert_eq!(v["you"]["can_act"], false);
    }

    #[test]
    fn a_turn_that_runs_out_is_played_by_the_server_and_noted() {
        let mut d = draft(
            &["ban:first:1", "ban:second:1", "random"],
            Decider::Captain,
            &["s1", "s2", "s3"],
            &["a1"],
            &["b1"],
        );
        let on = online(&["a1", "b1"]);
        let mut rng = first;
        let mut cx = Ctx {
            now: 0,
            online: &on,
            rng: &mut rng,
        };
        d.begin(&mut cx);
        cx.now = 19;
        assert!(!d.tick(&mut cx), "not yet");
        cx.now = 20;
        assert!(d.tick(&mut cx));
        assert_eq!(d.turns[0].results[0].by, "timeout");
        assert_eq!(d.idle, ids(&["a1"]));
        assert_eq!(d.current, Some(1));
        assert_eq!(d.turns[1].deadline, Some(40));
        cx.now = 40;
        assert!(d.tick(&mut cx));
        assert_eq!(d.state, DState::Done, "the phase never waits forever");
        assert_eq!(d.picked(Kind::Stage).len(), 1);
        // `skip`: a timed-out ban is lost.
        let mut d = draft(
            &["ban:first:1", "random"],
            Decider::Captain,
            &["s1", "s2"],
            &["a1"],
            &["b1"],
        );
        d.timeout = "skip".into();
        cx.now = 0;
        d.begin(&mut cx);
        cx.now = 30;
        d.tick(&mut cx);
        assert!(d.turns[0].results[0].ids.is_empty());
        assert_eq!(d.options.iter().filter(|o| o.state == OState::Banned).count(), 0);
    }

    #[test]
    fn a_team_without_a_drafting_launcher_is_played_at_once() {
        let mut d = draft(
            &["ban:alpha:1", "ban:bravo:1", "random"],
            Decider::Captain,
            &["s1", "s2", "s3"],
            &["a1"],
            &["b1"],
        );
        d.sides[1].capable.clear();
        d.sides[1].captain_pref = None;
        d.sides[1].captain = None;
        let on = online(&["a1", "b1"]);
        let mut rng = first;
        let mut cx = Ctx {
            now: 0,
            online: &on,
            rng: &mut rng,
        };
        d.begin(&mut cx);
        d.act("a1", 0, &ids(&["s3"]), &mut cx).unwrap();
        assert_eq!(d.state, DState::Done, "bravo's turn was played by the server at once");
        assert_eq!(d.turns[1].results[0].by, "server");
        assert!(d.view(Some("b1"))["teams"]["bravo"]["auto"].as_bool().unwrap());
        // Nobody can draft: the whole draft is played at its start.
        let mut d = draft(
            &["ban:alpha:1", "ban:bravo:1", "random"],
            Decider::Vote,
            &["s1", "s2", "s3"],
            &["a1"],
            &["b1"],
        );
        for s in &mut d.sides {
            s.capable.clear();
        }
        d.begin(&mut cx);
        assert_eq!(d.state, DState::Done);
    }

    #[test]
    fn votes_follow_the_majority_ties_go_to_the_captain_and_stay_in_the_team() {
        let mut d = draft(
            &["ban:alpha:1", "random"],
            Decider::Vote,
            &["s1", "s2", "s3", "s4"],
            &["a1", "a2", "a3", "a4"],
            &["b1"],
        );
        let on = online(&["a1", "a2", "a3", "a4", "b1"]);
        let mut rng = first;
        let mut cx = Ctx {
            now: 0,
            online: &on,
            rng: &mut rng,
        };
        d.begin(&mut cx);
        assert_eq!(
            d.act("a2", 0, &ids(&["s2"]), &mut cx),
            Ok(Acted {
                changed: true,
                public: false
            })
        );
        assert_eq!(
            d.act("a2", 0, &ids(&["s2"]), &mut cx),
            Ok(Acted {
                changed: false,
                public: false
            }),
            "same vote"
        );
        // A changed vote is allowed while the team has not settled.
        assert!(d.act("a2", 0, &ids(&["s3"]), &mut cx).unwrap().changed);
        // The other team sees no vote.
        assert!(d.view(Some("b1"))["teams"]["alpha"].get("votes").is_none());
        assert_eq!(d.view(Some("a1"))["teams"]["alpha"]["votes"][0]["ids"], json!(["s3"]));
        assert_eq!(d.view(Some("a2"))["you"]["choice"], json!(["s3"]));
        d.act("a1", 0, &ids(&["s1"]), &mut cx).unwrap();
        d.act("a3", 0, &ids(&["s1"]), &mut cx).unwrap();
        assert_eq!(d.current, Some(0), "2 of 4 is no strict majority");
        d.act("a4", 0, &ids(&["s3"]), &mut cx).unwrap();
        // 2 - 2: the captain (a1) voted s1.
        assert_eq!(d.state, DState::Done);
        assert_eq!(d.turns[0].results[0].ids, ids(&["s1"]));
        assert_eq!(d.turns[0].results[0].by, "vote");
        // A strict majority settles before everyone voted.
        let mut d = draft(
            &["ban:alpha:1", "random"],
            Decider::Vote,
            &["s1", "s2", "s3"],
            &["a1", "a2", "a3"],
            &["b1"],
        );
        d.begin(&mut cx);
        d.act("a2", 0, &ids(&["s2"]), &mut cx).unwrap();
        assert_eq!(
            d.act("a3", 0, &ids(&["s2"]), &mut cx),
            Ok(Acted {
                changed: true,
                public: true
            })
        );
        assert_eq!(d.turns[0].results[0].ids, ids(&["s2"]));
        // At the deadline the votes cast decide; the players who did not vote are noted.
        let mut d = draft(
            &["ban:alpha:1", "random"],
            Decider::Vote,
            &["s1", "s2", "s3"],
            &["a1", "a2", "a3"],
            &["b1"],
        );
        cx.now = 0;
        d.begin(&mut cx);
        d.act("a3", 0, &ids(&["s3"]), &mut cx).unwrap();
        cx.now = 25;
        d.tick(&mut cx);
        assert_eq!(
            d.turns[0].results[0],
            Choice {
                team: Some(Team::Alpha),
                ids: ids(&["s3"]),
                by: "vote".into()
            }
        );
        assert_eq!(d.idle, ids(&["a1", "a2"]));
    }

    #[test]
    fn blind_bans_stay_hidden_until_both_teams_chose() {
        let mut d = draft(
            &["ban:both:1", "random"],
            Decider::Any,
            &["s1", "s2", "s3", "s4"],
            &["a1", "a2"],
            &["b1"],
        );
        let on = online(&["a1", "a2", "b1"]);
        let mut rng = first;
        let mut cx = Ctx {
            now: 0,
            online: &on,
            rng: &mut rng,
        };
        d.begin(&mut cx);
        d.act("a2", 0, &ids(&["s1"]), &mut cx).unwrap();
        let theirs = d.view(Some("b1"));
        assert_eq!(theirs["turns"][0]["acted"], json!(["alpha"]));
        assert_eq!(theirs["turns"][0]["choices"], json!([]), "nothing revealed yet");
        assert_eq!(theirs["options"][0]["state"], "available");
        assert_eq!(d.view(Some("a1"))["you"]["choice"], json!(["s1"]));
        assert_eq!(
            d.act("a1", 0, &ids(&["s2"]), &mut cx),
            Err(ActError::Stale("your team has chosen for this turn"))
        );
        // Both ban the same stage: banned once.
        d.act("b1", 0, &ids(&["s1"]), &mut cx).unwrap();
        assert_eq!(d.options.iter().filter(|o| o.state == OState::Banned).count(), 1);
        assert_eq!(d.state, DState::Done);
    }

    #[test]
    fn strike_alternates_until_one_is_left_and_never_bans_the_last() {
        let mut d = draft(
            &["strike"],
            Decider::Captain,
            &["s1", "s2", "s3", "s4", "s5"],
            &["a1"],
            &["b1"],
        );
        assert_eq!(d.turns.len(), 5);
        assert_eq!(d.turns[1].teams, vec![Team::Bravo]);
        assert_eq!(d.turns[4].action, Action::Last);
        let on = online(&["a1", "b1"]);
        let mut rng = first;
        let mut cx = Ctx {
            now: 0,
            online: &on,
            rng: &mut rng,
        };
        d.begin(&mut cx);
        for (turn, who, s) in [(0, "a1", "s1"), (1, "b1", "s2"), (2, "a1", "s3"), (3, "b1", "s5")] {
            d.act(who, turn, &ids(&[s]), &mut cx).unwrap();
        }
        assert_eq!(d.result_stage().as_deref(), Some("s4"));
        // Bans asked beyond what remains are cut: one option always stays.
        let mut d = draft(
            &["ban:alpha:3", "ban:bravo:3", "pick:alpha:1"],
            Decider::Captain,
            &["s1", "s2", "s3"],
            &["a1"],
            &["b1"],
        );
        d.begin(&mut cx);
        assert_eq!(d.turns[0].eff, 2);
        d.act("a1", 0, &ids(&["s1", "s2"]), &mut cx).unwrap();
        assert_eq!(d.turns[1].state, TState::Skipped, "nothing left to ban");
        assert_eq!(d.current, Some(2));
        d.act("a1", 2, &ids(&["s3"]), &mut cx).unwrap();
        assert_eq!(d.result_stage().as_deref(), Some("s3"));
    }

    #[test]
    fn the_loser_picks_and_series_orders_follow_the_picks() {
        let specs: Vec<Spec> = ["ban:winner:1", "pick:loser:1"]
            .iter()
            .map(|s| parse_turn(s).unwrap())
            .collect();
        let counts: HashMap<Kind, usize> = [(Kind::Stage, 3)].into();
        let t = expand(&specs, Team::Alpha, Some(Team::Bravo), &counts);
        assert_eq!(
            (t[0].teams.clone(), t[1].teams.clone()),
            (vec![Team::Bravo], vec![Team::Alpha])
        );
        // No previous game: the loser is `first`.
        let t = expand(&specs, Team::Bravo, None, &counts);
        assert_eq!(t[1].teams, vec![Team::Bravo]);
        let mut d = draft(
            &["pick:first:1", "pick:second:1", "last"],
            Decider::Captain,
            &["s1", "s2", "s3"],
            &["a1"],
            &["b1"],
        );
        d.once = true;
        let on = online(&["a1", "b1"]);
        let mut rng = first;
        let mut cx = Ctx {
            now: 0,
            online: &on,
            rng: &mut rng,
        };
        d.begin(&mut cx);
        d.act("a1", 0, &ids(&["s3"]), &mut cx).unwrap();
        d.act("b1", 1, &ids(&["s1"]), &mut cx).unwrap();
        assert_eq!(d.picked(Kind::Stage), ids(&["s3", "s1", "s2"]));
        assert_eq!(d.view(None)["result"]["stages"], json!(["s3", "s1", "s2"]));
        assert!(d.view(None).get("you").is_none(), "the public view has no player part");
    }

    #[test]
    fn a_mode_first_then_a_stage_of_that_mode() {
        let mut d = draft(
            &["ban:first:1@mode", "random@mode", "pick:second:1"],
            Decider::Captain,
            &["s1", "s2", "s3"],
            &["a1"],
            &["b1"],
        );
        d.options.extend(opts(Kind::Mode, &["m1", "m2"]));
        d.pairs = vec![
            ("m1".into(), "s1".into()),
            ("m2".into(), "s2".into()),
            ("m2".into(), "s3".into()),
        ];
        let on = online(&["a1", "b1"]);
        let mut rng = first;
        let mut cx = Ctx {
            now: 0,
            online: &on,
            rng: &mut rng,
        };
        d.begin(&mut cx);
        d.act("a1", 0, &ids(&["m1"]), &mut cx).unwrap();
        assert_eq!(d.result_mode().as_deref(), Some("m2"));
        let s1 = d.options.iter().find(|o| o.id == "s1").unwrap();
        assert_eq!(
            (s1.state, s1.reason.as_deref()),
            (OState::Unavailable, Some("combination"))
        );
        assert!(matches!(
            d.act("b1", 2, &ids(&["s1"]), &mut cx),
            Err(ActError::Invalid(_))
        ));
        d.act("b1", 2, &ids(&["s3"]), &mut cx).unwrap();
        assert_eq!(d.result_stage().as_deref(), Some("s3"));
    }

    #[test]
    fn an_offline_captain_hands_the_turn_to_a_connected_teammate() {
        let mut d = draft(
            &["ban:alpha:1", "random"],
            Decider::Captain,
            &["s1", "s2"],
            &["a1", "a2"],
            &["b1"],
        );
        let on = online(&["a2", "b1"]);
        let mut rng = first;
        let mut cx = Ctx {
            now: 0,
            online: &on,
            rng: &mut rng,
        };
        d.begin(&mut cx);
        assert_eq!(d.sides[0].captain.as_deref(), Some("a2"));
        assert_eq!(d.view(Some("a2"))["you"]["role"], "captain");
        assert!(d.act("a2", 0, &ids(&["s1"]), &mut cx).unwrap().changed);
    }

    #[test]
    fn every_draft_of_the_example_configuration_is_valid_uncommented() {
        let text = include_str!("../../../deploy/kalamar.example.toml");
        let start = text.find("# More formats").expect("the commented formats");
        let end = start + text[start..].find("# Protocol 1.1 badges").expect("their end");
        let mut uncommented = String::from(text);
        let mut extra = String::new();
        let mut presets = 0;
        for line in text[start..end].lines() {
            let l = line.strip_prefix("# ").unwrap_or("");
            let key = l.split(" = ").next().unwrap_or("");
            if l == "[[drafts]]" {
                presets += 1;
                extra.push_str("\n[[drafts]]\n");
            } else if !key.is_empty() && key.bytes().all(|b| b.is_ascii_lowercase() || b == b'_') && l.contains(" = ") {
                extra.push_str(l);
                extra.push('\n');
            }
        }
        assert!(presets >= 5, "{presets} commented formats");
        uncommented.push_str(&extra);
        let mut cfg: Config = toml::from_str(&uncommented).expect("the example with every format");
        cfg.normalize();
        assert_eq!(cfg.validate(), Vec::<String>::new());
        assert_eq!(cfg.drafts.len(), presets + 1);
        let veto = cfg.draft("veto").expect("the default draft");
        assert_eq!(
            (veto.turns.len(), veto.turn_s, veto.decider.as_str()),
            (3, 20, "captain")
        );
    }

    #[test]
    fn a_draft_survives_serialization() {
        let mut d = draft(
            &["ban:alpha:1", "ban:bravo:1", "random"],
            Decider::Vote,
            &["s1", "s2", "s3"],
            &["a1", "a2"],
            &["b1"],
        );
        let on = online(&["a1", "a2", "b1"]);
        let mut rng = first;
        let mut cx = Ctx {
            now: 0,
            online: &on,
            rng: &mut rng,
        };
        d.begin(&mut cx);
        d.act("a1", 0, &ids(&["s1"]), &mut cx).unwrap();
        let back: Draft = serde_json::from_str(&serde_json::to_string(&d).unwrap()).unwrap();
        assert_eq!(back.view(Some("a1")), d.view(Some("a1")));
        assert_eq!(back.turns[0].votes.len(), 1);
    }

    /// The French texts sent to players are proper UTF-8 ("\u{e9}quipe"), not UTF-8 read as Latin-1 and encoded again.
    #[test]
    fn not_your_turn_texts_are_not_mojibake() {
        assert_eq!(
            not_your_turn_text("your team's captain chooses").1,
            "C'est le capitaine de votre \u{e9}quipe qui choisit."
        );
        assert_eq!(
            not_your_turn_text("this turn is the other team's").1,
            "C'est le tour de l'autre \u{e9}quipe."
        );
        assert_eq!(
            not_your_turn_text("you are not a player of this match"),
            ("You cannot act on this turn.", "Vous ne pouvez pas agir sur ce tour.")
        );
    }

    /// No source file of the service holds UTF-8 text decoded as Latin-1 or Windows-1252 and encoded again: the
    /// sequences of e-acute, e-grave, a-grave, c-cedilla, the right single quote and the no-break space read that way.
    /// Written with escapes so that this test does not find itself.
    #[test]
    fn service_sources_hold_no_mojibake() {
        let bad = [
            "\u{c3}\u{a9}",
            "\u{c3}\u{a8}",
            "\u{c3}\u{a0}",
            "\u{c3}\u{a7}",
            "\u{e2}\u{20ac}\u{2122}",
            "\u{c2}\u{a0}",
        ];
        let mut stack = vec![std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))];
        let mut found = Vec::new();
        let mut files = 0;
        while let Some(dir) = stack.pop() {
            for e in std::fs::read_dir(&dir).unwrap() {
                let p = e.unwrap().path();
                if p.is_dir() {
                    if p.file_name().is_some_and(|n| n != "target") {
                        stack.push(p);
                    }
                } else if p.extension().is_some_and(|x| x == "rs" || x == "toml") {
                    files += 1;
                    let text = std::fs::read_to_string(&p).unwrap();
                    for (n, line) in text.lines().enumerate() {
                        if bad.iter().any(|b| line.contains(b)) {
                            found.push(format!("{}:{}", p.display(), n + 1));
                        }
                    }
                }
            }
        }
        assert!(files > 20, "{files} files read");
        assert!(found.is_empty(), "mojibake in {found:?}");
    }
}
