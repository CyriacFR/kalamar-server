//! Building the service: state, router, request middleware (client address, rate limits, body limits, protocol
//! header, request log without personal data), the ticker, and serving.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use axum::Router;
use axum::extract::{ConnectInfo, MatchedPath, Request, State};
use axum::http::{HeaderValue, StatusCode, header};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post, put};
use rusqlite::params;

use crate::api::{self, account, friends, moderation, parties, queues, ratings, rooms};
use crate::auth::{self, ClientIp};
use crate::clock::Clock;
use crate::config::Config;
use crate::error::{ApiError, BodyLimit};
use crate::netutil::{Cidr, client_ip, client_key};
use crate::ratelimit::Limiters;
use crate::state::{AppState, Core};
use crate::util::{hex, random_bytes, unhex};

fn load_secret(cfg: &Config) -> Result<Vec<u8>, String> {
    if cfg.server.database.as_os_str() == ":memory:" {
        return Ok(random_bytes::<32>().to_vec());
    }
    let path = &cfg.server.secret_file;
    match std::fs::read_to_string(path) {
        Ok(s) => unhex(s.trim())
            .filter(|k| k.len() >= 32)
            .ok_or_else(|| format!("{}: 64 hex digits expected", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
                std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
            }
            let key = random_bytes::<32>();
            // Created exclusively and, on Unix, readable by the service's account only from the first instant.
            kalamar_relay::roomauth::create_private_file(path, hex(&key).as_bytes())
                .map_err(|e| format!("{}: {e}", path.display()))?;
            tracing::info!(path = %path.display(), "created the server secret");
            Ok(key.to_vec())
        }
        Err(e) => Err(format!("{}: {e}", path.display())),
    }
}

/// Builds the state: database (migrated), secret, OAuth provider, restored matches.
pub fn build_state(mut cfg: Config, clock: Arc<dyn Clock>) -> Result<AppState, String> {
    crate::auth::oauth::install_crypto();
    cfg.normalize();
    let errors = cfg.validate();
    if !errors.is_empty() {
        return Err(errors.join("\n"));
    }
    // Every relay secret first (the embedded relay's is created at the first start): a missing or unreadable one
    // stops the start here, before anything else is touched.
    let relay_keys = crate::relays::prepare(&cfg)?;
    let db = crate::db::open(&cfg.server.database)?;
    let secret = load_secret(&cfg)?;
    let mut oauth = Vec::new();
    for r in auth::oauth::configured(&cfg) {
        oauth.push(Arc::new(auth::oauth::OAuthProvider::new(r?, cfg.server.production)?));
    }
    let password = auth::password::Passwords::new(&cfg, &secret)?.map(Arc::new);
    for w in cfg.warnings() {
        tracing::warn!(warning = %w, "configuration");
    }
    for (a, b) in crate::relays::shared_secrets(&cfg, &relay_keys) {
        tracing::warn!(relays = %format!("{a}, {b}"), "two relays hold the same secret: whoever runs either can open rooms on the other");
    }
    let cfg = Arc::new(cfg);
    let mut core = Core::new(cfg.clone(), db, secret, clock.now());
    core.relay_keys = Arc::new(relay_keys);
    core.live.meta_revision = crate::reload::revision_after(0, core.now);
    core.live.webhooks = crate::webhooks::spawn(&cfg, clock.clone());
    crate::db::current_season(&core.db, core.now).map_err(|e| e.to_string())?;
    crate::matches::restore(&mut core).map_err(|e| e.message)?;
    crate::series::restore(&mut core).map_err(|e| e.to_string())?;
    crate::activities::restore(&mut core).map_err(|e| e.to_string())?;
    let trusted: Vec<Cidr> = cfg
        .server
        .trusted_proxies
        .iter()
        .filter_map(|c| Cidr::parse(c))
        .collect();
    Ok(AppState {
        limits: Arc::new(Limiters::new(&cfg.rate_limits)),
        current_cfg: Arc::new(std::sync::RwLock::new(cfg)),
        config_path: None,
        core: Arc::new(Mutex::new(core)),
        clock,
        trusted: Arc::new(trusted),
        oauth: Arc::new(oauth),
        password,
    })
}

