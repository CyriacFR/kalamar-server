//! OAuth 2 authorization-code login with PKCE and a loopback redirect to the launcher (`api.md`, "OAuth loopback
//! providers"), for any provider: `[[auth.oauth]]` entries configured without code, with presets for Discord, Google,
//! GitHub, Twitch and OpenID Connect discovery (`issuer`). The older `[auth.discord]` section is the Discord preset
//! with its own defaults, unchanged.
//!
//! The launcher creates the PKCE pair; the server stores the challenge with the single-use `state`, verifies the
//! verifier itself at `complete` (so an intercepted code is useless without it even if the provider ignores PKCE),
//! then exchanges the code with its client secret and reads the provider's user id from the userinfo endpoint. The
//! provider's tokens never leave this module and are never stored; the user id is stored only as a keyed hash
//! (`login_identity`), and the public name, if configured, only as the initial display name.
//!
//! Provider calls: https only (plain http to loopback on a non-production server, for tests), no redirect followed,
//! 10 s per request, answers above 64 KiB refused while reading. Every provider failure is `502 provider_error`
//! with a generic message (at most the provider's standard OAuth error code).

use std::time::{Duration, Instant};

use axum::http::StatusCode;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::config::{OAuthConfig, OAuthEntry, Text};
use crate::error::{ApiError, ApiResult};
use crate::state::{AppState, Core, OAuthPending};
use crate::util::{new_token, rfc3339, sha256_b64url};

/// How long a `state` is valid.
pub const STATE_TTL_S: i64 = 600;
/// Most pending states kept (abuse bound).
const MAX_PENDING: usize = 10_000;
/// Security review M5: most pending states of one client network (so that one network cannot take them all).
pub const MAX_PENDING_PER_CLIENT: usize = 16;
/// Largest provider answer read.
pub const MAX_RESPONSE: usize = 64 * 1024;
/// How long OpenID Connect discovery results are kept.
const DISCOVERY_TTL: Duration = Duration::from_secs(24 * 3600);

/// Client authentication at the token endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenAuth {
    /// `client_secret_basic`: HTTP Basic.
    Basic,
    /// `client_secret_post`: in the form.
    Post,
}

/// Where the client secret comes from.
#[derive(Debug, Clone)]
pub enum SecretSource {
    Inline(String),
    File(std::path::PathBuf),
    Env(String),
}

/// A provider with its preset applied: everything the service needs, before the secret is read.
#[derive(Debug, Clone)]
pub struct Resolved {
    /// Where it is configured (`auth.discord` or `auth.oauth.<id>`), for messages.
    pub section: String,
    pub id: String,
    pub label: Text,
    pub client_id: String,
    pub secret: Vec<SecretSource>,
    pub redirect_uris: Vec<String>,
    pub issuer: Option<String>,
    pub authorize_url: Option<String>,
    pub token_url: Option<String>,
    pub userinfo_url: Option<String>,
    pub scopes: Vec<String>,
    pub upstream_pkce: bool,
    pub subject: String,
    pub name: Vec<String>,
    pub token_auth: TokenAuth,
    pub client_id_header: Option<String>,
    pub authorize_params: Vec<(String, String)>,
}

struct Preset {
    label: (&'static str, &'static str),
    issuer: Option<&'static str>,
    authorize: Option<&'static str>,
    token: Option<&'static str>,
    userinfo: Option<&'static str>,
    scopes: &'static [&'static str],
    pkce: bool,
    subject: &'static str,
    name: &'static [&'static str],
    token_auth: TokenAuth,
    client_id_header: Option<&'static str>,
    params: &'static [(&'static str, &'static str)],
}

