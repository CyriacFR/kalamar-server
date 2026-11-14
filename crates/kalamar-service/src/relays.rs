//! The relays of matches (`[[relays]]`, `[embedded_relay]`, `relay.md` section 4): which one a match gets, what
//! launchers are told about them, the reachability check that keeps new matches off a relay that stopped answering,
//! the load the bound relays announce (1.1.13, `[relay_load]`: no new match on a full relay), the secrets of relays
//! bound to this service, and the relay this service runs itself.
//!
//! A match keeps its relay for its whole life, and a series for all its games: nothing here moves a running game.
//! Sealed room codes are made by the relay's own code (`kalamar_relay::roomauth`), the only implementation.

use std::collections::HashMap;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

use kalamar_relay::roomauth::{self, RoomSecret};
use serde_json::{Value, json};

use crate::config::{Config, RelayEntry, same_file};
use crate::state::{AppState, Core};

/// Checks in a row a relay must fail before new matches avoid it.
pub const DOWN_AFTER: u32 = 3;
/// A relay a player declared it could not reach counts as this slow (it is chosen only when every relay is like that).
const UNREACHABLE_MS: u32 = 1_000_000;
/// The longest round trip a launcher can declare: it gives up after 2 seconds (`relay.md` 4.1), so a larger value is
/// taken as 2 seconds, and a value below 1 ms as 1 ms.
pub const RTT_MAX_MS: u32 = 2_000;

/// `meta.relays`: every relay a match may be given.
pub fn declaration(c: &Config) -> Vec<Value> {
    c.relays
        .iter()
        .map(|r| {
            let mut v = json!({"id": r.key(), "host": r.host, "port": r.port});
            if let Some(region) = &r.region {
                v["region"] = json!(region);
            }
            if !r.label.is_empty() {
                v["label"] = json!(r.label);
            }
            v
        })
        .collect()
}

// ---- secrets -----------------------------------------------------------------------------------------------------

/// The secrets of the bound relays, read at start and at each reload and then kept in memory (never logged: a
/// [`RoomSecret`] prints its length only). A match never reads a file.
#[derive(Debug, Clone, Default)]
pub struct Keys {
    /// By relay id ([`RelayEntry::key`]).
    by_relay: HashMap<String, RoomSecret>,
    /// The secret of `[embedded_relay]`: the very value (the same allocation) the relay this service runs checks
    /// codes with, shared by the `[[relays]]` entries that name it.
    pub embedded: Option<RoomSecret>,
}

impl Keys {
    pub fn of(&self, r: &RelayEntry) -> Option<&RoomSecret> {
        self.by_relay.get(&r.key())
    }
}

/// The secret a `[[relays]]` entry names (`secret_file` or `secret_env`), read now; `None`: an open relay.
pub fn read_relay_secret(r: &RelayEntry) -> Result<Option<RoomSecret>, String> {
    let what = format!("relays.{}: secret", r.key());
    let hint = "(kalamar-relay --new-room-secret <file> makes one)";
    match (&r.secret_file, &r.secret_env) {
        (Some(f), _) => RoomSecret::from_file(f)
            .map(Some)
            .map_err(|e| format!("{what}: {e} {hint}")),
        (None, Some(name)) => {
            let v = std::env::var(name).map_err(|_| format!("{what}: the variable {name} is not set"))?;
            roomauth::parse_secret(&v)
                .map(|b| Some(RoomSecret::new(b)))
                .map_err(|e| format!("{what}: {name}: {e} {hint}"))
        }
        (None, None) => Ok(None),
    }
}

