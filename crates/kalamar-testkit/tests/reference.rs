//! The conformance suite against the reference service, in-process on 127.0.0.1 with a manual clock, in three
//! offerings: the full reference configuration (deploy/kalamar.example.toml), a private-rooms-only server with
//! display-name accounts, and a server with only a ranked 1v1 queue under a strict policy.

use std::sync::Arc;

use kalamar_service::clock::ManualClock;
use kalamar_service::config::Config;
use kalamar_testkit::conformance::{self, Options, Status};

/// A real relay (open, as `kalamar-relay` starts without a secret) on an ephemeral loopback port, for the
/// conformance scenario that joins a match's room (`core.relay.join`).
async fn open_relay() -> u16 {
    let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.expect("relay");
    let port = sock.local_addr().expect("addr").port();
    tokio::spawn(kalamar_relay::server::run(
        sock,
        kalamar_relay::relay::RelayConfig::default(),
        std::time::Duration::ZERO,
        std::future::pending(),
    ));
    port
}

async fn serve(mut doc: toml::Table) -> (kalamar_service::app::Running, String) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let relay_port = open_relay().await;
    for r in doc
        .get_mut("relays")
        .and_then(toml::Value::as_array_mut)
        .into_iter()
        .flatten()
    {
        if r.get("host").and_then(toml::Value::as_str) == Some("127.0.0.1") {
            r.as_table_mut()
                .expect("relay")
                .insert("port".into(), i64::from(relay_port).into());
        }
    }
    let server = doc
        .entry("server")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .expect("server table");
    server.insert("api_url".into(), format!("http://127.0.0.1:{port}/api").into());
    server.insert("bind".into(), "127.0.0.1:0".into());
    server.insert("production".into(), false.into());
    server.insert("database".into(), ":memory:".into());
    server.insert("trusted_proxies".into(), toml::Value::Array(Vec::new()));
    let features = doc
        .entry("features")
        .or_insert_with(|| toml::Value::Table(toml::Table::new()))
        .as_table_mut()
        .expect("features");
    features.insert("test_clock".into(), true.into());
    let mut limits = toml::Table::new();
    for (k, v) in [
        ("per_ip_rps", 1000.0),
        ("per_ip_burst", 5000.0),
        ("per_user_rps", 500.0),
        ("per_user_burst", 2000.0),
        ("auth_per_ip_per_min", 10000.0),
        ("room_join_per_min", 1000.0),
        ("friend_requests_per_hour", 1000.0),
        ("reports_per_day", 1000.0),
        // The suite makes many accounts and starts many rooms from one address.
        ("accounts_per_ip_per_day", 0.0),
        ("room_starts_per_hour", 0.0),
    ] {
        limits.insert(k.into(), v.into());
    }
    doc.insert("rate_limits".into(), toml::Value::Table(limits));
    let cfg: Config = toml::Value::Table(doc).try_into().expect("configuration");
    let st = kalamar_service::app::build_state(cfg, Arc::new(ManualClock::starting_now())).expect("state");
    let running = kalamar_service::app::start(st, listener, std::future::pending())
        .await
        .expect("start");
    let base = running.base_url();
    (running, base)
}

async fn conform(doc: toml::Table, expect_areas: &[&str]) {
    let (running, base) = serve(doc).await;
    let report = conformance::run(Options {
        api: base,
        discovery: None,
        areas: None,
        provider: None,
        tokens: Vec::new(),
        max_wait: 5,
        verbose: false,
        reload_command: None,
    })
    .await;
    println!("{}", report.table());
    assert_eq!(report.declared, expect_areas, "declared areas");
    assert!(report.passed(), "not conformant:\n{}", report.table());
    // Every scenario of a declared area ran (none skipped for lack of time: the test clock is declared; the relay
    // answers; a reload needs a configuration file and is run against the binary).
    for o in &report.results {
        if let Status::Skip(why) = &o.status {
            assert!(
                why.contains("not declared")
                    || why.contains("no queue")
                    || why.contains("no rated")
                    || why.contains("smaller")
                    || why.contains("parties of 2")
                    || why.contains("fewer than 3")
                    || why.contains("no --reload-command"),
                "{} skipped: {why}",
                o.id
            );
        }
    }
    for id in ["core.relays", "core.relay.join"] {
        let o = report.results.iter().find(|o| o.id == id).expect(id);
        assert_eq!(o.status, Status::Pass, "{id}");
    }
    running.stop();
}