/// The known providers. Untested against the real services (no credentials here): the endpoints are those the
/// providers document.
fn preset(name: &str) -> Option<Preset> {
    Some(match name {
        "discord" => Preset {
            label: ("Log in with Discord", "Se connecter avec Discord"),
            issuer: None,
            authorize: Some("https://discord.com/oauth2/authorize"),
            token: Some("https://discord.com/api/oauth2/token"),
            userinfo: Some("https://discord.com/api/users/@me"),
            scopes: &["identify"],
            pkce: true,
            subject: "id",
            name: &["global_name", "username"],
            token_auth: TokenAuth::Basic,
            client_id_header: None,
            params: &[("prompt", "consent")],
        },
        "google" => Preset {
            label: ("Log in with Google", "Se connecter avec Google"),
            issuer: None,
            authorize: Some("https://accounts.google.com/o/oauth2/v2/auth"),
            token: Some("https://oauth2.googleapis.com/token"),
            userinfo: Some("https://openidconnect.googleapis.com/v1/userinfo"),
            // `openid` alone gives the stable `sub`; add `profile` and `name = ["given_name"]` for a name.
            scopes: &["openid"],
            pkce: true,
            subject: "sub",
            name: &[],
            token_auth: TokenAuth::Basic,
            client_id_header: None,
            params: &[],
        },
        "github" => Preset {
            label: ("Log in with GitHub", "Se connecter avec GitHub"),
            issuer: None,
            authorize: Some("https://github.com/login/oauth/authorize"),
            token: Some("https://github.com/login/oauth/access_token"),
            userinfo: Some("https://api.github.com/user"),
            scopes: &[],
            pkce: true,
            subject: "id",
            name: &["login"],
            token_auth: TokenAuth::Post,
            client_id_header: None,
            params: &[],
        },
        "twitch" => Preset {
            label: ("Log in with Twitch", "Se connecter avec Twitch"),
            issuer: None,
            authorize: Some("https://id.twitch.tv/oauth2/authorize"),
            token: Some("https://id.twitch.tv/oauth2/token"),
            userinfo: Some("https://api.twitch.tv/helix/users"),
            scopes: &["openid"],
            pkce: false,
            subject: "data.0.id",
            name: &["data.0.display_name"],
            token_auth: TokenAuth::Post,
            client_id_header: Some("Client-Id"),
            params: &[],
        },
        "oidc" => Preset {
            label: ("Log in", "Se connecter"),
            issuer: None,
            authorize: None,
            token: None,
            userinfo: None,
            scopes: &["openid"],
            pkce: true,
            subject: "sub",
            name: &["preferred_username"],
            token_auth: TokenAuth::Basic,
            client_id_header: None,
            params: &[],
        },
        _ => return None,
    })
}

fn default_redirects() -> Vec<String> {
    vec![
        "http://127.0.0.1:53682/callback".into(),
        "http://127.0.0.1:53683/callback".into(),
    ]
}

/// Applies an entry's preset; `Err` for an unknown preset or a missing user-id path.
pub fn resolve_entry(e: &OAuthEntry) -> Result<Resolved, String> {
    let n = format!("auth.oauth.{}", e.id);
    let p = match e.preset.as_deref() {
        Some(name) => Some(
            preset(name)
                .ok_or_else(|| format!("{n}.preset: `{name}` is not one of discord, google, github, twitch, oidc"))?,
        ),
        None => None,
    };
    let subject = e
        .subject
        .clone()
        .or_else(|| p.as_ref().map(|p| p.subject.to_string()))
        .ok_or_else(|| format!("{n}.subject: the path of the user id is required without a preset"))?;
    let token_auth = match e.token_auth.as_deref() {
        Some("basic") => TokenAuth::Basic,
        Some("post") => TokenAuth::Post,
        Some(other) => return Err(format!("{n}.token_auth: `{other}` (basic or post)")),
        None => p.as_ref().map(|p| p.token_auth).unwrap_or(TokenAuth::Basic),
    };
    let label = if e.label.is_empty() {
        match &p {
            Some(p) => Text::from([
                ("en".to_string(), p.label.0.to_string()),
                ("fr".to_string(), p.label.1.to_string()),
            ]),
            None => Text::from([("en".to_string(), format!("Log in with {}", e.id))]),
        }
    } else {
        e.label.clone()
    };
    let mut secret = Vec::new();
    if let Some(f) = &e.client_secret_file {
        secret.push(SecretSource::File(f.clone()));
    }
    if let Some(v) = &e.client_secret_env {
        secret.push(SecretSource::Env(v.clone()));
    }
    let pick = |explicit: &Option<String>, from: Option<&'static str>| explicit.clone().or(from.map(str::to_string));
    Ok(Resolved {
        section: n.clone(),
        id: e.id.clone(),
        label,
        client_id: e.client_id.clone(),
        secret,
        redirect_uris: if e.redirect_uris.is_empty() {
            default_redirects()
        } else {
            e.redirect_uris.clone()
        },
        issuer: pick(&e.issuer, p.as_ref().and_then(|p| p.issuer)),
        authorize_url: pick(&e.authorize_url, p.as_ref().and_then(|p| p.authorize)),
        token_url: pick(&e.token_url, p.as_ref().and_then(|p| p.token)),
        userinfo_url: pick(&e.userinfo_url, p.as_ref().and_then(|p| p.userinfo)),
        scopes: e.scopes.clone().unwrap_or_else(|| {
            p.as_ref()
                .map(|p| p.scopes.iter().map(|s| s.to_string()).collect())
                .unwrap_or_default()
        }),
        upstream_pkce: e.upstream_pkce.unwrap_or(p.as_ref().is_none_or(|p| p.pkce)),
        subject,
        name: e.name.clone().unwrap_or_else(|| {
            p.as_ref()
                .map(|p| p.name.iter().map(|s| s.to_string()).collect())
                .unwrap_or_default()
        }),
        token_auth,
        client_id_header: e
            .userinfo_client_id_header
            .clone()
            .or(p.as_ref().and_then(|p| p.client_id_header.map(str::to_string))),
        authorize_params: match &e.authorize_params {
            Some(m) => m.iter().map(|(k, v)| (k.clone(), v.clone())).collect(),
            None => p
                .as_ref()
                .map(|p| p.params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect())
                .unwrap_or_default(),
        },
    })
}