async fn mw(State(st): State<AppState>, mut req: Request, next: Next) -> Response {
    let started = Instant::now();
    let peer = req
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|c| c.0.ip())
        .unwrap_or(std::net::IpAddr::from([127, 0, 0, 1]));
    // Security review M5: every per-address limit counts the client's network (an IPv6 /64 by default).
    let rl = &st.cfg().rate_limits;
    let ip = client_key(
        client_ip(peer, req.headers(), &st.trusted),
        rl.ipv4_prefix,
        rl.ipv6_prefix,
    );
    req.extensions_mut().insert(ClientIp(ip));
    if let Err(retry) = st.limits.per_ip.take(&ip, Instant::now()) {
        return ApiError::rate_limited(retry).into_response();
    }
    let path = req.uri().path().to_string();
    let limit = if path.ends_with("/result") || path.ends_with("/me/game") {
        st.cfg().limits.result_bytes
    } else if path.ends_with("/status") {
        st.cfg().limits.status_bytes
    } else {
        st.cfg().limits.body_bytes
    };
    req.extensions_mut().insert(BodyLimit(limit));
    let route = req
        .extensions()
        .get::<MatchedPath>()
        .map(|m| m.as_str().to_string())
        .unwrap_or_else(|| "unmatched".into());
    let method = req.method().clone();
    let mut resp = next.run(req).await;
    let h = resp.headers_mut();
    h.insert(
        "kalamar-protocol",
        HeaderValue::from_static(kalamar_protocol::PROTOCOL_VERSION),
    );
    // `no-store` unless the handler chose a cache policy (discovery, population: public data only).
    if !path.starts_with("/.well-known/") && !h.contains_key(header::CACHE_CONTROL) {
        h.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    }
    // Route templates only: no ids, codes, tokens or addresses in the log.
    tracing::info!(method = %method, route = %route, status = resp.status().as_u16(), ms = started.elapsed().as_millis() as u64, "request");
    resp
}

async fn not_found() -> ApiError {
    ApiError::not_found("no such endpoint")
}

async fn method_not_allowed() -> ApiError {
    ApiError::new(
        StatusCode::METHOD_NOT_ALLOWED,
        "not_found",
        "method not allowed on this endpoint",
    )
}

