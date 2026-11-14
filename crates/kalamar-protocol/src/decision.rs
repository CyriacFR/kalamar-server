//! Result validation (checks V1 to V10) and the result decision procedure of `api.md`, "Result decision".
//!
//! 1.1.7 (`spectators.md`): a match may have spectators. Their result files (`"outcome": "spectated"`) are
//! **witness reports**, validated by the witness reading of the checks (V1 and V6 change, the rest is the same) and
//! never counted as a player's report; a policy's `witness` member says whether they break a disagreement or are
//! required for a rated result. A spectator's error, silence or absence never voids a match.
//!
//! 1.1.8 (`api.md`, "Result decision", step 2): reports agree when they agree on what **decides** the match (rule,
//! mode, stage, winner, deemed result, every player's team and `disconnected` flag, and weapons when the policy checks
//! loadouts). The **counters** (the teams' scores and each player's statistics) are informative: each game computes
//! its own, and they may differ slightly between machines. A policy's `stats` member says what they weigh: `flag`
//! (default) compares them with a wide tolerance and flags a larger gap without voiding, `ignore` does not compare
//! them, `strict` compares every shared field. Check V11 rejects a record of `rule` 0 whose declared loser clearly
//! leads both team scores. The record kept (and so the statistics recorded) is the agreeing report that differs least from the
//! others ([`Decision::stats`]).
//!
//! 1.1.12 (`api.md`, "Result decision", steps 5, 6 and 8): an error report never supports a result. It contradicts
//! the candidate (void) unless the candidate explains it ([`error_explained`]: a failure in the match, of a player the
//! record shows gone, and whose own team's game recorded it); an explained error is left out of the count. A match is
//! rated only when each team confirmed the record with its own valid result, or a counted referee's record equals it:
//! a silence, an error, or a spectator who is not a referee never confirm for a team. Up to 1.1.11 a forged record
//! that showed the opponent gone won a rated duel as soon as the opponent's game honestly reported a failure.
//!
//! Pure functions: no clock, no storage. The caller says when to decide (every report in, grace or deadline
//! passed) and applies the [`Decision`].

use serde::Serialize;
use serde_json::{Value, json};

use crate::policy::{
    Abort, Disagreement, Disconnect, Leave, LoadoutCheck, Policy, Silence, StatsCheck, WitnessFrom, WitnessUse,
};
use crate::{Role, Team};

/// Game error codes that can happen after the match started, and the launcher's equivalent
/// (`match-contract.md`, "The result file" and "Uploading").
pub const MID_MATCH_ERRORS: &[&str] = &[
    "match-aborted",
    "record-timeout",
    "connection-lost",
    "left-by-player",
    "launcher-crashed",
];

/// The error a player's own game or launcher reports when the player chose to leave (the game's *Leave match*, or the
/// game closed by the player during the match).
pub const LEFT_BY_PLAYER: &str = "left-by-player";

/// 1.1.11 (`leaving.md`): whether a report proves that its own player left the match while it was being played: the
/// error `left-by-player` with `"in_play": true`, which the player's game sets when it heard every other player's
/// game until the departure (the session alive: the match was not already lost). A departure without it (a launcher
/// or a game that does not write `in_play`, a departure before the match or after the session stopped) proves
/// nothing: the match is decided as without a voluntary departure.
pub fn left_in_play(report: &Value) -> bool {
    report.get("error").and_then(Value::as_str) == Some(LEFT_BY_PLAYER)
        && report.get("in_play").and_then(Value::as_bool) == Some(true)
}

/// What one participant of the match is.
#[derive(Debug, Clone)]
pub struct Seat {
    pub member_name: String,
    pub team: Team,
    pub role: Role,
    /// The launcher reported its launch.
    pub launched: bool,
    /// A mirrored status reached `in-match` (or a later state).
    pub saw_in_match: bool,
    /// The terminal report received, verbatim.
    pub report: Option<Value>,
    /// 1.1.11: when the server received the report (seconds on any clock that only goes forward). A departure is a
    /// forfeit only when no other player's report came before it ([`departure_forfeit`]).
    pub reported_at: Option<i64>,
}

/// 1.1.7: a spectator of the match (a member of the game session seated in the stands). Its report is a witness's.
#[derive(Debug, Clone)]
pub struct Witness {
    pub member_name: String,
    /// Its part in the game session: a spectator may host it.
    pub role: Role,
    /// One of the operator's referees (its report counts under `witness_from: referees`).
    pub referee: bool,
    /// The launcher reported its launch (else the spectator is absent).
    pub launched: bool,
    /// The terminal report received, verbatim.
    pub report: Option<Value>,
}

/// The match as the server set it.
#[derive(Debug, Clone, Default)]
pub struct MatchSpec {
    pub game_match_id: String,
    pub mode: String,
    pub stage: Option<String>,
    pub rated: bool,
    /// The players (the participants), in order.
    pub seats: Vec<Seat>,
    /// 1.1.1: the match rules' `loadout` (checked when the policy's `loadout_check` asks).
    pub loadout: Option<Value>,
    /// 1.1.7: the spectators, in order (never in the record, never counted as players).
    pub witnesses: Vec<Witness>,
}

/// One team line of a record (shared field).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct TeamLine {
    pub team: String,
    pub paint_points: i64,
    pub turf_permille: i64,
}

/// One player line of a record (shared field).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlayerLine {
    pub name: String,
    pub team: String,
    pub paint: i64,
    pub kills: i64,
    pub assists: i64,
    pub deaths: i64,
    pub specials: i64,
    pub disconnected: bool,
    /// Contract 2: the player's weapon id as text (shared); `None` in version 1 files.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub weapon: Option<String>,
}

/// The shared fields of a result file: what decides the match and the counters. Up to 1.1.7 every one had to be equal
/// between reports (policy `stats: strict` still asks it); 1.1.8 compares the counters with a tolerance
/// ([`same_outcome`], [`StatsReport`]): each game computes its own area share.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Shared {
    pub rule: i64,
    pub mode: String,
    pub stage_id: i64,
    pub stage_scene: String,
    pub win_team: i64,
    pub winner: String,
    pub deemed_result: i64,
    pub teams: Vec<TeamLine>,
    pub players: Vec<PlayerLine>,
}

/// A validated record: the shared fields plus the informative ones.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Record {
    #[serde(flatten)]
    pub shared: Shared,
    /// 1.1.2: informative, not compared: the engines of one match count a few frames apart in most modes.
    pub battle_frames: i64,
    pub duration_s: f64,
    pub end_utc: Option<i64>,
    /// Contract 2: each player's gear ids `[head, clothes, shoes]` as text, from this report (not shared).
    #[serde(skip)]
    pub gear: Vec<Option<Vec<String>>>,
}

impl Record {
    /// The team the record says won, `None` for no winner.
    pub fn winner(&self) -> Option<Team> {
        match self.shared.win_team {
            0 => Some(Team::Alpha),
            1 => Some(Team::Bravo),
            _ => None,
        }
    }
    pub fn player(&self, member_name: &str) -> Option<&PlayerLine> {
        self.shared.players.iter().find(|p| p.name == member_name)
    }
    /// The record with only the alpha and bravo teams (api.md, "Outcome object").
    pub fn public_json(&self) -> Value {
        let mut v = serde_json::to_value(self).unwrap_or(Value::Null);
        if let Some(teams) = v.get_mut("teams").and_then(Value::as_array_mut) {
            teams.retain(|t| matches!(t.get("team").and_then(Value::as_str), Some("alpha" | "bravo")));
        }
        v
    }
}

/// How a terminal report reads.
#[derive(Debug, Clone, PartialEq)]
pub enum Classified {
    Result(Box<Record>),
    Error(String),
    Invalid(String),
}

fn int(v: &Value, key: &str, lo: i64, hi: i64) -> Result<i64, String> {
    let n = v
        .get(key)
        .and_then(Value::as_i64)
        .ok_or_else(|| format!("`{key}` is not an integer"))?;
    if (lo..=hi).contains(&n) {
        Ok(n)
    } else {
        Err(format!("`{key}` {n} out of range {lo}..{hi}"))
    }
}

fn string<'a>(v: &'a Value, key: &str) -> Result<&'a str, String> {
    v.get(key)
        .and_then(Value::as_str)
        .ok_or_else(|| format!("`{key}` is not a string"))
}

/// The error a report carries, when it is an error report (`None` when it is not one); `Err` when its `error` member
/// is malformed.
fn error_of(report: &Value) -> Option<Result<String, String>> {
    // Contract 2: a version 2 file whose outcome is `aborted` reports the error `match-aborted`.
    if report.get("version").and_then(Value::as_i64) == Some(2)
        && report.get("outcome").and_then(Value::as_str) == Some("aborted")
        && report.get("error").is_none()
    {
        return Some(Ok("match-aborted".into()));
    }
    let e = report.get("error")?;
    Some(match e.as_str() {
        Some(code) if (1..=64).contains(&code.chars().count()) && report.is_object() => Ok(code.to_string()),
        _ => Err("`error` is not a string of 1 to 64 characters".into()),
    })
}

/// Reads a report and checks it against the match for the seat `uploader` (checks V1 to V10). An object with a
/// string `error` of 1 to 64 characters is an error report.
pub fn classify(report: &Value, spec: &MatchSpec, uploader: usize) -> Classified {
    match error_of(report) {
        Some(Ok(code)) => return Classified::Error(code),
        Some(Err(why)) => return Classified::Invalid(why),
        None => {}
    }
    match validate(report, spec, uploader) {
        Ok(r) => Classified::Result(Box::new(r)),
        Err(why) => Classified::Invalid(why),
    }
}

/// 1.1.7: reads the report of the spectator `witness` (the witness reading of the checks, [`validate_witness`]).
pub fn classify_witness(report: &Value, spec: &MatchSpec, witness: usize) -> Classified {
    match error_of(report) {
        Some(Ok(code)) => return Classified::Error(code),
        Some(Err(why)) => return Classified::Invalid(why),
        None => {}
    }
    match validate_witness(report, spec, witness) {
        Ok(r) => Classified::Result(Box::new(r)),
        Err(why) => Classified::Invalid(why),
    }
}

/// Who uploaded the report being checked.
enum Uploader<'a> {
    /// A player (a participant): its member name.
    Player(&'a str),
    /// 1.1.7: a spectator.
    Witness,
}

/// Checks V1 to V10 of api.md, "Result decision", for the player `uploader`.
pub fn validate(v: &Value, spec: &MatchSpec, uploader: usize) -> Result<Record, String> {
    let seat = spec.seats.get(uploader).ok_or("unknown uploader")?;
    check(v, spec, seat.role, Uploader::Player(&seat.member_name))
}

/// 1.1.7: checks the report of the spectator `witness` with the witness reading of V1 to V10: V1 needs a version 2
/// file with `"outcome": "spectated"` (and `seat`, when present, `spectator`); V6 needs **no** player entry with
/// `self: true` (a spectator is not in the record); V2 to V5 and V7 to V10 are the players' checks (V3 and V10 with
/// the spectator's own session role: a spectator may host).
pub fn validate_witness(v: &Value, spec: &MatchSpec, witness: usize) -> Result<Record, String> {
    let w = spec.witnesses.get(witness).ok_or("unknown spectator")?;
    check(v, spec, w.role, Uploader::Witness)
}

