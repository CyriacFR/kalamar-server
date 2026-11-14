//! HTTP tests of badges, titles and profiles (protocol 1.1, `presentation.md`) on 127.0.0.1: a played match earns
//! badges shown by `GET /v1/users/{user_id}/badges`, `GET /v1/titles` and `GET /v1/users/{user_id}/profile`; the
//! featured badge and title of `PATCH /v1/me`; privacy (`public_profile`) and disabled areas.

use std::sync::Arc;

use kalamar_service::clock::ManualClock;
use kalamar_service::config::Config;
use serde_json::{Value, json};

const CONFIG: &str = r##"
[server]
name = "Test"
api_url = "http://127.0.0.1/api"
production = false
database = ":memory:"
privacy_url = "https://example.invalid/privacy"
operator = { name = "T", contact = "mailto:t@example.invalid" }
[features]
badges = true
profiles = true
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
[[badges]]
id = "first-match"
name = { en = "First match" }
description = { en = "Played a match." }
icon = "flag"
rule = "matches:1"
[[badges]]
id = "first-win"
name = { en = "First win" }
icon = "star"
color = "#D4AF37"
rule = "wins:1"
[[badges]]
id = "cup"
name = { en = "Cup champion" }
icon = "crown"
rule = "tournament:cup"
[[titles]]
id = "rookie"
text = { en = "Rookie" }
[[titles]]
id = "champ"
text = { en = "Champion" }
badge = "first-win"
"##;

struct Server {
    running: kalamar_service::app::Running,
    base: String,
    http: reqwest::Client,
}

async fn start(mut doc: toml::Table) -> Server {
    // The HTTP client's TLS provider (no TLS is used: the server is on 127.0.0.1).
    kalamar_service::auth::oauth::install_crypto();
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
        http: reqwest::Client::new(),
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
        (status, r.json::<Value>().await.unwrap_or(Value::Null))
    }
    async fn get(&self, path: &str, token: &str) -> (u16, Value) {
        self.call("GET", path, Some(token), None).await
    }
    /// Logs in with the dev provider: (access token, user id).
    async fn login(&self, name: &str) -> (String, String) {
        let body = json!({"username": name}).to_string();
        let (s, v) = self.call("POST", "/v1/auth/dev/login", None, Some(&body)).await;
        assert_eq!(s, 200, "{v}");
        let token = v["access_token"].as_str().unwrap().to_string();
        let (_, me) = self.get("/v1/me", &token).await;
        (token, me["id"].as_str().unwrap().to_string())
    }
}

/// The game's honest result file of participant `seat` (as `kalamar-testkit`'s simulated game writes it).
fn result_for(m: &Value, seat: usize, win_team: i64) -> Value {
    let ps = m["participants"].as_array().unwrap();
    let players: Vec<Value> = ps
        .iter()
        .enumerate()
        .map(|(i, p)| {
            json!({"name": p["member_name"], "self": i == seat, "team": p["team"], "paint": 600, "kills": 2,
                             "assists": 1, "deaths": 1, "specials": 1, "disconnected": false})
        })
        .collect();
    json!({"version": 1, "match_id": m["game"]["match_id"], "role": ps[seat]["role"], "rule": 0, "mode": m["rules"]["mode"],
           "stage_id": 1, "stage_scene": "Stage_Test01", "battle_frames": 10800, "duration_s": 180.0, "end_utc": 1_800_000_000,
           "win_team": win_team, "winner": if win_team == 0 { "alpha" } else { "bravo" }, "deemed_result": -1,
           "teams": [{"team": "alpha", "paint_points": 600, "turf_permille": 200},
                     {"team": "bravo", "paint_points": 600, "turf_permille": 200},
                     {"team": "charlie", "paint_points": 0, "turf_permille": 0}],
           "players": players})
}

fn ids(v: &Value) -> Vec<&str> {
    let mut v: Vec<&str> = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|b| b["id"].as_str().unwrap())
        .collect();
    v.sort();
    v
}

