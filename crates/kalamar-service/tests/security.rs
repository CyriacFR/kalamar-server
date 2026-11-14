//! Regression tests of the security review (docs/security-model.md): each attack the review proved
//! against the reference service, run again; each test failed before its fix. Every request goes to 127.0.0.1.

use std::sync::Arc;

use kalamar_service::clock::ManualClock;
use kalamar_service::config::Config;
use serde_json::{Value, json};

const NOW: i64 = 1_800_000_000;

fn base_config(extra: &str) -> toml::Table {
    format!(
        r#"
[server]
name = "Review"
api_url = "http://127.0.0.1/api"
production = false
database = ":memory:"
privacy_url = "https://example.invalid/privacy"
trusted_proxies = ["127.0.0.1/32"]
operator = {{ name = "T", contact = "mailto:t@example.invalid" }}
[features]
test_clock = true
spectators = true
[auth.dev]
enabled = true
[auth.guest]
enabled = true
[penalties]
counted = ["decline", "accept_timeout", "no_show", "left"]
window_s = 86400
cooldowns_s = [86400]
[[result_policies]]
id = "standard"
name = {{ en = "Standard" }}
quorum = "majority"
silence = "consent_after_play"
disagreement = "majority"
rated_requires_both_teams = true
accept_s = 20
launch_s = 90
result_s = 2400
grace_s = 120
[[result_policies]]
id = "forfeit"
name = {{ en = "A disconnected team loses" }}
quorum = "majority"
silence = "consent_after_play"
disagreement = "majority"
rated_requires_both_teams = true
disconnect = "forfeit"
leave = "forfeit"
accept_s = 20
launch_s = 90
result_s = 2400
grace_s = 120
[[result_policies]]
id = "witnessed"
name = {{ en = "A spectator breaks ties (as the tournament preset and the example configuration did)" }}
quorum = "majority"
silence = "consent_after_play"
disagreement = "majority"
rated_requires_both_teams = true
accept_s = 20
launch_s = 90
result_s = 2400
grace_s = 120
witness = "tiebreak"
witness_from = "any"
[[ladders]]
id = "duel"
name = {{ en = "Duel" }}
[[queues]]
id = "duel"
name = {{ en = "Ranked duel" }}
teams = [1, 1]
rated = true
ladder = "duel"
pool = [{{ mode = "turf" }}]
result_policy = "standard"
[[queues]]
id = "duel-forfeit"
name = {{ en = "Ranked duel, forfeits" }}
teams = [1, 1]
rated = true
ladder = "duel"
pool = [{{ mode = "turf" }}]
result_policy = "forfeit"
[[queues]]
id = "duel-witnessed"
name = {{ en = "Ranked duel, witnessed" }}
teams = [1, 1]
rated = true
ladder = "duel"
pool = [{{ mode = "turf" }}]
result_policy = "witnessed"
spectators = {{ seats = 2, who = "anyone" }}
{extra}
"#
    )
    .parse()
    .expect("toml")
}

const GENEROUS: &str = r#"
[rate_limits]
per_ip_rps = 10000.0
per_ip_burst = 10000.0
per_user_rps = 10000.0
per_user_burst = 10000.0
auth_per_ip_per_min = 100000.0
room_join_per_min = 10000.0
spectate_per_min = 10000.0
"#;

const RELAY: &str = "
[[relays]]
host = \"127.0.0.1\"
";

struct Server {
    running: kalamar_service::app::Running,
    base: String,
    http: reqwest::Client,
}

struct P {
    token: String,
    id: String,
}

