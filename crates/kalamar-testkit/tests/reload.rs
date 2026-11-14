//! Configuration reloads and several relays against the reference service in-process: a reload keeps sessions and
//! streams, announces `meta.changed`, cancels searches of queues that are gone, keeps what needs a restart, and
//! refuses an invalid file whole; a match's relay follows the players' measured latency and avoids a relay that
//! stopped answering.

use std::sync::Arc;
use std::time::Duration;

use kalamar_service::clock::ManualClock;
use kalamar_service::config::Config;
use kalamar_testkit::client::{Api, Events, WsAuth};
use serde_json::json;

const CONFIG: &str = r#"
[server]
name = "Before"
api_url = "http://127.0.0.1/api"
production = false
database = ":memory:"
privacy_url = "https://example.invalid/privacy"
operator = { name = "T", contact = "mailto:t@example.invalid" }
[features]
test_clock = true
[auth.dev]
enabled = true
[[relays]]
host = "127.0.0.1"
id = "eu"
region = "eu-west"
[rate_limits]
per_ip_rps = 10000.0
per_ip_burst = 10000.0
per_user_rps = 10000.0
per_user_burst = 10000.0
auth_per_ip_per_min = 100000.0
accounts_per_ip_per_day = 0.0
room_starts_per_hour = 0.0
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

/// `meta.revision` at start with the manual clock at 1 800 000 000 s: the clock in milliseconds.
const START_REVISION: u64 = 1_800_000_000_000;

fn config(text: &str, port: u16) -> Config {
    let mut doc: toml::Table = text.parse().expect("toml");
    doc["server"]
        .as_table_mut()
        .expect("server")
        .insert("api_url".into(), format!("http://127.0.0.1:{port}/api").into());
    toml::Value::Table(doc).try_into().expect("configuration")
}

async fn serve(text: &str) -> (kalamar_service::app::Running, Api, u16) {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let st = kalamar_service::app::build_state(config(text, port), Arc::new(ManualClock::new(1_800_000_000)))
        .expect("state");
    let running = kalamar_service::app::start(st, listener, std::future::pending())
        .await
        .expect("start");
    let api = Api::new(&running.base_url());
    (running, api, port)
}

