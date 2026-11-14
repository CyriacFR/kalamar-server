//! The conformance suite (`writing-your-own-server.md`, "Conformance areas"): documented scenarios run against any
//! server URL, with expectations derived from what the server declares (its areas, room form, queues, result
//! policies). Expected match decisions come from the normative procedure (`kalamar_protocol::decision`).

use std::collections::HashSet;
use std::time::Duration;

use kalamar_protocol::contract::{is_valid_game_match_id, is_valid_member_name, is_valid_relay_room, parse_rules};
use kalamar_protocol::decision::{Decision, MatchSpec, Seat, Witness, decide};
use kalamar_protocol::policy::{Leave, Policy};
use kalamar_protocol::{Role, Team};
use serde_json::{Value, json};

use crate::client::{Api, Events, Resp, WsAuth};
use crate::sim::{Play, Sim};

/// Every area, in report order.
pub const AREAS: &[&str] = &[
    "core",
    "rooms",
    "queues",
    "parties",
    "friends",
    "presence",
    "ratings",
    "leaderboards",
    "stats",
    "moderation",
    "activities",
    "series",
    "badges",
    "profiles",
    "population",
    "draft",
    "spectators",
];

#[derive(Debug, Clone)]
pub struct Options {
    pub api: String,
    pub discovery: Option<String>,
    pub areas: Option<Vec<String>>,
    pub provider: Option<String>,
    pub tokens: Vec<String>,
    pub max_wait: u64,
    pub verbose: bool,
    /// 1.1.9: a command that makes the server read its configuration again (`core.reload`); none: skipped.
    pub reload_command: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Status {
    Pass,
    Fail(String),
    Skip(String),
}

#[derive(Debug, Clone)]
pub struct Outcome {
    pub id: String,
    pub area: String,
    pub status: Status,
}

#[derive(Debug, Clone, Default)]
pub struct Report {
    pub results: Vec<Outcome>,
    pub declared: Vec<String>,
}

impl Report {
    /// Areas that count: the core and every declared area.
    fn counts(&self, area: &str) -> bool {
        area == "core" || self.declared.iter().any(|d| d == area)
    }

    /// Core and every declared area passed (skips do not fail).
    pub fn passed(&self) -> bool {
        !self
            .results
            .iter()
            .any(|o| self.counts(&o.area) && matches!(o.status, Status::Fail(_)))
    }

    pub fn failures(&self) -> Vec<&Outcome> {
        self.results
            .iter()
            .filter(|o| matches!(o.status, Status::Fail(_)))
            .collect()
    }

    pub fn table(&self) -> String {
        let mut out = String::new();
        for o in &self.results {
            let (s, why) = match &o.status {
                Status::Pass => ("pass", String::new()),
                Status::Fail(w) => ("FAIL", format!("  {w}")),
                Status::Skip(w) => ("skip", format!("  {w}")),
            };
            out.push_str(&format!("{s:4}  {:28}{why}\n", o.id));
        }
        out.push_str("\narea           declared  pass  fail  skip  verdict\n");
        for a in AREAS {
            let rs: Vec<&Outcome> = self.results.iter().filter(|o| o.area == *a).collect();
            let n = |f: fn(&Status) -> bool| rs.iter().filter(|o| f(&o.status)).count();
            let (p, f, s) = (
                n(|s| *s == Status::Pass),
                n(|s| matches!(s, Status::Fail(_))),
                n(|s| matches!(s, Status::Skip(_))),
            );
            let declared = self.counts(a);
            let verdict = if !declared {
                "not declared"
            } else if f > 0 {
                "NOT CONFORMANT"
            } else if p == 0 {
                "untested"
            } else {
                "conformant"
            };
            out.push_str(&format!(
                "{a:14} {:8}  {p:4}  {f:4}  {s:4}  {verdict}\n",
                if *a == "core" {
                    "always"
                } else if declared {
                    "yes"
                } else {
                    "no"
                }
            ));
        }
        out.push_str(&format!(
            "\nserver: {}\n",
            if self.passed() {
                "CONFORMANT (core and every declared area)"
            } else {
                "NOT CONFORMANT"
            }
        ));
        out
    }

    pub fn to_json(&self) -> Value {
        json!({
            "conformant": self.passed(),
            "declared": self.declared,
            "results": self.results.iter().map(|o| {
                let (s, why) = match &o.status { Status::Pass => ("pass", None), Status::Fail(w) => ("fail", Some(w.clone())), Status::Skip(w) => ("skip", Some(w.clone())) };
                json!({"id": o.id, "area": o.area, "status": s, "reason": why})
            }).collect::<Vec<_>>(),
        })
    }
}

/// Why a scenario did not pass.
pub enum Why {
    Fail(String),
    Skip(String),
}

type R = Result<(), Why>;

/// What each seat does in a scenario match.
type Plan = Box<dyn Fn(usize, &Sim) -> Play>;

fn fail<T>(m: impl Into<String>) -> Result<T, Why> {
    Err(Why::Fail(m.into()))
}
fn skip<T>(m: impl Into<String>) -> Result<T, Why> {
    Err(Why::Skip(m.into()))
}

macro_rules! check {
    ($cond:expr, $($fmt:tt)+) => {
        if !$cond {
            return Err(Why::Fail(format!($($fmt)+)));
        }
    };
}

fn expect(r: &Resp, status: u16, what: &str) -> R {
    if r.status == status {
        Ok(())
    } else {
        fail(format!(
            "{what}: expected {status}, got {} {}",
            r.status,
            compact(&r.body)
        ))
    }
}

fn expect_ok(r: &Resp, what: &str) -> R {
    if r.ok() {
        Ok(())
    } else {
        fail(format!("{what}: expected 2xx, got {} {}", r.status, compact(&r.body)))
    }
}

fn expect_code(r: &Resp, status: u16, code: &str, what: &str) -> R {
    if r.status == status && r.code() == Some(code) && r.has_error_shape() {
        Ok(())
    } else {
        fail(format!(
            "{what}: expected {status} {code}, got {} {}",
            r.status,
            compact(&r.body)
        ))
    }
}

fn compact(v: &Value) -> String {
    let s = v.to_string();
    if s.len() > 300 { format!("{}...", &s[..300]) } else { s }
}

pub struct Player {
    pub name: String,
    pub id: String,
    pub api: Api,
    pub refresh: String,
    pub events: Option<Events>,
}

enum Login {
    Direct { provider: String, field: String },
    Tokens,
}

/// Where scenario matches come from.
#[derive(Clone)]
enum Source {
    Room,
    Queue(String),
}

struct Ctx {
    opts: Options,
    anon: Api,
    meta: Value,
    features: HashSet<String>,
    test_clock: bool,
    run: String,
    counter: usize,
    login: Login,
    tokens: Vec<String>,
    pool: Vec<Player>,
    results: Vec<Outcome>,
    /// Ids of a completed match and its players, for the stats and ratings scenarios.
    finished: Option<(Value, Vec<String>)>,
    rated: Option<(Value, Vec<String>)>,
    /// Series met while playing activities (1.1).
    series_seen: Vec<String>,
}

const WAIT: Duration = Duration::from_secs(12);

impl Ctx {
    fn declared(&self, area: &str) -> bool {
        self.features.contains(area)
    }

    fn wanted(&self, area: &str) -> bool {
        self.opts.areas.as_ref().is_none_or(|a| a.iter().any(|x| x == area))
    }

    fn record(&mut self, id: &str, area: &str, r: R) {
        let status = match r {
            Ok(()) => Status::Pass,
            Err(Why::Fail(m)) => Status::Fail(m),
            Err(Why::Skip(m)) => Status::Skip(m),
        };
        if self.opts.verbose {
            eprintln!("{id}: {status:?}");
        }
        self.results.push(Outcome {
            id: id.into(),
            area: area.into(),
            status,
        });
    }

    /// Runs time-based transitions now (test clock) or waits a little.
    async fn kick(&self) {
        if self.test_clock {
            let _ = self.anon.post("/v1/_test/clock", json!({"advance_s": 0})).await;
        } else {
            tokio::time::sleep(Duration::from_millis(1200)).await;
        }
    }

    /// Moves time forward by `secs`: the test clock, or real waiting when it is at most `--max-wait`.
    async fn advance(&self, secs: u64) -> Result<(), Why> {
        if self.test_clock {
            let r = self.anon.post("/v1/_test/clock", json!({"advance_s": secs})).await;
            expect_ok(&r, "POST /v1/_test/clock")
        } else if secs <= self.opts.max_wait {
            tokio::time::sleep(Duration::from_secs(secs + 2)).await;
            Ok(())
        } else {
            skip(format!(
                "needs {secs} s to pass (no test-clock; --max-wait {})",
                self.opts.max_wait
            ))
        }
    }

    fn display_max(&self) -> usize {
        self.meta["limits"]["display_name"]["max_len"].as_u64().unwrap_or(10) as usize
    }

    /// A new account (a fresh login, or the next token of `--tokens`).
    async fn new_player(&mut self) -> Result<Player, Why> {
        self.counter += 1;
        let name: String = format!("k{}{}", self.run, self.counter)
            .chars()
            .take(self.display_max().max(1))
            .collect();
        let (token, refresh) = match &self.login {
            Login::Direct { provider, field } => {
                let r = self
                    .anon
                    .post(&format!("/v1/auth/{provider}/login"), json!({ field.as_str(): name }))
                    .await;
                expect_ok(&r, &format!("login with {provider}"))?;
                (
                    r.body["access_token"].as_str().unwrap_or_default().to_string(),
                    r.body["refresh_token"].as_str().unwrap_or_default().to_string(),
                )
            }
            Login::Tokens => match self.tokens.pop() {
                Some(t) => (t, String::new()),
                None => return skip("not enough --tokens for this scenario"),
            },
        };
        let api = self.anon.with_token(&token);
        let me = api.get("/v1/me").await;
        expect(&me, 200, "GET /v1/me")?;
        let id = me.body["id"].as_str().unwrap_or_default().to_string();
        check!(!id.is_empty(), "GET /v1/me has no id");
        let name = me.body["display_name"].as_str().unwrap_or_default().to_string();
        let _ = api.put("/v1/me/game", json!({"contract": {"id": kalamar_protocol::CONTRACT_ID, "version": kalamar_protocol::CONTRACT_VERSION}, "build": "testkit",
            // The simulated game applies imposed loadouts (it writes the required weapon and gear).
            "engine": {"match_loadout": true, "loadout": ["weapon", "gear"]}})).await;
        Ok(Player {
            name,
            id,
            api,
            refresh,
            events: None,
        })
    }

    /// Makes sure the pool has `n` players with connected event streams; returns their indices.
    async fn pool(&mut self, n: usize) -> Result<Vec<usize>, Why> {
        while self.pool.len() < n {
            let p = self.new_player().await?;
            self.pool.push(p);
        }
        for i in 0..n {
            if self.pool[i].events.is_none() {
                let token = self.pool[i].api.token.clone().unwrap_or_default();
                let ev = Events::connect(&self.opts.api, WsAuth::Bearer(&token), None)
                    .await
                    .map_err(|e| Why::Fail(format!("event stream: {e}")))?;
                self.pool[i].events = Some(ev);
            }
        }
        Ok((0..n).collect())
    }

    /// Leaves rooms, parties and queues and forgets pending events (between scenarios).
    async fn reset(&mut self) {
        for p in &mut self.pool {
            if self.features.contains("rooms") {
                let r = p.api.get("/v1/rooms/current").await;
                if let Some(id) = r.body["room"]["id"].as_str() {
                    let _ = p.api.post_empty(&format!("/v1/rooms/{id}/leave")).await;
                }
            }
            if self.features.contains("parties") {
                let _ = p.api.post_empty("/v1/party/leave").await;
            }
            if self.features.contains("queues") {
                let _ = p.api.delete("/v1/tickets/current").await;
            }
            if let Some(ev) = &mut p.events {
                ev.drain(Duration::from_millis(50)).await;
            }
        }
    }

    fn policy(&self, id: &str) -> Option<Policy> {
        let p = self.meta["result_policies"]
            .as_array()?
            .iter()
            .find(|p| p["id"] == id)?;
        serde_json::from_value(p.clone()).ok()
    }

    /// The source of core match scenarios: rooms if declared, else the smallest eligible solo queue.
    async fn source(&mut self) -> Result<(Source, usize), Why> {
        if self.declared("rooms") {
            return Ok((Source::Room, 2));
        }
        if self.declared("queues") {
            let idx = self.pool(1).await?;
            let r = self.pool[idx[0]].api.get("/v1/queues").await;
            expect(&r, 200, "GET /v1/queues")?;
            let mut best: Option<(usize, String)> = None;
            for q in r.body["items"].as_array().cloned().unwrap_or_default() {
                let n = (q["teams"][0].as_u64().unwrap_or(0) + q["teams"][1].as_u64().unwrap_or(0)) as usize;
                if q["eligible"] == true
                    && q["party"]["min"].as_u64().unwrap_or(1) == 1
                    && n >= 2
                    && best.as_ref().is_none_or(|b| n < b.0)
                {
                    best = Some((n, q["id"].as_str().unwrap_or_default().to_string()));
                }
            }
            if let Some((n, id)) = best {
                return Ok((Source::Queue(id), n));
            }
            return skip("no eligible solo queue to create matches");
        }
        skip("neither rooms nor queues declared")
    }

    /// Creates a match with the pool players `who` and returns the participant views in `who` order.
    async fn make_match(&mut self, who: &[usize], source: &Source) -> Result<Vec<Value>, Why> {
        match source {
            Source::Room => {
                let decl = self.meta["rooms"].clone();
                let n = who.len();
                let choices: Vec<[u64; 2]> = decl["options"]
                    .as_array()
                    .and_then(|o| o.iter().find(|f| f["key"] == "teams"))
                    .and_then(|f| serde_json::from_value(f["choices"].clone()).ok())
                    .unwrap_or_default();
                let mut options = json!({});
                if let Some(c) = choices
                    .iter()
                    .filter(|c| (c[0] + c[1]) as usize >= n)
                    .min_by_key(|c| c[0] + c[1])
                {
                    options["teams"] = json!(c);
                }
                let owner = &self.pool[who[0]];
                let r = owner.api.post("/v1/rooms", json!({"options": options})).await;
                expect(&r, 201, "POST /v1/rooms")?;
                let room = r.body["id"].as_str().unwrap_or_default().to_string();
                let code = r.body["code"].as_str().unwrap_or_default().to_lowercase();
                for &i in &who[1..] {
                    let r = self.pool[i].api.post("/v1/rooms/join", json!({"code": code})).await;
                    expect(&r, 200, "POST /v1/rooms/join")?;
                    let r = self.pool[i]
                        .api
                        .post(&format!("/v1/rooms/{room}/ready"), json!({"ready": true}))
                        .await;
                    expect(&r, 200, "POST /v1/rooms/{id}/ready")?;
                }
                let r = self.pool[who[0]]
                    .api
                    .post_empty(&format!("/v1/rooms/{room}/start"))
                    .await;
                expect(&r, 201, "POST /v1/rooms/{id}/start")?;
                let mid = r.body["id"].as_str().unwrap_or_default().to_string();
                self.wait_all(who, "match.ready", &mid).await
            }
            Source::Queue(q) => {
                for &i in who {
                    let r = self.pool[i]
                        .api
                        .post(&format!("/v1/queues/{q}/tickets"), json!({}))
                        .await;
                    expect(&r, 201, "POST /v1/queues/{id}/tickets")?;
                }
                let found = self.wait_found(who).await?;
                let mid = found[0]["id"].as_str().unwrap_or_default().to_string();
                for &i in who {
                    let r = self.pool[i].api.post_empty(&format!("/v1/matches/{mid}/accept")).await;
                    expect(&r, 200, "POST /v1/matches/{id}/accept")?;
                }
                self.wait_all(who, "match.ready", &mid).await
            }
        }
    }

    /// Waits for `match.found` at every player of `who` (kicking matchmaking), all for the same match.
    async fn wait_found(&mut self, who: &[usize]) -> Result<Vec<Value>, Why> {
        let mut out = Vec::new();
        for (k, &i) in who.iter().enumerate() {
            let mut got = None;
            for attempt in 0..12 {
                // Matchmaking may need waiting time (rating windows widen with it): with the test clock, move
                // time forward after the first tries.
                if k == 0 {
                    if self.test_clock && attempt >= 2 {
                        let _ = self.anon.post("/v1/_test/clock", json!({"advance_s": 30})).await;
                    } else {
                        self.kick().await;
                    }
                }
                let ev = self.pool[i].events.as_mut().expect("connected");
                if let Some(e) = ev.wait("match.found", |_| true, Duration::from_millis(1000)).await {
                    got = Some(e["data"].clone());
                    break;
                }
            }
            let Some(m) = got else {
                return fail("no match.found event");
            };
            out.push(m);
        }
        let ids: HashSet<&str> = out.iter().filter_map(|m| m["id"].as_str()).collect();
        check!(ids.len() == 1, "players were matched into different matches");
        Ok(out)
    }

    async fn wait_all(&mut self, who: &[usize], kind: &str, mid: &str) -> Result<Vec<Value>, Why> {
        let mut out = Vec::new();
        for &i in who {
            let ev = self.pool[i].events.as_mut().expect("connected");
            match ev.wait(kind, |e| e["data"]["id"] == mid, WAIT).await {
                Some(e) => out.push(e["data"].clone()),
                None => return fail(format!("no {kind} event for match {mid}")),
            }
        }
        Ok(out)
    }

