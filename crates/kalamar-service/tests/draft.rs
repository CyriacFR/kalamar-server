//! The stage draft (protocol 1.1.6, `draft.md`) through the HTTP API on 127.0.0.1 with a manual clock: a queue match
//! drafted turn by turn (only the right player, only in its turn, idempotent repeats, the result in the rules and in
//! a webhook), turns that run out, a launcher without the capability and a draft that requires it, votes kept inside
//! the team, a restart during the draft, a player who leaves, and a series where the loser picks.

use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::http::HeaderMap;
use axum::routing::post;
use kalamar_service::clock::ManualClock;
use kalamar_service::config::Config;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const STAGES: [&str; 5] = ["st-a", "st-b", "st-c", "st-d", "st-e"];

fn config(extra: &str) -> toml::Table {
    let hash: String = Sha256::digest(b"kop1.draft-referee")
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!(
        r#"
[server]
name = "Drafts"
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
[penalties]
counted = ["decline", "accept_timeout", "no_show", "left", "draft_timeout"]
window_s = 86400
cooldowns_s = [60]
[[ladders]]
id = "duel"
name = {{ en = "Duel" }}
[[drafts]]
id = "veto"
label = {{ en = "One ban each, then a random stage" }}
turns = ["ban:first:1", "ban:second:1", "random"]
[[drafts]]
id = "vote"
label = {{ en = "Team vote" }}
decider = "vote"
turns = ["ban:alpha:1", "ban:bravo:1", "pick:alpha:1"]
[[drafts]]
id = "strict"
label = {{ en = "Draft required" }}
required = true
turns = ["ban:first:1", "random"]
[[drafts]]
id = "loser-picks"
label = {{ en = "The loser picks" }}
first_game_turns = ["ban:first:1", "ban:second:1", "random"]
turns = ["ban:winner:1", "pick:loser:1"]
[[queues]]
id = "duel"
name = {{ en = "Duel" }}
teams = [1, 1]
pool = [{{ mode = "turf" }}]
result_policy = "standard"
draft = "veto"
[[queues]]
id = "strict"
name = {{ en = "Strict" }}
teams = [1, 1]
pool = [{{ mode = "turf" }}]
result_policy = "standard"
draft = "strict"
[rooms]
modes = ["turf"]
teams_choices = [[1, 1], [2, 2]]
default_teams = [1, 1]
drafts = ["veto", "vote"]
[[activities]]
id = "bo3"
kind = "duel_series"
title = {{ en = "Best of 3" }}
best_of = 3
mode = "turf"
result_policy = "standard"
draft = "loser-picks"
[[operator_tokens]]
name = "referee"
sha256 = "{hash}"
scopes = ["read", "results"]
{extra}
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
    Server {
        running,
        base,
        http: {
            kalamar_service::auth::oauth::install_crypto();
            reqwest::Client::new()
        },
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

    /// A player whose launcher declares the stages and, with `drafts`, the draft screen.
    async fn player(&self, name: &str, drafts: bool) -> String {
        let (s, v) = self
            .call("POST", "/v1/auth/dev/login", None, Some(json!({"username": name})))
            .await;
        assert_eq!(s, 200, "{v}");
        let token = v["access_token"].as_str().unwrap().to_string();
        let mut game = json!({"contract": {"id": "lan-auto-match", "version": 1}, "contracts": [{"id": "lan-auto-match", "versions": [1, 2]}], "stages": STAGES,
                              "can_select_stage": true, "engine": {"stage_list": "all"}});
        if drafts {
            game["launcher"] = json!({"draft": [1]});
        }
        let (s, v) = self.call("PUT", "/v1/me/game", Some(&token), Some(game)).await;
        assert_eq!(s, 204, "{v}");
        token
    }

    async fn clock(&self, secs: i64) {
        let (s, v) = self
            .call("POST", "/v1/_test/clock", None, Some(json!({"advance_s": secs})))
            .await;
        assert_eq!(s, 200, "{v}");
    }

    async fn current(&self, token: &str) -> Value {
        self.call("GET", "/v1/matches/current", Some(token), None).await.1["match"].clone()
    }

    async fn draft(&self, token: &str, mid: &str, turn: u64, ids: &[&str]) -> (u16, Value) {
        self.call(
            "POST",
            &format!("/v1/matches/{mid}/draft"),
            Some(token),
            Some(json!({"turn": turn, "choices": ids})),
        )
        .await
    }

    /// Two players queue in `queue`, the matchmaker pairs them, both accept: the match id.
    async fn queue_match(&self, queue: &str, a: &str, b: &str) -> String {
        for t in [a, b] {
            let (s, v) = self
                .call("POST", &format!("/v1/queues/{queue}/tickets"), Some(t), Some(json!({})))
                .await;
            assert_eq!(s, 201, "{v}");
        }
        self.clock(0).await;
        let m = self.current(a).await;
        assert_eq!(m["state"], "pending_accept", "{m}");
        assert_eq!(
            m["draft"]["state"], "waiting",
            "the draft is planned and shown before the answers"
        );
        let mid = m["id"].as_str().unwrap().to_string();
        for t in [a, b] {
            let (s, v) = self
                .call("POST", &format!("/v1/matches/{mid}/accept"), Some(t), None)
                .await;
            assert_eq!(s, 200, "{v}");
        }
        mid
    }
}

/// The token of the player of `tokens` whose team acts on the current turn, and the other one.
fn actor_of(m_a: &Value, a: &str, b: &str) -> (String, String) {
    if m_a["draft"]["you"]["can_act"] == true {
        (a.to_string(), b.to_string())
    } else {
        (b.to_string(), a.to_string())
    }
}

fn option<'a>(m: &'a Value, id: &str) -> &'a Value {
    m["draft"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .find(|o| o["id"] == id)
        .unwrap()
}

#[tokio::test]
async fn a_queue_match_is_drafted_turn_by_turn_then_ready_with_its_stage() {
    // A webhook receiver on 127.0.0.1 for `match.drafted`.
    let got: Arc<Mutex<Vec<String>>> = Arc::default();
    let g2 = got.clone();
    let hook = axum::Router::new().route(
        "/hook",
        post(move |_h: HeaderMap, body: String| {
            let g = g2.clone();
            async move {
                g.lock().unwrap().push(body);
                "ok"
            }
        }),
    );
    let hl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hport = hl.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(hl, hook).await.unwrap() });
    let s = start(config(&format!(
        "[[webhooks]]\nurl = \"http://127.0.0.1:{hport}/hook\"\nsecret = \"s\"\nevents = [\"match.drafted\"]\n"
    )))
    .await;
    let (_, meta) = s.call("GET", "/v1/meta", None, None).await;
    assert!(meta["features"].as_array().unwrap().contains(&json!("draft")));
    let a = s.player("ana", true).await;
    let b = s.player("bo", true).await;
    let (_, q) = s.call("GET", "/v1/queues/duel", Some(&a), None).await;
    assert_eq!(q["draft"]["id"], "veto");
    assert_eq!(q["draft"]["turn_s"], 20);
    let mid = s.queue_match("duel", &a, &b).await;
    let m = s.current(&a).await;
    assert_eq!(m["state"], "drafting");
    assert!(m["game"].is_null(), "no game block before the draft is over");
    let d = &m["draft"];
    assert_eq!(d["options"].as_array().unwrap().len(), 5);
    assert_eq!(d["turns"].as_array().unwrap().len(), 3);
    assert_eq!(d["current"], 0);
    assert_eq!(d["turns"][0]["state"], "current");
    assert_eq!(
        d["turns"][0]["deadline"],
        json!(kalamar_service::util::rfc3339(1_800_000_020))
    );
    let (actor, other) = actor_of(&m, &a, &b);
    let first_team = d["turns"][0]["teams"][0].clone();
    // Only the player whose turn it is, on that turn, with available options.
    assert_eq!(s.draft(&other, &mid, 0, &["st-a"]).await.0, 403);
    let refused = s.draft(&other, &mid, 0, &["st-a"]).await.1;
    assert_eq!(refused["error"]["code"], "not_your_turn");
    // The player's text in both languages, the French one in proper UTF-8 (an e-acute, not its Latin-1 mojibake).
    assert_eq!(refused["error"]["text"]["en"], "It is the other team's turn.");
    let fr = refused["error"]["text"]["fr"].as_str().unwrap();
    assert!(fr.contains("\u{e9}quipe") && !fr.contains('\u{c3}'), "{fr}");
    assert_eq!(
        s.draft(&actor, &mid, 1, &["st-a"]).await.1["error"]["code"],
        "draft_turn"
    );
    assert_eq!(s.draft(&actor, &mid, 0, &["nope"]).await.0, 422);
    assert_eq!(s.draft(&actor, &mid, 0, &["st-a", "st-b"]).await.0, 422);
    let (st, after) = s.draft(&actor, &mid, 0, &["st-a"]).await;
    assert_eq!(st, 200, "{after}");
    assert_eq!(after["draft"]["current"], 1);
    assert_eq!(option(&after, "st-a")["state"], "banned");
    assert_eq!(option(&after, "st-a")["by"], first_team);
    // The same action again is a success; another one on a closed turn is refused.
    assert_eq!(s.draft(&actor, &mid, 0, &["st-a"]).await.0, 200);
    assert_eq!(
        s.draft(&actor, &mid, 0, &["st-c"]).await.1["error"]["code"],
        "draft_turn"
    );
    assert_eq!(s.draft(&other, &mid, 1, &["st-a"]).await.0, 422, "already banned");
    let (st, done) = s.draft(&other, &mid, 1, &["st-b"]).await;
    assert_eq!(st, 200, "{done}");
    assert_eq!(done["state"], "ready");
    let stage = done["draft"]["result"]["stage"].as_str().unwrap().to_string();
    assert!(["st-c", "st-d", "st-e"].contains(&stage.as_str()));
    assert_eq!(done["rules"]["stage"], json!(stage));
    assert!(
        done["game"]["rules"]
            .as_str()
            .unwrap()
            .ends_with(&format!(",stage={stage}")),
        "{}",
        done["game"]["rules"]
    );
    assert_eq!(done["draft"]["turns"][2]["choices"][0]["by"], "server");
    assert!(done["deadlines"]["launch"].is_string());
    // Kept with the match after it is over, and sent to the operator's webhook.
    let (_, again) = s.call("GET", &format!("/v1/matches/{mid}"), Some(&a), None).await;
    assert_eq!(again["draft"]["state"], "done");
    for _ in 0..50 {
        if !got.lock().unwrap().is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    let hooks = got.lock().unwrap().clone();
    assert_eq!(hooks.len(), 1, "one match.drafted");
    let h: Value = serde_json::from_str(&hooks[0]).unwrap();
    assert_eq!(h["type"], "match.drafted");
    assert_eq!(h["data"]["draft"]["result"]["stage"], json!(stage));
    assert!(h["data"]["draft"].get("you").is_none(), "the public view");
    s.running.stop();
}

