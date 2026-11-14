//! Generic OAuth providers (`[[auth.oauth]]`) against a fake provider on 127.0.0.1 (no real provider is contacted):
//! a GitHub-like provider (client secret in the form, numeric id), a Twitch-like one without preset (nested id, a
//! client-id header), OpenID Connect discovery (and a document naming another issuer), provider errors that must not
//! leak, an oversized answer, a redirect that must not be followed, PKCE and replayed states.

mod common;

use std::sync::{Arc, Mutex};

use axum::Json;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use common::{BASE, start};
use serde_json::{Value, json};

#[derive(Clone, Default)]
struct Seen {
    token: Arc<Mutex<Vec<(String, String, String)>>>,
    userinfo_headers: Arc<Mutex<Vec<HeaderMap>>>,
    trap: Arc<Mutex<u32>>,
    base: Arc<Mutex<String>>,
}

fn header(h: &HeaderMap, k: &str) -> String {
    h.get(k).and_then(|v| v.to_str().ok()).unwrap_or_default().to_string()
}

async fn token(State(s): State<Seen>, h: HeaderMap, body: String) -> Json<Value> {
    s.token
        .lock()
        .unwrap()
        .push((header(&h, "authorization"), header(&h, "accept"), body.clone()));
    let kind = if body.contains("client_secret=gh-secret") {
        "gh"
    } else {
        "other"
    };
    Json(json!({"access_token": format!("tok-{kind}"), "token_type": "bearer", "scope": ""}))
}

async fn gh_user(h: HeaderMap) -> Response {
    if header(&h, "authorization") != "Bearer tok-gh" {
        return (StatusCode::UNAUTHORIZED, Json(json!({"message": "Bad credentials"}))).into_response();
    }
    Json(json!({"id": 583231, "login": "octocat", "name": "The Real Name"})).into_response()
}

async fn tw_users(State(s): State<Seen>, h: HeaderMap) -> Json<Value> {
    s.userinfo_headers.lock().unwrap().push(h);
    Json(json!({"data": [{"id": "141981764", "login": "streamy", "display_name": "Streamy"}]}))
}

async fn discovery(State(s): State<Seen>) -> Json<Value> {
    let b = s.base.lock().unwrap().clone();
    Json(
        json!({"issuer": format!("{b}/oidc"), "authorization_endpoint": format!("{b}/oidc/auth"),
                "token_endpoint": format!("{b}/oidc/token"), "userinfo_endpoint": format!("{b}/oidc/userinfo")}),
    )
}

async fn evil_discovery(State(s): State<Seen>) -> Json<Value> {
    let b = s.base.lock().unwrap().clone();
    Json(
        json!({"issuer": "https://elsewhere.example", "authorization_endpoint": format!("{b}/oidc/auth"),
                "token_endpoint": format!("{b}/oidc/token"), "userinfo_endpoint": format!("{b}/oidc/userinfo")}),
    )
}

async fn oidc_userinfo() -> Json<Value> {
    Json(json!({"sub": "f:3a8c-11:kc-user", "preferred_username": "kc-user"}))
}

async fn bad_token() -> Response {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({"error": "invalid_grant", "error_description": "code for client secret s3cr3t-value expired"})),
    )
        .into_response()
}

async fn big_user() -> String {
    format!("{{\"id\": \"1\", \"pad\": \"{}\"}}", "a".repeat(100_000))
}

async fn redirect() -> Response {
    (StatusCode::FOUND, [("location", "/trap")]).into_response()
}

async fn trap(State(s): State<Seen>) -> Json<Value> {
    *s.trap.lock().unwrap() += 1;
    Json(json!({"access_token": "trap", "token_type": "Bearer"}))
}

async fn mock() -> (String, Seen) {
    let seen = Seen::default();
    let app = axum::Router::new()
        .route("/gh/token", post(token))
        .route("/gh/user", get(gh_user))
        .route("/tw/token", post(token))
        .route("/tw/users", get(tw_users))
        .route("/oidc/.well-known/openid-configuration", get(discovery))
        .route("/evil/.well-known/openid-configuration", get(evil_discovery))
        .route("/oidc/token", post(token))
        .route("/oidc/userinfo", get(oidc_userinfo))
        .route("/bad/token", post(bad_token))
        .route("/big/token", post(token))
        .route("/big/user", get(big_user))
        .route("/redir/token", post(redirect))
        .route("/trap", post(trap).get(trap))
        .with_state(seen.clone());
    let l = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://127.0.0.1:{}", l.local_addr().unwrap().port());
    *seen.base.lock().unwrap() = base.clone();
    tokio::spawn(async move { axum::serve(l, app).await.unwrap() });
    (base, seen)
}