/// The secret of `[embedded_relay]`, created at the first start (readable by the service's account only, see
/// `roomauth::create_private_file`). `None` without `[embedded_relay]` or without its `secret_file`.
pub fn embedded_secret(cfg: &Config) -> Result<Option<RoomSecret>, String> {
    let er = &cfg.embedded_relay;
    let (true, Some(f)) = (er.enabled, &er.secret_file) else {
        return Ok(None);
    };
    if !f.exists() {
        if let Some(dir) = f.parent().filter(|d| !d.as_os_str().is_empty()) {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        match roomauth::create_secret_file(f) {
            Ok(s) => {
                tracing::info!(path = %f.display(), "created the secret of the relay this service runs");
                return Ok(Some(s));
            }
            // Created meanwhile by someone else: read it like any other.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(format!("embedded_relay.secret_file: {}: {e}", f.display())),
        }
    }
    RoomSecret::from_file(f)
        .map(Some)
        .map_err(|e| format!("embedded_relay.secret_file: {e}"))
}

/// Reads the secret of every bound relay of `cfg`. The entries whose `secret_file` is the embedded relay's get
/// `embedded` itself (never a second read of the file), so that the service and its relay cannot disagree.
pub fn load_keys(cfg: &Config, embedded: Option<&RoomSecret>) -> Result<Keys, Vec<String>> {
    let mut keys = Keys {
        by_relay: HashMap::new(),
        embedded: embedded.cloned(),
    };
    let mut errors = Vec::new();
    let er = &cfg.embedded_relay;
    for r in &cfg.relays {
        let shared = match (embedded, &r.secret_file, &er.secret_file) {
            (Some(e), Some(f), Some(ef)) if er.enabled && same_file(f, ef) => Some(e.clone()),
            _ => None,
        };
        match shared.map(|s| Ok(Some(s))).unwrap_or_else(|| read_relay_secret(r)) {
            Ok(Some(s)) => {
                keys.by_relay.insert(r.key(), s);
            }
            Ok(None) => {}
            Err(e) => errors.push(e),
        }
    }
    if errors.is_empty() { Ok(keys) } else { Err(errors) }
}

/// Everything about relay secrets a start needs: the embedded relay's (created if needed), then every other.
pub fn prepare(cfg: &Config) -> Result<Keys, String> {
    let embedded = embedded_secret(cfg)?;
    load_keys(cfg, embedded.as_ref()).map_err(|e| e.join("\n"))
}

/// The room code a match on `relay` gets from the fresh (or kept) code `room`: sealed when the relay is bound to
/// this service, unchanged otherwise. A sealed code still accepted is kept, so that the rounds of one game session
/// stay in one room.
pub fn room_for(core: &Core, relay: &RelayEntry, room: &str, now: i64) -> Result<String, String> {
    match core.relay_keys.of(relay) {
        None => Ok(room.to_string()),
        // (The message never holds the code: it is logged.)
        Some(s) => roomauth::seal_room(s, room, now.max(0) as u64)
            .ok_or_else(|| format!("relays.{}: a room code that cannot be sealed", relay.key())),
    }
}

/// Two `[[relays]]` entries that hold the same secret (check-config says so: the operator of either relay can then
/// open rooms on the other).
pub fn shared_secrets(cfg: &Config, keys: &Keys) -> Vec<(String, String)> {
    let mut out = Vec::new();
    for (i, a) in cfg.relays.iter().enumerate() {
        for b in &cfg.relays[i + 1..] {
            if let (Some(x), Some(y)) = (keys.of(a), keys.of(b))
                && x == y
            {
                out.push((a.key(), b.key()));
            }
        }
    }
    out
}

/// [`shared_secrets`] for `check-config`, from the files as they are now (an embedded relay's secret not created yet
/// is left out).
pub fn shared_secrets_on_disk(cfg: &Config) -> Vec<(String, String)> {
    let er = &cfg.embedded_relay;
    let embedded = er
        .secret_file
        .as_ref()
        .filter(|f| er.enabled && f.exists())
        .and_then(|f| RoomSecret::from_file(f).ok());
    match load_keys(cfg, embedded.as_ref()) {
        Ok(keys) => shared_secrets(cfg, &keys),
        Err(_) => Vec::new(),
    }
}

// ---- the relay this service runs ------------------------------------------------------------------------------

/// The relay of `[embedded_relay]`, on threads of its own (its workers, and one with a single-threaded runtime for its
/// periodic work): the work of the HTTP API never delays a datagram, and a stop closes every member's session as
/// `kalamar-relay` does.
pub struct Embedded {
    pub addr: SocketAddr,
    stop: Option<tokio::sync::oneshot::Sender<()>>,
    thread: Option<std::thread::JoinHandle<Option<kalamar_relay::relay::Stats>>>,
}

/// Starts the relay of `[embedded_relay]` with `secret` (the value of [`Keys::embedded`]). `Ok(None)`: not enabled.
/// The UDP port is bound before this returns, so that a port in use stops the start with a clear message.
pub fn start_embedded(cfg: &Config, secret: Option<RoomSecret>) -> Result<Option<Embedded>, String> {
    let er = &cfg.embedded_relay;
    if !er.enabled {
        return Ok(None);
    }
    let relay_cfg = er.relay_config(secret);
    relay_cfg.validate().map_err(|e| format!("embedded_relay: {e}"))?;
    let ip: IpAddr = er.bind.parse().map_err(|e| format!("embedded_relay.bind: {e}"))?;
    let want = SocketAddr::new(ip, er.port);
    let workers = er.worker_count();
    let sockets = kalamar_relay::server::bind(want, workers).map_err(|e| format!("embedded relay: UDP {want}: {e}"))?;
    let addr = sockets[0].local_addr().map_err(|e| format!("embedded relay: {e}"))?;
    let (tx, rx) = tokio::sync::oneshot::channel::<()>();
    let every = Duration::from_secs(er.stats_interval_s);
    let (bound, max_rooms, max_sessions) = (
        relay_cfg.room_secret.is_some(),
        relay_cfg.max_rooms,
        relay_cfg.max_sessions,
    );
    let thread = std::thread::Builder::new()
        .name("kalamar-relay".into())
        .spawn(move || {
            let rt = match tokio::runtime::Builder::new_current_thread().enable_all().build() {
                Ok(rt) => rt,
                Err(e) => {
                    tracing::error!(error = %e, "embedded relay: no runtime");
                    return None;
                }
            };
            let run = rt.block_on(kalamar_relay::server::run_on(
                sockets,
                workers,
                relay_cfg,
                every,
                async move {
                    let _ = rx.await;
                },
            ));
            match run {
                Ok(stats) => Some(stats),
                Err(e) => {
                    tracing::error!(error = %e, "embedded relay stopped");
                    None
                }
            }
        })
        .map_err(|e| format!("embedded relay: {e}"))?;
    tracing::info!(%addr, workers, bound, max_rooms, max_sessions, "embedded relay listening (UDP, on threads of its own; a cap of 0 is none)");
    Ok(Some(Embedded {
        addr,
        stop: Some(tx),
        thread: Some(thread),
    }))
}

impl Embedded {
    /// Stops the relay: every member gets `CLOSED` (reason 4, shutdown), then the thread ends; its last counters.
    pub fn stop(mut self) -> Option<kalamar_relay::relay::Stats> {
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
        self.thread.take().and_then(|t| t.join().ok()).flatten()
    }
}

impl Drop for Embedded {
    fn drop(&mut self) {
        // Dropped without `stop`: the relay still ends cleanly, nobody waits for it.
        if let Some(tx) = self.stop.take() {
            let _ = tx.send(());
        }
    }
}

// ---- choosing ----------------------------------------------------------------------------------------------------

/// Matches not ended that run on `relay`.
fn busy(core: &Core, relay: &RelayEntry) -> usize {
    core.live
        .matches
        .values()
        .filter(|m| !m.state.ended() && m.game.relay_host == relay.host && m.game.relay_port == relay.port)
        .count()
}

/// Members (players and spectators) of the matches not ended that run on `relay`: the sessions this service sends
/// there, which the relay counts only once their games joined.
fn members_on(core: &Core, relay: &RelayEntry) -> usize {
    core.live
        .matches
        .values()
        .filter(|m| !m.state.ended() && m.game.relay_host == relay.host && m.game.relay_port == relay.port)
        .map(|m| m.game.players)
        .sum()
}

/// Whether the reachability check currently holds `relay` for down.
pub fn is_down(core: &Core, relay: &RelayEntry) -> bool {
    core.live
        .relay_fails
        .get(&relay.addr())
        .is_some_and(|n| *n >= DOWN_AFTER)
}

/// What the service knows of a bound relay's load (1.1.13, `[relay_load]`).
#[derive(Debug, Clone, Default)]
pub struct LoadState {
    /// The last answer, and when it came (the service's clock).
    pub load: Option<kalamar_relay::status::Load>,
    pub at: i64,
    /// Questions in a row left unanswered.
    pub misses: u32,
    /// Whether the relay ever answered since this service started: one that never did announces nothing (an older
    /// relay), one that stopped is silent.
    pub ever: bool,
    /// At `full_at_percent` and not yet back under `resume_at_percent`.
    pub full: bool,
}

/// Unanswered questions in a row after which a relay that answered before counts as silent.
pub const SILENT_AFTER: u32 = 3;

/// A relay as the choice of a match sees it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Room {
    /// It announces nothing (an open relay, one that does not know the query, or `status_s = 0`): chosen without its load.
    Unknown,
    /// It has room: the percentage left.
    Free(u8),
    /// Full, or the match would not fit (its sessions or a room more than the caps).
    Full,
    /// It answered before and stopped answering.
    Silent,
}