#[tokio::test]
async fn a_won_match_earns_badges_shown_with_titles_and_profile() {
    let s = start(CONFIG.parse().unwrap()).await;
    let (_, meta) = s.call("GET", "/v1/meta", None, None).await;
    let features = meta["features"].as_array().unwrap();
    assert!(
        features.contains(&json!("badges")) && features.contains(&json!("profiles")),
        "{meta}"
    );
    let (a, ida) = s.login("alpha").await;
    let (b, idb) = s.login("bravo").await;
    let (_, room) = s.call("POST", "/v1/rooms", Some(&a), Some("{}")).await;
    let rid = room["id"].as_str().unwrap();
    let join = json!({"code": room["code"]}).to_string();
    s.call("POST", "/v1/rooms/join", Some(&b), Some(&join)).await;
    let ready = format!("/v1/rooms/{rid}/ready");
    s.call("POST", &ready, Some(&b), Some("{\"ready\": true}")).await;
    let (st, m) = s.call("POST", &format!("/v1/rooms/{rid}/start"), Some(&a), None).await;
    assert_eq!(st, 201, "{m}");
    let mid = m["id"].as_str().unwrap().to_string();
    // Alpha wins; every player launches and uploads the same record.
    let ps = m["participants"].as_array().unwrap().clone();
    let winner = ps.iter().find(|p| p["team"] == "alpha").unwrap()["user"]["id"]
        .as_str()
        .unwrap()
        .to_string();
    for (seat, p) in ps.iter().enumerate() {
        let token = if p["user"]["id"] == json!(ida) { &a } else { &b };
        s.call("POST", &format!("/v1/matches/{mid}/launch"), Some(token), Some("{}"))
            .await;
        let body = result_for(&m, seat, 0).to_string();
        let (st, v) = s
            .call("POST", &format!("/v1/matches/{mid}/result"), Some(token), Some(&body))
            .await;
        assert_eq!(st, 200, "{v}");
    }
    let (_, done) = s.get(&format!("/v1/matches/{mid}"), &a).await;
    assert_eq!(done["state"], "completed", "{}", done["outcome"]);
    let (wt, loser, lt) = if winner == ida { (&a, idb, &b) } else { (&b, ida, &a) };

    let (st, v) = s.get(&format!("/v1/users/{winner}/badges"), lt).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(ids(&v), ["first-match", "first-win"]);
    let win = v["items"]
        .as_array()
        .unwrap()
        .iter()
        .find(|x| x["id"] == "first-win")
        .unwrap();
    assert_eq!(
        (&win["icon"], &win["color"], &win["name"]),
        (&json!("star"), &json!("#D4AF37"), &json!({"en": "First win"}))
    );
    assert_eq!(win["earned_at"], "2027-01-15T08:00:00Z");
    assert_eq!((&v["featured_badge"], &v["title"]), (&Value::Null, &Value::Null));
    let (_, v) = s.get(&format!("/v1/users/{loser}/badges"), wt).await;
    assert_eq!(ids(&v), ["first-match"]);

    let (st, v) = s.get("/v1/titles", wt).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(v["items"][0]["id"], "rookie");
    assert_eq!(
        (&v["items"][1]["id"], &v["items"][1]["unlocked"]),
        (&json!("champ"), &json!(true))
    );
    assert_eq!(v["items"][1]["hint"]["en"], "Earn the badge \"First win\".");
    let (_, v) = s.get("/v1/titles", lt).await;
    assert_eq!(v["items"][1]["unlocked"], false);

    // Only an earned badge and an unlocked title can be shown.
    let pick = json!({"featured_badge": "first-win"}).to_string();
    let (st, v) = s.call("PATCH", "/v1/me", Some(lt), Some(&pick)).await;
    assert_eq!(
        (st, v["error"]["field"].as_str()),
        (422, Some("/featured_badge")),
        "{v}"
    );
    let (st, v) = s
        .call("PATCH", "/v1/me", Some(lt), Some("{\"title\": \"champ\"}"))
        .await;
    assert_eq!((st, v["error"]["field"].as_str()), (422, Some("/title")), "{v}");
    let pick = json!({"featured_badge": "first-win", "title": "champ"}).to_string();
    let (st, me) = s.call("PATCH", "/v1/me", Some(wt), Some(&pick)).await;
    assert_eq!(st, 200, "{me}");
    assert_eq!(
        (&me["featured_badge"], &me["title"]),
        (&json!("first-win"), &json!("champ"))
    );
    let (_, v) = s.get(&format!("/v1/users/{winner}/badges"), lt).await;
    assert_eq!(
        (&v["featured_badge"], &v["title"]),
        (&json!("first-win"), &json!({"en": "Champion"}))
    );

    let (st, p) = s.get(&format!("/v1/users/{winner}/profile"), lt).await;
    assert_eq!(st, 200, "{p}");
    assert_eq!(p["id"], json!(winner));
    let sections = p["sections"].as_array().unwrap();
    let card = &sections[0];
    assert_eq!(
        (&card["type"], &card["subtitle"]),
        (&json!("user"), &json!({"en": "Champion"}))
    );
    assert_eq!(card["badges"].as_array().unwrap().len(), 2);
    assert_eq!(sections[1]["type"], "fields");
    assert_eq!(sections[1]["items"][1]["value"], 1, "one win");
    let recent = sections.iter().find(|x| x["id"] == "recent").unwrap();
    assert_eq!(recent["children"][0]["items"][0]["title"]["en"], "Win");
    assert_eq!(recent["children"][0]["items"][0]["subtitle"]["en"], "Private room");
    let shown = sections.iter().find(|x| x["id"] == "badges").unwrap();
    assert_eq!(shown["children"][0]["items"].as_array().unwrap().len(), 2);

    // null clears.
    let (st, me) = s
        .call("PATCH", "/v1/me", Some(wt), Some("{\"featured_badge\": null}"))
        .await;
    assert_eq!(st, 200, "{me}");
    assert_eq!(
        (&me["featured_badge"], &me["title"]),
        (&Value::Null, &json!("champ")),
        "null clears the badge only"
    );
    s.running.stop();
}

