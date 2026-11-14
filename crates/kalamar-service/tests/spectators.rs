//! Places in the stands (protocol 1.1.7, `spectators.md`) through the HTTP API on 127.0.0.1 with a manual clock: a room
//! member who moves into the stands and back, a launcher or game that does not know spectators (refused with what it
//! lacks, never seated), the match's rules and views, a witness who breaks a 1v1 disagreement, a referee required for a
//! rated result (seated by the operator API), a spectator's error that voids nothing, the places that close at the
//! launch (a late spectator refused), the draft seen read-only, a series' places, and a player who refuses to be
//! watched.

use std::sync::Arc;

use kalamar_service::clock::ManualClock;
use kalamar_service::config::Config;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const REFEREE_TOKEN: &str = "kop1.spect-referee";

fn config() -> toml::Table {
    let hash: String = Sha256::digest(REFEREE_TOKEN.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!(
        r#"
[server]
name = "Stands"
api_url = "http://127.0.0.1/api"
production = false
database = ":memory:"
privacy_url = "https://example.invalid/privacy"
operator = {{ name = "T", contact = "mailto:t@example.invalid" }}
[features]
test_clock = true
draft = true
activities = true
series = true
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
[spectators]
opt_out = true
wait_s = 45
[[result_policies]]
id = "witnessed"
name = {{ en = "A spectator breaks ties" }}
quorum = "majority"
silence = "consent_after_play"
disagreement = "void"
rated_requires_both_teams = true
accept_s = 20
launch_s = 90
result_s = 2400
grace_s = 120
witness = "tiebreak"
[[result_policies]]
id = "refereed"
name = {{ en = "A referee confirms rated results" }}
quorum = "majority"
silence = "consent_after_play"
disagreement = "majority"
rated_requires_both_teams = true
accept_s = 20
launch_s = 90
result_s = 2400
grace_s = 120
witness = "required"
witness_from = "referees"
[[ladders]]
id = "duel"
name = {{ en = "Duel" }}
[[drafts]]
id = "veto"
label = {{ en = "One ban each, then a random stage" }}
turns = ["ban:first:1", "ban:second:1", "random"]
stages = ["st-a", "st-b", "st-c", "st-d"]
[[queues]]
id = "duel"
name = {{ en = "Duel" }}
teams = [1, 1]
pool = [{{ mode = "turf" }}]
result_policy = "standard"
spectators = {{ seats = 1, who = "anyone" }}
[[queues]]
id = "ranked"
name = {{ en = "Ranked" }}
teams = [1, 1]
rated = true
ladder = "duel"
pool = [{{ mode = "turf" }}]
result_policy = "refereed"
spectators = {{ seats = 1, who = "referees" }}
[[queues]]
id = "drafted"
name = {{ en = "Drafted" }}
teams = [1, 1]
pool = [{{ mode = "turf" }}]
result_policy = "standard"
draft = "veto"
spectators = {{ seats = 2, who = "friends" }}
[rooms]
modes = ["turf", "zones"]
teams_choices = [[1, 1], [2, 2]]
default_teams = [1, 1]
result_policy = "witnessed"
[rooms.spectators]
max = 2
default = 1
who = ["anyone", "friends", "invited"]
host = true
[[activities]]
id = "bo3"
kind = "duel_series"
title = {{ en = "Best of 3" }}
best_of = 3
mode = "turf"
result_policy = "standard"
spectators = {{ seats = 1, who = "anyone" }}
[[operator_tokens]]
name = "referee"
sha256 = "{hash}"
scopes = ["read", "results"]
"#
    )
    .parse()
    .expect("toml")
}

struct Server {
    running: kalamar_service::app::Running,
    base: String,
    http: reqwest::Client,
}

async fn start(mut doc: toml::Table) -> Server {
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

/// What a player's launcher declares.
#[derive(Clone, Copy, PartialEq)]
enum Launcher {
    /// 1.1.7: an engine with spectator places and a launcher that runs the seat.
    Spectators,
    /// A 1.1.6 launcher (its engine may even know the letter S: the 1.1 boolean alone seats nobody).
    Older,
}

struct P {
    token: String,
    id: String,
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

    async fn player(&self, name: &str, l: Launcher) -> P {
        let (s, v) = self
            .call("POST", "/v1/auth/dev/login", None, Some(json!({"username": name})))
            .await;
        assert_eq!(s, 200, "{v}");
        let token = v["access_token"].as_str().unwrap().to_string();
        let (_, me) = self.call("GET", "/v1/me", Some(&token), None).await;
        let mut game = json!({"contract": {"id": "lan-auto-match", "version": 1},
            "contracts": [{"id": "lan-auto-match", "versions": [1, 2]}],
            "modes": ["turf", "zones"], "stages": ["st-a", "st-b", "st-c", "st-d"], "can_select_stage": true,
            "engine": {"stage_list": "all", "spectators": true}, "launcher": {"draft": [1]}});
        if l == Launcher::Spectators {
            game["engine"]["spectator_seats"] =
                json!({"max": 2, "modes": ["turf", "zones"], "verified_modes": ["turf", "zones"], "host": true});
            game["launcher"]["spectate"] = json!([1]);
        }
        let (s, v) = self.call("PUT", "/v1/me/game", Some(&token), Some(game)).await;
        assert_eq!(s, 204, "{v}");
        P {
            token,
            id: me["id"].as_str().unwrap().to_string(),
        }
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

    /// A room of `owner` with these options; returns (id, code).
    async fn room(&self, owner: &P, options: Value) -> (String, String) {
        let (s, r) = self.post(owner, "/v1/rooms", json!({"options": options})).await;
        assert_eq!(s, 201, "{r}");
        (r["id"].as_str().unwrap().into(), r["code"].as_str().unwrap().into())
    }

    /// Two players queue in `queue` and the matchmaker pairs them (state `pending_accept`): the match id.
    async fn found(&self, queue: &str, a: &P, b: &P) -> String {
        for p in [a, b] {
            let (s, v) = self.post(p, &format!("/v1/queues/{queue}/tickets"), json!({})).await;
            assert_eq!(s, 201, "{v}");
        }
        self.clock(0).await;
        let m = self.current(a).await;
        if m.is_null() {
            let (_, t) = self.call("GET", "/v1/tickets/current", Some(&a.token), None).await;
            let (_, q) = self
                .call("GET", &format!("/v1/queues/{queue}"), Some(&a.token), None)
                .await;
            panic!("no match: ticket {t} queue {q}");
        }
        assert_eq!(m["state"], "pending_accept", "{m}");
        m["id"].as_str().unwrap().to_string()
    }

    async fn accept(&self, mid: &str, who: &[&P]) {
        for p in who {
            let (s, v) = self.post(p, &format!("/v1/matches/{mid}/accept"), json!({})).await;
            assert_eq!(s, 200, "{v}");
        }
    }

    /// Launches, plays to `in-match` and uploads `report` for `p` (statuses included).
    async fn play(&self, p: &P, mid: &str, report: Value) -> Value {
        let (s, v) = self.post(p, &format!("/v1/matches/{mid}/launch"), json!({})).await;
        assert_eq!(s, 200, "{v}");
        for st in ["booting", "in-room", "loading-stage", "in-match", "results"] {
            let (s, v) = self
                .post(
                    p,
                    &format!("/v1/matches/{mid}/status"),
                    json!({"version": 1, "state": st}),
                )
                .await;
            assert_eq!(s, 204, "{v}");
        }
        let (s, v) = self.post(p, &format!("/v1/matches/{mid}/result"), report).await;
        assert_eq!(s, 200, "{v}");
        v
    }
}

/// A version 2 result file of a match seen by `me` (`None`: a spectator's, `spectated`), with `win_team`.
fn result_file(m: &Value, me: Option<&str>, role: &str, win_team: i64) -> Value {
    let players: Vec<Value> = m["participants"]
        .as_array()
        .unwrap()
        .iter()
        .enumerate()
        .map(|(i, p)| {
            let name = p["member_name"].as_str().unwrap();
            json!({"name": name, "self": Some(name) == me, "team": p["team"], "paint": 300 + i, "kills": 1,
                   "assists": 0, "deaths": 2, "specials": 1, "disconnected": false, "weapon": 40, "gear": [1, 2, 3]})
        })
        .collect();
    let mut r = json!({
        "version": 2, "match_id": m["game"]["match_id"], "role": role, "round": 1,
        "outcome": if me.is_some() { "completed" } else { "spectated" },
        "seat": if me.is_some() { "player" } else { "spectator" },
        "rule": 0, "mode": m["rules"]["mode"], "rule_name": "R0", "stage_id": 1,
        "stage_scene": m["rules"]["stage"].as_str().unwrap_or("st-a"),
        "end_utc": 1_800_000_300i64, "battle_frames": 10800, "duration_s": 180.0, "win_team": win_team,
        "winner": if win_team == 0 { "alpha" } else { "bravo" }, "deemed_result": -1,
        "teams": [{"team": "alpha", "paint_points": 301, "turf_permille": 400},
                  {"team": "bravo", "paint_points": 302, "turf_permille": 410},
                  {"team": "charlie", "paint_points": 0, "turf_permille": 0}],
        "players": players, "t0_unix": 1_800_000_000.0, "timeline_s": []
    });
    if role == "host" {
        r["settings_ok"] = json!(true);
        r["requested"] = json!({"mode": m["rules"]["mode"]});
    }
    r
}

/// The participant whose `you` this view is, as (member name, role).
fn me(m: &Value) -> (String, String) {
    (
        m["you"]["member_name"].as_str().unwrap().to_string(),
        m["you"]["role"].as_str().unwrap().to_string(),
    )
}

#[tokio::test]
async fn a_room_member_moves_into_the_stands_and_back_and_the_match_seats_it() {
    let s = start(config()).await;
    let (_, meta) = s.call("GET", "/v1/meta", None, None).await;
    assert!(meta["features"].as_array().unwrap().contains(&json!("spectators")));
    assert_eq!(meta["spectators"]["opt_out"], true);
    let opts = meta["rooms"]["options"].as_array().unwrap();
    let field = opts.iter().find(|f| f["key"] == "spectators").unwrap();
    assert_eq!(
        (field["type"].as_str(), field["max"].as_u64()),
        (Some("integer"), Some(2)),
        "an ordinary field"
    );
    assert_eq!(meta["rooms"]["spectators"]["host"], true);
    assert_eq!(
        meta["result_policies"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["id"] == "witnessed")
            .unwrap()["witness"],
        "tiebreak"
    );
    let ana = s.player("ana", Launcher::Spectators).await;
    let bo = s.player("bo", Launcher::Spectators).await;
    let cy = s.player("cy", Launcher::Spectators).await;
    let (rid, code) = s.room(&ana, json!({"spectators": 2})).await;
    assert_eq!(s.post(&bo, "/v1/rooms/join", json!({"code": code})).await.0, 200);
    // A full room still takes a member in its stands, but only when it asks for that seat.
    let (st, r) = s.post(&cy, "/v1/rooms/join", json!({"code": code})).await;
    assert_eq!(
        (st, r["error"]["code"].as_str()),
        (409, Some("room_full")),
        "never a spectator by surprise"
    );
    let (st, r) = s
        .post(&cy, "/v1/rooms/join", json!({"code": code, "seat": "spectator"}))
        .await;
    assert_eq!(st, 200, "{r}");
    let member = |r: &Value, id: &str| {
        r["members"]
            .as_array()
            .unwrap()
            .iter()
            .find(|m| m["user"]["id"] == id)
            .unwrap()
            .clone()
    };
    assert_eq!(member(&r, &cy.id)["team"], "spectator");
    assert_eq!(
        r["spectators"],
        json!({"seats": 2, "taken": 1, "who": "anyone", "host": true})
    );
    // Bo moves into the stands and back to play, like a team change (ready reset).
    s.post(&bo, &format!("/v1/rooms/{rid}/ready"), json!({"ready": true}))
        .await;
    let (st, r) = s
        .call(
            "PUT",
            &format!("/v1/rooms/{rid}/members/{}", bo.id),
            Some(&bo.token),
            Some(json!({"team": "spectator"})),
        )
        .await;
    assert_eq!(st, 200, "{r}");
    assert_eq!(
        (
            member(&r, &bo.id)["team"].as_str(),
            member(&r, &bo.id)["ready"].as_bool()
        ),
        (Some("spectator"), Some(false))
    );
    // Two places, both taken: the stands are full; the start needs two players.
    let (st, r) = s.post(&ana, &format!("/v1/rooms/{rid}/start"), json!({})).await;
    assert_eq!((st, r["error"]["code"].as_str()), (409, Some("not_ready")), "{r}");
    s.post(&bo, &format!("/v1/rooms/{rid}/ready"), json!({"ready": true}))
        .await;
    s.post(&cy, &format!("/v1/rooms/{rid}/ready"), json!({"ready": true}))
        .await;
    let (st, r) = s.post(&ana, &format!("/v1/rooms/{rid}/start"), json!({})).await;
    assert_eq!(
        (st, r["error"]["code"].as_str()),
        (409, Some("not_enough_players")),
        "spectators are not players: {r}"
    );
    let (st, r) = s
        .call(
            "PUT",
            &format!("/v1/rooms/{rid}/members/{}", bo.id),
            Some(&bo.token),
            Some(json!({"team": "bravo"})),
        )
        .await;
    assert_eq!((st, member(&r, &bo.id)["team"].as_str()), (200, Some("bravo")));
    s.post(&bo, &format!("/v1/rooms/{rid}/ready"), json!({"ready": true}))
        .await;
    let (st, m) = s.post(&ana, &format!("/v1/rooms/{rid}/start"), json!({})).await;
    assert_eq!(st, 201, "{m}");
    let rules = m["game"]["rules"].as_str().unwrap();
    assert!(
        rules.starts_with("players=3,teams=")
            && rules.contains(":S,mode=turf")
            && rules.ends_with(",spectators_wait=45"),
        "{rules}"
    );
    assert_eq!(m["game"]["players"], 3);
    assert_eq!(
        m["participants"].as_array().unwrap().len(),
        2,
        "the participants are the players"
    );
    assert_eq!(
        m["spectators"].as_array().unwrap().len(),
        1,
        "no spectator is hidden from the players"
    );
    assert_eq!(
        m["rules"]["spectators"]["open"], false,
        "a room's places close at the start"
    );
    let mc = s.current(&cy).await;
    assert_eq!(
        (mc["you"]["team"].as_str(), mc["you"]["seat"].as_str()),
        (Some("spectator"), Some("spectator"))
    );
    assert_eq!(
        mc["game"]["rules"], m["game"]["rules"],
        "a spectator gets the same rules"
    );
    assert!(mc["game"]["host_name"].as_str() != Some(mc["you"]["member_name"].as_str().unwrap()));
    s.running.stop();
}

#[tokio::test]
async fn an_older_launcher_is_never_seated_and_a_place_is_refused_with_what_is_missing() {
    let s = start(config()).await;
    let ana = s.player("ana", Launcher::Spectators).await;
    let old = s.player("old", Launcher::Older).await;
    let cy = s.player("cy", Launcher::Spectators).await;
    let (rid, code) = s.room(&ana, json!({"spectators": 1})).await;
    // The older launcher cannot sit in the stands itself.
    s.post(&old, "/v1/rooms/join", json!({"code": code})).await;
    let (st, r) = s
        .call(
            "PUT",
            &format!("/v1/rooms/{rid}/members/{}", old.id),
            Some(&old.token),
            Some(json!({"team": "spectator"})),
        )
        .await;
    assert_eq!(
        (st, r["error"]["code"].as_str()),
        (409, Some("incompatible_game")),
        "{r}"
    );
    let missing = &r["error"]["details"]["players"][0]["missing"];
    assert!(
        missing.as_array().unwrap().contains(&json!("launcher:spectate"))
            && missing.as_array().unwrap().contains(&json!("spectators")),
        "{missing}"
    );
    // A capable member cannot sit either while an older launcher plays: its game would fail on the rules.
    let (st, r) = s
        .post(&cy, "/v1/rooms/join", json!({"code": code, "seat": "spectator"}))
        .await;
    assert_eq!(
        (st, r["error"]["code"].as_str()),
        (409, Some("incompatible_game")),
        "{r}"
    );
    assert_eq!(
        r["error"]["details"]["players"][0]["user_id"],
        json!(old.id),
        "it names whose game lacks it"
    );
    // Without anybody in the stands the older launcher plays as before.
    s.post(&old, &format!("/v1/rooms/{rid}/ready"), json!({"ready": true}))
        .await;
    let (st, m) = s.post(&ana, &format!("/v1/rooms/{rid}/start"), json!({})).await;
    assert_eq!(st, 201, "{m}");
    assert!(!m["game"]["rules"].as_str().unwrap().contains(":S"));
    s.running.stop();
}

#[tokio::test]
async fn a_witness_breaks_a_duel_disagreement_and_is_never_counted_as_a_player() {
    let s = start(config()).await;
    let ana = s.player("ana", Launcher::Spectators).await;
    let bo = s.player("bo", Launcher::Spectators).await;
    let cy = s.player("cy", Launcher::Spectators).await;
    let (rid, code) = s.room(&ana, json!({"spectators": 1})).await;
    s.post(&bo, "/v1/rooms/join", json!({"code": code})).await;
    s.post(&cy, "/v1/rooms/join", json!({"code": code, "seat": "spectator"}))
        .await;
    for p in [&bo, &cy] {
        s.post(p, &format!("/v1/rooms/{rid}/ready"), json!({"ready": true}))
            .await;
    }
    let (_, m) = s.post(&ana, &format!("/v1/rooms/{rid}/start"), json!({})).await;
    let mid = m["id"].as_str().unwrap().to_string();
    let views = [s.current(&ana).await, s.current(&bo).await, s.current(&cy).await];
    let (an, ar) = me(&views[0]);
    let (bn, br) = me(&views[1]);
    let (_, cr) = me(&views[2]);
    // Everyone launches; each player claims the win of its own team; the spectator's game saw bravo win.
    let (_, launched) = s.post(&cy, &format!("/v1/matches/{mid}/launch"), json!({})).await;
    assert_eq!(launched["you"]["seat"], "spectator");
    let ana_team = views[0]["you"]["team"].as_str().unwrap();
    let own = |t: &str| if t == "alpha" { 0 } else { 1 };
    s.play(&ana, &mid, result_file(&views[0], Some(&an), &ar, own(ana_team)))
        .await;
    s.play(&bo, &mid, result_file(&views[1], Some(&bn), &br, 1 - own(ana_team)))
        .await;
    // The players disagree: the decision waits for the witness who launched (the policy uses it). 1.1.10: the players'
    // reports are in, so the match no longer holds them (`you.free`) while it waits.
    let (_, waiting) = s
        .call("GET", &format!("/v1/matches/{mid}"), Some(&ana.token), None)
        .await;
    assert_eq!(
        (waiting["state"].as_str(), waiting["you"]["free"].as_bool()),
        (Some("running"), Some(true))
    );
    assert!(s.current(&ana).await.is_null());
    let r = s
        .post(
            &cy,
            &format!("/v1/matches/{mid}/result"),
            result_file(&views[2], None, &cr, 1),
        )
        .await;
    assert_eq!(r.0, 200, "{}", r.1);
    let o = &r.1["match"]["outcome"];
    assert_eq!(
        (o["decision"].as_str(), o["winner"].as_str(), o["witnessed"].as_bool()),
        (Some("witnessed"), Some("bravo"), Some(true)),
        "{o}"
    );
    assert_eq!(o["reports"]["expected"], 2, "the witness is not a player");
    assert_eq!(o["spectators"][0]["verdict"], "agreed");
    assert_eq!(o["record"]["players"].as_array().unwrap().len(), 2);
    assert_eq!(r.1["report"]["verdict"], "agreed");
    // The public view of the ended match keeps who watched out.
    let (_, public) = s
        .call(
            "GET",
            &format!("/v1/matches/{mid}"),
            Some(&s.player("dee", Launcher::Older).await.token),
            None,
        )
        .await;
    assert!(
        public["outcome"].get("spectators").is_none() && public.get("spectators").is_none(),
        "{public}"
    );
    s.running.stop();
}

#[tokio::test]
async fn a_late_spectator_is_refused_and_a_spectators_error_voids_nothing() {
    let s = start(config()).await;
    let ana = s.player("ana", Launcher::Spectators).await;
    let bo = s.player("bo", Launcher::Spectators).await;
    let cy = s.player("cy", Launcher::Spectators).await;
    let dee = s.player("dee", Launcher::Spectators).await;
    let mid = s.found("duel", &ana, &bo).await;
    // Watchable before the launch: listed, one place, anyone.
    let (st, l) = s.call("GET", "/v1/spectate", Some(&cy.token), None).await;
    assert_eq!(st, 200, "{l}");
    let item = &l["items"][0];
    assert_eq!(
        (item["match_id"].as_str(), item["can_take"].as_bool()),
        (Some(mid.as_str()), Some(true)),
        "{l}"
    );
    assert_eq!(item["places"]["seats"], 1);
    let (st, m) = s.post(&cy, &format!("/v1/matches/{mid}/spectate"), json!({})).await;
    assert_eq!(st, 200, "{m}");
    assert_eq!(m["you"]["seat"], "spectator");
    let (st, r) = s.post(&dee, &format!("/v1/matches/{mid}/spectate"), json!({})).await;
    assert_eq!((st, r["error"]["code"].as_str()), (409, Some("spectators_full")));
    // The match is ready: the places close, the rules carry the spectator.
    s.accept(&mid, &[&ana, &bo]).await;
    let m = s.current(&ana).await;
    assert_eq!(m["state"], "ready");
    assert!(
        m["game"]["rules"].as_str().unwrap().contains(":S,"),
        "{}",
        m["game"]["rules"]
    );
    s.post(&cy, &format!("/v1/matches/{mid}/spectate"), json!({})).await; // idempotent
    let (st, r) = s
        .call("DELETE", &format!("/v1/matches/{mid}/spectate"), Some(&cy.token), None)
        .await;
    assert_eq!(st, 204, "{r}");
    let (st, r) = s.post(&dee, &format!("/v1/matches/{mid}/spectate"), json!({})).await;
    assert_eq!(
        (st, r["error"]["code"].as_str()),
        (409, Some("match_started")),
        "nobody joins after the launch: {r}"
    );
    let (_, l) = s.call("GET", "/v1/spectate", Some(&dee.token), None).await;
    assert!(
        l["items"].as_array().unwrap().is_empty(),
        "a launched match is not listed"
    );
    s.running.stop();

    // Another match: the spectator launches too late (a player's stage loads), then its game fails: the match counts.
    let s = start(config()).await;
    let ana = s.player("ana", Launcher::Spectators).await;
    let bo = s.player("bo", Launcher::Spectators).await;
    let cy = s.player("cy", Launcher::Spectators).await;
    let mid = s.found("duel", &ana, &bo).await;
    assert_eq!(
        s.post(&cy, &format!("/v1/matches/{mid}/spectate"), json!({})).await.0,
        200
    );
    s.accept(&mid, &[&ana, &bo]).await;
    let va = s.current(&ana).await;
    let vb = s.current(&bo).await;
    let (an, ar) = me(&va);
    let (bn, br) = me(&vb);
    s.post(&ana, &format!("/v1/matches/{mid}/launch"), json!({})).await;
    s.post(
        &ana,
        &format!("/v1/matches/{mid}/status"),
        json!({"version": 1, "state": "loading-stage"}),
    )
    .await;
    let (st, r) = s.post(&cy, &format!("/v1/matches/{mid}/launch"), json!({})).await;
    assert_eq!((st, r["error"]["code"].as_str()), (409, Some("match_started")), "{r}");
    // Its launcher reports the refusal; the players' reports decide.
    let (st, _) = s
        .post(
            &cy,
            &format!("/v1/matches/{mid}/result"),
            json!({"version": 2, "match_id": va["game"]["match_id"], "error": "host-left"}),
        )
        .await;
    assert_eq!(st, 200);
    s.play(&ana, &mid, result_file(&va, Some(&an), &ar, 0)).await;
    let r = s.play(&bo, &mid, result_file(&vb, Some(&bn), &br, 0)).await;
    let o = &r["match"]["outcome"];
    assert_eq!(
        (o["decision"].as_str(), o["counted"].as_bool()),
        (Some("agreed"), Some(true)),
        "{o}"
    );
    assert_eq!(o["spectators"][0]["verdict"], "error");
    assert_eq!(o["reports"]["errors"], 0, "a spectator's error is not a player's");
    let (_, me_c) = s.call("GET", "/v1/me", Some(&cy.token), None).await;
    assert!(
        me_c["restrictions"].as_array().unwrap().is_empty(),
        "no offence for a spectator: {me_c}"
    );
    s.running.stop();
}

#[tokio::test]
async fn a_rated_result_needs_the_referee_the_operator_seated() {
    let s = start(config()).await;
    let ana = s.player("ana", Launcher::Spectators).await;
    let bo = s.player("bo", Launcher::Spectators).await;
    let cy = s.player("cy", Launcher::Spectators).await;
    let referee = s.player("ref", Launcher::Spectators).await;
    // Without a referee: the result counts, ratings do not change.
    let mid = s.found("ranked", &ana, &bo).await;
    let (st, r) = s.post(&cy, &format!("/v1/matches/{mid}/spectate"), json!({})).await;
    assert_eq!(
        (st, r["error"]["code"].as_str()),
        (403, Some("spectators_not_allowed")),
        "{r}"
    );
    let (_, l) = s.call("GET", "/v1/spectate", Some(&cy.token), None).await;
    assert!(
        l["items"].as_array().unwrap().is_empty(),
        "places for referees are not shown to others"
    );
    s.accept(&mid, &[&ana, &bo]).await;
    let (va, vb) = (s.current(&ana).await, s.current(&bo).await);
    s.play(&ana, &mid, result_file(&va, Some(&me(&va).0), &me(&va).1, 0))
        .await;
    let r = s
        .play(&bo, &mid, result_file(&vb, Some(&me(&vb).0), &me(&vb).1, 0))
        .await;
    let o = &r["match"]["outcome"];
    assert_eq!(
        (
            o["counted"].as_bool(),
            o["rated"].as_bool(),
            o["unrated_reason"].as_str()
        ),
        (Some(true), Some(false), Some("no_witness")),
        "{o}"
    );
    // With the referee the operator seated: rated.
    let mid = s.found("ranked", &ana, &bo).await;
    let (st, v) = s
        .call(
            "POST",
            &format!("/v1/operator/matches/{mid}/spectators"),
            Some(REFEREE_TOKEN),
            Some(json!({"user_id": referee.id})),
        )
        .await;
    assert_eq!(st, 204, "{v}");
    s.accept(&mid, &[&ana, &bo]).await;
    let vr = s.current(&referee).await;
    assert_eq!(
        (vr["you"]["seat"].as_str(), vr["you"]["referee"].as_bool()),
        (Some("spectator"), Some(true))
    );
    let (va, vb) = (s.current(&ana).await, s.current(&bo).await);
    assert_eq!(va["spectators"][0]["referee"], true, "the players see the referee");
    assert_eq!(
        s.post(&referee, &format!("/v1/matches/{mid}/launch"), json!({}))
            .await
            .0,
        200
    );
    s.play(&ana, &mid, result_file(&va, Some(&me(&va).0), &me(&va).1, 1))
        .await;
    s.play(&bo, &mid, result_file(&vb, Some(&me(&vb).0), &me(&vb).1, 1))
        .await;
    // Both players agree; the decision waits for the referee who launched.
    let r = s.play(&referee, &mid, result_file(&vr, None, &me(&vr).1, 1)).await;
    let o = &r["match"]["outcome"];
    assert_eq!(
        (o["decision"].as_str(), o["rated"].as_bool(), o["witnessed"].as_bool()),
        (Some("agreed"), Some(true), Some(true)),
        "{o}"
    );
    // The referee saw something else than both players report: void (two new players: the ratings moved apart).
    let (dee, eve) = (
        s.player("dee", Launcher::Spectators).await,
        s.player("eve", Launcher::Spectators).await,
    );
    let mid = s.found("ranked", &dee, &eve).await;
    s.call(
        "POST",
        &format!("/v1/operator/matches/{mid}/spectators"),
        Some(REFEREE_TOKEN),
        Some(json!({"user_id": referee.id})),
    )
    .await;
    s.accept(&mid, &[&dee, &eve]).await;
    let (va, vb, vr) = (s.current(&dee).await, s.current(&eve).await, s.current(&referee).await);
    s.post(&referee, &format!("/v1/matches/{mid}/launch"), json!({})).await;
    s.play(&dee, &mid, result_file(&va, Some(&me(&va).0), &me(&va).1, 0))
        .await;
    s.play(&eve, &mid, result_file(&vb, Some(&me(&vb).0), &me(&vb).1, 0))
        .await;
    let r = s.play(&referee, &mid, result_file(&vr, None, &me(&vr).1, 1)).await;
    assert_eq!(r["match"]["outcome"]["decision"], "witness_disagreement");
    s.running.stop();
}

#[tokio::test]
async fn a_spectator_watches_the_draft_read_only() {
    let s = start(config()).await;
    let ana = s.player("ana", Launcher::Spectators).await;
    let bo = s.player("bo", Launcher::Spectators).await;
    let cy = s.player("cy", Launcher::Spectators).await;
    let mid = s.found("drafted", &ana, &bo).await;
    s.accept(&mid, &[&ana, &bo]).await;
    assert_eq!(s.current(&ana).await["state"], "drafting");
    // `friends`: a stranger neither sees nor takes the place; a friend of a player does.
    let (_, l) = s.call("GET", "/v1/spectate", Some(&cy.token), None).await;
    assert!(l["items"].as_array().unwrap().is_empty());
    let (_, me_a) = s.call("GET", "/v1/me", Some(&ana.token), None).await;
    let code = me_a["friend_code"].as_str().unwrap();
    let (st, req) = s.post(&cy, "/v1/friends/requests", json!({"friend_code": code})).await;
    assert_eq!(st, 201, "{req}");
    let (_, reqs) = s.call("GET", "/v1/friends/requests", Some(&ana.token), None).await;
    let rid = reqs["incoming"][0]["id"].as_str().unwrap();
    assert_eq!(
        s.post(&ana, &format!("/v1/friends/requests/{rid}/accept"), json!({}))
            .await
            .0,
        200
    );
    let (_, l) = s.call("GET", "/v1/spectate", Some(&cy.token), None).await;
    assert_eq!(l["items"][0]["draft"], true, "{l}");
    let (st, m) = s.post(&cy, &format!("/v1/matches/{mid}/spectate"), json!({})).await;
    assert_eq!(st, 200, "{m}");
    let d = &m["draft"];
    assert_eq!(
        (d["you"]["team"].as_str(), d["you"]["can_act"].as_bool()),
        (Some("spectator"), Some(false))
    );
    assert!(d["teams"]["alpha"].get("votes").is_none() && d["teams"]["bravo"].get("votes").is_none());
    let (st, r) = s
        .post(
            &cy,
            &format!("/v1/matches/{mid}/draft"),
            json!({"turn": 0, "choices": ["st-a"]}),
        )
        .await;
    assert_eq!((st, r["error"]["code"].as_str()), (403, Some("not_your_turn")));
    // The players draft; the end of the draft closes the places with the spectator in the rules.
    let actor = |m: &Value| m["draft"]["you"]["can_act"] == true;
    for turn in 0..2 {
        let va = s.current(&ana).await;
        let p = if actor(&va) { &ana } else { &bo };
        let opt = va["draft"]["options"]
            .as_array()
            .unwrap()
            .iter()
            .find(|o| o["state"] == "available")
            .unwrap()["id"]
            .clone();
        let (st, v) = s
            .post(
                p,
                &format!("/v1/matches/{mid}/draft"),
                json!({"turn": turn, "choices": [opt]}),
            )
            .await;
        assert_eq!(st, 200, "{v}");
    }
    let m = s.current(&cy).await;
    assert_eq!(m["state"], "ready");
    assert!(m["game"]["rules"].as_str().unwrap().contains(":S,"));
    assert_eq!(m["rules"]["spectators"]["open"], false);
    s.running.stop();
}

#[tokio::test]
async fn a_series_place_holds_for_the_next_games_and_a_player_may_refuse_to_be_watched() {
    let s = start(config()).await;
    let ana = s.player("ana", Launcher::Spectators).await;
    let bo = s.player("bo", Launcher::Spectators).await;
    let cy = s.player("cy", Launcher::Spectators).await;
    for p in [&ana, &bo] {
        let (st, v) = s.post(p, "/v1/activities/bo3/actions/find", json!({})).await;
        assert_eq!(st, 200, "{v}");
    }
    let m1 = s.current(&ana).await;
    let sid = m1["series"]["id"].as_str().unwrap().to_string();
    let (_, l) = s.call("GET", "/v1/spectate", Some(&cy.token), None).await;
    let item = &l["items"][0];
    assert_eq!(
        (item["kind"].as_str(), item["series"]["index"].as_u64()),
        (Some("series"), Some(2)),
        "{l}"
    );
    let (st, sv) = s.post(&cy, &format!("/v1/series/{sid}/spectate"), json!({})).await;
    assert_eq!(st, 200, "{sv}");
    assert_eq!(sv["spectators"]["users"][0]["id"], json!(cy.id));
    // Game 1 was ready already: it is played without the spectator, who watches from game 2 on.
    let mid1 = m1["id"].as_str().unwrap();
    assert!(!m1["game"]["rules"].as_str().unwrap().contains(":S"));
    let (va, vb) = (s.current(&ana).await, s.current(&bo).await);
    s.play(&ana, mid1, result_file(&va, Some(&me(&va).0), &me(&va).1, 0))
        .await;
    s.play(&bo, mid1, result_file(&vb, Some(&me(&vb).0), &me(&vb).1, 0))
        .await;
    let m2 = s.current(&cy).await;
    assert_eq!(
        (m2["series"]["index"].as_u64(), m2["you"]["seat"].as_str()),
        (Some(2), Some("spectator")),
        "{m2}"
    );
    assert!(m2["game"]["rules"].as_str().unwrap().contains(":S,"));
    assert!(
        m2["game"]["continue_from"].is_null(),
        "a new member: a new game session"
    );
    // The spectator leaves between games: game 3 is the players' alone.
    let mid2 = m2["id"].as_str().unwrap().to_string();
    let (st, _) = s
        .call("DELETE", &format!("/v1/series/{sid}/spectate"), Some(&cy.token), None)
        .await;
    assert_eq!(st, 204);
    let (va, vb) = (s.current(&ana).await, s.current(&bo).await);
    s.play(&ana, &mid2, result_file(&va, Some(&me(&va).0), &me(&va).1, 1))
        .await;
    s.play(&bo, &mid2, result_file(&vb, Some(&me(&vb).0), &me(&vb).1, 1))
        .await;
    let m3 = s.current(&ana).await;
    assert_eq!(m3["series"]["index"], 3);
    assert!(!m3["game"]["rules"].as_str().unwrap().contains(":S"));
    // A player refuses to be watched: the series is no longer listed nor open to the spectator.
    let (st, me_b) = s
        .call(
            "PATCH",
            "/v1/me",
            Some(&bo.token),
            Some(json!({"settings": {"allow_spectators": false}})),
        )
        .await;
    assert_eq!((st, me_b["settings"]["allow_spectators"].as_bool()), (200, Some(false)));
    let (_, l) = s.call("GET", "/v1/spectate", Some(&cy.token), None).await;
    assert!(l["items"].as_array().unwrap().is_empty(), "{l}");
    let (st, r) = s.post(&cy, &format!("/v1/series/{sid}/spectate"), json!({})).await;
    assert_eq!((st, r["error"]["code"].as_str()), (403, Some("spectators_refused")));
    // The export holds what the spectator watched.
    let (_, export) = s.call("GET", "/v1/me/export", Some(&cy.token), None).await;
    assert_eq!(
        export["matches_watched"][0]["match_id"],
        json!(mid2),
        "{}",
        export["matches_watched"]
    );
    s.running.stop();
}

/// 1.1.9: the relay of a match is chosen from its players' measurements only: a spectator who declares every relay
/// but one unreachable does not move the match (relay.md 4.1).
#[tokio::test]
async fn a_spectator_has_no_say_in_the_relay_of_a_match() {
    let mut doc = config();
    doc["server"]
        .as_table_mut()
        .unwrap()
        .insert("relay_probe_s".into(), 0.into());
    doc["relays"] = toml::Value::Array(vec![
        "host = \"127.0.0.1\"\nid = \"eu\""
            .parse::<toml::Table>()
            .unwrap()
            .into(),
        "host = \"127.0.0.2\"\nid = \"us\""
            .parse::<toml::Table>()
            .unwrap()
            .into(),
    ]);
    let s = start(doc).await;
    let server = &s;
    let declare = move |p: &P, eu: Value, us: Value| {
        let mut game = json!({"contract": {"id": "lan-auto-match", "version": 1},
            "contracts": [{"id": "lan-auto-match", "versions": [1, 2]}],
            "modes": ["turf", "zones"], "stages": ["st-a", "st-b", "st-c", "st-d"], "can_select_stage": true,
            "engine": {"stage_list": "all", "spectators": true,
                       "spectator_seats": {"max": 2, "modes": ["turf", "zones"], "verified_modes": ["turf", "zones"], "host": true}},
            "launcher": {"draft": [1], "spectate": [1]}});
        game["launcher"]["relay_rtt_ms"] = json!({"eu": eu, "us": us});
        let token = p.token.clone();
        async move {
            let (st, v) = server.call("PUT", "/v1/me/game", Some(&token), Some(game)).await;
            assert_eq!(st, 204, "{v}");
        }
    };
    let ana = s.player("ana", Launcher::Spectators).await;
    let bo = s.player("bo", Launcher::Spectators).await;
    let cy = s.player("cy", Launcher::Spectators).await;
    declare(&ana, json!(20), json!(30)).await;
    declare(&bo, json!(25), json!(40)).await;
    // The spectator says it cannot reach eu and is 0 ms from us.
    declare(&cy, json!(null), json!(0)).await;
    let (rid, code) = s.room(&ana, json!({"spectators": 1})).await;
    s.post(&bo, "/v1/rooms/join", json!({"code": code})).await;
    s.post(&cy, "/v1/rooms/join", json!({"code": code, "seat": "spectator"}))
        .await;
    for p in [&bo, &cy] {
        s.post(p, &format!("/v1/rooms/{rid}/ready"), json!({"ready": true}))
            .await;
    }
    let (st, m) = s.post(&ana, &format!("/v1/rooms/{rid}/start"), json!({})).await;
    assert_eq!(st, 201, "{m}");
    let view = s.current(&cy).await;
    assert_eq!(view["you"]["seat"], "spectator", "{view}");
    assert_eq!(
        (
            view["game"]["relay"]["host"].as_str(),
            view["game"]["relay"]["port"].as_u64()
        ),
        (Some("127.0.0.1"), Some(47313)),
        "the players' relay (eu), not the spectator's: {}",
        view["game"]["relay"]
    );
    s.running.stop();
}

/// 1.1.13 (relay.md 4.3): when every relay that could take a match is full, a room start is refused with
/// `503 relay_capacity` and changes nothing; once a relay has room again the same start succeeds there.
#[tokio::test]
async fn a_room_start_waits_while_every_relay_is_full() {
    let mut doc = config();
    doc["server"]
        .as_table_mut()
        .unwrap()
        .insert("relay_probe_s".into(), 0.into());
    let key = std::env::temp_dir().join(format!("kalamar-relay-load-{}.key", std::process::id()));
    std::fs::write(&key, "ab".repeat(32)).unwrap();
    doc["relays"] = toml::Value::Array(vec![
        format!("host = \"127.0.0.1\"\nid = \"eu\"\nsecret_file = {:?}", key)
            .parse::<toml::Table>()
            .unwrap()
            .into(),
    ]);
    // asked once at the start (nothing answers on that port), then not for ten minutes
    doc.insert(
        "relay_load".into(),
        "status_s = 600".parse::<toml::Table>().unwrap().into(),
    );
    let s = start(doc).await;
    let full = kalamar_relay::status::Load {
        workers: 1,
        window_s: 5,
        sessions: 1020,
        rooms: 300,
        max_sessions: 1024,
        max_rooms: 0,
        max_members: 12,
        worker_busy: vec![10],
        ..Default::default()
    };
    kalamar_service::relays::record_load(&mut s.running.state.lock(), "eu", Some(full.clone()));
    let ana = s.player("ana", Launcher::Spectators).await;
    let bo = s.player("bo", Launcher::Spectators).await;
    let (rid, code) = s.room(&ana, json!({})).await;
    s.post(&bo, "/v1/rooms/join", json!({"code": code})).await;
    s.post(&bo, &format!("/v1/rooms/{rid}/ready"), json!({"ready": true}))
        .await;
    let (st, e) = s.post(&ana, &format!("/v1/rooms/{rid}/start"), json!({})).await;
    assert_eq!(st, 503, "{e}");
    assert_eq!(e["error"]["code"], "relay_capacity", "{e}");
    assert_eq!(e["error"]["retry_after_s"], 30, "{e}");
    assert!(
        e["error"]["text"]["fr"].as_str().is_some_and(|t| t.contains("pleins")),
        "{e}"
    );
    // The relay has room again: the same start succeeds.
    let room = kalamar_relay::status::Load {
        sessions: 10,
        rooms: 5,
        ..full
    };
    kalamar_service::relays::record_load(&mut s.running.state.lock(), "eu", Some(room));
    let (st, m) = s.post(&ana, &format!("/v1/rooms/{rid}/start"), json!({})).await;
    assert_eq!(st, 201, "{m}");
    s.running.stop();
    let _ = std::fs::remove_file(key);
}