#[tokio::test]
async fn turns_that_run_out_are_played_by_the_server_and_counted_when_the_operator_wants() {
    let s = start(config("")).await;
    let a = s.player("ana", true).await;
    let b = s.player("bo", true).await;
    let mid = s.queue_match("duel", &a, &b).await;
    s.clock(19).await;
    assert_eq!(
        s.current(&a).await["draft"]["current"],
        0,
        "19 s of 20: still the first turn"
    );
    s.clock(2).await;
    let m = s.current(&a).await;
    assert_eq!(m["draft"]["turns"][0]["choices"][0]["by"], "timeout");
    assert_eq!(m["draft"]["current"], 1);
    s.clock(21).await;
    let m = s.current(&a).await;
    assert_eq!(m["state"], "ready", "the draft never waits forever");
    assert!(m["rules"]["stage"].is_string());
    assert_eq!(m["id"], json!(mid));
    // `draft_timeout` is counted on this server: both players let a turn run out.
    for t in [&a, &b] {
        let (_, me) = s.call("GET", "/v1/me", Some(t), None).await;
        assert_eq!(me["restrictions"][0]["kind"], "queue_cooldown", "{me}");
        let (_, export) = s.call("GET", "/v1/me/export", Some(t), None).await;
        assert_eq!(export["offences"][0]["kind"], "draft_timeout");
    }
    s.running.stop();
}