const VERIFIER: &str = "dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk";
const CHALLENGE: &str = "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM";
const REDIRECT: &str = "http://127.0.0.1:53682/callback";

fn config(base: &str) -> toml::Table {
    let gh = common::secret_file("gh-secret\n");
    let other = common::secret_file("other-secret");
    let mut doc: toml::Table = BASE.parse().unwrap();
    let extra: toml::Table = format!(
        r#"
[[auth.oauth]]
id = "gh"
preset = "github"
client_id = "gh-client"
client_secret_file = '{gh}'
authorize_url = "https://github.example/login/oauth/authorize"
token_url = "{base}/gh/token"
userinfo_url = "{base}/gh/user"

[[auth.oauth]]
id = "tw"
label = {{ en = "Log in with the stream site", fr = "Se connecter avec le site de streaming" }}
client_id = "tw-client"
client_secret_file = '{other}'
authorize_url = "https://id.stream.example/oauth2/authorize"
token_url = "{base}/tw/token"
userinfo_url = "{base}/tw/users"
subject = "data.0.id"
name = ["data.0.display_name"]
userinfo_client_id_header = "Client-Id"
upstream_pkce = false
scopes = ["openid"]

[[auth.oauth]]
id = "club"
preset = "oidc"
issuer = "{base}/oidc"
client_id = "club-client"
client_secret_file = '{other}'

[[auth.oauth]]
id = "evil"
preset = "oidc"
issuer = "{base}/evil"
client_id = "x"
client_secret_file = '{other}'

[[auth.oauth]]
id = "bad"
preset = "github"
client_id = "x"
client_secret_file = '{other}'
token_url = "{base}/bad/token"
userinfo_url = "{base}/gh/user"

[[auth.oauth]]
id = "big"
preset = "github"
client_id = "x"
client_secret_file = '{other}'
token_url = "{base}/big/token"
userinfo_url = "{base}/big/user"

[[auth.oauth]]
id = "redir"
preset = "github"
client_id = "x"
client_secret_file = '{other}'
token_url = "{base}/redir/token"
userinfo_url = "{base}/gh/user"

[[auth.oauth]]
id = "off"
enabled = false
preset = "google"
"#,
        gh = gh.display(),
        other = other.display()
    )
    .parse()
    .unwrap();
    let auth = doc["auth"].as_table_mut().unwrap();
    auth.insert("oauth".into(), extra["auth"]["oauth"].clone());
    doc
}

async fn begin(s: &common::Server, id: &str) -> (u16, Value) {
    let r = s
        .post(
            "198.51.100.7",
            &format!("/v1/auth/{id}/begin"),
            &json!({"redirect_uri": REDIRECT, "code_challenge": CHALLENGE, "code_challenge_method": "S256"}),
        )
        .await;
    (r.status, r.body)
}

async fn login(s: &common::Server, id: &str) -> (u16, Value) {
    let (st, b) = begin(s, id).await;
    if st != 200 {
        return (st, b);
    }
    let r = s
        .post(
            "198.51.100.7",
            &format!("/v1/auth/{id}/complete"),
            &json!({"state": b["state"], "code": "code-from-redirect", "code_verifier": VERIFIER}),
        )
        .await;
    (r.status, r.body)
}