/// The `[auth.discord]` section as the Discord preset, with exactly its former behavior (`user_url`, the
/// `use_provider_name` switch, the inline secret and `KALAMAR_DISCORD_CLIENT_SECRET`).
pub fn resolve_discord(d: &OAuthConfig) -> Resolved {
    let p = preset("discord").expect("the discord preset exists");
    let mut secret = Vec::new();
    if let Some(s) = &d.client_secret {
        secret.push(SecretSource::Inline(s.clone()));
    }
    if let Some(f) = &d.client_secret_file {
        secret.push(SecretSource::File(f.clone()));
    }
    secret.push(SecretSource::Env("KALAMAR_DISCORD_CLIENT_SECRET".into()));
    Resolved {
        section: "auth.discord".into(),
        id: d.id.clone(),
        label: d.label.clone(),
        client_id: d.client_id.clone(),
        secret,
        redirect_uris: d.redirect_uris.clone(),
        issuer: None,
        authorize_url: Some(d.authorize_url.clone()),
        token_url: Some(d.token_url.clone()),
        userinfo_url: Some(d.user_url.clone()),
        scopes: d.scopes.clone(),
        upstream_pkce: d.upstream_pkce,
        subject: p.subject.into(),
        name: if d.use_provider_name {
            p.name.iter().map(|s| s.to_string()).collect()
        } else {
            Vec::new()
        },
        token_auth: p.token_auth,
        client_id_header: None,
        authorize_params: p.params.iter().map(|(k, v)| (k.to_string(), v.to_string())).collect(),
    }
}

/// Every enabled OAuth provider of the configuration (the legacy Discord section first), or the reasons they cannot
/// be built.
pub fn configured(cfg: &crate::config::Config) -> Vec<Result<Resolved, String>> {
    let mut v = Vec::new();
    if let Some(d) = cfg.auth.discord.as_ref().filter(|d| d.enabled) {
        v.push(Ok(resolve_discord(d)));
    }
    for e in cfg.auth.oauth.iter().filter(|e| e.enabled) {
        v.push(resolve_entry(e));
    }
    v
}

/// A provider endpoint the service may call or send a browser to: `https://` with a host, no credentials and no
/// fragment; `http://` only to 127.0.0.1, `[::1]` or `localhost` on a non-production server (tests).
pub fn endpoint_ok(u: &str, production: bool) -> bool {
    let Ok(url) = reqwest::Url::parse(u) else {
        return false;
    };
    let clean = url.username().is_empty() && url.password().is_none() && url.fragment().is_none() && u.len() <= 500;
    let loopback = matches!(url.host_str(), Some("127.0.0.1") | Some("[::1]") | Some("localhost"));
    clean
        && url.host_str().is_some_and(|h| !h.is_empty())
        && match url.scheme() {
            "https" => true,
            "http" => loopback && !production,
            _ => false,
        }
}

/// A path of the userinfo JSON: dot-separated keys, array indices as numbers (`data.0.id`).
pub fn path_ok(p: &str) -> bool {
    !p.is_empty() && p.len() <= 200 && p.split('.').count() <= 8 && p.split('.').all(|s| !s.is_empty())
}