/// The load of `relay` in percent: the larger of its sessions and rooms against their caps (what it reported, or
/// the members of this service's matches there when more: a match counts before its games joined) and its workers'
/// mean time spent working. `None` when it announces nothing. `extra` is the members of the match being placed.
pub fn load_percent(core: &Core, relay: &RelayEntry, extra: usize) -> Option<(u8, bool)> {
    let st = core.live.relay_load.get(&relay.key())?;
    let l = st.load.as_ref()?;
    let sessions = l.sessions.max(members_on(core, relay)) + extra;
    let rooms = l.rooms.max(busy(core, relay)) + usize::from(extra > 0);
    let frac = |n: usize, cap: usize| if cap == 0 { 0 } else { (n * 100).div_ceil(cap) };
    let pct = frac(sessions, l.max_sessions)
        .max(frac(rooms, l.max_rooms))
        .max(l.mean_busy() as usize);
    let fits = (l.max_sessions == 0 || sessions <= l.max_sessions) && (l.max_rooms == 0 || rooms <= l.max_rooms);
    Some((pct.min(255) as u8, fits))
}

/// What `relay` offers a new match of `members` members.
pub fn room_on(core: &Core, relay: &RelayEntry, members: usize) -> Room {
    if core.cfg.relay_load.status_s == 0 || core.relay_keys.of(relay).is_none() {
        return Room::Unknown;
    }
    let Some(st) = core.live.relay_load.get(&relay.key()) else {
        return Room::Unknown;
    };
    if !st.ever {
        return Room::Unknown;
    }
    if st.misses >= SILENT_AFTER {
        return Room::Silent;
    }
    match load_percent(core, relay, members) {
        None => Room::Unknown,
        Some((_, false)) => Room::Full,
        Some(_) if st.full => Room::Full,
        Some((pct, true)) if pct >= core.cfg.relay_load.full_at_percent => Room::Full,
        Some((pct, true)) => Room::Free(100u8.saturating_sub(pct)),
    }
}

/// Why no relay was chosen.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NoRelay {
    /// No `[[relays]]` at all.
    None,
    /// Every relay that could take the match is full (1.1.13: the error `relay_capacity`).
    Capacity,
}

/// The relay of a new match between the `players` (never its spectators): among the relays that answer (all of them
/// when none does), by `server.relay_selection`. `None` without any relay, or when every relay is full.
pub fn pick(core: &Core, players: &[String]) -> Option<RelayEntry> {
    choose(core, players, players.len()).ok()
}

/// The slowest counted player's round trip to each of `candidates` (1.1.9), `None` where nobody's counts.
///
/// A launcher's declaration (`launcher.relay_rtt_ms`) counts only when it names every candidate relay and reached at
/// least one: a relay it leaves out is unknown, not unreachable, and a partial declaration says nothing. Values are
/// bounded (1 ms to [`RTT_MAX_MS`]); `null` is a relay the launcher could not reach.
fn slowest(core: &Core, players: &[String], candidates: &[&RelayEntry]) -> Vec<Option<u32>> {
    let mut worst: Vec<Option<u32>> = vec![None; candidates.len()];
    for u in players {
        let rtt = crate::api::account::game_caps(core, u).relay_rtt;
        let Some(values) = candidates
            .iter()
            .map(|r| rtt.get(&r.key()).copied())
            .collect::<Option<Vec<Option<u32>>>>()
        else {
            continue;
        };
        if values.iter().all(Option::is_none) {
            continue;
        }
        for (i, v) in values.into_iter().enumerate() {
            let ms = v.map_or(UNREACHABLE_MS, |ms| ms.clamp(1, RTT_MAX_MS));
            worst[i] = Some(worst[i].map_or(ms, |w| w.max(ms)));
        }
    }
    worst
}