#[tokio::test]
async fn generic_providers_log_in_and_fail_safely() {
    let (base, seen) = mock().await;
    let s = start(config(&base)).await;

    // The providers as launchers see them (disabled ones absent), each a loopback OAuth provider.
    let meta = s.meta().await;
    let ids: Vec<&str> = meta["auth"]["providers"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|p| p["kind"] == "oauth_loopback")
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    assert_eq!(ids, ["gh", "tw", "club", "evil", "bad", "big", "redir"]);
    let gh = meta["auth"]["providers"]
        .as_array()
        .unwrap()
        .iter()
        .find(|p| p["id"] == "gh")
        .unwrap()
        .clone();
    assert_eq!(gh["label"]["fr"], "Se connecter avec GitHub");
    assert!(gh["loopback_redirects"].as_array().is_some_and(|r| !r.is_empty()));
    let text = meta.to_string();
    assert!(
        !text.contains("gh-secret") && !text.contains("other-secret") && !text.contains("client_secret"),
        "no secret in the meta document"
    );

    // GitHub-like: secret in the form, JSON asked for, PKCE sent upstream, numeric id, login as the name.
    let (st, b) = begin(&s, "gh").await;
    assert_eq!(st, 200, "{b}");
    let url = b["authorize_url"].as_str().unwrap();
    assert!(url.starts_with("https://github.example/login/oauth/authorize?"));
    assert!(url.contains(CHALLENGE) && url.contains("client_id=gh-client") && !url.contains("scope="));
    let (st, session) = login(&s, "gh").await;
    assert_eq!(st, 200, "{session}");
    assert_eq!(session["created"], true);
    assert_eq!(session["me"]["display_name"], "octocat", "the login, not the real name");
    assert_eq!(session["me"]["logins"], json!(["gh"]));
    {
        let t = seen.token.lock().unwrap();
        let (auth, accept, body) = &t[0];
        assert!(auth.is_empty(), "client_secret_post: no Basic header");
        assert_eq!(accept, "application/json");
        for part in [
            "grant_type=authorization_code",
            "client_id=gh-client",
            "client_secret=gh-secret",
            &format!("code_verifier={VERIFIER}"),
        ] {
            assert!(body.contains(part), "{part} in {body}");
        }
    }
    let (_, again) = login(&s, "gh").await;
    assert_eq!(
        (again["created"].clone(), again["me"]["id"].clone()),
        (json!(false), session["me"]["id"].clone())
    );

    // No preset: nested id, the client id in a header, Basic authentication, no PKCE upstream.
    let (st, b) = begin(&s, "tw").await;
    assert_eq!(st, 200);
    assert!(!b["authorize_url"].as_str().unwrap().contains("code_challenge"));
    let (st, tw) = login(&s, "tw").await;
    assert_eq!(st, 200, "{tw}");
    assert_eq!(tw["me"]["display_name"], "Streamy");
    assert_eq!(
        header(&seen.userinfo_headers.lock().unwrap()[0], "client-id"),
        "tw-client"
    );
    {
        let t = seen.token.lock().unwrap();
        let (auth, _, body) = t.last().unwrap();
        assert!(auth.starts_with("Basic ") && !body.contains("client_secret") && !body.contains("code_verifier"));
    }

    // OpenID Connect discovery.
    let (st, b) = begin(&s, "club").await;
    assert_eq!(st, 200, "{b}");
    assert!(
        b["authorize_url"]
            .as_str()
            .unwrap()
            .starts_with(&format!("{base}/oidc/auth?"))
    );
    let (st, club) = login(&s, "club").await;
    assert_eq!(st, 200, "{club}");
    assert_eq!(club["me"]["display_name"], "kc-user");
    // A discovery document naming another issuer is refused.
    let (st, b) = begin(&s, "evil").await;
    assert_eq!((st, b["error"]["code"].as_str()), (502, Some("provider_error")), "{b}");

    // Provider errors: 502, the standard code at most, never the description or the secret.
    let (st, b) = login(&s, "bad").await;
    assert_eq!((st, b["error"]["code"].as_str()), (502, Some("provider_error")));
    let text = b.to_string();
    assert!(
        text.contains("invalid_grant") && !text.contains("s3cr3t") && !text.contains("expired"),
        "{text}"
    );
    let (st, b) = login(&s, "big").await;
    assert_eq!((st, b["error"]["code"].as_str()), (502, Some("provider_error")), "{b}");
    assert!(b["error"]["message"].as_str().unwrap().contains("too large"));
    let (st, b) = login(&s, "redir").await;
    assert_eq!((st, b["error"]["code"].as_str()), (502, Some("provider_error")), "{b}");
    assert_eq!(*seen.trap.lock().unwrap(), 0, "a redirect is never followed");

    // Nothing of the providers is stored: no token, no user id in clear.
    let core = s.running.state.lock();
    let dump: String = core
        .db
        .prepare("SELECT provider || hex(subject_hash) FROM identities")
        .unwrap()
        .query_map([], |r| r.get::<_, String>(0))
        .unwrap()
        .flatten()
        .collect();
    for leak in ["583231", "141981764", "kc-user", "tok-"] {
        assert!(!dump.contains(leak), "{leak}");
    }
    drop(core);
    s.running.stop();
}

