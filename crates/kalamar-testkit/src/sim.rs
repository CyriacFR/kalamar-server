//! A simulated game: the status files and result files the supported game build writes in its automatic match mode
//! (`match-contract.md`), shaped exactly like the real ones, for any match object — with optional misbehavior
//! (silence, errors, a cheated or forged result, a crash shown as a disconnection).

use kalamar_protocol::{Role, Team};
use serde_json::{Value, json};

/// How one simulated player behaves.
#[derive(Debug, Clone, PartialEq)]
pub enum Play {
    /// Plays and uploads the game's result.
    Honest,
    /// Uploads nothing; `played`: its status reached `in-match` first.
    Silent { played: bool },
    /// The game stops with this error code (status `error`, result file with `error`).
    Error(String),
    /// Uploads a record where its own team won with extra paint.
    Cheat,
    /// Uploads a record whose `self` flag is someone else (a copied file).
    WrongSelf,
    /// Its game crashes mid-match: the others' records show it disconnected; it uploads `launcher-crashed`.
    Crashed,
    /// 1.1.8: plays honestly, but its game computed its own area share, this many permille below the others' for
    /// alpha (and two thirds of it for bravo), as between two machines.
    OwnTurf(i64),
    /// 1.1.12: a cheater: plays, then uploads a record where its own team won and every player of the other team is
    /// `disconnected` (a forgery that must not win a rated match against an honest error report).
    Forge,
}

#[derive(Debug, Clone)]
pub struct SimSeat {
    pub member_name: String,
    pub team: Team,
    pub role: Role,
}

#[derive(Debug, Clone)]
pub struct Sim {
    pub game_match_id: String,
    pub mode: String,
    pub stage: Option<String>,
    /// The match's `rules.loadout`, applied by the simulated launcher (1.1.3).
    pub loadout: Option<Value>,
    pub seats: Vec<SimSeat>,
    pub seed: u64,
}

fn mix(mut x: u64) -> u64 {
    x ^= x >> 33;
    x = x.wrapping_mul(0xff51_afd7_ed55_8ccd);
    x ^= x >> 33;
    x = x.wrapping_mul(0xc4ce_b9fe_1a85_ec53);
    x ^ (x >> 33)
}