/// [`pick`] with the reason when there is none; `members` is the match's size (players and spectators), which must
/// fit on the relay.
///
/// The rule (`relay.md` 4.3):
/// 1. the relays the reachability check does not hold for down (all of them when it holds every one);
/// 2. of them, those with room for the match, and those that announce nothing; when there are none, the silent
///    ones (a relay that stopped answering is used when nothing else is left); when there are none
///    either, every relay is full: [`NoRelay::Capacity`];
/// 3. `latency`: those whose slowest counted player is within `relay_load.latency_tolerance_ms` of the best one;
/// 4. the one with the most room left (in steps of 10 %; one that announces nothing counts as half full), then the
///    fewer matches under way, then the configuration's order.
///
/// When no candidate announces its load, steps 3 and 4 are the rule of 1.1.9 to 1.1.12: latencies in steps of 25 ms,
/// then the fewer matches under way, then the configuration's order (`least_busy` without the latencies, `first`
/// the first one).
pub fn choose(core: &Core, players: &[String], members: usize) -> Result<RelayEntry, NoRelay> {
    let all = &core.cfg.relays;
    if all.is_empty() {
        return Err(NoRelay::None);
    }
    let up: Vec<&RelayEntry> = all.iter().filter(|r| !is_down(core, r)).collect();
    let reachable: Vec<&RelayEntry> = if up.is_empty() { all.iter().collect() } else { up };
    let rooms: Vec<Room> = reachable.iter().map(|r| room_on(core, r, members)).collect();
    let mut candidates: Vec<(&RelayEntry, Room)> = reachable
        .iter()
        .zip(&rooms)
        .filter(|(_, x)| matches!(x, Room::Free(_) | Room::Unknown))
        .map(|(r, x)| (*r, *x))
        .collect();
    if candidates.is_empty() {
        candidates = reachable
            .iter()
            .zip(&rooms)
            .filter(|(_, x)| **x == Room::Silent)
            .map(|(r, x)| (*r, *x))
            .collect();
    }
    if candidates.is_empty() {
        return Err(NoRelay::Capacity);
    }
    let mode = core.cfg.server.relay_selection.as_str();
    if candidates.len() == 1 || mode == "first" {
        return Ok(candidates[0].0.clone());
    }
    let relays: Vec<&RelayEntry> = candidates.iter().map(|(r, _)| *r).collect();
    let worst = if mode == "latency" {
        slowest(core, players, &relays)
    } else {
        vec![None; relays.len()]
    };
    let announced = candidates.iter().any(|(_, x)| matches!(x, Room::Free(_)));
    if !announced {
        // Close latencies (25 ms steps) are a tie: the less busy relay then wins, then the configuration's order.
        return Ok(relays
            .iter()
            .enumerate()
            .min_by_key(|(i, r)| (worst[*i].map_or(0, |ms| ms / 25), busy(core, r), *i))
            .map(|(_, r)| (*r).clone())
            .expect("a candidate"));
    }
    let best = worst.iter().flatten().min().copied();
    let tolerance = core.cfg.relay_load.latency_tolerance_ms;
    let acceptable = |i: usize| match (worst[i], best) {
        (Some(w), Some(b)) => w <= b.saturating_add(tolerance),
        _ => true,
    };
    let room_step = |x: Room| match x {
        Room::Free(p) => p / 10,
        _ => 5,
    };
    Ok(candidates
        .iter()
        .enumerate()
        .filter(|(i, _)| acceptable(*i))
        .min_by_key(|(i, (r, x))| (std::cmp::Reverse(room_step(*x)), busy(core, r), *i))
        .map(|(_, (r, _))| (*r).clone())
        .expect("the best relay is acceptable"))
}

// ---- load --------------------------------------------------------------------------------------------------------

/// Where the service asks `r` for its load: the relay this service runs on this machine (no detour through a public
/// address), any other at its host.
fn status_target(cfg: &Config, r: &RelayEntry) -> (String, u16) {
    let er = &cfg.embedded_relay;
    if er.enabled && cfg.names_embedded(r) {
        let local = match er.bind.parse::<IpAddr>() {
            Ok(ip) if !ip.is_unspecified() => ip.to_string(),
            _ => "127.0.0.1".to_string(),
        };
        return (local, er.port);
    }
    (r.host.clone(), r.port)
}

/// One question: the relay's load, or `None` (no IPv4 address, no answer within two seconds, or not its answer).
pub async fn ask(host: &str, port: u16, secret: &RoomSecret) -> Option<kalamar_relay::status::Load> {
    let mut addrs = tokio::time::timeout(Duration::from_secs(2), tokio::net::lookup_host((host, port)))
        .await
        .ok()?
        .ok()?;
    let addr = addrs.find(SocketAddr::is_ipv4)?;
    kalamar_relay::client::status(addr, secret, Duration::from_secs(2)).await
}