/// Every problem of a resolved provider (empty when it can run).
pub fn check(r: &Resolved, production: bool) -> Vec<String> {
    let n = r.section.clone();
    let mut e = Vec::new();
    if !crate::util::is_id(&r.id) {
        e.push(format!("{n}: the id is 1 to 64 of A-Z a-z 0-9 _ -"));
    }
    if r.label.is_empty() || r.label.values().any(|l| l.trim().is_empty() || l.chars().count() > 64) {
        e.push(format!("{n}.label: 1 to 64 characters per language"));
    }
    if r.client_id.is_empty() || r.client_id.len() > 256 {
        e.push(format!("{n}.client_id is required"));
    }
    if r.secret.is_empty() {
        e.push(format!(
            "{n}: set client_secret_file or client_secret_env (never the secret in this file)"
        ));
    }
    if r.redirect_uris.is_empty() || r.redirect_uris.iter().any(|u| !crate::auth::is_loopback_redirect(u)) {
        e.push(format!(
            "{n}.redirect_uris: loopback http URIs (http://127.0.0.1:<port>/<path>)"
        ));
    }
    match &r.issuer {
        Some(i) if !endpoint_ok(i, production) || reqwest::Url::parse(i).is_ok_and(|u| u.query().is_some()) => e.push(
            format!("{n}.issuer: an https URL without query (http only to loopback off production)"),
        ),
        None if r.authorize_url.is_none() || r.token_url.is_none() || r.userinfo_url.is_none() => e.push(format!(
            "{n}: set issuer (OpenID Connect discovery) or authorize_url, token_url and userinfo_url"
        )),
        _ => {}
    }
    for (k, u) in [
        ("authorize_url", &r.authorize_url),
        ("token_url", &r.token_url),
        ("userinfo_url", &r.userinfo_url),
    ] {
        if let Some(u) = u
            && !endpoint_ok(u, production)
        {
            e.push(format!(
                "{n}.{k}: an https URL (http only to 127.0.0.1 or localhost when production = false)"
            ));
        }
    }
    if !path_ok(&r.subject) || r.name.iter().any(|p| !path_ok(p)) {
        e.push(format!(
            "{n}.subject / name: dot-separated paths such as sub, id or data.0.id"
        ));
    }
    if r.scopes
        .iter()
        .any(|s| s.is_empty() || s.contains(' ') || s.len() > 200)
    {
        e.push(format!("{n}.scopes: scope names without spaces"));
    }
    if let Some(h) = &r.client_id_header
        && (h.is_empty() || !h.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-'))
    {
        e.push(format!("{n}.userinfo_client_id_header: a header name"));
    }
    for (k, _) in &r.authorize_params {
        if matches!(
            k.as_str(),
            "response_type"
                | "client_id"
                | "state"
                | "redirect_uri"
                | "code_challenge"
                | "code_challenge_method"
                | "scope"
        ) {
            e.push(format!("{n}.authorize_params: `{k}` is set by the service"));
        }
    }
    e
}

/// The launcher's view of a provider and the provider calls of one login.
pub struct OAuthProvider {
    r: Resolved,
    secret: String,
    http: reqwest::Client,
    production: bool,
    discovered: tokio::sync::Mutex<Option<(Endpoints, Instant)>>,
}

#[derive(Clone, Debug)]
struct Endpoints {
    authorize: String,
    token: String,
    userinfo: String,
}

pub struct Identity {
    pub subject: String,
    pub name: Option<String>,
}

#[derive(Deserialize)]
pub struct BeginBody {
    pub redirect_uri: String,
    pub code_challenge: String,
    #[serde(default = "s256")]
    pub code_challenge_method: String,
}

fn s256() -> String {
    "S256".into()
}

#[derive(Deserialize)]
pub struct CompleteBody {
    pub state: String,
    pub code: String,
    pub code_verifier: String,
}

/// RFC 7636: 43 to 128 characters from `A-Z a-z 0-9 - . _ ~`.
pub fn is_valid_verifier(s: &str) -> bool {
    (43..=128).contains(&s.len()) && s.bytes().all(|b| b.is_ascii_alphanumeric() || b"-._~".contains(&b))
}

/// An S256 challenge: 43 base64url characters.
pub fn is_valid_challenge(s: &str) -> bool {
    s.len() == 43 && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Installs the process-wide TLS crypto provider (ring) once; later calls do nothing.
pub fn install_crypto() {
    static INSTALL: std::sync::Once = std::sync::Once::new();
    INSTALL.call_once(|| {
        let _ = rustls::crypto::ring::default_provider().install_default();
    });
}

/// The value at a dot-separated path (`data.0.id`).
pub fn at_path<'a>(v: &'a Value, path: &str) -> Option<&'a Value> {
    path.split('.').try_fold(v, |cur, seg| match cur {
        Value::Object(m) => m.get(seg),
        Value::Array(a) => seg.parse::<usize>().ok().and_then(|i| a.get(i)),
        _ => None,
    })
}