#[tokio::test]
async fn pkce_and_states_are_checked_before_any_provider_call() {
    let (base, seen) = mock().await;
    let s = start(config(&base)).await;
    // A redirect outside the list, a challenge that is not S256.
    let r = s
        .post(
            "198.51.100.7",
            "/v1/auth/gh/begin",
            &json!({"redirect_uri": "http://127.0.0.1:9/evil", "code_challenge": CHALLENGE}),
        )
        .await;
    assert_eq!((r.status, r.code()), (422, Some("validation_failed")));
    let r = s
        .post(
            "198.51.100.7",
            "/v1/auth/gh/begin",
            &json!({"redirect_uri": REDIRECT, "code_challenge": "short", "code_challenge_method": "plain"}),
        )
        .await;
    assert_eq!((r.status, r.code()), (422, Some("validation_failed")));
    // A wrong verifier burns the state; a replay is refused; a state of another provider too.
    let (_, b) = begin(&s, "gh").await;
    let complete = |state: &Value, verifier: &str| json!({"state": state, "code": "c", "code_verifier": verifier});
    let r = s
        .post(
            "198.51.100.7",
            "/v1/auth/gh/complete",
            &complete(&b["state"], &"x".repeat(43)),
        )
        .await;
    assert_eq!((r.status, r.code()), (400, Some("pkce_mismatch")));
    let r = s
        .post("198.51.100.7", "/v1/auth/gh/complete", &complete(&b["state"], VERIFIER))
        .await;
    assert_eq!((r.status, r.code()), (400, Some("invalid_state")));
    let (_, b) = begin(&s, "tw").await;
    let r = s
        .post("198.51.100.7", "/v1/auth/gh/complete", &complete(&b["state"], VERIFIER))
        .await;
    assert_eq!(
        (r.status, r.code()),
        (400, Some("invalid_state")),
        "a state is for its provider"
    );
    let r = s
        .post(
            "198.51.100.7",
            "/v1/auth/gh/complete",
            &complete(&json!("st_unknown"), VERIFIER),
        )
        .await;
    assert_eq!((r.status, r.code()), (400, Some("invalid_state")));
    // Disabled and unknown providers.
    for id in ["off", "nope"] {
        let (st, _) = begin(&s, id).await;
        assert_eq!(st, 404);
    }
    assert!(seen.token.lock().unwrap().is_empty(), "no code was exchanged");
    s.running.stop();
}

#[test]
fn configuration_refuses_unsafe_providers() {
    let check = |extra: &str, production: bool| -> Vec<String> {
        let mut doc: toml::Table = BASE.parse().unwrap();
        let e: toml::Table = extra.parse().unwrap();
        doc["auth"]
            .as_table_mut()
            .unwrap()
            .insert("oauth".into(), e["auth"]["oauth"].clone());
        doc["server"]
            .as_table_mut()
            .unwrap()
            .insert("production".into(), production.into());
        let mut cfg = common::config(doc, 8080);
        if production {
            cfg.server.api_url = "https://play.example.org/api".into();
            cfg.features.test_clock = false;
        }
        cfg.normalize();
        cfg.validate()
    };
    let ok = r#"[[auth.oauth]]
id = "google"
preset = "google"
client_id = "c"
client_secret_env = "KALAMAR_GOOGLE_SECRET""#;
    assert!(check(ok, true).is_empty(), "{:?}", check(ok, true));
    for (bad, why) in [
        (
            "[[auth.oauth]]\nid = \"g\"\npreset = \"google\"\nclient_id = \"c\"",
            "client_secret",
        ),
        (
            "[[auth.oauth]]\nid = \"g\"\npreset = \"google\"\nclient_id = \"c\"\nclient_secret_env = \"X\"\ntoken_url = \"http://127.0.0.1:9/token\"",
            "token_url",
        ),
        (
            "[[auth.oauth]]\nid = \"g\"\npreset = \"google\"\nclient_id = \"c\"\nclient_secret_env = \"X\"\nredirect_uris = [\"https://play.example.org/cb\"]",
            "redirect_uris",
        ),
        (
            "[[auth.oauth]]\nid = \"guest\"\npreset = \"github\"\nclient_id = \"c\"\nclient_secret_env = \"X\"",
            "two login providers",
        ),
        (
            "[[auth.oauth]]\nid = \"g\"\nclient_id = \"c\"\nclient_secret_env = \"X\"\nauthorize_url = \"https://a.example/a\"\ntoken_url = \"https://a.example/t\"\nuserinfo_url = \"https://a.example/u\"",
            "subject",
        ),
        (
            "[[auth.oauth]]\nid = \"g\"\npreset = \"oidc\"\nclient_id = \"c\"\nclient_secret_env = \"X\"",
            "issuer",
        ),
    ] {
        let errs = check(bad, true);
        assert!(errs.iter().any(|e| e.contains(why)), "{why}: {errs:?}");
    }
    // Loopback http endpoints are for test servers only.
    let local = "[[auth.oauth]]\nid = \"g\"\npreset = \"github\"\nclient_id = \"c\"\nclient_secret_env = \"X\"\ntoken_url = \"http://127.0.0.1:9/token\"";
    assert!(check(local, false).is_empty(), "{:?}", check(local, false));
}