async fn login(api: &Api, name: &str) -> Api {
    let r = api.post("/v1/auth/dev/login", json!({"username": name})).await;
    assert_eq!(r.status, 200, "{}", r.body);
    api.with_token(r.body["access_token"].as_str().expect("token"))
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_reload_changes_the_offering_and_keeps_the_players() {
    let (running, anon, port) = serve(CONFIG).await;
    let ana = login(&anon, "ana").await;
    let mut events = Events::connect(&anon.base, WsAuth::Bearer(ana.token.as_deref().expect("token")), None)
        .await
        .expect("events");
    let meta = anon.get("/v1/meta").await.body;
    // The revision at start: the clock in milliseconds (it keeps growing from one start to the next).
    assert_eq!(
        (meta["name"].as_str(), meta["revision"].as_u64()),
        (Some("Before"), Some(START_REVISION))
    );
    assert_eq!(
        meta["relays"],
        json!([{"id": "eu", "host": "127.0.0.1", "port": 47313, "region": "eu-west"}])
    );
    let r = ana.post_empty("/v1/queues/duel/tickets").await;
    assert!(r.ok(), "{}", r.body);

    // The new file: another name, the queue replaced, a second relay, and two things only a restart can change.
    let new = CONFIG
        .replace("name = \"Before\"", "name = \"After\"\nbind = \"127.0.0.1:9\"")
        .replace(
            "id = \"duel\"\nname = { en = \"Duel\" }\nteams",
            "id = \"solo\"\nname = { en = \"Solo\" }\nteams",
        )
        .replace(
            "[rate_limits]",
            "[[relays]]\nhost = \"127.0.0.2\"\nid = \"us\"\n[rate_limits]",
        )
        .replace(
            "[auth.dev]\nenabled = true",
            "[auth.dev]\nenabled = true\n[auth.guest]\nenabled = true",
        );
    let report = kalamar_service::reload::apply(&running.state, config(&new, port)).expect("reload");
    assert_eq!(report.revision, START_REVISION + 1);
    assert_eq!(report.cancelled_tickets, 1);
    assert_eq!(
        report.restart_needed,
        vec!["server.bind".to_string(), "auth".to_string()]
    );

    // The launcher is told, the search is over, the session still works.
    let ev = events
        .wait("meta.changed", |_| true, Duration::from_secs(5))
        .await
        .expect("meta.changed");
    assert_eq!(ev["data"]["revision"], START_REVISION + 1);
    let ev = events
        .wait(
            "queue.ticket",
            |e| e["data"]["ticket"].is_null(),
            Duration::from_secs(5),
        )
        .await;
    // (The ticket event precedes meta.changed; `wait` keeps earlier events pending.)
    assert!(ev.is_none_or(|e| e["data"]["reason"] == "queue_closed"));
    assert_eq!(ana.get("/v1/tickets/current").await.body["ticket"], json!(null));
    assert_eq!(ana.get("/v1/me").await.status, 200);

    let meta = anon.get("/v1/meta").await.body;
    assert_eq!(
        (meta["name"].as_str(), meta["revision"].as_u64()),
        (Some("After"), Some(START_REVISION + 1))
    );
    assert_eq!(meta["relays"].as_array().map(Vec::len), Some(2));
    // The guest login waits for a restart: it is neither announced nor usable.
    let providers: Vec<&str> = meta["auth"]["providers"]
        .as_array()
        .expect("providers")
        .iter()
        .filter_map(|p| p["id"].as_str())
        .collect();
    assert_eq!(providers, vec!["dev"]);
    let queues = ana.get("/v1/queues").await.body;
    let ids: Vec<&str> = queues["items"]
        .as_array()
        .expect("queues")
        .iter()
        .filter_map(|q| q["id"].as_str())
        .collect();
    assert_eq!(ids, vec!["solo"]);
    assert_eq!(ana.post_empty("/v1/queues/duel/tickets").await.status, 404);
    assert!(ana.post_empty("/v1/queues/solo/tickets").await.ok());

    // An invalid file changes nothing at all.
    let bad = new
        .replace("name = \"After\"", "name = \"Broken\"")
        .replace("teams = [1, 1]", "teams = [9, 9]");
    let err = kalamar_service::reload::apply(&running.state, config(&bad, port)).expect_err("refused");
    assert!(err.contains("queues.solo.teams"), "{err}");
    let meta = anon.get("/v1/meta").await.body;
    assert_eq!(
        (meta["name"].as_str(), meta["revision"].as_u64()),
        (Some("After"), Some(START_REVISION + 1))
    );
    assert!(ana.get("/v1/tickets/current").await.body["ticket"].is_object());
    events.close().await;
    running.stop();
}

async fn id_of(p: &Api) -> String {
    p.get("/v1/me").await.body["id"].as_str().expect("id").to_string()
}

async fn declare(p: &Api, rtt: serde_json::Value) {
    let r = p.put("/v1/me/game", json!({"launcher": {"relay_rtt_ms": rtt}})).await;
    assert_eq!(r.status, 204, "{}", r.body);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_match_gets_the_relay_closest_to_its_slowest_player() {
    // The background check is off: this test records the relays' answers itself.
    let two = CONFIG
        .replace(
            "[rate_limits]",
            "[[relays]]\nhost = \"127.0.0.2\"\nid = \"us\"\n[rate_limits]",
        )
        .replace("production = false", "production = false\nrelay_probe_s = 0");
    let (running, anon, _) = serve(&two).await;
    let mut ids = Vec::new();
    for (name, eu, us) in [("ana", json!(20), json!(140)), ("bo", json!(180), json!(60))] {
        let p = login(&anon, name).await;
        ids.push(id_of(&p).await);
        declare(&p, json!({"eu": eu, "us": us})).await;
    }
    let pick = |users: &[String]| {
        kalamar_service::relays::pick(&running.state.lock(), users)
            .expect("a relay")
            .key()
    };
    // ana alone is closest to eu; with bo, eu's slowest player is at 180 ms and us's at 140 ms.
    assert_eq!(pick(&ids[..1]), "eu");
    assert_eq!(pick(&ids), "us");
    // A player who could not reach a relay keeps the match off it.
    let cy = login(&anon, "cy").await;
    let cy_id = id_of(&cy).await;
    declare(&cy, json!({"eu": 90, "us": null})).await;
    assert_eq!(pick(&[ids[1].clone(), cy_id]), "eu");
    // Nobody measured: the configuration's order.
    let dee = login(&anon, "dee").await;
    let dee_id = id_of(&dee).await;
    assert_eq!(pick(std::slice::from_ref(&dee_id)), "eu");

    // One participant cannot steer the choice alone: a relay left out of a declaration is unknown, not unreachable
    // (such a partial declaration says nothing), 0 ms counts as 1 ms, and a value above the 2 s a launcher waits
    // counts as 2 s.
    let eve = login(&anon, "eve").await;
    let eve_id = id_of(&eve).await;
    declare(&eve, json!({"eu": 0})).await;
    assert_eq!(pick(&[ids[1].clone(), eve_id.clone()]), "us", "bo alone decides");
    declare(&eve, json!({"eu": 0, "us": 999_999})).await;
    let caps = kalamar_service::api::account::game_caps(&running.state.lock(), &eve_id);
    assert_eq!(
        (caps.relay_rtt["eu"], caps.relay_rtt["us"]),
        (Some(1), Some(kalamar_service::relays::RTT_MAX_MS))
    );
    // A launcher that reached no relay at all says nothing either.
    let fay = login(&anon, "fay").await;
    let fay_id = id_of(&fay).await;
    declare(&fay, json!({"eu": null, "us": null})).await;
    assert_eq!(pick(&[ids[1].clone(), fay_id]), "us");

    // A relay that failed three checks in a row gets no new match while the other answers.
    {
        let mut core = running.state.lock();
        assert_eq!(
            kalamar_service::relays::record(&mut core, "127.0.0.2:47313", false),
            None
        );
        assert_eq!(
            kalamar_service::relays::record(&mut core, "127.0.0.2:47313", false),
            None
        );
        assert_eq!(
            kalamar_service::relays::record(&mut core, "127.0.0.2:47313", false),
            Some(false)
        );
    }
    assert_eq!(pick(&ids), "eu");
    // Every relay down: any of them rather than none.
    for _ in 0..3 {
        kalamar_service::relays::record(&mut running.state.lock(), "127.0.0.1:47313", false);
    }
    assert_eq!(pick(&ids), "us");
    assert_eq!(
        kalamar_service::relays::record(&mut running.state.lock(), "127.0.0.2:47313", true),
        Some(true)
    );
    running.stop();
}

#[tokio::test]
async fn the_reachability_check_talks_to_a_real_relay() {
    // The relay's own code on a loopback port, and a port where nothing answers.
    let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    let port = sock.local_addr().expect("addr").port();
    tokio::spawn(kalamar_relay::server::run(
        sock,
        kalamar_relay::relay::RelayConfig::default(),
        Duration::ZERO,
        std::future::pending(),
    ));
    assert!(kalamar_service::relays::probe("127.0.0.1", port).await);
    let silent = tokio::net::UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    let silent_port = silent.local_addr().expect("addr").port();
    assert!(!kalamar_service::relays::probe("127.0.0.1", silent_port).await);
}

/// A match keeps the result policy it was created with: a reload that removes it does not make the match fall back
/// to `standard`.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_running_match_keeps_its_policy_when_a_reload_removes_it() {
    let custom = "[[result_policies]]\nid = \"custom\"\nname = { en = \"Custom\" }\nquorum = \"any\"\nsilence = \"ignore\"\ndisagreement = \"void\"\nrated_requires_both_teams = false\naccept_s = 20\nlaunch_s = 777\nresult_s = 3333\ngrace_s = 44\nreview_deadline_s = 555\n";
    let with_custom = format!("{CONFIG}{custom}").replace(
        "pool = [{ mode = \"turf\" }]\nresult_policy = \"standard\"",
        "pool = [{ mode = \"turf\" }]\nresult_policy = \"custom\"",
    );
    let (running, anon, port) = serve(&with_custom).await;
    let a = login(&anon, "ana").await;
    let b = login(&anon, "bo").await;
    for p in [&a, &b] {
        assert!(p.post_empty("/v1/queues/duel/tickets").await.ok());
    }
    let mut mid = None;
    for _ in 0..20 {
        anon.post("/v1/_test/clock", json!({"advance_s": 0})).await;
        if let Some(id) = a.get("/v1/matches/current").await.body["match"]["id"].as_str() {
            mid = Some(id.to_string());
            break;
        }
        tokio::time::sleep(Duration::from_millis(200)).await;
    }
    let mid = mid.expect("a match");
    for p in [&a, &b] {
        assert!(p.post_empty(&format!("/v1/matches/{mid}/accept")).await.ok());
    }
    // The reload removes the policy and moves the queue to `standard`.
    let report = kalamar_service::reload::apply(&running.state, config(CONFIG, port)).expect("reload");
    assert!(report.restart_needed.is_empty(), "{report:?}");
    assert!(running.state.cfg().policy("custom").is_none());
    {
        let core = running.state.lock();
        let m = &core.live.matches[&mid];
        assert_eq!(m.rules.result_policy, "custom");
        assert_eq!(
            (
                m.policy.id.as_str(),
                m.policy.policy().deadlines.result_s,
                m.policy.review_deadline_s
            ),
            ("custom", 3333, 555),
            "the definition the match started with"
        );
    }
    // A new match of the queue gets the new one.
    let view = a.get(&format!("/v1/matches/{mid}")).await.body;
    assert_eq!(view["rules"]["result_policy"], "custom", "{view}");
    running.stop();
}

/// While a change of [embedded_relay] waits for a restart, a reload keeps the running [[relays]] too: new matches
/// are never sent to a port where nothing listens yet.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn relays_wait_with_the_embedded_relay() {
    let (running, anon, port) = serve(CONFIG).await;
    let moved = CONFIG.replace(
        "[[relays]]\nhost = \"127.0.0.1\"",
        "[embedded_relay]\nenabled = true\nport = 47999\n[[relays]]\nhost = \"127.0.0.1\"\nport = 47999",
    );
    let report = kalamar_service::reload::apply(&running.state, config(&moved, port)).expect("reload");
    assert_eq!(
        report.restart_needed,
        vec![
            "embedded_relay".to_string(),
            "relays (they wait with embedded_relay)".to_string()
        ]
    );
    let meta = anon.get("/v1/meta").await.body;
    assert_eq!(meta["relays"][0]["port"], 47313, "{}", meta["relays"]);
    // Other changes of the same file apply.
    let renamed = moved.replace("name = \"Before\"", "name = \"Renamed\"");
    let report = kalamar_service::reload::apply(&running.state, config(&renamed, port)).expect("reload");
    assert_eq!(report.restart_needed.len(), 2);
    assert_eq!(anon.get("/v1/meta").await.body["name"], "Renamed");
    running.stop();
}