/// The router: the discovery document at the root, the API under the path of `server.api_url`.
pub fn router(st: AppState) -> Router {
    let v1 = Router::new()
        .route("/v1/meta", get(api::meta::meta))
        .route("/v1/population", get(api::population::get))
        .route("/v1/auth/refresh", post(auth::refresh))
        .route("/v1/auth/logout", post(auth::logout))
        .route("/v1/auth/{provider}/login", post(auth::login))
        .route("/v1/auth/{provider}/begin", post(auth::oauth_begin))
        .route("/v1/auth/{provider}/complete", post(auth::oauth_complete))
        .route(
            "/v1/me",
            get(account::get_me).patch(account::patch_me).delete(account::erase),
        )
        .route("/v1/me/export", get(account::export))
        .route("/v1/me/password", post(auth::password::change_me))
        .route("/v1/me/game", put(account::put_game))
        .route("/v1/me/friend-code", post(friends::rotate_code))
        .route("/v1/state", get(account::get_state))
        .route("/v1/users/{user_id}", get(account::get_user))
        .route("/v1/users/{user_id}/ratings", get(ratings::user_ratings))
        .route("/v1/users/{user_id}/stats", get(ratings::stats))
        .route("/v1/users/{user_id}/matches", get(ratings::history))
        .route("/v1/events", get(crate::events::events))
        .route("/v1/events/ticket", post(crate::events::ticket))
        .route("/v1/matches/current", get(api::match_current))
        .route("/v1/matches/{match_id}", get(api::match_get))
        .route("/v1/matches/{match_id}/accept", post(api::match_accept))
        .route("/v1/matches/{match_id}/decline", post(api::match_decline))
        .route("/v1/matches/{match_id}/launch", post(api::match_launch))
        .route("/v1/matches/{match_id}/status", post(api::match_status))
        .route("/v1/matches/{match_id}/result", post(api::match_result))
        .route("/v1/matches/{match_id}/draft", post(crate::draft::post))
        .route(
            "/v1/matches/{match_id}/spectate",
            post(crate::spectate::take).delete(crate::spectate::leave),
        )
        .route("/v1/spectate", get(crate::spectate::list))
        .route(
            "/v1/series/{series_id}/spectate",
            post(crate::spectate::take_series_ep).delete(crate::spectate::leave_series_ep),
        )
        .route("/v1/rooms", post(rooms::create))
        .route("/v1/rooms/current", get(rooms::current))
        .route("/v1/rooms/join", post(rooms::join))
        .route("/v1/rooms/{room_id}", get(rooms::get).patch(rooms::patch))
        .route("/v1/rooms/{room_id}/leave", post(rooms::leave))
        .route("/v1/rooms/{room_id}/invites", post(rooms::invite))
        .route("/v1/rooms/{room_id}/kick", post(rooms::kick))
        .route("/v1/rooms/{room_id}/owner", post(rooms::owner))
        .route("/v1/rooms/{room_id}/members/{user_id}", put(rooms::set_team))
        .route("/v1/rooms/{room_id}/ready", post(rooms::ready))
        .route("/v1/rooms/{room_id}/start", post(rooms::start))
        .route("/v1/queues", get(queues::list))
        .route("/v1/queues/{queue_id}", get(queues::get))
        .route("/v1/queues/{queue_id}/tickets", post(queues::join))
        .route("/v1/tickets/current", get(queues::current).delete(queues::cancel))
        .route("/v1/party", get(parties::get).post(parties::create))
        .route("/v1/party/invites", get(parties::incoming).post(parties::invite))
        .route("/v1/party/invites/{user_id}", delete(parties::revoke))
        .route("/v1/parties/{party_id}/join", post(parties::join))
        .route("/v1/parties/{party_id}/decline", post(parties::decline))
        .route("/v1/party/leave", post(parties::leave))
        .route("/v1/party/kick", post(parties::kick))
        .route("/v1/party/leader", post(parties::leader))
        .route("/v1/friends", get(friends::list))
        .route("/v1/friends/{user_id}", delete(friends::remove))
        .route("/v1/friends/requests", get(friends::requests).post(friends::send))
        .route("/v1/friends/requests/{request_id}", delete(friends::cancel))
        .route("/v1/friends/requests/{request_id}/accept", post(friends::accept))
        .route("/v1/friends/requests/{request_id}/decline", post(friends::decline))
        .route("/v1/ladders", get(ratings::ladders))
        .route("/v1/ladders/{ladder_id}/leaderboard", get(ratings::leaderboard))
        .route("/v1/seasons", get(ratings::seasons))
        .route("/v1/reports", post(moderation::report))
        .route("/v1/blocks", get(moderation::blocks))
        .route(
            "/v1/blocks/{user_id}",
            put(moderation::block).delete(moderation::unblock),
        )
        .route("/v1/_test/clock", post(api::test_clock))
        .route("/v1/activities", get(crate::activities::list))
        .route("/v1/activities/{activity_id}", get(crate::activities::get))
        .route(
            "/v1/activities/{activity_id}/actions/{action_id}",
            post(crate::activities::act),
        )
        .route("/v1/series/{series_id}", get(crate::series::get))
        .route("/v1/users/{user_id}/badges", get(crate::badges::user_badges))
        .route("/v1/users/{user_id}/profile", get(crate::badges::profile))
        .route("/v1/titles", get(crate::badges::titles))
        .route("/v1/operator/matches", get(crate::operator::matches))
        .route("/v1/operator/matches/{match_id}", get(crate::operator::match_detail))
        .route(
            "/v1/operator/matches/{match_id}/decision",
            post(crate::operator::decide),
        )
        .route("/v1/operator/reports", get(crate::operator::reports))
        .route(
            "/v1/operator/matches/{match_id}/spectators",
            post(crate::spectate::operator_match),
        )
        .route(
            "/v1/operator/series/{series_id}/spectators",
            post(crate::spectate::operator_series),
        )
        .route(
            "/v1/operator/reports/{report_id}/resolve",
            post(crate::operator::resolve),
        )
        .route("/v1/operator/users/{user_id}/ban", post(crate::operator::ban))
        .route("/v1/operator/users/{user_id}/unban", post(crate::operator::unban))
        .route(
            "/v1/operator/ladders/{ladder_id}/leaderboard",
            get(crate::operator::leaderboard),
        )
        .method_not_allowed_fallback(method_not_allowed);
    let prefix = st.cfg().api_prefix();
    let app = Router::new().route("/.well-known/kalamar-server.json", get(api::meta::discovery));
    let app = if prefix.is_empty() {
        app.merge(v1)
    } else {
        app.nest(&prefix, v1)
    };
    app.fallback(not_found)
        .layer(middleware::from_fn_with_state(st.clone(), mw))
        .with_state(st)
}

