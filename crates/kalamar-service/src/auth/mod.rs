//! Authentication (`api.md`, "Authentication"): login providers behind one interface, sessions with hashed access and
//! refresh tokens, refresh rotation with a 60 s reuse window, and the request extractors.
//!
//! Providers: `dev` (username-only accounts for local testing, refused in production), `guest` (an account with only
//! a display name), OAuth 2 loopback providers (`[auth.discord]` and `[[auth.oauth]]`, in [`oauth`]) and e-mail and
//! password accounts (`[auth.password]`, in [`password`], mails in [`mail`]).

pub mod mail;
pub mod oauth;
pub mod password;

use std::time::Instant;

use axum::Json;
use axum::extract::{FromRequestParts, Path, State};
use axum::http::request::Parts;
use axum::http::{HeaderMap, StatusCode};
use rusqlite::{OptionalExtension, params};
use serde::Deserialize;
use serde_json::{Value, json};

use crate::error::{ApiError, ApiResult, JsonBody};
use crate::state::{AppState, Core};
use crate::util::{identity_hash, new_code, new_id, new_token, rfc3339, token_hash};

/// The authenticated caller.
#[derive(Debug, Clone)]
pub struct Authed {
    pub user: String,
    pub session: String,
}

/// The authenticated caller, banned or not (`GET /v1/me`, export, erase, logout).
pub struct AuthedAny(pub Authed);

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let v = headers.get(axum::http::header::AUTHORIZATION)?.to_str().ok()?;
    let (scheme, token) = v.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty() && token.len() <= 512).then_some(token.trim())
}