fn check(v: &Value, spec: &MatchSpec, role: Role, who: Uploader) -> Result<Record, String> {
    let seat_of_file = v.get("seat").and_then(Value::as_str);
    match who {
        // V1 (1.1: version 2 files of contract 2 too, with outcome `completed`; 1.1.7: and, when the file names its
        // seat, `player`: a player's game that seated it in the stands did not play the match).
        Uploader::Player(_) => {
            match v.get("version").and_then(Value::as_i64) {
                Some(1) if v.is_object() => {}
                Some(2) if v.get("outcome").and_then(Value::as_str) == Some("completed") => {}
                _ => return Err("V1: not a version 1 result, nor a completed version 2 result".into()),
            }
            if seat_of_file.is_some_and(|s| s != "player") {
                return Err("V1: the game did not seat this player as a player".into());
            }
        }
        // V1, witness reading: a spectator's file says `spectated`, never `completed`.
        Uploader::Witness => {
            let spectated = v.get("version").and_then(Value::as_i64) == Some(2)
                && v.get("outcome").and_then(Value::as_str) == Some("spectated");
            if !spectated || seat_of_file.is_some_and(|s| s != "spectator") {
                return Err("V1: a spectator's report must be a version 2 file with outcome spectated".into());
            }
        }
    }
    // V10 (1.1): the host's room did not take the rules.
    if role == Role::Host && v.get("settings_ok").and_then(Value::as_bool) == Some(false) {
        return Err("V10: the host's room did not take the requested settings".into());
    }
    // V2
    if string(v, "match_id")? != spec.game_match_id {
        return Err("V2: match_id is not this match's".into());
    }
    // V3
    if Role::parse(string(v, "role")?) != Some(role) {
        return Err("V3: role is not the uploader's".into());
    }
    // V7
    let mode = string(v, "mode")?;
    if mode != spec.mode {
        return Err(format!("V7: mode `{}` is not the match's", shown(mode)));
    }
    let stage_scene = string(v, "stage_scene")?.to_string();
    if spec.stage.as_ref().is_some_and(|stage| &stage_scene != stage) {
        return Err("V7: stage is not the match's".into());
    }
    // V8 and V9 on the record fields.
    let rule = int(v, "rule", i64::from(i32::MIN), i64::from(i32::MAX))?;
    let stage_id = int(v, "stage_id", i64::from(i32::MIN), i64::from(i32::MAX))?;
    let battle_frames = int(v, "battle_frames", 1, 216_000)?;
    let deemed_result = int(v, "deemed_result", i64::from(i32::MIN), i64::from(i32::MAX))?;
    let win_team = int(v, "win_team", -1, 1)?;
    let winner = string(v, "winner")?.to_string();
    let expected_winner = match win_team {
        0 => "alpha",
        1 => "bravo",
        _ => "none",
    };
    if winner != expected_winner {
        return Err("V8: winner does not match win_team".into());
    }
    let teams_v = v
        .get("teams")
        .and_then(Value::as_array)
        .ok_or("`teams` is not an array")?;
    if teams_v.len() > 8 {
        return Err("V9: too many teams".into());
    }
    let mut teams = Vec::with_capacity(teams_v.len());
    for t in teams_v {
        teams.push(TeamLine {
            team: string(t, "team")?.to_string(),
            paint_points: int(t, "paint_points", 0, 999_999)?,
            turf_permille: int(t, "turf_permille", 0, 1000)?,
        });
    }
    for name in ["alpha", "bravo"] {
        if !teams.iter().any(|t| t.team == name) {
            return Err(format!("V8: no `{name}` team"));
        }
    }
    let players_v = v
        .get("players")
        .and_then(Value::as_array)
        .ok_or("`players` is not an array")?;
    // V4
    if players_v.len() != spec.seats.len() {
        return Err("V4: not one player per participant".into());
    }
    let mut players = Vec::with_capacity(players_v.len());
    let mut gear = Vec::with_capacity(players_v.len());
    let mut selfs = Vec::new();
    for p in players_v {
        let line = PlayerLine {
            name: string(p, "name")?.to_string(),
            team: string(p, "team")?.to_string(),
            paint: int(p, "paint", 0, 999_999)?,
            kills: int(p, "kills", 0, 9999)?,
            assists: int(p, "assists", 0, 9999)?,
            deaths: int(p, "deaths", 0, 9999)?,
            specials: int(p, "specials", 0, 9999)?,
            disconnected: p
                .get("disconnected")
                .and_then(Value::as_bool)
                .ok_or("`disconnected` is not a boolean")?,
            weapon: p.get("weapon").and_then(id_text),
        };
        gear.push(
            p.get("gear")
                .and_then(Value::as_array)
                .map(|g| g.iter().filter_map(id_text).collect()),
        );
        if p.get("self").and_then(Value::as_bool) == Some(true) {
            selfs.push(line.name.clone());
        }
        players.push(line);
    }
    for seat in &spec.seats {
        let found: Vec<&PlayerLine> = players.iter().filter(|p| p.name == seat.member_name).collect();
        if found.len() != 1 {
            return Err(format!(
                "V4: member `{}` is not in the record exactly once",
                seat.member_name
            ));
        }
        // V5
        if found[0].team != seat.team.as_str() {
            return Err(format!("V5: member `{}` is not in its team", seat.member_name));
        }
    }
    // V6; witness reading: no entry is the uploader's (spectators are never in the record).
    match who {
        Uploader::Player(me) => {
            if selfs.len() != 1 || selfs[0] != me {
                return Err("V6: the record's own player is not the uploader".into());
            }
        }
        Uploader::Witness => {
            if !selfs.is_empty() {
                return Err("V6: a spectator's record has a player of its own".into());
            }
        }
    }
    // V11 (1.1.8): a record of `rule` 0 does not contradict its own winner.
    check_winner_scores(rule, win_team, deemed_result, &teams)?;
    let duration_s = v
        .get("duration_s")
        .and_then(Value::as_f64)
        .unwrap_or(battle_frames as f64 / 60.0);
    let end_utc = v.get("end_utc").and_then(Value::as_i64);
    Ok(Record {
        shared: Shared {
            rule,
            mode: mode.to_string(),
            stage_id,
            stage_scene,
            win_team,
            winner,
            deemed_result,
            teams,
            players,
        },
        battle_frames,
        duration_s,
        end_utc,
        gear,
    })
}

/// Security review L-S3: a text of a report quoted in a reason (which the other participants see and the server logs):
/// at most 32 characters, every control or format character (line breaks, bidirectional overrides) replaced by `?`.
fn shown(text: &str) -> String {
    let mut out: String = text
        .chars()
        .take(32)
        .map(|c| if c.is_control() || is_format(c) { '?' } else { c })
        .collect();
    if text.chars().nth(32).is_some() {
        out.push_str("...");
    }
    out
}

/// Unicode format characters that reorder or hide text (bidirectional controls, zero-width characters).
fn is_format(c: char) -> bool {
    matches!(c, '\u{200B}'..='\u{200F}' | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{2069}' | '\u{FEFF}')
}

/// The game's rule id whose winner the teams' area share decides (`rule` in result files).
pub const RULE_AREA_SHARE: i64 = 0;

/// 1.1.8, check V11: how far, in `turf_permille`, the declared loser of a `rule` 0 match may lead the winner before
/// the record contradicts itself. Each game computes its own share while the winner is decided once for all, so in a
/// close match a guest's own share may show the loser slightly ahead; the margin is wide on purpose.
pub const V11_SHARE_MARGIN: i64 = 50;

/// Check V11 (1.1.8): under `rule` 0 with a winner and no deemed result (`deemed_result` -1: the match went to
/// its end), a record whose declared loser leads the winner by more than [`V11_SHARE_MARGIN`] `turf_permille` **and**
/// on `paint_points` contradicts itself (a winner flipped by hand). Other rules end on counts the record does not
/// reliably carry, and a deemed result was decided otherwise: nothing is checked there.
fn check_winner_scores(rule: i64, win_team: i64, deemed_result: i64, teams: &[TeamLine]) -> Result<(), String> {
    if rule != RULE_AREA_SHARE || deemed_result != -1 || !(0..=1).contains(&win_team) {
        return Ok(());
    }
    let line = |name: &str| teams.iter().find(|t| t.team == name);
    let (winner, loser) = if win_team == 0 {
        ("alpha", "bravo")
    } else {
        ("bravo", "alpha")
    };
    let (Some(w), Some(l)) = (line(winner), line(loser)) else {
        return Ok(());
    };
    if l.turf_permille > w.turf_permille + V11_SHARE_MARGIN && l.paint_points > w.paint_points {
        return Err(format!(
            "V11: {winner} is declared the winner while {loser} leads on turf_permille ({} against {}) and paint_points ({} against {})",
            l.turf_permille, w.turf_permille, l.paint_points, w.paint_points
        ));
    }
    Ok(())
}

/// A game identifier (string or number) as text.
fn id_text(v: &Value) -> Option<String> {
    match v {
        Value::String(s) if s.len() <= 64 => Some(s.clone()),
        Value::Number(n) => Some(n.to_string()),
        _ => None,
    }
}

fn walk<'a>(base: &'a Value, slot: &[&str]) -> Option<&'a Value> {
    let mut v = base;
    for s in slot {
        v = v.get(*s)?;
    }
    Some(v)
}

/// Whether `id` satisfies a constraint object (`fixed`, `allowed` or `denied`).
fn satisfies(c: &Value, id: &str) -> bool {
    let list = |k: &str| {
        c.get(k)
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(id_text).any(|x| x == id))
    };
    if let Some(f) = c.get("fixed").and_then(id_text) {
        return f == id;
    }
    if let Some(found) = list("allowed") {
        return found;
    }
    if let Some(found) = list("denied") {
        return !found;
    }
    true
}

/// Seats whose recorded loadout breaks `loadout` (match-contract-2.md, section 5). Unknown items are not violations.
pub fn loadout_violations(loadout: &Value, spec: &MatchSpec, record: &Record) -> Vec<usize> {
    let mut out = Vec::new();
    for (i, seat) in spec.seats.iter().enumerate() {
        let Some(k) = record.shared.players.iter().position(|p| p.name == seat.member_name) else {
            continue;
        };
        let pick = |slot: &[&str]| -> Option<&Value> {
            loadout
                .get("players")
                .and_then(|p| p.get(&seat.member_name))
                .and_then(|b| walk(b, slot))
                .or_else(|| {
                    loadout
                        .get("teams")
                        .and_then(|t| t.get(seat.team.as_str()))
                        .and_then(|b| walk(b, slot))
                })
                .or_else(|| loadout.get("all").and_then(|b| walk(b, slot)))
        };
        let mut bad = false;
        if let (Some(c), Some(w)) = (pick(&["weapon"]), record.shared.players[k].weapon.as_deref()) {
            bad |= !satisfies(c, w);
        }
        if let Some(Some(g)) = record.gear.get(k) {
            for (j, slot) in ["head", "clothes", "shoes"].iter().enumerate() {
                if let (Some(c), Some(id)) = (pick(&["gear", slot]), g.get(j)) {
                    bad |= !satisfies(c, id);
                }
            }
        }
        if bad {
            out.push(i);
        }
    }
    out
}

/// What decides the match in a record (1.1.8, step 2): equal in every report that agrees. Players are compared by
/// name (their order in the record decides nothing); a weapon only when the policy checks loadouts.
#[derive(PartialEq)]
struct Decisive<'a> {
    rule: i64,
    mode: &'a str,
    stage_id: i64,
    stage_scene: &'a str,
    win_team: i64,
    winner: &'a str,
    deemed_result: i64,
    players: Vec<(&'a str, &'a str, bool, Option<&'a str>)>,
}

fn decisive<'a>(r: &'a Record, weapons: bool) -> Decisive<'a> {
    let s = &r.shared;
    let mut players: Vec<_> = s
        .players
        .iter()
        .map(|p| {
            (
                p.name.as_str(),
                p.team.as_str(),
                p.disconnected,
                weapons.then_some(p.weapon.as_deref()).flatten(),
            )
        })
        .collect();
    players.sort_unstable();
    Decisive {
        rule: s.rule,
        mode: &s.mode,
        stage_id: s.stage_id,
        stage_scene: &s.stage_scene,
        win_team: s.win_team,
        winner: &s.winner,
        deemed_result: s.deemed_result,
        players,
    }
}

/// Whether the weapons decide (the policy checks the match's loadout against the record).
fn weapons_decide(policy: &Policy, spec: &MatchSpec) -> bool {
    policy.loadout_check != LoadoutCheck::Ignore && spec.loadout.is_some()
}

/// Whether two valid records report the same match under `policy` (step 2, grouping; also how a witness's record is
/// compared). `stats: strict`: every shared field equal (1.1.7). Otherwise (1.1.8): rule, mode, stage (`stage_id`,
/// `stage_scene`), `win_team`, `winner`, `deemed_result`, and every player's name, team and `disconnected` flag (step 5
/// and `disconnect: forfeit` read it), plus each player's weapon when the policy checks loadouts. `match_id` and the
/// players' names and teams are already the match's in every valid record (V2, V4, V5).
pub fn same_outcome(policy: &Policy, spec: &MatchSpec, a: &Record, b: &Record) -> bool {
    if policy.stats == StatsCheck::Strict {
        return a.shared == b.shared;
    }
    let w = weapons_decide(policy, spec);
    decisive(a, w) == decisive(b, w)
}

/// One counter of a record: `(team, seat, field, value)` (a team's score has a team, a player's counter a seat).
type Counter = (Option<Team>, Option<usize>, &'static str, i64);

/// The counters of a record (1.1.8: informative), in a fixed order: alpha's then bravo's scores, then every
/// participant's counters in seat order.
fn counters(spec: &MatchSpec, r: &Record) -> Vec<Counter> {
    let mut out = Vec::new();
    for team in [Team::Alpha, Team::Bravo] {
        if let Some(t) = r.shared.teams.iter().find(|t| t.team == team.as_str()) {
            out.push((Some(team), None, "paint_points", t.paint_points));
            out.push((Some(team), None, "turf_permille", t.turf_permille));
        }
    }
    for (i, seat) in spec.seats.iter().enumerate() {
        if let Some(p) = r.player(&seat.member_name) {
            for (field, v) in [
                ("paint", p.paint),
                ("kills", p.kills),
                ("assists", p.assists),
                ("deaths", p.deaths),
                ("specials", p.specials),
            ] {
                out.push((None, Some(i), field, v));
            }
        }
    }
    out
}

/// Every participant's weapon in a record, in seat order.
fn weapons<'a>(spec: &MatchSpec, r: &'a Record) -> Vec<Option<&'a str>> {
    spec.seats
        .iter()
        .map(|s| r.player(&s.member_name).and_then(|p| p.weapon.as_deref()))
        .collect()
}

/// How many counters and weapons differ between two records (the distance that picks the record kept).
fn distance(spec: &MatchSpec, a: &Record, b: &Record) -> usize {
    let n = counters(spec, a)
        .iter()
        .zip(counters(spec, b))
        .filter(|(x, y)| x.3 != y.3)
        .count();
    n + weapons(spec, a)
        .iter()
        .zip(weapons(spec, b))
        .filter(|(x, y)| **x != *y)
        .count()
}

/// 1.1.8: the tolerance of a counter, `(floor, percent)`: a gap between two reports that agree on the outcome is
/// flagged when it exceeds both `floor` and `percent` % of the larger value. Each game computes its own counters,
/// which may differ slightly between the games of one match. The floors and percentages are wide on purpose: a flag means a report was edited or a game went badly out of step, not a game's own rounding.
pub fn tolerance(field: &str) -> (i64, i64) {
    match field {
        "turf_permille" => (30, 25),
        "paint_points" | "paint" => (100, 25),
        // kills, assists, deaths, specials: small numbers.
        _ => (2, 50),
    }
}

/// Whether the gap between `low` and `high` of `field` is beyond its [`tolerance`].
pub fn beyond_tolerance(field: &str, low: i64, high: i64) -> bool {
    let (floor, percent) = tolerance(field);
    let gap = high - low;
    gap > floor && gap * 100 > percent * high.abs().max(low.abs())
}

/// 1.1.8: one counter whose values differ beyond the tolerance between reports that agree on the outcome.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Gap {
    /// `paint_points`, `turf_permille` (a team's), or `paint`, `kills`, `assists`, `deaths`, `specials`, `weapon` (a
    /// player's).
    pub field: &'static str,
    /// The team, for a team's score.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub team: Option<Team>,
    /// The participant (index in `spec.seats`), for a player's counter.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub seat: Option<usize>,
    /// The smallest and the largest value reported (two different ids for a weapon).
    pub values: [Value; 2],
}