/// Every time-based transition: matchmaking, deadlines, expiries, sweeps, retention.
pub fn tick(st: &AppState) {
    let mut core = st.lock();
    let core = &mut *core;
    crate::matches::tick(core);
    crate::draft::tick(core);
    queues::tick(core);
    rooms::sweep(core);
    crate::activities::tick(core);
    crate::events::sweep(core);
    let now = core.now;
    core.live.party_invites.retain(|_, (_, exp)| *exp > now);
    core.live.oauth_states.retain(|_, p| p.expires > now);
    core.live
        .last_seen
        .retain(|_, t| now - *t <= api::population::ACTIVITY_WINDOW_S);
    if now - core.live.last_sweep >= 30 {
        core.live.last_sweep = now;
        enforce_restrictions(core);
        auth::password::sweep_known(&mut core.live.known_ips, now);
    }
    if now - core.live.last_purge >= 3600 {
        core.live.last_purge = now;
        if let Err(e) = purge(core) {
            tracing::error!(error = %e, "retention purge failed");
        }
    }
}

/// Players banned or erased from the command line leave the live state.
fn enforce_restrictions(core: &mut Core) {
    let mut users: std::collections::HashSet<String> = core.live.user_streams.keys().cloned().collect();
    users.extend(core.live.user_room.keys().cloned());
    users.extend(core.live.user_party.keys().cloned());
    users.extend(core.live.user_ticket.keys().cloned());
    for u in users {
        let gone = !core.user_exists(&u);
        if gone || core.ban_of(&u).is_some() {
            queues::cancel_ticket_of(core, &u, "cancelled");
            parties::remove_from_party(core, &u, "kicked");
            rooms::remove_from_room(core, &u, "admin");
            core.push(
                &u,
                "session.revoked",
                serde_json::json!({"reason": if gone { "erased" } else { "banned" }}),
            );
            core.close_streams(&u, None, if gone { "session_revoked" } else { "banned" });
        }
    }
}

fn purge(core: &mut Core) -> rusqlite::Result<()> {
    let now = core.now;
    crate::matches::purge_raw(core)?;
    core.db
        .execute("DELETE FROM sessions WHERE refresh_expires < ?1", [now])?;
    core.db.execute(
        "DELETE FROM friend_requests WHERE created_at < ?1",
        [now - core.cfg.limits.friend_request_ttl_s],
    )?;
    core.db.execute("DELETE FROM identity_bans WHERE until < ?1", [now])?;
    // Security review M6: the holds of erased identities end; their reports stay anonymous until resolved and purged.
    core.db.execute("DELETE FROM identity_holds WHERE until < ?1", [now])?;
    core.db.execute(
        "UPDATE reports SET target_hold = NULL WHERE target_hold IS NOT NULL
         AND target_hold NOT IN (SELECT hold_id FROM identity_holds)",
        [],
    )?;
    core.db.execute(
        "DELETE FROM audit WHERE at < ?1",
        [now - core.cfg.retention.audit_days * 86_400],
    )?;
    // Security review M5: a guest account whose last session ended can never be logged in to again (it lived in its
    // launcher's tokens): it is erased like any account, so that dead guest accounts never fill `limits.max_users`.
    let dead: Vec<String> = {
        let mut s = core.db.prepare(
            "SELECT u.id FROM users u WHERE u.created_at < ?1
               AND EXISTS (SELECT 1 FROM identities i WHERE i.user_id = u.id AND i.provider = 'guest')
               AND NOT EXISTS (SELECT 1 FROM identities i WHERE i.user_id = u.id AND i.provider != 'guest')
               AND NOT EXISTS (SELECT 1 FROM sessions s WHERE s.user_id = u.id)
             LIMIT 1000",
        )?;
        s.query_map([now - core.cfg.limits.refresh_ttl_s], |r| r.get(0))?
            .collect::<rusqlite::Result<_>>()?
    };
    for u in &dead {
        if let Err(e) = account::erase_user(core, u) {
            tracing::error!(error = %e.message, "a dead guest account was not erased");
        }
    }
    if !dead.is_empty() {
        tracing::info!(accounts = dead.len(), "guest accounts without a session left erased");
    }
    core.db
        .execute("DELETE FROM password_codes WHERE expires_at <= ?1", [now])?;
    core.db.execute(
        "DELETE FROM reports WHERE status = 'resolved' AND resolved_at < ?1",
        params![now - core.cfg.retention.closed_reports_days * 86_400],
    )?;
    Ok(())
}