/// Records one answer (or its absence) of the relay `key`; the log says when it fills up, has room again, stops or
/// starts answering.
pub fn record_load(core: &mut Core, key: &str, answer: Option<kalamar_relay::status::Load>) {
    let (full_at, resume_at) = (
        core.cfg.relay_load.full_at_percent,
        core.cfg.relay_load.resume_at_percent,
    );
    let now = core.now;
    let relay = core.cfg.relays.iter().find(|r| r.key() == key).cloned();
    let st = core.live.relay_load.entry(key.to_string()).or_default();
    match answer {
        None => {
            st.misses = st.misses.saturating_add(1);
            if st.ever && st.misses == SILENT_AFTER {
                tracing::warn!(relay = %key, "relay does not answer the service's status questions: no new match goes there while another has room");
            }
            return;
        }
        Some(l) => {
            if st.ever && st.misses >= SILENT_AFTER {
                tracing::info!(relay = %key, "relay answers the status questions again");
            }
            st.ever = true;
            st.misses = 0;
            st.at = now;
            st.load = Some(l);
        }
    }
    let Some(relay) = relay else { return };
    let Some((pct, _)) = load_percent(core, &relay, 0) else {
        return;
    };
    let st = core.live.relay_load.get_mut(key).expect("just recorded");
    if !st.full && pct >= full_at {
        st.full = true;
        tracing::warn!(relay = %key, load = pct, "relay full: no new match goes there until it is back under {resume_at} %");
    } else if st.full && pct <= resume_at {
        st.full = false;
        tracing::info!(relay = %key, load = pct, "relay has room again");
    }
}

/// Asks every bound relay for its load every `relay_load.status_s` seconds (a single relay too: its load decides
/// whether a match can start). Follows configuration reloads.
async fn status_loop(st: AppState) {
    loop {
        let cfg = st.cfg();
        let every = cfg.relay_load.status_s;
        if every == 0 {
            st.lock().live.relay_load.clear();
            tokio::time::sleep(Duration::from_secs(10)).await;
            continue;
        }
        let keys = st.lock().relay_keys.clone();
        let mut asks = Vec::new();
        for r in &cfg.relays {
            let Some(secret) = keys.of(r).cloned() else { continue };
            let (host, port) = status_target(&cfg, r);
            let key = r.key();
            asks.push(tokio::spawn(async move { (key, ask(&host, port, &secret).await) }));
        }
        for a in asks {
            if let Ok((key, answer)) = a.await {
                record_load(&mut st.lock(), &key, answer);
            }
        }
        // Relays removed by a reload, or no longer bound, are forgotten.
        let bound: Vec<String> = cfg
            .relays
            .iter()
            .filter(|r| keys.of(r).is_some())
            .map(RelayEntry::key)
            .collect();
        st.lock().live.relay_load.retain(|k, _| bound.contains(k));
        tokio::time::sleep(Duration::from_secs(every)).await;
    }
}

/// `admin relays`: each relay as the service sees it now.
pub fn report(core: &Core) -> String {
    let mut out = String::new();
    let rl = &core.cfg.relay_load;
    for r in &core.cfg.relays {
        let bound = core.relay_keys.of(r).is_some();
        out.push_str(&format!(
            "{} ({}){}{}\n",
            r.key(),
            r.addr(),
            r.region.as_ref().map(|g| format!(", region {g}")).unwrap_or_default(),
            if bound {
                ", bound to this service"
            } else {
                ", open (announces nothing)"
            },
        ));
        out.push_str(&format!(
            "  matches under way {}, their members {}; reachability: {}\n",
            busy(core, r),
            members_on(core, r),
            match core.live.relay_fails.get(&r.addr()) {
                Some(n) if *n >= DOWN_AFTER => format!("down ({n} checks missed)"),
                Some(n) => format!("{n} check(s) missed"),
                None => "answers (or not checked: one relay, or relay_probe_s = 0)".into(),
            }
        ));
        let state = match room_on(core, r, 0) {
            Room::Unknown => "announces nothing: chosen without its load".to_string(),
            Room::Free(p) => format!("has room ({p} % left)"),
            Room::Full => format!("FULL: no new match until it is back under {} %", rl.resume_at_percent),
            Room::Silent => "SILENT: stopped answering, no new match while another has room".into(),
        };
        out.push_str(&format!("  state: {state}\n"));
        match core
            .live
            .relay_load
            .get(&r.key())
            .and_then(|s| s.load.as_ref().map(|l| (s, l)))
        {
            Some((s, l)) => {
                let cap = |n: usize| if n == 0 { "no cap".to_string() } else { n.to_string() };
                let busy: Vec<String> = l.worker_busy.iter().map(|b| format!("{b}%")).collect();
                out.push_str(&format!(
                    "  sessions {} / {}, rooms {} / {}, {} worker(s), load {} (mean {} %)\n",
                    l.sessions,
                    cap(l.max_sessions),
                    l.rooms,
                    cap(l.max_rooms),
                    l.workers,
                    busy.join(" "),
                    l.mean_busy()
                ));
                out.push_str(&format!(
                    "  over {} s: received {}/s ({:.1} Mbit/s), sent {}/s ({:.1} Mbit/s), dropped {}/s, JOINs refused {} ({} for want of room); answered {} s ago, up {} s\n",
                    l.window_s,
                    l.rx_pps,
                    l.rx_bps as f64 * 8.0 / 1e6,
                    l.tx_pps,
                    l.tx_bps as f64 * 8.0 / 1e6,
                    l.dropped_ps,
                    l.refused,
                    l.refused_full,
                    (core.now - s.at).max(0),
                    l.uptime_s
                ));
            }
            None if bound && rl.status_s == 0 => out.push_str("  load: not asked (relay_load.status_s = 0)\n"),
            None if bound => out.push_str("  load: no answer yet (some relays never answer the query)\n"),
            None => {}
        }
    }
    if out.is_empty() {
        out.push_str("no [[relays]]\n");
    }
    out
}