/// The most gaps a decision lists (the flag is set whatever their number).
pub const MAX_GAPS: usize = 16;

/// 1.1.8: how the counters of the reports that agree on the accepted outcome compare, and whose report is the record
/// (`outcome.stats_check`).
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StatsReport {
    /// The policy's `stats`.
    pub check: StatsCheck,
    /// A counter or a weapon differs between the agreeing reports, by any amount.
    pub differ: bool,
    /// With `stats: flag`, a gap beyond the tolerance: reported and logged, it never voids a match.
    pub flagged: bool,
    /// The gaps beyond the tolerance (`stats: flag`), at most [`MAX_GAPS`].
    pub gaps: Vec<Gap>,
    /// The participant whose report is the accepted record, its counters the ones recorded: among the reports that
    /// agree, the one whose counters differ least from the others' (and from the counted witnesses' that agree),
    /// ties going to the host's, then to seat order.
    pub record_seat: Option<usize>,
}

impl StatsReport {
    fn none(policy: &Policy) -> StatsReport {
        StatsReport {
            check: policy.stats,
            differ: false,
            flagged: false,
            gaps: Vec::new(),
            record_seat: None,
        }
    }
}

/// Picks the record among the candidate's reports `group` (seat, record) and compares their counters: see
/// [`StatsReport`]. `witnesses`: the counted witnesses' records that agree with the candidate.
fn keep_record(
    policy: &Policy,
    spec: &MatchSpec,
    group: &[(usize, &Record)],
    witnesses: &[&Record],
) -> (usize, StatsReport) {
    let refs: Vec<&Record> = group.iter().map(|(_, r)| *r).chain(witnesses.iter().copied()).collect();
    let k = (0..group.len())
        .min_by_key(|&k| {
            let (seat, r) = group[k];
            let d: usize = refs.iter().map(|o| distance(spec, r, o)).sum();
            (d, spec.seats[seat].role != Role::Host, seat)
        })
        .unwrap_or(0);
    let mut report = StatsReport::none(policy);
    report.record_seat = Some(group[k].0);
    let all: Vec<Vec<Counter>> = group.iter().map(|(_, r)| counters(spec, r)).collect();
    for (j, &(team, seat, field, _)) in all[0].iter().enumerate() {
        let values = all.iter().filter_map(|c| c.get(j).map(|x| x.3));
        let (low, high) = values.fold((i64::MAX, i64::MIN), |(l, h), v| (l.min(v), h.max(v)));
        if low != high {
            report.differ = true;
            if policy.stats == StatsCheck::Flag && beyond_tolerance(field, low, high) {
                report.flagged = true;
                if report.gaps.len() < MAX_GAPS {
                    report.gaps.push(Gap {
                        field,
                        team,
                        seat,
                        values: [json!(low), json!(high)],
                    });
                }
            }
        }
    }
    let ws: Vec<Vec<Option<&str>>> = group.iter().map(|(_, r)| weapons(spec, r)).collect();
    for seat in 0..spec.seats.len() {
        let first = ws[0][seat];
        if let Some(other) = ws.iter().map(|w| w[seat]).find(|w| *w != first) {
            report.differ = true;
            if policy.stats == StatsCheck::Flag {
                report.flagged = true;
                if report.gaps.len() < MAX_GAPS {
                    report.gaps.push(Gap {
                        field: "weapon",
                        team: None,
                        seat: Some(seat),
                        values: [json!(first), json!(other)],
                    });
                }
            }
        }
    }
    (k, report)
}

/// A participant's verdict (api.md, "Outcome object").
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Agreed,
    Dissent,
    Invalid,
    Error,
    Silent,
    NoShow,
}

impl Verdict {
    pub fn as_str(self) -> &'static str {
        match self {
            Verdict::Agreed => "agreed",
            Verdict::Dissent => "dissent",
            Verdict::Invalid => "invalid",
            Verdict::Error => "error",
            Verdict::Silent => "silent",
            Verdict::NoShow => "no_show",
        }
    }
}

/// 1.1.7: what a spectator's report did (`outcome.spectators[].verdict`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WitnessVerdict {
    /// A valid witness report equal to the accepted record.
    Agreed,
    /// A valid witness report that differs from the accepted record.
    Dissent,
    /// A valid witness report of a match that no result counts for (nothing to compare with).
    Reported,
    /// The report failed the witness reading of the checks.
    Invalid,
    /// The spectator's game or launcher reported an error (never held against the match or the spectator).
    Error,
    /// Launched, no report.
    Silent,
    /// Never launched.
    Absent,
}

impl WitnessVerdict {
    pub fn as_str(self) -> &'static str {
        match self {
            WitnessVerdict::Agreed => "agreed",
            WitnessVerdict::Dissent => "dissent",
            WitnessVerdict::Reported => "reported",
            WitnessVerdict::Invalid => "invalid",
            WitnessVerdict::Error => "error",
            WitnessVerdict::Silent => "silent",
            WitnessVerdict::Absent => "absent",
        }
    }
}

/// The decision on a match.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct Decision {
    /// `true`: `completed` (a result counts); `false`: `void`.
    pub counted: bool,
    /// `agreed`, `majority`, `single`, `forfeit`, 1.1.7 `witnessed`; `aborted`, `disagreement`,
    /// `insufficient_reports`, `no_reports`, `no_show`, `loadout_violation`, 1.1.7 `witness_disagreement`.
    pub decision: &'static str,
    pub winner: Option<Team>,
    pub rated: bool,
    /// `no_winner` or `one_sided` when the match was meant to be rated and is not.
    pub unrated_reason: Option<&'static str>,
    pub record: Option<Record>,
    pub verdicts: Vec<Verdict>,
    /// Why each invalid report was invalid (same order as the seats; `None` for others).
    pub invalid_reasons: Vec<Option<String>>,
    /// Seats whose report or silence supported the accepted record (1.1.12: never an error).
    pub support: Vec<usize>,
    pub results: usize,
    pub agreeing: usize,
    pub errors: usize,
    pub invalid: usize,
    /// The policy asks for a rematch.
    pub rematch: bool,
    /// 1.1.7: each spectator's verdict, in the order of `spec.witnesses`.
    pub witnesses: Vec<WitnessVerdict>,
    /// 1.1.7: why each spectator's report was invalid (same order; `None` for others).
    pub witness_reasons: Vec<Option<String>>,
    /// 1.1.7: the witnesses decided the result (a disagreement broken, or a required witness found).
    pub witnessed: bool,
    /// 1.1.8: the counters of the agreeing reports and whose report is the record (a counted result only).
    pub stats: StatsReport,
    /// 1.1.11: why a team lost by forfeit (decision `forfeit`), `None` otherwise.
    pub forfeit: Option<Forfeit>,
}

/// 1.1.11: a team that lost by forfeit, and why (`outcome.forfeit`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Forfeit {
    /// `left`: a player of the team left the match in play, its own launcher said so (`leave`, step 3b);
    /// `disconnected`: the record shows a player of the team disconnected (`disconnect: forfeit`, step 7); `loadout`: a
    /// player of the team broke the match's loadout (`loadout_check: forfeit`, step 7b).
    pub reason: &'static str,
    /// The team that lost.
    pub team: Team,
    /// The participants (indices in `spec.seats`) who left, disconnected or broke the loadout.
    pub seats: Vec<usize>,
}

/// 1.1.11, step 3b (`api.md`, "Result decision"; `leaving.md`): the forfeit a policy with `leave` `forfeit` or
/// `forfeit_unrated` gives a match that has **no valid result**, when one team's players left it in play. All of:
///
/// - at least one participant's report proves its own departure in play ([`left_in_play`]) and its mirrored status
///   reached `in-match` (a departure while the stage loads is no forfeit: the match had not begun);
/// - every such participant is in the same team (players of both teams who left: void);
/// - the departure was the first thing that went wrong: no other participant's report arrived before the first of
///   them (a crash or a lost connection reported earlier means the match was already lost; a player who then left a
///   frozen match is not the cause);
/// - a player of the other team reached `in-match` (that team was there to play).
///
/// Nothing else counts: a crash (`launcher-crashed`), a lost connection, a silence, a spectator's report. Only the
/// leaver's own report can make its team lose, so a report can only harm its author's team; a lie (`in_play` on a
/// departure that was not one) costs the liar.
pub fn departure_forfeit(policy: &Policy, spec: &MatchSpec) -> Option<Forfeit> {
    if !matches!(policy.leave, Leave::Forfeit | Leave::ForfeitUnrated) {
        return None;
    }
    let left: Vec<usize> = spec
        .seats
        .iter()
        .enumerate()
        .filter(|(_, s)| s.saw_in_match && s.report.as_ref().is_some_and(left_in_play))
        .map(|(i, _)| i)
        .collect();
    let team = spec.seats[*left.first()?].team;
    if left.iter().any(|&i| spec.seats[i].team != team) {
        return None;
    }
    let first = left.iter().filter_map(|&i| spec.seats[i].reported_at).min()?;
    let earlier = spec
        .seats
        .iter()
        .enumerate()
        .any(|(i, s)| !left.contains(&i) && s.report.is_some() && s.reported_at.is_some_and(|t| t < first));
    let other_played = spec.seats.iter().any(|s| s.team != team && s.saw_in_match);
    (!earlier && other_played).then_some(Forfeit {
        reason: "left",
        team,
        seats: left,
    })
}

/// 1.1.12, step 5: whether the candidate `record` explains the error `code` that participant `i` reported (the
/// error then neither supports nor contradicts it, and `i` is left out of the count of step 6). All of:
///
/// - the code is one that happens after the match started ([`MID_MATCH_ERRORS`]): any other says the match never
///   began for that player, which no record can be true against;
/// - its mirrored status reached `in-match`: it was in the match;
/// - the record shows it `disconnected`;
/// - a participant of its **own team** reported the candidate (`agreeing`): its team's game saw it leave. In a 1v1 an
///   error is therefore never explained (no game writes a record once the other player is gone), and a
///   record from the other team alone can never mark a player gone: that is the record a cheater would forge.
///
/// Up to 1.1.11 an error from a player the record showed `disconnected` counted as support: a forged record that
/// marked the opponent gone won a rated match whenever the opponent's game honestly reported a failure.
pub fn error_explained(spec: &MatchSpec, record: &Record, agreeing: &[usize], i: usize, code: &str) -> bool {
    let seat = &spec.seats[i];
    MID_MATCH_ERRORS.contains(&code)
        && seat.saw_in_match
        && record.player(&seat.member_name).is_some_and(|p| p.disconnected)
        && agreeing.iter().any(|&j| j != i && spec.seats[j].team == seat.team)
}

/// Whether the spectator's report counts as a witness's under `policy`.
fn counted_witness(policy: &Policy, w: &Witness) -> bool {
    policy.witness != WitnessUse::Ignore && (policy.witness_from == WitnessFrom::Any || w.referee)
}

/// Every spectator's verdict against `record` (the accepted one, or `None` for a void match); 1.1.8: a witness agrees
/// as a player does ([`same_outcome`]).
fn witness_verdicts(
    policy: &Policy,
    spec: &MatchSpec,
    classified: &[Option<Classified>],
    record: Option<&Record>,
) -> Vec<WitnessVerdict> {
    spec.witnesses
        .iter()
        .zip(classified)
        .map(|(w, c)| match c {
            None if w.launched => WitnessVerdict::Silent,
            None => WitnessVerdict::Absent,
            Some(Classified::Error(_)) => WitnessVerdict::Error,
            Some(Classified::Invalid(_)) => WitnessVerdict::Invalid,
            Some(Classified::Result(r)) => match record {
                Some(rec) if same_outcome(policy, spec, rec, r) => WitnessVerdict::Agreed,
                Some(_) => WitnessVerdict::Dissent,
                None => WitnessVerdict::Reported,
            },
        })
        .collect()
}

fn classify_witnesses(spec: &MatchSpec) -> Vec<Option<Classified>> {
    spec.witnesses
        .iter()
        .enumerate()
        .map(|(i, w)| w.report.as_ref().map(|r| classify_witness(r, spec, i)))
        .collect()
}

fn witness_reasons(classified: &[Option<Classified>]) -> Vec<Option<String>> {
    classified
        .iter()
        .map(|c| match c {
            Some(Classified::Invalid(why)) => Some(why.clone()),
            _ => None,
        })
        .collect()
}

/// Decides a match whose launch deadline passed with participants not launched (`no_show`). Spectators that did not
/// launch are never a no-show of the match.
pub fn decide_no_show(policy: &Policy, spec: &MatchSpec) -> Decision {
    let verdicts = spec
        .seats
        .iter()
        .map(|s| if s.launched { Verdict::Silent } else { Verdict::NoShow })
        .collect();
    let wc = classify_witnesses(spec);
    Decision {
        counted: false,
        decision: "no_show",
        winner: None,
        rated: false,
        unrated_reason: None,
        record: None,
        verdicts,
        invalid_reasons: vec![None; spec.seats.len()],
        support: Vec::new(),
        results: 0,
        agreeing: 0,
        errors: 0,
        invalid: 0,
        rematch: policy.no_show == crate::policy::NoShow::Rematch,
        witnesses: witness_verdicts(policy, spec, &wc, None),
        witness_reasons: witness_reasons(&wc),
        witnessed: false,
        stats: StatsReport::none(policy),
        forfeit: None,
    }
}