async fn start(doc: toml::Table) -> Server {
    let mut doc = doc;
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    doc["server"]
        .as_table_mut()
        .unwrap()
        .insert("api_url".into(), format!("http://127.0.0.1:{port}/api").into());
    let cfg: Config = toml::Value::Table(doc).try_into().unwrap();
    let st = kalamar_service::app::build_state(cfg, Arc::new(ManualClock::new(NOW))).unwrap();
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
    async fn call_from(
        &self,
        ip: Option<&str>,
        method: &str,
        path: &str,
        token: Option<&str>,
        body: Option<Value>,
    ) -> (u16, Value) {
        let mut rb = self.http.request(
            reqwest::Method::from_bytes(method.as_bytes()).unwrap(),
            format!("{}{path}", self.base),
        );
        if let Some(ip) = ip {
            rb = rb.header("x-forwarded-for", ip);
        }
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

    async fn call(&self, method: &str, path: &str, token: Option<&str>, body: Option<Value>) -> (u16, Value) {
        self.call_from(None, method, path, token, body).await
    }

    async fn with_game(&self, v: &Value) -> P {
        let token = v["access_token"].as_str().unwrap().to_string();
        let id = v["me"]["id"].as_str().unwrap().to_string();
        let game = json!({"contract": {"id": "lan-auto-match", "version": 1},
            "contracts": [{"id": "lan-auto-match", "versions": [1, 2]}],
            "modes": ["turf"], "stages": [], "can_select_stage": false,
            "engine": {"stage_list": "all", "spectators": true,
                       "spectator_seats": {"max": 2, "modes": ["turf"], "verified_modes": ["turf"], "host": true}},
            "launcher": {"spectate": [1]}});
        let (s, v) = self.call("PUT", "/v1/me/game", Some(&token), Some(game)).await;
        assert_eq!(s, 204, "{v}");
        P { token, id }
    }

    async fn player(&self, name: &str) -> P {
        let (s, v) = self
            .call("POST", "/v1/auth/dev/login", None, Some(json!({"username": name})))
            .await;
        assert_eq!(s, 200, "{v}");
        self.with_game(&v).await
    }

    async fn clock(&self, secs: i64) {
        let (s, v) = self
            .call("POST", "/v1/_test/clock", None, Some(json!({"advance_s": secs})))
            .await;
        assert_eq!(s, 200, "{v}");
    }

    async fn post(&self, p: &P, path: &str, body: Value) -> (u16, Value) {
        self.call("POST", path, Some(&p.token), Some(body)).await
    }

    async fn view(&self, p: &P, mid: &str) -> Value {
        self.call("GET", &format!("/v1/matches/{mid}"), Some(&p.token), None)
            .await
            .1
    }

    /// Two players found in `queue` (and the given spectators), accepted: the match id.
    async fn ready(&self, queue: &str, a: &P, b: &P, watchers: &[&P]) -> String {
        for p in [a, b] {
            let (s, v) = self.post(p, &format!("/v1/queues/{queue}/tickets"), json!({})).await;
            assert_eq!(s, 201, "{v}");
        }
        self.clock(0).await;
        let m = self.call("GET", "/v1/matches/current", Some(&a.token), None).await.1["match"].clone();
        assert_eq!(m["state"], "pending_accept", "{m}");
        let mid = m["id"].as_str().unwrap().to_string();
        for w in watchers {
            let (s, v) = self.post(w, &format!("/v1/matches/{mid}/spectate"), json!({})).await;
            assert_eq!(s, 200, "{v}");
        }
        for p in [a, b] {
            let (s, v) = self.post(p, &format!("/v1/matches/{mid}/accept"), json!({})).await;
            assert_eq!(s, 200, "{v}");
        }
        mid
    }

    async fn statuses(&self, p: &P, mid: &str, states: &[&str]) {
        let (s, v) = self.post(p, &format!("/v1/matches/{mid}/launch"), json!({})).await;
        assert_eq!(s, 200, "{v}");
        for st in states {
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

    fn count(&self, sql: &str) -> i64 {
        let core = self.running.state.lock();
        core.db.query_row(sql, [], |r| r.get(0)).unwrap()
    }
}

/// A result file as a game writes it, built from the participant view of `uploader` (`me`), with `winner` the team
/// that wins and the members of `disconnected` marked disconnected. `witness`: a spectator's version 2 file.
fn record(view: &Value, winner: &str, disconnected: &[&str], witness: bool) -> Value {
    let me = view["you"]["member_name"].as_str().unwrap_or("");
    let players: Vec<Value> = view["participants"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| {
            let name = p["member_name"].as_str().unwrap();
            let team = p["team"].as_str().unwrap();
            json!({"name": name, "self": !witness && name == me, "team": team,
                   "paint": if team == winner { 900 } else { 300 }, "kills": 3, "assists": 1, "deaths": 2,
                   "specials": 1, "disconnected": disconnected.contains(&name), "weapon": 40})
        })
        .collect();
    let (wt, (ap, at), (bp, bt)) = if winner == "alpha" {
        (0, (900, 600), (300, 200))
    } else {
        (1, (300, 200), (900, 600))
    };
    let mut r = json!({"version": 1, "match_id": view["game"]["match_id"], "role": view["you"]["role"],
        "rule": 0, "mode": "turf", "stage_id": 1, "stage_scene": "st-a", "end_utc": NOW + 300,
        "battle_frames": 10800, "duration_s": 180.0, "win_team": wt, "winner": winner, "deemed_result": -1,
        "teams": [{"team": "alpha", "paint_points": ap, "turf_permille": at},
                  {"team": "bravo", "paint_points": bp, "turf_permille": bt}],
        "players": players, "t0_unix": NOW as f64, "timeline_s": []});
    if witness {
        r["version"] = json!(2);
        r["outcome"] = json!("spectated");
        r["seat"] = json!("spectator");
        r["round"] = json!(1);
        r["rule_name"] = json!("R0");
    }
    r
}

fn error_report(view: &Value, code: &str) -> Value {
    json!({"version": 1, "match_id": view["game"]["match_id"], "error": code, "reason": "game failed"})
}

/// H1. One cheater, no accomplice: the victim's game fails before the match (the cheater just leaves the lobby) and
/// its launcher honestly reports the error; the cheater uploads a forged record that marks the victim disconnected.
/// Up to 1.1.11 the error counted as support of the forgery: completed, the cheater the winner, rated.
#[tokio::test]
async fn a_forged_record_never_wins_over_an_honest_error_before_the_match() {
    let s = start(base_config(&format!("{GENEROUS}{RELAY}"))).await;
    let (ana, bo) = (s.player("ana").await, s.player("bo").await);
    let mid = s.ready("duel", &ana, &bo, &[]).await;
    s.statuses(&ana, &mid, &["booting", "lobby"]).await;
    s.statuses(&bo, &mid, &["booting", "lobby", "in-room", "loading-stage", "in-match"])
        .await;
    let bo_view = s.view(&bo, &mid).await;
    let ana_view = s.view(&ana, &mid).await;
    let bo_team = bo_view["you"]["team"].as_str().unwrap().to_string();
    let ana_name = ana_view["you"]["member_name"].as_str().unwrap().to_string();
    let ana_code = if ana_view["you"]["role"] == "host" {
        "players-timeout"
    } else {
        "host-left"
    };
    let forged = record(&bo_view, &bo_team, &[&ana_name], false);
    let (st, v) = s.post(&bo, &format!("/v1/matches/{mid}/result"), forged).await;
    assert_eq!(st, 200, "{v}");
    let (st, v) = s
        .post(
            &ana,
            &format!("/v1/matches/{mid}/result"),
            error_report(&ana_view, ana_code),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    let m = s.view(&ana, &mid).await;
    let o = &m["outcome"];
    assert_eq!(m["state"], "void", "{o}");
    assert_eq!(
        (o["counted"].clone(), o["rated"].clone()),
        (json!(false), json!(false)),
        "{o}"
    );
    assert_eq!(o["rating_changes"], json!([]), "{o}");
    s.running.stop();
}

/// H1, the same in the match: the cheater quits mid-match (the victim's game reports `connection-lost`) and forges a
/// record that shows the victim gone, under the standard policy and under `disconnect = "forfeit"` (where the record
/// alone would make the victim's team lose). A duel's record never explains the other player's failure.
#[tokio::test]
async fn a_forged_record_never_wins_over_an_honest_error_in_the_match() {
    let s = start(base_config(&format!("{GENEROUS}{RELAY}"))).await;
    for (k, queue) in ["duel", "duel-forfeit"].iter().enumerate() {
        let (ana, bo) = (s.player(&format!("ana{k}")).await, s.player(&format!("bo{k}")).await);
        let mid = s.ready(queue, &ana, &bo, &[]).await;
        for p in [&ana, &bo] {
            s.statuses(p, &mid, &["booting", "lobby", "in-room", "loading-stage", "in-match"])
                .await;
        }
        let bo_view = s.view(&bo, &mid).await;
        let ana_view = s.view(&ana, &mid).await;
        let bo_team = bo_view["you"]["team"].as_str().unwrap().to_string();
        let ana_name = ana_view["you"]["member_name"].as_str().unwrap().to_string();
        let (st, v) = s
            .post(
                &bo,
                &format!("/v1/matches/{mid}/result"),
                record(&bo_view, &bo_team, &[&ana_name], false),
            )
            .await;
        assert_eq!(st, 200, "{v}");
        let (st, v) = s
            .post(
                &ana,
                &format!("/v1/matches/{mid}/result"),
                error_report(&ana_view, "connection-lost"),
            )
            .await;
        assert_eq!(st, 200, "{v}");
        let m = s.view(&ana, &mid).await;
        assert_eq!(m["state"], "void", "{queue}: {}", m["outcome"]);
        assert_eq!(m["outcome"]["rating_changes"], json!([]), "{queue}");
    }
    s.running.stop();
}

/// H1: a forged record against a player who played and then sent nothing counts (`consent_after_play`), but only a
/// valid result from each team rates a match.
#[tokio::test]
async fn a_record_from_one_team_alone_is_never_rated() {
    let s = start(base_config(&format!("{GENEROUS}{RELAY}"))).await;
    let (ana, bo) = (s.player("ana4").await, s.player("bo4").await);
    let mid = s.ready("duel", &ana, &bo, &[]).await;
    for p in [&ana, &bo] {
        s.statuses(p, &mid, &["booting", "lobby", "in-room", "loading-stage", "in-match"])
            .await;
    }
    let bo_view = s.view(&bo, &mid).await;
    let bo_team = bo_view["you"]["team"].as_str().unwrap().to_string();
    let (st, v) = s
        .post(
            &bo,
            &format!("/v1/matches/{mid}/result"),
            record(&bo_view, &bo_team, &[], false),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    s.clock(121).await;
    let m = s.view(&ana, &mid).await;
    let o = &m["outcome"];
    assert_eq!(
        (o["decision"].as_str(), o["rated"].as_bool()),
        (Some("single"), Some(false)),
        "{o}"
    );
    assert_eq!(o["unrated_reason"], "one_sided", "{o}");
    assert_eq!(o["rating_changes"], json!([]), "{o}");
    s.running.stop();
}

/// M3. `witness = "tiebreak"` with `witness_from = "any"` and places for `anyone` (a configuration `check-config`
/// warns about): the cheater's own guest account sits in the stands and "witnesses" the forged record.
/// It may still break the tie its policy declares, but it never rates the match (only a referee stands for a team).
#[tokio::test]
async fn a_second_account_in_the_stands_never_rates_a_flipped_duel() {
    let s = start(base_config(&format!("{GENEROUS}{RELAY}"))).await;
    let (ana, bo) = (s.player("ana2").await, s.player("bo2").await);
    let (st, v) = s
        .call(
            "POST",
            "/v1/auth/guest/login",
            None,
            Some(json!({"display_name": "Alt"})),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    let alt = s.with_game(&v).await;
    let mid = s.ready("duel-witnessed", &ana, &bo, &[&alt]).await;
    for p in [&alt, &ana, &bo] {
        s.statuses(p, &mid, &["booting", "lobby", "in-room", "loading-stage", "in-match"])
            .await;
    }
    let ana_view = s.view(&ana, &mid).await;
    let bo_view = s.view(&bo, &mid).await;
    let alt_view = s.view(&alt, &mid).await;
    let ana_team = ana_view["you"]["team"].as_str().unwrap().to_string();
    let bo_team = bo_view["you"]["team"].as_str().unwrap().to_string();
    for (p, view, team, witness) in [
        (&ana, &ana_view, &ana_team, false),
        (&bo, &bo_view, &bo_team, false),
        (&alt, &alt_view, &bo_team, true),
    ] {
        let (st, v) = s
            .post(
                p,
                &format!("/v1/matches/{mid}/result"),
                record(view, team, &[], witness),
            )
            .await;
        assert_eq!(st, 200, "{v}");
    }
    s.clock(1).await;
    let m = s.view(&ana, &mid).await;
    let o = &m["outcome"];
    assert_eq!(o["rated"], false, "{o}");
    assert_eq!(o["rating_changes"], json!([]), "{o}");
    s.running.stop();
}

/// M3, M7: `check-config` says it when one player can decide competitive results with second accounts: guest accounts
/// on rated queues, any spectator as a witness where the places are open to non-referees.
#[test]
fn check_config_warns_about_second_accounts_in_ranked_play() {
    let cfg: Config = toml::Value::Table(base_config(RELAY)).try_into().unwrap();
    let w = cfg.competitive_warnings().join("\n");
    assert!(
        w.contains("auth.guest: guest accounts may play the rated queues"),
        "{w}"
    );
    assert!(
        w.contains("queues.duel-witnessed: its result policy `witnessed` lets any spectator break a tie"),
        "{w}"
    );
    assert!(!w.contains("queues.duel:"), "{w}");
    let mut doc = base_config(RELAY);
    doc["auth"]["guest"]
        .as_table_mut()
        .unwrap()
        .insert("ranked".into(), false.into());
    for p in doc["result_policies"].as_array_mut().unwrap() {
        p.as_table_mut()
            .unwrap()
            .insert("witness_from".into(), "referees".into());
    }
    let cfg: Config = toml::Value::Table(doc).try_into().unwrap();
    assert!(
        cfg.competitive_warnings().is_empty(),
        "{:?}",
        cfg.competitive_warnings()
    );
}

/// M7: with `[auth.guest] ranked = false`, a guest account plays rooms and unrated queues, never a rated queue or a
/// tournament (the requirement `account` says why).
#[tokio::test]
async fn guest_accounts_stay_out_of_ranked_play_when_the_server_says_so() {
    let extra = format!(
        "{GENEROUS}{RELAY}\n[[queues]]\nid = \"casual\"\nname = {{ en = \"Casual\" }}\nteams = [1, 1]\npool = [{{ mode = \"turf\" }}]\nresult_policy = \"standard\"\n[[activities]]\nid = \"cup\"\nkind = \"tournament\"\ntitle = {{ en = \"Cup\" }}\nsize = 2\nbest_of = 3\nmode = \"turf\"\nresult_policy = \"standard\"\n"
    );
    let mut doc = base_config(&extra);
    doc["auth"]["guest"]
        .as_table_mut()
        .unwrap()
        .insert("ranked".into(), false.into());
    doc["features"]
        .as_table_mut()
        .unwrap()
        .insert("activities".into(), true.into());
    doc["features"]
        .as_table_mut()
        .unwrap()
        .insert("series".into(), true.into());
    let s = start(doc).await;
    let (st, v) = s
        .call(
            "POST",
            "/v1/auth/guest/login",
            None,
            Some(json!({"display_name": "Guest"})),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    let guest = s.with_game(&v).await;
    let (st, v) = s.post(&guest, "/v1/queues/duel/tickets", json!({})).await;
    assert_eq!((st, v["error"]["code"].as_str()), (403, Some("not_eligible")), "{v}");
    assert_eq!(v["error"]["details"]["requirements"][0]["id"], "account", "{v}");
    let (_, q) = s.call("GET", "/v1/queues/duel", Some(&guest.token), None).await;
    assert_eq!(q["eligible"], false, "{q}");
    let (st, v) = s.post(&guest, "/v1/activities/cup/actions/register", json!({})).await;
    assert_eq!((st, v["error"]["code"].as_str()), (403, Some("not_eligible")), "{v}");
    let (st, v) = s.post(&guest, "/v1/queues/casual/tickets", json!({})).await;
    assert_eq!(st, 201, "an unrated queue: {v}");
    // An account with a login of its own plays ranked.
    let member = s.player("member").await;
    let (st, v) = s.post(&member, "/v1/queues/duel/tickets", json!({})).await;
    assert_eq!(st, 201, "{v}");
    s.running.stop();
}

/// M8. Up to 1.1.11 a void series of a tournament let its first-listed player through: that player voided every
/// game (an error at once) and advanced. A void series is now drawn by lot: over 16 cups, the first-listed player
/// does not always advance (a false failure has odds of 1 in 65536).
#[tokio::test]
async fn voiding_every_game_of_a_tournament_series_is_a_coin_flip() {
    let extra = format!(
        "{GENEROUS}{RELAY}\n[[activities]]\nid = \"cup\"\nkind = \"tournament\"\ntitle = {{ en = \"Cup\" }}\nsize = 2\nbest_of = 3\nmode = \"turf\"\nresult_policy = \"standard\"\n"
    );
    let mut first_won = 0;
    for run in 0..16 {
        let mut doc = base_config(&extra);
        doc["features"]
            .as_table_mut()
            .unwrap()
            .insert("activities".into(), true.into());
        doc["features"]
            .as_table_mut()
            .unwrap()
            .insert("series".into(), true.into());
        let s = start(doc).await;
        let (first, second) = (
            s.player(&format!("first{run}")).await,
            s.player(&format!("second{run}")).await,
        );
        for p in [&first, &second] {
            let (st, v) = s.post(p, "/v1/activities/cup/actions/register", json!({})).await;
            assert_eq!(st, 200, "{v}");
        }
        for _ in 0..6 {
            let m = s.call("GET", "/v1/matches/current", Some(&first.token), None).await.1["match"].clone();
            if m.is_null() {
                break;
            }
            let mid = m["id"].as_str().unwrap().to_string();
            let v1 = s.view(&first, &mid).await;
            let (st, v) = s
                .post(
                    &first,
                    &format!("/v1/matches/{mid}/result"),
                    error_report(&v1, "launcher-failed"),
                )
                .await;
            assert_eq!(st, 200, "{v}");
            s.statuses(
                &second,
                &mid,
                &["booting", "lobby", "creating-room", "waiting-for-players"],
            )
            .await;
            let v2 = s.view(&second, &mid).await;
            let (st, v) = s
                .post(
                    &second,
                    &format!("/v1/matches/{mid}/result"),
                    error_report(&v2, "players-timeout"),
                )
                .await;
            assert_eq!(st, 200, "{v}");
        }
        let champion = {
            let core = s.running.state.lock();
            core.live.activities.get("cup").and_then(|a| a.champion.clone())
        };
        assert!(
            champion.as_deref() == Some(first.id.as_str()) || champion.as_deref() == Some(second.id.as_str()),
            "{champion:?}"
        );
        if champion.as_deref() == Some(first.id.as_str()) {
            first_won += 1;
        }
        s.running.stop();
    }
    assert!(first_won < 16, "the first-listed player advanced every time");
}

/// M6. A player with a running queue cooldown and a cheating report against them erases the account and signs up
/// again with the same address: the cooldown, the offense and the open report follow the login identity (kept as a
/// keyed hash, for a limited time).
#[tokio::test]
async fn erase_and_sign_up_again_keeps_the_cooldown_and_the_reports() {
    let extra = format!(
        "{GENEROUS}{RELAY}\n[auth.password]\nenabled = true\nsignup = true\n[auth.password.argon2]\nmemory_kib = 8192\niterations = 1\nparallelism = 1\n"
    );
    let s = start(base_config(&extra)).await;
    let signup =
        |email: &'static str| json!({"email": email, "password": "correct horse battery", "display_name": "Mallory"});
    let (st, v) = s
        .call(
            "POST",
            "/v1/auth/password-signup/login",
            None,
            Some(signup("mallory@example.org")),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    let mal = s.with_game(&v).await;
    let ana = s.player("ana3").await;
    for p in [&ana, &mal] {
        let (st, v) = s.post(p, "/v1/queues/duel/tickets", json!({})).await;
        assert_eq!(st, 201, "{v}");
    }
    s.clock(0).await;
    let m = s.call("GET", "/v1/matches/current", Some(&mal.token), None).await.1["match"].clone();
    let mid = m["id"].as_str().unwrap().to_string();
    let (st, v) = s.post(&mal, &format!("/v1/matches/{mid}/decline"), json!({})).await;
    assert_eq!(st, 200, "{v}");
    let (st, v) = s.post(&mal, "/v1/queues/duel/tickets", json!({})).await;
    assert_eq!((st, v["error"]["code"].as_str()), (403, Some("queue_cooldown")), "{v}");
    let (st, v) = s
        .post(
            &ana,
            "/v1/reports",
            json!({"user_id": mal.id, "category": "cheating", "comment": "edited results"}),
        )
        .await;
    assert_eq!(st, 201, "{v}");
    let (st, v) = s.call("DELETE", "/v1/me?confirm=erase", Some(&mal.token), None).await;
    assert_eq!(st, 204, "{v}");
    // Erased: nothing names the account any more, the report waits for the identity.
    assert_eq!(
        s.count(&format!("SELECT COUNT(*) FROM users WHERE id = '{}'", mal.id)),
        0
    );
    assert_eq!(s.count("SELECT COUNT(*) FROM reports WHERE target IS NULL"), 1);
    let (st, v) = s
        .call(
            "POST",
            "/v1/auth/password-signup/login",
            None,
            Some(signup("mallory@example.org")),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    let mal2 = s.with_game(&v).await;
    assert_ne!(mal.id, mal2.id);
    let (st, v) = s.post(&mal2, "/v1/queues/duel/tickets", json!({})).await;
    assert_eq!((st, v["error"]["code"].as_str()), (403, Some("queue_cooldown")), "{v}");
    assert_eq!(
        s.count(&format!("SELECT COUNT(*) FROM offences WHERE user_id = '{}'", mal2.id)),
        1
    );
    assert_eq!(
        s.count(&format!("SELECT COUNT(*) FROM reports WHERE target = '{}'", mal2.id)),
        1
    );
    // Another address is someone else: nothing carried over.
    let (st, v) = s
        .call(
            "POST",
            "/v1/auth/password-signup/login",
            None,
            Some(signup("someone@example.org")),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    let other = s.with_game(&v).await;
    let (st, v) = s.post(&other, "/v1/queues/duel/tickets", json!({})).await;
    assert_eq!(st, 201, "{v}");
    s.running.stop();
}

/// M1, the server's part: the game declaration's `build` (the size and the modification time of the
/// locally compiled game would tell one installation from every other) is neither stored nor exported.
#[tokio::test]
async fn the_game_declarations_build_is_not_kept() {
    let s = start(base_config(&format!("{GENEROUS}{RELAY}"))).await;
    let p = s.player("builder").await;
    let game = json!({"contract": {"id": "lan-auto-match", "version": 1}, "build": "exe-48213504-1794346556",
                      "modes": ["turf"], "stages": [], "can_select_stage": false});
    let (st, v) = s.call("PUT", "/v1/me/game", Some(&p.token), Some(game)).await;
    assert_eq!(st, 204, "{v}");
    let (st, export) = s.call("GET", "/v1/me/export", Some(&p.token), None).await;
    assert_eq!(st, 200, "{export}");
    assert!(!export.to_string().contains("exe-48213504"), "{export}");
    assert_eq!(
        s.count("SELECT COUNT(*) FROM users WHERE game_caps LIKE '%exe-48213504%'"),
        0
    );
    s.running.stop();
}

/// M5. One IPv6 /64 (what any home or VPS connection gets) is one client for the rate limits, and one client cannot
/// fill the server's account cap: after its daily quota of new accounts it is refused, and a new player elsewhere
/// still gets in.
#[tokio::test]
async fn one_ipv6_prefix_is_one_client_and_cannot_exhaust_max_users() {
    let s = start(base_config(&format!(
        "{RELAY}\n[rate_limits]\nauth_per_ip_per_min = 5.0\n[limits]\nmax_users = 40\n"
    )))
    .await;
    let guest = || Some(json!({"display_name": "x"}));
    let mut codes = Vec::new();
    for i in 1..=8u32 {
        let (st, _) = s
            .call_from(
                Some(&format!("2001:db8:7:7::{i:x}")),
                "POST",
                "/v1/auth/guest/login",
                None,
                guest(),
            )
            .await;
        codes.push(st);
    }
    assert_eq!(
        codes,
        vec![200, 200, 200, 200, 200, 429, 429, 429],
        "one /64, one bucket"
    );
    s.running.stop();
    // With a generous login limit: the prefix's quota of new accounts.
    let s = start(base_config(&format!("{GENEROUS}{RELAY}\n[limits]\nmax_users = 40\n"))).await;
    let mut created = 0;
    let mut last = 0;
    for i in 2..200u32 {
        let (st, _) = s
            .call_from(
                Some(&format!("2001:db8:7:7::{i:x}")),
                "POST",
                "/v1/auth/guest/login",
                None,
                guest(),
            )
            .await;
        last = st;
        if st != 200 {
            break;
        }
        created += 1;
    }
    assert_eq!(last, 429, "the quota ends with 429 rate_limited");
    assert!(created < 40, "{created} accounts from one /64");
    let (st, v) = s
        .call_from(Some("198.51.100.20"), "POST", "/v1/auth/guest/login", None, guest())
        .await;
    assert_eq!(st, 200, "a new player elsewhere: {v}");
    s.running.stop();
}

/// M4, the service's part: any two accounts minted sealed room codes at will (a private room started and voided at
/// once, 90 in under a second). Starts are now limited per account.
#[tokio::test]
async fn room_starts_are_limited_per_account() {
    let s = start(base_config(&format!("{GENEROUS}{RELAY}"))).await;
    let (a, b) = (s.player("minter1").await, s.player("minter2").await);
    let (st, room) = s.post(&a, "/v1/rooms", json!({})).await;
    assert_eq!(st, 201, "{room}");
    let rid = room["id"].as_str().unwrap().to_string();
    let (st, v) = s.post(&b, "/v1/rooms/join", json!({"code": room["code"]})).await;
    assert_eq!(st, 200, "{v}");
    let mut started = 0;
    let mut refused = None;
    for _ in 0..90 {
        let (st, v) = s
            .post(&b, &format!("/v1/rooms/{rid}/ready"), json!({"ready": true}))
            .await;
        assert_eq!(st, 200, "{v}");
        let (st, m) = s.post(&a, &format!("/v1/rooms/{rid}/start"), json!({})).await;
        if st != 201 {
            refused = Some((st, m["error"]["code"].clone()));
            break;
        }
        started += 1;
        let mid = m["id"].as_str().unwrap().to_string();
        for p in [&a, &b] {
            let view = s.view(p, &mid).await;
            let (st, v) = s
                .post(
                    p,
                    &format!("/v1/matches/{mid}/result"),
                    error_report(&view, "launcher-failed"),
                )
                .await;
            assert_eq!(st, 200, "{v}");
        }
    }
    assert_eq!(refused, Some((429, json!("rate_limited"))), "after {started} starts");
    assert!(started <= 30, "{started}");
    s.running.stop();
}

/// M5: a guest account whose last session ended can never be logged in to again; it is erased, so that dead guest
/// accounts never fill `limits.max_users`. An account with a login of its own is never touched.
#[tokio::test]
async fn guest_accounts_without_a_session_left_are_erased() {
    let s = start(base_config(&format!("{GENEROUS}{RELAY}"))).await;
    let (st, v) = s
        .call(
            "POST",
            "/v1/auth/guest/login",
            None,
            Some(json!({"display_name": "Gone"})),
        )
        .await;
    assert_eq!(st, 200, "{v}");
    let member = s.player("stays").await;
    assert_eq!(s.count("SELECT COUNT(*) FROM users"), 2);
    // 35 days: every session (refresh tokens live 30 days) has ended; the hourly purge runs at each clock step.
    for _ in 0..5 {
        s.clock(7 * 86_400).await;
    }
    for _ in 0..50 {
        if s.count("SELECT COUNT(*) FROM users") == 1 {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(s.count("SELECT COUNT(*) FROM users"), 1);
    assert_eq!(
        s.count(&format!("SELECT COUNT(*) FROM users WHERE id = '{}'", member.id)),
        1
    );
    s.running.stop();
}