fn table(s: &str) -> toml::Table {
    s.parse::<toml::Table>().expect("toml")
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn full_reference_configuration_is_conformant() {
    let mut doc = table(include_str!("../../../deploy/kalamar.example.toml"));
    doc["auth"]["dev"]
        .as_table_mut()
        .expect("auth.dev")
        .insert("enabled".into(), true.into());
    doc["relays"][0]
        .as_table_mut()
        .expect("relay")
        .insert("host".into(), "127.0.0.1".into());
    conform(
        doc,
        &[
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
        ],
    )
    .await;
}

/// 1.1.6: a queue-only server whose 2 v 2 matches are drafted by team votes, and whose draft is required.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn voted_and_required_draft_server_is_conformant() {
    let doc = table(
        r#"
[server]
name = "Draft nights"
api_url = "http://127.0.0.1/api"
privacy_url = "https://draft.example.net/privacy"
operator = { name = "Noa", contact = "mailto:noa@example.net" }
[features]
rooms = false
queues = true
parties = false
friends = false
presence = false
ratings = false
leaderboards = false
stats = false
moderation = false
draft = true
[auth.dev]
enabled = true
[[relays]]
host = "127.0.0.1"
[[drafts]]
id = "blind"
label = { en = "Blind bans" }
summary = { en = "Both teams vote two bans at once, unseen; then each team picks in turn and the first pick is played." }
turns = ["ban:both:2", "pick:first:1", "pick:second:1"]
decider = "vote"
first = "random"
turn_s = 30
required = true
stages = ["kt-1", "kt-2", "kt-3", "kt-4", "kt-5", "kt-6", "kt-7"]
[[queues]]
id = "pairs"
name = { en = "2 v 2" }
teams = [2, 2]
selection = "game"
pool = [{ mode = "turf" }]
result_policy = "standard"
draft = "blind"
"#,
    );
    let (running, base) = serve(doc).await;
    let report = conformance::run(Options {
        api: base,
        discovery: None,
        areas: Some(vec!["core".into(), "draft".into()]),
        provider: None,
        tokens: Vec::new(),
        max_wait: 5,
        verbose: false,
        reload_command: None,
    })
    .await;
    println!("{}", report.table());
    assert!(report.passed(), "not conformant:\n{}", report.table());
    for id in [
        "draft.declaration",
        "draft.play",
        "draft.timeout",
        "draft.no-capability",
        "draft.leave",
    ] {
        let o = report.results.iter().find(|o| o.id == id).expect(id);
        assert_eq!(o.status, Status::Pass, "{id}");
    }
    running.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn rooms_only_server_is_conformant() {
    let doc = table(
        r#"
[server]
name = "Thursday rooms"
api_url = "http://127.0.0.1/api"
privacy_url = "https://rooms.example.net/privacy"
operator = { name = "Sam", contact = "mailto:sam@example.net" }
[features]
rooms = true
queues = false
parties = false
friends = false
presence = false
ratings = false
leaderboards = false
stats = false
moderation = false
[auth.guest]
enabled = true
[[relays]]
host = "127.0.0.1"
[rooms]
teams_choices = [[1, 1], [2, 2], [4, 4]]
default_teams = [4, 4]
modes = ["turf"]
invites = false
result_policy = "casual"
"#,
    );
    conform(doc, &["rooms"]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn duel_ladder_only_server_is_conformant() {
    let doc = table(
        r#"
[server]
name = "Duel ladder"
api_url = "http://127.0.0.1/api"
privacy_url = "https://duel.example.net/privacy"
operator = { name = "Alex", contact = "https://duel.example.net/contact" }
[features]
rooms = false
queues = true
parties = false
friends = false
presence = false
ratings = true
leaderboards = true
stats = false
moderation = false
[auth.dev]
enabled = true
[[relays]]
host = "127.0.0.1"
port = 47400
[[result_policies]]
id = "strict"
name = { en = "Strict" }
summary = { en = "Both results must agree; a silent player who played accepts the other's result." }
quorum = "all"
silence = "consent_after_play"
disagreement = "void"
rated_requires_both_teams = false
accept_s = 15
launch_s = 90
result_s = 1800
grace_s = 90
leave = "forfeit"
[[queues]]
id = "duel"
name = { en = "Duel" }
teams = [1, 1]
rated = true
ladder = "points"
selection = "game"
pool = [{ mode = "turf" }]
result_policy = "strict"
[penalties]
counted = ["decline", "accept_timeout"]
window_s = 3600
cooldowns_s = [120]
summary = { en = "Declining blocks the queue for 2 minutes." }
[[ladders]]
id = "points"
name = { en = "Points" }
"#,
    );
    conform(doc, &["queues", "ratings", "leaderboards"]).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn password_and_oauth_logins_are_conformant() {
    // A client secret in a file only this test reads; the provider is never contacted (the suite stops at PKCE).
    let dir = std::env::temp_dir().join(format!("kalamar-conf-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let secret = dir.join("oauth-secret");
    std::fs::write(&secret, "conformance-secret").expect("secret");
    let doc = table(&format!(
        r#"
[server]
name = "Logins"
api_url = "http://127.0.0.1/api"
privacy_url = "https://logins.example.net/privacy"
operator = {{ name = "Lou", contact = "mailto:lou@example.net" }}
[features]
rooms = true
queues = false
parties = false
friends = false
presence = false
ratings = false
leaderboards = false
stats = false
moderation = false
population = true
[auth.guest]
enabled = true
[auth.password]
enabled = true
[[auth.oauth]]
id = "sso"
preset = "github"
client_id = "conformance"
client_secret_file = '{}'
[[relays]]
host = "127.0.0.1"
[rooms]
modes = ["turf"]
result_policy = "casual"
"#,
        secret.display()
    ));
    let (running, base) = serve(doc).await;
    let report = conformance::run(Options {
        api: base,
        discovery: None,
        areas: Some(vec!["core".into(), "population".into()]),
        provider: None,
        tokens: Vec::new(),
        max_wait: 5,
        verbose: false,
        reload_command: None,
    })
    .await;
    println!("{}", report.table());
    assert!(report.passed(), "not conformant:\n{}", report.table());
    for id in [
        "core.oauth",
        "core.password",
        "core.password.lockout",
        "population.counts",
    ] {
        let o = report.results.iter().find(|o| o.id == id).expect(id);
        assert_eq!(o.status, Status::Pass, "{id}");
    }
    running.stop();
    let _ = std::fs::remove_dir_all(dir);
}

fn full_doc() -> toml::Table {
    let mut doc = table(include_str!("../../../deploy/kalamar.example.toml"));
    doc["auth"]["dev"]
        .as_table_mut()
        .expect("auth.dev")
        .insert("enabled".into(), true.into());
    doc["relays"][0]
        .as_table_mut()
        .expect("relay")
        .insert("host".into(), "127.0.0.1".into());
    doc
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn tournament_and_best_of_3_are_played_to_the_end() {
    let (running, base) = serve(full_doc()).await;
    let report = conformance::run(Options {
        api: base,
        discovery: None,
        areas: Some(vec!["activities".into(), "series".into()]),
        provider: None,
        tokens: Vec::new(),
        max_wait: 5,
        verbose: false,
        reload_command: None,
    })
    .await;
    assert!(report.passed(), "{}", report.table());
    let core = running.state.lock();
    let (n, done): (i64, i64) = core
        .db
        .query_row("SELECT COUNT(*), SUM(state = 'completed') FROM series", [], |r| {
            Ok((r.get(0)?, r.get(1)?))
        })
        .expect("series");
    assert!(
        n >= 4 && n == done,
        "{n} series, {done} completed: the cup has 3, the challenge 1"
    );
    let games: i64 = core
        .db
        .query_row(
            "SELECT COUNT(*) FROM matches WHERE origin_kind = 'activity' AND state = 'completed'",
            [],
            |r| r.get(0),
        )
        .expect("matches");
    assert!(games >= 8, "only {games} activity games were played");
    let cup = &core.live.activities["cup"];
    assert_eq!(cup.state, "finished");
    assert!(cup.champion.is_some());
    drop(core);
    running.stop();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn contract_2_rounds_continue_in_the_same_room() {
    use kalamar_testkit::client::Api;
    use kalamar_testkit::sim::{Play, Sim};
    use serde_json::json;
    let mut doc = full_doc();
    // Kept-open sessions: allowed by the operator, used when every engine says rounds are faster.
    let acts = doc["activities"].as_array_mut().expect("activities");
    let bo3 = acts.iter_mut().find(|a| a["id"].as_str() == Some("bo3")).expect("bo3");
    bo3.as_table_mut().expect("bo3").insert("keep_open".into(), true.into());
    let (running, base) = serve(doc).await;
    let anon = Api::new(&base);
    let mut players = Vec::new();
    for name in ["round1", "round2"] {
        let r = anon.post("/v1/auth/dev/login", json!({"username": name})).await;
        let api = anon.with_token(r.body["access_token"].as_str().expect("token"));
        let caps = json!({"contracts": [{"id": "lan-auto-match", "versions": [1, 2]}], "engine": {"rounds": true, "rounds_faster": true}});
        assert_eq!(api.put("/v1/me/game", caps).await.status, 204);
        players.push(api);
    }
    for p in &players {
        let r = p.post("/v1/activities/bo3/actions/find", json!({})).await;
        assert_eq!(r.status, 200, "{}", r.body);
    }
    let first = players[0].get("/v1/matches/current").await.body["match"].clone();
    let g = &first["game"];
    assert_eq!(g["contract"]["version"], 2);
    assert_eq!(g["keep_open"], true);
    assert!(g["continue_from"].is_null());
    assert_ne!(
        g["match_id"], g["relay"]["room"],
        "contract 2: the match id is not the room"
    );
    assert_eq!(first["series"]["index"], 1);
    // Play game 1 with the simulated game.
    let mid = first["id"].as_str().expect("id").to_string();
    let views: Vec<_> = futures_join(&players, &mid).await;
    let sim = Sim::from_match(&views[0]).expect("sim");
    let plays = vec![Play::Honest, Play::Honest];
    for (k, p) in players.iter().enumerate() {
        let seat = sim
            .seat_of(views[k]["you"]["member_name"].as_str().expect("name"))
            .expect("seat");
        p.post(&format!("/v1/matches/{mid}/launch"), json!({})).await;
        for st in sim.statuses(seat, &plays[seat]) {
            p.post(&format!("/v1/matches/{mid}/status"), st).await;
        }
        let mut res = sim.result_for(seat, &plays).expect("result");
        res["version"] = json!(2);
        res["outcome"] = json!("completed");
        res["round"] = json!(1);
        if seat
            == sim
                .seats
                .iter()
                .position(|s| s.role == kalamar_protocol::Role::Host)
                .expect("host")
        {
            res["settings_ok"] = json!(true);
        }
        let r = p.post(&format!("/v1/matches/{mid}/result"), res).await;
        assert_eq!(r.status, 200, "{}", r.body);
    }
    let done = players[0].get(&format!("/v1/matches/{mid}")).await.body;
    assert_eq!(done["state"], "completed", "{}", done["outcome"]);
    // The activity's view shows the running score after the first game (1.1.2).
    let view = players[0].get("/v1/activities/bo3").await.body.to_string();
    assert!(view.contains("1 - 0") || view.contains("0 - 1"), "{view}");
    // Game 2 continues the session: same relay room, continue_from game 1.
    let second = players[0].get("/v1/matches/current").await.body["match"].clone();
    assert_eq!(second["series"]["index"], 2);
    assert_eq!(second["game"]["continue_from"], json!(mid));
    assert_eq!(second["game"]["relay"]["room"], g["relay"]["room"]);
    assert_eq!(
        second["game"]["host_name"], g["host_name"],
        "a continuation keeps the host"
    );
    assert_eq!(second["game"]["continue_window_s"], 90);
    let series = players[0]
        .get(&format!(
            "/v1/series/{}",
            first["series"]["id"].as_str().expect("series")
        ))
        .await
        .body;
    assert_eq!(series["games"].as_array().map(Vec::len), Some(2));
    let wins: u64 = series["sides"]
        .as_array()
        .expect("sides")
        .iter()
        .map(|s| s["wins"].as_u64().unwrap_or(0))
        .sum();
    assert_eq!(wins, 1);
    running.stop();
}

async fn futures_join(players: &[kalamar_testkit::client::Api], mid: &str) -> Vec<serde_json::Value> {
    let mut v = Vec::new();
    for p in players {
        v.push(p.get(&format!("/v1/matches/{mid}")).await.body);
    }
    v
}