/// `admin reload` requests are served once: when the answer cannot be written, it is written later without
/// reloading again (and without `meta.changed` every second).
#[tokio::test]
async fn a_reload_request_is_served_once_even_when_its_answer_cannot_be_written() {
    let dir = std::env::temp_dir().join(format!("kalamar-poll-{}", std::process::id()));
    std::fs::create_dir_all(&dir).expect("dir");
    let file = dir.join("kalamar.toml");
    std::fs::write(&file, CONFIG.replace("name = \"Before\"", "name = \"From the file\"")).expect("file");
    let mut st = kalamar_service::app::build_state(config(CONFIG, 8080), Arc::new(ManualClock::new(1_800_000_000)))
        .expect("state");
    st.config_path = Some(Arc::new(file));
    let revision = || st.lock().live.meta_revision;
    let before = revision();
    {
        let core = st.lock();
        core.db
            .execute("INSERT INTO control (kind, requested_at) VALUES ('reload', 0)", [])
            .expect("request");
        core.db
            .execute_batch(
                "CREATE TEMP TRIGGER no_answer BEFORE UPDATE ON control BEGIN SELECT RAISE(ABORT, 'locked'); END;",
            )
            .expect("trigger");
    }
    kalamar_service::reload::poll(&st);
    assert_eq!(revision(), before + 1, "reloaded");
    assert_eq!(st.cfg().server.name, "From the file");
    for _ in 0..3 {
        kalamar_service::reload::poll(&st);
    }
    assert_eq!(revision(), before + 1, "never reloaded again for the same request");
    let pending = |st: &kalamar_service::state::AppState| -> i64 {
        st.lock()
            .db
            .query_row("SELECT COUNT(*) FROM control WHERE done_at IS NULL", [], |r| r.get(0))
            .expect("count")
    };
    assert_eq!(pending(&st), 1, "the answer is still waiting");
    st.lock().db.execute_batch("DROP TRIGGER no_answer;").expect("drop");
    kalamar_service::reload::poll(&st);
    assert_eq!(pending(&st), 0, "the answer is written");
    assert_eq!(revision(), before + 1);
    let (ok, detail): (i64, String) = st
        .lock()
        .db
        .query_row("SELECT ok, detail FROM control", [], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("answer");
    assert_eq!(ok, 1, "{detail}");
    let _ = std::fs::remove_dir_all(dir);
}

/// A relay secret that is missing stops the start (not a 500 on every match later).
#[test]
fn a_missing_relay_secret_stops_the_start() {
    let missing = std::env::temp_dir().join("kalamar-no-such-dir").join("relay.key");
    let text = CONFIG.replace(
        "region = \"eu-west\"",
        &format!("region = \"eu-west\"\nsecret_file = {missing:?}"),
    );
    let cfg = config(&text, 8080);
    let err = match kalamar_service::app::build_state(cfg.clone(), Arc::new(ManualClock::new(1_800_000_000))) {
        Ok(_) => panic!("started without its relay secret"),
        Err(e) => e,
    };
    assert!(
        err.contains("relays.eu: secret") && err.contains("relay.key") && err.contains("--new-room-secret"),
        "{err}"
    );
    // check-config (and serve, which calls it first) says the same.
    assert!(cfg.check_secrets().join("\n").contains("relays.eu: secret"));
}

/// A free UDP port on loopback for a relay.
fn free_udp_port() -> u16 {
    std::net::UdpSocket::bind("127.0.0.1:0")
        .expect("bind")
        .local_addr()
        .expect("addr")
        .port()
}

const EMBEDDED: &str = r#"
[server]
name = "Embedded"
api_url = "http://127.0.0.1/api"
production = false
database = ":memory:"
privacy_url = "https://example.invalid/privacy"
operator = { name = "T", contact = "mailto:t@example.invalid" }
[features]
test_clock = true
queues = false
parties = false
ratings = false
leaderboards = false
[auth.dev]
enabled = true
[rate_limits]
per_ip_rps = 10000.0
per_ip_burst = 10000.0
per_user_rps = 10000.0
per_user_burst = 10000.0
auth_per_ip_per_min = 100000.0
accounts_per_ip_per_day = 0.0
room_starts_per_hour = 0.0
room_join_per_min = 10000.0
[rooms]
modes = ["turf"]
result_policy = "casual"
"#;

/// The service with the relay of [embedded_relay] bound by its secret (created at the first start, given to the
/// only [[relays]] entry without being written twice), on a thread of its own: `(running, api, relay address)`.
async fn serve_embedded(
    dir: &std::path::Path,
) -> (kalamar_service::app::Running, Api, kalamar_service::relays::Embedded) {
    let relay_port = free_udp_port();
    let key = dir.join("data").join("relay.key");
    let text = format!(
        "{EMBEDDED}[embedded_relay]\nenabled = true\nbind = \"127.0.0.1\"\nport = {relay_port}\nsecret_file = {key:?}\nmax_rooms = 8\nmax_sessions = 32\nstats_interval_s = 0\n[[relays]]\nhost = \"127.0.0.1\"\nport = {relay_port}\n"
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
    let port = listener.local_addr().expect("addr").port();
    let st =
        kalamar_service::app::build_state(config(&text, port), Arc::new(ManualClock::starting_now())).expect("state");
    assert!(key.exists(), "the secret is created at the first start");
    let secret = st.lock().relay_keys.embedded.clone().expect("bound");
    let relay = kalamar_service::relays::start_embedded(&st.cfg(), Some(secret.clone()))
        .expect("relay")
        .expect("enabled");
    // The entry that names it holds the very same secret in memory.
    let entry = st.cfg().relays[0].clone();
    assert!(
        st.lock()
            .relay_keys
            .of(&entry)
            .expect("sealed")
            .is_same_allocation(&secret)
    );
    let running = kalamar_service::app::start(st, listener, std::future::pending())
        .await
        .expect("start");
    let api = Api::new(&running.base_url());
    (running, api, relay)
}

/// End to end: service -> relay bound by a secret. The conformance suite joins the room of a match on it
/// (`core.relay.join`), a room code the service did not seal is refused, and the relay keeps answering while the
/// service's runtime is blocked.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_embedded_relay_bound_by_its_secret_opens_the_services_rooms() {
    let dir = std::env::temp_dir().join(format!("kalamar-embedded-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (running, api, relay) = serve_embedded(&dir).await;
    let report = kalamar_testkit::conformance::run(kalamar_testkit::conformance::Options {
        api: api.base.clone(),
        discovery: None,
        areas: Some(vec!["core".into()]),
        provider: None,
        tokens: Vec::new(),
        max_wait: 5,
        verbose: false,
        reload_command: None,
    })
    .await;
    println!("{}", report.table());
    assert!(report.passed(), "{}", report.table());
    let join = report
        .results
        .iter()
        .find(|o| o.id == "core.relay.join")
        .expect("core.relay.join");
    assert_eq!(join.status, kalamar_testkit::conformance::Status::Pass);
    // A code the service did not seal: refused, reason 3.
    let addr = relay.addr;
    assert_eq!(
        kalamar_relay::client::try_join(addr, "K7Q2MXA9P84DZB3W8R6TNY5HCF", "Ana", Duration::from_secs(2))
            .await
            .expect("join"),
        kalamar_relay::client::JoinOutcome::Refused(kalamar_relay::wire::nak::ROOM)
    );
    // The service's tasks blocked (the global lock held, as a slow request would): the relay answers at once.
    let st = running.state.clone();
    let hold = std::thread::spawn(move || {
        let _g = st.lock();
        std::thread::sleep(Duration::from_millis(800));
    });
    std::thread::sleep(Duration::from_millis(100));
    let probe = std::thread::spawn(move || {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        rt.block_on(kalamar_relay::client::hello_rtt(addr, Duration::from_secs(2)))
    })
    .join()
    .expect("probe");
    hold.join().expect("hold");
    let rtt = probe.expect("the relay answered while the service was blocked");
    assert!(rtt < Duration::from_millis(200), "{rtt:?}");
    running.stop();
    let stats = relay.stop().expect("the relay's last counters");
    assert!(stats.joins >= 1 && stats.refused >= 1, "{stats:?}");
    let _ = std::fs::remove_dir_all(dir);
}

/// Stopping the embedded relay closes its members' sessions (CLOSED, reason 4), as kalamar-relay does on SIGTERM.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn stopping_the_embedded_relay_closes_its_members_sessions() {
    use kalamar_relay::wire::{self, Msg};
    let dir = std::env::temp_dir().join(format!("kalamar-embedded-stop-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let (running, api, relay) = serve_embedded(&dir).await;
    // A room match between two players: its room code is sealed by the service.
    let a = login(&api, "ana").await;
    let b = login(&api, "bo").await;
    let r = a.post("/v1/rooms", json!({"options": {"teams": [1, 1]}})).await;
    assert_eq!(r.status, 201, "{}", r.body);
    let (rid, code) = (
        r.body["id"].as_str().expect("id").to_string(),
        r.body["code"].as_str().expect("code").to_string(),
    );
    assert_eq!(b.post("/v1/rooms/join", json!({"code": code})).await.status, 200);
    b.post(&format!("/v1/rooms/{rid}/ready"), json!({"ready": true})).await;
    let m = a.post_empty(&format!("/v1/rooms/{rid}/start")).await;
    assert_eq!(m.status, 201, "{}", m.body);
    let room = m.body["game"]["relay"]["room"].as_str().expect("room").to_string();
    assert_eq!(room.len(), 32, "sealed: {room}");
    // A member joins and stays.
    let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.expect("bind");
    sock.connect(relay.addr).await.expect("connect");
    let mut buf = [0u8; 2048];
    let mut out = Vec::new();
    wire::encode_hello(&mut out, 7);
    sock.send(&out).await.expect("hello");
    let n = tokio::time::timeout(Duration::from_secs(2), sock.recv(&mut buf))
        .await
        .expect("challenge")
        .expect("recv");
    let Ok(Msg::Challenge { cookie, .. }) = wire::parse(&buf[..n]) else {
        panic!("no CHALLENGE")
    };
    out.clear();
    wire::encode_join(&mut out, 7, &cookie, 0, room.as_bytes(), b"Ana");
    sock.send(&out).await.expect("join");
    let n = tokio::time::timeout(Duration::from_secs(2), sock.recv(&mut buf))
        .await
        .expect("join answer")
        .expect("recv");
    let Ok(Msg::JoinAck(ack)) = wire::parse(&buf[..n]) else {
        panic!("not joined: {:?}", wire::parse(&buf[..n]))
    };
    running.stop();
    let stop = std::thread::spawn(move || relay.stop());
    let closed = loop {
        let n = tokio::time::timeout(Duration::from_secs(3), sock.recv(&mut buf))
            .await
            .expect("CLOSED")
            .expect("recv");
        if let Ok(Msg::Closed { auth, reason }) = wire::parse(&buf[..n]) {
            break (auth.session, reason);
        }
    };
    assert_eq!(closed, (ack.session, wire::closed::SHUTDOWN));
    assert!(stop.join().expect("stop").is_some());
    let _ = std::fs::remove_dir_all(dir);
}

/// A relay bound by a secret the service does not seal with refuses every room (JOIN_NAK 3): the conformance suite
/// says so, instead of every match failing in the game. With the right secret, the same relay opens them.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_conformance_suite_finds_a_relay_bound_by_another_secret() {
    let dir = std::env::temp_dir().join(format!("kalamar-wrong-secret-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("dir");
    let key = dir.join("relay.key");
    let secret = kalamar_relay::roomauth::create_secret_file(&key).expect("secret");
    let sock = tokio::net::UdpSocket::bind("127.0.0.1:0").await.expect("relay");
    let relay_port = sock.local_addr().expect("addr").port();
    tokio::spawn(kalamar_relay::server::run(
        sock,
        kalamar_relay::relay::RelayConfig {
            room_secret: Some(secret),
            ..Default::default()
        },
        Duration::ZERO,
        std::future::pending(),
    ));
    for (entry, ok) in [
        (
            format!("[[relays]]\nhost = \"127.0.0.1\"\nport = {relay_port}\n"),
            false,
        ),
        (
            format!("[[relays]]\nhost = \"127.0.0.1\"\nport = {relay_port}\nsecret_file = {key:?}\n"),
            true,
        ),
    ] {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.expect("bind");
        let port = listener.local_addr().expect("addr").port();
        let st = kalamar_service::app::build_state(
            config(&format!("{EMBEDDED}{entry}"), port),
            Arc::new(ManualClock::starting_now()),
        )
        .expect("state");
        let running = kalamar_service::app::start(st, listener, std::future::pending())
            .await
            .expect("start");
        let report = kalamar_testkit::conformance::run(kalamar_testkit::conformance::Options {
            api: running.base_url(),
            discovery: None,
            areas: Some(vec!["core".into()]),
            provider: None,
            tokens: Vec::new(),
            max_wait: 5,
            verbose: false,
            reload_command: None,
        })
        .await;
        let join = report
            .results
            .iter()
            .find(|o| o.id == "core.relay.join")
            .expect("core.relay.join");
        if ok {
            assert_eq!(
                join.status,
                kalamar_testkit::conformance::Status::Pass,
                "{}",
                report.table()
            );
        } else {
            match &join.status {
                kalamar_testkit::conformance::Status::Fail(why) => assert!(why.contains("JOIN_NAK 3"), "{why}"),
                other => panic!("{other:?}\n{}", report.table()),
            }
        }
        running.stop();
    }
    let _ = std::fs::remove_dir_all(dir);
}