/// Runs the ticker every second until the returned handle is aborted.
pub fn spawn_ticker(st: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut iv = tokio::time::interval(Duration::from_secs(1));
        loop {
            iv.tick().await;
            tick(&st);
            crate::reload::poll(&st);
            crate::relays::serve_admin(&st);
            st.limits.sweep(Instant::now());
            if let Some(p) = &st.password {
                p.sweep(Instant::now());
            }
        }
    })
}

/// A running service (tests and the binary).
pub struct Running {
    pub addr: SocketAddr,
    pub state: AppState,
    pub server: tokio::task::JoinHandle<()>,
    pub ticker: tokio::task::JoinHandle<()>,
    /// The relays' reachability check (`relays.rs`).
    pub probe: tokio::task::JoinHandle<()>,
}

impl Running {
    pub fn base_url(&self) -> String {
        format!("http://{}{}", self.addr, self.state.cfg().api_prefix())
    }
    pub fn stop(&self) {
        self.server.abort();
        self.ticker.abort();
        self.probe.abort();
    }
}

/// The HTTP/1.1 server (security review L-S2): a client has `header_timeout` to send its request's headers (the body has
/// [`crate::error::BODY_TIMEOUT`]), and at most `max_connections` connections are open at once (more are closed at
/// once); `axum::serve` had neither, so a service reachable without its reverse proxy could be held by slow clients.
/// On `shutdown` it stops accepting, lets the requests in flight end (10 s at most) and returns.
async fn serve(
    listener: tokio::net::TcpListener,
    app: Router,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
    header_timeout: Duration,
    max_connections: usize,
) {
    use hyper_util::rt::{TokioIo, TokioTimer};
    let permits = Arc::new(tokio::sync::Semaphore::new(max_connections));
    let (stop_tx, stop_rx) = tokio::sync::watch::channel(false);
    tokio::pin!(shutdown);
    loop {
        let (stream, peer) = tokio::select! {
            r = listener.accept() => match r {
                Ok(c) => c,
                Err(e) => {
                    // Out of file descriptors, a reset before accept: wait a little, never spin.
                    tracing::warn!(error = %e, "accept failed");
                    tokio::time::sleep(Duration::from_millis(50)).await;
                    continue;
                }
            },
            () = &mut shutdown => break,
        };
        let Ok(permit) = permits.clone().try_acquire_owned() else {
            drop(stream);
            continue;
        };
        let _ = stream.set_nodelay(true);
        let app = app.clone();
        let svc = hyper::service::service_fn(move |mut req: hyper::Request<hyper::body::Incoming>| {
            req.extensions_mut().insert(ConnectInfo(peer));
            tower::ServiceExt::oneshot(app.clone(), req)
        });
        let mut stop = stop_rx.clone();
        tokio::spawn(async move {
            let conn = hyper::server::conn::http1::Builder::new()
                .timer(TokioTimer::new())
                .header_read_timeout(header_timeout)
                .serve_connection(TokioIo::new(stream), svc)
                .with_upgrades();
            tokio::pin!(conn);
            tokio::select! {
                _ = conn.as_mut() => {}
                _ = stop.changed() => {
                    conn.as_mut().graceful_shutdown();
                    let _ = conn.await;
                }
            }
            drop(permit);
        });
    }
    let _ = stop_tx.send(true);
    let all = u32::try_from(max_connections).unwrap_or(u32::MAX);
    let _ = tokio::time::timeout(Duration::from_secs(10), permits.acquire_many(all)).await;
}

/// Serves on `listener` until `shutdown` resolves.
pub async fn start(
    st: AppState,
    listener: tokio::net::TcpListener,
    shutdown: impl std::future::Future<Output = ()> + Send + 'static,
) -> std::io::Result<Running> {
    let addr = listener.local_addr()?;
    let app = router(st.clone());
    let ticker = spawn_ticker(st.clone());
    let probe = crate::relays::spawn_probe(st.clone());
    let limits = &st.cfg().limits;
    let (header_timeout, max_connections) = (
        Duration::from_secs(limits.header_timeout_s.max(1)),
        limits.max_connections.max(1),
    );
    let server = tokio::spawn(serve(listener, app, shutdown, header_timeout, max_connections));
    Ok(Running {
        addr,
        state: st,
        server,
        ticker,
        probe,
    })
}