/// Serves the `admin relays` requests left in the database (called by the ticker every second).
pub fn serve_admin(st: &AppState) {
    let core = st.lock();
    let Ok(mut q) = core
        .db
        .prepare_cached("SELECT id FROM control WHERE kind = 'relays' AND done_at IS NULL")
    else {
        return;
    };
    let ids: Vec<i64> = q
        .query_map([], |r| r.get(0))
        .map(|rows| rows.flatten().collect())
        .unwrap_or_default();
    drop(q);
    if ids.is_empty() {
        return;
    }
    let text = report(&core);
    for id in ids {
        let _ = core.db.execute(
            "UPDATE control SET done_at = ?1, ok = 1, detail = ?2 WHERE id = ?3",
            rusqlite::params![core.now, text, id],
        );
    }
}

// ---- reachability ------------------------------------------------------------------------------------------------

/// One reachability check: a relay protocol HELLO, and its CHALLENGE within two seconds (`kalamar_relay::client`).
/// Nothing is joined.
pub async fn probe(host: &str, port: u16) -> bool {
    let Ok(Ok(mut addrs)) = tokio::time::timeout(Duration::from_secs(2), tokio::net::lookup_host((host, port))).await
    else {
        return false;
    };
    let Some(addr) = addrs.find(SocketAddr::is_ipv4) else {
        return false;
    };
    kalamar_relay::client::hello_rtt(addr, Duration::from_secs(2))
        .await
        .is_some()
}

/// Records the result of one check; says whether the relay just went down or came back.
pub fn record(core: &mut Core, addr: &str, ok: bool) -> Option<bool> {
    let before = core.live.relay_fails.get(addr).copied().unwrap_or(0);
    let after = if ok { 0 } else { before.saturating_add(1) };
    if after == 0 {
        core.live.relay_fails.remove(addr);
    } else {
        core.live.relay_fails.insert(addr.to_string(), after);
    }
    match (before >= DOWN_AFTER, after >= DOWN_AFTER) {
        (false, true) => Some(false),
        (true, false) => Some(true),
        _ => None,
    }
}