fn provider_error(m: &str) -> ApiError {
    ApiError::new(StatusCode::BAD_GATEWAY, "provider_error", m)
}

/// Reads at most [`MAX_RESPONSE`] bytes of an answer, as JSON; any failure is a `provider_error`.
async fn read_json(mut resp: reqwest::Response, what: &str) -> ApiResult<Value> {
    let status = resp.status();
    if resp.content_length().is_some_and(|n| n > MAX_RESPONSE as u64) {
        return Err(provider_error(&format!("the provider's {what} answer is too large")));
    }
    let mut buf: Vec<u8> = Vec::new();
    loop {
        match resp.chunk().await {
            Ok(Some(c)) => {
                if buf.len() + c.len() > MAX_RESPONSE {
                    return Err(provider_error(&format!("the provider's {what} answer is too large")));
                }
                buf.extend_from_slice(&c);
            }
            Ok(None) => break,
            Err(_) => {
                return Err(provider_error(&format!(
                    "the provider's {what} answer could not be read"
                )));
            }
        }
    }
    if status.is_redirection() {
        return Err(provider_error(&format!(
            "the provider redirected the {what} request (redirects are not followed)"
        )));
    }
    let json = serde_json::from_slice::<Value>(&buf);
    if !status.is_success() {
        // Only the standard OAuth error code is repeated (never a description or a body).
        let code = json
            .as_ref()
            .ok()
            .and_then(|v| v.get("error"))
            .and_then(Value::as_str)
            .filter(|c| (1..=40).contains(&c.len()) && c.bytes().all(|b| b.is_ascii_lowercase() || b == b'_'));
        return Err(provider_error(&match code {
            Some(c) => format!("the provider refused the {what} request ({c})"),
            None => format!("the provider answered {} to the {what} request", status.as_u16()),
        }));
    }
    json.map_err(|_| provider_error(&format!("the provider's {what} answer is not JSON")))
}

impl OAuthProvider {
    /// Reads the client secret from its first configured source and builds the HTTP client.
    pub fn new(r: Resolved, production: bool) -> Result<OAuthProvider, String> {
        let what = format!("{}: client secret", r.section);
        let secret = match r.secret.first() {
            Some(SecretSource::Inline(v)) if !v.is_empty() => v.clone(),
            Some(SecretSource::Inline(_)) => return Err(format!("{what}: empty")),
            Some(SecretSource::File(f)) => crate::config::read_secret(Some(f), None, &what)?,
            Some(SecretSource::Env(n)) => crate::config::read_secret(None, Some(n), &what)?,
            None => return Err(format!("{what}: set client_secret_file or client_secret_env")),
        };
        install_crypto();
        let http = reqwest::Client::builder()
            .timeout(Duration::from_secs(10))
            .connect_timeout(Duration::from_secs(5))
            .redirect(reqwest::redirect::Policy::none())
            .user_agent(concat!("kalamar-service/", env!("CARGO_PKG_VERSION")))
            .build()
            .map_err(|e| format!("HTTP client: {e}"))?;
        Ok(OAuthProvider {
            r,
            secret,
            http,
            production,
            discovered: tokio::sync::Mutex::new(None),
        })
    }

    pub fn id(&self) -> &str {
        &self.r.id
    }

    pub fn info(&self) -> Value {
        json!({"id": self.r.id, "kind": "oauth_loopback", "label": self.r.label,
               "loopback_redirects": self.r.redirect_uris})
    }