/// The result decision procedure, steps 1 to 8. `None` when the policy has a value this crate does not know.
pub fn decide(policy: &Policy, spec: &MatchSpec) -> Option<Decision> {
    if !policy.is_known() {
        return None;
    }
    let n = spec.seats.len();
    // Step 1: validation.
    let classified: Vec<Option<Classified>> = spec
        .seats
        .iter()
        .enumerate()
        .map(|(i, s)| s.report.as_ref().map(|r| classify(r, spec, i)))
        .collect();
    let mut verdicts: Vec<Verdict> = classified
        .iter()
        .map(|c| match c {
            None => Verdict::Silent,
            Some(Classified::Error(_)) => Verdict::Error,
            Some(Classified::Invalid(_)) => Verdict::Invalid,
            Some(Classified::Result(_)) => Verdict::Dissent,
        })
        .collect();
    let invalid_reasons: Vec<Option<String>> = classified
        .iter()
        .map(|c| match c {
            Some(Classified::Invalid(why)) => Some(why.clone()),
            _ => None,
        })
        .collect();
    let valid: Vec<(usize, &Record)> = classified
        .iter()
        .enumerate()
        .filter_map(|(i, c)| match c {
            Some(Classified::Result(r)) => Some((i, r.as_ref())),
            _ => None,
        })
        .collect();
    let errors = classified
        .iter()
        .filter(|c| matches!(c, Some(Classified::Error(_))))
        .count();
    let invalid = classified
        .iter()
        .filter(|c| matches!(c, Some(Classified::Invalid(_))))
        .count();
    // 1.1.7: the spectators' reports, read as witness reports; only those the policy counts can weigh.
    let wc = classify_witnesses(spec);
    let counted_records: Vec<&Record> = spec
        .witnesses
        .iter()
        .zip(&wc)
        .filter(|(w, _)| counted_witness(policy, w))
        .filter_map(|(_, c)| match c {
            Some(Classified::Result(r)) => Some(r.as_ref()),
            _ => None,
        })
        .collect();
    let base = Decision {
        counted: false,
        decision: "aborted",
        winner: None,
        rated: false,
        unrated_reason: None,
        record: None,
        verdicts: Vec::new(),
        invalid_reasons,
        support: Vec::new(),
        results: valid.len() + invalid,
        agreeing: 0,
        errors,
        invalid,
        rematch: false,
        witnesses: witness_verdicts(policy, spec, &wc, None),
        witness_reasons: witness_reasons(&wc),
        witnessed: false,
        stats: StatsReport::none(policy),
        forfeit: None,
    };
    let same = |a: &Record, b: &Record| same_outcome(policy, spec, a, b);
    // Step 3: no valid result.
    if valid.is_empty() {
        // Step 3b (1.1.11): a team whose player left the match in play loses by forfeit (`leave`). No record: the
        // winner is the other team; the rating follows `leave` and `witness: required` (a witness cannot confirm a
        // match that ended without a record, so a forfeit then changes no rating, as any unconfirmed result).
        if let Some(f) = departure_forfeit(policy, spec) {
            let (rated, unrated_reason) = if !spec.rated {
                (false, None)
            } else if policy.leave == Leave::ForfeitUnrated {
                (false, Some("forfeit"))
            } else if policy.witness == WitnessUse::Required {
                (false, Some("no_witness"))
            } else {
                (true, None)
            };
            return Some(Decision {
                counted: true,
                decision: "forfeit",
                winner: Some(f.team.other()),
                rated,
                unrated_reason,
                verdicts,
                forfeit: Some(f),
                ..base
            });
        }
        return Some(Decision {
            decision: match (errors, invalid) {
                (0, 0) => "no_reports",
                (0, _) => "invalid_reports",
                _ => "aborted",
            },
            verdicts,
            rematch: policy.abort == Abort::Rematch,
            ..base
        });
    }
    // Step 2: grouping by what decides the match (1.1.8; every shared field with `stats: strict`), in order of first
    // appearance.
    let mut groups: Vec<Vec<usize>> = Vec::new();
    for (k, (_, rec)) in valid.iter().enumerate() {
        match groups.iter_mut().find(|g| same(valid[g[0]].1, rec)) {
            Some(g) => g.push(k),
            None => groups.push(vec![k]),
        }
    }
    let void_disagreement = |verdicts: Vec<Verdict>, rematch: bool| Decision {
        decision: "disagreement",
        verdicts,
        rematch,
        ..base.clone()
    };
    // Step 4: the candidate. 1.1.7, step 4b: with `witness` `tiebreak` or `required`, when the players' valid results
    // differ, the counted witnesses break the tie if they all reported the same record and it is one group's.
    let witness_group = (groups.len() > 1
        && matches!(policy.witness, WitnessUse::Tiebreak | WitnessUse::Required)
        && !counted_records.is_empty()
        && counted_records.iter().all(|r| same(r, counted_records[0])))
    .then(|| groups.iter().position(|g| same(valid[g[0]].1, counted_records[0])))
    .flatten();
    let (cand, mut decision) = if groups.len() == 1 {
        (0, "agreed")
    } else if let Some(g) = witness_group {
        (g, "witnessed")
    } else {
        match policy.disagreement {
            Disagreement::Majority => {
                let (best, size) = groups
                    .iter()
                    .enumerate()
                    .map(|(i, g)| (i, g.len()))
                    .max_by_key(|&(i, len)| (len, usize::MAX - i))?;
                if size * 2 > valid.len() {
                    (best, "majority")
                } else {
                    return Some(void_disagreement(verdicts, false));
                }
            }
            Disagreement::Rematch => return Some(void_disagreement(verdicts, true)),
            _ => return Some(void_disagreement(verdicts, false)),
        }
    };
    // The record kept (1.1.8): among the candidate's reports, the one whose counters differ least from the others' and
    // the agreeing counted witnesses'; ties to the host's, then seat order. With two reports it is the host's when it
    // agrees (its battle_frames and counters are recorded); with three or more, one report alone that
    // strays (an edited file, a game out of step) is never the one recorded.
    let group: Vec<(usize, &Record)> = groups[cand].iter().map(|&k| valid[k]).collect();
    let agreeing_witnesses: Vec<&Record> = counted_records
        .iter()
        .copied()
        .filter(|r| same(r, group[0].1))
        .collect();
    let (kept, stats) = keep_record(policy, spec, &group, &agreeing_witnesses);
    let record = group[kept].1.clone();
    let agreeing_seats: Vec<usize> = groups[cand].iter().map(|&k| valid[k].0).collect();
    // Step 5 (1.1.12): an error never supports a result. It contradicts the candidate (void) unless the candidate
    // explains it: see [`error_explained`]. An explained error is left out of the count (step 6).
    let mut explained = 0;
    for (i, c) in classified.iter().enumerate() {
        if let Some(Classified::Error(code)) = c {
            if !error_explained(spec, &record, &agreeing_seats, i, code) {
                return Some(void_disagreement(
                    verdicts,
                    policy.disagreement == Disagreement::Rematch,
                ));
            }
            explained += 1;
        }
    }
    // Step 6: support and quorum. Support: the candidate's reporters, and with `consent_after_play` the silent
    // participants whose status reached `in-match`; out of the participants whose error the record does not explain.
    let mut support = agreeing_seats.clone();
    if policy.silence == Silence::ConsentAfterPlay {
        for (i, seat) in spec.seats.iter().enumerate() {
            if classified[i].is_none() && seat.saw_in_match {
                support.push(i);
            }
        }
    }
    support.sort_unstable();
    let counted_seats = n - explained;
    // 1.1.7: a candidate the witnesses chose counts them as one more voice, out of one more.
    let witnessed = decision == "witnessed";
    let (voices, of) = if witnessed {
        (support.len() + 1, counted_seats + 1)
    } else {
        (support.len(), counted_seats)
    };
    if !policy.quorum.met(voices, of)? {
        return Some(Decision {
            decision: "insufficient_reports",
            verdicts,
            ..base
        });
    }
    if groups.len() == 1 && agreeing_seats.len() == 1 {
        decision = "single";
    }
    for &i in &agreeing_seats {
        verdicts[i] = Verdict::Agreed;
    }
    // Step 7: the winner.
    let mut winner = record.winner();
    let mut forfeit = None;
    if policy.disconnect == Disconnect::Forfeit {
        let gone = |t: Team| -> Vec<usize> {
            (0..n)
                .filter(|&i| {
                    spec.seats[i].team == t
                        && record
                            .player(&spec.seats[i].member_name)
                            .is_some_and(|p| p.disconnected)
                })
                .collect()
        };
        let (a, b) = (gone(Team::Alpha), gone(Team::Bravo));
        if a.is_empty() != b.is_empty() {
            let (team, seats) = if a.is_empty() {
                (Team::Bravo, b)
            } else {
                (Team::Alpha, a)
            };
            winner = Some(team.other());
            decision = "forfeit";
            forfeit = Some(Forfeit {
                reason: "disconnected",
                team,
                seats,
            });
        }
    }
    // Step 7b (1.1.1): loadout constraints against the accepted record.
    if let (Some(l), true) = (&spec.loadout, policy.loadout_check != LoadoutCheck::Ignore) {
        let bad = loadout_violations(l, spec, &record);
        let teams: Vec<Team> = bad.iter().map(|&i| spec.seats[i].team).collect();
        let alpha = teams.contains(&Team::Alpha);
        let bravo = teams.contains(&Team::Bravo);
        if alpha || bravo {
            if policy.loadout_check == LoadoutCheck::Forfeit && alpha != bravo {
                let team = if alpha { Team::Alpha } else { Team::Bravo };
                winner = Some(team.other());
                decision = "forfeit";
                forfeit = Some(Forfeit {
                    reason: "loadout",
                    team,
                    seats: bad,
                });
            } else {
                return Some(Decision {
                    decision: "loadout_violation",
                    verdicts,
                    ..base
                });
            }
        }
    }
    // Step 7c (1.1.7): `witness: required`: a counted witness whose valid report differs from the accepted record
    // voids the match (the players agree on what the witness did not see: collusion or a lie); without a counted
    // witness agreeing, the result counts but changes no rating.
    let witness_agrees = counted_records.iter().any(|r| same(r, &record));
    if policy.witness == WitnessUse::Required && counted_records.iter().any(|r| !same(r, &record)) {
        return Some(Decision {
            decision: "witness_disagreement",
            verdicts,
            witnesses: witness_verdicts(policy, spec, &wc, None),
            ..base
        });
    }
    // Step 8 (1.1.12): rating. Each team must have confirmed the record with its own valid result (an error, a
    // silence or another team's report never confirm for a team); a counted referee's record equal to it confirms for
    // every team (the operator's own witness). A spectator who is not a referee may break a tie (step 4b) but never
    // stands for a team: it may be a player's second account.
    let referee_agrees = spec
        .witnesses
        .iter()
        .zip(&wc)
        .filter(|(w, _)| w.referee && counted_witness(policy, w))
        .any(|(_, c)| matches!(c, Some(Classified::Result(r)) if same(r, &record)));
    let both_teams = referee_agrees
        || [Team::Alpha, Team::Bravo]
            .iter()
            .all(|t| agreeing_seats.iter().any(|&i| spec.seats[i].team == *t));
    let (rated, unrated_reason) = if !spec.rated {
        (false, None)
    } else if winner.is_none() {
        (false, Some("no_winner"))
    } else if policy.rated_requires_both_teams && !both_teams {
        (false, Some("one_sided"))
    } else if policy.witness == WitnessUse::Required && !witness_agrees {
        (false, Some("no_witness"))
    } else {
        (true, None)
    };
    Some(Decision {
        counted: true,
        decision,
        winner,
        rated,
        unrated_reason,
        agreeing: agreeing_seats.len(),
        witnesses: witness_verdicts(policy, spec, &wc, Some(&record)),
        witnessed: witnessed || (policy.witness == WitnessUse::Required && witness_agrees),
        record: Some(record),
        verdicts,
        support,
        stats,
        forfeit,
        ..base
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::policy::{Quorum, Silence};
    use serde_json::json;

    /// A record of the usual shape for `names` (alpha, bravo, alpha, ...), seen by `me`.
    fn record(spec: &MatchSpec, me: usize, win_team: i64) -> Value {
        let players: Vec<Value> = spec
            .seats
            .iter()
            .enumerate()
            .map(|(i, s)| {
                json!({"name": s.member_name, "self": i == me, "team": s.team.as_str(), "paint": 100 + i,
                       "kills": i, "assists": 0, "deaths": 1, "specials": 0, "disconnected": false})
            })
            .collect();
        json!({
            "version": 1, "match_id": spec.game_match_id, "role": spec.seats[me].role.as_str(),
            "rule": 0, "mode": spec.mode, "stage_id": 3, "stage_scene": "Stage_A", "end_utc": 1794346807,
            "battle_frames": 10800, "duration_s": 180.0, "win_team": win_team,
            "winner": match win_team { 0 => "alpha", 1 => "bravo", _ => "none" }, "deemed_result": -1,
            "teams": [{"team": "alpha", "paint_points": 28, "turf_permille": 11},
                      {"team": "bravo", "paint_points": 57, "turf_permille": 22},
                      {"team": "charlie", "paint_points": 0, "turf_permille": 0}],
            "players": players, "t0_unix": 1794346556.64 + me as f64, "timeline_s": [["lobby loading", 7.3]]
        })
    }

    fn spec(n: usize) -> MatchSpec {
        MatchSpec {
            game_match_id: "K7Q2MXA9PL4DZB3W8R6TNY5HCF".into(),
            mode: "turf".into(),
            stage: None,
            rated: true,
            loadout: None,
            witnesses: Vec::new(),
            seats: (0..n)
                .map(|i| Seat {
                    member_name: format!("P{i}"),
                    team: if i % 2 == 0 { Team::Alpha } else { Team::Bravo },
                    role: if i == 0 { Role::Host } else { Role::Join },
                    launched: true,
                    saw_in_match: true,
                    report: None,
                    reported_at: None,
                })
                .collect(),
        }
    }

    fn report_all(s: &mut MatchSpec, win_team: i64) {
        for i in 0..s.seats.len() {
            let r = record(s, i, win_team);
            s.seats[i].report = Some(r);
        }
    }

    #[test]
    fn the_proof_run_files_agree() {
        // The two files of one match (shortened in docs), as written by the game.
        let host = json!({"version": 1, "match_id": "R3", "role": "host", "rule": 0, "mode": "turf", "stage_id": 3,
            "stage_scene": "Stage_03", "end_utc": 1794346807, "battle_frames": 10800, "duration_s": 180.00,
            "win_team": 1, "winner": "bravo", "deemed_result": -1,
            "teams": [{"team": "alpha", "paint_points": 28, "turf_permille": 11},
                      {"team": "bravo", "paint_points": 57, "turf_permille": 22},
                      {"team": "charlie", "paint_points": 0, "turf_permille": 0}],
            "players": [
              {"name": "Alpha", "self": true, "team": "bravo", "paint": 56, "kills": 0, "assists": 0, "deaths": 0, "specials": 0, "disconnected": false},
              {"name": "Bravo", "self": false, "team": "alpha", "paint": 27, "kills": 0, "assists": 0, "deaths": 0, "specials": 0, "disconnected": false}],
            "t0_unix": 1794346556.640, "timeline_s": [["lobby loading", 7.3]]});
        let mut guest = host.clone();
        guest["role"] = json!("join");
        guest["players"][0]["self"] = json!(false);
        guest["players"][1]["self"] = json!(true);
        guest["t0_unix"] = json!(1794346556.672);
        let mut s = MatchSpec {
            game_match_id: "R3".into(),
            mode: "turf".into(),
            stage: None,
            rated: true,
            loadout: None,
            witnesses: Vec::new(),
            seats: vec![
                Seat {
                    member_name: "Alpha".into(),
                    team: Team::Bravo,
                    role: Role::Host,
                    launched: true,
                    saw_in_match: true,
                    report: Some(host),
                    reported_at: None,
                },
                Seat {
                    member_name: "Bravo".into(),
                    team: Team::Alpha,
                    role: Role::Join,
                    launched: true,
                    saw_in_match: true,
                    report: Some(guest),
                    reported_at: None,
                },
            ],
        };
        let d = decide(&Policy::standard(), &s).unwrap();
        assert!(d.counted && d.rated);
        assert_eq!(d.decision, "agreed");
        assert_eq!(d.winner, Some(Team::Bravo));
        assert_eq!(d.verdicts, vec![Verdict::Agreed, Verdict::Agreed]);
        let public = d.record.unwrap().public_json();
        assert_eq!(public["teams"].as_array().unwrap().len(), 2);
        // The guest uploads the host's file: V3/V6 fail.
        s.seats[1].report = s.seats[0].report.clone();
        assert!(matches!(
            classify(s.seats[1].report.as_ref().unwrap(), &s, 1),
            Classified::Invalid(_)
        ));
    }

    #[test]
    fn validation_checks() {
        let s = spec(2);
        let good = record(&s, 0, 0);
        assert!(validate(&good, &s, 0).is_ok());
        let bad = |f: &dyn Fn(&mut Value)| {
            let mut r = good.clone();
            f(&mut r);
            validate(&r, &s, 0).unwrap_err()
        };
        assert!(bad(&|r| r["version"] = json!(2)).starts_with("V1"));
        assert!(bad(&|r| r["match_id"] = json!("OTHER")).starts_with("V2"));
        assert!(bad(&|r| r["role"] = json!("join")).starts_with("V3"));
        assert!(bad(&|r| r["players"][1]["name"] = json!("Mallory")).starts_with("V4"));
        assert!(
            bad(&|r| {
                r["players"].as_array_mut().unwrap().pop();
            })
            .starts_with("V4")
        );
        assert!(bad(&|r| r["players"][0]["team"] = json!("bravo")).starts_with("V5"));
        assert!(bad(&|r| r["players"][1]["self"] = json!(true)).starts_with("V6"));
        assert!(bad(&|r| r["mode"] = json!("zones")).starts_with("V7"));
        // L-S3: a report's text is quoted short, without line breaks or bidirectional controls.
        let long = bad(&|r| {
            r["mode"] = json!(format!(
                "x\n2026-11-13 INFO forged log line\u{202E}{}",
                "y".repeat(70_000)
            ))
        });
        assert!(
            long.len() < 100 && !long.contains('\n') && !long.contains('\u{202E}'),
            "{long}"
        );
        assert!(bad(&|r| r["winner"] = json!("bravo")).starts_with("V8"));
        assert!(
            bad(&|r| {
                r["teams"].as_array_mut().unwrap().remove(1);
            })
            .starts_with("V8")
        );
        assert!(bad(&|r| r["players"][0]["kills"] = json!(10_000)).contains("out of range"));
        assert!(bad(&|r| r["battle_frames"] = json!(0)).contains("out of range"));
        assert!(bad(&|r| r["players"][0]["paint"] = json!("lots")).contains("not an integer"));
        let mut staged = s.clone();
        staged.stage = Some("Stage_B".into());
        assert!(validate(&good, &staged, 0).unwrap_err().starts_with("V7"));
        assert!(matches!(classify(&json!("text"), &s, 0), Classified::Invalid(_)));
        assert!(matches!(classify(&json!({"error": 3}), &s, 0), Classified::Invalid(_)));
        assert_eq!(
            classify(&json!({"version": 1, "error": "room-not-found"}), &s, 0),
            Classified::Error("room-not-found".into())
        );
    }

    #[test]
    fn agreement_majority_and_dissent() {
        let p = Policy::standard();
        let mut s = spec(8);
        report_all(&mut s, 0);
        let d = decide(&p, &s).unwrap();
        assert_eq!((d.decision, d.counted, d.rated, d.agreeing), ("agreed", true, true, 8));
        // One cheater claims the other result.
        s.seats[3].report = Some(record(&s, 3, 1));
        let d = decide(&p, &s).unwrap();
        assert_eq!((d.decision, d.winner, d.agreeing), ("majority", Some(Team::Alpha), 7));
        assert_eq!(d.verdicts[3], Verdict::Dissent);
        // 4 against 4: void.
        for i in 0..4 {
            s.seats[i].report = Some(record(&s, i, 1));
        }
        let d = decide(&p, &s).unwrap();
        assert_eq!((d.decision, d.counted), ("disagreement", false));
        // With disagreement "void", even 7 against 1 is void.
        let strict = Policy {
            disagreement: Disagreement::Void,
            ..p.clone()
        };
        report_all(&mut s, 0);
        s.seats[3].report = Some(record(&s, 3, 1));
        assert_eq!(decide(&strict, &s).unwrap().decision, "disagreement");
    }

    #[test]
    fn contract_2_result_files() {
        let mut s = spec(2);
        for i in 0..2 {
            let mut r = record(&s, i, 0);
            r["version"] = json!(2);
            r["outcome"] = json!("completed");
            r["round"] = json!(2);
            r["requested"] = json!({"mode": "turf"});
            if i == 0 {
                r["settings_ok"] = json!(true);
            }
            s.seats[i].report = Some(r);
        }
        assert_eq!(decide(&Policy::standard(), &s).unwrap().decision, "agreed");
        // The host's room did not take the rules: its report is invalid (V10).
        s.seats[0].report.as_mut().unwrap()["settings_ok"] = json!(false);
        assert!(
            matches!(classify(s.seats[0].report.as_ref().unwrap(), &s, 0), Classified::Invalid(w) if w.starts_with("V10"))
        );
        // An aborted version 2 file is the error match-aborted.
        let aborted = json!({"version": 2, "match_id": "x", "outcome": "aborted", "reason": "a player left"});
        assert_eq!(classify(&aborted, &s, 1), Classified::Error("match-aborted".into()));
        assert!(matches!(
            classify(&json!({"version": 3}), &s, 1),
            Classified::Invalid(_)
        ));
    }

    #[test]
    fn loadout_constraints_are_checked_after_the_match() {
        use crate::policy::LoadoutCheck;
        let mut s = spec(2);
        for i in 0..2 {
            let mut r = record(&s, i, 0);
            r["players"][0]["weapon"] = json!(40);
            r["players"][1]["weapon"] = json!("7");
            r["players"][1]["gear"] = json!([1, 2, 3]);
            s.seats[i].report = Some(r);
        }
        // Bravo (P1) had to play weapon 50.
        s.loadout =
            Some(json!({"teams": {"bravo": {"weapon": {"fixed": "50"}}}, "all": {"weapon": {"allowed": [40, 50]}}}));
        let ignore = Policy::standard();
        assert_eq!(decide(&ignore, &s).unwrap().decision, "agreed", "ignored by default");
        let void = Policy {
            loadout_check: LoadoutCheck::Void,
            ..Policy::standard()
        };
        assert_eq!(decide(&void, &s).unwrap().decision, "loadout_violation");
        let forfeit = Policy {
            loadout_check: LoadoutCheck::Forfeit,
            ..Policy::standard()
        };
        let d = decide(&forfeit, &s).unwrap();
        assert_eq!((d.decision, d.winner), ("forfeit", Some(Team::Alpha)));
        // A player-level gear constraint, and a satisfied loadout.
        s.loadout = Some(json!({"players": {"P1": {"gear": {"clothes": {"denied": ["2"]}}}}}));
        assert_eq!(decide(&void, &s).unwrap().decision, "loadout_violation");
        s.loadout = Some(json!({"all": {"weapon": {"denied": ["99"]}}}));
        assert_eq!(decide(&void, &s).unwrap().decision, "agreed");
    }

    #[test]
    fn frame_counts_that_differ_between_engines_still_agree() {
        // The games of one match may count a few frames apart.
        for (host, guest) in [(18000, 18003), (28800, 28805), (10801, 10800)] {
            let mut s = spec(2);
            for (i, frames) in [(0, host), (1, guest)] {
                let mut r = record(&s, i, 1);
                r["battle_frames"] = json!(frames);
                r["duration_s"] = json!(frames as f64 / 60.0);
                s.seats[i].report = Some(r);
            }
            let d = decide(&Policy::standard(), &s).unwrap();
            assert_eq!(d.decision, "agreed", "{host} / {guest}");
            assert_eq!(d.record.unwrap().battle_frames, host, "the host's value is kept");
        }
    }

    #[test]
    fn only_invalid_reports_say_so() {
        let mut s = spec(2);
        s.stage = Some("Stage_B".into());
        for i in 0..2 {
            let r = record(&s, i, 0);
            s.seats[i].report = Some(r);
        }
        let d = decide(&Policy::standard(), &s).unwrap();
        assert_eq!((d.decision, d.counted, d.invalid), ("invalid_reports", false, 2));
        assert!(d.invalid_reasons[0].as_ref().unwrap().starts_with("V7"));
    }

    #[test]
    fn duel_disagreement_is_void() {
        let mut s = spec(2);
        s.seats[0].report = Some(record(&s, 0, 0));
        s.seats[1].report = Some(record(&s, 1, 1));
        let d = decide(&Policy::standard(), &s).unwrap();
        assert_eq!((d.decision, d.counted), ("disagreement", false));
    }

    #[test]
    fn silence_after_play_is_consent() {
        let p = Policy::standard();
        let mut s = spec(2);
        s.seats[0].report = Some(record(&s, 0, 0));
        let d = decide(&p, &s).unwrap();
        // 1.1.12: counted (the silent player played), never rated: only one team confirmed the record.
        assert_eq!(
            (d.decision, d.counted, d.rated, d.unrated_reason),
            ("single", true, false, Some("one_sided"))
        );
        assert_eq!(d.verdicts, vec![Verdict::Agreed, Verdict::Silent]);
        assert_eq!(d.support, vec![0, 1]);
        // The silent player never reached the match: insufficient support for a majority of 2.
        s.seats[1].saw_in_match = false;
        assert_eq!(decide(&p, &s).unwrap().decision, "insufficient_reports");
        // With quorum "any" it counts, but unrated: only one team supports it.
        let any = Policy {
            quorum: Quorum::Any,
            ..p.clone()
        };
        let d = decide(&any, &s).unwrap();
        assert_eq!((d.counted, d.rated, d.unrated_reason), (true, false, Some("one_sided")));
        // With silence "ignore" and the default quorum, even a played match needs the second report.
        s.seats[1].saw_in_match = true;
        let ignore = Policy {
            silence: Silence::Ignore,
            ..p
        };
        assert_eq!(decide(&ignore, &s).unwrap().decision, "insufficient_reports");
    }

    #[test]
    fn errors_aborts_and_disconnects() {
        let p = Policy::standard();
        let mut s = spec(2);
        s.seats[0].report = Some(json!({"version": 1, "match_id": "x", "error": "players-timeout"}));
        s.seats[1].report = Some(json!({"version": 1, "match_id": "x", "error": "room-not-found"}));
        let d = decide(&p, &s).unwrap();
        assert_eq!((d.decision, d.counted, d.errors), ("aborted", false, 2));
        assert_eq!(decide(&p, &spec(2)).unwrap().decision, "no_reports");
        // 1.1.12: in a duel, a record that shows the other player gone never explains its error (no game writes a
        // record once the other player is gone; that record is the one a cheater forges): void.
        let mut s = spec(2);
        let mut r = record(&s, 0, 0);
        r["players"][1]["disconnected"] = json!(true);
        s.seats[0].report = Some(r.clone());
        s.seats[1].report = Some(json!({"version": 1, "match_id": "x", "error": "launcher-crashed"}));
        let d = decide(&p, &s).unwrap();
        assert_eq!((d.decision, d.counted), ("disagreement", false), "{d:?}");
        // Same error but the record says the player stayed to the end: contradiction.
        s.seats[0].report = Some(record(&s, 0, 0));
        assert_eq!(decide(&p, &s).unwrap().decision, "disagreement");
        // 2v2: the crashed player's teammate's game recorded it leaving: explained, left out of the count, rated.
        let mut s = spec(4);
        for i in [0, 1, 3] {
            let mut r = record(&s, i, 0);
            r["players"][2]["disconnected"] = json!(true);
            s.seats[i].report = Some(r);
        }
        s.seats[2].report = Some(json!({"version": 1, "match_id": "x", "error": "launcher-crashed"}));
        let d = decide(&p, &s).unwrap();
        assert_eq!((d.decision, d.counted, d.rated), ("agreed", true, true), "{d:?}");
        assert_eq!(d.support, vec![0, 1, 3]);
        assert_eq!(d.verdicts[2], Verdict::Error);
        // Forfeit: the team with a disconnected player loses even if the record says it won.
        let forfeit = Policy {
            disconnect: Disconnect::Forfeit,
            ..p.clone()
        };
        let mut s = spec(2);
        let mut r0 = record(&s, 0, 0);
        r0["players"][0]["disconnected"] = json!(true);
        let mut r1 = record(&s, 1, 0);
        r1["players"][0]["disconnected"] = json!(true);
        s.seats[0].report = Some(r0);
        s.seats[1].report = Some(r1);
        let d = decide(&forfeit, &s).unwrap();
        assert_eq!((d.decision, d.winner), ("forfeit", Some(Team::Bravo)));
        // No winner in the record: completed but unrated.
        let mut s = spec(2);
        report_all(&mut s, -1);
        let d = decide(&p, &s).unwrap();
        assert_eq!(
            (d.counted, d.rated, d.unrated_reason, d.winner),
            (true, false, Some("no_winner"), None)
        );
        let ns = decide_no_show(&p, &spec(2));
        assert_eq!((ns.decision, ns.counted), ("no_show", false));
    }

    /// 1.1.12 (`api.md`, "Result decision", the table of what one player can do): in a duel, P1 forges a record that
    /// makes its team win and shows P0 gone; whatever P0's honest launcher reports, the forgery never rates, and it
    /// counts only when P0 played and stayed silent (`consent_after_play`) or the policy counts one report alone.
    #[test]
    fn one_cheater_never_imposes_a_rated_result() {
        let forged = |s: &MatchSpec| {
            let mut r = record(s, 1, 1);
            r["players"][0]["disconnected"] = json!(true);
            r
        };
        let error = |code: &str| json!({"version": 2, "match_id": "x", "error": code, "reason": "test"});
        let policies = [
            Policy::standard(),
            Policy {
                quorum: Quorum::Any,
                ..Policy::standard()
            },
            Policy {
                disconnect: Disconnect::Forfeit,
                ..Policy::standard()
            },
            leave(Leave::Forfeit),
            policy_with(WitnessUse::Tiebreak, WitnessFrom::Any),
        ];
        for p in &policies {
            let outcome = |s: &MatchSpec| {
                let d = decide(p, s).unwrap();
                (d.decision, d.counted, d.rated)
            };
            // P0's game failed before the match (the cheater, host or not, left the lobby), P0 never in the match.
            for code in ["players-timeout", "host-left", "room-not-found", "launcher-failed"] {
                let mut s = spec(2);
                s.seats[1].report = Some(forged(&s));
                s.seats[0].report = Some(error(code));
                s.seats[0].saw_in_match = false;
                assert_eq!(outcome(&s), ("disagreement", false, false), "{code} {p:?}");
                // Even with a status that reached the match: a pre-match code says the match never began.
                s.seats[0].saw_in_match = true;
                assert_eq!(outcome(&s), ("disagreement", false, false), "{code} {p:?}");
            }
            // P0's game failed in the match (the cheater quit): a duel's record never explains it.
            for code in ["connection-lost", "match-aborted", "launcher-crashed", "record-timeout"] {
                let mut s = spec(2);
                s.seats[1].report = Some(forged(&s));
                s.seats[0].report = Some(error(code));
                assert_eq!(outcome(&s), ("disagreement", false, false), "{code} {p:?}");
            }
            // P0's honest record: the reports disagree.
            let mut s = spec(2);
            s.seats[1].report = Some(forged(&s));
            s.seats[0].report = Some(record(&s, 0, 0));
            assert_eq!(outcome(&s), ("disagreement", false, false), "{p:?}");
            // P0 played and sent nothing: counted under `consent_after_play`, never rated.
            let mut s = spec(2);
            s.seats[1].report = Some(forged(&s));
            let d = decide(p, &s).unwrap();
            assert!(d.counted && !d.rated, "{p:?} {d:?}");
            // P0 never reached the match and sent nothing: only a policy that counts one report alone counts it.
            s.seats[0].saw_in_match = false;
            let d = decide(p, &s).unwrap();
            assert_eq!(d.counted, p.quorum == Quorum::Any, "{p:?} {d:?}");
            assert!(!d.rated);
        }
        // The cheater's second account in the stands, a counted witness under `witness_from: any`: it breaks the tie
        // it was brought for, never rates it.
        let tie = policy_with(WitnessUse::Tiebreak, WitnessFrom::Any);
        let mut s = with_witness(spec(2), false);
        s.seats[0].report = Some(record(&s, 0, 0));
        s.seats[1].report = Some(record(&s, 1, 1));
        s.witnesses[0].report = Some(witness_file(&s, 1));
        let d = decide(&tie, &s).unwrap();
        assert_eq!((d.decision, d.winner, d.rated), ("witnessed", Some(Team::Bravo), false));
        // A departure is a forfeit only on the leaver's own report: P1 can make its own team lose, never P0's.
        let forfeit = leave(Leave::Forfeit);
        let mut s = spec(2);
        error_at(&mut s, 1, LEFT_BY_PLAYER, Some(true), 100);
        let d = decide(&forfeit, &s).unwrap();
        assert_eq!((d.decision, d.winner), ("forfeit", Some(Team::Alpha)));
        for code in ["connection-lost", "launcher-crashed", "players-timeout"] {
            let mut s = spec(2);
            error_at(&mut s, 0, code, None, 100);
            s.seats[1].report = Some(forged(&s));
            s.seats[1].reported_at = Some(90);
            assert!(!decide(&forfeit, &s).unwrap().counted, "{code}");
        }
    }

    /// A spectator's file of the match: the record as the players' games have it, `spectated`, no entry of its own.
    fn witness_file(spec: &MatchSpec, win_team: i64) -> Value {
        let mut r = record(spec, 0, win_team);
        r["version"] = json!(2);
        r["outcome"] = json!("spectated");
        r["seat"] = json!("spectator");
        r["role"] = json!("join");
        for p in r["players"].as_array_mut().unwrap() {
            p["self"] = json!(false);
        }
        r
    }

    fn with_witness(mut s: MatchSpec, referee: bool) -> MatchSpec {
        s.witnesses.push(Witness {
            member_name: format!("W{}", s.witnesses.len()),
            role: Role::Join,
            referee,
            launched: true,
            report: None,
        });
        s
    }

    fn policy_with(witness: WitnessUse, from: WitnessFrom) -> Policy {
        Policy {
            witness,
            witness_from: from,
            ..Policy::standard()
        }
    }

    #[test]
    fn a_spectators_report_is_a_witness_never_a_players() {
        let mut s = with_witness(spec(2), false);
        report_all(&mut s, 0);
        s.witnesses[0].report = Some(witness_file(&s, 0));
        let d = decide(&Policy::standard(), &s).unwrap();
        assert_eq!((d.decision, d.agreeing, d.witnessed), ("agreed", 2, false), "{d:?}");
        assert_eq!(d.witnesses, vec![WitnessVerdict::Agreed]);
        assert_eq!(d.verdicts, vec![Verdict::Agreed, Verdict::Agreed]);
        // V1 and V6, witness reading: a spectator's file claiming a player of its own, or `completed`, is invalid.
        let mut forged = witness_file(&s, 0);
        forged["players"][1]["self"] = json!(true);
        assert!(validate_witness(&forged, &s, 0).unwrap_err().starts_with("V6"));
        let mut completed = witness_file(&s, 0);
        completed["outcome"] = json!("completed");
        assert!(validate_witness(&completed, &s, 0).unwrap_err().starts_with("V1"));
        s.witnesses[0].report = Some(completed);
        assert_eq!(
            decide(&Policy::standard(), &s).unwrap().witnesses,
            vec![WitnessVerdict::Invalid]
        );
        // V1, player reading: a player's file saying `spectated` (or seated in the stands) is not a player's report.
        assert!(validate(&witness_file(&s, 0), &s, 1).unwrap_err().starts_with("V1"));
        let mut seated = record(&s, 1, 0);
        seated["seat"] = json!("spectator");
        assert!(validate(&seated, &s, 1).unwrap_err().starts_with("V1"));
        // V3 and V10 take the spectator's own session role: a spectator may host.
        s.witnesses[0].role = Role::Host;
        let mut host_file = witness_file(&s, 0);
        host_file["role"] = json!("host");
        host_file["settings_ok"] = json!(false);
        assert!(validate_witness(&host_file, &s, 0).unwrap_err().starts_with("V10"));
        host_file["settings_ok"] = json!(true);
        assert!(validate_witness(&host_file, &s, 0).is_ok());
    }

    #[test]
    fn a_witness_breaks_a_duel_disagreement_when_the_policy_says_so() {
        let mut s = with_witness(spec(2), false);
        s.seats[0].report = Some(record(&s, 0, 0));
        s.seats[1].report = Some(record(&s, 1, 1));
        s.witnesses[0].report = Some(witness_file(&s, 1));
        // `ignore` (the default): the duel is void as before.
        assert_eq!(decide(&Policy::standard(), &s).unwrap().decision, "disagreement");
        let tie = policy_with(WitnessUse::Tiebreak, WitnessFrom::Any);
        let d = decide(&tie, &s).unwrap();
        assert_eq!(
            (d.decision, d.counted, d.winner, d.witnessed),
            ("witnessed", true, Some(Team::Bravo), true),
            "{d:?}"
        );
        assert_eq!(d.verdicts, vec![Verdict::Dissent, Verdict::Agreed]);
        assert_eq!(d.witnesses, vec![WitnessVerdict::Agreed]);
        // 1.1.12: a spectator who is not a referee breaks the tie but never rates it (it may be a player's second
        // account); a referee stands for the team that did not confirm.
        assert_eq!((d.rated, d.unrated_reason), (false, Some("one_sided")));
        // Only referees count: a member's report does not break it.
        let referees = policy_with(WitnessUse::Tiebreak, WitnessFrom::Referees);
        assert_eq!(decide(&referees, &s).unwrap().decision, "disagreement");
        s.witnesses[0].referee = true;
        let d = decide(&referees, &s).unwrap();
        assert_eq!((d.decision, d.rated), ("witnessed", true));
        assert!(decide(&tie, &s).unwrap().rated, "a referee counted under `any` too");
        // Two witnesses that disagree break nothing; a witness that matches neither group neither.
        let mut two = with_witness(s.clone(), true);
        two.witnesses[1].report = Some(witness_file(&two, 0));
        assert_eq!(decide(&tie, &two).unwrap().decision, "disagreement");
        // 1.1.8: what decides must differ (a deemed result); counters alone never make a witness match no group.
        let mut neither = witness_file(&s, 1);
        neither["deemed_result"] = json!(0);
        s.witnesses[0].report = Some(neither);
        assert_eq!(decide(&tie, &s).unwrap().decision, "disagreement");
        let mut own_counters = witness_file(&s, 1);
        own_counters["teams"][0]["paint_points"] = json!(1);
        own_counters["teams"][1]["turf_permille"] = json!(19);
        s.witnesses[0].report = Some(own_counters);
        assert_eq!(decide(&tie, &s).unwrap().decision, "witnessed");
        let strict_tie = Policy {
            stats: StatsCheck::Strict,
            ..tie.clone()
        };
        assert_eq!(
            decide(&strict_tie, &s).unwrap().decision,
            "disagreement",
            "1.1.7's rule"
        );
        // A witness never overturns players who agree (tiebreak only).
        report_all(&mut s, 0);
        s.witnesses[0].report = Some(witness_file(&s, 1));
        let d = decide(&tie, &s).unwrap();
        assert_eq!((d.decision, d.winner), ("agreed", Some(Team::Alpha)));
        assert_eq!(d.witnesses, vec![WitnessVerdict::Dissent]);
    }

    #[test]
    fn a_required_witness_rates_a_match_or_voids_a_contradicted_one() {
        let req = policy_with(WitnessUse::Required, WitnessFrom::Referees);
        let mut s = with_witness(spec(2), true);
        report_all(&mut s, 0);
        // No witness report: the result counts, without rating.
        let d = decide(&req, &s).unwrap();
        assert_eq!(
            (d.counted, d.rated, d.unrated_reason),
            (true, false, Some("no_witness")),
            "{d:?}"
        );
        assert_eq!(d.witnesses, vec![WitnessVerdict::Silent]);
        // The referee saw the same match: rated.
        s.witnesses[0].report = Some(witness_file(&s, 0));
        let d = decide(&req, &s).unwrap();
        assert_eq!((d.decision, d.rated, d.witnessed), ("agreed", true, true));
        // The referee saw another outcome than both players report: void.
        s.witnesses[0].report = Some(witness_file(&s, 1));
        let d = decide(&req, &s).unwrap();
        assert_eq!((d.decision, d.counted), ("witness_disagreement", false));
        // A member's report is not a referee's: no rating, nothing voided.
        s.witnesses[0].referee = false;
        let d = decide(&req, &s).unwrap();
        assert_eq!(
            (d.counted, d.rated, d.unrated_reason),
            (true, false, Some("no_witness"))
        );
    }

    #[test]
    fn a_spectators_error_or_absence_never_voids_the_match() {
        let mut s = with_witness(with_witness(spec(2), true), false);
        report_all(&mut s, 1);
        s.witnesses[0].report = Some(json!({"version": 2, "match_id": "x", "error": "host-left"}));
        s.witnesses[1].launched = false;
        for p in [Policy::standard(), policy_with(WitnessUse::Tiebreak, WitnessFrom::Any)] {
            let d = decide(&p, &s).unwrap();
            assert_eq!((d.decision, d.counted, d.errors), ("agreed", true, 0), "{d:?}");
            assert_eq!(d.witnesses, vec![WitnessVerdict::Error, WitnessVerdict::Absent]);
        }
        // A no-show is a player's: spectators that never launched are absent, not no-shows.
        let mut late = with_witness(spec(2), false);
        late.witnesses[0].launched = false;
        let ns = decide_no_show(&Policy::standard(), &late);
        assert_eq!(ns.verdicts, vec![Verdict::Silent, Verdict::Silent]);
        assert_eq!(ns.witnesses, vec![WitnessVerdict::Absent]);
        // The witness reading is part of policy knowledge: an unknown `witness` value decides nothing.
        let unknown = Policy {
            witness: WitnessUse::Unknown,
            ..Policy::standard()
        };
        assert!(decide(&unknown, &s).is_none());
    }

    #[test]
    fn invalid_reports_do_not_count() {
        let p = Policy::standard();
        let mut s = spec(2);
        s.seats[0].report = Some(record(&s, 0, 0));
        let mut forged = record(&s, 1, 1);
        forged["players"][0]["self"] = json!(true);
        s.seats[1].report = Some(forged);
        let d = decide(&p, &s).unwrap();
        // The forged report is invalid; the valid one alone lacks support (the forger reported, so is not silent).
        assert_eq!(d.verdicts[1], Verdict::Invalid);
        assert_eq!(d.decision, "insufficient_reports");
        assert!(d.invalid_reasons[1].as_ref().unwrap().starts_with("V6"));
        let unknown = Policy {
            quorum: Quorum::Unknown,
            ..p
        };
        assert!(decide(&unknown, &s).is_none());
    }

    fn with_stats(stats: StatsCheck) -> Policy {
        Policy {
            stats,
            ..Policy::standard()
        }
    }

    /// The two reports of a 1v1 between two machines: equal everywhere but each game's own area share (host
    /// 405 / 329, guest 399 / 325) and the machines' clocks (`end_utc`, 3 s apart). `strict` voids it (`disagreement`).
    fn real_duel() -> MatchSpec {
        let host = json!({"version": 2, "match_id": "m_test1v1turf", "role": "host", "seat": "player",
            "outcome": "completed", "rule": 0, "mode": "turf", "stage_id": 24, "stage_scene": "scene-a",
            "settings_ok": true, "battle_frames": 10800, "duration_s": 180.00, "end_utc": 1794500000,
            "win_team": 0, "winner": "alpha", "deemed_result": -1,
            "teams": [{"team": "alpha", "paint_points": 904, "turf_permille": 405, "count": 100},
                      {"team": "bravo", "paint_points": 736, "turf_permille": 329, "count": 100},
                      {"team": "charlie", "paint_points": 0, "turf_permille": 0, "count": 100}],
            "players": [
              {"name": "Lou", "self": false, "team": "alpha", "paint": 1600, "kills": 3, "assists": 0, "deaths": 1,
               "specials": 5, "disconnected": false, "weapon": 1110, "gear": [8010, 25010, 6027]},
              {"name": "Camille", "self": true, "team": "bravo", "paint": 1401, "kills": 1, "assists": 0, "deaths": 3,
               "specials": 0, "disconnected": false, "weapon": 8012, "gear": [8009, 5027, 2026]}]});
        let mut guest = host.clone();
        guest["role"] = json!("join");
        guest.as_object_mut().unwrap().remove("settings_ok");
        guest["end_utc"] = json!(1794500003);
        guest["teams"][0]["turf_permille"] = json!(399);
        guest["teams"][1]["turf_permille"] = json!(325);
        guest["players"][0]["self"] = json!(true);
        guest["players"][1]["self"] = json!(false);
        MatchSpec {
            game_match_id: "m_test1v1turf".into(),
            mode: "turf".into(),
            stage: None,
            rated: true,
            loadout: None,
            witnesses: Vec::new(),
            seats: vec![
                Seat {
                    member_name: "Camille".into(),
                    team: Team::Bravo,
                    role: Role::Host,
                    launched: true,
                    saw_in_match: true,
                    report: Some(host),
                    reported_at: None,
                },
                Seat {
                    member_name: "Lou".into(),
                    team: Team::Alpha,
                    role: Role::Join,
                    launched: true,
                    saw_in_match: true,
                    report: Some(guest),
                    reported_at: None,
                },
            ],
        }
    }

    #[test]
    fn the_real_duel_voided_by_1_1_7_agrees() {
        let s = real_duel();
        let d = decide(&Policy::standard(), &s).unwrap();
        assert_eq!(
            (d.decision, d.counted, d.winner, d.rated, d.agreeing),
            ("agreed", true, Some(Team::Alpha), true, 2),
            "{d:?}"
        );
        assert_eq!(d.verdicts, vec![Verdict::Agreed, Verdict::Agreed]);
        // The counters differ, within the tolerance: nothing flagged; the host's report is the record.
        assert_eq!(
            (d.stats.differ, d.stats.flagged, d.stats.record_seat),
            (true, false, Some(0))
        );
        assert!(d.stats.gaps.is_empty());
        let rec = d.record.unwrap();
        assert_eq!(
            (rec.shared.teams[0].turf_permille, rec.shared.teams[1].turf_permille),
            (405, 329)
        );
        assert_eq!(rec.player("Lou").unwrap().kills, 3);
        // `ignore` too; `strict` is 1.1.7's rule, which voided it.
        let d = decide(&with_stats(StatsCheck::Ignore), &s).unwrap();
        assert_eq!((d.decision, d.stats.differ, d.stats.flagged), ("agreed", true, false));
        let d = decide(&with_stats(StatsCheck::Strict), &s).unwrap();
        assert_eq!((d.decision, d.counted), ("disagreement", false));
        assert_eq!(d.verdicts, vec![Verdict::Dissent, Verdict::Dissent]);
    }

    #[test]
    fn a_winner_flipped_by_hand_is_refused_and_a_coherent_one_still_voids_a_duel() {
        let mut s = real_duel();
        // The guest (alpha) claims... the other way: bravo won, with the scores left as they were. Check V11.
        let mut flipped = s.seats[1].report.clone().unwrap();
        flipped["win_team"] = json!(1);
        flipped["winner"] = json!("bravo");
        assert!(
            validate(&flipped, &s, 1).unwrap_err().starts_with("V11"),
            "{:?}",
            validate(&flipped, &s, 1)
        );
        s.seats[1].report = Some(flipped.clone());
        let d = decide(&Policy::standard(), &s).unwrap();
        assert_eq!(
            (d.decision, d.counted, d.invalid),
            ("insufficient_reports", false, 1),
            "{d:?}"
        );
        assert!(d.invalid_reasons[1].as_ref().unwrap().starts_with("V11"));
        // Alone (the other player silent after playing), it is still refused: no result counts.
        let mut alone = real_duel();
        alone.seats[0].report = None;
        alone.seats[1].report = Some(flipped.clone());
        assert_eq!(decide(&Policy::standard(), &alone).unwrap().decision, "invalid_reports");
        // A forger who also swaps the scores is coherent: the reports disagree on the winner, a duel is void.
        let mut swapped = flipped;
        swapped["teams"][0] = json!({"team": "alpha", "paint_points": 736, "turf_permille": 325});
        swapped["teams"][1] = json!({"team": "bravo", "paint_points": 904, "turf_permille": 399});
        s.seats[1].report = Some(swapped);
        let d = decide(&Policy::standard(), &s).unwrap();
        assert_eq!((d.decision, d.counted), ("disagreement", false));
        assert_eq!(d.verdicts, vec![Verdict::Dissent, Verdict::Dissent]);
    }

    #[test]
    fn check_v11_only_judges_a_clear_turf_war_contradiction() {
        let teams = |a: (i64, i64), b: (i64, i64)| {
            vec![
                TeamLine {
                    team: "alpha".into(),
                    paint_points: a.0,
                    turf_permille: a.1,
                },
                TeamLine {
                    team: "bravo".into(),
                    paint_points: b.0,
                    turf_permille: b.1,
                },
            ]
        };
        // Alpha declared the winner; bravo leads.
        let clear = teams((700, 300), (900, 420));
        assert!(check_winner_scores(0, 0, -1, &clear).is_err());
        // A close match: each game computes its own share, the loser may lead a little on the guest's.
        assert!(check_winner_scores(0, 0, -1, &teams((700, 400), (710, 440))).is_ok());
        // The turf alone, or the points alone, are not enough.
        assert!(check_winner_scores(0, 0, -1, &teams((950, 300), (900, 420))).is_ok());
        assert!(check_winner_scores(0, 0, -1, &teams((700, 400), (900, 420))).is_ok());
        // Another rule, a deemed result, no winner: the record carries no reliable deciding quantity.
        assert!(check_winner_scores(1, 0, -1, &clear).is_ok());
        assert!(check_winner_scores(0, 0, 1, &clear).is_ok());
        assert!(check_winner_scores(0, -1, -1, &clear).is_ok());
        // Bravo declared the winner, alpha clearly ahead.
        assert!(check_winner_scores(0, 1, -1, &teams((900, 420), (700, 300))).is_err());
    }

    #[test]
    fn counters_inform_what_decides_must_agree() {
        let p = Policy::standard();
        let mut s = spec(2);
        report_all(&mut s, 1);
        // An elimination or an assist seen differently, some paint: agreed, not flagged.
        let mut r1 = record(&s, 1, 1);
        r1["players"][0]["kills"] = json!(1);
        r1["players"][1]["assists"] = json!(1);
        r1["players"][1]["paint"] = json!(140);
        s.seats[1].report = Some(r1);
        let d = decide(&p, &s).unwrap();
        assert_eq!(
            (d.decision, d.stats.differ, d.stats.flagged),
            ("agreed", true, false),
            "{d:?}"
        );
        // A huge gap: still agreed (the winners agree), flagged, with the gap named.
        let mut r1 = record(&s, 1, 1);
        r1["players"][1]["kills"] = json!(30);
        r1["teams"][1]["paint_points"] = json!(5000);
        s.seats[1].report = Some(r1);
        let d = decide(&p, &s).unwrap();
        assert_eq!(
            (d.decision, d.counted, d.stats.flagged),
            ("agreed", true, true),
            "{d:?}"
        );
        let fields: Vec<(&str, Option<Team>, Option<usize>)> =
            d.stats.gaps.iter().map(|g| (g.field, g.team, g.seat)).collect();
        assert_eq!(
            fields,
            vec![("paint_points", Some(Team::Bravo), None), ("kills", None, Some(1))]
        );
        assert_eq!(d.stats.gaps[1].values, [json!(1), json!(30)]);
        assert_eq!(d.stats.record_seat, Some(0), "two reports: the host's");
        // `ignore` never flags; `strict` voids (1.1.7).
        let d = decide(&with_stats(StatsCheck::Ignore), &s).unwrap();
        assert_eq!((d.decision, d.stats.differ, d.stats.flagged), ("agreed", true, false));
        assert_eq!(
            decide(&with_stats(StatsCheck::Strict), &s).unwrap().decision,
            "disagreement"
        );
        // What decides must still be equal: the deemed result, a player's disconnection, the stage.
        for (k, v) in [
            ("deemed_result", json!(1)),
            ("stage_scene", json!("Stage_B")),
            ("stage_id", json!(4)),
            ("rule", json!(1)),
        ] {
            let mut r1 = record(&s, 1, 1);
            r1[k] = v;
            s.seats[1].report = Some(r1);
            assert_eq!(decide(&p, &s).unwrap().decision, "disagreement", "{k}");
        }
        let mut r1 = record(&s, 1, 1);
        r1["players"][0]["disconnected"] = json!(true);
        s.seats[1].report = Some(r1);
        assert_eq!(decide(&p, &s).unwrap().decision, "disagreement", "disconnected");
        // The order of the players in the record decides nothing.
        let mut r1 = record(&s, 1, 1);
        r1["players"].as_array_mut().unwrap().reverse();
        s.seats[1].report = Some(r1);
        assert_eq!(decide(&p, &s).unwrap().decision, "agreed");
        assert_eq!(
            decide(&with_stats(StatsCheck::Strict), &s).unwrap().decision,
            "disagreement"
        );
    }

    #[test]
    fn eight_players_each_game_its_own_turf_share() {
        let p = Policy::standard();
        let mut s = spec(8);
        for i in 0..8 {
            let mut r = record(&s, i, 0);
            r["teams"][0]["turf_permille"] = json!(11 + (i as i64 % 3));
            r["teams"][1]["turf_permille"] = json!(22 - (i as i64 % 2));
            s.seats[i].report = Some(r);
        }
        let d = decide(&p, &s).unwrap();
        assert_eq!((d.decision, d.agreeing, d.stats.flagged), ("agreed", 8, false), "{d:?}");
        assert_eq!(
            decide(&with_stats(StatsCheck::Strict), &s).unwrap().decision,
            "disagreement",
            "1.1.7: three groups, none a majority"
        );
        // One player claims the other winner: the majority decides, the claim is a dissent.
        let mut cheat = record(&s, 5, 1);
        cheat["teams"][1]["turf_permille"] = json!(40);
        s.seats[5].report = Some(cheat);
        let d = decide(&p, &s).unwrap();
        assert_eq!((d.decision, d.winner, d.agreeing), ("majority", Some(Team::Alpha), 7));
        assert_eq!(d.verdicts[5], Verdict::Dissent);
        // The host edits its counters: the result stands, flagged, and the host's report is not the record.
        let mut host = record(&s, 0, 0);
        host["players"][0]["kills"] = json!(40);
        host["players"][0]["paint"] = json!(9000);
        s.seats[0].report = Some(host);
        let d = decide(&p, &s).unwrap();
        assert_eq!((d.decision, d.stats.flagged), ("majority", true));
        assert_ne!(d.stats.record_seat, Some(0));
        let kept = d.record.unwrap();
        assert_eq!(kept.player("P0").unwrap().kills, 0, "the honest value is recorded");
        // Four against four on the winner: void.
        for i in 0..4 {
            s.seats[i].report = Some(record(&s, i, 1));
        }
        for i in 4..8 {
            s.seats[i].report = Some(record(&s, i, 0));
        }
        assert_eq!(decide(&p, &s).unwrap().decision, "disagreement");
    }

    #[test]
    fn the_record_kept_is_the_report_closest_to_the_others() {
        let p = Policy::standard();
        let mut s = spec(3);
        report_all(&mut s, 0);
        // Seat 1 counts an elimination differently from both others: the host's (seat 0) is kept (equal to seat 2's).
        let mut r1 = record(&s, 1, 0);
        r1["players"][2]["kills"] = json!(5);
        s.seats[1].report = Some(r1);
        assert_eq!(decide(&p, &s).unwrap().stats.record_seat, Some(0));
        // The host strays alone: seat 1 (now equal to seat 2) is kept.
        report_all(&mut s, 0);
        let mut r0 = record(&s, 0, 0);
        r0["teams"][0]["paint_points"] = json!(29);
        s.seats[0].report = Some(r0);
        let d = decide(&p, &s).unwrap();
        assert_eq!((d.decision, d.stats.record_seat), ("agreed", Some(1)));
        assert_eq!(d.record.unwrap().shared.teams[0].paint_points, 28);
        // A duel with a counted witness: the player report closest to the witness's is kept.
        let mut duel = with_witness(spec(2), true);
        report_all(&mut duel, 0);
        let mut r0 = record(&duel, 0, 0);
        r0["teams"][1]["turf_permille"] = json!(25);
        duel.seats[0].report = Some(r0);
        duel.witnesses[0].report = Some(witness_file(&duel, 0));
        let req = policy_with(WitnessUse::Required, WitnessFrom::Referees);
        let d = decide(&req, &duel).unwrap();
        assert_eq!(
            (d.decision, d.rated, d.stats.record_seat),
            ("agreed", true, Some(1)),
            "{d:?}"
        );
        assert_eq!(d.witnesses, vec![WitnessVerdict::Agreed]);
    }

    #[test]
    fn witnesses_agree_on_what_decides() {
        // A referee's game computes its own area share too: with `required`, the match is rated, not voided.
        let req = policy_with(WitnessUse::Required, WitnessFrom::Referees);
        let mut s = with_witness(real_duel(), true);
        let mut w = s.seats[1].report.clone().unwrap();
        w["outcome"] = json!("spectated");
        w["seat"] = json!("spectator");
        w["role"] = json!("join");
        w["players"][0]["self"] = json!(false);
        w["teams"][0]["turf_permille"] = json!(402);
        w["teams"][1]["turf_permille"] = json!(327);
        s.witnesses[0].report = Some(w.clone());
        let d = decide(&req, &s).unwrap();
        assert_eq!((d.decision, d.counted, d.rated), ("agreed", true, true), "{d:?}");
        assert_eq!(d.witnesses, vec![WitnessVerdict::Agreed]);
        // 1.1.7's rule voided it twice over (players, then the witness).
        let strict = Policy {
            stats: StatsCheck::Strict,
            ..req.clone()
        };
        assert_eq!(decide(&strict, &s).unwrap().decision, "disagreement");
        // A witness who saw the other winner still voids a required-witness match.
        w["win_team"] = json!(1);
        w["winner"] = json!("bravo");
        w["teams"][0] = json!({"team": "alpha", "paint_points": 736, "turf_permille": 327});
        w["teams"][1] = json!({"team": "bravo", "paint_points": 904, "turf_permille": 402});
        s.witnesses[0].report = Some(w);
        assert_eq!(decide(&req, &s).unwrap().decision, "witness_disagreement");
    }

    #[test]
    fn weapons_decide_only_when_the_policy_checks_loadouts() {
        let mut s = spec(2);
        for i in 0..2 {
            let mut r = record(&s, i, 0);
            r["players"][0]["weapon"] = json!(if i == 0 { 40 } else { 41 });
            r["players"][1]["weapon"] = json!(50);
            s.seats[i].report = Some(r);
        }
        let d = decide(&Policy::standard(), &s).unwrap();
        assert_eq!((d.decision, d.stats.flagged), ("agreed", true));
        assert_eq!(d.stats.gaps[0].field, "weapon");
        s.loadout = Some(json!({"all": {"weapon": {"allowed": [40, 41, 50]}}}));
        let void = Policy {
            loadout_check: LoadoutCheck::Void,
            ..Policy::standard()
        };
        assert_eq!(decide(&void, &s).unwrap().decision, "disagreement");
        assert_eq!(decide(&Policy::standard(), &s).unwrap().decision, "agreed");
    }

    /// 1.1.11: an error report as a launcher uploads it, received at `at` (server seconds).
    fn error_at(s: &mut MatchSpec, i: usize, code: &str, in_play: Option<bool>, at: i64) {
        let mut r = json!({"version": 2, "match_id": s.game_match_id, "error": code, "reason": "test"});
        if let Some(b) = in_play {
            r["in_play"] = json!(b);
        }
        s.seats[i].report = Some(r);
        s.seats[i].reported_at = Some(at);
    }

    fn leave(l: Leave) -> Policy {
        Policy {
            leave: l,
            ..Policy::standard()
        }
    }

    /// 1.1.11, step 3b, case by case in a 1v1 (`leaving.md`, "The rule").
    #[test]
    fn a_departure_in_play_is_a_forfeit_in_a_duel() {
        let forfeit = leave(Leave::Forfeit);
        // The usual case: the leaver's own launcher says it left in play, the other game `connection-lost` later.
        let mut s = spec(2);
        error_at(&mut s, 1, LEFT_BY_PLAYER, Some(true), 100);
        error_at(&mut s, 0, "connection-lost", None, 115);
        let d = decide(&forfeit, &s).unwrap();
        assert_eq!(
            (d.decision, d.counted, d.winner, d.rated, d.unrated_reason),
            ("forfeit", true, Some(Team::Alpha), true, None)
        );
        assert_eq!(
            d.forfeit,
            Some(Forfeit {
                reason: "left",
                team: Team::Bravo,
                seats: vec![1]
            })
        );
        assert!(d.record.is_none());
        assert_eq!(d.verdicts, vec![Verdict::Error, Verdict::Error]);
        // The policy's default (and every declaration without `leave`): void.
        let d = decide(&Policy::standard(), &s).unwrap();
        assert_eq!((d.decision, d.counted, d.forfeit), ("aborted", false, None));
        // `forfeit_unrated`: the same winner, no rating change.
        let d = decide(&leave(Leave::ForfeitUnrated), &s).unwrap();
        assert_eq!(
            (d.decision, d.winner, d.rated, d.unrated_reason),
            ("forfeit", Some(Team::Alpha), false, Some("forfeit"))
        );
        // An unrated match: the forfeit counts, nothing to rate.
        s.rated = false;
        let d = decide(&forfeit, &s).unwrap();
        assert_eq!((d.decision, d.rated, d.unrated_reason), ("forfeit", false, None));
        s.rated = true;
        // `witness: required`: no witness can confirm a match without a record, so no rating change.
        let req = Policy {
            witness: WitnessUse::Required,
            ..forfeit.clone()
        };
        let d = decide(&req, &s).unwrap();
        assert_eq!(
            (d.decision, d.rated, d.unrated_reason),
            ("forfeit", false, Some("no_witness"))
        );
        // The other player never reports (its launcher went away too): the forfeit stands at the decision.
        let mut s = spec(2);
        error_at(&mut s, 0, LEFT_BY_PLAYER, Some(true), 100);
        let d = decide(&forfeit, &s).unwrap();
        assert_eq!((d.decision, d.winner), ("forfeit", Some(Team::Bravo)));
        assert_eq!(d.verdicts, vec![Verdict::Error, Verdict::Silent]);
    }

    #[test]
    fn what_never_makes_a_forfeit() {
        let forfeit = leave(Leave::Forfeit);
        let void = |s: &MatchSpec| {
            let d = decide(&forfeit, s).unwrap();
            (d.decision, d.counted, d.forfeit.is_some())
        };
        let aborted = ("aborted", false, false);
        // A crash, a lost connection: never.
        let mut s = spec(2);
        error_at(&mut s, 1, "launcher-crashed", None, 100);
        error_at(&mut s, 0, "connection-lost", None, 115);
        assert_eq!(void(&s), aborted);
        let mut s = spec(2);
        error_at(&mut s, 0, "connection-lost", None, 100);
        error_at(&mut s, 1, "connection-lost", None, 100);
        assert_eq!(void(&s), aborted);
        // A departure its launcher could not prove in play (a launcher or a game without `in_play`; a departure after the
        // session stopped is `connection-lost` from a 1.1.11 game; before the match, `in_play: false`).
        for in_play in [None, Some(false)] {
            let mut s = spec(2);
            error_at(&mut s, 1, LEFT_BY_PLAYER, in_play, 100);
            error_at(&mut s, 0, "connection-lost", None, 115);
            assert_eq!(void(&s), aborted, "in_play {in_play:?}");
        }
        // Players of both teams left: void.
        let mut s = spec(2);
        error_at(&mut s, 0, LEFT_BY_PLAYER, Some(true), 100);
        error_at(&mut s, 1, LEFT_BY_PLAYER, Some(true), 101);
        assert_eq!(void(&s), aborted);
        // The other player's crash came first: the match was already lost when the player left it.
        let mut s = spec(2);
        error_at(&mut s, 0, "launcher-crashed", None, 99);
        error_at(&mut s, 1, LEFT_BY_PLAYER, Some(true), 100);
        assert_eq!(void(&s), aborted);
        // ... but a report received the same second does not come before.
        s.seats[0].reported_at = Some(100);
        assert_eq!(void(&s), ("forfeit", true, true));
        // The leaver never reached the match (it left while the stage loaded): no forfeit, the offense only.
        let mut s = spec(2);
        error_at(&mut s, 1, LEFT_BY_PLAYER, Some(true), 100);
        error_at(&mut s, 0, "connection-lost", None, 115);
        s.seats[1].saw_in_match = false;
        assert_eq!(void(&s), aborted);
        // The other team never reached the match either.
        s.seats[1].saw_in_match = true;
        s.seats[0].saw_in_match = false;
        assert_eq!(void(&s), aborted);
        // A spectator who leaves is never a player's departure (its report is a witness's).
        let mut s = spec(2);
        s.witnesses.push(Witness {
            member_name: "S".into(),
            role: Role::Join,
            referee: false,
            launched: true,
            report: Some(json!({"version": 2, "match_id": s.game_match_id, "error": LEFT_BY_PLAYER, "in_play": true})),
        });
        error_at(&mut s, 0, "connection-lost", None, 115);
        error_at(&mut s, 1, "connection-lost", None, 115);
        assert_eq!(void(&s), aborted);
    }

    /// Teams: a departure makes its team lose only when the match produced no valid result; a result the remaining
    /// games agree on is the one that counts.
    #[test]
    fn in_teams_a_departure_is_a_forfeit_only_without_a_result() {
        let forfeit = leave(Leave::Forfeit);
        // 2v2 (P0, P2 alpha; P1, P3 bravo): P2 left, every other game stopped with the session (no record).
        let mut s = spec(4);
        error_at(&mut s, 2, LEFT_BY_PLAYER, Some(true), 100);
        for i in [0, 1, 3] {
            error_at(&mut s, i, "connection-lost", None, 120);
        }
        let d = decide(&forfeit, &s).unwrap();
        assert_eq!((d.decision, d.winner), ("forfeit", Some(Team::Bravo)));
        assert_eq!(
            d.forfeit.as_ref().map(|f| (f.team, f.seats.clone())),
            Some((Team::Alpha, vec![2]))
        );
        // Two players of the same team left: still that team's forfeit.
        error_at(&mut s, 0, LEFT_BY_PLAYER, Some(true), 130);
        let d = decide(&forfeit, &s).unwrap();
        assert_eq!(d.forfeit.map(|f| f.seats), Some(vec![0, 2]));
        // The other games went on and agree on a record that shows the leaver disconnected: that record counts (here
        // alpha's win, whatever the departure: `disconnect: game_result`).
        let mut s = spec(4);
        for i in [0, 1, 3] {
            let mut r = record(&s, i, 0);
            r["players"][2]["disconnected"] = json!(true);
            s.seats[i].report = Some(r);
            s.seats[i].reported_at = Some(300);
        }
        error_at(&mut s, 2, LEFT_BY_PLAYER, Some(true), 100);
        let d = decide(&forfeit, &s).unwrap();
        assert_eq!((d.decision, d.winner, d.forfeit), ("agreed", Some(Team::Alpha), None));
        // Records that say the leaver played to the end contradict its report: void, as before (step 5).
        for i in [0, 1, 3] {
            let r = record(&s, i, 0);
            s.seats[i].report = Some(r);
        }
        assert_eq!(decide(&forfeit, &s).unwrap().decision, "disagreement");
    }

    /// The other forfeits name their reason too (1.1.11 `outcome.forfeit`).
    #[test]
    fn every_forfeit_says_why() {
        let p = Policy {
            disconnect: Disconnect::Forfeit,
            ..Policy::standard()
        };
        let mut s = spec(2);
        for i in 0..2 {
            let mut r = record(&s, i, 0);
            r["players"][0]["disconnected"] = json!(true);
            s.seats[i].report = Some(r);
        }
        let d = decide(&p, &s).unwrap();
        assert_eq!(
            d.forfeit,
            Some(Forfeit {
                reason: "disconnected",
                team: Team::Alpha,
                seats: vec![0]
            })
        );
        assert_eq!(decide(&Policy::standard(), &s).unwrap().forfeit, None);
    }
}