    /// Plays a ready match: launch, statuses, results as `plays` (by seat) say; then waits for the decision and
    /// compares it with the normative procedure under the declared policy.
    async fn play(
        &mut self,
        who: &[usize],
        views: &[Value],
        plays_by_seat: &dyn Fn(usize, &Sim) -> Play,
    ) -> Result<(Value, Option<Decision>), Why> {
        let view = &views[0];
        let mid = view["id"].as_str().unwrap_or_default().to_string();
        let sim = Sim::from_match(view)
            .ok_or_else(|| Why::Fail(format!("match view without a usable game block: {}", compact(view))))?;
        let plays: Vec<Play> = (0..sim.seats.len()).map(|s| plays_by_seat(s, &sim)).collect();
        let seat_of = |v: &Value| sim.seat_of(v["you"]["member_name"].as_str().unwrap_or_default());
        for (k, &i) in who.iter().enumerate() {
            let seat = seat_of(&views[k]).ok_or_else(|| Why::Fail("you.member_name is not a participant".into()))?;
            let api = self.pool[i].api.clone();
            let r = api.post(&format!("/v1/matches/{mid}/launch"), json!({})).await;
            expect(&r, 200, "POST /v1/matches/{id}/launch")?;
            for st in sim.statuses(seat, &plays[seat]) {
                let r = api.post(&format!("/v1/matches/{mid}/status"), st).await;
                expect(&r, 204, "POST /v1/matches/{id}/status")?;
            }
        }
        let mut uploaded = 0;
        for (k, &i) in who.iter().enumerate() {
            let seat = seat_of(&views[k]).unwrap_or(0);
            if let Some(res) = sim.result_for(seat, &plays) {
                let api = self.pool[i].api.clone();
                let r = api.post(&format!("/v1/matches/{mid}/result"), res.clone()).await;
                expect(&r, 200, "POST /v1/matches/{id}/result")?;
                if uploaded == 0 && who.len() > 1 {
                    // A repeat of the same report is accepted; a different one is refused.
                    let again = api.post(&format!("/v1/matches/{mid}/result"), res.clone()).await;
                    if again.status != 409 || again.code() != Some("match_state") {
                        expect(&again, 200, "identical result upload repeated")?;
                        let mut other = res.clone();
                        other["t0_unix"] = json!(1.0);
                        let r = api.post(&format!("/v1/matches/{mid}/result"), other).await;
                        expect_code(&r, 409, "already_reported", "a different second result")?;
                    }
                }
                uploaded += 1;
            }
        }
        // The decision.
        let policy = self.policy(view["rules"]["result_policy"].as_str().unwrap_or_default());
        let mut decided = None;
        let ev_idx = who[0];
        for attempt in 0..3 {
            let ev = self.pool[ev_idx].events.as_mut().expect("connected");
            let e = ev
                .wait_any(
                    &["match.completed", "match.void"],
                    |e| e["data"]["id"] == mid,
                    Duration::from_secs(4),
                )
                .await;
            if let Some(e) = e {
                decided = Some(e["data"].clone());
                break;
            }
            if attempt < 2 {
                let grace = policy.as_ref().map(|p| p.deadlines.grace_s as u64).unwrap_or(120);
                let result = policy.as_ref().map(|p| p.deadlines.result_s as u64).unwrap_or(1800);
                self.advance(if attempt == 0 { grace + 1 } else { result + 1 }).await?;
            }
        }
        let Some(m) = decided else {
            return fail("the match was never decided (no match.completed or match.void)");
        };
        let expected = policy.filter(Policy::is_known).and_then(|p| {
            let spec = MatchSpec {
                game_match_id: sim.game_match_id.clone(),
                mode: sim.mode.clone(),
                stage: sim.stage.clone(),
                rated: view["rules"]["rated"] == true,
                loadout: view["rules"].get("loadout").cloned().filter(|v| !v.is_null()),
                witnesses: Vec::new(),
                seats: sim
                    .seats
                    .iter()
                    .enumerate()
                    .map(|(s, x)| Seat {
                        member_name: x.member_name.clone(),
                        team: x.team,
                        role: x.role,
                        launched: true,
                        saw_in_match: !matches!(plays[s], Play::Silent { played: false } | Play::Error(_)),
                        report: sim.result_for(s, &plays),
                        reported_at: None,
                    })
                    .collect(),
            };
            decide(&p, &spec)
        });
        if let Some(d) = &expected {
            let o = &m["outcome"];
            let state = if d.counted { "completed" } else { "void" };
            check!(
                m["state"] == state,
                "state {} where the declared policy gives {state} ({})",
                m["state"],
                d.decision
            );
            check!(
                o["decision"] == d.decision,
                "decision {} where the declared policy gives {}",
                o["decision"],
                d.decision
            );
            check!(
                o["counted"] == d.counted,
                "outcome.counted {} expected {}",
                o["counted"],
                d.counted
            );
            check!(
                o["winner"] == json!(d.winner.map(Team::as_str)),
                "winner {} expected {:?}",
                o["winner"],
                d.winner
            );
            check!(o["rated"] == d.rated, "rated {} expected {}", o["rated"], d.rated);
            check!(o["summary"].is_object(), "outcome.summary is not localized text");
            if d.counted {
                check!(
                    o["record"]["players"]
                        .as_array()
                        .is_some_and(|p| p.len() == sim.seats.len()),
                    "outcome.record.players"
                );
            }
        }
        Ok((m, expected))
    }
}

// ---------------------------------------------------------------- core

async fn core_discovery(c: &mut Ctx) -> R {
    let origin = match &c.opts.discovery {
        Some(h) if h.starts_with("http") => h.trim_end_matches('/').to_string(),
        Some(h) => format!("https://{}", h.trim_end_matches('/')),
        None => {
            let u = &c.opts.api;
            let scheme_end = u.find("://").map(|i| i + 3).unwrap_or(0);
            match u[scheme_end..].find('/') {
                Some(i) => u[..scheme_end + i].to_string(),
                None => u.clone(),
            }
        }
    };
    let api = Api::new(&origin);
    let r = api.get("/.well-known/kalamar-server.json").await;
    if r.status == 404 && c.opts.discovery.is_none() {
        return skip("no discovery document at the API's origin (give --discovery)");
    }
    expect(&r, 200, "GET /.well-known/kalamar-server.json")?;
    let d = &r.body;
    check!(d["kalamar_server"] == 1, "kalamar_server is not 1");
    check!(
        d["name"]
            .as_str()
            .is_some_and(|n| !n.is_empty() && n.chars().count() <= 64),
        "name"
    );
    check!(
        d["api"].as_str().is_some_and(|a| a.starts_with("https://")
            || a.starts_with("http://127.0.0.1")
            || a.starts_with("http://localhost")
            || a.starts_with("http://[::1]")),
        "api is not an https URL"
    );
    check!(d["relay"].is_string() || d["relay"].is_null(), "relay");
    check!(
        d["protocol"]["versions"]
            .as_array()
            .is_some_and(|v| v.contains(&json!("1.0"))),
        "protocol.versions lacks 1.0"
    );
    check!(
        d["operator"]["name"].is_string() && d["operator"]["contact"].is_string(),
        "operator"
    );
    check!(d["privacy_url"].is_string(), "privacy_url");
    let f1: HashSet<String> = d["features"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(str::to_string))
        .collect();
    check!(f1 == c.features, "features differ from GET /v1/meta");
    Ok(())
}

async fn core_meta(c: &mut Ctx) -> R {
    let m = &c.meta;
    check!(m["name"].is_string(), "name");
    check!(
        m["protocol"]["versions"]
            .as_array()
            .is_some_and(|v| v.contains(&json!("1.0"))),
        "protocol.versions lacks 1.0"
    );
    check!(
        m["time"].as_str().is_some_and(|t| t.ends_with('Z') && t.len() >= 20),
        "time is not RFC 3339 UTC"
    );
    check!(m["features"].is_array(), "features");
    check!(
        m["operator"]["name"].is_string() && m["operator"]["contact"].is_string(),
        "operator"
    );
    check!(m["privacy_url"].is_string(), "privacy_url");
    let gc = &m["game_contract"];
    check!(
        gc["id"] == kalamar_protocol::CONTRACT_ID && gc["versions"].as_array().is_some_and(|v| v.contains(&json!(1))),
        "game_contract lacks lan-auto-match 1"
    );
    let l = &m["limits"];
    check!(
        l["body_bytes"].as_u64().unwrap_or(0) >= 16384
            && l["result_bytes"].as_u64().unwrap_or(0) >= 65536
            && l["status_bytes"].as_u64().unwrap_or(0) >= 4096,
        "limits below the protocol minimums"
    );
    check!(
        l["display_name"]["min_len"].is_u64() && l["display_name"]["max_len"].is_u64(),
        "limits.display_name"
    );
    let providers = m["auth"]["providers"].as_array().cloned().unwrap_or_default();
    check!(!providers.is_empty(), "auth.providers is empty");
    for p in &providers {
        check!(
            p["id"].is_string() && p["kind"].is_string() && p["label"].is_object(),
            "provider shape: {}",
            compact(p)
        );
        if p["kind"] == "oauth_loopback" {
            check!(
                p["loopback_redirects"].as_array().is_some_and(|r| !r.is_empty()),
                "oauth provider without loopback_redirects"
            );
        }
    }
    check!(
        m["rules"]["summary"].is_object() && (m["rules"]["url"].is_string() || m["rules"]["url"].is_null()),
        "rules block"
    );
    let pols = m["result_policies"].as_array().cloned().unwrap_or_default();
    check!(!pols.is_empty(), "result_policies is empty");
    for p in &pols {
        check!(
            p["id"].is_string() && p["summary"].is_object() && p["deadlines"]["launch_s"].is_u64(),
            "result policy shape: {}",
            compact(p)
        );
        // 1.1.8: what the reports' counters weigh (absent: `strict`).
        if let Some(s) = p.get("stats") {
            check!(
                matches!(s.as_str(), Some("flag" | "ignore" | "strict")),
                "result policy {} has stats {s}",
                p["id"]
            );
        }
    }
    check!(
        c.declared("rooms") == m["rooms"].is_object(),
        "rooms declaration present iff rooms is declared"
    );
    check!(!c.declared("queues") || m["queues"]["count"].is_u64(), "queues.count");
    check!(
        !c.declared("moderation")
            || m["moderation"]["report_categories"]
                .as_array()
                .is_some_and(|a| !a.is_empty()),
        "moderation.report_categories"
    );
    if c.declared("presence") {
        check!(c.declared("friends"), "presence declared without friends");
    }
    if c.declared("leaderboards") {
        check!(c.declared("ratings"), "leaderboards declared without ratings");
    }
    Ok(())
}

async fn core_errors(c: &mut Ctx) -> R {
    let r = c.anon.get("/v1/me").await;
    expect_code(&r, 401, "unauthorized", "GET /v1/me without a token")?;
    let bad = c.anon.with_token("kat1.not-a-real-token");
    expect_code(
        &bad.get("/v1/me").await,
        401,
        "unauthorized",
        "GET /v1/me with an unknown token",
    )?;
    let r = c
        .anon
        .call_raw("POST", "/v1/auth/refresh", Some(b"{not json".to_vec()))
        .await;
    expect_code(&r, 400, "invalid_json", "POST /v1/auth/refresh with malformed JSON")?;
    let r = c.anon.get("/v1/no-such-endpoint").await;
    check!(
        r.status == 404 && r.has_error_shape(),
        "unknown endpoint: expected 404 with the error shape, got {}",
        r.status
    );
    let idx = c.pool(1).await?;
    let p = &c.pool[idx[0]];
    let limit = c.meta["limits"]["body_bytes"].as_u64().unwrap_or(16384) as usize;
    let big = json!({"x-padding": "a".repeat(limit + 1024)});
    let r = p.api.patch("/v1/me", big).await;
    expect_code(&r, 413, "payload_too_large", "PATCH /v1/me above limits.body_bytes")?;
    let r = p.api.call_raw("PATCH", "/v1/me", Some(b"[1, 2]".to_vec())).await;
    check!(
        r.status == 400 || r.status == 422,
        "PATCH /v1/me with an array: expected 400 or 422, got {}",
        r.status
    );
    if !c.declared("population") {
        // 1.1.4: an undeclared population answers 404 like an unknown path.
        let r = c.anon.get("/v1/population").await;
        check!(
            r.status == 404,
            "GET /v1/population on a server without the area: expected 404, got {}",
            r.status
        );
    }
    for (area, path) in [
        ("friends", "/v1/friends"),
        ("queues", "/v1/queues"),
        ("ratings", "/v1/ladders"),
        ("parties", "/v1/party"),
        ("rooms", "/v1/rooms/current"),
    ] {
        if !c.declared(area) {
            let r = c.pool[idx[0]].api.get(path).await;
            expect_code(
                &r,
                404,
                "feature_disabled",
                &format!("GET {path} on an undeclared area"),
            )?;
        }
    }
    Ok(())
}

async fn core_login(c: &mut Ctx) -> R {
    if matches!(c.login, Login::Tokens) {
        return skip("accounts come from --tokens");
    }
    let p = c.new_player().await?;
    let me = p.api.get("/v1/me").await;
    let b = &me.body;
    check!(
        b["display_name"].is_string()
            && b["created_at"].is_string()
            && b["logins"].is_array()
            && b["settings"].is_object()
            && b["restrictions"].is_array(),
        "Me shape: {}",
        compact(b)
    );
    // Refresh: a new pair; the old access token stops working.
    let r = c
        .anon
        .post("/v1/auth/refresh", json!({"refresh_token": p.refresh}))
        .await;
    expect(&r, 200, "POST /v1/auth/refresh")?;
    let access2 = r.body["access_token"].as_str().unwrap_or_default().to_string();
    check!(
        r.body["refresh_token"].is_string() && r.body["access_expires_at"].is_string(),
        "session shape"
    );
    expect_code(
        &p.api.get("/v1/me").await,
        401,
        "unauthorized",
        "the old access token after a refresh",
    )?;
    let api2 = c.anon.with_token(&access2);
    expect(&api2.get("/v1/me").await, 200, "the new access token")?;
    // The rotated refresh token still works within 60 s (a lost answer does not lock the player out).
    let again = c
        .anon
        .post("/v1/auth/refresh", json!({"refresh_token": p.refresh}))
        .await;
    expect(&again, 200, "the previous refresh token within its 60 s window")?;
    let api3 = c
        .anon
        .with_token(again.body["access_token"].as_str().unwrap_or_default());
    // Logout ends the session.
    expect(
        &api3.post("/v1/auth/logout", json!({})).await,
        204,
        "POST /v1/auth/logout",
    )?;
    expect_code(&api3.get("/v1/me").await, 401, "unauthorized", "a token after logout")?;
    Ok(())
}

async fn core_account(c: &mut Ctx) -> R {
    let p = c.new_player().await?;
    let max = c.display_max();
    let new_name: String = "Renamed".chars().take(max).collect();
    let r = p.api.patch("/v1/me", json!({"display_name": new_name})).await;
    expect(&r, 200, "PATCH /v1/me display_name")?;
    check!(
        r.body["display_name"] == new_name.as_str(),
        "the new display name is not returned"
    );
    let r = p
        .api
        .patch("/v1/me", json!({"display_name": "x".repeat(max + 1)}))
        .await;
    check!(
        r.status == 422 && r.code() == Some("validation_failed") && r.body["error"]["field"] == "/display_name",
        "a too long name: expected 422 validation_failed /display_name, got {} {}",
        r.status,
        compact(&r.body)
    );
    let settings = p.api.get("/v1/me").await.body["settings"].clone();
    if let Some((k, v)) = settings
        .as_object()
        .and_then(|m| m.iter().next().map(|(k, v)| (k.clone(), v.clone())))
    {
        let flipped = !v.as_bool().unwrap_or(true);
        let r = p.api.patch("/v1/me", json!({"settings": {k.clone(): flipped}})).await;
        expect(&r, 200, "PATCH /v1/me settings")?;
        check!(r.body["settings"][&k] == flipped, "setting {k} did not change");
    }
    let r = p.api.put("/v1/me/game", json!({"contract": {"id": "lan-auto-match", "version": 1}, "build": "x", "modes": [], "can_select_stage": false, "max_players": 8, "x-extra": true})).await;
    expect(&r, 204, "PUT /v1/me/game")?;
    let r = p.api.get("/v1/state").await;
    check!(r.status == 200 && r.body["me"]["id"] == p.id.as_str(), "GET /v1/state");
    let r = p.api.get(&format!("/v1/users/{}", p.id)).await;
    check!(
        r.status == 200 && r.body["display_name"].is_string(),
        "GET /v1/users/{{id}}"
    );
    Ok(())
}

async fn core_events(c: &mut Ctx) -> R {
    let p = c.new_player().await?;
    let token = p.api.token.clone().unwrap_or_default();
    let mut ev = Events::connect(&c.opts.api, WsAuth::Bearer(&token), None)
        .await
        .map_err(|e| Why::Fail(format!("connect: {e}")))?;
    let h = ev.hello.clone();
    check!(
        h["stream"].is_string() && h["resumed"] == false && h["heartbeat_s"].is_u64() && h["resume_window_s"].is_u64(),
        "hello shape: {}",
        compact(&h)
    );
    ev.send(json!({"type": "ping", "t": 7}));
    check!(
        ev.wait("pong", |e| e["t"] == 7, WAIT).await.is_some(),
        "no pong to a ping"
    );
    // Ticket authentication: single use.
    let t = p.api.post_empty("/v1/events/ticket").await;
    check!(
        t.ok() && t.body["ticket"].is_string(),
        "POST /v1/events/ticket: {} {}",
        t.status,
        compact(&t.body)
    );
    let ticket = t.body["ticket"].as_str().unwrap_or_default().to_string();
    let ev2 = Events::connect(&c.opts.api, WsAuth::Ticket(&ticket), None)
        .await
        .map_err(|e| Why::Fail(format!("ticket connect: {e}")))?;
    ev2.close().await;
    check!(
        Events::connect(&c.opts.api, WsAuth::Ticket(&ticket), None)
            .await
            .is_err(),
        "a ticket was accepted twice"
    );
    // Resumption: an event while disconnected is replayed.
    let generate = |api: Api, n: u32| async move {
        let s = api.get("/v1/me").await.body["settings"].clone();
        if let Some(k) = s.as_object().and_then(|m| m.keys().next().cloned()) {
            api.patch("/v1/me", json!({"settings": {k: n.is_multiple_of(2)}})).await
        } else {
            api.patch("/v1/me", json!({})).await
        }
    };
    generate(p.api.clone(), 1).await;
    let Some(first) = ev.wait("me.updated", |_| true, WAIT).await else {
        return skip("the server sent no me.updated event to generate events with");
    };
    let seq = first["seq"].as_u64().unwrap_or(0);
    check!(seq >= 1, "events carry no seq");
    let stream = ev.stream_id();
    ev.close().await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    generate(p.api.clone(), 2).await;
    let mut ev3 = Events::connect(&c.opts.api, WsAuth::Bearer(&token), Some((&stream, seq)))
        .await
        .map_err(|e| Why::Fail(format!("resume: {e}")))?;
    check!(
        ev3.hello["resumed"] == true && ev3.hello["stream"] == stream.as_str(),
        "resume refused: {}",
        compact(&ev3.hello)
    );
    // Another event may come in between (a presence change, `meta.changed` after a reload): the one generated while
    // disconnected is the first `me.updated` after `seq`, whatever its own number.
    let replayed = ev3
        .wait("me.updated", |e| e["seq"].as_u64().is_some_and(|s| s > seq), WAIT)
        .await;
    check!(replayed.is_some(), "the event sent while disconnected was not replayed");
    // An unknown stream starts afresh.
    let ev4 = Events::connect(&c.opts.api, WsAuth::Bearer(&token), Some(("es_unknown", 3)))
        .await
        .map_err(|e| Why::Fail(format!("connect: {e}")))?;
    check!(ev4.hello["resumed"] == false, "an unknown stream was resumed");
    ev4.close().await;
    ev3.close().await;
    Ok(())
}

// ---------------------------------------------------------------- 1.1.9: several relays, a changing offer

/// `meta.relays`, `meta.revision` and `launcher.relay_rtt_ms` (relay.md 4.1, api.md "GET /v1/meta").
async fn core_relays(c: &mut Ctx) -> R {
    let m = c.meta.clone();
    if let Some(rev) = m.get("revision") {
        check!(rev.is_u64(), "meta.revision is not a non-negative integer: {rev}");
        let again = c.anon.get("/v1/meta").await;
        expect(&again, 200, "GET /v1/meta")?;
        check!(
            again.body["revision"].as_u64() >= rev.as_u64(),
            "meta.revision went down without a restart: {rev} then {}",
            again.body["revision"]
        );
    }
    let Some(relays) = m.get("relays") else {
        return skip("meta.relays is not declared (protocol 1.1.9)");
    };
    let list = relays.as_array().cloned().unwrap_or_default();
    check!(
        relays.is_array() && list.len() <= 16,
        "meta.relays: an array of at most 16"
    );
    let mut ids = HashSet::new();
    for r in &list {
        let id = r["id"].as_str().unwrap_or_default();
        check!(
            (1..=260).contains(&id.len()) && ids.insert(id.to_string()),
            "meta.relays: missing or repeated id in {}",
            compact(r)
        );
        check!(
            r["host"].as_str().is_some_and(|h| !h.is_empty() && h.len() <= 253)
                && r["port"].as_u64().is_some_and(|p| (1..=65535).contains(&p)),
            "meta.relays: host and port of {}",
            compact(r)
        );
        check!(
            r.get("region").is_none_or(Value::is_string) && r.get("label").is_none_or(Value::is_object),
            "meta.relays: region is text and label is localized text in {}",
            compact(r)
        );
    }
    // A launcher's measurement, every relay at once, plus an id the server does not know (it ignores it).
    let p = c.new_player().await?;
    let mut rtt = serde_json::Map::new();
    for (k, r) in list.iter().enumerate() {
        let id = r["id"].as_str().unwrap_or_default().to_string();
        rtt.insert(id, if k == 0 { json!(25) } else { json!(null) });
    }
    rtt.insert("x-unknown-relay".into(), json!(7));
    let r = p
        .api
        .put(
            "/v1/me/game",
            json!({"contract": {"id": kalamar_protocol::CONTRACT_ID, "version": kalamar_protocol::CONTRACT_VERSION},
                   "build": "testkit", "launcher": {"relay_rtt_ms": rtt}}),
        )
        .await;
    expect(&r, 204, "PUT /v1/me/game with launcher.relay_rtt_ms")?;
    Ok(())
}

/// 1.1.13 (relay.md 4.3): a relay tells its load only to the service bound to it. A status query sealed with another
/// key (the suite does not hold any relay's secret) gets no answer, and neither `meta` nor `GET /v1/population`
/// carries the relays' load. Each relay of `meta.relays` that answers a HELLO from here is asked; one that does not is
/// left out (a skip when none answers).
async fn core_relay_status(c: &mut Ctx) -> R {
    for key in ["load", "sessions", "workers", "capacity"] {
        check!(
            c.meta
                .get("relays")
                .and_then(Value::as_array)
                .is_none_or(|l| l.iter().all(|r| r.get(key).is_none())),
            "meta.relays carries `{key}`: the relays' load is for the operator only"
        );
    }
    let pop = c.anon.get("/v1/population").await;
    if pop.status == 200 {
        check!(
            pop.body.get("relays").is_none(),
            "GET /v1/population carries `relays`: the relays' load is for the operator only"
        );
    }
    let mut targets: Vec<(String, u16)> = c
        .meta
        .get("relays")
        .and_then(Value::as_array)
        .map(|l| {
            l.iter()
                .filter_map(|r| {
                    Some((
                        r["host"].as_str()?.to_string(),
                        u16::try_from(r["port"].as_u64()?).ok()?,
                    ))
                })
                .collect()
        })
        .unwrap_or_default();
    if targets.is_empty()
        && let Some((h, p)) = c
            .meta
            .get("relay")
            .and_then(Value::as_str)
            .and_then(|a| a.rsplit_once(':'))
        && let Ok(port) = p.parse::<u16>()
    {
        targets.push((h.to_string(), port));
    }
    let stranger = kalamar_relay::roomauth::RoomSecret::new(kalamar_relay::roomauth::new_secret_hex().into_bytes());
    let mut asked = 0;
    for (host, port) in targets {
        let Some(addr) = tokio::net::lookup_host((host.as_str(), port))
            .await
            .ok()
            .and_then(|mut a| a.find(std::net::SocketAddr::is_ipv4))
        else {
            continue;
        };
        if kalamar_relay::client::hello_rtt(addr, Duration::from_secs(2))
            .await
            .is_none()
        {
            continue;
        }
        asked += 1;
        check!(
            kalamar_relay::client::status(addr, &stranger, Duration::from_millis(1500))
                .await
                .is_none(),
            "the relay {host}:{port} answers a status query that is not from its service: anyone can read its load"
        );
        check!(
            kalamar_relay::client::hello_rtt(addr, Duration::from_secs(2))
                .await
                .is_some(),
            "the relay {host}:{port} stopped answering after a status query"
        );
    }
    if asked == 0 {
        return skip("no relay of meta.relays answers a HELLO from here (UDP)");
    }
    Ok(())
}

/// The relay of a ready match opens its room to a participant (relay protocol: HELLO, CHALLENGE, JOIN, JOIN_ACK),
/// whether it is open or bound to the service by a secret (relay.md 4.2): a relay that refuses the code the service
/// gave (`JOIN_NAK` 3) fails every match. Skipped when the relay does not answer from where the suite runs.
async fn relay_join(c: &Ctx, views: &[Value]) -> R {
    let v = &views[0];
    let g = &v["game"]["relay"];
    let (host, port, room) = (
        g["host"].as_str().unwrap_or_default(),
        g["port"].as_u64().unwrap_or(0),
        g["room"].as_str().unwrap_or_default(),
    );
    check!(
        is_valid_relay_room(room),
        "game.relay.room `{room}` is not a relay room code"
    );
    if let Some(list) = c.meta.get("relays").and_then(Value::as_array) {
        check!(
            list.iter()
                .any(|r| r["host"] == host && r["port"].as_u64() == Some(port)),
            "the match's relay {host}:{port} is not in meta.relays"
        );
    }
    let port = u16::try_from(port).map_err(|_| Why::Fail("game.relay.port".into()))?;
    let addr = tokio::net::lookup_host((host, port))
        .await
        .ok()
        .and_then(|mut a| a.find(std::net::SocketAddr::is_ipv4));
    let Some(addr) = addr else {
        return skip(format!("the relay's host {host} has no IPv4 address from here"));
    };
    let t = Duration::from_secs(3);
    if kalamar_relay::client::hello_rtt(addr, t).await.is_none() {
        return skip(format!(
            "the relay {host}:{port} does not answer a HELLO from here (UDP)"
        ));
    }
    let name = v["you"]["member_name"].as_str().unwrap_or("testkit");
    use kalamar_relay::client::JoinOutcome;
    match kalamar_relay::client::try_join(addr, room, name, t).await {
        Ok(JoinOutcome::Joined) => Ok(()),
        Ok(JoinOutcome::Refused(3)) => fail(format!(
            "the relay {host}:{port} refuses the room code the service gave (JOIN_NAK 3: a relay bound by a secret the \
             service does not seal this relay's codes with): every match there fails"
        )),
        Ok(JoinOutcome::Refused(r)) => fail(format!("the relay refused the JOIN with reason {r}")),
        Ok(JoinOutcome::NoAnswer) => fail("the relay answered the HELLO but not the JOIN"),
        Err(e) => fail(format!("relay client: {e}")),
    }
}

/// The offer changes while the server runs (1.1.9): `--reload-command` makes it read its configuration again; a
/// connected launcher gets `meta.changed` (with a larger `revision` when the server declares one), its session and
/// stream go on.
async fn core_reload(c: &mut Ctx) -> R {
    let Some(cmd) = c.opts.reload_command.clone() else {
        return skip("no --reload-command given");
    };
    let idx = c.pool(1).await?;
    let before = c.anon.get("/v1/meta").await.body["revision"].as_u64();
    let run = tokio::task::spawn_blocking(move || {
        let mut sh = if cfg!(windows) {
            let mut x = std::process::Command::new("cmd");
            x.arg("/C").arg(&cmd);
            x
        } else {
            let mut x = std::process::Command::new("sh");
            x.arg("-c").arg(&cmd);
            x
        };
        sh.output()
    })
    .await
    .map_err(|e| Why::Fail(format!("--reload-command: {e}")))?
    .map_err(|e| Why::Fail(format!("--reload-command: {e}")))?;
    check!(
        run.status.success(),
        "--reload-command failed: {}{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let ev = c.pool[idx[0]].events.as_mut().expect("connected");
    let Some(e) = ev.wait("meta.changed", |_| true, WAIT).await else {
        return fail("no meta.changed after the reload");
    };
    let after = c.anon.get("/v1/meta").await;
    expect(&after, 200, "GET /v1/meta after the reload")?;
    if let Some(b) = before {
        let pushed = e["data"]["revision"].as_u64();
        let now = after.body["revision"].as_u64();
        check!(
            pushed.is_some_and(|r| r > b) && now.is_some_and(|r| r > b),
            "meta.revision did not grow: {b} before, {pushed:?} in meta.changed, {now:?} after"
        );
    }
    expect(
        &c.pool[idx[0]].api.get("/v1/me").await,
        200,
        "the session after a reload",
    )?;
    c.meta = after.body;
    Ok(())
}

/// Checks the `game` block of every participant's view against the match contract.
fn check_contract(views: &[Value]) -> R {
    for v in views {
        let g = &v["game"];
        check!(
            g["contract"]["id"] == kalamar_protocol::CONTRACT_ID && g["contract"]["version"] == 1,
            "game.contract"
        );
        let mid = g["match_id"].as_str().unwrap_or_default();
        let room = g["relay"]["room"].as_str().unwrap_or_default();
        check!(
            is_valid_game_match_id(mid),
            "game.match_id {mid:?} is not 1-32 of A-Z 0-9"
        );
        check!(
            is_valid_relay_room(room) && room == mid,
            "relay.room must equal game.match_id in contract 1"
        );
        check!(room.len() >= 24, "relay.room has fewer than 24 characters (120 bits)");
        check!(
            g["relay"]["host"].as_str().is_some_and(|h| !h.is_empty())
                && g["relay"]["port"].as_u64().is_some_and(|p| (1..=65535).contains(&p)),
            "game.relay host/port"
        );
        let parts = v["participants"].as_array().cloned().unwrap_or_default();
        check!(
            g["players"].as_u64() == Some(parts.len() as u64) && (2..=8).contains(&parts.len()),
            "game.players"
        );
        let rules = parse_rules(g["rules"].as_str().unwrap_or_default())
            .ok_or_else(|| Why::Fail(format!("game.rules is malformed: {}", g["rules"])))?;
        check!(
            rules.players == parts.len(),
            "rules players= does not match the participants"
        );
        check!(
            rules.mode == v["rules"]["mode"].as_str().unwrap_or_default(),
            "rules mode= is not rules.mode"
        );
        let mut seen = HashSet::new();
        let mut hosts = 0;
        let mut sizes = [0u64; 2];
        for (k, p) in parts.iter().enumerate() {
            let name = p["member_name"].as_str().unwrap_or_default();
            check!(is_valid_member_name(name), "member name {name:?} breaks the contract");
            check!(seen.insert(name.to_lowercase()), "member names are not unique");
            let team = Team::parse(p["team"].as_str().unwrap_or_default())
                .ok_or_else(|| Why::Fail("participant team".into()))?;
            sizes[if team == Team::Alpha { 0 } else { 1 }] += 1;
            check!(
                rules.teams.get(k) == Some(&(name.to_string(), team)),
                "rules teams= does not list participant {k} with its team"
            );
            if Role::parse(p["role"].as_str().unwrap_or_default()) == Some(Role::Host) {
                hosts += 1;
                check!(g["host_name"] == name, "host_name is not the host's member name");
            }
        }
        check!(hosts == 1, "exactly one host expected");
        check!(
            v["rules"]["teams"] == json!(sizes),
            "rules.teams {} does not match the participants' teams {sizes:?}",
            v["rules"]["teams"]
        );
        check!(
            v["you"]["member_name"].is_string() && v["deadlines"]["launch"].is_string(),
            "you / deadlines"
        );
    }
    Ok(())
}

async fn core_match(c: &mut Ctx, id: &str) -> R {
    let (source, n) = c.source().await?;
    let who = c.pool(n).await?;
    let views = c.make_match(&who, &source).await?;
    if id == "core.match.contract" {
        check_contract(&views)?;
    }
    // The match is played whatever the relay says, so that its players are free for the next scenarios.
    let relay = if id == "core.relay.join" {
        Some(relay_join(c, &views).await)
    } else {
        None
    };
    let plan: Plan = match id {
        "core.match.disagreement" => Box::new(|s, _| if s == 1 { Play::Cheat } else { Play::Honest }),
        "core.match.silent" => Box::new(|s, _| {
            if s == 1 {
                Play::Silent { played: true }
            } else {
                Play::Honest
            }
        }),
        "core.match.error" => Box::new(|s, _| {
            Play::Error(if s == 0 {
                "players-timeout".into()
            } else {
                "room-not-found".into()
            })
        }),
        "core.match.invalid" => Box::new(|s, _| if s == 1 { Play::WrongSelf } else { Play::Honest }),
        "core.match.crash" => Box::new(|s, _| if s == 1 { Play::Crashed } else { Play::Honest }),
        // 1.1.8: one game computed its own area share, a few permille off (as between two machines), or
        // far off (an edited file, a game badly out of step).
        "core.match.counters" => Box::new(|s, _| if s == 1 { Play::OwnTurf(6) } else { Play::Honest }),
        "core.match.counters-gap" => Box::new(|s, _| if s == 1 { Play::OwnTurf(400) } else { Play::Honest }),
        // 1.1.12: one cheater forges a record that shows the other team gone; the others' games honestly failed
        // before the match, or crashed in it. A forgery is never counted against them, and never rated.
        "core.match.forged" => Box::new(|s, _| {
            if s == 1 {
                Play::Forge
            } else {
                Play::Error("players-timeout".into())
            }
        }),
        "core.match.forged-crash" => Box::new(|s, _| if s == 1 { Play::Forge } else { Play::Crashed }),
        _ => Box::new(|_, _| Play::Honest),
    };
    let (m, expected) = c.play(&who, &views, &*plan).await?;
    if id.starts_with("core.match.forged") {
        let o = &m["outcome"];
        let forger = Sim::from_match(&views[0]).and_then(|sim| sim.seats.get(1).map(|x| x.team));
        check!(
            o["rated"] != true,
            "a forged record was rated: decision {}, rating_changes {}",
            o["decision"],
            compact(&o["rating_changes"])
        );
        check!(
            !(m["state"] == "completed" && forger.is_some_and(|t| o["winner"] == t.as_str())),
            "a forged record won against players whose games failed: decision {}",
            o["decision"]
        );
    }
    // 1.1.8: a server that declares `stats` says in the outcome what it made of the counters.
    let declared = c.meta["result_policies"]
        .as_array()
        .and_then(|ps| ps.iter().find(|p| p["id"] == views[0]["rules"]["result_policy"]))
        .and_then(|p| p.get("stats"))
        .and_then(Value::as_str)
        .map(str::to_string);
    if let (Some(stats), Some(d), true) = (
        declared,
        expected,
        matches!(id, "core.match.counters" | "core.match.counters-gap"),
    ) && d.counted
    {
        let sc = &m["outcome"]["stats_check"];
        check!(
            sc["check"] == stats.as_str() && sc["differ"] == true,
            "outcome.stats_check {} where the policy declares stats {stats} and the counters differ",
            compact(sc)
        );
        check!(
            sc["flagged"] == d.stats.flagged,
            "outcome.stats_check.flagged {} expected {} (tolerance of api.md, step 2)",
            sc["flagged"],
            d.stats.flagged
        );
    }
    if id == "core.match.agreed" {
        check!(m["state"] == "completed", "an agreed match is not completed");
        // A report after the end is refused with match_state.
        let r = c.pool[who[0]]
            .api
            .post(
                &format!("/v1/matches/{}/status", m["id"].as_str().unwrap_or_default()),
                json!({"state": "finished"}),
            )
            .await;
        expect_code(&r, 409, "match_state", "a status after the end")?;
        let ids: Vec<String> = who.iter().map(|&i| c.pool[i].id.clone()).collect();
        c.finished = Some((m, ids));
    }
    relay.unwrap_or(Ok(()))
}

/// 1.1.10 (`api.md`, "Lifecycle"): a participant whose report is in is free of a match that waits for the others'
/// (meta `matches.free_after_report`): its current match is no longer this one and its view says `you.free`; the
/// others stay held; the decision still reaches the free one. The reports of a 1v1 that one player left, as two real
/// games and their launchers sent them ([leaving.md](../../docs/protocol/leaving.md)): `connection-lost` from the one
/// who stayed, `launcher-crashed` from the one who left.
async fn core_match_free(c: &mut Ctx) -> R {
    if c.meta["matches"]["free_after_report"] != true {
        return skip("meta.matches.free_after_report is not declared (protocol 1.1.10)");
    }
    let (source, n) = c.source().await?;
    let who = c.pool(n).await?;
    let views = c.make_match(&who, &source).await?;
    let mid = views[0]["id"].as_str().unwrap_or_default().to_string();
    let game_id = views[0]["game"]["match_id"].clone();
    if views[0]["game"]["keep_open"] == true || views[0]["series"].is_object() {
        return skip("the match is part of a session or series (held until the decision)");
    }
    for &i in &who {
        let api = c.pool[i].api.clone();
        let r = api.post(&format!("/v1/matches/{mid}/launch"), json!({})).await;
        expect(&r, 200, "POST /v1/matches/{id}/launch")?;
        for st in ["booting", "in-match"] {
            let r = api
                .post(&format!("/v1/matches/{mid}/status"), json!({"version": 1, "state": st}))
                .await;
            expect(&r, 204, "POST /v1/matches/{id}/status")?;
        }
    }
    let error = |code: &str| json!({"version": 1, "match_id": game_id, "error": code, "reason": "conformance"});
    let r = c.pool[who[0]]
        .api
        .post(&format!("/v1/matches/{mid}/result"), error("connection-lost"))
        .await;
    expect(&r, 200, "POST /v1/matches/{id}/result")?;
    check!(
        r.body["match"]["state"] == "running" && r.body["match"]["you"]["free"] == true,
        "after its report a participant's view says you.free (state {}, you {})",
        r.body["match"]["state"],
        compact(&r.body["match"]["you"])
    );
    let cur = c.pool[who[0]].api.get("/v1/matches/current").await;
    expect(&cur, 200, "GET /v1/matches/current")?;
    check!(
        cur.body["match"]["id"] != mid.as_str(),
        "a participant free of the match still has it as its current match"
    );
    for &i in &who[1..] {
        let cur = c.pool[i].api.get("/v1/matches/current").await;
        check!(
            cur.body["match"]["id"] == mid.as_str() && cur.body["match"]["you"].get("free").is_none(),
            "a participant without a report is still held by the match"
        );
    }
    for &i in &who[1..] {
        let r = c.pool[i]
            .api
            .post(&format!("/v1/matches/{mid}/result"), error("launcher-crashed"))
            .await;
        expect(&r, 200, "POST /v1/matches/{id}/result")?;
    }
    let ev = c.pool[who[0]].events.as_mut().expect("connected");
    check!(
        ev.wait_any(&["match.void", "match.completed"], |e| e["data"]["id"] == mid, WAIT)
            .await
            .is_some(),
        "the decision did not reach the participant who was free of the match"
    );
    Ok(())
}

/// 1.1.11 (`api.md`, "Result decision", step 3b; [leaving.md](../../docs/protocol/leaving.md)): under a result policy
/// whose `leave` is `forfeit` or `forfeit_unrated`, a participant whose own report proves it left the match in play
/// (`left-by-player` with `in_play: true`, its status in the match), while the others' games stopped with the
/// session (`connection-lost`), makes its team lose: `completed`, decision `forfeit`, the other team the winner,
/// `outcome.forfeit` naming the reason `left`, the team and the player. Fresh accounts: the leaver may get an offense.
async fn core_match_leave(c: &mut Ctx) -> R {
    let (source, n) = c.source().await?;
    let who = fresh(c, n).await?;
    let views = c.make_match(&who, &source).await?;
    let mid = views[0]["id"].as_str().unwrap_or_default().to_string();
    let game_id = views[0]["game"]["match_id"].clone();
    let policy_id = views[0]["rules"]["result_policy"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let Some(policy) = c.policy(&policy_id) else {
        return fail(format!(
            "the match's result policy `{policy_id}` is not in meta.result_policies"
        ));
    };
    let lost = json!({"version": 2, "match_id": game_id, "error": "connection-lost", "reason": "conformance"});
    let why = if !matches!(policy.leave, Leave::Forfeit | Leave::ForfeitUnrated) {
        Some(format!(
            "leave: forfeit is not declared by the result policy `{policy_id}` of the matches this suite creates (protocol 1.1.11)"
        ))
    } else if views[0]["series"].is_object() {
        Some("the match is part of a series".to_string())
    } else {
        None
    };
    if let Some(why) = why {
        // End the match (void) so that its players are free for the next scenarios.
        for &i in &who {
            let _ = c.pool[i]
                .api
                .post(&format!("/v1/matches/{mid}/result"), lost.clone())
                .await;
        }
        return skip(why);
    }
    for &i in &who {
        let api = c.pool[i].api.clone();
        let r = api.post(&format!("/v1/matches/{mid}/launch"), json!({})).await;
        expect(&r, 200, "POST /v1/matches/{id}/launch")?;
        for st in ["booting", "loading-stage", "in-match"] {
            let r = api
                .post(&format!("/v1/matches/{mid}/status"), json!({"version": 2, "state": st}))
                .await;
            expect(&r, 204, "POST /v1/matches/{id}/status")?;
        }
    }
    let left = json!({"version": 2, "match_id": game_id, "error": "left-by-player", "in_play": true,
                      "reason": "conformance: the player left the match from the game's menu"});
    let r = c.pool[who[0]]
        .api
        .post(&format!("/v1/matches/{mid}/result"), left)
        .await;
    expect(&r, 200, "POST /v1/matches/{id}/result")?;
    let mut last = r;
    for &i in &who[1..] {
        last = c.pool[i]
            .api
            .post(&format!("/v1/matches/{mid}/result"), lost.clone())
            .await;
        expect(&last, 200, "POST /v1/matches/{id}/result")?;
    }
    // Every report is in: decided at once.
    let m = &last.body["match"];
    let o = &m["outcome"];
    let leaver_team = views[0]["you"]["team"].as_str().unwrap_or_default();
    let other = if leaver_team == "alpha" { "bravo" } else { "alpha" };
    check!(
        m["state"] == "completed" && o["decision"] == "forfeit" && o["counted"] == true && o["winner"] == other,
        "a departure in play under leave: {} is not a forfeit of the leaver's team (state {}, decision {}, winner {})",
        policy.leave.as_str(),
        m["state"],
        o["decision"],
        o["winner"]
    );
    let leaver = json!(c.pool[who[0]].id);
    check!(
        o["forfeit"]["reason"] == "left"
            && o["forfeit"]["team"] == leaver_team
            && o["forfeit"]["players"]
                .as_array()
                .is_some_and(|a| a.iter().any(|p| p["user_id"] == leaver)),
        "outcome.forfeit does not name the departure (`left`), the team and the player: {}",
        compact(&o["forfeit"])
    );
    let rated = m["rules"]["rated"] == true
        && policy.leave == Leave::Forfeit
        && policy.witness != kalamar_protocol::policy::WitnessUse::Required;
    check!(
        o["rated"] == rated,
        "a forfeit's `rated` is {} (expected {rated}: rules.rated, leave {}, witness {})",
        o["rated"],
        policy.leave.as_str(),
        policy.witness.as_str()
    );
    Ok(())
}

async fn core_erase(c: &mut Ctx) -> R {
    let p = c.new_player().await?;
    let r = p.api.get("/v1/me/export").await;
    check!(
        r.status == 200 && r.body.to_string().contains(&p.id),
        "GET /v1/me/export does not contain the account"
    );
    let r = p.api.delete("/v1/me").await;
    expect_code(&r, 422, "validation_failed", "DELETE /v1/me without confirm=erase")?;
    expect(
        &p.api.delete("/v1/me?confirm=erase").await,
        204,
        "DELETE /v1/me?confirm=erase",
    )?;
    let r = p.api.get("/v1/me").await;
    check!(
        r.status == 401,
        "the account still answers after erasure ({})",
        r.status
    );
    Ok(())
}

// ---------------------------------------------------------------- rooms

async fn rooms_declaration(c: &mut Ctx) -> R {
    let d = &c.meta["rooms"];
    let opts = d["options"].as_array().cloned().unwrap_or_default();
    check!(!opts.is_empty(), "rooms.options is empty");
    for f in &opts {
        check!(
            f["key"].is_string() && f["type"].is_string() && f["label"].is_object() && f["required"].is_boolean(),
            "field shape: {}",
            compact(f)
        );
        match f["key"].as_str() {
            Some("teams") => {
                check!(f["type"] == "teams", "teams field type");
                let ok = f["choices"].as_array().is_some_and(|cs| {
                    !cs.is_empty()
                        && cs.iter().all(|c| {
                            let (a, b) = (c[0].as_u64().unwrap_or(0), c[1].as_u64().unwrap_or(0));
                            a >= 1 && b >= 1 && a + b <= 8
                        })
                });
                check!(
                    ok,
                    "teams choices must be [alpha, bravo] pairs, each at least 1, at most 8 players"
                );
            }
            Some("mode") => check!(f["type"] == "mode", "mode field type"),
            Some("stage") => check!(f["type"] == "stage", "stage field type"),
            Some("password") => check!(
                f["type"] == "string" && f["secret"] == true,
                "password field must be a secret string"
            ),
            _ => {}
        }
    }
    check!(d["min_players"].as_u64().is_some_and(|n| n >= 2), "min_players");
    check!(
        c.policy(d["result_policy"].as_str().unwrap_or_default()).is_some(),
        "rooms.result_policy is not a declared policy"
    );
    Ok(())
}

async fn rooms_lifecycle(c: &mut Ctx) -> R {
    let who = c.pool(2).await?;
    let has_password = c.meta["rooms"]["options"]
        .as_array()
        .is_some_and(|o| o.iter().any(|f| f["key"] == "password"));
    let mut options = json!({});
    if has_password {
        options["password"] = json!("s3cret");
    }
    let (a, b) = (who[0], who[1]);
    let r = c.pool[a].api.post("/v1/rooms", json!({"options": options})).await;
    expect(&r, 201, "POST /v1/rooms")?;
    let room = r.body.clone();
    let rid = room["id"].as_str().unwrap_or_default().to_string();
    check!(
        room["code"].is_string() && room["owner"] == c.pool[a].id.as_str() && room["state"] == "open",
        "room shape: {}",
        compact(&room)
    );
    check!(
        room["has_password"] == has_password && room["options"].get("password").is_none(),
        "the password must not be returned"
    );
    let code = room["code"].as_str().unwrap_or_default().to_lowercase();
    if has_password {
        let r = c.pool[b]
            .api
            .post("/v1/rooms/join", json!({"code": code, "password": "wrong"}))
            .await;
        expect_code(&r, 403, "wrong_password", "joining with a wrong password")?;
    }
    let r = c.pool[b]
        .api
        .post("/v1/rooms/join", json!({"code": code, "password": "s3cret"}))
        .await;
    expect(&r, 200, "joining by code in lower case")?;
    check!(
        r.body["members"].as_array().map(Vec::len) == Some(2),
        "two members expected"
    );
    let ev = c.pool[a].events.as_mut().expect("connected");
    check!(
        ev.wait("room.updated", |e| e["data"]["id"] == rid.as_str(), WAIT)
            .await
            .is_some(),
        "the owner got no room.updated"
    );
    let r = c.pool[a].api.post_empty(&format!("/v1/rooms/{rid}/start")).await;
    expect_code(&r, 409, "not_ready", "start before everyone is ready")?;
    let r = c.pool[b]
        .api
        .post(&format!("/v1/rooms/{rid}/ready"), json!({"ready": true}))
        .await;
    expect(&r, 200, "ready")?;
    let r = c.pool[a].api.post_empty(&format!("/v1/rooms/{rid}/start")).await;
    expect(&r, 201, "start")?;
    let mid = r.body["id"].as_str().unwrap_or_default().to_string();
    check!(
        r.body["state"] == "ready" && r.body["game"].is_object(),
        "a room match starts ready with its game block"
    );
    let views = c.wait_all(&who, "match.ready", &mid).await?;
    check_contract(&views)?;
    let (m, _) = c.play(&who, &views, &|_, _| Play::Honest).await?;
    check!(m["state"] == "completed", "the room match did not complete");
    tokio::time::sleep(Duration::from_millis(200)).await;
    let r = c.pool[a].api.get(&format!("/v1/rooms/{rid}")).await;
    check!(
        r.status == 200
            && r.body["state"] == "open"
            && r.body["members"]
                .as_array()
                .is_some_and(|ms| ms.iter().all(|m| m["ready"] == false)),
        "the room is not open again with ready reset: {}",
        compact(&r.body)
    );
    Ok(())
}

async fn rooms_full(c: &mut Ctx) -> R {
    let choices: Vec<[u64; 2]> = c.meta["rooms"]["options"]
        .as_array()
        .and_then(|o| o.iter().find(|f| f["key"] == "teams"))
        .and_then(|f| serde_json::from_value(f["choices"].clone()).ok())
        .unwrap_or_default();
    let Some(small) = choices.iter().min_by_key(|c| c[0] + c[1]).copied() else {
        return skip("no teams choice");
    };
    let n = (small[0] + small[1]) as usize;
    let who = c.pool(n + 1).await?;
    let r = c.pool[who[0]]
        .api
        .post("/v1/rooms", json!({"options": {"teams": small}}))
        .await;
    expect(&r, 201, "POST /v1/rooms")?;
    let code = r.body["code"].as_str().unwrap_or_default().to_string();
    for &i in &who[1..n] {
        expect(
            &c.pool[i].api.post("/v1/rooms/join", json!({"code": code})).await,
            200,
            "join",
        )?;
    }
    let r = c.pool[who[n]].api.post("/v1/rooms/join", json!({"code": code})).await;
    expect_code(&r, 409, "room_full", "joining a full room")?;
    let r = c.pool[who[n]]
        .api
        .post("/v1/rooms/join", json!({"code": "ZZZZZZZZ"}))
        .await;
    expect_code(&r, 404, "not_found", "joining an unknown code")?;
    Ok(())
}

async fn rooms_kick_leave_owner(c: &mut Ctx) -> R {
    let who = c.pool(3).await?;
    let (a, b, d) = (who[0], who[1], who[2]);
    let r = c.pool[a].api.post("/v1/rooms", json!({"options": {}})).await;
    expect(&r, 201, "POST /v1/rooms")?;
    let rid = r.body["id"].as_str().unwrap_or_default().to_string();
    let code = r.body["code"].as_str().unwrap_or_default().to_string();
    let cap: u64 = r.body["options"]["teams"]
        .as_array()
        .map(|t| t.iter().filter_map(Value::as_u64).sum())
        .unwrap_or(2);
    if cap < 3 {
        return skip("the default room holds fewer than 3 players");
    }
    expect(
        &c.pool[b].api.post("/v1/rooms/join", json!({"code": code})).await,
        200,
        "join",
    )?;
    expect(
        &c.pool[d].api.post("/v1/rooms/join", json!({"code": code})).await,
        200,
        "join",
    )?;
    let r = c.pool[b]
        .api
        .post(&format!("/v1/rooms/{rid}/kick"), json!({"user_id": c.pool[d].id}))
        .await;
    expect_code(&r, 403, "not_owner", "a member kicking")?;
    let did = c.pool[d].id.clone();
    let r = c.pool[a]
        .api
        .post(&format!("/v1/rooms/{rid}/kick"), json!({"user_id": did}))
        .await;
    expect(&r, 200, "kick")?;
    let ev = c.pool[d].events.as_mut().expect("connected");
    check!(
        ev.wait("room.closed", |e| e["data"]["room_id"] == rid.as_str(), WAIT)
            .await
            .is_some(),
        "the kicked player got no room.closed"
    );
    let r = c.pool[d].api.post("/v1/rooms/join", json!({"code": code})).await;
    expect_code(&r, 403, "kicked", "rejoining after a kick")?;
    let bid = c.pool[b].id.clone();
    let r = c.pool[a]
        .api
        .post(&format!("/v1/rooms/{rid}/owner"), json!({"user_id": bid}))
        .await;
    check!(r.status == 200 && r.body["owner"] == bid.as_str(), "ownership transfer");
    expect(
        &c.pool[b].api.post_empty(&format!("/v1/rooms/{rid}/leave")).await,
        204,
        "the owner leaves",
    )?;
    let r = c.pool[a].api.get(&format!("/v1/rooms/{rid}")).await;
    check!(
        r.status == 200 && r.body["owner"] == c.pool[a].id.as_str(),
        "ownership did not pass to the remaining member"
    );
    expect(
        &c.pool[a].api.post_empty(&format!("/v1/rooms/{rid}/leave")).await,
        204,
        "the last member leaves",
    )?;
    let r = c.pool[a].api.get(&format!("/v1/rooms/{rid}")).await;
    check!(r.status == 404, "an empty room still exists");
    Ok(())
}

// ---------------------------------------------------------------- queues

async fn queues_list(c: &mut Ctx) -> R {
    let idx = c.pool(1).await?;
    let r = c.pool[idx[0]].api.get("/v1/queues").await;
    expect(&r, 200, "GET /v1/queues")?;
    let items = r.body["items"].as_array().cloned().unwrap_or_default();
    check!(!items.is_empty(), "no queue listed");
    check!(
        c.meta["queues"]["count"].as_u64() == Some(items.len() as u64),
        "queues.count differs from the list"
    );
    for q in &items {
        let t = [q["teams"][0].as_u64().unwrap_or(0), q["teams"][1].as_u64().unwrap_or(0)];
        let mt = [
            q["min_teams"][0].as_u64().unwrap_or(0),
            q["min_teams"][1].as_u64().unwrap_or(0),
        ];
        check!(
            q["id"].is_string() && q["name"].is_object(),
            "queue id/name: {}",
            compact(q)
        );
        check!(
            t[0] >= 1 && t[1] >= 1 && t[0] + t[1] <= 8,
            "queue teams outside the contract: {:?}",
            t
        );
        check!(
            mt[0] >= 1 && mt[1] >= 1 && mt[0] <= t[0] && mt[1] <= t[1],
            "min_teams outside teams"
        );
        check!(
            q["party"]["min"].as_u64().unwrap_or(0) >= 1 && q["party"]["max"].as_u64() >= q["party"]["min"].as_u64(),
            "party limits"
        );
        check!(
            q["rated"].is_boolean() && (q["ladder"].is_string() || q["ladder"].is_null()),
            "rated/ladder"
        );
        check!(
            q["pool"]
                .as_array()
                .is_some_and(|p| !p.is_empty() && p.iter().all(|e| e["mode"].is_string())),
            "pool"
        );
        check!(
            c.policy(q["result_policy"].as_str().unwrap_or_default()).is_some(),
            "result_policy is not declared"
        );
        check!(
            q["availability"]["open"].is_boolean() && q["eligible"].is_boolean() && q["requirements"].is_array(),
            "availability/eligible/requirements"
        );
        let r = c.pool[idx[0]]
            .api
            .get(&format!("/v1/queues/{}", q["id"].as_str().unwrap_or_default()))
            .await;
        expect(&r, 200, "GET /v1/queues/{id}")?;
    }
    let r = c.pool[idx[0]].api.get("/v1/queues/no-such-queue").await;
    expect_code(&r, 404, "not_found", "an unknown queue")?;
    Ok(())
}

async fn queue_items(c: &mut Ctx) -> Result<Vec<Value>, Why> {
    let idx = c.pool(1).await?;
    let r = c.pool[idx[0]].api.get("/v1/queues").await;
    expect(&r, 200, "GET /v1/queues")?;
    Ok(r.body["items"].as_array().cloned().unwrap_or_default())
}

fn size(q: &Value) -> usize {
    (q["teams"][0].as_u64().unwrap_or(0) + q["teams"][1].as_u64().unwrap_or(0)) as usize
}

async fn queues_match(c: &mut Ctx) -> R {
    let queues = queue_items(c).await?;
    let mut played = 0;
    for q in queues
        .iter()
        .filter(|q| q["eligible"] == true && q["party"]["min"].as_u64() == Some(1))
    {
        let n = size(q);
        let who = c.pool(n).await?;
        let qid = q["id"].as_str().unwrap_or_default().to_string();
        let views = c
            .make_match(&who, &Source::Queue(qid.clone()))
            .await
            .map_err(|w| match w {
                Why::Fail(m) => Why::Fail(format!("queue {qid}: {m}")),
                s => s,
            })?;
        check!(
            views[0]["origin"]["queue_id"] == qid.as_str() && views[0]["rules"]["rated"] == q["rated"],
            "match origin/rated"
        );
        check_contract(&views)?;
        let (m, _) = c.play(&who, &views, &|_, _| Play::Honest).await?;
        let ids: Vec<String> = who.iter().map(|&i| c.pool[i].id.clone()).collect();
        if m["outcome"]["rated"] == true {
            c.rated = Some((m.clone(), ids.clone()));
        }
        if m["state"] == "completed" {
            c.finished = Some((m, ids));
        }
        played += 1;
        c.reset().await;
    }
    if played == 0 {
        return skip("no eligible solo queue");
    }
    Ok(())
}

async fn smallest_queue(c: &mut Ctx) -> Result<Value, Why> {
    let queues = queue_items(c).await?;
    queues
        .into_iter()
        .filter(|q| q["eligible"] == true && q["party"]["min"].as_u64() == Some(1))
        .min_by_key(size)
        .map(Ok)
        .unwrap_or_else(|| skip("no eligible solo queue"))
}

/// Fresh accounts (offenses must not reach the shared pool).
async fn fresh(c: &mut Ctx, n: usize) -> Result<Vec<usize>, Why> {
    let start = c.pool.len();
    for _ in 0..n {
        let p = c.new_player().await?;
        c.pool.push(p);
    }
    let all = c.pool(start + n).await?;
    Ok(all[start..].to_vec())
}

async fn queues_decline(c: &mut Ctx) -> R {
    let q = smallest_queue(c).await?;
    let qid = q["id"].as_str().unwrap_or_default().to_string();
    let who = fresh(c, size(&q)).await?;
    for &i in &who {
        expect(
            &c.pool[i]
                .api
                .post(&format!("/v1/queues/{qid}/tickets"), json!({}))
                .await,
            201,
            "join queue",
        )?;
    }
    let found = c.wait_found(&who).await?;
    let mid = found[0]["id"].as_str().unwrap_or_default().to_string();
    check!(
        found[0]["state"] == "pending_accept" && found[0]["deadlines"]["accept"].is_string(),
        "match.found shape"
    );
    check!(
        found[0]["game"].is_null(),
        "the relay room is revealed before everyone accepted"
    );
    for &i in &who[1..] {
        expect(
            &c.pool[i].api.post_empty(&format!("/v1/matches/{mid}/accept")).await,
            200,
            "accept",
        )?;
    }
    let r = c.pool[who[0]]
        .api
        .post_empty(&format!("/v1/matches/{mid}/decline"))
        .await;
    expect(&r, 200, "decline")?;
    let ev = c.pool[who[1]].events.as_mut().expect("connected");
    let e = ev
        .wait("match.cancelled", |e| e["data"]["id"] == mid.as_str(), WAIT)
        .await
        .ok_or_else(|| Why::Fail("no match.cancelled".into()))?;
    check!(
        e["data"]["cancel_reason"] == "declined",
        "cancel_reason is not declined"
    );
    let t = c.pool[who[1]].api.get("/v1/tickets/current").await;
    check!(
        t.body["ticket"]["state"] == "searching",
        "an accepting player's ticket did not go back to searching"
    );
    let t = c.pool[who[0]].api.get("/v1/tickets/current").await;
    check!(t.body["ticket"].is_null(), "the decliner's ticket still exists");
    let first = c.meta["queues"]["penalties"]["cooldowns_s"][0].as_u64().unwrap_or(0);
    let counted = c.meta["queues"]["penalties"]["counted"]
        .as_array()
        .is_some_and(|a| a.contains(&json!("decline")));
    if counted && first > 0 {
        let me = c.pool[who[0]].api.get("/v1/me").await;
        check!(
            me.body["restrictions"]
                .as_array()
                .is_some_and(|r| r.iter().any(|x| x["kind"] == "queue_cooldown")),
            "no queue_cooldown restriction after a counted decline"
        );
        let r = c.pool[who[0]]
            .api
            .post(&format!("/v1/queues/{qid}/tickets"), json!({}))
            .await;
        expect_code(&r, 403, "queue_cooldown", "joining during a cooldown")?;
    }
    for &i in &who {
        let _ = c.pool[i].api.delete("/v1/tickets/current").await;
    }
    Ok(())
}

async fn queues_timeout(c: &mut Ctx) -> R {
    let q = smallest_queue(c).await?;
    let qid = q["id"].as_str().unwrap_or_default().to_string();
    let accept_s = c
        .policy(q["result_policy"].as_str().unwrap_or_default())
        .map(|p| p.deadlines.accept_s as u64)
        .unwrap_or(20);
    if !c.test_clock && accept_s > c.opts.max_wait {
        return skip(format!("needs {accept_s} s to pass (no test-clock)"));
    }
    let who = fresh(c, size(&q)).await?;
    for &i in &who {
        expect(
            &c.pool[i]
                .api
                .post(&format!("/v1/queues/{qid}/tickets"), json!({}))
                .await,
            201,
            "join queue",
        )?;
    }
    let found = c.wait_found(&who).await?;
    let mid = found[0]["id"].as_str().unwrap_or_default().to_string();
    expect(
        &c.pool[who[0]]
            .api
            .post_empty(&format!("/v1/matches/{mid}/accept"))
            .await,
        200,
        "accept",
    )?;
    c.advance(accept_s + 1).await?;
    let ev = c.pool[who[0]].events.as_mut().expect("connected");
    let e = ev
        .wait("match.cancelled", |e| e["data"]["id"] == mid.as_str(), WAIT)
        .await
        .ok_or_else(|| Why::Fail("no match.cancelled after the accept deadline".into()))?;
    check!(
        e["data"]["cancel_reason"] == "accept_timeout",
        "cancel_reason is not accept_timeout"
    );
    for &i in &who {
        let _ = c.pool[i].api.delete("/v1/tickets/current").await;
    }
    Ok(())
}

async fn queues_cancel(c: &mut Ctx) -> R {
    let q = smallest_queue(c).await?;
    if size(&q) < 2 {
        return skip("queue too small");
    }
    let who = c.pool(1).await?;
    let qid = q["id"].as_str().unwrap_or_default();
    let api = c.pool[who[0]].api.clone();
    let r = api.post(&format!("/v1/queues/{qid}/tickets"), json!({})).await;
    expect(&r, 201, "join queue")?;
    check!(
        r.body["state"] == "searching" && r.body["queue_id"] == qid,
        "ticket shape: {}",
        compact(&r.body)
    );
    let r = api.post(&format!("/v1/queues/{qid}/tickets"), json!({})).await;
    expect_code(&r, 409, "already_in_queue", "joining twice")?;
    expect(
        &api.delete("/v1/tickets/current").await,
        204,
        "DELETE /v1/tickets/current",
    )?;
    let r = api.get("/v1/tickets/current").await;
    check!(
        r.status == 200 && r.body["ticket"].is_null(),
        "the ticket is still there"
    );
    Ok(())
}

async fn queues_shrink(c: &mut Ctx) -> R {
    let queues = queue_items(c).await?;
    let Some(q) = queues
        .into_iter()
        .find(|q| q["eligible"] == true && q["party"]["min"].as_u64() == Some(1) && q["min_teams"] != q["teams"])
    else {
        return skip("no queue with min_teams smaller than teams");
    };
    if !c.test_clock {
        return skip("needs the test clock (the shrink delay is the server's)");
    }
    let n = (q["min_teams"][0].as_u64().unwrap_or(1) + q["min_teams"][1].as_u64().unwrap_or(1)) as usize;
    let who = c.pool(n).await?;
    let qid = q["id"].as_str().unwrap_or_default().to_string();
    for &i in &who {
        expect(
            &c.pool[i]
                .api
                .post(&format!("/v1/queues/{qid}/tickets"), json!({}))
                .await,
            201,
            "join queue",
        )?;
    }
    let mut found = None;
    for _ in 0..20 {
        c.advance(60).await?;
        let ev = c.pool[who[0]].events.as_mut().expect("connected");
        if let Some(e) = ev.wait("match.found", |_| true, Duration::from_millis(300)).await {
            found = Some(e["data"].clone());
            break;
        }
    }
    let Some(m) = found else {
        return fail("no match with the smaller teams after 20 minutes of waiting");
    };
    check!(
        m["rules"]["teams"] == q["min_teams"],
        "the match has teams {} instead of {}",
        m["rules"]["teams"],
        q["min_teams"]
    );
    let mid = m["id"].as_str().unwrap_or_default().to_string();
    let _ = c.wait_found(&who[1..]).await;
    for &i in &who {
        let _ = c.pool[i].api.post_empty(&format!("/v1/matches/{mid}/accept")).await;
    }
    let views = c.wait_all(&who, "match.ready", &mid).await?;
    let (m, _) = c.play(&who, &views, &|_, _| Play::Honest).await?;
    check!(m["state"] == "completed", "the smaller match did not complete");
    Ok(())
}

// ---------------------------------------------------------------- friends, presence, parties, moderation

async fn befriend(c: &mut Ctx, a: usize, b: usize) -> R {
    let code = c.pool[b].api.get("/v1/me").await.body["friend_code"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let r = c.pool[a]
        .api
        .post("/v1/friends/requests", json!({"friend_code": code}))
        .await;
    if r.code() == Some("already_friends") {
        return Ok(());
    }
    check!(
        r.status == 201 || r.status == 200,
        "POST /v1/friends/requests: {} {}",
        r.status,
        compact(&r.body)
    );
    if r.status == 201 {
        let id = r.body["id"].as_str().unwrap_or_default().to_string();
        expect(
            &c.pool[b]
                .api
                .post_empty(&format!("/v1/friends/requests/{id}/accept"))
                .await,
            200,
            "accept the request",
        )?;
    }
    Ok(())
}

async fn friends_requests(c: &mut Ctx) -> R {
    let who = fresh(c, 2).await?;
    let (a, b) = (who[0], who[1]);
    let me_b = c.pool[b].api.get("/v1/me").await;
    let code = me_b.body["friend_code"].as_str().unwrap_or_default().to_string();
    check!(!code.is_empty(), "Me has no friend_code");
    let r = c.pool[a]
        .api
        .post("/v1/friends/requests", json!({"friend_code": code.to_lowercase()}))
        .await;
    expect(&r, 201, "a request by friend code (lower case)")?;
    let rid = r.body["id"].as_str().unwrap_or_default().to_string();
    let ev = c.pool[b].events.as_mut().expect("connected");
    check!(
        ev.wait("friend.request", |_| true, WAIT).await.is_some(),
        "no friend.request event"
    );
    let r = c.pool[b].api.get("/v1/friends/requests").await;
    check!(
        r.body["incoming"]
            .as_array()
            .is_some_and(|i| i.iter().any(|x| x["id"] == rid.as_str())),
        "the request is not listed as incoming"
    );
    let r = c.pool[a]
        .api
        .post("/v1/friends/requests", json!({"friend_code": code}))
        .await;
    expect_code(&r, 409, "request_exists", "a second request")?;
    expect(
        &c.pool[b]
            .api
            .post_empty(&format!("/v1/friends/requests/{rid}/accept"))
            .await,
        200,
        "accept",
    )?;
    let r = c.pool[a].api.get("/v1/friends").await;
    check!(
        r.body["items"]
            .as_array()
            .is_some_and(|i| i.iter().any(|f| f["user"]["id"] == c.pool[b].id.as_str())),
        "the friend is not listed"
    );
    let bid = c.pool[b].id.clone();
    expect(
        &c.pool[a].api.delete(&format!("/v1/friends/{bid}")).await,
        204,
        "remove a friend",
    )?;
    let ev = c.pool[b].events.as_mut().expect("connected");
    check!(
        ev.wait("friend.removed", |_| true, WAIT).await.is_some(),
        "no friend.removed event"
    );
    // Decline, cancel, rotate.
    let r = c.pool[a]
        .api
        .post("/v1/friends/requests", json!({"friend_code": code}))
        .await;
    expect(&r, 201, "request again")?;
    let rid = r.body["id"].as_str().unwrap_or_default().to_string();
    expect(
        &c.pool[b]
            .api
            .post_empty(&format!("/v1/friends/requests/{rid}/decline"))
            .await,
        204,
        "decline",
    )?;
    let r = c.pool[a]
        .api
        .post("/v1/friends/requests", json!({"friend_code": code}))
        .await;
    expect(&r, 201, "request again")?;
    let rid = r.body["id"].as_str().unwrap_or_default().to_string();
    expect(
        &c.pool[a].api.delete(&format!("/v1/friends/requests/{rid}")).await,
        204,
        "cancel",
    )?;
    let r = c.pool[b].api.post_empty("/v1/me/friend-code").await;
    check!(
        r.status == 200 && r.body["friend_code"].is_string() && r.body["friend_code"] != code.as_str(),
        "rotating the friend code"
    );
    let r = c.pool[a]
        .api
        .post("/v1/friends/requests", json!({"friend_code": code}))
        .await;
    expect_code(&r, 404, "not_found", "the old friend code after rotation")?;
    Ok(())
}

async fn friends_presence(c: &mut Ctx) -> R {
    let who = fresh(c, 2).await?;
    let (a, b) = (who[0], who[1]);
    befriend(c, a, b).await?;
    let presence = |r: &Resp, id: &str| {
        r.body["items"]
            .as_array()
            .and_then(|i| i.iter().find(|f| f["user"]["id"] == id).map(|f| f["presence"].clone()))
            .unwrap_or(Value::Null)
    };
    let bid = c.pool[b].id.clone();
    let r = c.pool[a].api.get("/v1/friends").await;
    check!(
        presence(&r, &bid) == "online",
        "a connected friend is {} instead of online",
        presence(&r, &bid)
    );
    if c.declared("queues")
        && let Ok(q) = smallest_queue(c).await
    {
        let qid = q["id"].as_str().unwrap_or_default().to_string();
        if size(&q) >= 2
            && c.pool[b]
                .api
                .post(&format!("/v1/queues/{qid}/tickets"), json!({}))
                .await
                .status
                == 201
        {
            let ev = c.pool[a].events.as_mut().expect("connected");
            check!(
                ev.wait("friend.presence", |e| e["data"]["presence"] == "in_queue", WAIT)
                    .await
                    .is_some(),
                "no friend.presence in_queue"
            );
            let _ = c.pool[b].api.delete("/v1/tickets/current").await;
        }
    }
    let r = c.pool[b]
        .api
        .patch("/v1/me", json!({"settings": {"share_presence": false}}))
        .await;
    expect(&r, 200, "hide presence")?;
    let r = c.pool[a].api.get("/v1/friends").await;
    check!(
        presence(&r, &bid) == "offline",
        "a hidden presence shows {}",
        presence(&r, &bid)
    );
    Ok(())
}

async fn parties_lifecycle(c: &mut Ctx) -> R {
    let who = fresh(c, 2).await?;
    let (a, b) = (who[0], who[1]);
    let r = c.pool[a].api.post_empty("/v1/party").await;
    expect(&r, 201, "POST /v1/party")?;
    let pid = r.body["id"].as_str().unwrap_or_default().to_string();
    check!(
        r.body["leader"] == c.pool[a].id.as_str() && r.body["max_size"].is_u64(),
        "party shape"
    );
    let bid = c.pool[b].id.clone();
    let mut r = c.pool[a].api.post("/v1/party/invites", json!({"user_id": bid})).await;
    if r.code() == Some("not_friends") && c.declared("friends") {
        befriend(c, a, b).await?;
        r = c.pool[a].api.post("/v1/party/invites", json!({"user_id": bid})).await;
    }
    expect(&r, 201, "invite")?;
    let ev = c.pool[b].events.as_mut().expect("connected");
    check!(
        ev.wait("party.invite", |e| e["data"]["party_id"] == pid.as_str(), WAIT)
            .await
            .is_some(),
        "no party.invite event"
    );
    let r = c.pool[b].api.post_empty(&format!("/v1/parties/{pid}/join")).await;
    expect(&r, 200, "join the party")?;
    check!(
        r.body["members"].as_array().map(Vec::len) == Some(2),
        "two members expected"
    );
    let r = c.pool[b]
        .api
        .post("/v1/party/kick", json!({"user_id": c.pool[a].id}))
        .await;
    expect_code(&r, 403, "not_leader", "a member kicking")?;
    let r = c.pool[a].api.post("/v1/party/leader", json!({"user_id": bid})).await;
    check!(r.status == 200 && r.body["leader"] == bid.as_str(), "leader transfer");
    expect(&c.pool[a].api.post_empty("/v1/party/leave").await, 204, "leave")?;
    let r = c.pool[b].api.get("/v1/party").await;
    check!(
        r.body["party"]["members"].as_array().map(Vec::len) == Some(1),
        "the party did not shrink"
    );
    expect(
        &c.pool[b].api.post_empty("/v1/party/leave").await,
        204,
        "the last member leaves",
    )?;
    let r = c.pool[b].api.get("/v1/party").await;
    check!(r.body["party"].is_null(), "an empty party still exists");
    Ok(())
}

async fn parties_queue(c: &mut Ctx) -> R {
    let queues = queue_items(c).await?;
    let Some(q) = queues
        .into_iter()
        .filter(|q| {
            q["eligible"] == true
                && q["party"]["max"].as_u64().unwrap_or(1) >= 2
                && q["party"]["min"].as_u64() == Some(1)
                && q["teams"][0].as_u64().unwrap_or(0) >= 2
                && q["teams"][1].as_u64().unwrap_or(0) >= 2
        })
        .min_by_key(size)
    else {
        return skip("no queue takes parties of 2");
    };
    let n = size(&q);
    let who = c.pool(n).await?;
    let (a, b) = (who[0], who[1]);
    expect(&c.pool[a].api.post_empty("/v1/party").await, 201, "POST /v1/party")?;
    let pid = c.pool[a].api.get("/v1/party").await.body["party"]["id"]
        .as_str()
        .unwrap_or_default()
        .to_string();
    let bid = c.pool[b].id.clone();
    let mut r = c.pool[a].api.post("/v1/party/invites", json!({"user_id": bid})).await;
    if r.code() == Some("not_friends") && c.declared("friends") {
        befriend(c, a, b).await?;
        r = c.pool[a].api.post("/v1/party/invites", json!({"user_id": bid})).await;
    }
    expect(&r, 201, "invite")?;
    expect(
        &c.pool[b].api.post_empty(&format!("/v1/parties/{pid}/join")).await,
        200,
        "join",
    )?;
    let qid = q["id"].as_str().unwrap_or_default().to_string();
    let r = c.pool[b]
        .api
        .post(&format!("/v1/queues/{qid}/tickets"), json!({}))
        .await;
    expect_code(&r, 403, "not_leader", "a party member joining a queue")?;
    let r = c.pool[a]
        .api
        .post(&format!("/v1/queues/{qid}/tickets"), json!({}))
        .await;
    expect(&r, 201, "the leader queues the party")?;
    check!(
        r.body["members"].as_array().map(Vec::len) == Some(2),
        "the ticket does not hold the party"
    );
    for &i in &who[2..] {
        expect(
            &c.pool[i]
                .api
                .post(&format!("/v1/queues/{qid}/tickets"), json!({}))
                .await,
            201,
            "solo join",
        )?;
    }
    let found = c.wait_found(&who).await?;
    let m = &found[0];
    let team_of = |id: &str| {
        m["participants"]
            .as_array()
            .and_then(|p| p.iter().find(|x| x["user"]["id"] == id))
            .map(|x| x["team"].clone())
    };
    check!(
        team_of(&c.pool[a].id) == team_of(&bid),
        "the party was split across teams"
    );
    let mid = m["id"].as_str().unwrap_or_default().to_string();
    for &i in &who {
        expect(
            &c.pool[i].api.post_empty(&format!("/v1/matches/{mid}/accept")).await,
            200,
            "accept",
        )?;
    }
    let views = c.wait_all(&who, "match.ready", &mid).await?;
    c.play(&who, &views, &|_, _| Play::Honest).await?;
    Ok(())
}

async fn moderation_report(c: &mut Ctx) -> R {
    let who = c.pool(2).await?;
    let cat = c.meta["moderation"]["report_categories"][0]["id"]
        .as_str()
        .unwrap_or("other")
        .to_string();
    let target = c.pool[who[1]].id.clone();
    let r = c.pool[who[0]]
        .api
        .post(
            "/v1/reports",
            json!({"user_id": target, "category": cat, "comment": "conformance suite test report, please ignore"}),
        )
        .await;
    expect(&r, 201, "POST /v1/reports")?;
    check!(r.body["id"].is_string(), "report id");
    let r = c.pool[who[0]]
        .api
        .post(
            "/v1/reports",
            json!({"user_id": target, "category": "no-such-category"}),
        )
        .await;
    expect_code(&r, 422, "validation_failed", "an unknown category")?;
    Ok(())
}

async fn moderation_block(c: &mut Ctx) -> R {
    let who = fresh(c, 2).await?;
    let (a, b) = (who[0], who[1]);
    let bid = c.pool[b].id.clone();
    expect(
        &c.pool[a].api.put(&format!("/v1/blocks/{bid}"), json!({})).await,
        204,
        "PUT /v1/blocks/{id}",
    )?;
    let r = c.pool[a].api.get("/v1/blocks").await;
    check!(
        r.body["items"]
            .as_array()
            .is_some_and(|i| i.iter().any(|x| x["user"]["id"] == bid.as_str())),
        "the block is not listed"
    );
    if c.declared("friends") {
        let code = c.pool[a].api.get("/v1/me").await.body["friend_code"]
            .as_str()
            .unwrap_or_default()
            .to_string();
        let r = c.pool[b]
            .api
            .post("/v1/friends/requests", json!({"friend_code": code}))
            .await;
        expect_code(&r, 403, "blocked", "a friend request from a blocked player")?;
    }
    expect(
        &c.pool[a].api.delete(&format!("/v1/blocks/{bid}")).await,
        204,
        "DELETE /v1/blocks/{id}",
    )?;
    Ok(())
}

// ---------------------------------------------------------------- ratings, leaderboards, stats

async fn ratings_ladders(c: &mut Ctx) -> R {
    let idx = c.pool(1).await?;
    let r = c.pool[idx[0]].api.get("/v1/ladders").await;
    expect(&r, 200, "GET /v1/ladders")?;
    for l in r.body["items"].as_array().cloned().unwrap_or_default() {
        check!(
            l["id"].is_string() && l["name"].is_object() && l["queues"].is_array() && l["leaderboard"].is_boolean(),
            "ladder shape: {}",
            compact(&l)
        );
        for t in l["tiers"].as_array().cloned().unwrap_or_default() {
            check!(
                t["id"].is_string() && t["name"].is_object() && t["order"].is_u64(),
                "tier shape"
            );
        }
    }
    let r = c.pool[idx[0]].api.get("/v1/seasons").await;
    expect(&r, 200, "GET /v1/seasons")?;
    Ok(())
}

async fn ratings_change(c: &mut Ctx) -> R {
    let Some((m, players)) = c.rated.clone() else {
        return skip("no rated match was played (no eligible rated queue)");
    };
    let o = &m["outcome"];
    let changes = o["rating_changes"].as_array().cloned().unwrap_or_default();
    check!(
        changes.len() == players.len(),
        "rating_changes has {} entries for {} players",
        changes.len(),
        players.len()
    );
    for ch in &changes {
        check!(
            ch["user_id"].is_string()
                && ch["ladder"].is_string()
                && ch["delta"].is_number()
                && ch["reason"].is_object(),
            "rating change shape: {}",
            compact(ch)
        );
    }
    let idx = c.pool(1).await?;
    let r = c.pool[idx[0]]
        .api
        .get(&format!("/v1/users/{}/ratings", players[0]))
        .await;
    expect(&r, 200, "GET /v1/users/{id}/ratings")?;
    let ladder = m["rules"]["ladder"].clone();
    check!(
        r.body["items"]
            .as_array()
            .is_some_and(|i| i.iter().any(|v| v["ladder"] == ladder)),
        "no rating view on the match's ladder"
    );
    Ok(())
}

async fn leaderboards_shape(c: &mut Ctx) -> R {
    let idx = c.pool(1).await?;
    let r = c.pool[idx[0]].api.get("/v1/ladders").await;
    let ladders: Vec<String> = r.body["items"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|l| l["leaderboard"] == true)
        .filter_map(|l| l["id"].as_str().map(str::to_string))
        .collect();
    if ladders.is_empty() {
        return skip("no ladder with a leaderboard");
    }
    for l in ladders {
        let r = c.pool[idx[0]]
            .api
            .get(&format!("/v1/ladders/{l}/leaderboard?limit=5"))
            .await;
        expect(&r, 200, "GET /v1/ladders/{id}/leaderboard")?;
        let cols = r.body["columns"].as_array().cloned().unwrap_or_default();
        check!(
            !cols.is_empty()
                && cols
                    .iter()
                    .all(|c| c["key"].is_string() && c["label"].is_object() && c["type"].is_string()),
            "columns shape"
        );
        check!(
            r.body["items"].is_array() && (r.body["next_cursor"].is_null() || r.body["next_cursor"].is_string()),
            "items / next_cursor"
        );
        for row in r.body["items"].as_array().cloned().unwrap_or_default() {
            check!(row["cells"].is_object(), "a row without cells");
        }
        let r = c.pool[idx[0]]
            .api
            .get(&format!("/v1/ladders/{l}/leaderboard?cursor=garbage!"))
            .await;
        expect_code(&r, 400, "bad_cursor", "an unknown cursor")?;
    }
    Ok(())
}

async fn stats_history(c: &mut Ctx) -> R {
    let Some((m, players)) = c.finished.clone() else {
        return skip("no completed match to look up");
    };
    let idx = c.pool(1).await?;
    let mid = m["id"].as_str().unwrap_or_default();
    let r = c.pool[idx[0]]
        .api
        .get(&format!("/v1/users/{}/matches", players[0]))
        .await;
    expect(&r, 200, "GET /v1/users/{id}/matches")?;
    let item = r.body["items"]
        .as_array()
        .and_then(|i| i.iter().find(|x| x["match_id"] == mid).cloned());
    let Some(item) = item else {
        return fail("the completed match is not in the player's history");
    };
    let winner = m["outcome"]["winner"].clone();
    let expected = if winner.is_null() {
        "none"
    } else if item["team"] == winner {
        "win"
    } else {
        "loss"
    };
    check!(
        item["result"] == expected,
        "history result {} expected {expected}",
        item["result"]
    );
    let r = c.pool[idx[0]].api.get(&format!("/v1/users/{}/stats", players[0])).await;
    expect(&r, 200, "GET /v1/users/{id}/stats")?;
    check!(
        r.body["groups"]
            .as_array()
            .is_some_and(|g| !g.is_empty() && g.iter().all(|x| x["key"].is_string() && x["fields"].is_array())),
        "stats groups shape"
    );
    let r = c.pool[idx[0]].api.get(&format!("/v1/matches/{mid}")).await;
    check!(
        r.status == 200 && r.body["state"] == "completed",
        "an ended match's view"
    );
    Ok(())
}

// ---------------------------------------------------------------- runner

/// Runs the suite.
pub async fn run(opts: Options) -> Report {
    let anon = Api::new(&opts.api);
    let mut report = Report::default();
    let meta = anon.get("/v1/meta").await;
    if meta.status != 200 {
        report.results.push(Outcome {
            id: "core.meta".into(),
            area: "core".into(),
            status: Status::Fail(format!("GET /v1/meta: {} {}", meta.status, compact(&meta.body))),
        });
        return report;
    }
    let features: HashSet<String> = meta.body["features"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|x| x.as_str().map(str::to_string))
        .collect();
    report.declared = AREAS
        .iter()
        .filter(|a| features.contains(**a))
        .map(|a| a.to_string())
        .collect();
    let providers = meta.body["auth"]["providers"].as_array().cloned().unwrap_or_default();
    let direct = providers.iter().find(|p| {
        p["kind"] == "direct"
            && opts.provider.as_ref().is_none_or(|w| p["id"] == w.as_str())
            && p["fields"]
                .as_array()
                .is_some_and(|f| f.len() == 1 && f[0]["type"] == "string")
    });
    let preferred = opts
        .provider
        .clone()
        .or_else(|| providers.iter().find(|p| p["id"] == "dev").and(Some("dev".to_string())));
    let direct = preferred
        .as_ref()
        .and_then(|w| providers.iter().find(|p| p["id"] == w.as_str()))
        .filter(|p| p["kind"] == "direct")
        .or(direct);
    let login = if !opts.tokens.is_empty() {
        Login::Tokens
    } else if let Some(p) = direct {
        Login::Direct {
            provider: p["id"].as_str().unwrap_or_default().to_string(),
            field: p["fields"][0]["key"].as_str().unwrap_or_default().to_string(),
        }
    } else {
        report.results.push(Outcome {
            id: "setup".into(),
            area: "core".into(),
            status: Status::Skip("no direct login provider: give --tokens".into()),
        });
        return report;
    };
    let run_tag = format!(
        "{:04}",
        (std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_millis())
            .unwrap_or(0)
            % 10_000) as u32
    );
    let mut c = Ctx {
        test_clock: features.contains("test-clock"),
        features,
        meta: meta.body,
        anon,
        run: run_tag,
        counter: 0,
        login,
        tokens: opts.tokens.iter().rev().cloned().collect(),
        pool: Vec::new(),
        results: Vec::new(),
        finished: None,
        rated: None,
        series_seen: Vec::new(),
        opts,
    };
    macro_rules! scenario {
        ($id:expr, $area:expr, $f:expr) => {
            if c.wanted($area) {
                if $area != "core" && !c.declared($area) {
                    c.record($id, $area, Err(Why::Skip("area not declared".into())));
                } else {
                    let r = $f(&mut c).await;
                    c.record($id, $area, r);
                    c.reset().await;
                }
            }
        };
    }
    scenario!("core.discovery", "core", core_discovery);
    scenario!("core.meta", "core", core_meta);
    scenario!("core.errors", "core", core_errors);
    scenario!("core.login", "core", core_login);
    scenario!("core.oauth", "core", core_oauth);
    scenario!("core.password", "core", core_password);
    scenario!("core.password.lockout", "core", core_password_lockout);
    scenario!("core.account", "core", core_account);
    scenario!("core.events", "core", core_events);
    scenario!("core.capabilities", "core", core_capabilities);
    scenario!("core.relays", "core", core_relays);
    scenario!("core.relay.status", "core", core_relay_status);
    for id in [
        "core.match.contract",
        "core.relay.join",
        "core.match.agreed",
        "core.match.disagreement",
        "core.match.silent",
        "core.match.error",
        "core.match.invalid",
        "core.match.crash",
        "core.match.counters",
        "core.match.counters-gap",
        "core.match.forged",
        "core.match.forged-crash",
    ] {
        if c.wanted("core") {
            let r = core_match(&mut c, id).await;
            c.record(id, "core", r);
            c.reset().await;
        }
    }
    scenario!("core.match.free", "core", core_match_free);
    scenario!("core.match.leave", "core", core_match_leave);
    scenario!("rooms.declaration", "rooms", rooms_declaration);
    scenario!("rooms.lifecycle", "rooms", rooms_lifecycle);
    scenario!("rooms.full", "rooms", rooms_full);
    scenario!("rooms.kick-leave-owner", "rooms", rooms_kick_leave_owner);
    scenario!("queues.list", "queues", queues_list);
    scenario!("queues.match", "queues", queues_match);
    scenario!("queues.cancel", "queues", queues_cancel);
    scenario!("queues.decline", "queues", queues_decline);
    scenario!("queues.timeout", "queues", queues_timeout);
    scenario!("queues.shrink", "queues", queues_shrink);
    scenario!("parties.lifecycle", "parties", parties_lifecycle);
    scenario!("parties.queue", "parties", parties_queue);
    scenario!("friends.requests", "friends", friends_requests);
    scenario!("presence.friends", "presence", friends_presence);
    scenario!("ratings.ladders", "ratings", ratings_ladders);
    scenario!("ratings.change", "ratings", ratings_change);
    scenario!("leaderboards.shape", "leaderboards", leaderboards_shape);
    scenario!("stats.history", "stats", stats_history);
    scenario!("moderation.report", "moderation", moderation_report);
    scenario!("moderation.block", "moderation", moderation_block);
    scenario!("activities.views", "activities", activities_views);
    scenario!("activities.play", "activities", activities_play);
    scenario!("series.invariants", "series", series_invariants);
    scenario!("badges.shape", "badges", badges_shape);
    scenario!("profiles.view", "profiles", profiles_view);
    scenario!("population.counts", "population", population_counts);
    scenario!("draft.declaration", "draft", draft_declaration);
    scenario!("draft.play", "draft", draft_play);
    scenario!("draft.timeout", "draft", draft_timeout);
    scenario!("draft.no-capability", "draft", draft_no_capability);
    scenario!("draft.leave", "draft", draft_leave);
    scenario!("spectators.declaration", "spectators", spectators_declaration);
    scenario!("spectators.no-capability", "spectators", spectators_no_capability);
    scenario!("spectators.room", "spectators", spectators_room);
    scenario!("spectators.late", "spectators", spectators_late);
    scenario!("core.reload", "core", core_reload);
    scenario!("core.erase", "core", core_erase);
    for p in c.pool.drain(..) {
        if let Some(ev) = p.events {
            ev.close().await;
        }
    }
    report.results = c.results;
    report
}

// ---------------------------------------------------------------- 1.1: capabilities, activities, series

async fn core_capabilities(c: &mut Ctx) -> R {
    let p = c.new_player().await?;
    let body = json!({
        "contract": {"id": "lan-auto-match", "version": 1},
        "contracts": [{"id": "lan-auto-match", "versions": [1, 2]}],
        "build": "testkit",
        "engine": {"rule_keys": ["players", "teams", "mode", "stage"], "rounds": true, "max_players": 8,
                   "max_per_team": 4, "uneven_teams": true, "spectators": false, "loadout": ["weapon"],
                   "x-future-capability": {"anything": true}},
        "stages": [3, "7"],
        "options": {"modes": [{"key": "m1"}], "stages": [{"id": 3}]},
    });
    let r = p.api.put("/v1/me/game", body).await;
    expect(&r, 204, "PUT /v1/me/game with a 1.1 body")?;
    let r = p
        .api
        .put(
            "/v1/me/game",
            json!({"contract": {"id": "lan-auto-match", "version": 1}}),
        )
        .await;
    expect(&r, 204, "PUT /v1/me/game with a 1.0 body")?;
    Ok(())
}

const COMPONENTS: &[&str] = &[
    "text",
    "notice",
    "fields",
    "list",
    "table",
    "countdown",
    "progress",
    "bracket",
    "series",
    "match",
    "user",
    "form",
    "actions",
    "link",
    "group",
];

fn text_ok(v: &Value, max: usize) -> bool {
    match v {
        Value::Null => true,
        Value::String(t) => t.chars().count() <= max,
        Value::Object(m) => m.values().all(|x| x.as_str().is_some_and(|t| t.chars().count() <= max)),
        Value::Number(_) => true,
        _ => false,
    }
}

fn check_action(a: &Value) -> R {
    check!(
        a["id"].is_string() && a["label"].is_object() && a["enabled"].is_boolean(),
        "action shape: {}",
        compact(a)
    );
    check!(text_ok(&a["label"], 120), "an action label is too long");
    Ok(())
}

/// Checks one component and its children; returns the number of components.
fn check_component(v: &Value, depth: usize) -> Result<usize, Why> {
    check!(depth <= 5, "views nest more than 4 groups");
    let t = v["type"].as_str().unwrap_or_default();
    check!(!t.is_empty(), "a component without type: {}", compact(v));
    let mut n = 1;
    for k in ["text", "title", "label", "subtitle"] {
        check!(
            text_ok(&v[k], if k == "text" { 2000 } else { 120 }),
            "component {t}: `{k}` too long or not text"
        );
    }
    match t {
        "list" => {
            let items = v["items"].as_array().cloned().unwrap_or_default();
            check!(items.len() <= 500, "a list over 500 items");
            for i in &items {
                check!(
                    i["title"].is_object() || i["title"].is_string(),
                    "list item without title"
                );
                if !i["action"].is_null() {
                    check_action(&i["action"])?;
                }
            }
        }
        "table" => {
            check!(
                v["columns"].is_array() && v["rows"].as_array().is_some_and(|r| r.len() <= 500),
                "table shape"
            );
        }
        "fields" => check!(
            v["items"]
                .as_array()
                .is_some_and(|i| i.iter().all(|x| x["label"].is_object())),
            "fields shape"
        ),
        "bracket" => {
            let rounds = v["rounds"].as_array().cloned().unwrap_or_default();
            check!(rounds.len() <= 8, "a bracket over 8 rounds");
            for r in &rounds {
                let ms = r["matches"].as_array().cloned().unwrap_or_default();
                check!(
                    ms.len() <= 128 && ms.iter().all(|m| m["slots"].is_array() && m["state"].is_string()),
                    "bracket match shape"
                );
            }
        }
        "countdown" => check!(v["until"].is_string(), "countdown without until"),
        "progress" => check!(v["value"].is_number() && v["max"].is_number(), "progress shape"),
        "series" => check!(v["series_id"].is_string(), "series component without series_id"),
        "match" => check!(v["match_id"].is_string(), "match component without match_id"),
        "user" => check!(v["user"]["id"].is_string(), "user component without user"),
        "form" => {
            check!(v["fields"].is_array(), "form without fields");
            check_action(&v["submit"])?;
        }
        "actions" => {
            for a in v["items"].as_array().cloned().unwrap_or_default() {
                check_action(&a)?;
            }
        }
        "link" => check!(
            v["url"].as_str().is_some_and(|u| u.starts_with("https://")),
            "a link that is not https"
        ),
        "group" => {
            for ch in v["children"].as_array().cloned().unwrap_or_default() {
                n += check_component(&ch, depth + 1)?;
            }
        }
        _ => {}
    }
    if !COMPONENTS.contains(&t) && !t.starts_with("x-") {
        check!(
            v["fallback"].is_object() || v["fallback"].is_null(),
            "unknown component {t}: fallback must be text"
        );
    }
    Ok(n)
}

fn check_view(v: &Value) -> R {
    check!(
        v["id"].is_string() && v["revision"].is_u64() && v["title"].is_object(),
        "view shape: {}",
        compact(v)
    );
    check!(v.to_string().len() <= 256 * 1024, "a view over 256 KiB");
    let mut count = 0;
    for s in v["sections"].as_array().cloned().unwrap_or_default() {
        count += check_component(&s, 1)?;
    }
    check!(count <= 500, "a view over 500 components");
    let actions = v["actions"].as_array().cloned().unwrap_or_default();
    check!(actions.len() <= 50, "a view over 50 actions");
    for a in &actions {
        check_action(a)?;
    }
    Ok(())
}

async fn activities_views(c: &mut Ctx) -> R {
    let idx = c.pool(1).await?;
    let api = c.pool[idx[0]].api.clone();
    let r = api.get("/v1/activities").await;
    expect(&r, 200, "GET /v1/activities")?;
    let items = r.body["items"].as_array().cloned().unwrap_or_default();
    for a in &items {
        check!(
            a["id"].is_string()
                && a["title"].is_object()
                && a["kind"].is_object()
                && a["state"].is_string()
                && a["revision"].is_u64(),
            "activity summary: {}",
            compact(a)
        );
        let id = a["id"].as_str().unwrap_or_default();
        let v = api.get(&format!("/v1/activities/{id}")).await;
        expect(&v, 200, "GET /v1/activities/{id}")?;
        check_view(&v.body)?;
        if let Some(join) = a["join"]["action"].as_str() {
            let declared = v.body["actions"]
                .as_array()
                .is_some_and(|acts| acts.iter().any(|x| x["id"] == join));
            check!(declared, "the join action {join} is not an action of the view");
        }
    }
    let r = api.post("/v1/activities/no-such-activity/actions/x", json!({})).await;
    expect_code(&r, 404, "not_found", "an unknown activity")?;
    if let Some(a) = items.first() {
        let id = a["id"].as_str().unwrap_or_default();
        let r = api
            .post(&format!("/v1/activities/{id}/actions/no-such-action"), json!({}))
            .await;
        expect_code(&r, 404, "not_found", "an unknown action")?;
    }
    Ok(())
}

/// Plays every match the players of `who` are in, until none appears for a while; returns how many.
async fn play_all(c: &mut Ctx, who: &[usize], limit: usize) -> Result<usize, Why> {
    let mut played = 0;
    let mut idle = 0;
    while played < limit && idle < 4 {
        let mut found: Option<Value> = None;
        for &i in who {
            let r = c.pool[i].api.get("/v1/matches/current").await;
            if r.body["match"].is_object() {
                found = Some(r.body["match"].clone());
                break;
            }
        }
        let Some(m) = found else {
            idle += 1;
            c.kick().await;
            continue;
        };
        idle = 0;
        let mid = m["id"].as_str().unwrap_or_default().to_string();
        let ids: Vec<String> = m["participants"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|p| p["user"]["id"].as_str().map(str::to_string))
            .collect();
        let players: Vec<usize> = ids
            .iter()
            .filter_map(|id| c.pool.iter().position(|p| &p.id == id))
            .collect();
        check!(
            players.len() == ids.len(),
            "a match with players outside the suite's accounts"
        );
        if m["state"] == "pending_accept" {
            for &i in &players {
                let _ = c.pool[i].api.post_empty(&format!("/v1/matches/{mid}/accept")).await;
            }
        }
        if let Some(sid) = m["series"]["id"].as_str()
            && !c.series_seen.iter().any(|x| x == sid)
        {
            c.series_seen.push(sid.to_string());
        }
        let mut views = Vec::new();
        for &i in &players {
            let mut v = Value::Null;
            for _ in 0..10 {
                let r = c.pool[i].api.get(&format!("/v1/matches/{mid}")).await;
                if r.body["game"].is_object() {
                    v = r.body;
                    break;
                }
                tokio::time::sleep(Duration::from_millis(200)).await;
            }
            check!(v.is_object(), "match {mid} never became ready");
            views.push(v);
        }
        c.play(&players, &views, &|_, _| Play::Honest).await?;
        played += 1;
    }
    Ok(played)
}

async fn activities_play(c: &mut Ctx) -> R {
    let idx = c.pool(1).await?;
    let r = c.pool[idx[0]].api.get("/v1/activities").await;
    expect(&r, 200, "GET /v1/activities")?;
    let mut total = 0;
    for a in r.body["items"].as_array().cloned().unwrap_or_default() {
        let (Some(id), Some(join)) = (
            a["id"].as_str().map(str::to_string),
            a["join"]["action"].as_str().map(str::to_string),
        ) else {
            continue;
        };
        if a["state"] != "open" {
            continue;
        }
        let n = a["participants"]["capacity"]
            .as_u64()
            .map(|c| c as usize)
            .unwrap_or(2)
            .clamp(2, 8);
        let who = c.pool(n).await?;
        for &i in &who {
            let v = c.pool[i].api.get(&format!("/v1/activities/{id}")).await;
            let rev = v.body["revision"].clone();
            let r = c.pool[i]
                .api
                .post(
                    &format!("/v1/activities/{id}/actions/{join}"),
                    json!({"values": {}, "revision": rev}),
                )
                .await;
            expect(&r, 200, &format!("joining activity {id} with {join}"))?;
            if !r.body["view"].is_null() {
                check_view(&r.body["view"])?;
            }
        }
        let played = play_all(c, &who, 40).await?;
        check!(played > 0, "activity {id}: joining with {n} players created no match");
        total += played;
        let v = c.pool[who[0]].api.get(&format!("/v1/activities/{id}")).await;
        check_view(&v.body)?;
        c.reset().await;
    }
    if total == 0 {
        return skip("no open activity with a join action");
    }
    Ok(())
}

async fn series_invariants(c: &mut Ctx) -> R {
    if c.series_seen.is_empty() {
        return skip("no series was played (activities.play creates them)");
    }
    let idx = c.pool(1).await?;
    for sid in c.series_seen.clone() {
        let r = c.pool[idx[0]].api.get(&format!("/v1/series/{sid}")).await;
        expect(&r, 200, "GET /v1/series/{id}")?;
        let s = &r.body;
        check!(
            s["sides"].as_array().map(Vec::len) == Some(2) && s["games"].is_array() && s["state"].is_string(),
            "series shape: {}",
            compact(s)
        );
        let wins: Vec<u64> = s["sides"]
            .as_array()
            .into_iter()
            .flatten()
            .map(|x| x["wins"].as_u64().unwrap_or(0))
            .collect();
        let by_games = |team: &str| {
            s["games"]
                .as_array()
                .into_iter()
                .flatten()
                .filter(|g| g["winner"] == team)
                .count() as u64
        };
        check!(
            wins == vec![by_games("alpha"), by_games("bravo")],
            "series {sid}: wins {wins:?} do not match its games"
        );
        if s["format"]["kind"] == "best_of" && s["state"] == "completed" {
            let need = s["format"]["games"].as_u64().unwrap_or(1) / 2 + 1;
            check!(
                wins.contains(&need),
                "series {sid} completed without a side reaching {need} wins"
            );
            let winner = s["winner"].as_str().unwrap_or_default();
            let wi = if winner == "alpha" { 0 } else { 1 };
            check!(
                wins[wi] == need,
                "series {sid}: the winner is not the side with {need} wins"
            );
        }
        check!(
            s["state"] != "running",
            "series {sid} still running after its activity stopped creating matches"
        );
    }
    Ok(())
}

async fn badges_shape(c: &mut Ctx) -> R {
    let idx = c.pool(1).await?;
    let p = &c.pool[idx[0]];
    let r = p.api.get("/v1/titles").await;
    expect(&r, 200, "GET /v1/titles")?;
    check!(
        r.body["items"]
            .as_array()
            .is_some_and(|i| i.iter().all(|t| t["id"].is_string() && t["unlocked"].is_boolean())),
        "titles shape"
    );
    let r = p.api.get(&format!("/v1/users/{}/badges", p.id)).await;
    expect(&r, 200, "GET /v1/users/{id}/badges")?;
    check!(
        r.body["items"]
            .as_array()
            .is_some_and(|i| i.iter().all(|b| b["id"].is_string() && b["name"].is_object())),
        "badges shape"
    );
    Ok(())
}

async fn profiles_view(c: &mut Ctx) -> R {
    let idx = c.pool(1).await?;
    let p = &c.pool[idx[0]];
    let r = p.api.get(&format!("/v1/users/{}/profile", p.id)).await;
    expect(&r, 200, "GET /v1/users/{id}/profile")?;
    let mut count = 0;
    for s in r.body["sections"].as_array().cloned().unwrap_or_default() {
        count += check_component(&s, 1)?;
    }
    check!(count <= 500, "a profile over 500 components");
    Ok(())
}

// ---------------------------------------------------------------- logins: OAuth loopback and password forms

/// RFC 7636, appendix B.
const PKCE_VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const PKCE_CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";

/// Every `oauth_loopback` provider checks the redirect, keeps states single use and checks the PKCE verifier before
/// it calls the provider (so no provider is contacted, but for OpenID discovery).
async fn core_oauth(c: &mut Ctx) -> R {
    let providers: Vec<Value> = c.meta["auth"]["providers"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|p| p["kind"] == "oauth_loopback")
        .cloned()
        .collect();
    if providers.is_empty() {
        return skip("oauth_loopback providers: not declared");
    }
    let mut checked = 0;
    for p in providers {
        let id = p["id"].as_str().unwrap_or_default().to_string();
        let redirects: Vec<String> = p["loopback_redirects"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|r| r.as_str().map(str::to_string))
            .collect();
        let Some(redirect) = redirects.first().cloned() else {
            return fail(format!("{id}: no loopback_redirects"));
        };
        let begin =
            |uri: &str| json!({"redirect_uri": uri, "code_challenge": PKCE_CHALLENGE, "code_challenge_method": "S256"});
        let stranger = "http://127.0.0.1:1/kalamar-conformance";
        if !redirects.iter().any(|r| r == stranger) {
            let r = c.anon.post(&format!("/v1/auth/{id}/begin"), begin(stranger)).await;
            expect_code(
                &r,
                422,
                "validation_failed",
                &format!("{id}: begin with an unlisted redirect"),
            )?;
        }
        let r = c.anon.post(&format!("/v1/auth/{id}/begin"), begin(&redirect)).await;
        if r.status == 502 && r.code() == Some("provider_error") {
            // OpenID discovery could not reach the provider: nothing more to check here.
            continue;
        }
        expect(&r, 200, &format!("POST /v1/auth/{id}/begin"))?;
        let state = r.body["state"].as_str().unwrap_or_default().to_string();
        let url = r.body["authorize_url"].as_str().unwrap_or_default();
        check!(
            !state.is_empty() && (url.starts_with("https://") || url.starts_with("http://127.0.0.1")),
            "{id}: authorize_url is not an https URL: {url}"
        );
        check!(
            url.contains(&state) && r.body["expires_at"].is_string(),
            "{id}: the authorize_url does not carry the state"
        );
        let complete = |state: &str, verifier: &str| json!({"state": state, "code": "kalamar-conformance", "code_verifier": verifier});
        let r = c
            .anon
            .post(&format!("/v1/auth/{id}/complete"), complete(&state, &"x".repeat(43)))
            .await;
        expect_code(
            &r,
            400,
            "pkce_mismatch",
            &format!("{id}: complete with a wrong verifier"),
        )?;
        let r = c
            .anon
            .post(&format!("/v1/auth/{id}/complete"), complete(&state, PKCE_VERIFIER))
            .await;
        expect_code(&r, 400, "invalid_state", &format!("{id}: a state used once"))?;
        let r = c
            .anon
            .post(
                &format!("/v1/auth/{id}/complete"),
                complete("st_kalamar_conformance", PKCE_VERIFIER),
            )
            .await;
        expect_code(&r, 400, "invalid_state", &format!("{id}: an unknown state"))?;
        checked += 1;
    }
    if checked == 0 {
        return skip("every provider needs OpenID discovery the server could not reach");
    }
    Ok(())
}

/// A 1.1.5 password form of `purpose` (a `direct` provider with a secret field).
fn password_form(c: &Ctx, purpose: &str) -> Option<Value> {
    c.meta["auth"]["providers"]
        .as_array()?
        .iter()
        .find(|p| {
            p["kind"] == "direct"
                && p["purpose"] == purpose
                && p["fields"]
                    .as_array()
                    .is_some_and(|f| f.iter().any(|x| x["secret"] == true))
        })
        .cloned()
}

/// Fills a password form from its fields: the address, the password (every secret field), the display name.
fn fill(form: &Value, email: &str, password: &str, name: &str) -> Result<Value, Why> {
    let mut body = serde_json::Map::new();
    for f in form["fields"].as_array().into_iter().flatten() {
        let key = f["key"].as_str().unwrap_or_default();
        let v = if f["autocomplete"] == "email" || key == "email" {
            email
        } else if f["secret"] == true {
            password
        } else if key == "display_name" {
            name
        } else if f["required"] == true {
            return skip(format!("the form `{}` asks for `{key}`", form["id"]));
        } else {
            continue;
        };
        body.insert(key.to_string(), json!(v));
    }
    Ok(Value::Object(body))
}

fn same_error(a: &Resp, b: &Resp) -> bool {
    let e = |r: &Resp| {
        let x = &r.body["error"];
        (
            r.status,
            x["code"].clone(),
            x["message"].clone(),
            x["field"].clone(),
            x["text"].clone(),
        )
    };
    e(a) == e(b)
}

/// A fresh password account (by the sign-up form), its address and its session.
async fn password_account(c: &mut Ctx, password: &str) -> Result<(String, Value, Api), Why> {
    let Some(signup) = password_form(c, "signup") else {
        return skip("password sign-up form: not declared (1.1.5 `purpose`)");
    };
    if signup["fields"]
        .as_array()
        .is_some_and(|f| f.iter().any(|x| x["key"] == "code"))
    {
        return skip("sign-up needs a code sent by e-mail");
    }
    c.counter += 1;
    let email = format!("kt{}-{}@example.invalid", c.run, c.counter);
    let name: String = format!("p{}{}", c.run, c.counter)
        .chars()
        .take(c.display_max())
        .collect();
    let id = signup["id"].as_str().unwrap_or_default().to_string();
    let r = c
        .anon
        .post(&format!("/v1/auth/{id}/login"), fill(&signup, &email, password, &name)?)
        .await;
    expect(&r, 200, "sign-up with the password form")?;
    check!(r.body["created"] == true, "a sign-up did not create an account");
    let api = c.anon.with_token(r.body["access_token"].as_str().unwrap_or_default());
    Ok((email, r.body["me"].clone(), api))
}

async fn core_password(c: &mut Ctx) -> R {
    let Some(login) = password_form(c, "login") else {
        return skip("password forms: not declared (1.1.5 `purpose`)");
    };
    let login_id = login["id"].as_str().unwrap_or_default().to_string();
    let first = format!("Kalamar conformance {} one", c.run);
    let (email, me, owner) = password_account(c, &first).await?;
    let user = me["id"].as_str().unwrap_or_default().to_string();
    check!(
        me["password"]["changed_at"].is_string(),
        "1.1.5: Me has no `password` member"
    );
    check!(
        !me.to_string().contains("example.invalid"),
        "the Me object shows the address"
    );
    let path = format!("/v1/auth/{login_id}/login");
    let r = c
        .anon
        .post(&path, fill(&login, &email.to_uppercase(), &first, "")?)
        .await;
    expect(&r, 200, "login with the address in capitals")?;
    check!(r.body["me"]["id"] == user.as_str(), "the login found another account");
    let second = c.anon.with_token(r.body["access_token"].as_str().unwrap_or_default());
    // A wrong password and an unknown address answer exactly alike.
    let wrong = c
        .anon
        .post(&path, fill(&login, &email, "not the password at all", "")?)
        .await;
    expect_code(&wrong, 403, "wrong_password", "a wrong password")?;
    let unknown_addr = format!("kt{}-nobody@example.invalid", c.run);
    let unknown = c
        .anon
        .post(&path, fill(&login, &unknown_addr, "not the password at all", "")?)
        .await;
    check!(
        same_error(&wrong, &unknown),
        "an unknown address answers differently: {} / {}",
        compact(&wrong.body),
        compact(&unknown.body)
    );
    // Other players never see the address.
    let other = c.new_player().await?;
    let r = other.api.get(&format!("/v1/users/{user}")).await;
    expect(&r, 200, "GET /v1/users/{id} of a password account")?;
    check!(
        !r.body.to_string().contains("example.invalid"),
        "another player sees the address"
    );
    // POST /v1/me/password: the current password is checked, the other sessions end.
    let new = format!("Kalamar conformance {} two", c.run);
    let r = owner
        .post(
            "/v1/me/password",
            json!({"password": "not the password at all", "new_password": new}),
        )
        .await;
    expect_code(&r, 403, "wrong_password", "POST /v1/me/password with a wrong password")?;
    let r = owner
        .post("/v1/me/password", json!({"password": first, "new_password": new}))
        .await;
    expect(&r, 204, "POST /v1/me/password")?;
    expect(&owner.get("/v1/me").await, 200, "the session that changed the password")?;
    expect_code(
        &second.get("/v1/me").await,
        401,
        "unauthorized",
        "another session after a password change",
    )?;
    expect_code(
        &c.anon.post(&path, fill(&login, &email, &first, "")?).await,
        403,
        "wrong_password",
        "the old password",
    )?;
    expect(
        &c.anon.post(&path, fill(&login, &email, &new, "")?).await,
        200,
        "the new password",
    )?;
    expect(
        &owner.delete("/v1/me?confirm=erase").await,
        204,
        "erasing the password account",
    )?;
    Ok(())
}

/// Failed password checks end in `429` (only on servers declaring `test-clock`: it locks this machine's address for
/// password checks for a while).
async fn core_password_lockout(c: &mut Ctx) -> R {
    if !c.test_clock {
        return skip("locks this machine's address: run on a test server (test-clock)");
    }
    let Some(login) = password_form(c, "login") else {
        return skip("password forms: not declared (1.1.5 `purpose`)");
    };
    let (email, _, owner) = password_account(c, &format!("Kalamar conformance {} lock", c.run)).await?;
    let path = format!("/v1/auth/{}/login", login["id"].as_str().unwrap_or_default());
    let body = fill(&login, &email, "a guess that is wrong", "")?;
    let mut locked = None;
    for i in 0..60 {
        // No retry on 429 here: the lock is what is checked.
        let r = c.anon.call_once("POST", &path, Some(body.clone())).await;
        if r.status == 429 {
            locked = Some((i, r));
            break;
        }
        expect_code(&r, 403, "wrong_password", "a wrong password")?;
    }
    let Some((n, r)) = locked else {
        return fail("60 wrong passwords in a row were all checked: no lock");
    };
    check!(
        r.code() == Some("rate_limited") && r.retry_after.is_some(),
        "the lock is not 429 rate_limited with Retry-After"
    );
    check!(n >= 1, "locked before any failure");
    expect(
        &owner.delete("/v1/me?confirm=erase").await,
        204,
        "erasing the password account",
    )?;
    Ok(())
}

// ---------------------------------------------------------------- 1.1.4: population

/// The shape of `GET /v1/population`: `online` (and `in_match`, `searching` when present) non-negative integers,
/// `updated_at` RFC 3339 UTC.
fn check_population(v: &Value) -> R {
    check!(
        v["online"].is_u64(),
        "online is not a non-negative integer: {}",
        compact(v)
    );
    for k in ["in_match", "searching"] {
        check!(
            v.get(k).is_none_or(Value::is_u64),
            "{k} is not a non-negative integer: {}",
            compact(v)
        );
    }
    check!(
        v["updated_at"]
            .as_str()
            .is_some_and(|t| t.ends_with('Z') && t.len() >= 20),
        "updated_at is not RFC 3339 UTC"
    );
    let text = v.to_string();
    check!(
        !text.contains("user_id") && !text.contains("display_name"),
        "the population names players: {}",
        compact(v)
    );
    Ok(())
}

async fn population_counts(c: &mut Ctx) -> R {
    // Public: no token.
    let r = c.anon.get("/v1/population").await;
    expect(&r, 200, "GET /v1/population without a token")?;
    check_population(&r.body)?;
    check!(
        r.cache_control.as_deref().is_some_and(|h| h.contains("max-age")),
        "GET /v1/population without Cache-Control max-age (got {:?})",
        r.cache_control
    );
    // Two players with open event connections count as online once the cached answer is renewed.
    let idx = c.pool(2).await?;
    check!(idx.len() == 2, "pool");
    c.advance(11).await?;
    let r = c.anon.get("/v1/population").await;
    expect(&r, 200, "GET /v1/population")?;
    check_population(&r.body)?;
    let online = r.body["online"].as_u64().unwrap_or(0);
    check!(
        online >= 2,
        "two connected players, online = {online}: {}",
        compact(&r.body)
    );
    Ok(())
}

// ---------------------------------------------------------------- 1.1.6: draft

/// Where drafted matches come from: a room with a draft of its form, or a solo queue whose matches have one.
enum DraftSource {
    Room(String),
    Queue(String, usize),
}

/// The stage ids the suite's players declare: the draft's own pool when the server shows it, else made-up ids.
fn draft_stages(pool: &Value) -> Vec<String> {
    let listed: Vec<String> = pool["stages"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|s| s.as_str().map(str::to_string))
        .collect();
    if listed.is_empty() {
        (1..=6).map(|i| format!("kt-stage-{i}")).collect()
    } else {
        listed
    }
}

/// A source of drafted matches, and the stages to declare.
async fn draft_source(c: &mut Ctx) -> Result<(DraftSource, Vec<String>, Value), Why> {
    if c.declared("rooms")
        && let Some(f) = c.meta["rooms"]["options"]
            .as_array()
            .and_then(|o| o.iter().find(|f| f["key"] == "draft"))
        && let Some(v) = f["values"].as_array().and_then(|v| v.first())
    {
        let mut stages = draft_stages(&v["pool"]);
        // The room form's stages are the room drafts' pool when the preset has none.
        if v["pool"]["stages"].as_array().is_none_or(Vec::is_empty) {
            let room_stages: Vec<String> = c.meta["rooms"]["options"]
                .as_array()
                .and_then(|o| o.iter().find(|f| f["key"] == "stage"))
                .and_then(|f| f["values"].as_array())
                .into_iter()
                .flatten()
                .filter_map(|s| s["id"].as_str().map(str::to_string))
                .collect();
            if !room_stages.is_empty() {
                stages = room_stages;
            }
        }
        let id = v["id"].as_str().unwrap_or_default().to_string();
        return Ok((DraftSource::Room(id), stages, v.clone()));
    }
    if c.declared("queues") {
        let queues = queue_items(c).await?;
        if let Some(q) = queues
            .iter()
            .filter(|q| q["draft"].is_object() && q["party"]["min"].as_u64().unwrap_or(1) == 1)
            .min_by_key(|q| size(q))
        {
            let d = q["draft"].clone();
            return Ok((
                DraftSource::Queue(q["id"].as_str().unwrap_or_default().to_string(), size(q)),
                draft_stages(&d["pool"]),
                d,
            ));
        }
    }
    skip("no room form option `draft` and no queue with a `draft`")
}

/// Declares what the suite's simulated launcher supports, with or without the draft screen.
async fn declare_draft(c: &Ctx, who: &[usize], stages: &[String], drafts: bool) -> R {
    for &i in who {
        let mut body = json!({"contract": {"id": kalamar_protocol::CONTRACT_ID, "version": kalamar_protocol::CONTRACT_VERSION},
            "contracts": [{"id": kalamar_protocol::CONTRACT_ID, "versions": [1, 2]}], "build": "testkit", "stages": stages, "can_select_stage": true,
            "engine": {"match_loadout": true, "loadout": ["weapon", "gear"], "stage_list": "all"}});
        if drafts {
            body["launcher"] = json!({"draft": [1]});
        }
        let r = c.pool[i].api.put("/v1/me/game", body).await;
        expect(&r, 204, "PUT /v1/me/game with launcher.draft")?;
    }
    Ok(())
}

/// Creates a match of the source with the players `who` (fresh accounts) and returns its id once it is past its
/// accept phase (drafting, or already ready when nothing was drafted).
async fn drafted_match(c: &mut Ctx, who: &[usize], src: &DraftSource) -> Result<String, Why> {
    match src {
        DraftSource::Room(preset) => {
            let n = who.len();
            let choices: Vec<[u64; 2]> = c.meta["rooms"]["options"]
                .as_array()
                .and_then(|o| o.iter().find(|f| f["key"] == "teams"))
                .and_then(|f| serde_json::from_value(f["choices"].clone()).ok())
                .unwrap_or_default();
            let mut options = json!({"draft": preset});
            if let Some(ch) = choices.iter().find(|ch| (ch[0] + ch[1]) as usize == n) {
                options["teams"] = json!(ch);
            }
            let r = c.pool[who[0]].api.post("/v1/rooms", json!({"options": options})).await;
            expect(&r, 201, "POST /v1/rooms with a draft")?;
            check!(
                r.body["options"]["draft"] == preset.as_str(),
                "the room does not keep its draft option"
            );
            let room = r.body["id"].as_str().unwrap_or_default().to_string();
            let code = r.body["code"].as_str().unwrap_or_default().to_string();
            for &i in &who[1..] {
                let r = c.pool[i].api.post("/v1/rooms/join", json!({"code": code})).await;
                expect(&r, 200, "POST /v1/rooms/join")?;
                let r = c.pool[i]
                    .api
                    .post(&format!("/v1/rooms/{room}/ready"), json!({"ready": true}))
                    .await;
                expect(&r, 200, "ready")?;
            }
            let r = c.pool[who[0]].api.post_empty(&format!("/v1/rooms/{room}/start")).await;
            expect(&r, 201, "POST /v1/rooms/{id}/start")?;
            Ok(r.body["id"].as_str().unwrap_or_default().to_string())
        }
        DraftSource::Queue(q, _) => {
            for &i in who {
                let r = c.pool[i].api.post(&format!("/v1/queues/{q}/tickets"), json!({})).await;
                expect(&r, 201, "POST /v1/queues/{id}/tickets")?;
            }
            let found = c.wait_found(who).await?;
            let mid = found[0]["id"].as_str().unwrap_or_default().to_string();
            check!(
                found[0]["draft"].is_object(),
                "a match of a queue with a draft has no `draft` during its accept phase"
            );
            for &i in who {
                let r = c.pool[i].api.post_empty(&format!("/v1/matches/{mid}/accept")).await;
                expect(&r, 200, "accept")?;
            }
            Ok(mid)
        }
    }
}

/// The participant views of a match, in `who` order.
async fn views_of(c: &Ctx, who: &[usize], mid: &str) -> Result<Vec<Value>, Why> {
    let mut v = Vec::new();
    for &i in who {
        let r = c.pool[i].api.get(&format!("/v1/matches/{mid}")).await;
        expect(&r, 200, "GET /v1/matches/{id}")?;
        v.push(r.body);
    }
    Ok(v)
}

/// The draft object's shape and its invariants (`draft.md`, section 3).
fn check_draft(m: &Value) -> R {
    let d = &m["draft"];
    check!(d.is_object(), "no draft object: {}", compact(m));
    check!(d["version"] == 1, "draft.version");
    check!(d["title"].is_object(), "draft.title is localized text");
    let kinds = ["stage", "mode"];
    let opts = d["options"].as_array().cloned().unwrap_or_default();
    check!(!opts.is_empty(), "draft.options is empty");
    for o in &opts {
        check!(
            o["id"].is_string() && kinds.contains(&o["kind"].as_str().unwrap_or("")),
            "option shape: {}",
            compact(o)
        );
        check!(
            ["available", "banned", "picked", "unavailable"].contains(&o["state"].as_str().unwrap_or("")),
            "option state: {}",
            compact(o)
        );
    }
    let turns = d["turns"].as_array().cloned().unwrap_or_default();
    check!(!turns.is_empty(), "draft.turns is empty");
    let current: Vec<usize> = turns
        .iter()
        .enumerate()
        .filter(|(_, t)| t["state"] == "current")
        .map(|(i, _)| i)
        .collect();
    check!(current.len() <= 1, "more than one current turn");
    check!(
        d["current"] == json!(current.first()),
        "draft.current does not name the current turn"
    );
    if let Some(&i) = current.first() {
        check!(turns[i]["deadline"].is_string(), "the current turn has no deadline");
    }
    check!(
        d["teams"]["alpha"].is_object() && d["teams"]["bravo"].is_object(),
        "draft.teams"
    );
    check!(
        d["you"]["team"].is_string() && d["you"]["can_act"].is_boolean(),
        "draft.you"
    );
    Ok(())
}

/// Plays the current turn: every player who may act chooses the first options of the turn's kind still available.
async fn act_turn(c: &Ctx, who: &[usize], mid: &str, views: &[Value]) -> Result<bool, Why> {
    let d = &views[0]["draft"];
    let Some(turn) = d["current"].as_u64() else {
        return Ok(false);
    };
    let t = &d["turns"][turn as usize];
    let kind = t["kind"].clone();
    let n = t["count"].as_u64().unwrap_or(1) as usize;
    let ids: Vec<String> = d["options"]
        .as_array()
        .into_iter()
        .flatten()
        .filter(|o| o["kind"] == kind && o["state"] == "available")
        .take(n)
        .filter_map(|o| o["id"].as_str().map(str::to_string))
        .collect();
    let mut acted = false;
    for (k, &i) in who.iter().enumerate() {
        if views[k]["draft"]["you"]["can_act"] != true {
            continue;
        }
        let r = c.pool[i]
            .api
            .post(
                &format!("/v1/matches/{mid}/draft"),
                json!({"turn": turn, "choices": ids}),
            )
            .await;
        if r.status == 409 && r.code() == Some("draft_turn") {
            continue; // the team's choice was settled by a teammate a moment ago
        }
        expect(&r, 200, "POST /v1/matches/{id}/draft")?;
        acted = true;
    }
    Ok(acted)
}

/// Plays a ready match out (so that the players are free again).
async fn finish_ready(c: &mut Ctx, who: &[usize], mid: &str) -> R {
    let views = views_of(c, who, mid).await?;
    check!(
        views[0]["state"] == "ready",
        "the match is not ready after its draft: {}",
        views[0]["state"]
    );
    c.play(who, &views, &|_, _| Play::Honest).await?;
    Ok(())
}

async fn draft_declaration(c: &mut Ctx) -> R {
    let (_, _, d) = draft_source(c).await?;
    check!(
        d["id"].is_string() && d["label"].is_object(),
        "a declared draft has id and label: {}",
        compact(&d)
    );
    if let Some(s) = d.get("turn_s") {
        check!(s.as_u64().is_some_and(|s| s >= 1), "turn_s");
    }
    Ok(())
}

async fn draft_play(c: &mut Ctx) -> R {
    let (src, stages, _) = draft_source(c).await?;
    let n = match &src {
        DraftSource::Queue(_, n) => *n,
        DraftSource::Room(_) => 2,
    };
    let who = fresh(c, n).await?;
    declare_draft(c, &who, &stages, true).await?;
    let mid = drafted_match(c, &who, &src).await?;
    let mut views = views_of(c, &who, &mid).await?;
    if views[0]["state"] == "ready" {
        finish_ready(c, &who, &mid).await?;
        return skip("the server drafted nothing for these players (no stage they declare is in its pool)");
    }
    check!(
        views[0]["state"] == "drafting",
        "state {} where drafting was expected",
        views[0]["state"]
    );
    check!(views[0]["game"].is_null(), "a drafting match shows a game block");
    check_draft(&views[0])?;
    // Integrity: a player who cannot act on this turn is refused, a turn that is not the current one too.
    let turn = views[0]["draft"]["current"].as_u64().unwrap_or(0);
    if let Some(k) = (0..who.len()).find(|&k| {
        let team = &views[k]["draft"]["you"]["team"];
        !views[k]["draft"]["turns"][turn as usize]["teams"]
            .as_array()
            .is_some_and(|t| t.contains(team))
    }) {
        let r = c.pool[who[k]]
            .api
            .post(
                &format!("/v1/matches/{mid}/draft"),
                json!({"turn": turn, "choices": ["kt-none"]}),
            )
            .await;
        expect_code(&r, 403, "not_your_turn", "an action by a player of the other team")?;
    }
    if let Some(k) = (0..who.len()).find(|&k| views[k]["draft"]["you"]["can_act"] == true) {
        let r = c.pool[who[k]]
            .api
            .post(
                &format!("/v1/matches/{mid}/draft"),
                json!({"turn": turn + 50, "choices": []}),
            )
            .await;
        expect_code(&r, 409, "draft_turn", "an action on a turn that is not the current one")?;
    }
    let mut rounds = 0;
    while views[0]["state"] == "drafting" && rounds < 70 {
        rounds += 1;
        if !act_turn(c, &who, &mid, &views).await? {
            c.kick().await;
        }
        views = views_of(c, &who, &mid).await?;
        if views[0]["state"] == "drafting" {
            check_draft(&views[0])?;
        }
    }
    check!(
        views[0]["state"] == "ready",
        "the draft did not end: {}",
        views[0]["state"]
    );
    let d = &views[0]["draft"];
    check!(d["state"] == "done", "draft.state {} on a ready match", d["state"]);
    if let Some(stage) = d["result"]["stage"].as_str() {
        check!(
            views[0]["rules"]["stage"] == stage,
            "rules.stage is not the draft's stage"
        );
        check!(
            views[0]["game"]["rules"]
                .as_str()
                .is_some_and(|r| r.contains(&format!("stage={stage}"))),
            "the game's rules do not carry the draft's stage"
        );
    }
    if let Some(mode) = d["result"]["mode"].as_str() {
        check!(views[0]["rules"]["mode"] == mode, "rules.mode is not the draft's mode");
    }
    finish_ready(c, &who, &mid).await?;
    declare_draft(c, &who, &[], false).await
}

async fn draft_timeout(c: &mut Ctx) -> R {
    let (src, stages, d) = draft_source(c).await?;
    let n = match &src {
        DraftSource::Queue(_, n) => *n,
        DraftSource::Room(_) => 2,
    };
    let who = fresh(c, n).await?;
    declare_draft(c, &who, &stages, true).await?;
    let mid = drafted_match(c, &who, &src).await?;
    let mut views = views_of(c, &who, &mid).await?;
    if views[0]["state"] != "drafting" {
        finish_ready(c, &who, &mid).await?;
        return skip("the server drafted nothing for these players");
    }
    let turn_s = views[0]["draft"]["turn_s"]
        .as_u64()
        .or(d["turn_s"].as_u64())
        .unwrap_or(30);
    let turns = views[0]["draft"]["turns"].as_array().map(Vec::len).unwrap_or(1) as u64;
    if !c.test_clock && turn_s * turns > c.opts.max_wait {
        let _ = c.pool[who[0]]
            .api
            .post_empty(&format!("/v1/matches/{mid}/decline"))
            .await;
        return skip(format!(
            "needs {} s (no test-clock; --max-wait {})",
            turn_s * turns,
            c.opts.max_wait
        ));
    }
    let mut rounds = 0;
    while views[0]["state"] == "drafting" && rounds <= turns + 2 {
        rounds += 1;
        c.advance(turn_s + 1).await?;
        views = views_of(c, &who, &mid).await?;
    }
    check!(
        views[0]["state"] == "ready",
        "the draft waited after its turns ran out: {}",
        views[0]["state"]
    );
    let by: Vec<String> = views[0]["draft"]["turns"]
        .as_array()
        .into_iter()
        .flatten()
        .flat_map(|t| t["choices"].as_array().cloned().unwrap_or_default())
        .filter_map(|ch| ch["by"].as_str().map(str::to_string))
        .collect();
    check!(
        by.iter().all(|b| b == "timeout" || b == "server"),
        "a turn nobody played shows choices by {by:?}"
    );
    finish_ready(c, &who, &mid).await?;
    declare_draft(c, &who, &[], false).await
}

async fn draft_no_capability(c: &mut Ctx) -> R {
    let (src, stages, d) = draft_source(c).await?;
    let n = match &src {
        DraftSource::Queue(_, n) => *n,
        DraftSource::Room(_) => 2,
    };
    let who = fresh(c, n).await?;
    declare_draft(c, &who, &stages, false).await?;
    if d["required"] == true {
        // The server says the launcher must show the draft: a clear refusal, never a stuck match.
        match &src {
            DraftSource::Queue(q, _) => {
                let r = c.pool[who[0]]
                    .api
                    .post(&format!("/v1/queues/{q}/tickets"), json!({}))
                    .await;
                expect_code(
                    &r,
                    403,
                    "not_eligible",
                    "a queue whose draft is required, without launcher.draft",
                )?;
            }
            DraftSource::Room(_) => {
                let r = drafted_match(c, &who, &src).await;
                check!(
                    r.is_err(),
                    "a room whose draft is required started without launcher.draft"
                );
            }
        }
        return Ok(());
    }
    let mid = drafted_match(c, &who, &src).await?;
    // Nobody can draft: the server plays every turn at once.
    let mut views = views_of(c, &who, &mid).await?;
    for _ in 0..3 {
        if views[0]["state"] == "ready" {
            break;
        }
        c.kick().await;
        views = views_of(c, &who, &mid).await?;
    }
    check!(
        views[0]["state"] == "ready",
        "launchers without launcher.draft wait in state {}",
        views[0]["state"]
    );
    finish_ready(c, &who, &mid).await
}

async fn draft_leave(c: &mut Ctx) -> R {
    let (src, stages, _) = draft_source(c).await?;
    let n = match &src {
        DraftSource::Queue(_, n) => *n,
        DraftSource::Room(_) => 2,
    };
    let who = fresh(c, n).await?;
    declare_draft(c, &who, &stages, true).await?;
    let mid = drafted_match(c, &who, &src).await?;
    let views = views_of(c, &who, &mid).await?;
    if views[0]["state"] != "drafting" {
        finish_ready(c, &who, &mid).await?;
        return skip("the server drafted nothing for these players");
    }
    let r = c.pool[who[1]]
        .api
        .post_empty(&format!("/v1/matches/{mid}/decline"))
        .await;
    expect(&r, 200, "POST /v1/matches/{id}/decline during the draft")?;
    check!(
        r.body["state"] == "cancelled" && r.body["cancel_reason"] == "left",
        "abandoning the draft gives {} / {}",
        r.body["state"],
        r.body["cancel_reason"]
    );
    let cur = c.pool[who[0]].api.get("/v1/matches/current").await;
    check!(
        cur.body["match"].is_null(),
        "the other player is still in the abandoned match"
    );
    Ok(())
}

// ---------------------------------------------------------------- 1.1.7: places in the stands (spectators.md)

/// What the suite's simulated launcher declares: contract 2 and, with `watch`, the 1.1.7 spectator seat (an engine
/// with places for spectators and a launcher that runs the seat). Without it, only the 1.1 boolean.
async fn declare_spectate(c: &Ctx, who: &[usize], watch: bool) -> R {
    for &i in who {
        let mut body = json!({"contract": {"id": kalamar_protocol::CONTRACT_ID, "version": kalamar_protocol::CONTRACT_VERSION},
            "contracts": [{"id": kalamar_protocol::CONTRACT_ID, "versions": [1, 2]}], "build": "testkit",
            "engine": {"match_loadout": true, "loadout": ["weapon", "gear"], "spectators": true}});
        if watch {
            body["engine"]["spectator_seats"] = json!({"max": 2, "host": true});
            body["launcher"] = json!({"spectate": [1]});
        }
        let r = c.pool[i].api.put("/v1/me/game", body).await;
        expect(&r, 204, "PUT /v1/me/game with engine.spectator_seats")?;
    }
    Ok(())
}

async fn spectators_declaration(c: &mut Ctx) -> R {
    let s = c.meta["spectators"].clone();
    check!(
        s.is_object() && s["version"] == 1,
        "GET /v1/meta has no `spectators` object of version 1"
    );
    check!(
        s["late"] == false,
        "`spectators.late`: the game cannot seat a spectator after the start"
    );
    if c.declared("rooms") {
        for f in c.meta["rooms"]["options"].as_array().cloned().unwrap_or_default() {
            if f["key"] == "spectators" {
                check!(
                    f["type"] == "integer" && f["max"].as_u64().is_some_and(|m| (1..=2).contains(&m)),
                    "the room option `spectators` is not an integer field of at most 2: {}",
                    compact(&f)
                );
            }
            if f["key"] == "spectators_who" {
                check!(
                    f["type"] == "enum",
                    "the room option `spectators_who` is not an enum field"
                );
            }
        }
    }
    for p in c.meta["result_policies"].as_array().cloned().unwrap_or_default() {
        if let Some(w) = p.get("witness") {
            check!(
                matches!(w.as_str(), Some("ignore" | "tiebreak" | "required")),
                "result policy {} has witness {w}",
                p["id"]
            );
        }
    }
    let idx = c.pool(1).await?;
    if c.declared("queues") {
        let r = c.pool[idx[0]].api.get("/v1/queues").await;
        expect(&r, 200, "GET /v1/queues")?;
        for q in r.body["items"].as_array().cloned().unwrap_or_default() {
            let sp = &q["spectators"];
            check!(
                sp.is_null() || sp["seats"].as_u64().is_some_and(|n| (1..=2).contains(&n)),
                "queue {} has spectators {}",
                q["id"],
                compact(sp)
            );
        }
    }
    let r = c.pool[idx[0]].api.get("/v1/spectate").await;
    expect(&r, 200, "GET /v1/spectate")?;
    check!(r.body["items"].is_array(), "GET /v1/spectate has no `items` array");
    Ok(())
}

/// The room form's most places in the stands (skips when rooms have none).
fn room_stands(c: &Ctx) -> Result<u64, Why> {
    if !c.declared("rooms") {
        return skip("no rooms: the room scenarios of the stands need them");
    }
    let max = c.meta["rooms"]["options"]
        .as_array()
        .and_then(|o| o.iter().find(|f| f["key"] == "spectators"))
        .and_then(|f| f["max"].as_u64())
        .unwrap_or(0);
    if max == 0 {
        return skip("the rooms have no places in the stands");
    }
    Ok(max)
}

/// A room of `owner` with one place in the stands for anyone (when it can say so) and the smallest teams that fit
/// `players`: (room id, code).
async fn stands_room(c: &Ctx, owner: usize, players: usize) -> Result<(String, String), Why> {
    let options = c.meta["rooms"]["options"].as_array().cloned().unwrap_or_default();
    let choices: Vec<[u64; 2]> = options
        .iter()
        .find(|f| f["key"] == "teams")
        .and_then(|f| serde_json::from_value(f["choices"].clone()).ok())
        .unwrap_or_default();
    let mut body = json!({"spectators": 1});
    if let Some(ch) = choices
        .iter()
        .filter(|ch| (ch[0] + ch[1]) as usize >= players)
        .min_by_key(|ch| ch[0] + ch[1])
    {
        body["teams"] = json!(ch);
    }
    if let Some(w) = options.iter().find(|f| f["key"] == "spectators_who")
        && w["values"]
            .as_array()
            .is_some_and(|v| v.iter().any(|x| x["id"] == "anyone"))
    {
        body["spectators_who"] = json!("anyone");
    }
    let r = c.pool[owner].api.post("/v1/rooms", json!({"options": body})).await;
    expect(&r, 201, "POST /v1/rooms with places in the stands")?;
    Ok((
        r.body["id"].as_str().unwrap_or_default().to_string(),
        r.body["code"].as_str().unwrap_or_default().to_string(),
    ))
}

/// A started room match of `who[0]` (owner) and `who[1]` (player) watched by `who[2]`: the views in `who` order.
async fn watched_room_match(c: &mut Ctx, who: &[usize]) -> Result<(String, Vec<Value>), Why> {
    let (room, code) = stands_room(c, who[0], 2).await?;
    let r = c.pool[who[1]].api.post("/v1/rooms/join", json!({"code": code})).await;
    expect(&r, 200, "POST /v1/rooms/join")?;
    let r = c.pool[who[2]]
        .api
        .post("/v1/rooms/join", json!({"code": code, "seat": "spectator"}))
        .await;
    expect(&r, 200, "POST /v1/rooms/join with seat spectator")?;
    let me = c.pool[who[2]].id.clone();
    let seat = r.body["members"]
        .as_array()
        .and_then(|m| m.iter().find(|x| x["user"]["id"] == me.as_str()))
        .map(|x| x["team"].clone());
    check!(
        seat == Some(json!("spectator")),
        "the member who joined the stands has team {seat:?}"
    );
    for &i in &who[1..] {
        let r = c.pool[i]
            .api
            .post(&format!("/v1/rooms/{room}/ready"), json!({"ready": true}))
            .await;
        expect(&r, 200, "ready")?;
    }
    let r = c.pool[who[0]].api.post_empty(&format!("/v1/rooms/{room}/start")).await;
    expect(&r, 201, "POST /v1/rooms/{id}/start with a spectator")?;
    let mid = r.body["id"].as_str().unwrap_or_default().to_string();
    let views = views_of(c, who, &mid).await?;
    check!(
        views[0]["participants"].as_array().is_some_and(|p| p.len() == 2),
        "a spectator is listed among the participants (the players)"
    );
    check!(
        views[0]["spectators"].as_array().is_some_and(|s| s.len() == 1),
        "the players do not see the spectator: {}",
        compact(&views[0]["spectators"])
    );
    check!(
        views[2]["you"]["team"] == "spectator",
        "the spectator's `you.team` is {}",
        views[2]["you"]["team"]
    );
    let rules = views[2]["game"]["rules"].as_str().unwrap_or_default();
    let parsed = parse_rules(rules).ok_or_else(|| Why::Fail(format!("unreadable rules {rules}")))?;
    let name = views[2]["you"]["member_name"].as_str().unwrap_or_default();
    check!(
        parsed.spectators == [name.to_string()] && parsed.players == 3 && views[2]["game"]["players"] == 3,
        "the rules do not seat the spectator with the letter S and count it in players=: {rules}"
    );
    check!(
        parsed.other.iter().any(|(k, _)| k == "spectators_wait"),
        "the rules carry no spectators_wait: {rules}"
    );
    Ok((mid, views))
}

/// Waits for the decision of `mid` (moving time past the grace and the deadline), at `who[0]`.
async fn decided(c: &mut Ctx, who: usize, mid: &str, policy: &Option<Policy>) -> Result<Value, Why> {
    for attempt in 0..3 {
        let ev = c.pool[who].events.as_mut().expect("connected");
        if let Some(e) = ev
            .wait_any(
                &["match.completed", "match.void"],
                |e| e["data"]["id"] == mid,
                Duration::from_secs(4),
            )
            .await
        {
            return Ok(e["data"].clone());
        }
        if attempt < 2 {
            let grace = policy.as_ref().map(|p| p.deadlines.grace_s as u64).unwrap_or(120);
            let result = policy.as_ref().map(|p| p.deadlines.result_s as u64).unwrap_or(1800);
            c.advance(if attempt == 0 { grace + 1 } else { result + 1 }).await?;
        }
    }
    fail("the match was never decided")
}

async fn spectators_no_capability(c: &mut Ctx) -> R {
    room_stands(c)?;
    let who = fresh(c, 2).await?;
    declare_spectate(c, &who[..1], true).await?;
    declare_spectate(c, &who[1..], false).await?;
    let (room, code) = stands_room(c, who[0], 2).await?;
    let r = c.pool[who[1]]
        .api
        .post("/v1/rooms/join", json!({"code": code, "seat": "spectator"}))
        .await;
    expect_code(
        &r,
        409,
        "incompatible_game",
        "a launcher without the spectator seat joining the stands",
    )?;
    let missing = r.body["error"]["details"]["players"]
        .as_array()
        .and_then(|p| p.first())
        .map(|p| p["missing"].clone())
        .unwrap_or(Value::Null);
    check!(
        missing
            .as_array()
            .is_some_and(|m| m.contains(&json!("launcher:spectate"))),
        "the refusal does not say what is missing: {}",
        compact(&missing)
    );
    // It plays instead, never in the stands.
    let r = c.pool[who[1]].api.post("/v1/rooms/join", json!({"code": code})).await;
    expect(&r, 200, "POST /v1/rooms/join without a seat")?;
    let me = c.pool[who[1]].id.clone();
    let team = r.body["members"]
        .as_array()
        .and_then(|m| m.iter().find(|x| x["user"]["id"] == me.as_str()))
        .map(|x| x["team"].clone());
    check!(
        matches!(team.as_ref().and_then(Value::as_str), Some("alpha" | "bravo")),
        "a launcher that did not ask for the stands is in team {team:?}"
    );
    let _ = room;
    Ok(())
}

async fn spectators_room(c: &mut Ctx) -> R {
    room_stands(c)?;
    let who = fresh(c, 3).await?;
    declare_spectate(c, &who, true).await?;
    let (mid, views) = watched_room_match(c, &who).await?;
    let sim = Sim::from_match(&views[0]).ok_or_else(|| {
        Why::Fail(format!(
            "match view without a usable game block: {}",
            compact(&views[0])
        ))
    })?;
    let plays = vec![Play::Honest; sim.seats.len()];
    // The spectator launches before the stage loads; the players play; the spectator's game writes its file.
    let r = c.pool[who[2]]
        .api
        .post(&format!("/v1/matches/{mid}/launch"), json!({}))
        .await;
    expect(&r, 200, "a spectator's POST /v1/matches/{id}/launch before the start")?;
    for k in 0..2 {
        let seat = sim
            .seat_of(views[k]["you"]["member_name"].as_str().unwrap_or_default())
            .ok_or_else(|| Why::Fail("a player is not a participant".into()))?;
        let api = c.pool[who[k]].api.clone();
        expect(
            &api.post(&format!("/v1/matches/{mid}/launch"), json!({})).await,
            200,
            "launch",
        )?;
        for st in sim.statuses(seat, &plays[seat]) {
            expect(&api.post(&format!("/v1/matches/{mid}/status"), st).await, 204, "status")?;
        }
        if let Some(res) = sim.result_for(seat, &plays) {
            expect(
                &api.post(&format!("/v1/matches/{mid}/result"), res).await,
                200,
                "result",
            )?;
        }
    }
    let role = Role::parse(views[2]["you"]["role"].as_str().unwrap_or_default()).unwrap_or(Role::Join);
    let witness = sim.witness_result(role, &plays, None);
    let r = c.pool[who[2]]
        .api
        .post(&format!("/v1/matches/{mid}/result"), witness.clone())
        .await;
    // The decision may already be taken (a policy that ignores witnesses): then the upload is refused as late.
    check!(
        r.status == 200 || (r.status == 409 && r.code() == Some("match_state")),
        "a spectator's result upload got {} {}",
        r.status,
        compact(&r.body)
    );
    let policy = c.policy(views[0]["rules"]["result_policy"].as_str().unwrap_or_default());
    let m = decided(c, who[0], &mid, &policy).await?;
    let o = &m["outcome"];
    check!(
        o["reports"]["expected"] == 2,
        "a spectator counted among the expected reports: {}",
        compact(&o["reports"])
    );
    if let Some(p) = policy.filter(Policy::is_known) {
        let spec = MatchSpec {
            game_match_id: sim.game_match_id.clone(),
            mode: sim.mode.clone(),
            stage: sim.stage.clone(),
            rated: views[0]["rules"]["rated"] == true,
            loadout: None,
            seats: sim
                .seats
                .iter()
                .enumerate()
                .map(|(s, x)| Seat {
                    member_name: x.member_name.clone(),
                    team: x.team,
                    role: x.role,
                    launched: true,
                    saw_in_match: true,
                    report: sim.result_for(s, &plays),
                    reported_at: None,
                })
                .collect(),
            witnesses: vec![Witness {
                member_name: views[2]["you"]["member_name"].as_str().unwrap_or_default().to_string(),
                role,
                referee: views[2]["you"]["referee"] == true,
                launched: true,
                report: (r.status == 200).then_some(witness),
            }],
        };
        if let Some(d) = decide(&p, &spec) {
            check!(
                o["decision"] == d.decision && o["winner"] == json!(d.winner.map(Team::as_str)),
                "decision {} / winner {} where the declared policy gives {} / {:?}",
                o["decision"],
                o["winner"],
                d.decision,
                d.winner
            );
        }
    }
    let ended = c.pool[who[2]].api.get(&format!("/v1/matches/{mid}")).await;
    expect(&ended, 200, "the spectator's GET /v1/matches/{id}")?;
    let verdict = &ended.body["outcome"]["spectators"][0]["verdict"];
    check!(
        verdict.is_string(),
        "the outcome has no verdict for the spectator: {}",
        compact(&ended.body["outcome"])
    );
    check!(
        ended.body["outcome"]["players"]
            .as_array()
            .is_some_and(|p| p.iter().all(|x| x["user_id"] != c.pool[who[2]].id.as_str())),
        "the spectator has a player's verdict"
    );
    Ok(())
}

async fn spectators_late(c: &mut Ctx) -> R {
    room_stands(c)?;
    let who = fresh(c, 3).await?;
    declare_spectate(c, &who, true).await?;
    let (mid, views) = watched_room_match(c, &who).await?;
    let sim = Sim::from_match(&views[0]).ok_or_else(|| Why::Fail("match view without a game block".into()))?;
    let plays = vec![Play::Honest; sim.seats.len()];
    // A player's stage loads: the game cannot let a spectator in any more.
    let api = c.pool[who[0]].api.clone();
    expect(
        &api.post(&format!("/v1/matches/{mid}/launch"), json!({})).await,
        200,
        "launch",
    )?;
    let st = json!({"version": 1, "match_id": sim.game_match_id, "state": "loading-stage"});
    expect(
        &api.post(&format!("/v1/matches/{mid}/status"), st).await,
        204,
        "status loading-stage",
    )?;
    let r = c.pool[who[2]]
        .api
        .post(&format!("/v1/matches/{mid}/launch"), json!({}))
        .await;
    expect_code(
        &r,
        409,
        "match_started",
        "a spectator launching after a player's stage loads",
    )?;
    // Its launcher reports the refusal; the match goes on and counts.
    let refused = json!({"version": 2, "match_id": sim.game_match_id, "error": "launcher-spectator-late",
                         "reason": "the match had started"});
    let r = c.pool[who[2]]
        .api
        .post(&format!("/v1/matches/{mid}/result"), refused)
        .await;
    expect(&r, 200, "a spectator's error report")?;
    for k in 0..2 {
        let seat = sim
            .seat_of(views[k]["you"]["member_name"].as_str().unwrap_or_default())
            .unwrap_or(k);
        let api = c.pool[who[k]].api.clone();
        expect(
            &api.post(&format!("/v1/matches/{mid}/launch"), json!({})).await,
            200,
            "launch",
        )?;
        for st in sim.statuses(seat, &plays[seat]) {
            expect(&api.post(&format!("/v1/matches/{mid}/status"), st).await, 204, "status")?;
        }
        if let Some(res) = sim.result_for(seat, &plays) {
            expect(
                &api.post(&format!("/v1/matches/{mid}/result"), res).await,
                200,
                "result",
            )?;
        }
    }
    let policy = c.policy(views[0]["rules"]["result_policy"].as_str().unwrap_or_default());
    let m = decided(c, who[0], &mid, &policy).await?;
    check!(
        m["state"] == "completed",
        "a spectator who could not get in voided the match: {}",
        compact(&m["outcome"])
    );
    Ok(())
}
