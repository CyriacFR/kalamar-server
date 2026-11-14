//! HTTP-level tests of the reference service on 127.0.0.1: the OAuth loopback flow against a mock provider (no real
//! provider is contacted), malformed input never answering 5xx, bans, and matches surviving a restart.

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::State;
use axum::http::HeaderMap;
use axum::routing::{get, post};
use kalamar_service::clock::ManualClock;
use kalamar_service::config::Config;
use serde_json::{Value, json};

const BASE_CONFIG: &str = r#"
[server]
name = "Test"
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
[rate_limits]
per_ip_rps = 10000.0
per_ip_burst = 10000.0
per_user_rps = 10000.0
per_user_burst = 10000.0
auth_per_ip_per_min = 100000.0
accounts_per_ip_per_day = 0.0
room_starts_per_hour = 0.0
room_join_per_min = 10000.0
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
"#;

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
    async fn call(&self, method: &str, path: &str, token: Option<&str>, body: Option<&str>) -> (u16, Value) {
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
        let v = r.json::<Value>().await.unwrap_or(Value::Null);
        (status, v)
    }
    async fn login(&self, name: &str) -> String {
        let (s, v) = self
            .call(
                "POST",
                "/v1/auth/dev/login",
                None,
                Some(&json!({"username": name}).to_string()),
            )
            .await;
        assert_eq!(s, 200, "{v}");
        v["access_token"].as_str().unwrap().to_string()
    }
}

#[derive(Clone, Default)]
struct MockSeen {
    token_requests: Arc<Mutex<Vec<(String, String)>>>,
}

async fn mock_token(State(seen): State<MockSeen>, headers: HeaderMap, body: String) -> Json<Value> {
    let auth = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or_default()
        .to_string();
    seen.token_requests.lock().unwrap().push((auth, body));
    Json(json!({"access_token": "provider-access-token", "token_type": "Bearer", "expires_in": 600}))
}

async fn mock_user(headers: HeaderMap) -> Json<Value> {
    assert_eq!(headers.get("authorization").unwrap(), "Bearer provider-access-token");
    Json(json!({"id": "80351110224678912", "username": "camille", "global_name": "Camille"}))
}