#[tokio::test]
async fn a_launcher_without_the_draft_is_played_for_unless_the_draft_is_required() {
    let s = start(config("")).await;
    let a = s.player("ana", true).await;
    let b = s.player("bo", false).await;
    let mid = s.queue_match("duel", &a, &b).await;
    let m = s.current(&a).await;
    assert_eq!(m["state"], "drafting");
    let bteam = if m["draft"]["you"]["team"] == "alpha" {
        "bravo"
    } else {
        "alpha"
    };
    assert_eq!(m["draft"]["teams"][bteam]["auto"], true);
    // Bo's turn (first or second) is played at once: Ana acts once and the match is ready.
    let turn = m["draft"]["current"].as_u64().unwrap();
    assert_eq!(m["draft"]["you"]["can_act"], true);
    let banned: Vec<&str> = m["draft"]["options"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|o| o["state"] == "banned")
        .map(|o| o["id"].as_str().unwrap())
        .collect();
    let free = STAGES.iter().find(|x| !banned.contains(x)).unwrap();
    let (st, done) = s.draft(&a, &mid, turn, &[free]).await;
    assert_eq!(st, 200, "{done}");
    assert_eq!(done["state"], "ready");
    let bturn = done["draft"]["turns"]
        .as_array()
        .unwrap()
        .iter()
        .find(|t| t["teams"][0] == bteam)
        .unwrap()
        .clone();
    assert_eq!(bturn["choices"][0]["by"], "server");
    // The operator may require the capability: a player without it cannot enter that queue, and is told why.
    let c = s.player("cy", false).await;
    let (_, q) = s.call("GET", "/v1/queues/strict", Some(&c), None).await;
    assert_eq!(q["eligible"], false);
    let (st, e) = s
        .call("POST", "/v1/queues/strict/tickets", Some(&c), Some(json!({})))
        .await;
    assert_eq!((st, e["error"]["code"].as_str()), (403, Some("not_eligible")));
    assert_eq!(e["error"]["details"]["requirements"][0]["id"], "draft");
    let (_, q) = s.call("GET", "/v1/queues/strict", Some(&a), None).await;
    assert_eq!(q["requirements"].as_array().unwrap().last().unwrap()["met"], true);
    s.running.stop();
}