#[tokio::test]
async fn private_profiles_and_disabled_areas() {
    let s = start(CONFIG.parse().unwrap()).await;
    let (a, ida) = s.login("private").await;
    let (b, idb) = s.login("friend").await;
    let (c, _) = s.login("stranger").await;
    {
        let mut core = s.running.state.lock();
        kalamar_service::badges::on_tournament_win(&mut core, "cup", &ida);
        let (x, y) = if ida < idb { (&ida, &idb) } else { (&idb, &ida) };
        core.db
            .execute(
                "INSERT INTO friendships (user_a, user_b, created_at) VALUES (?1, ?2, 0)",
                rusqlite::params![x, y],
            )
            .unwrap();
    }
    let (st, v) = s.get(&format!("/v1/users/{ida}/badges"), &c).await;
    assert_eq!(st, 200, "{v}");
    assert_eq!(ids(&v), ["cup"]);
    assert_eq!(v["items"][0]["icon"], "crown");
    let hide = json!({"settings": {"public_profile": false}}).to_string();
    let (st, _) = s.call("PATCH", "/v1/me", Some(&a), Some(&hide)).await;
    assert_eq!(st, 200);
    for path in [format!("/v1/users/{ida}/badges"), format!("/v1/users/{ida}/profile")] {
        let (st, v) = s.get(&path, &c).await;
        assert_eq!(
            (st, v["error"]["code"].as_str()),
            (403, Some("private_profile")),
            "{path}"
        );
        assert_eq!(s.get(&path, &a).await.0, 200, "the player sees their own {path}");
        assert_eq!(s.get(&path, &b).await.0, 200, "a friend sees {path}");
    }
    let (st, _) = s.get("/v1/users/u_nobody/badges", &c).await;
    assert_eq!(st, 404);
    let (st, _) = s.get("/v1/users/u_nobody/profile", &c).await;
    assert_eq!(st, 404);
    s.running.stop();

    // A server without the areas.
    let mut doc: toml::Table = CONFIG.parse().unwrap();
    let f = doc["features"].as_table_mut().unwrap();
    f.insert("badges".into(), false.into());
    f.insert("profiles".into(), false.into());
    let s = start(doc).await;
    let (a, ida) = s.login("plain").await;
    for path in [
        "/v1/titles".to_string(),
        format!("/v1/users/{ida}/badges"),
        format!("/v1/users/{ida}/profile"),
    ] {
        let (st, v) = s.get(&path, &a).await;
        assert_eq!(
            (st, v["error"]["code"].as_str()),
            (404, Some("feature_disabled")),
            "{path}"
        );
    }
    let (st, v) = s
        .call("PATCH", "/v1/me", Some(&a), Some("{\"featured_badge\": \"cup\"}"))
        .await;
    assert_eq!((st, v["error"]["field"].as_str()), (422, Some("/featured_badge")));
    let (_, me) = s.get("/v1/me", &a).await;
    assert!(me.get("featured_badge").is_none(), "{me}");
    s.running.stop();
}