/// Looks an access token up: the caller, or `401`.
pub fn check_access(core: &mut Core, token: &str) -> ApiResult<Authed> {
    let hash = token_hash(token);
    let row: Option<(String, String, i64, i64)> = core
        .db
        .query_row(
            "SELECT id, user_id, access_expires, last_used_at FROM sessions WHERE access_hash = ?1",
            [&hash],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .optional()?;
    let Some((session, user, expires, last_used)) = row else {
        return Err(ApiError::unauthorized("unauthorized", "unknown or revoked token"));
    };
    if expires <= core.now {
        return Err(ApiError::unauthorized(
            "token_expired",
            "the access token expired: refresh it",
        ));
    }
    if core.now - last_used >= 60 {
        core.db.execute(
            "UPDATE sessions SET last_used_at = ?1 WHERE id = ?2",
            params![core.now, session],
        )?;
    }
    Ok(Authed { user, session })
}

async fn authenticate(parts: &Parts, st: &AppState) -> ApiResult<(Authed, Option<(i64, Option<String>)>)> {
    let token = bearer(&parts.headers).ok_or_else(|| ApiError::unauthorized("unauthorized", "missing bearer token"))?;
    let (a, ban) = {
        let mut core = st.lock();
        let a = check_access(&mut core, token)?;
        let ban = core.ban_of(&a.user);
        // In memory only: the `online` count of `GET /v1/population`.
        let now = core.now;
        core.live.last_seen.insert(a.user.clone(), now);
        (a, ban)
    };
    if let Err(retry) = st.limits.per_user.take(&a.user, Instant::now()) {
        return Err(ApiError::rate_limited(retry));
    }
    Ok((a, ban))
}

pub fn banned_error(until: i64, reason: Option<String>, now: i64) -> ApiError {
    let mut e = ApiError::forbidden(
        "banned",
        format!(
            "this account is banned{}",
            reason.map(|r| format!(": {r}")).unwrap_or_default()
        ),
    );
    if until != i64::MAX {
        e = e.retry_after(until - now);
    }
    e
}

impl FromRequestParts<AppState> for Authed {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> ApiResult<Self> {
        let (a, ban) = authenticate(parts, st).await?;
        match ban {
            Some((until, reason)) => Err(banned_error(until, reason, st.clock.now())),
            None => Ok(a),
        }
    }
}

impl FromRequestParts<AppState> for AuthedAny {
    type Rejection = ApiError;
    async fn from_request_parts(parts: &mut Parts, st: &AppState) -> ApiResult<Self> {
        authenticate(parts, st).await.map(|(a, _)| AuthedAny(a))
    }
}

/// A loopback redirect URI: `http://127.0.0.1:<port>/<path>` or `http://[::1]:<port>/<path>`.
pub fn is_loopback_redirect(u: &str) -> bool {
    let Some(rest) = u
        .strip_prefix("http://127.0.0.1:")
        .or_else(|| u.strip_prefix("http://[::1]:"))
    else {
        return false;
    };
    let (port, path) = rest.split_once('/').unwrap_or((rest, ""));
    port.parse::<u16>().is_ok_and(|p| p > 0) && path.len() <= 200 && !path.contains(['?', '#', ' ']) && u.len() <= 256
}

/// A display name the server accepts: within the configured length, no control character, no leading or trailing
/// whitespace.
pub fn check_display_name(core: &Core, name: &str) -> ApiResult<()> {
    let n = name.chars().count();
    let l = &core.cfg.limits;
    if n < l.display_name_min || n > l.display_name_max || name.trim() != name || name.chars().any(char::is_control) {
        return Err(ApiError::invalid(
            "/display_name",
            format!(
                "a display name has {} to {} characters, no control character and no surrounding space",
                l.display_name_min, l.display_name_max
            ),
        ));
    }
    Ok(())
}

/// A display name made acceptable (cut, cleaned); `Player` when nothing is left.
pub fn sanitize_display_name(core: &Core, name: &str) -> String {
    let cleaned: String = name
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    let cut: String = cleaned
        .chars()
        .take(core.cfg.limits.display_name_max)
        .collect::<String>()
        .trim()
        .to_string();
    if cut.chars().count() < core.cfg.limits.display_name_min {
        "Player".chars().take(core.cfg.limits.display_name_max).collect()
    } else {
        cut
    }
}

/// Finds the account of a login identity, or creates it. Returns `(user, created)`.
pub fn login_identity(core: &mut Core, provider: &str, subject: &str, display_name: &str) -> ApiResult<(String, bool)> {
    let hash = identity_hash(&core.secret, provider, subject);
    if let Some(user) = find_identity(core, provider, &hash)? {
        return Ok((user, false));
    }
    create_account(core, provider, &hash, display_name, |_, _| Ok(())).map(|u| (u, true))
}

/// The account of a login identity (by its keyed hash).
pub fn find_identity(core: &Core, provider: &str, hash: &[u8]) -> ApiResult<Option<String>> {
    Ok(core
        .db
        .query_row(
            "SELECT user_id FROM identities WHERE provider = ?1 AND subject_hash = ?2",
            params![provider, hash],
            |r| r.get::<_, String>(0),
        )
        .optional()?)
}

/// Whether every login of `user` is a guest one (a display name only, no identity to come back with).
pub fn is_guest(core: &Core, user: &str) -> bool {
    core.db
        .query_row(
            "SELECT COUNT(*), COALESCE(SUM(provider = 'guest'), 0) FROM identities WHERE user_id = ?1",
            [user],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        )
        .is_ok_and(|(all, guest)| all > 0 && all == guest)
}

/// Security review M7: whether `user` may play rated queues and tournaments (`[auth.guest] ranked`).
pub fn may_play_ranked(core: &Core, user: &str) -> bool {
    core.cfg.auth.guest.ranked || !is_guest(core, user)
}

/// The requirement a guest account does not meet where `[auth.guest] ranked = false`.
pub fn ranked_requirement() -> Value {
    json!({"en": "An account you can log in to again (not a guest account)",
           "fr": "Un compte auquel on peut se reconnecter (pas un compte invité)"})
}

/// Creates an account with one login identity, and whatever `extra` writes in the same transaction (a password
/// account's hash). A running ban of an erased account with this identity carries over.
pub fn create_account(
    core: &mut Core,
    provider: &str,
    hash: &[u8],
    display_name: &str,
    extra: impl FnOnce(&rusqlite::Transaction<'_>, &str) -> rusqlite::Result<()>,
) -> ApiResult<String> {
    let count: i64 = core.db.query_row("SELECT COUNT(*) FROM users", [], |r| r.get(0))?;
    if count as usize >= core.cfg.limits.max_users {
        return Err(ApiError::new(
            StatusCode::SERVICE_UNAVAILABLE,
            "unavailable",
            "this server accepts no new accounts",
        ));
    }
    let user = new_id("u");
    let name = sanitize_display_name(core, display_name);
    let mut code = new_code(12);
    while core
        .db
        .query_row("SELECT 1 FROM users WHERE friend_code = ?1", [&code], |_| Ok(()))
        .optional()?
        .is_some()
    {
        code = new_code(12);
    }
    // An identity whose erased account was banned gets the rest of the ban.
    let ban: Option<(i64, Option<String>)> = core
        .db
        .query_row(
            "SELECT until, reason FROM identity_bans WHERE provider = ?1 AND subject_hash = ?2 AND until > ?3",
            params![provider, hash, core.now],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let tx = core.db.unchecked_transaction()?;
    tx.execute(
        "INSERT INTO users (id, display_name, friend_code, created_at, banned_until, ban_reason) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![user, name, code, core.now, ban.as_ref().map(|b| b.0), ban.and_then(|b| b.1)],
    )?;
    tx.execute(
        "INSERT INTO identities (provider, subject_hash, user_id, created_at) VALUES (?1, ?2, ?3, ?4)",
        params![provider, hash, user, core.now],
    )?;
    // Security review M6: what an erased account of this identity still answered for.
    crate::api::account::take_over_hold(&tx, core.now, provider, hash, &user)?;
    extra(&tx, &user)?;
    tx.commit()?;
    Ok(user)
}

/// Creates a session and returns the session object of the protocol (with `me`).
pub fn issue_session(core: &mut Core, user: &str, created: bool) -> ApiResult<Value> {
    let access = new_token("kat1");
    let refresh = new_token("krt1");
    let l = &core.cfg.limits;
    let (aexp, rexp) = (core.now + l.access_ttl_s, core.now + l.refresh_ttl_s);
    core.db.execute(
        "INSERT INTO sessions (id, user_id, access_hash, access_expires, refresh_hash, refresh_expires, created_at, last_used_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?7)",
        params![new_id("se"), user, token_hash(&access), aexp, token_hash(&refresh), rexp, core.now],
    )?;
    Ok(session_json(core, user, &access, aexp, &refresh, rexp, created))
}

fn session_json(core: &Core, user: &str, access: &str, aexp: i64, refresh: &str, rexp: i64, created: bool) -> Value {
    json!({
        "access_token": access,
        "access_expires_at": rfc3339(aexp),
        "refresh_token": refresh,
        "refresh_expires_at": rfc3339(rexp),
        "created": created,
        "me": crate::api::account::me_json(core, user),
    })
}

/// Security review M5: one new account out of the client network's daily quota (`rate_limits.accounts_per_ip_per_day`,
/// 0: none). Taken just before the account is created, never for a login to an existing account: one network could
/// otherwise fill `limits.max_users` (guest logins create accounts) and close the server to every new player.
pub fn charge_creation(st: &AppState, ip: std::net::IpAddr) -> ApiResult<()> {
    if st.cfg().rate_limits.accounts_per_ip_per_day <= 0.0 {
        return Ok(());
    }
    st.limits
        .accounts
        .take(&ip, Instant::now())
        .map_err(ApiError::rate_limited)
}

/// The account of a login identity, or a new one charged to the client network's quota ([`charge_creation`]).
pub fn login_or_create(
    st: &AppState,
    core: &mut Core,
    ip: std::net::IpAddr,
    provider: &str,
    subject: &str,
    display_name: &str,
) -> ApiResult<(String, bool)> {
    let hash = identity_hash(&core.secret, provider, subject);
    if let Some(user) = find_identity(core, provider, &hash)? {
        return Ok((user, false));
    }
    charge_creation(st, ip)?;
    create_account(core, provider, &hash, display_name, |_, _| Ok(())).map(|u| (u, true))
}

fn check_auth_rate(st: &AppState, parts_ip: std::net::IpAddr) -> ApiResult<()> {
    st.limits
        .auth_per_ip
        .take(&parts_ip, Instant::now())
        .map_err(ApiError::rate_limited)
}

/// The client's address as set by the request middleware.
#[derive(Clone, Copy)]
pub struct ClientIp(pub std::net::IpAddr);

pub fn ip_of(ext: &axum::http::Extensions) -> std::net::IpAddr {
    ext.get::<ClientIp>()
        .map(|c| c.0)
        .unwrap_or(std::net::IpAddr::V4(std::net::Ipv4Addr::UNSPECIFIED))
}

/// `POST /v1/auth/{provider}/login` (direct providers).
pub async fn login(
    State(st): State<AppState>,
    Path(provider): Path<String>,
    ext: axum::http::Extensions,
    headers: HeaderMap,
    JsonBody(body): JsonBody<serde_json::Map<String, Value>>,
) -> ApiResult<Json<Value>> {
    let ip = ip_of(&ext);
    check_auth_rate(&st, ip)?;
    if let Some(p) = st.password.clone().filter(|p| p.handles(&provider)) {
        return p
            .form(&st, &provider, ip, password::lang_of(&headers), &body)
            .await
            .map(Json);
    }
    let field = |k: &str| body.get(k).and_then(Value::as_str).map(str::to_string);
    let mut core = st.lock();
    let (user, created) = match provider.as_str() {
        "dev" if st.cfg().auth.dev.enabled => {
            let name = field("username").ok_or_else(|| ApiError::invalid("/username", "username is required"))?;
            if !(1..=32).contains(&name.len())
                || !name.bytes().all(|b| b.is_ascii_alphanumeric() || b"_.-".contains(&b))
            {
                return Err(ApiError::invalid("/username", "1 to 32 of A-Z a-z 0-9 _ . -"));
            }
            login_or_create(&st, &mut core, ip, "dev", &name.to_ascii_lowercase(), &name)?
        }
        "guest" if st.cfg().auth.guest.enabled => {
            let name =
                field("display_name").ok_or_else(|| ApiError::invalid("/display_name", "display_name is required"))?;
            check_display_name(&core, &name)?;
            login_or_create(&st, &mut core, ip, "guest", &new_id("g"), &name)?
        }
        _ => return Err(ApiError::not_found("no such direct login provider")),
    };
    issue_session(&mut core, &user, created).map(Json)
}

#[derive(Deserialize)]
pub struct RefreshBody {
    refresh_token: String,
}

/// `POST /v1/auth/refresh`: rotation, with the previous token accepted for 60 s.
pub async fn refresh(
    State(st): State<AppState>,
    ext: axum::http::Extensions,
    JsonBody(body): JsonBody<RefreshBody>,
) -> ApiResult<Json<Value>> {
    check_auth_rate(&st, ip_of(&ext))?;
    let mut core = st.lock();
    let hash = token_hash(&body.refresh_token);
    let row: Option<(String, String, i64)> = core
        .db
        .query_row(
            "SELECT id, user_id, refresh_expires FROM sessions WHERE refresh_hash = ?1",
            [&hash],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .optional()?;
    let (session, user, keep_prev) = match row {
        Some((s, u, exp)) if exp > core.now => (s, u, false),
        Some(_) => return Err(ApiError::unauthorized("unauthorized", "the refresh token expired")),
        None => {
            let prev: Option<(String, String, i64)> = core
                .db
                .query_row(
                    "SELECT id, user_id, prev_refresh_until FROM sessions WHERE prev_refresh_hash = ?1",
                    [&hash],
                    |r| Ok((r.get(0)?, r.get(1)?, r.get::<_, Option<i64>>(2)?.unwrap_or(0))),
                )
                .optional()?;
            match prev {
                Some((s, u, until)) if until >= core.now => (s, u, true),
                Some((s, u, _)) => {
                    // Reuse of a rotated token after the window: treat as theft, end the session.
                    core.db.execute("DELETE FROM sessions WHERE id = ?1", [&s])?;
                    core.close_streams(&u, Some(&s), "session_revoked");
                    return Err(ApiError::unauthorized("session_revoked", "this session was revoked"));
                }
                None => return Err(ApiError::unauthorized("unauthorized", "unknown refresh token")),
            }
        }
    };
    let access = new_token("kat1");
    let refresh = new_token("krt1");
    let (aexp, rexp) = (
        core.now + core.cfg.limits.access_ttl_s,
        core.now + core.cfg.limits.refresh_ttl_s,
    );
    if keep_prev {
        core.db.execute(
            "UPDATE sessions SET access_hash = ?1, access_expires = ?2, refresh_hash = ?3, refresh_expires = ?4, last_used_at = ?5 WHERE id = ?6",
            params![token_hash(&access), aexp, token_hash(&refresh), rexp, core.now, session],
        )?;
    } else {
        core.db.execute(
            "UPDATE sessions SET access_hash = ?1, access_expires = ?2, refresh_hash = ?3, refresh_expires = ?4,
             prev_refresh_hash = ?5, prev_refresh_until = ?6, last_used_at = ?7 WHERE id = ?8",
            params![
                token_hash(&access),
                aexp,
                token_hash(&refresh),
                rexp,
                hash,
                core.now + 60,
                core.now,
                session
            ],
        )?;
    }
    Ok(Json(session_json(&core, &user, &access, aexp, &refresh, rexp, false)))
}

#[derive(Deserialize, Default)]
pub struct LogoutBody {
    #[serde(default)]
    all: bool,
}

/// `POST /v1/auth/logout`.
pub async fn logout(
    State(st): State<AppState>,
    AuthedAny(a): AuthedAny,
    JsonBody(body): JsonBody<LogoutBody>,
) -> ApiResult<StatusCode> {
    let mut core = st.lock();
    let sessions: Vec<String> = if body.all {
        let mut s = core.db.prepare("SELECT id FROM sessions WHERE user_id = ?1")?;
        s.query_map([&a.user], |r| r.get(0))?.flatten().collect()
    } else {
        vec![a.session.clone()]
    };
    for s in &sessions {
        core.db.execute("DELETE FROM sessions WHERE id = ?1", [s])?;
        core.push(&a.user, "session.revoked", json!({"reason": "logout"}));
        core.close_streams(&a.user, Some(s), "session_revoked");
    }
    Ok(StatusCode::NO_CONTENT)
}

/// The provider list of `GET /v1/meta`.
pub fn providers_json(st: &AppState) -> Vec<Value> {
    let mut v = Vec::new();
    let max = st.cfg().limits.display_name_max;
    let min = st.cfg().limits.display_name_min;
    for o in st.oauth.iter() {
        v.push(o.info());
    }
    if let Some(p) = &st.password {
        v.extend(p.providers(&st.cfg()));
    }
    if st.cfg().auth.guest.enabled {
        v.push(json!({"id": "guest", "kind": "direct", "label": {"en": "Just a name", "fr": "Juste un nom"},
            "fields": [{"key": "display_name", "type": "string", "label": {"en": "Name", "fr": "Nom"}, "required": true,
                        "min_len": min, "max_len": max}],
            "warning": {"en": "Your account lives only in this launcher's login. If you lose it, you lose the account.",
                        "fr": "Votre compte n'existe que dans la connexion de ce lanceur : si vous la perdez, vous perdez le compte."}}));
    }
    if st.cfg().auth.dev.enabled {
        v.push(
            json!({"id": "dev", "kind": "direct", "label": {"en": "Developer login"},
            "fields": [{"key": "username", "type": "string", "label": {"en": "Username"}, "required": true,
                        "min_len": 1, "max_len": 32}],
            "warning": {"en": "Test server: anyone can log in as any username."}}),
        );
    }
    v
}

/// `POST /v1/auth/{provider}/begin`.
pub async fn oauth_begin(
    State(st): State<AppState>,
    Path(provider): Path<String>,
    ext: axum::http::Extensions,
    JsonBody(body): JsonBody<oauth::BeginBody>,
) -> ApiResult<Json<Value>> {
    check_auth_rate(&st, ip_of(&ext))?;
    let o = oauth_provider(&st, &provider)?;
    o.begin(&st, ip_of(&ext), body).await.map(Json)
}

fn oauth_provider(st: &AppState, id: &str) -> ApiResult<std::sync::Arc<oauth::OAuthProvider>> {
    st.oauth
        .iter()
        .find(|o| o.id() == id)
        .cloned()
        .ok_or_else(|| ApiError::not_found("no such OAuth provider"))
}

/// `POST /v1/auth/{provider}/complete`.
pub async fn oauth_complete(
    State(st): State<AppState>,
    Path(provider): Path<String>,
    ext: axum::http::Extensions,
    JsonBody(body): JsonBody<oauth::CompleteBody>,
) -> ApiResult<Json<Value>> {
    let ip = ip_of(&ext);
    check_auth_rate(&st, ip)?;
    let o = oauth_provider(&st, &provider)?;
    let pending = {
        let mut core = st.lock();
        o.take_state(&mut core, &body)?
    };
    let identity = o
        .exchange(&body.code, &body.code_verifier, &pending.redirect_uri)
        .await?;
    let mut core = st.lock();
    let (user, created) = login_or_create(
        &st,
        &mut core,
        ip,
        o.id(),
        &identity.subject,
        &identity.name.unwrap_or_else(|| "Player".into()),
    )?;
    issue_session(&mut core, &user, created).map(Json)
}