    /// The endpoints: configured ones, the rest from OpenID Connect discovery (cached for a day).
    async fn endpoints(&self) -> ApiResult<Endpoints> {
        if let (Some(a), Some(t), Some(u)) = (&self.r.authorize_url, &self.r.token_url, &self.r.userinfo_url) {
            return Ok(Endpoints {
                authorize: a.clone(),
                token: t.clone(),
                userinfo: u.clone(),
            });
        }
        let issuer = self
            .r
            .issuer
            .as_deref()
            .ok_or_else(|| ApiError::internal("no issuer"))?
            .trim_end_matches('/')
            .to_string();
        let mut cache = self.discovered.lock().await;
        if let Some((ep, at)) = cache.as_ref()
            && at.elapsed() < DISCOVERY_TTL
        {
            return Ok(ep.clone());
        }
        let resp = self
            .http
            .get(format!("{issuer}/.well-known/openid-configuration"))
            .header(reqwest::header::ACCEPT, "application/json")
            .send()
            .await
            .map_err(|_| provider_error("the provider could not be reached (discovery)"))?;
        let doc = read_json(resp, "discovery").await?;
        let declared = doc.get("issuer").and_then(Value::as_str).unwrap_or_default();
        if declared.trim_end_matches('/') != issuer {
            tracing::warn!(provider = %self.r.id, "OpenID discovery: the document names another issuer");
            return Err(provider_error("the provider's discovery document names another issuer"));
        }
        let get = |explicit: &Option<String>, key: &str| -> ApiResult<String> {
            let u = explicit
                .clone()
                .or_else(|| doc.get(key).and_then(Value::as_str).map(str::to_string))
                .ok_or_else(|| provider_error(&format!("the provider's discovery document has no {key}")))?;
            if endpoint_ok(&u, self.production) {
                Ok(u)
            } else {
                Err(provider_error(&format!("the provider's {key} is not an https URL")))
            }
        };
        let ep = Endpoints {
            authorize: get(&self.r.authorize_url, "authorization_endpoint")?,
            token: get(&self.r.token_url, "token_endpoint")?,
            userinfo: get(&self.r.userinfo_url, "userinfo_endpoint")?,
        };
        *cache = Some((ep.clone(), Instant::now()));
        Ok(ep)
    }

    /// The provider's authorization URL for a state and challenge.
    pub fn authorize_url(
        &self,
        authorize: &str,
        state: &str,
        challenge: &str,
        redirect_uri: &str,
    ) -> Result<String, String> {
        let mut params = vec![
            ("response_type".to_string(), "code".to_string()),
            ("client_id".to_string(), self.r.client_id.clone()),
        ];
        if !self.r.scopes.is_empty() {
            params.push(("scope".into(), self.r.scopes.join(" ")));
        }
        params.push(("state".into(), state.to_string()));
        params.push(("redirect_uri".into(), redirect_uri.to_string()));
        params.extend(self.r.authorize_params.iter().cloned());
        if self.r.upstream_pkce {
            params.push(("code_challenge".into(), challenge.to_string()));
            params.push(("code_challenge_method".into(), "S256".to_string()));
        }
        reqwest::Url::parse_with_params(authorize, &params)
            .map(|u| u.to_string())
            .map_err(|e| e.to_string())
    }

    /// `POST /v1/auth/{provider}/begin`: checks the request, resolves the endpoints, stores the state.
    pub async fn begin(&self, st: &AppState, client: std::net::IpAddr, body: BeginBody) -> ApiResult<Value> {
        if !self.r.redirect_uris.contains(&body.redirect_uri) {
            return Err(ApiError::invalid("/redirect_uri", "not one of loopback_redirects"));
        }
        if body.code_challenge_method != "S256" || !is_valid_challenge(&body.code_challenge) {
            return Err(ApiError::invalid(
                "/code_challenge",
                "an S256 challenge (43 base64url characters)",
            ));
        }
        let ep = self.endpoints().await?;
        let state = new_token("st").replace('.', "_");
        let url = self
            .authorize_url(&ep.authorize, &state, &body.code_challenge, &body.redirect_uri)
            .map_err(ApiError::internal)?;
        let mut core = st.lock();
        let now = core.now;
        let live = &mut core.live.oauth_states;
        live.retain(|_, p| p.expires > now);
        if live.values().filter(|p| p.client == client).count() >= MAX_PENDING_PER_CLIENT {
            return Err(ApiError::rate_limited(STATE_TTL_S));
        }
        if live.len() >= MAX_PENDING {
            return Err(ApiError::new(
                StatusCode::SERVICE_UNAVAILABLE,
                "unavailable",
                "too many logins in progress",
            ));
        }
        live.insert(
            state.clone(),
            OAuthPending {
                provider: self.r.id.clone(),
                client,
                challenge: body.code_challenge,
                redirect_uri: body.redirect_uri,
                expires: now + STATE_TTL_S,
            },
        );
        Ok(json!({"authorize_url": url, "state": state, "expires_at": rfc3339(now + STATE_TTL_S)}))
    }