#[tokio::test]
async fn oauth_loopback_flow_against_a_mock_provider() {
    let seen = MockSeen::default();
    let mock = axum::Router::new()
        .route("/oauth2/token", post(mock_token))
        .route("/users/@me", get(mock_user))
        .with_state(seen.clone());
    let ml = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let mport = ml.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(ml, mock).await.unwrap() });

    let mut doc: toml::Table = BASE_CONFIG.parse().unwrap();
    let discord: toml::Table = format!(
        r#"enabled = true
client_id = "client-1"
client_secret = "secret-1"
redirect_uris = ["http://127.0.0.1:53682/callback"]
authorize_url = "https://discord.example/oauth2/authorize"
token_url = "http://127.0.0.1:{mport}/oauth2/token"
user_url = "http://127.0.0.1:{mport}/users/@me""#
    )
    .parse()
    .unwrap();
    doc["auth"]
        .as_table_mut()
        .unwrap()
        .insert("discord".into(), toml::Value::Table(discord));
    let s = start(doc).await;
    let (_, meta) = s.call("GET", "/v1/meta", None, None).await;
    let p = meta["auth"]["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "discord")
        .unwrap()
        .clone();
    assert_eq!(p["kind"], "oauth_loopback");
    assert_eq!(p["loopback_redirects"][0], "http://127.0.0.1:53682/callback");

    // RFC 7636 appendix B pair.
    let verifier = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
    let challenge = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
    let begin = |redirect: &str| {
        json!({"redirect_uri": redirect, "code_challenge": challenge, "code_challenge_method": "S256"}).to_string()
    };
    let (st, v) = s
        .call(
            "POST",
            "/v1/auth/discord/begin",
            None,
            Some(&begin("http://127.0.0.1:9999/evil")),
        )
        .await;
    assert_eq!(
        (st, v["error"]["code"].as_str()),
        (422, Some("validation_failed")),
        "an unregistered redirect is refused"
    );
    let (st, v) = s
        .call(
            "POST",
            "/v1/auth/discord/begin",
            None,
            Some(&begin("http://127.0.0.1:53682/callback")),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    let url = v["authorize_url"].as_str().unwrap();
    let state = v["state"].as_str().unwrap().to_string();
    assert!(
        url.starts_with("https://discord.example/oauth2/authorize?") && url.contains(challenge) && url.contains(&state)
    );

    // A wrong verifier burns the state.
    let complete = |state: &str, verifier: &str| {
        json!({"state": state, "code": "code-from-redirect", "code_verifier": verifier}).to_string()
    };
    let (st, v) = s
        .call(
            "POST",
            "/v1/auth/discord/complete",
            None,
            Some(&complete(&state, &"x".repeat(43))),
        )
        .await;
    assert_eq!((st, v["error"]["code"].as_str()), (400, Some("pkce_mismatch")));
    let (st, v) = s
        .call(
            "POST",
            "/v1/auth/discord/complete",
            None,
            Some(&complete(&state, verifier)),
        )
        .await;
    assert_eq!(
        (st, v["error"]["code"].as_str()),
        (400, Some("invalid_state")),
        "states are single use"
    );

    // The full flow.
    let (_, v) = s
        .call(
            "POST",
            "/v1/auth/discord/begin",
            None,
            Some(&begin("http://127.0.0.1:53682/callback")),
        )
        .await;
    let state = v["state"].as_str().unwrap().to_string();
    let (st, session) = s
        .call(
            "POST",
            "/v1/auth/discord/complete",
            None,
            Some(&complete(&state, verifier)),
        )
        .await;
    assert_eq!(st, 200, "{session}");
    assert_eq!(session["created"], true);
    assert_eq!(session["me"]["display_name"], "Camille");
    assert_eq!(session["me"]["logins"], json!(["discord"]));
    {
        let reqs = seen.token_requests.lock().unwrap();
        let (auth, body) = &reqs[0];
        assert!(auth.starts_with("Basic "), "client credentials by HTTP basic auth");
        assert!(body.contains("grant_type=authorization_code") && body.contains("code=code-from-redirect"));
        assert!(
            body.contains(&format!("code_verifier={verifier}")),
            "PKCE sent upstream"
        );
        assert!(body.contains("redirect_uri=http%3A%2F%2F127.0.0.1%3A53682%2Fcallback"));
    }
    // The same provider account logs in to the same Kalamar account; its id is never stored in clear.
    let (_, v) = s
        .call(
            "POST",
            "/v1/auth/discord/begin",
            None,
            Some(&begin("http://127.0.0.1:53682/callback")),
        )
        .await;
    let (_, again) = s
        .call(
            "POST",
            "/v1/auth/discord/complete",
            None,
            Some(&complete(v["state"].as_str().unwrap(), verifier)),
        )
        .await;
    assert_eq!(again["created"], false);
    assert_eq!(again["me"]["id"], session["me"]["id"]);
    let core = s.running.state.lock();
    let leaked: i64 = core
        .db
        .query_row(
            "SELECT COUNT(*) FROM identities WHERE CAST(subject_hash AS TEXT) LIKE '%80351110224678912%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(leaked, 0);
    drop(core);
    s.running.stop();
}

#[tokio::test]
async fn malformed_input_never_answers_5xx() {
    let s = start(BASE_CONFIG.parse().unwrap()).await;
    let token = s.login("fuzzer").await;
    let other = s.login("other").await;
    let (_, me) = s.call("GET", "/v1/me", Some(&other), None).await;
    let other_id = me["id"].as_str().unwrap().to_string();
    let endpoints = [
        ("POST", "/v1/auth/dev/login"),
        ("POST", "/v1/auth/guest/login"),
        ("POST", "/v1/auth/refresh"),
        ("POST", "/v1/auth/discord/begin"),
        ("POST", "/v1/auth/discord/complete"),
        ("PATCH", "/v1/me"),
        ("PUT", "/v1/me/game"),
        ("POST", "/v1/rooms"),
        ("POST", "/v1/rooms/join"),
        ("PATCH", "/v1/rooms/r_x"),
        ("PUT", "/v1/rooms/r_x/members/u_x"),
        ("POST", "/v1/rooms/r_x/ready"),
        ("POST", "/v1/rooms/r_x/kick"),
        ("POST", "/v1/queues/duel/tickets"),
        ("POST", "/v1/friends/requests"),
        ("POST", "/v1/party/invites"),
        ("POST", "/v1/party/kick"),
        ("POST", "/v1/reports"),
        ("POST", "/v1/matches/m_x/result"),
        ("POST", "/v1/matches/m_x/status"),
        ("POST", "/v1/matches/m_x/launch"),
        ("POST", "/v1/_test/clock"),
        ("GET", "/v1/ladders/duel/leaderboard?cursor=%00%ff&limit=-3"),
        ("GET", "/v1/users/%00/matches?limit=abc"),
        ("DELETE", "/v1/me?confirm=no"),
        ("GET", "/v1/events?resume=x&last_seq=-1"),
    ];
    let long = "é".repeat(5000);
    let bodies = [
        "",
        "null",
        "[]",
        "{",
        "\"text\"",
        "123",
        "{\"username\": 5}",
        "{\"display_name\": null}",
        "{\"options\": []}",
        "{\"options\": {\"teams\": [9, 9], \"password\": 7}}",
        "{\"code\": \"\\u0000\"}",
        "{\"friend_code\": [1]}",
        "{\"user_id\": {\"a\": 1}}",
        "{\"advance_s\": -5}",
        "{\"team\": \"charlie\"}",
        "{\"ready\": \"yes\"}",
        "{\"version\": 1, \"error\": 42}",
        "{\"state\": 1}",
    ];
    for (m, path) in endpoints {
        for b in bodies
            .iter()
            .copied()
            .chain([format!("{{\"display_name\": \"{long}\"}}").as_str()])
        {
            for t in [Some(token.as_str()), None] {
                let (st, v) = s.call(m, path, t, Some(b)).await;
                assert!(st < 500, "{m} {path} {b:.40} -> {st} {v}");
                if st >= 400 {
                    assert!(
                        v["error"]["code"].is_string(),
                        "{m} {path}: error without the protocol shape: {v}"
                    );
                }
            }
        }
    }
    // Bodies above the limits.
    let huge = format!("{{\"x\": \"{}\"}}", "a".repeat(80_000));
    let (st, v) = s
        .call("POST", "/v1/matches/m_x/result", Some(&token), Some(&huge))
        .await;
    assert_eq!((st, v["error"]["code"].as_str()), (413, Some("payload_too_large")));
    let (st, _) = s
        .call("PUT", &format!("/v1/blocks/{other_id}"), Some(&token), Some("{}"))
        .await;
    assert_eq!(st, 204);
    s.running.stop();
}

#[tokio::test]
async fn bans_block_play_but_keep_privacy_rights() {
    let s = start(BASE_CONFIG.parse().unwrap()).await;
    let token = s.login("cheater").await;
    let (_, me) = s.call("GET", "/v1/me", Some(&token), None).await;
    let id = me["id"].as_str().unwrap().to_string();
    {
        let core = s.running.state.lock();
        core.db
            .execute(
                "UPDATE users SET banned_until = ?1, ban_reason = 'cheating' WHERE id = ?2",
                rusqlite::params![1_800_003_600i64, id],
            )
            .unwrap();
    }
    let (st, v) = s.call("GET", "/v1/queues", Some(&token), None).await;
    assert_eq!((st, v["error"]["code"].as_str()), (403, Some("banned")));
    assert!(v["error"]["retry_after_s"].as_i64().unwrap() > 0);
    let (st, v) = s.call("GET", "/v1/me", Some(&token), None).await;
    assert_eq!(st, 200);
    assert_eq!(v["restrictions"][0]["kind"], "ban");
    let (st, _) = s.call("GET", "/v1/me/export", Some(&token), None).await;
    assert_eq!(st, 200);
    // Erasing a banned account keeps the ban on the login identity: logging in again gives a banned account.
    let (st, _) = s.call("DELETE", "/v1/me?confirm=erase", Some(&token), None).await;
    assert_eq!(st, 204);
    let again = s.login("cheater").await;
    let (_, me2) = s.call("GET", "/v1/me", Some(&again), None).await;
    assert_ne!(me2["id"], json!(id));
    assert_eq!(me2["restrictions"][0]["kind"], "ban");
    s.running.stop();
}

#[tokio::test]
async fn a_running_match_survives_a_restart() {
    let dir = std::env::temp_dir().join(format!("kalamar-restart-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut doc: toml::Table = BASE_CONFIG.parse().unwrap();
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
    let a = s.login("alpha").await;
    let b = s.login("bravo").await;
    let (_, room) = s.call("POST", "/v1/rooms", Some(&a), Some("{}")).await;
    let code = room["code"].as_str().unwrap();
    let rid = room["id"].as_str().unwrap();
    s.call(
        "POST",
        "/v1/rooms/join",
        Some(&b),
        Some(&json!({"code": code}).to_string()),
    )
    .await;
    s.call(
        "POST",
        &format!("/v1/rooms/{rid}/ready"),
        Some(&b),
        Some("{\"ready\": true}"),
    )
    .await;
    let (st, m) = s.call("POST", &format!("/v1/rooms/{rid}/start"), Some(&a), None).await;
    assert_eq!(st, 201, "{m}");
    let mid = m["id"].as_str().unwrap().to_string();
    s.call("POST", &format!("/v1/matches/{mid}/launch"), Some(&a), Some("{}"))
        .await;
    s.running.stop();
    drop(s);
    // Same database and secret: tokens, the match and its launch are still there.
    let s2 = start(doc).await;
    let (st, cur) = s2.call("GET", "/v1/matches/current", Some(&b), None).await;
    assert_eq!(st, 200);
    assert_eq!(cur["match"]["id"], json!(mid));
    assert_eq!(cur["match"]["state"], "running");
    assert_eq!(cur["match"]["participants"][0]["launched"], true);
    s2.running.stop();
    drop(s2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_missing_stage_is_drawn_again_not_voided() {
    let s = start(BASE_CONFIG.parse().unwrap()).await;
    let a = s.login("hoststage").await;
    let b = s.login("gueststage").await;
    let (_, room) = s.call("POST", "/v1/rooms", Some(&a), Some("{}")).await;
    let rid = room["id"].as_str().unwrap();
    s.call(
        "POST",
        "/v1/rooms/join",
        Some(&b),
        Some(&json!({"code": room["code"]}).to_string()),
    )
    .await;
    s.call(
        "POST",
        &format!("/v1/rooms/{rid}/ready"),
        Some(&b),
        Some("{\"ready\": true}"),
    )
    .await;
    let (_, m) = s.call("POST", &format!("/v1/rooms/{rid}/start"), Some(&a), None).await;
    let mid = m["id"].as_str().unwrap().to_string();
    let gid = m["game"]["match_id"].clone();
    for (t, code) in [(&a, "stage-unavailable"), (&b, "room-not-found")] {
        let body = json!({"version": 2, "match_id": gid, "error": code});
        let (st, _) = s
            .call(
                "POST",
                &format!("/v1/matches/{mid}/result"),
                Some(t),
                Some(&body.to_string()),
            )
            .await;
        assert_eq!(st, 200);
    }
    let (_, old) = s.call("GET", &format!("/v1/matches/{mid}"), Some(&a), None).await;
    assert_eq!(old["state"], "void");
    let next = old["outcome"]["rematch_id"]
        .as_str()
        .expect("a replacement match")
        .to_string();
    let (_, cur) = s.call("GET", "/v1/matches/current", Some(&b), None).await;
    assert_eq!(cur["match"]["id"], json!(next));
    assert_eq!(cur["match"]["state"], "ready");
    assert!(cur["match"]["rules"]["stage"].is_null(), "the game's draw");
    let (_, r) = s.call("GET", &format!("/v1/rooms/{rid}"), Some(&a), None).await;
    assert_eq!(r["match_id"], json!(next));
    s.running.stop();
}

#[tokio::test]
async fn activities_survive_a_restart() {
    let dir = std::env::temp_dir().join(format!("kalamar-activities-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let mut doc: toml::Table = BASE_CONFIG.parse().unwrap();
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
        let f = doc["features"].as_table_mut().unwrap();
        f.insert("activities".into(), true.into());
        f.insert("series".into(), true.into());
    }
    let act: toml::Table = r#"id = "bo3"
kind = "duel_series"
title = { en = "Best of 3" }"#
        .parse()
        .unwrap();
    doc.insert("activities".into(), toml::Value::Array(vec![toml::Value::Table(act)]));
    let s = start(doc.clone()).await;
    let a = s.login("waiter").await;
    let (st, v) = s
        .call("POST", "/v1/activities/bo3/actions/find", Some(&a), Some("{}"))
        .await;
    assert_eq!(st, 200, "{v}");
    s.running.stop();
    drop(s);
    let s2 = start(doc).await;
    let (_, list) = s2.call("GET", "/v1/activities", Some(&a), None).await;
    let item = &list["items"][0];
    assert_eq!(item["participants"]["count"], 1);
    assert_eq!(item["you"]["joined"], true, "{item}");
    s2.running.stop();
    drop(s2);
    let _ = std::fs::remove_dir_all(&dir);
}

#[tokio::test]
async fn a_referee_decides_through_the_operator_api_and_webhooks_are_signed() {
    use sha2::{Digest, Sha256};
    // A webhook receiver on 127.0.0.1.
    let got: Arc<Mutex<Vec<(String, String)>>> = Arc::default();
    let g2 = got.clone();
    let hook = axum::Router::new().route(
        "/hook",
        post(move |headers: HeaderMap, body: String| {
            let g = g2.clone();
            async move {
                let sig = headers
                    .get("kalamar-signature")
                    .and_then(|v| v.to_str().ok())
                    .unwrap_or_default()
                    .to_string();
                g.lock().unwrap().push((sig, body));
                "ok"
            }
        }),
    );
    let hl = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let hport = hl.local_addr().unwrap().port();
    tokio::spawn(async move { axum::serve(hl, hook).await.unwrap() });

    let hash: String = Sha256::digest(b"kop1.referee-token")
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let readonly: String = Sha256::digest(b"kop1.read-token")
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    let extra = format!(
        r#"
[[result_policies]]
id = "refereed"
quorum = "majority"
silence = "ignore"
disagreement = "void"
rated_requires_both_teams = false
decider = "referee"
review_deadline_s = 600
accept_s = 20
launch_s = 90
result_s = 1800
grace_s = 60
[rooms]
result_policy = "refereed"
[[operator_tokens]]
name = "referee"
sha256 = "{hash}"
scopes = ["read", "results", "moderation"]
[[operator_tokens]]
name = "site"
sha256 = "{readonly}"
scopes = ["read"]
[[webhooks]]
url = "http://127.0.0.1:{hport}/hook"
secret = "hook-secret"
events = ["match.completed"]
"#
    );
    let doc: toml::Table = format!("{BASE_CONFIG}{extra}").parse().unwrap();
    let s = start(doc).await;
    let (_, meta) = s.call("GET", "/v1/meta", None, None).await;
    assert_eq!(
        meta["operator_api"],
        json!({"webhooks": true, "bot_api": true, "referee": true})
    );
    let a = s.login("refa").await;
    let b = s.login("refb").await;
    let (_, room) = s.call("POST", "/v1/rooms", Some(&a), Some("{}")).await;
    let rid = room["id"].as_str().unwrap();
    s.call(
        "POST",
        "/v1/rooms/join",
        Some(&b),
        Some(&json!({"code": room["code"]}).to_string()),
    )
    .await;
    s.call(
        "POST",
        &format!("/v1/rooms/{rid}/ready"),
        Some(&b),
        Some("{\"ready\": true}"),
    )
    .await;
    let (_, m) = s.call("POST", &format!("/v1/rooms/{rid}/start"), Some(&a), None).await;
    let mid = m["id"].as_str().unwrap().to_string();
    for t in [&a, &b] {
        let body = json!({"version": 1, "match_id": m["game"]["match_id"], "error": "players-timeout"});
        s.call(
            "POST",
            &format!("/v1/matches/{mid}/result"),
            Some(t),
            Some(&body.to_string()),
        )
        .await;
    }
    let (_, under) = s.call("GET", &format!("/v1/matches/{mid}"), Some(&a), None).await;
    assert_eq!(under["state"], "under_review");
    assert!(under["outcome"].is_null());
    // The players are free while a referee looks.
    let (_, cur) = s.call("GET", "/v1/matches/current", Some(&a), None).await;
    assert!(cur["match"].is_null());
    // Tokens: unknown, read-only, referee.
    let (st, v) = s
        .call(
            "GET",
            "/v1/operator/matches?state=under_review",
            Some("kop1.nope"),
            None,
        )
        .await;
    assert_eq!((st, v["error"]["code"].as_str()), (401, Some("unauthorized")));
    let (st, list) = s
        .call(
            "GET",
            "/v1/operator/matches?state=under_review",
            Some("kop1.read-token"),
            None,
        )
        .await;
    assert_eq!(st, 200);
    assert_eq!(list["items"][0]["id"], json!(mid));
    let decision =
        json!({"counted": true, "winner": "bravo", "rated": false, "reason": {"en": "host's network failed"}})
            .to_string();
    let (st, v) = s
        .call(
            "POST",
            &format!("/v1/operator/matches/{mid}/decision"),
            Some("kop1.read-token"),
            Some(&decision),
        )
        .await;
    assert_eq!((st, v["error"]["code"].as_str()), (403, Some("forbidden")));
    let (st, v) = s
        .call(
            "POST",
            &format!("/v1/operator/matches/{mid}/decision"),
            Some("kop1.referee-token"),
            Some(&decision),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["state"], "completed");
    assert_eq!(v["outcome"]["decision"], "admin");
    assert_eq!(v["outcome"]["winner"], "bravo");
    let (st, _) = s
        .call(
            "POST",
            &format!("/v1/operator/matches/{mid}/decision"),
            Some("kop1.referee-token"),
            Some(&decision),
        )
        .await;
    assert_eq!(st, 409, "a decided match is not decided twice");
    let (_, detail) = s
        .call(
            "GET",
            &format!("/v1/operator/matches/{mid}"),
            Some("kop1.read-token"),
            None,
        )
        .await;
    assert_eq!(detail["reports"].as_array().map(Vec::len), Some(2));
    // The signed webhook.
    let mut tries = 0;
    while got.lock().unwrap().is_empty() && tries < 50 {
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        tries += 1;
    }
    let (sig, body) = got.lock().unwrap()[0].clone();
    let t: i64 = sig.split(',').next().unwrap().trim_start_matches("t=").parse().unwrap();
    assert_eq!(sig, kalamar_service::webhooks::sign(b"hook-secret", t, &body));
    let event: serde_json::Value = serde_json::from_str(&body).unwrap();
    assert_eq!(event["type"], "match.completed");
    assert_eq!(event["data"]["id"], json!(mid));
    assert!(
        event["data"].get("game").is_none(),
        "the relay room never leaves through webhooks"
    );
    s.running.stop();
}

#[tokio::test]
async fn refused_reports_say_why() {
    let s = start(BASE_CONFIG.parse().unwrap()).await;
    let a = s.login("whyhost").await;
    let b = s.login("whyguest").await;
    let (_, room) = s.call("POST", "/v1/rooms", Some(&a), Some("{}")).await;
    let rid = room["id"].as_str().unwrap();
    s.call(
        "POST",
        "/v1/rooms/join",
        Some(&b),
        Some(&json!({"code": room["code"]}).to_string()),
    )
    .await;
    s.call(
        "POST",
        &format!("/v1/rooms/{rid}/ready"),
        Some(&b),
        Some("{\"ready\": true}"),
    )
    .await;
    let (_, m) = s.call("POST", &format!("/v1/rooms/{rid}/start"), Some(&a), None).await;
    let mid = m["id"].as_str().unwrap().to_string();
    let parts = m["participants"].as_array().unwrap().clone();
    for (token, me) in [(&a, 0usize), (&b, 1usize)] {
        let players: Vec<Value> = parts
            .iter()
            .enumerate()
            .map(|(i, p)| {
                json!({"name": p["member_name"], "self": i == me, "team": p["team"], "paint": 100, "kills": 0,
                                 "assists": 0, "deaths": 0, "specials": 0, "disconnected": false})
            })
            .collect();
        // Both games agree, but on a mode the match did not ask for (check V7).
        let r = json!({"version": 1, "match_id": m["game"]["match_id"], "role": parts[me]["role"], "rule": 1,
            "mode": "another-mode", "stage_id": 1, "stage_scene": "S1", "battle_frames": 18000 + me as i64 * 3,
            "duration_s": 300.0, "win_team": 0, "winner": "alpha", "deemed_result": -1,
            "teams": [{"team": "alpha", "paint_points": 100, "turf_permille": 1}, {"team": "bravo", "paint_points": 100, "turf_permille": 1}],
            "players": players});
        let (st, _) = s
            .call(
                "POST",
                &format!("/v1/matches/{mid}/result"),
                Some(token),
                Some(&r.to_string()),
            )
            .await;
        assert_eq!(st, 200);
    }
    let (_, done) = s.call("GET", &format!("/v1/matches/{mid}"), Some(&a), None).await;
    let o = &done["outcome"];
    assert_eq!(o["decision"], "invalid_reports", "{o}");
    assert_eq!(o["invalid_reasons"].as_array().map(Vec::len), Some(2));
    assert_eq!(o["invalid_reasons"][0]["check"], "V7");
    assert!(o["summary"]["en"].as_str().unwrap().contains("V7"));
    s.running.stop();
}

/// 1.1.8: the two reports of a 1v1 between two machines: equal but for each game's own area share and the clocks.
/// It counts, the counters are informative, and the host's report is the record.
#[tokio::test]
async fn a_real_duel_whose_games_count_their_own_turf_is_counted() {
    let cfg = format!("{BASE_CONFIG}\n[rooms]\nresult_policy = \"standard\"\n");
    let s = start(cfg.parse().unwrap()).await;
    let (_, meta) = s.call("GET", "/v1/meta", None, None).await;
    let declared: Vec<&Value> = meta["result_policies"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| &p["stats"])
        .collect();
    assert!(
        declared.iter().all(|v| *v == "flag"),
        "the default applies to every policy: {declared:?}"
    );
    for (flag_gap, names) in [(false, ["duelhost", "duelguest"]), (true, ["gaphost", "gapguest"])] {
        let a = s.login(names[0]).await;
        let b = s.login(names[1]).await;
        let (_, room) = s.call("POST", "/v1/rooms", Some(&a), Some("{}")).await;
        let rid = room["id"].as_str().unwrap();
        s.call(
            "POST",
            "/v1/rooms/join",
            Some(&b),
            Some(&json!({"code": room["code"]}).to_string()),
        )
        .await;
        s.call(
            "POST",
            &format!("/v1/rooms/{rid}/ready"),
            Some(&b),
            Some("{\"ready\": true}"),
        )
        .await;
        let (_, m) = s.call("POST", &format!("/v1/rooms/{rid}/start"), Some(&a), None).await;
        let mid = m["id"].as_str().unwrap().to_string();
        let parts = m["participants"].as_array().unwrap().clone();
        let host = parts.iter().position(|p| p["role"] == "host").unwrap();
        let guest = 1 - host;
        // As on the server: the host in bravo, the guest in alpha, alpha won 904 to 736.
        let (host_team, guest_team) = (parts[host]["team"].clone(), parts[guest]["team"].clone());
        let win_team = if guest_team == "alpha" { 0 } else { 1 };
        let teams = |win: (i64, i64), lose: (i64, i64)| {
            let line = |t: &Value, (p, pm): (i64, i64)| json!({"team": t, "paint_points": p, "turf_permille": pm, "count": 100});
            json!([line(&guest_team, win), line(&host_team, lose), {"team": "charlie", "paint_points": 0, "turf_permille": 0, "count": 100}])
        };
        let file = |me: usize, t: Value, guest_kills: i64| {
            json!({"version": 2, "match_id": m["game"]["match_id"], "role": parts[me]["role"], "seat": "player",
                "outcome": "completed", "rule": 0, "mode": m["rules"]["mode"], "stage_id": 24,
                "stage_scene": "scene-a", "settings_ok": true, "battle_frames": 10800, "duration_s": 180.0,
                "end_utc": 1_800_000_300 + 3 * me as i64, "win_team": win_team,
                "winner": if win_team == 0 { "alpha" } else { "bravo" }, "deemed_result": -1, "teams": t,
                "players": [
                  {"name": parts[guest]["member_name"], "self": me == guest, "team": guest_team, "paint": 1600,
                   "kills": guest_kills, "assists": 0, "deaths": 1, "specials": 5, "disconnected": false,
                   "weapon": 1110, "gear": [8010, 25010, 6027]},
                  {"name": parts[host]["member_name"], "self": me == host, "team": host_team, "paint": 1401,
                   "kills": 1, "assists": 0, "deaths": 3, "specials": 0, "disconnected": false,
                   "weapon": 8012, "gear": [8009, 5027, 2026]}]})
        };
        let host_file = file(host, teams((904, 405), (736, 329)), 3);
        let mut guest_file = file(guest, teams((904, 399), (736, 325)), if flag_gap { 30 } else { 3 });
        guest_file.as_object_mut().unwrap().remove("settings_ok");
        for (me, f) in [(host, &host_file), (guest, &guest_file)] {
            let token = if parts[me]["user"]["id"] == m["you"]["user_id"] {
                &a
            } else {
                &b
            };
            let (st, v) = s
                .call(
                    "POST",
                    &format!("/v1/matches/{mid}/result"),
                    Some(token),
                    Some(&f.to_string()),
                )
                .await;
            assert_eq!(st, 200, "{v}");
        }
        let (_, done) = s.call("GET", &format!("/v1/matches/{mid}"), Some(&a), None).await;
        let o = &done["outcome"];
        assert_eq!(
            (&done["state"], &o["decision"]),
            (&json!("completed"), &json!("agreed")),
            "{o}"
        );
        assert_eq!(o["winner"], guest_team);
        assert_eq!(o["players"][0]["verdict"], "agreed");
        assert_eq!(o["players"][1]["verdict"], "agreed");
        let sc = &o["stats_check"];
        assert_eq!(
            (&sc["check"], &sc["differ"], &sc["flagged"]),
            (&json!("flag"), &json!(true), &json!(flag_gap)),
            "{sc}"
        );
        assert_eq!(
            sc["record_from"]["user_id"], parts[host]["user"]["id"],
            "two reports: the host's is the record"
        );
        // The record (and the statistics) are the host's report: its scores, its count of the guest's eliminations.
        assert_eq!(o["record"]["teams"][0]["turf_permille"], 405);
        let guest_id = parts[guest]["user"]["id"].as_str().unwrap();
        let (_, hist) = s
            .call("GET", &format!("/v1/users/{guest_id}/matches"), Some(&a), None)
            .await;
        assert_eq!(hist["items"][0]["stats"]["kills"], 3, "{hist}");
        assert_eq!(hist["items"][0]["result"], "win");
        if flag_gap {
            assert_eq!(sc["gaps"][0]["field"], "kills");
            assert_eq!(sc["gaps"][0]["user_id"], json!(guest_id));
            assert_eq!(sc["gaps"][0]["values"], json!([3, 30]));
            assert!(
                o["summary"]["en"].as_str().unwrap().contains("operator"),
                "{}",
                o["summary"]
            );
        } else {
            assert_eq!(sc["gaps"], json!([]));
        }
        // The public view keeps the check (the operator API and webhooks use it).
        let other = s.login(&format!("{}-viewer", names[0])).await;
        let (_, public) = s.call("GET", &format!("/v1/matches/{mid}"), Some(&other), None).await;
        assert_eq!(public["outcome"]["stats_check"]["flagged"], json!(flag_gap), "{public}");
    }
    s.running.stop();
}

#[tokio::test]
async fn an_imposed_loadout_only_reaches_engines_that_apply_it() {
    let extra = r#"
[rooms]
loadouts = [{ id = "one", label = { en = "One weapon" }, loadout = { all = { weapon = { fixed = "weapon-a" } } } }]
"#;
    let s = start(format!("{BASE_CONFIG}{extra}").parse().unwrap()).await;
    let a = s.login("loada").await;
    let b = s.login("loadb").await;
    let (_, meta) = s.call("GET", "/v1/meta", None, None).await;
    assert!(
        meta["rooms"]["options"]
            .as_array()
            .unwrap()
            .iter()
            .any(|f| f["key"] == "loadout")
    );
    let (st, room) = s
        .call(
            "POST",
            "/v1/rooms",
            Some(&a),
            Some(r#"{"options": {"loadout": "one"}}"#),
        )
        .await;
    assert_eq!(st, 201, "{room}");
    assert_eq!(room["options"]["loadout"], "one");
    let rid = room["id"].as_str().unwrap();
    s.call(
        "POST",
        "/v1/rooms/join",
        Some(&b),
        Some(&json!({"code": room["code"]}).to_string()),
    )
    .await;
    s.call(
        "POST",
        &format!("/v1/rooms/{rid}/ready"),
        Some(&b),
        Some("{\"ready\": true}"),
    )
    .await;
    let (st, v) = s.call("POST", &format!("/v1/rooms/{rid}/start"), Some(&a), None).await;
    assert_eq!(
        (st, v["error"]["code"].as_str()),
        (409, Some("incompatible_game")),
        "{v}"
    );
    for t in [&a, &b] {
        let caps = json!({"contract": {"id": "lan-auto-match", "version": 1}, "engine": {"match_loadout": true}});
        assert_eq!(
            s.call("PUT", "/v1/me/game", Some(t), Some(&caps.to_string())).await.0,
            204
        );
    }
    let (st, m) = s.call("POST", &format!("/v1/rooms/{rid}/start"), Some(&a), None).await;
    assert_eq!(st, 201, "{m}");
    assert_eq!(m["rules"]["loadout"]["all"]["weapon"]["fixed"], "weapon-a");
    s.running.stop();
}
