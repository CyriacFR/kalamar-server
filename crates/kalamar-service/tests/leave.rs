//! A player who leaves a match (a crash, the window closed, the game's menu, a lost connection) through the HTTP API on
//! 127.0.0.1 with a manual clock, with the reports two real games and their launchers send in
//! 1v1 matches: the survivor's game says `connection-lost` 45 s after the other vanished, the leaver's launcher
//! `launcher-crashed` (crash, closed window) or `left-by-player` (the game's menu), or nothing when the whole computer
//! went down. In a 1v1 neither game writes a record (both stop when the session is lost), so the match
//! is void; these tests pin when the players are free again (1.1.10: a reporter at once) and what each `witness` value
//! makes of a spectator's report then. 1.1.11: a policy's `leave` makes a departure in play that the leaver's own
//! launcher reports (`left-by-player` with `in_play: true`) a forfeit of the leaver's team; everything else stays void.

use std::sync::Arc;

use kalamar_service::clock::ManualClock;
use kalamar_service::config::Config;
use serde_json::{Value, json};

fn config() -> toml::Table {
    r#"
[server]
name = "Leave"
api_url = "http://127.0.0.1/api"
production = false
database = ":memory:"
privacy_url = "https://example.invalid/privacy"
operator = { name = "T", contact = "mailto:t@example.invalid" }
[features]
test_clock = true
spectators = true
[auth.dev]
enabled = true
[[relays]]
host = "127.0.0.1"
[rate_limits]
per_ip_rps = 10000.0
per_ip_burst = 10000.0
per_user_rps = 10000.0
per_user_burst = 10000.0
auth_per_ip_per_min = 100000.0
accounts_per_ip_per_day = 0.0
room_starts_per_hour = 0.0
room_join_per_min = 10000.0
spectate_per_min = 10000.0
[penalties]
counted = ["decline", "accept_timeout", "no_show", "left"]
window_s = 86400
cooldowns_s = [60]
[[result_policies]]
id = "standard"
name = { en = "Standard" }
quorum = "majority"
silence = "consent_after_play"
disagreement = "majority"
rated_requires_both_teams = true
accept_s = 20
launch_s = 90
result_s = 2400
grace_s = 120
[[result_policies]]
id = "tiebreak"
name = { en = "A spectator breaks ties" }
quorum = "majority"
silence = "consent_after_play"
disagreement = "majority"
rated_requires_both_teams = true
accept_s = 20
launch_s = 90
result_s = 2400
grace_s = 120
witness = "tiebreak"
[[result_policies]]
id = "required"
name = { en = "A spectator confirms rated results" }
quorum = "majority"
silence = "consent_after_play"
disagreement = "majority"
rated_requires_both_teams = true
accept_s = 20
launch_s = 90
result_s = 2400
grace_s = 120
witness = "required"
[[result_policies]]
id = "ranked"
name = { en = "Standard, leaving loses" }
quorum = "majority"
silence = "consent_after_play"
disagreement = "majority"
rated_requires_both_teams = true
accept_s = 20
launch_s = 90
result_s = 2400
grace_s = 120
leave = "forfeit"
[[ladders]]
id = "duel"
name = { en = "Duel" }
[[queues]]
id = "duel"
name = { en = "Duel" }
teams = [1, 1]
rated = true
ladder = "duel"
pool = [{ mode = "turf" }]
result_policy = "standard"
spectators = { seats = 1, who = "anyone" }
[[queues]]
id = "duel-tiebreak"
name = { en = "Duel, tiebreak" }
teams = [1, 1]
rated = true
ladder = "duel"
pool = [{ mode = "turf" }]
result_policy = "tiebreak"
spectators = { seats = 1, who = "anyone" }
[[queues]]
id = "duel-required"
name = { en = "Duel, witness required" }
teams = [1, 1]
rated = true
ladder = "duel"
pool = [{ mode = "turf" }]
result_policy = "required"
spectators = { seats = 1, who = "anyone" }
[[queues]]
id = "duel-ranked"
name = { en = "Ranked duel, leaving loses" }
teams = [1, 1]
rated = true
ladder = "duel"
pool = [{ mode = "turf" }]
result_policy = "ranked"
spectators = { seats = 1, who = "anyone" }
[[queues]]
id = "duel-casual"
name = { en = "Unrated duel, leaving loses" }
teams = [1, 1]
pool = [{ mode = "turf" }]
result_policy = "ranked"
"#
    .parse()
    .expect("toml")
}