    /// Consumes the state (single use, whatever follows) and checks the verifier against its challenge.
    pub fn take_state(&self, core: &mut Core, body: &CompleteBody) -> ApiResult<OAuthPending> {
        let pending = core
            .live
            .oauth_states
            .remove(&body.state)
            .filter(|p| p.expires > core.now && p.provider == self.r.id)
            .ok_or_else(|| ApiError::bad_request("invalid_state", "unknown, expired or used state"))?;
        if !is_valid_verifier(&body.code_verifier) || sha256_b64url(body.code_verifier.as_bytes()) != pending.challenge
        {
            return Err(ApiError::bad_request(
                "pkce_mismatch",
                "the verifier does not match the challenge",
            ));
        }
        if body.code.is_empty() || body.code.len() > 512 {
            return Err(ApiError::invalid("/code", "1 to 512 characters"));
        }
        Ok(pending)
    }

    /// Exchanges the code and reads the provider's user.
    pub async fn exchange(&self, code: &str, verifier: &str, redirect_uri: &str) -> ApiResult<Identity> {
        let ep = self.endpoints().await?;
        let mut form = vec![
            ("grant_type", "authorization_code"),
            ("code", code),
            ("redirect_uri", redirect_uri),
        ];
        if self.r.upstream_pkce {
            form.push(("code_verifier", verifier));
        }
        let mut rb = self
            .http
            .post(&ep.token)
            .header(reqwest::header::ACCEPT, "application/json");
        match self.r.token_auth {
            TokenAuth::Basic => rb = rb.basic_auth(&self.r.client_id, Some(&self.secret)),
            TokenAuth::Post => {
                form.push(("client_id", &self.r.client_id));
                form.push(("client_secret", &self.secret));
            }
        }
        let resp = rb.form(&form).send().await.map_err(|_| {
            tracing::warn!(provider = %self.r.id, "token endpoint unreachable");
            provider_error("the provider could not be reached")
        })?;
        let token = read_json(resp, "token").await.inspect_err(|e| {
            tracing::warn!(provider = %self.r.id, reason = %e.message, "token exchange failed");
        })?;
        let access = token
            .get("access_token")
            .and_then(Value::as_str)
            .filter(|t| !t.is_empty() && t.len() <= 4096)
            .ok_or_else(|| provider_error("no access token in the provider's answer"))?;
        if let Some(t) = token.get("token_type").and_then(Value::as_str)
            && !t.eq_ignore_ascii_case("bearer")
        {
            return Err(provider_error("the provider's token is not a bearer token"));
        }
        let mut rb = self
            .http
            .get(&ep.userinfo)
            .bearer_auth(access)
            .header(reqwest::header::ACCEPT, "application/json");
        if let Some(h) = &self.r.client_id_header {
            rb = rb.header(h.as_str(), &self.r.client_id);
        }
        let resp = rb
            .send()
            .await
            .map_err(|_| provider_error("the provider could not be reached"))?;
        let user = read_json(resp, "userinfo").await.inspect_err(|e| {
            tracing::warn!(provider = %self.r.id, reason = %e.message, "userinfo failed");
        })?;
        let subject = match at_path(&user, &self.r.subject) {
            Some(Value::String(s)) if !s.is_empty() && s.len() <= 255 && !s.chars().any(char::is_control) => s.clone(),
            Some(Value::Number(n)) if n.is_u64() || n.is_i64() => n.to_string(),
            _ => return Err(provider_error("no user id in the provider's answer")),
        };
        let name = self
            .r
            .name
            .iter()
            .find_map(|p| {
                at_path(&user, p)
                    .and_then(Value::as_str)
                    .filter(|s| !s.trim().is_empty())
            })
            .map(|s| s.chars().take(64).collect());
        Ok(Identity { subject, name })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    fn discord() -> OAuthProvider {
        let cfg = OAuthConfig {
            enabled: true,
            client_id: "client123".into(),
            client_secret: Some("s3cret".into()),
            ..OAuthConfig::default()
        };
        OAuthProvider::new(resolve_discord(&cfg), true).unwrap()
    }

    #[test]
    fn authorize_url_carries_state_challenge_and_redirect() {
        let p = discord();
        let u = p
            .authorize_url(
                p.r.authorize_url.as_deref().unwrap(),
                "st_abc",
                "E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
                "http://127.0.0.1:53682/callback",
            )
            .unwrap();
        assert!(u.starts_with("https://discord.com/oauth2/authorize?"));
        for part in [
            "response_type=code",
            "client_id=client123",
            "state=st_abc",
            "code_challenge_method=S256",
            "redirect_uri=http%3A%2F%2F127.0.0.1%3A53682%2Fcallback",
            "scope=identify",
            "prompt=consent",
            "code_challenge=E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM",
        ] {
            assert!(u.contains(part), "{part} in {u}");
        }
        assert_eq!(p.info()["kind"], "oauth_loopback");
        assert_eq!(p.info()["label"], json!({"en": "Log in with Discord"}));
    }

    #[test]
    fn pkce_shapes() {
        assert!(is_valid_verifier("dBjftJeZ4CVP-mB92K27uhbUJU1p1r_wW1gFWFOEjXk"));
        assert!(!is_valid_verifier("short"));
        assert!(!is_valid_verifier(&"a".repeat(129)));
        assert!(!is_valid_verifier(&format!("{}!", "a".repeat(50))));
        assert!(is_valid_challenge("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-cM"));
        assert!(!is_valid_challenge("E9Melhoa2OwvFrEMTJguCHaoeK1t8URWbuGJSstw-c="));
        assert!(crate::auth::is_loopback_redirect("http://127.0.0.1:53682/callback"));
        assert!(crate::auth::is_loopback_redirect("http://[::1]:8000/"));
        assert!(!crate::auth::is_loopback_redirect("http://localhost:53682/callback"));
        assert!(!crate::auth::is_loopback_redirect("https://127.0.0.1:53682/callback"));
        assert!(!crate::auth::is_loopback_redirect("http://127.0.0.1:0/x"));
        assert!(!crate::auth::is_loopback_redirect("http://127.0.0.1:99999/x"));
    }

    fn entry(preset: Option<&str>) -> OAuthEntry {
        OAuthEntry {
            enabled: true,
            id: "sso".into(),
            preset: preset.map(str::to_string),
            client_id: "c".into(),
            client_secret_env: Some("X".into()),
            ..OAuthEntry::default()
        }
    }

    #[test]
    fn presets_fill_what_is_not_given() {
        let g = resolve_entry(&entry(Some("github"))).unwrap();
        assert_eq!(g.token_auth, TokenAuth::Post);
        assert_eq!(g.subject, "id");
        assert!(g.scopes.is_empty());
        assert!(check(&g, true).is_empty(), "{:?}", check(&g, true));
        let t = resolve_entry(&entry(Some("twitch"))).unwrap();
        assert_eq!(t.client_id_header.as_deref(), Some("Client-Id"));
        assert_eq!(t.subject, "data.0.id");
        assert!(!t.upstream_pkce);
        let mut o = entry(Some("oidc"));
        assert!(
            !check(&resolve_entry(&o).unwrap(), true).is_empty(),
            "oidc needs an issuer"
        );
        o.issuer = Some("https://sso.example.org/realms/play".into());
        o.label = Text::from([("en".to_string(), "Club login".to_string())]);
        let r = resolve_entry(&o).unwrap();
        assert!(check(&r, true).is_empty(), "{:?}", check(&r, true));
        assert_eq!(r.label["en"], "Club login");
        // No preset: the user-id path is required.
        assert!(resolve_entry(&entry(None)).is_err());
        assert!(resolve_entry(&entry(Some("myspace"))).is_err());
    }

    #[test]
    fn endpoints_must_be_https() {
        assert!(endpoint_ok("https://id.example.org/token", true));
        assert!(!endpoint_ok("http://id.example.org/token", false));
        assert!(
            !endpoint_ok("http://127.0.0.1:8000/token", true),
            "loopback http only off production"
        );
        assert!(endpoint_ok("http://127.0.0.1:8000/token", false));
        assert!(!endpoint_ok("https://user:pw@id.example.org/token", true));
        assert!(!endpoint_ok("https://id.example.org/token#x", true));
        assert!(!endpoint_ok("ftp://id.example.org/", true));
        let mut e = entry(Some("github"));
        e.token_url = Some("http://github.example/token".into());
        assert!(
            check(&resolve_entry(&e).unwrap(), false)
                .iter()
                .any(|m| m.contains("token_url"))
        );
        e.token_url = None;
        e.authorize_params = Some(BTreeMap::from([("state".to_string(), "x".to_string())]));
        assert!(!check(&resolve_entry(&e).unwrap(), false).is_empty());
    }

    #[test]
    fn paths_reach_nested_ids() {
        let v = json!({"data": [{"id": "42", "display_name": "Cam"}], "sub": 7});
        assert_eq!(at_path(&v, "data.0.id"), Some(&json!("42")));
        assert_eq!(at_path(&v, "sub"), Some(&json!(7)));
        assert_eq!(at_path(&v, "data.1.id"), None);
        assert_eq!(at_path(&v, "data.x"), None);
        assert!(path_ok("data.0.id") && !path_ok("a..b") && !path_ok(""));
    }
}