#[tokio::test]
async fn votes_stay_in_the_team_and_ties_go_to_the_captain() {
    let s = start(config("")).await;
    let p: Vec<String> = futures_seq(&s, &["p1", "p2", "p3", "p4"]).await;
    let (st, room) = s
        .call(
            "POST",
            "/v1/rooms",
            Some(&p[0]),
            Some(json!({"options": {"teams": [2, 2], "draft": "vote"}})),
        )
        .await;
    assert_eq!(st, 201, "{room}");
    assert_eq!(room["options"]["draft"], "vote");
    let (rid, code) = (room["id"].as_str().unwrap(), room["code"].as_str().unwrap());
    for t in &p[1..] {
        assert_eq!(
            s.call("POST", "/v1/rooms/join", Some(t), Some(json!({"code": code})))
                .await
                .0,
            200
        );
        s.call(
            "POST",
            &format!("/v1/rooms/{rid}/ready"),
            Some(t),
            Some(json!({"ready": true})),
        )
        .await;
    }
    // Joining fills the team with fewer players: p1 and p3 are alpha, p2 and p4 bravo.
    let (st, m) = s
        .call("POST", &format!("/v1/rooms/{rid}/start"), Some(&p[0]), None)
        .await;
    assert_eq!(st, 201, "{m}");
    assert_eq!(m["state"], "drafting");
    let mid = m["id"].as_str().unwrap().to_string();
    assert_eq!(m["draft"]["decider"], "vote");
    assert_eq!(m["draft"]["you"]["role"], "voter");
    let (st, v) = s.draft(&p[0], &mid, 0, &["st-a"]).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["draft"]["current"], 0, "one vote of two: not settled");
    assert_eq!(v["draft"]["teams"]["alpha"]["votes"][0]["ids"], json!(["st-a"]));
    // The other team sees no vote.
    let (_, theirs) = s.call("GET", &format!("/v1/matches/{mid}"), Some(&p[1]), None).await;
    assert!(
        theirs["draft"]["teams"]["alpha"].get("votes").is_none(),
        "{}",
        theirs["draft"]["teams"]
    );
    assert_eq!(theirs["draft"]["turns"][0]["acted"], json!([]));
    assert_eq!(s.draft(&p[1], &mid, 0, &["st-a"]).await.0, 403);
    // 1 - 1: the captain's vote (p1) decides.
    let (_, v) = s.draft(&p[2], &mid, 0, &["st-b"]).await;
    assert_eq!(
        v["draft"]["turns"][0]["choices"][0],
        json!({"team": "alpha", "ids": ["st-a"], "by": "vote"})
    );
    assert_eq!(v["draft"]["current"], 1);
    s.draft(&p[1], &mid, 1, &["st-c"]).await;
    let (_, v) = s.draft(&p[3], &mid, 1, &["st-c"]).await;
    assert_eq!(option(&v, "st-c")["state"], "banned");
    s.draft(&p[0], &mid, 2, &["st-d"]).await;
    let (_, v) = s.draft(&p[2], &mid, 2, &["st-d"]).await;
    assert_eq!(v["state"], "ready");
    assert_eq!(v["rules"]["stage"], "st-d");
    assert_eq!(option(&v, "st-d")["state"], "picked");
    s.running.stop();
}