struct Server {
    running: kalamar_service::app::Running,
    base: String,
    http: reqwest::Client,
}

struct P {
    token: String,
}

async fn start() -> Server {
    let mut doc = config();
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    doc["server"]
        .as_table_mut()
        .unwrap()
        .insert("api_url".into(), format!("http://127.0.0.1:{port}/api").into());
    let cfg: Config = toml::Value::Table(doc).try_into().unwrap();
    let st = kalamar_service::app::build_state(cfg, Arc::new(ManualClock::new(1_800_000_000))).unwrap();
    let running = kalamar_service::app::start(st, listener, std::future::pending())
        .await
        .unwrap();
    let base = running.base_url();
    kalamar_service::auth::oauth::install_crypto();
    Server {
        running,
        base,
        http: reqwest::Client::new(),
    }
}

impl Server {
    async fn call(&self, method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (u16, Value) {
        let mut rb = self.http.request(
            reqwest::Method::from_bytes(method.as_bytes()).unwrap(),
            format!("{}{path}", self.base),
        );
        if let Some(t) = token {
            rb = rb.bearer_auth(t);
        }
        if let Some(b) = body {
            rb = rb.header("content-type", "application/json").body(b.to_string());
        }
        let r = rb.send().await.unwrap();
        let status = r.status().as_u16();
        (status, r.json::<Value>().await.unwrap_or(Value::Null))
    }

    /// A player whose launcher and engine declare spectator places (as the reference launcher and engine do).
    async fn player(&self, name: &str) -> P {
        let (s, v) = self
            .call("POST", "/v1/auth/dev/login", None, Some(json!({"username": name})))
            .await;
        assert_eq!(s, 200, "{v}");
        let token = v["access_token"].as_str().unwrap().to_string();
        let game = json!({"contract": {"id": "lan-auto-match", "version": 1},
            "contracts": [{"id": "lan-auto-match", "versions": [1, 2]}],
            "modes": ["turf"], "stages": [], "can_select_stage": false,
            "engine": {"stage_list": "all", "spectators": true,
                       "spectator_seats": {"max": 2, "modes": ["turf"], "verified_modes": ["turf"], "host": true}},
            "launcher": {"spectate": [1]}});
        let (s, v) = self.call("PUT", "/v1/me/game", Some(&token), Some(game)).await;
        assert_eq!(s, 204, "{v}");
        P { token }
    }

    async fn clock(&self, secs: i64) {
        let (s, v) = self
            .call("POST", "/v1/_test/clock", None, Some(json!({"advance_s": secs})))
            .await;
        assert_eq!(s, 200, "{v}");
    }

    async fn current(&self, p: &P) -> Value {
        self.call("GET", "/v1/matches/current", Some(&p.token), None).await.1["match"].clone()
    }

    async fn post(&self, p: &P, path: &str, body: Value) -> (u16, Value) {
        self.call("POST", path, Some(&p.token), Some(body)).await
    }

    /// Two players found in `queue` (and `watcher` in the stands), accepted: the match id.
    async fn ready(&self, queue: &str, a: &P, b: &P, watcher: Option<&P>) -> String {
        for p in [a, b] {
            let (s, v) = self.post(p, &format!("/v1/queues/{queue}/tickets"), json!({})).await;
            assert_eq!(s, 201, "{v}");
        }
        self.clock(0).await;
        let m = self.current(a).await;
        assert_eq!(m["state"], "pending_accept", "{m}");
        let mid = m["id"].as_str().unwrap().to_string();
        if let Some(w) = watcher {
            let (s, v) = self.post(w, &format!("/v1/matches/{mid}/spectate"), json!({})).await;
            assert_eq!(s, 200, "{v}");
        }
        for p in [a, b] {
            let (s, v) = self.post(p, &format!("/v1/matches/{mid}/accept"), json!({})).await;
            assert_eq!(s, 200, "{v}");
        }
        mid
    }

    /// The launcher started the game and mirrored its statuses up to `in-match`.
    async fn in_match(&self, p: &P, mid: &str) {
        let (s, v) = self.post(p, &format!("/v1/matches/{mid}/launch"), json!({})).await;
        assert_eq!(s, 200, "{v}");
        for st in ["booting", "lobby", "in-room", "loading-stage", "in-match"] {
            let (s, v) = self
                .post(
                    p,
                    &format!("/v1/matches/{mid}/status"),
                    json!({"version": 1, "state": st}),
                )
                .await;
            assert_eq!(s, 204, "{v}");
        }
    }

    /// The error report a launcher uploads when its game wrote no result (contract 1: `version` 1).
    async fn error(&self, p: &P, mid: &str, code: &str) -> Value {
        let m = self
            .call("GET", &format!("/v1/matches/{mid}"), Some(&p.token), None)
            .await
            .1;
        let body = json!({"version": 1, "match_id": m["game"]["match_id"], "error": code, "reason": "test"});
        let (s, v) = self.post(p, &format!("/v1/matches/{mid}/result"), body).await;
        assert_eq!(s, 200, "{v}");
        v
    }

    /// The launcher statuses up to `last` (a player whose game left while the stage loaded stops at `loading-stage`).
    async fn statuses(&self, p: &P, mid: &str, last: &str) {
        let (s, v) = self.post(p, &format!("/v1/matches/{mid}/launch"), json!({})).await;
        assert_eq!(s, 200, "{v}");
        for st in ["booting", "lobby", "in-room", "loading-stage", "in-match"] {
            let (s, v) = self
                .post(
                    p,
                    &format!("/v1/matches/{mid}/status"),
                    json!({"version": 1, "state": st}),
                )
                .await;
            assert_eq!(s, 204, "{v}");
            if st == last {
                break;
            }
        }
    }

    /// 1.1.11: the report a launcher uploads for its player's departure, with the game's `in_play` when it has one.
    async fn left(&self, p: &P, mid: &str, in_play: Option<bool>) -> Value {
        let m = self.match_of(p, mid).await;
        let mut body = json!({"version": 2, "match_id": m["game"]["match_id"], "error": "left-by-player",
                              "reason": "the player left the match from the game's menu"});
        if let Some(b) = in_play {
            body["in_play"] = json!(b);
        }
        let (s, v) = self.post(p, &format!("/v1/matches/{mid}/result"), body).await;
        assert_eq!(s, 200, "{v}");
        v
    }

    async fn uid(&self, p: &P) -> Value {
        self.call("GET", "/v1/me", Some(&p.token), None).await.1["id"].clone()
    }

    async fn history(&self, p: &P) -> Value {
        let uid = self.uid(p).await;
        let uid = uid.as_str().unwrap();
        self.call("GET", &format!("/v1/users/{uid}/matches"), Some(&p.token), None)
            .await
            .1
    }

    async fn search(&self, p: &P, queue: &str) -> (u16, Option<String>) {
        let (s, v) = self.post(p, &format!("/v1/queues/{queue}/tickets"), json!({})).await;
        if s == 201 {
            let (d, _) = self.call("DELETE", "/v1/tickets/current", Some(&p.token), None).await;
            assert_eq!(d, 204);
        }
        (s, v["error"]["code"].as_str().map(str::to_string))
    }

    async fn match_of(&self, p: &P, mid: &str) -> Value {
        self.call("GET", &format!("/v1/matches/{mid}"), Some(&p.token), None)
            .await
            .1
    }
}

/// One player's game crashed or its window was closed; its launcher reported
/// `launcher-crashed` within 3 s, the other's `connection-lost` 45 s later: void at once, both free, no offense.
#[tokio::test]
async fn a_crash_in_a_duel_is_void_at_the_second_report_and_nobody_is_penalized() {
    let s = start().await;
    let (ana, bo) = (s.player("ana").await, s.player("bo").await);
    let mid = s.ready("duel", &ana, &bo, None).await;
    s.in_match(&ana, &mid).await;
    s.in_match(&bo, &mid).await;
    s.error(&bo, &mid, "launcher-crashed").await;
    let r = s.error(&ana, &mid, "connection-lost").await;
    let o = &r["match"]["outcome"];
    assert_eq!(
        (
            r["match"]["state"].as_str(),
            o["decision"].as_str(),
            o["rated"].as_bool()
        ),
        (Some("void"), Some("aborted"), Some(false)),
        "{o}"
    );
    assert_eq!(s.search(&ana, "duel").await.0, 201);
    assert_eq!(s.search(&bo, "duel").await.0, 201, "a crash is no offence");
    s.running.stop();
}

/// 1.1.10. The other player's computer went down (its game and its launcher): its launcher
/// never reports, the survivor's reports `connection-lost` 45 s later. Before 1.1.10 the survivor stayed held
/// (`409 in_match`) until the result deadline, 40 minutes; now it is free at its report. The match is decided at the
/// deadline as before (void: no record), and the silent player is held until then.
#[tokio::test]
async fn a_survivor_is_free_at_its_report_when_the_other_launcher_vanished() {
    let s = start().await;
    let (_, meta) = s.call("GET", "/v1/meta", None, None).await;
    assert_eq!(meta["matches"]["free_after_report"], true, "1.1.10 is declared");
    let (ana, bo) = (s.player("ana").await, s.player("bo").await);
    let mid = s.ready("duel", &ana, &bo, None).await;
    s.in_match(&ana, &mid).await;
    s.in_match(&bo, &mid).await;
    let r = s.error(&ana, &mid, "connection-lost").await;
    assert_eq!(r["match"]["state"], "running", "the other player may still report");
    assert_eq!(r["match"]["you"]["free"], true, "{}", r["match"]["you"]);
    assert!(s.current(&ana).await.is_null(), "the survivor is in no match any more");
    assert_eq!(
        s.search(&ana, "duel").await,
        (201, None),
        "the survivor can search again at once"
    );
    assert_eq!(s.current(&bo).await["id"], mid.as_str());
    assert_eq!(
        s.search(&bo, "duel").await,
        (409, Some("in_match".into())),
        "the silent player is held"
    );
    // The silent player's view does not say free; the survivor's still does.
    assert!(s.current(&bo).await["you"].get("free").is_none());
    assert_eq!(s.match_of(&ana, &mid).await["you"]["free"], true);
    // The decision comes at the result deadline, as before.
    s.clock(2399).await;
    assert_eq!(s.match_of(&ana, &mid).await["state"], "running");
    s.clock(2).await;
    let m = s.match_of(&ana, &mid).await;
    assert_eq!(
        (m["state"].as_str(), m["outcome"]["decision"].as_str()),
        (Some("void"), Some("aborted")),
        "{}",
        m["outcome"]
    );
    assert!(m["you"].get("free").is_none(), "an ended match frees nobody");
    assert_eq!(s.search(&bo, "duel").await.0, 201);
    s.running.stop();
}

/// 1.1.10: a player who left from the game's menu (`left-by-player`) in a queue match stays held until the decision,
/// which gives the offense `left` (the game's menu writes that error; the survivor's game `connection-lost`).
#[tokio::test]
async fn a_player_who_left_from_the_menu_stays_held_until_the_decision_and_gets_the_offence() {
    let s = start().await;
    let (ana, bo) = (s.player("ana").await, s.player("bo").await);
    let mid = s.ready("duel", &ana, &bo, None).await;
    s.in_match(&ana, &mid).await;
    s.in_match(&bo, &mid).await;
    let r = s.error(&bo, &mid, "left-by-player").await;
    assert!(r["match"]["you"].get("free").is_none());
    assert_eq!(s.search(&bo, "duel").await, (409, Some("in_match".into())));
    let r = s.error(&ana, &mid, "connection-lost").await;
    assert_eq!(r["match"]["outcome"]["decision"], "aborted");
    assert_eq!(s.search(&ana, "duel").await.0, 201);
    assert_eq!(
        s.search(&bo, "duel").await,
        (403, Some("queue_cooldown".into())),
        "leaving a queue match is an offence"
    );
    s.running.stop();
}

/// What each `witness` value makes of a 1v1 that one player left, a spectator watching: no player's report is a
/// record (the games stop when the session is lost), so the match is void whatever the spectator reports, its record
/// included: a witness only chooses between players' records (spectators.md 6.3), it never makes one count alone.
#[tokio::test]
async fn a_spectator_cannot_save_a_duel_that_a_player_left() {
    let s = start().await;
    for (k, queue) in ["duel", "duel-tiebreak", "duel-required"].iter().enumerate() {
        let ana = s.player(&format!("ana{k}")).await;
        let bo = s.player(&format!("bo{k}")).await;
        let cy = s.player(&format!("cy{k}")).await;
        let mid = s.ready(queue, &ana, &bo, Some(&cy)).await;
        for p in [&cy, &ana, &bo] {
            s.in_match(p, &mid).await;
        }
        // The spectator's game recorded a winner (as if its game had gone on): a witness report of version 2.
        let view = s.match_of(&cy, &mid).await;
        let players: Vec<Value> = view["participants"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| {
                json!({"name": p["member_name"], "self": false, "team": p["team"], "paint": 100, "kills": 0,
                            "assists": 0, "deaths": 0, "specials": 0, "disconnected": false, "weapon": 40})
            })
            .collect();
        let witness = json!({"version": 2, "match_id": view["game"]["match_id"], "role": view["you"]["role"],
            "round": 1, "outcome": "spectated", "seat": "spectator", "rule": 0, "mode": "turf", "rule_name": "R0",
            "stage_id": 1, "stage_scene": "st-a", "end_utc": 1_800_000_300i64, "battle_frames": 10800,
            "duration_s": 180.0, "win_team": 0, "winner": "alpha", "deemed_result": -1,
            "teams": [{"team": "alpha", "paint_points": 100, "turf_permille": 40},
                      {"team": "bravo", "paint_points": 50, "turf_permille": 20}],
            "players": players, "t0_unix": 1_800_000_000.0, "timeline_s": []});
        let (st, v) = s.post(&cy, &format!("/v1/matches/{mid}/result"), witness).await;
        assert_eq!(st, 200, "{v}");
        s.error(&bo, &mid, "launcher-crashed").await;
        let r = s.error(&ana, &mid, "connection-lost").await;
        let o = &r["match"]["outcome"];
        assert_eq!(
            (r["match"]["state"].as_str(), o["decision"].as_str()),
            (Some("void"), Some("aborted")),
            "{queue}: {o}"
        );
        assert_eq!(o["spectators"][0]["verdict"], "reported", "{queue}: {o}");
    }
    s.running.stop();
}

/// 1.1.11, the usual shape (`leaving.md`): the leaver's launcher reports `left-by-player` with the game's
/// `in_play: true` at once, the other game `connection-lost` later. Under `leave = "forfeit"` the other player wins
/// by forfeit, rated; the leaver gets the offense `left` as well; the other may play again at once.
#[tokio::test]
async fn a_player_who_leaves_in_play_loses_by_forfeit() {
    let s = start().await;
    let (_, meta) = s.call("GET", "/v1/meta", None, None).await;
    let policy = |id: &str| {
        meta["result_policies"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(policy("ranked")["leave"], "forfeit");
    assert_eq!(
        policy("standard")["leave"],
        "void",
        "the default keeps the behavior of 1.1.10"
    );
    let (ana, bo) = (s.player("ana").await, s.player("bo").await);
    let mid = s.ready("duel-ranked", &ana, &bo, None).await;
    s.in_match(&ana, &mid).await;
    s.in_match(&bo, &mid).await;
    let r = s.left(&bo, &mid, Some(true)).await;
    assert_eq!(
        r["match"]["state"], "running",
        "the other player's report is waited for"
    );
    assert_eq!(
        s.search(&bo, "duel").await,
        (409, Some("in_match".into())),
        "the leaver is held"
    );
    let r = s.error(&ana, &mid, "connection-lost").await;
    let m = &r["match"];
    let o = &m["outcome"];
    assert_eq!(
        (
            m["state"].as_str(),
            o["decision"].as_str(),
            o["counted"].as_bool(),
            o["rated"].as_bool()
        ),
        (Some("completed"), Some("forfeit"), Some(true), Some(true)),
        "{o}"
    );
    let ana_team = m["you"]["team"].as_str().unwrap();
    assert_eq!(o["winner"], ana_team, "the one who stayed wins: {o}");
    assert_eq!(o["forfeit"]["reason"], "left");
    assert_ne!(o["forfeit"]["team"], ana_team);
    let bo_id = s.uid(&bo).await;
    assert_eq!(o["forfeit"]["players"], json!([{"user_id": bo_id}]));
    assert!(o["record"].is_null());
    assert!(o["summary"]["en"].as_str().unwrap().contains("left the match"), "{o}");
    assert!(o["summary"]["fr"].as_str().unwrap().contains("forfait"), "{o}");
    // Ratings: a win and a loss like any result.
    let changes = o["rating_changes"].as_array().unwrap();
    assert_eq!(changes.len(), 2, "{o}");
    let delta = |uid: &Value| {
        changes.iter().find(|c| &c["user_id"] == uid).unwrap()["delta"]
            .as_f64()
            .unwrap()
    };
    assert!(delta(&bo_id) < 0.0 && delta(&m["you"]["user_id"]) > 0.0, "{o}");
    // The history says why, on both sides.
    let (ha, hb) = (s.history(&ana).await, s.history(&bo).await);
    assert_eq!(
        (
            ha["items"][0]["result"].as_str(),
            ha["items"][0]["forfeit"]["reason"].as_str()
        ),
        (Some("win"), Some("left")),
        "{ha}"
    );
    assert_eq!(
        (
            hb["items"][0]["result"].as_str(),
            hb["items"][0]["forfeit"]["reason"].as_str()
        ),
        (Some("loss"), Some("left")),
        "{hb}"
    );
    // The leaver also has the offense; the other is free.
    assert_eq!(s.search(&ana, "duel").await.0, 201);
    assert_eq!(s.search(&bo, "duel").await, (403, Some("queue_cooldown".into())));
    s.running.stop();
}

/// 1.1.11: what stays void under `leave = "forfeit"`: a crash, a departure that its launcher could not prove in play
/// (a launcher that writes no `in_play`), a departure while the stage loaded (the offense only), a departure after the other
/// player's crash, and both players leaving.
#[tokio::test]
async fn crashes_and_unproven_departures_stay_void_under_the_forfeit_policy() {
    let s = start().await;
    let void = |r: &Value| {
        (
            r["match"]["state"].as_str().map(str::to_string),
            r["match"]["outcome"]["decision"].as_str().map(str::to_string),
        )
    };
    let aborted = (Some("void".to_string()), Some("aborted".to_string()));
    // A crash (the game killed: `launcher-crashed`), then the other game's `connection-lost`.
    let (a, b) = (s.player("a1").await, s.player("b1").await);
    let mid = s.ready("duel-ranked", &a, &b, None).await;
    s.in_match(&a, &mid).await;
    s.in_match(&b, &mid).await;
    s.error(&b, &mid, "launcher-crashed").await;
    assert_eq!(void(&s.error(&a, &mid, "connection-lost").await), aborted, "a crash");
    assert_eq!(s.search(&b, "duel").await.0, 201, "a crash is no offence");
    // A departure without `in_play` (a launcher of 1.1.10): void and the offense, as before.
    let (a, b) = (s.player("a2").await, s.player("b2").await);
    let mid = s.ready("duel-ranked", &a, &b, None).await;
    s.in_match(&a, &mid).await;
    s.in_match(&b, &mid).await;
    s.left(&b, &mid, None).await;
    assert_eq!(void(&s.error(&a, &mid, "connection-lost").await), aborted, "no in_play");
    assert_eq!(s.search(&b, "duel").await, (403, Some("queue_cooldown".into())));
    // A departure while the stage loaded (the game says `in_play: false`): void and the offense.
    let (a, b) = (s.player("a3").await, s.player("b3").await);
    let mid = s.ready("duel-ranked", &a, &b, None).await;
    s.statuses(&a, &mid, "loading-stage").await;
    s.statuses(&b, &mid, "loading-stage").await;
    s.left(&b, &mid, Some(false)).await;
    assert_eq!(
        void(&s.error(&a, &mid, "match-aborted").await),
        aborted,
        "left while loading"
    );
    assert_eq!(s.search(&b, "duel").await, (403, Some("queue_cooldown".into())));
    // Even a report that claims `in_play` is no forfeit when its player never reached the match.
    let (a, b) = (s.player("a4").await, s.player("b4").await);
    let mid = s.ready("duel-ranked", &a, &b, None).await;
    s.statuses(&a, &mid, "loading-stage").await;
    s.statuses(&b, &mid, "loading-stage").await;
    s.left(&b, &mid, Some(true)).await;
    assert_eq!(
        void(&s.error(&a, &mid, "match-aborted").await),
        aborted,
        "not in the match"
    );
    // The other player crashed first: the match was lost before the departure.
    let (a, b) = (s.player("a5").await, s.player("b5").await);
    let mid = s.ready("duel-ranked", &a, &b, None).await;
    s.in_match(&a, &mid).await;
    s.in_match(&b, &mid).await;
    s.error(&a, &mid, "launcher-crashed").await;
    s.clock(4).await;
    assert_eq!(
        void(&s.left(&b, &mid, Some(true)).await),
        aborted,
        "after the other's crash"
    );
    // Both players left: void, both get the offense.
    let (a, b) = (s.player("a6").await, s.player("b6").await);
    let mid = s.ready("duel-ranked", &a, &b, None).await;
    s.in_match(&a, &mid).await;
    s.in_match(&b, &mid).await;
    s.left(&a, &mid, Some(true)).await;
    assert_eq!(void(&s.left(&b, &mid, Some(true)).await), aborted, "both left");
    for p in [&a, &b] {
        assert_eq!(s.search(p, "duel").await, (403, Some("queue_cooldown".into())));
    }
    s.running.stop();
}

/// 1.1.11: the other player of a 1v1 never reports (its launcher went away with its game): the forfeit is decided
/// `grace_s` (120 s) after the departure, not at the result deadline; the leaver is held until then.
#[tokio::test]
async fn a_forfeit_without_the_other_report_comes_after_the_grace_period() {
    let s = start().await;
    let (ana, bo) = (s.player("ana").await, s.player("bo").await);
    let mid = s.ready("duel-ranked", &ana, &bo, None).await;
    s.in_match(&ana, &mid).await;
    s.in_match(&bo, &mid).await;
    s.left(&bo, &mid, Some(true)).await;
    s.clock(119).await;
    assert_eq!(s.match_of(&ana, &mid).await["state"], "running");
    s.clock(1).await;
    let m = s.match_of(&ana, &mid).await;
    assert_eq!(
        (
            m["state"].as_str(),
            m["outcome"]["decision"].as_str(),
            m["outcome"]["winner"].clone()
        ),
        (Some("completed"), Some("forfeit"), m["you"]["team"].clone()),
        "{}",
        m["outcome"]
    );
    assert_eq!(s.search(&ana, "duel").await.0, 201);
    // Under a policy without `leave`, the same match waits for its result deadline as before.
    let (a, b) = (s.player("a7").await, s.player("b7").await);
    let mid = s.ready("duel", &a, &b, None).await;
    s.in_match(&a, &mid).await;
    s.in_match(&b, &mid).await;
    s.left(&b, &mid, Some(true)).await;
    s.clock(600).await;
    assert_eq!(s.match_of(&a, &mid).await["state"], "running");
    s.running.stop();
}

/// 1.1.11: an unrated match: the forfeit is shown (a winner, decision `forfeit`), no rating changes.
#[tokio::test]
async fn an_unrated_forfeit_changes_no_rating() {
    let s = start().await;
    let (ana, bo) = (s.player("ana").await, s.player("bo").await);
    let mid = s.ready("duel-casual", &ana, &bo, None).await;
    s.in_match(&ana, &mid).await;
    s.in_match(&bo, &mid).await;
    s.left(&ana, &mid, Some(true)).await;
    let r = s.error(&bo, &mid, "connection-lost").await;
    let o = &r["match"]["outcome"];
    assert_eq!(
        (o["decision"].as_str(), o["counted"].as_bool(), o["rated"].as_bool()),
        (Some("forfeit"), Some(true), Some(false)),
        "{o}"
    );
    assert_eq!(o["winner"], r["match"]["you"]["team"]);
    assert_eq!(o["rating_changes"], json!([]));
    s.running.stop();
}

/// 1.1.11: a spectator who leaves never makes anyone lose; the players' departure rule ignores the stands.
#[tokio::test]
async fn a_spectator_who_leaves_is_no_forfeit() {
    let s = start().await;
    let (ana, bo, cy) = (s.player("ana").await, s.player("bo").await, s.player("cy").await);
    let mid = s.ready("duel-ranked", &ana, &bo, Some(&cy)).await;
    for p in [&cy, &ana, &bo] {
        s.in_match(p, &mid).await;
    }
    s.left(&cy, &mid, Some(true)).await;
    s.error(&ana, &mid, "connection-lost").await;
    let r = s.error(&bo, &mid, "connection-lost").await;
    assert_eq!(
        r["match"]["outcome"]["decision"], "aborted",
        "{}",
        r["match"]["outcome"]
    );
    s.running.stop();
}