impl Sim {
    /// From a participant view of a match (with `game`).
    pub fn from_match(m: &Value) -> Option<Sim> {
        let game_match_id = m["game"]["match_id"].as_str()?.to_string();
        let seats: Option<Vec<SimSeat>> = m["participants"]
            .as_array()?
            .iter()
            .map(|p| {
                Some(SimSeat {
                    member_name: p["member_name"].as_str()?.to_string(),
                    team: Team::parse(p["team"].as_str()?)?,
                    role: Role::parse(p["role"].as_str()?)?,
                })
            })
            .collect();
        let seed = game_match_id.bytes().fold(0xcbf2_9ce4_8422_2325u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x100_0000_01b3)
        });
        Some(Sim {
            game_match_id,
            mode: m["rules"]["mode"].as_str()?.to_string(),
            stage: m["rules"]["stage"].as_str().map(str::to_string),
            loadout: m["rules"].get("loadout").cloned().filter(|v| !v.is_null()),
            seats: seats?,
            seed,
        })
    }

    /// The weapon an honest simulated launcher gives seat `i`: the imposed one (player, team, everyone), else the
    /// first allowed, else its own (opaque ids).
    pub fn weapon_for(&self, i: usize) -> Value {
        let s = &self.seats[i];
        let Some(l) = &self.loadout else {
            return json!("weapon-own");
        };
        let w = l["players"][&s.member_name]["weapon"]
            .as_object()
            .or(l["teams"][s.team.as_str()]["weapon"].as_object())
            .or(l["all"]["weapon"].as_object());
        match w {
            Some(c) if c.contains_key("fixed") => c["fixed"].clone(),
            Some(c) if c.contains_key("allowed") => c["allowed"][0].clone(),
            _ => json!("weapon-own"),
        }
    }

    pub fn seat_of(&self, member_name: &str) -> Option<usize> {
        self.seats.iter().position(|s| s.member_name == member_name)
    }

    /// The winner the honest game records (0 alpha, 1 bravo), from the seed.
    pub fn honest_win_team(&self) -> i64 {
        (mix(self.seed) % 2) as i64
    }

    /// The shared record for `win_team`, with `disconnected` seats marked and optional extra paint for a team. The
    /// winning team scores more than the other (a record of `rule` 0 whose winner clearly trails on both scores fails check V11, 1.1.8).
    fn record(&self, win_team: i64, disconnected: &[bool], bonus: Option<(Team, i64)>) -> (Vec<Value>, Vec<Value>) {
        let team_index = |t: Team| if t == Team::Alpha { 0 } else { 1 };
        let mut paints: Vec<i64> = self
            .seats
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let r = mix(self.seed ^ (i as u64 + 1));
                let mut paint = 200 + (r % 900) as i64;
                if let Some((t, b)) = bonus
                    && s.team == t
                {
                    paint += b;
                }
                paint
            })
            .collect();
        let sum = |paints: &[i64], k: usize| -> i64 {
            self.seats
                .iter()
                .zip(paints)
                .filter(|(s, _)| team_index(s.team) == k)
                .map(|(_, p)| *p)
                .sum()
        };
        if (0..=1).contains(&win_team) {
            let (w, l) = (win_team as usize, 1 - win_team as usize);
            let short = sum(&paints, l) - sum(&paints, w);
            if short >= 0
                && let Some(first) = self.seats.iter().position(|s| team_index(s.team) == w)
            {
                paints[first] += short + 60;
            }
        }
        let paint_by_team = [sum(&paints, 0), sum(&paints, 1)];
        let players: Vec<Value> = self
            .seats
            .iter()
            .enumerate()
            .map(|(i, s)| {
                let r = mix(self.seed ^ (i as u64 + 1));
                json!({"name": s.member_name, "self": false, "team": s.team.as_str(), "paint": paints[i],
                       "kills": (r >> 12) % 9, "assists": (r >> 20) % 5, "deaths": (r >> 28) % 8,
                       "specials": (r >> 36) % 4, "disconnected": disconnected[i],
                       "weapon": self.weapon_for(i), "gear": ["gear-h", "gear-c", "gear-s"]})
            })
            .collect();
        // As in real records, a team's paint points can exceed its players' sum by one.
        let teams = vec![
            json!({"team": "alpha", "paint_points": paint_by_team[0] + 1, "turf_permille": (paint_by_team[0] / 6).min(1000)}),
            json!({"team": "bravo", "paint_points": paint_by_team[1], "turf_permille": (paint_by_team[1] / 6).min(1000)}),
            json!({"team": "charlie", "paint_points": 0, "turf_permille": 0}),
        ];
        (teams, players)
    }

    fn timeline(&self, seat: usize, upto_match: bool) -> Value {
        let host = self.seats[seat].role == Role::Host;
        let mut t = vec![
            json!(["lobby loading", 7.3]),
            json!(["lobby network up", 11.4]),
            json!(["session list open", 17.7]),
            json!(["search ended", 20.7]),
        ];
        if host {
            t.extend([
                json!(["session created", 23.0]),
                json!(["all players in", 33.3]),
                json!(["teams screen", 40.9]),
                json!(["teams confirmed", 46.5]),
            ]);
        } else {
            t.extend([json!(["a session is listed", 31.2]), json!(["session joined", 33.3])]);
        }
        if upto_match {
            t.extend([
                json!(["stage loading (Stage_Sim01)", 50.0]),
                json!(["stage intro (its frames)", 53.8]),
                json!(["revealed (stage intro)", 53.8]),
                json!(["match record", 251.2]),
            ]);
        }
        Value::Array(t)
    }

    /// The result file `seat` uploads under `plays` (every seat's behavior, which shapes the shared record), or
    /// `None` when it uploads nothing.
    pub fn result_for(&self, seat: usize, plays: &[Play]) -> Option<Value> {
        let s = &self.seats[seat];
        let t0 = 1_794_346_556.640 + seat as f64 * 0.032;
        match &plays[seat] {
            Play::Silent { .. } => return None,
            Play::Error(code) => {
                return Some(
                    json!({"version": 1, "match_id": self.game_match_id, "role": s.role.as_str(), "error": code,
                    "reason": format!("simulated error {code}"), "t0_unix": t0, "timeline_s": self.timeline(seat, false)}),
                );
            }
            Play::Crashed => {
                return Some(
                    json!({"version": 1, "match_id": self.game_match_id, "error": "launcher-crashed",
                    "reason": "the game exited with code -1073741819 and no result"}),
                );
            }
            _ => {}
        }
        let mut disconnected: Vec<bool> = plays.iter().map(|p| *p == Play::Crashed).collect();
        if plays[seat] == Play::Forge {
            for (i, d) in disconnected.iter_mut().enumerate() {
                *d = self.seats[i].team != s.team;
            }
        }
        let (win_team, bonus) = match &plays[seat] {
            Play::Cheat | Play::Forge => {
                let own = if s.team == Team::Alpha { 0 } else { 1 };
                (own, Some((s.team, 500)))
            }
            _ => (self.honest_win_team(), None),
        };
        let (mut teams, mut players) = self.record(win_team, &disconnected, bonus);
        if let Play::OwnTurf(delta) = plays[seat] {
            for (k, d) in [(0, delta), (1, delta * 2 / 3)] {
                let t = teams[k]["turf_permille"].as_i64().unwrap_or(0);
                teams[k]["turf_permille"] = json!((t - d).clamp(0, 1000));
            }
        }
        let self_index = if plays[seat] == Play::WrongSelf {
            (seat + 1) % self.seats.len()
        } else {
            seat
        };
        players[self_index]["self"] = json!(true);
        Some(json!({
            "version": 1,
            "match_id": self.game_match_id,
            "role": s.role.as_str(),
            "rule": 0,
            "mode": self.mode,
            "stage_id": 1,
            "stage_scene": self.stage.clone().unwrap_or_else(|| "Stage_Sim01".into()),
            "end_utc": 1_794_346_807,
            // As with real games, guests may count a few frames more than the host.
            "battle_frames": 10800 + if s.role == Role::Host { 0 } else { (seat % 4) as i64 },
            "duration_s": (10800 + if s.role == Role::Host { 0 } else { (seat % 4) as i64 }) as f64 / 60.0,
            "win_team": win_team,
            "winner": if win_team == 0 { "alpha" } else { "bravo" },
            "deemed_result": -1,
            "teams": teams,
            "players": players,
            "t0_unix": t0,
            "timeline_s": self.timeline(seat, true),
        }))
    }

    /// 1.1.7: the result file a spectator's game writes (`spectators.md`): the record the honest players' games share
    /// (under `plays`), `"outcome": "spectated"`, `"seat": "spectator"`, no entry of its own; `win_team` overrides the
    /// winner (a spectator who saw, or claims, another outcome).
    pub fn witness_result(&self, role: Role, plays: &[Play], win_team: Option<i64>) -> Value {
        let disconnected: Vec<bool> = plays.iter().map(|p| *p == Play::Crashed).collect();
        let win = win_team.unwrap_or_else(|| self.honest_win_team());
        let (teams, players) = self.record(win, &disconnected, None);
        json!({
            "version": 2, "match_id": self.game_match_id, "role": role.as_str(), "round": 1,
            "outcome": "spectated", "seat": "spectator", "seat_asked": "spectator",
            "rule": 0, "mode": self.mode, "stage_id": 1,
            "stage_scene": self.stage.clone().unwrap_or_else(|| "Stage_Sim01".into()),
            "end_utc": 1_794_346_807, "battle_frames": 10805, "duration_s": 10805.0 / 60.0,
            "win_team": win, "winner": if win == 0 { "alpha" } else { "bravo" }, "deemed_result": -1,
            "teams": teams, "players": players, "t0_unix": 1_794_346_560.0, "timeline_s": [],
        })
    }

    /// The status files `seat` writes, in order, under its behavior.
    pub fn statuses(&self, seat: usize, play: &Play) -> Vec<Value> {
        let s = &self.seats[seat];
        let n = self.seats.len();
        let pid = 30_000 + seat as u64;
        let st = |state: &str, detail: &str, since: f64, visible: bool| -> Value {
            json!({"version": 1, "match_id": self.game_match_id, "role": s.role.as_str(), "state": state,
                   "since_unix": 1_794_346_556.6 + since, "detail": detail, "visible": visible, "pid": pid})
        };
        let mut v = vec![
            st("booting", "starting the game", 0.0, false),
            st("lobby", "opening the local lobby", 7.3, false),
        ];
        if s.role == Role::Host {
            v.push(st("creating-room", "creating the session", 17.7, false));
            let mut w = st("waiting-for-players", "waiting for the players", 23.0, false);
            w["present"] = json!(n);
            w["expected"] = json!(n);
            v.push(w);
        } else {
            v.push(st("joining", "searching for the host's session", 17.6, false));
        }
        if let Play::Error(code) = play {
            let mut e = st("error", "simulated failure", 42.0, false);
            e["error"] = json!(code);
            e["reason"] = json!(format!("simulated error {code}"));
            v.push(e);
            return v;
        }
        if *play == (Play::Silent { played: false }) {
            return v;
        }
        v.push(st("in-room", "in the room", 33.3, false));
        v.push(st("loading-stage", "loading Stage_Sim01", 50.0, false));
        v.push(st("in-match", "the match is on", 53.8, true));
        if matches!(play, Play::Silent { .. } | Play::Crashed) {
            return v;
        }
        v.push(st("results", "the result screens", 251.2, true));
        v.push(st("finished", "closing the game", 281.2, false));
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kalamar_protocol::decision::{MatchSpec, Seat, decide};
    use kalamar_protocol::policy::Policy;

    fn match_view() -> Value {
        json!({"game": {"match_id": "K7Q2MXA9PL4DZB3W8R6TNY5HCF"}, "rules": {"mode": "turf", "stage": null},
               "participants": [
                 {"member_name": "Ana", "team": "alpha", "role": "host"},
                 {"member_name": "Bob", "team": "bravo", "role": "join"},
                 {"member_name": "Cleo", "team": "alpha", "role": "join"},
                 {"member_name": "Dee", "team": "bravo", "role": "join"}]})
    }

    fn spec(sim: &Sim, plays: &[Play]) -> MatchSpec {
        MatchSpec {
            game_match_id: sim.game_match_id.clone(),
            mode: sim.mode.clone(),
            stage: None,
            rated: true,
            loadout: None,
            witnesses: Vec::new(),
            seats: sim
                .seats
                .iter()
                .enumerate()
                .map(|(i, s)| Seat {
                    member_name: s.member_name.clone(),
                    team: s.team,
                    role: s.role,
                    launched: true,
                    saw_in_match: !matches!(plays[i], Play::Silent { played: false } | Play::Error(_)),
                    report: sim.result_for(i, plays),
                    reported_at: None,
                })
                .collect(),
        }
    }

    #[test]
    fn honest_reports_agree_and_validate() {
        let sim = Sim::from_match(&match_view()).unwrap();
        let plays = vec![Play::Honest; 4];
        let d = decide(&Policy::standard(), &spec(&sim, &plays)).unwrap();
        assert_eq!((d.decision, d.counted, d.agreeing), ("agreed", true, 4), "{d:?}");
        // The shape of the real files: the members the game writes.
        let r = sim.result_for(1, &plays).unwrap();
        for k in [
            "version",
            "match_id",
            "role",
            "rule",
            "mode",
            "stage_id",
            "stage_scene",
            "end_utc",
            "battle_frames",
            "duration_s",
            "win_team",
            "winner",
            "deemed_result",
            "teams",
            "players",
            "t0_unix",
            "timeline_s",
        ] {
            assert!(r.get(k).is_some(), "{k}");
        }
        assert_eq!(r["players"][1]["self"], true);
        assert_eq!(r["teams"].as_array().unwrap().len(), 3);
        assert_eq!(sim.statuses(0, &Play::Honest).last().unwrap()["state"], "finished");
        assert_eq!(
            sim.statuses(1, &Play::Error("room-not-found".into())).last().unwrap()["error"],
            "room-not-found"
        );
    }

    #[test]
    fn misbehavior_is_detected() {
        let sim = Sim::from_match(&match_view()).unwrap();
        let mut plays = vec![Play::Honest; 4];
        plays[1] = Play::Cheat;
        let d = decide(&Policy::standard(), &spec(&sim, &plays)).unwrap();
        assert!(matches!(d.decision, "majority" | "agreed"), "{d:?}");
        plays[1] = Play::WrongSelf;
        let d = decide(&Policy::standard(), &spec(&sim, &plays)).unwrap();
        assert_eq!(d.invalid, 1);
        plays[1] = Play::Crashed;
        let d = decide(&Policy::standard(), &spec(&sim, &plays)).unwrap();
        assert!(d.counted, "{d:?}");
    }

    #[test]
    fn simulated_records_never_contradict_their_winner() {
        // Check V11 (1.1.8): whatever the seed, the honest record and a cheater's are coherent records of `rule` 0.
        for n in 0..64 {
            let mut view = match_view();
            view["game"]["match_id"] = json!(format!("M{n:02}"));
            let sim = Sim::from_match(&view).unwrap();
            let mut plays = vec![Play::Honest; 4];
            let d = decide(&Policy::standard(), &spec(&sim, &plays)).unwrap();
            assert_eq!((d.decision, d.invalid), ("agreed", 0), "{n}: {d:?}");
            plays[2] = Play::Cheat;
            assert_eq!(
                decide(&Policy::standard(), &spec(&sim, &plays)).unwrap().invalid,
                0,
                "{n}"
            );
        }
    }

    #[test]
    fn a_game_that_computes_its_own_turf_share_still_agrees() {
        use kalamar_protocol::policy::StatsCheck;
        let sim = Sim::from_match(&match_view()).unwrap();
        let mut plays = vec![Play::Honest; 4];
        plays[1] = Play::OwnTurf(6);
        let d = decide(&Policy::standard(), &spec(&sim, &plays)).unwrap();
        assert_eq!(
            (d.decision, d.agreeing, d.stats.differ, d.stats.flagged),
            ("agreed", 4, true, false),
            "{d:?}"
        );
        let strict = Policy {
            stats: StatsCheck::Strict,
            ..Policy::standard()
        };
        assert_eq!(decide(&strict, &spec(&sim, &plays)).unwrap().decision, "majority");
        plays[1] = Play::OwnTurf(400);
        let d = decide(&Policy::standard(), &spec(&sim, &plays)).unwrap();
        assert_eq!((d.decision, d.stats.flagged), ("agreed", true), "{d:?}");
        assert_ne!(d.stats.record_seat, Some(1), "the report that strays is not the record");
    }
}