/// Aborts a task when dropped (the status questions end with the reachability check that started them).
struct AbortOnDrop(tokio::task::JoinHandle<()>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

/// Checks every relay every `server.relay_probe_s` while there are several (a single relay is used whatever it
/// answers), and asks the bound relays for their load (1.1.13, `[relay_load]`). Follows configuration reloads.
pub fn spawn_probe(st: AppState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let _status = AbortOnDrop(tokio::spawn(status_loop(st.clone())));
        loop {
            let cfg = st.cfg();
            let every = cfg.server.relay_probe_s;
            if every == 0 || cfg.relays.len() < 2 {
                st.lock().live.relay_fails.clear();
                tokio::time::sleep(Duration::from_secs(10)).await;
                continue;
            }
            for r in &cfg.relays {
                let ok = probe(&r.host, r.port).await;
                match record(&mut st.lock(), &r.addr(), ok) {
                    Some(false) => tracing::warn!(relay = %r.key(), "relay does not answer: no new match goes there"),
                    Some(true) => tracing::info!(relay = %r.key(), "relay answers again"),
                    None => {}
                }
            }
            // Relays removed by a reload are forgotten.
            let known: Vec<String> = cfg.relays.iter().map(RelayEntry::addr).collect();
            st.lock().live.relay_fails.retain(|a, _| known.contains(a));
            tokio::time::sleep(Duration::from_secs(every)).await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg(extra: &str) -> Config {
        let text = format!(
            r#"
[server]
name = "T"
api_url = "https://play.example.org/api"
privacy_url = "https://example.invalid/privacy"
operator = {{ name = "T", contact = "mailto:t@example.invalid" }}
[features]
rooms = true
queues = false
parties = false
ratings = false
leaderboards = false
[auth.guest]
enabled = true
{extra}
"#
        );
        Config::parse(&text).unwrap_or_else(|e| panic!("{e}\n{text}"))
    }

    fn temp(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!("kalamar-relays-{}", crate::util::new_id("t")));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join(name)
    }

    /// The service seals with the relay's own code (`roomauth`): the shared test vector of relay.md 4.2.
    #[test]
    fn rooms_of_a_bound_relay_are_sealed_with_the_relays_code() {
        let file = temp("relay.key");
        std::fs::write(
            &file,
            format!("{}\n", crate::util::hex(b"0123456789abcdef0123456789abcdef")),
        )
        .unwrap();
        let c = cfg(&format!(
            "[[relays]]\nhost = \"relay.example.org\"\nsecret_file = {:?}\n[[relays]]\nhost = \"open.example.org\"\n",
            file
        ));
        let keys = load_keys(&c, None).unwrap();
        let mut core = Core::new(
            std::sync::Arc::new(c.clone()),
            crate::db::open(std::path::Path::new(":memory:")).unwrap(),
            vec![0; 32],
            0,
        );
        core.relay_keys = std::sync::Arc::new(keys);
        let day = 1_794_441_600;
        let room = room_for(&core, &c.relays[0], "K7Q2MXA9P84DZB3W8R6TNY5HCF", day).unwrap();
        assert_eq!(room, "K7Q2MXA9P84DZM91NYAXD5NJAJ56RNH3");
        assert!(kalamar_protocol::contract::is_valid_relay_room(&room));
        assert_eq!(
            room_for(&core, &c.relays[0], &room, day + 3600).unwrap(),
            room,
            "a session keeps its room"
        );
        assert_eq!(
            room_for(&core, &c.relays[1], "K7Q2MXA9P84DZB3W8R6TNY5HCF", day).unwrap(),
            "K7Q2MXA9P84DZB3W8R6TNY5HCF",
            "an open relay keeps the code"
        );
        // The secret is in memory: a file changed or removed afterwards changes nothing until a reload.
        std::fs::remove_file(&file).unwrap();
        assert_eq!(
            room_for(&core, &c.relays[0], "K7Q2MXA9P84DZB3W8R6TNY5HCF", day).unwrap(),
            room
        );
        let err = load_keys(&c, None).unwrap_err().join("\n");
        assert!(
            err.contains("relays.relay.example.org:47313: secret") && err.contains("relay.key"),
            "{err}"
        );
    }

    #[test]
    fn the_embedded_relay_and_its_entries_share_one_value() {
        let file = temp("relay.key");
        let c = cfg(&format!(
            "[embedded_relay]\nenabled = true\nsecret_file = {:?}\n[[relays]]\nhost = \"play.example.org\"\n",
            file
        ));
        // Derived from [embedded_relay]: no second entry of the path needed.
        assert!(c.relays[0].secret_file.is_some());
        let keys = prepare(&c).unwrap();
        let e = keys.embedded.clone().expect("created at the first start");
        assert!(file.exists());
        assert!(keys.of(&c.relays[0]).unwrap().is_same_allocation(&e));
        // Read again (second start): the same value.
        let again = prepare(&c).unwrap();
        assert_eq!(again.embedded.unwrap(), e);
        // An unreadable secret stops the start with the file's name.
        std::fs::write(&file, "not hex").unwrap();
        let err = prepare(&c).unwrap_err();
        assert!(
            err.contains("embedded_relay.secret_file") && err.contains("relay.key"),
            "{err}"
        );
    }

    #[test]
    fn relays_sharing_a_secret_are_named() {
        let a = temp("a.key");
        let b = temp("b.key");
        let hex = crate::util::hex(&[7u8; 32]);
        std::fs::write(&a, &hex).unwrap();
        std::fs::write(&b, &hex).unwrap();
        let c = cfg(&format!(
            "[[relays]]\nhost = \"eu.example.org\"\nid = \"eu\"\nsecret_file = {a:?}\n[[relays]]\nhost = \"us.example.org\"\nid = \"us\"\nsecret_file = {b:?}\n[[relays]]\nhost = \"asia.example.org\"\nid = \"asia\"\n"
        ));
        let keys = load_keys(&c, None).unwrap();
        assert_eq!(shared_secrets(&c, &keys), vec![("eu".to_string(), "us".to_string())]);
        std::fs::write(&b, crate::util::hex(&[8u8; 32])).unwrap();
        assert!(shared_secrets(&c, &load_keys(&c, None).unwrap()).is_empty());
    }

    // ---- 1.1.13: the load of the relays ---------------------------------------------------------------------------

    /// A core with two relays bound to it (`eu`, `us`, each its own secret) and an open one (`open`), and two players
    /// who measured `eu` and `us` (20 and 40 ms, 25 and 45 ms: `eu` is 20 ms faster for the slowest).
    fn bound_core(extra: &str) -> (Core, Vec<String>) {
        let (a, b) = (temp("eu.key"), temp("us.key"));
        std::fs::write(&a, crate::util::hex(&[1u8; 32])).unwrap();
        std::fs::write(&b, crate::util::hex(&[2u8; 32])).unwrap();
        let c = cfg(&format!(
            "[[relays]]\nhost = \"eu.example.org\"\nid = \"eu\"\nsecret_file = {a:?}\n[[relays]]\nhost = \"us.example.org\"\nid = \"us\"\nsecret_file = {b:?}\n{extra}"
        ));
        let keys = load_keys(&c, None).unwrap();
        let mut core = Core::new(
            std::sync::Arc::new(c),
            crate::db::open(std::path::Path::new(":memory:")).unwrap(),
            vec![0; 32],
            0,
        );
        core.relay_keys = std::sync::Arc::new(keys);
        let mut players = Vec::new();
        for (id, eu, us) in [("u_a", 20, 40), ("u_b", 25, 45)] {
            let caps = json!({"launcher": {"relay_rtt_ms": {"eu": eu, "us": us}}}).to_string();
            core.db
                .execute(
                    "INSERT INTO users (id, display_name, friend_code, created_at, game_caps) VALUES (?1, ?1, ?1, 1, ?2)",
                    rusqlite::params![id, caps],
                )
                .unwrap();
            players.push(id.to_string());
        }
        (core, players)
    }

    fn load(sessions: usize, max_sessions: usize, busy: u8) -> kalamar_relay::status::Load {
        kalamar_relay::status::Load {
            workers: 2,
            window_s: 5,
            sessions,
            rooms: sessions / 2,
            max_sessions,
            max_rooms: 0,
            max_members: 12,
            worker_busy: vec![busy, busy],
            ..Default::default()
        }
    }

    fn chosen(core: &Core, players: &[String]) -> Result<String, NoRelay> {
        choose(core, players, players.len()).map(|r| r.key())
    }

    /// Among the relays close enough to the fastest one, the one with the most room; a full one never while another
    /// has room; none when all are full.
    #[test]
    fn a_new_match_goes_where_there_is_room() {
        let (mut core, players) = bound_core("");
        // Nothing announced yet: the rule of 1.1.9 (eu, 20 ms faster for the slowest player).
        assert_eq!(chosen(&core, &players), Ok("eu".into()));
        assert_eq!(
            (
                room_on(&core, &core.cfg.relays[0], 2),
                room_on(&core, &core.cfg.relays[1], 2)
            ),
            (Room::Unknown, Room::Unknown)
        );
        // eu 59 % loaded, us 10 %: us is within 30 ms of eu (45 against 25) and has more room.
        record_load(&mut core, "eu", Some(load(600, 1024, 20)));
        record_load(&mut core, "us", Some(load(100, 1024, 5)));
        assert_eq!(room_on(&core, &core.cfg.relays[1], 2), Room::Free(90));
        assert_eq!(chosen(&core, &players), Ok("us".into()));
        // With a tolerance of 10 ms, us is too slow: eu, which has room.
        std::sync::Arc::make_mut(&mut core.cfg).relay_load.latency_tolerance_ms = 10;
        assert_eq!(chosen(&core, &players), Ok("eu".into()));
        // eu full (its workers at 95 %): us, whatever the latency.
        record_load(&mut core, "eu", Some(load(600, 1024, 95)));
        assert_eq!(room_on(&core, &core.cfg.relays[0], 2), Room::Full);
        assert_eq!(chosen(&core, &players), Ok("us".into()));
        // Both full: no relay, the error relay_capacity.
        record_load(&mut core, "us", Some(load(1000, 1024, 5)));
        assert_eq!(chosen(&core, &players), Err(NoRelay::Capacity));
        assert!(pick(&core, &players).is_none());
        // An open relay (it announces nothing) is chosen without its load.
        let (mut core, players) = bound_core("[[relays]]\nhost = \"open.example.org\"\nid = \"open\"\n");
        record_load(&mut core, "eu", Some(load(1000, 1024, 5)));
        record_load(&mut core, "us", Some(load(1000, 1024, 5)));
        assert_eq!(chosen(&core, &players), Ok("open".into()));
    }

    /// A relay that answered and stopped answering gets no new match while another has room; when only silent relays
    /// are left, they are used.
    #[test]
    fn a_silent_relay_is_avoided_while_another_has_room() {
        let (mut core, players) = bound_core("");
        record_load(&mut core, "eu", Some(load(10, 1024, 1)));
        record_load(&mut core, "us", Some(load(900, 1024, 1)));
        assert_eq!(chosen(&core, &players), Ok("eu".into()));
        for _ in 0..SILENT_AFTER {
            record_load(&mut core, "eu", None);
        }
        assert_eq!(room_on(&core, &core.cfg.relays[0], 2), Room::Silent);
        assert_eq!(chosen(&core, &players), Ok("us".into()), "us still has room (88 %)");
        record_load(&mut core, "us", Some(load(1000, 1024, 1)));
        assert_eq!(chosen(&core, &players), Ok("eu".into()), "only the silent one is left");
        record_load(&mut core, "eu", Some(load(10, 1024, 1)));
        assert_eq!(room_on(&core, &core.cfg.relays[0], 2), Room::Free(98));
        // A relay that never answered is not silent: it announces nothing.
        let (mut core, players) = bound_core("");
        for _ in 0..10 {
            record_load(&mut core, "eu", None);
        }
        assert_eq!(room_on(&core, &core.cfg.relays[0], 2), Room::Unknown);
        assert_eq!(chosen(&core, &players), Ok("eu".into()));
    }

    /// A full relay stays full until its load is back at `resume_at_percent`: it does not take and refuse matches in
    /// turn around the mark.
    #[test]
    fn a_full_relay_stays_full_until_it_is_back_under_the_resume_mark() {
        let (mut core, _) = bound_core("");
        let eu = core.cfg.relays[0].clone();
        record_load(&mut core, "eu", Some(load(0, 1000, 92)));
        assert!(core.live.relay_load["eu"].full);
        record_load(&mut core, "eu", Some(load(0, 1000, 80)));
        assert_eq!(room_on(&core, &eu, 2), Room::Full, "80 %: still full");
        record_load(&mut core, "eu", Some(load(0, 1000, 75)));
        assert_eq!(room_on(&core, &eu, 2), Room::Free(25));
        record_load(&mut core, "eu", Some(load(0, 1000, 89)));
        assert_eq!(room_on(&core, &eu, 2), Room::Free(11), "under 90 %: room again");
        // The match being placed counts: 1018 of 1024 sessions, a match of 8 does not fit, a match of 2 does (99 %
        // is still full: the mark is reached).
        record_load(&mut core, "eu", Some(load(1018, 1024, 1)));
        assert_eq!(room_on(&core, &eu, 8), Room::Full);
        assert_eq!(load_percent(&core, &eu, 2), Some((100, true)));
        // No cap (0): only the workers' load counts.
        record_load(&mut core, "eu", Some(load(100_000, 0, 30)));
        assert_eq!(room_on(&core, &eu, 8), Room::Free(70));
    }

    /// The status questions are off with `status_s = 0`: the relays are chosen as in 1.1.12, whatever was recorded.
    #[test]
    fn without_status_questions_the_choice_is_the_one_of_1_1_12() {
        let (mut core, players) = bound_core("[relay_load]\nstatus_s = 0\n");
        record_load(&mut core, "eu", Some(load(1024, 1024, 100)));
        assert_eq!(room_on(&core, &core.cfg.relays[0], 2), Room::Unknown);
        assert_eq!(chosen(&core, &players), Ok("eu".into()));
        let text = report(&core);
        assert!(
            text.contains("eu (eu.example.org:47313), bound to this service"),
            "{text}"
        );
    }
}
