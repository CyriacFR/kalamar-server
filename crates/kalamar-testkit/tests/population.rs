//! `GET /v1/population` (protocol 1.1.4) against the reference service in-process, with a manual clock: no player,
//! one account with one and with several event connections, activity without a connection, players searching and in
//! a match, the 10 s cache, and the area turned off.

use std::sync::Arc;
use std::time::Duration;

use kalamar_service::clock::ManualClock;
use kalamar_service::config::Config;
use kalamar_testkit::client::{Api, Events, WsAuth};
use serde_json::{Value, json};

const CONFIG: &str = r#"
[server]
name = "Population"
api_url = "http://127.0.0.1/api"
production = false
database = ":memory:"
privacy_url = "https://example.invalid/privacy"
operator = { name = "T", contact = "mailto:t@example.invalid" }
[features]
test_clock = true
population = true
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
population_per_ip_per_min = 100000.0
[[ladders]]
id = "duel"
name = { en = "Duel" }
[[queues]]
id = "duel"
name = { en = "Duel" }
teams = [1, 1]
pool = [{ mode = "turf" }]
result_policy = "standard"
"#;

async fn serve(population: bool) -> (kalamar_service::app::Running, Api) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let mut doc: toml::Table = CONFIG.parse().expect("toml");
    doc["server"]
        .as_table_mut()
        .expect("server")
        .insert("api_url".into(), format!("http://127.0.0.1:{port}/api").into());
    doc["features"]
        .as_table_mut()
        .expect("features")
        .insert("population".into(), population.into());
    let cfg: Config = toml::Value::Table(doc).try_into().expect("configuration");
    let st = kalamar_service::app::build_state(cfg, Arc::new(ManualClock::new(1_800_000_000))).expect("state");
    let running = kalamar_service::app::start(st, listener, std::future::pending())
        .await
        .expect("start");
    let api = Api::new(&running.base_url());
    (running, api)
}

async fn population(api: &Api) -> Value {
    let r = api.get("/v1/population").await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.cache_control.as_deref(), Some("max-age=10"));
    for k in ["online", "in_match", "searching"] {
        assert!(r.body[k].is_u64(), "{k}: {}", r.body);
    }
    r.body
}

/// The counts after the cache expired.
async fn fresh(api: &Api) -> Value {
    advance(api, 10).await;
    population(api).await
}

async fn advance(api: &Api, s: u64) {
    let r = api.post("/v1/_test/clock", json!({"advance_s": s})).await;
    assert!(r.ok(), "{}", r.body);
}

async fn login(api: &Api, name: &str) -> Api {
    let r = api.post("/v1/auth/dev/login", json!({"username": name})).await;
    assert_eq!(r.status, 200, "{}", r.body);
    api.with_token(r.body["access_token"].as_str().expect("token"))
}

fn counts(v: &Value) -> (u64, u64, u64) {
    let n = |k: &str| v[k].as_u64().unwrap_or(u64::MAX);
    (n("online"), n("in_match"), n("searching"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn counts_connections_activity_queues_and_matches() {
    let (running, anon) = serve(true).await;
    let meta = anon.get("/v1/meta").await.body;
    assert!(
        meta["features"]
            .as_array()
            .expect("features")
            .contains(&json!("population"))
    );
    let disc = Api::new(&format!("http://{}", running.addr))
        .get("/.well-known/kalamar-server.json")
        .await
        .body;
    assert!(
        disc["features"]
            .as_array()
            .expect("features")
            .contains(&json!("population"))
    );

    // Nobody yet.
    let first = population(&anon).await;
    assert_eq!(counts(&first), (0, 0, 0));
    assert!(
        first.as_object().expect("object").len() == 4,
        "nothing but the counts: {first}"
    );

    // An authenticated request makes an account online; the cached answer is served for 10 s.
    let ana = login(&anon, "ana").await;
    assert_eq!(ana.get("/v1/me").await.status, 200);
    let cached = population(&anon).await;
    assert_eq!(cached, first, "served from the cache");
    assert_eq!(counts(&fresh(&anon).await), (1, 0, 0));

    // Three connections of the same account count once; another account with only a connection counts too.
    let token = ana.token.clone().expect("token");
    let mut conns = Vec::new();
    for _ in 0..3 {
        conns.push(
            Events::connect(&anon.base, WsAuth::Bearer(&token), None)
                .await
                .expect("events"),
        );
    }
    let bo = login(&anon, "bo").await;
    let bo_events = Events::connect(&anon.base, WsAuth::Bearer(bo.token.as_deref().expect("token")), None)
        .await
        .expect("events");
    assert_eq!(counts(&fresh(&anon).await), (2, 0, 0));

    // Two minutes later, open connections still count, past activity alone no longer does.
    advance(&anon, 121).await;
    assert_eq!(counts(&fresh(&anon).await), (2, 0, 0));
    for c in conns {
        c.close().await;
    }
    let mut seen = (0, 0, 0);
    for _ in 0..50 {
        seen = counts(&fresh(&anon).await);
        if seen == (1, 0, 0) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert_eq!(seen, (1, 0, 0), "ana's connections closed and her activity is old");

    // Searching, then in a match once both accepted.
    for p in [&ana, &bo] {
        let r = p.post("/v1/queues/duel/tickets", json!({})).await;
        assert_eq!(r.status, 201, "{}", r.body);
    }
    let (o, m, s) = counts(&fresh(&anon).await);
    assert_eq!((o, m), (2, 0), "both queued players made requests");
    assert!(s == 2, "two searching players, got {s}");
    let mut mid = String::new();
    for _ in 0..20 {
        advance(&anon, 0).await;
        if let Some(id) = ana.get("/v1/matches/current").await.body["match"]["id"].as_str() {
            mid = id.to_string();
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(!mid.is_empty(), "no match found");
    for p in [&ana, &bo] {
        let r = p.post_empty(&format!("/v1/matches/{mid}/accept")).await;
        assert_eq!(r.status, 200, "{}", r.body);
    }
    assert_eq!(counts(&fresh(&anon).await), (2, 2, 0));
    bo_events.close().await;
    running.stop();
}

#[tokio::test]
async fn an_undeclared_population_answers_404() {
    let (running, anon) = serve(false).await;
    let meta = anon.get("/v1/meta").await.body;
    assert!(
        !meta["features"]
            .as_array()
            .expect("features")
            .contains(&json!("population"))
    );
    let r = anon.get("/v1/population").await;
    assert_eq!((r.status, r.code()), (404, Some("not_found")));
    running.stop();
}