async fn futures_seq(s: &Server, names: &[&str]) -> Vec<String> {
    let mut v = Vec::new();
    for n in names {
        v.push(s.player(n, true).await);
    }
    v
}

#[tokio::test]
async fn a_draft_survives_a_restart_and_a_reconnection() {
    let dir = std::env::temp_dir().join(format!("kalamar-draft-restart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut doc = config("");
    {
        let server = doc["server"].as_table_mut().unwrap();
        server.insert(
            "database".into(),
            dir.join("db.sqlite3").to_string_lossy().into_owned().into(),
        );
        server.insert(
            "secret_file".into(),
            dir.join("secret.key").to_string_lossy().into_owned().into(),
        );
    }
    let s = start(doc.clone()).await;
    let a = s.player("ana", true).await;
    let b = s.player("bo", true).await;
    let (_, room) = s
        .call(
            "POST",
            "/v1/rooms",
            Some(&a),
            Some(json!({"options": {"draft": "veto"}})),
        )
        .await;
    let (rid, code) = (
        room["id"].as_str().unwrap().to_string(),
        room["code"].as_str().unwrap().to_string(),
    );
    s.call("POST", "/v1/rooms/join", Some(&b), Some(json!({"code": code})))
        .await;
    s.call(
        "POST",
        &format!("/v1/rooms/{rid}/ready"),
        Some(&b),
        Some(json!({"ready": true})),
    )
    .await;
    let (_, m) = s.call("POST", &format!("/v1/rooms/{rid}/start"), Some(&a), None).await;
    let mid = m["id"].as_str().unwrap().to_string();
    let (actor, other) = actor_of(&m, &a, &b);
    assert_eq!(s.draft(&actor, &mid, 0, &["st-e"]).await.0, 200);
    s.running.stop();
    drop(s);
    let s2 = start(doc).await;
    // The relaunched launcher reads its match back: the draft where it was, its own part included.
    let (_, state) = s2.call("GET", "/v1/state", Some(&other), None).await;
    let m = &state["match"];
    assert_eq!(m["state"], "drafting");
    assert_eq!(m["draft"]["current"], 1);
    assert_eq!(m["draft"]["you"]["can_act"], true);
    assert_eq!(option(m, "st-e")["state"], "banned");
    let (st, done) = s2.draft(&other, &mid, 1, &["st-d"]).await;
    assert_eq!(st, 200, "{done}");
    assert_eq!(done["state"], "ready");
    s2.running.stop();
    drop(s2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_player_who_leaves_the_draft_cancels_it_and_the_other_searches_again() {
    let s = start(config("")).await;
    let a = s.player("ana", true).await;
    let b = s.player("bo", true).await;
    let mid = s.queue_match("duel", &a, &b).await;
    let (st, m) = s
        .call("POST", &format!("/v1/matches/{mid}/decline"), Some(&b), None)
        .await;
    assert_eq!(st, 200, "{m}");
    assert_eq!(
        (m["state"].as_str(), m["cancel_reason"].as_str()),
        (Some("cancelled"), Some("left"))
    );
    assert_eq!(m["draft"]["state"], "cancelled");
    assert!(s.current(&a).await.is_null());
    let (_, t) = s.call("GET", "/v1/tickets/current", Some(&a), None).await;
    assert_eq!(t["ticket"]["state"], "searching", "requeued with its priority: {t}");
    let (_, t) = s.call("GET", "/v1/tickets/current", Some(&b), None).await;
    assert!(t["ticket"].is_null());
    let (_, me) = s.call("GET", "/v1/me", Some(&b), None).await;
    assert_eq!(
        me["restrictions"][0]["kind"], "queue_cooldown",
        "the leaver's offence: {me}"
    );
    s.running.stop();
}

#[tokio::test]
async fn in_a_series_the_loser_picks_and_played_stages_are_out() {
    let s = start(config("")).await;
    let a = s.player("ana", true).await;
    let b = s.player("bo", true).await;
    for t in [&a, &b] {
        let (st, v) = s
            .call("POST", "/v1/activities/bo3/actions/find", Some(t), Some(json!({})))
            .await;
        assert_eq!(st, 200, "{v}");
    }
    // Game 1: one ban each, then a random stage.
    let m = s.current(&a).await;
    assert_eq!(
        (m["state"].as_str(), m["series"]["index"].as_u64()),
        (Some("drafting"), Some(1))
    );
    let mid = m["id"].as_str().unwrap().to_string();
    let (actor, other) = actor_of(&m, &a, &b);
    s.draft(&actor, &mid, 0, &["st-a"]).await;
    let (_, g1) = s.draft(&other, &mid, 1, &["st-b"]).await;
    assert_eq!(g1["state"], "ready");
    let stage1 = g1["rules"]["stage"].as_str().unwrap().to_string();
    let a_team = s.current(&a).await["you"]["team"].as_str().unwrap().to_string();
    // A referee decides: Ana's team wins game 1.
    let (st, v) = s
        .call(
            "POST",
            &format!("/v1/operator/matches/{mid}/decision"),
            Some("kop1.draft-referee"),
            Some(json!({"counted": true, "winner": a_team, "rated": false})),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    // Game 2: the winner bans one, the loser picks; the stage of game 1 is out.
    let m = s.current(&b).await;
    assert_eq!(
        (m["state"].as_str(), m["series"]["index"].as_u64()),
        (Some("drafting"), Some(2)),
        "{m}"
    );
    let mid2 = m["id"].as_str().unwrap().to_string();
    let d = &m["draft"];
    assert_eq!(d["turns"][0]["action"], "ban");
    assert_eq!(d["turns"][0]["teams"], json!([a_team]));
    assert_eq!(d["turns"][1]["action"], "pick");
    assert_ne!(d["turns"][1]["teams"], json!([a_team]));
    assert_eq!(
        (
            option(&m, &stage1)["state"].as_str(),
            option(&m, &stage1)["reason"].as_str()
        ),
        (Some("unavailable"), Some("played"))
    );
    let free: Vec<&str> = STAGES.iter().copied().filter(|x| *x != stage1).collect();
    assert_eq!(s.draft(&a, &mid2, 0, &[free[0]]).await.0, 200);
    let (st, g2) = s.draft(&b, &mid2, 1, &[free[1]]).await;
    assert_eq!(st, 200, "{g2}");
    assert_eq!(g2["rules"]["stage"], json!(free[1]), "the loser's pick");
    let sid = g2["series"]["id"].as_str().unwrap();
    let (_, series) = s.call("GET", &format!("/v1/series/{sid}"), Some(&a), None).await;
    assert_eq!(series["draft"]["played"], json!([stage1]));
    s.running.stop();
}
