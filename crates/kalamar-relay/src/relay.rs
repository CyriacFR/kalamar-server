//! The relay state machine: no sockets, no clock, no logging sink of its own.
//!
//! [`Relay::handle`] takes one received datagram (with the current monotonic and unix time) and appends the
//! datagrams to send to an [`Outbox`]; [`Relay::tick`] does the periodic work. The server in `server.rs` runs it on
//! one or several worker threads, and the tests drive it directly with simulated clients and a simulated clock.
//!
//! Security properties, and where they are enforced:
//! - HELLO keeps no state at all: the CHALLENGE is computed from the datagram and the cookie secret.
//! - A JOIN creates state only after its cookie verified (return routability of the source address).
//! - Before that, the only datagrams sent are CHALLENGE (32), JOIN_NAK (24) and an at-most-once-per-second CLOSED
//!   (24), each answering a request at least as large (`handle` checks sizes through the parser, and `reply`
//!   asserts the invariant in debug builds).
//! - Everything else goes to a member's own address, learned from a verified JOIN or an authenticated KEEPALIVE.
//! - Payloads are copied between datagrams and never inspected or logged.
//!
//! Several workers (`server.rs`) call [`Relay::handle_on`] at the same time. What they share, and how:
//! - a room is one `Mutex<Room>`: its members, their tokens, addresses and rate buckets. A `DATA` datagram takes the
//!   lock of its own room only, and only to check the sender and copy the destinations (the datagrams are built
//!   after it is released); two rooms never wait for each other;
//! - the session map (session id to room) is split into [`SHARDS`] read-write locks, read once per datagram;
//! - everything that changes who is where (JOIN, LEAVE, an address change, a timeout, the shutdown) runs under one
//!   admission lock, so that the limits (rooms, sessions, members, per source) and the last free place of a room are
//!   decided for one JOIN at a time. A JOIN is rare (the HELLO and JOIN rates are limited per address); a datagram of
//!   a running match never takes this lock;
//! - the counters are per worker (no shared cache line on the hot path), summed when read.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex, MutexGuard, RwLock};
use std::time::{Duration, Instant};

use subtle::ConstantTimeEq;
use tracing::{debug, info};

use crate::cookie::CookieKeys;
use crate::ratelimit::{SlotCounter, SlotTimes, TokenBucket};
use crate::status::{self, Load, StatusKey};
use crate::wire::{self, closed, nak, Auth, JoinAck, Msg, ParseError};

/// Session-map shards (a power of two).
pub const SHARDS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelayConfig {
    /// Rooms and sessions at the same time; 0: no cap (memory is still bounded by the per-source limits and the
    /// HELLO and JOIN rates, `relay.md` 6).
    pub max_rooms: usize,
    pub max_sessions: usize,
    /// Members per room, 2..=254.
    pub max_members: usize,
    /// HELLO and JOIN per source IP address per second (each); 0 = unlimited.
    pub hello_per_s: u32,
    pub join_per_s: u32,
    /// DATA per member: datagrams and bytes per second; 0 = unlimited.
    pub member_pps: u32,
    pub member_bytes_per_s: u32,
    /// DATA per room (all members together, counted on receipt, before fan-out).
    pub room_pps: u32,
    pub room_bytes_per_s: u32,
    pub keepalive_s: u8,
    pub idle_s: u8,
    pub cookie_rotation: Duration,
    /// With a secret, only room codes its holder issued can be joined (`roomauth.rs`), and the holder may ask for the
    /// relay's load (`status.rs`); `None`: an open relay.
    pub room_secret: Option<crate::roomauth::RoomSecret>,
    /// Sessions of one source, all rooms together; 0 = unlimited. A source is an IPv4 address or an IPv6 /64
    /// ([`source_of`]); loopback is never limited (local tests). Security review M4: one address held every session
    /// of a relay (1024) with codes any two accounts could mint, and every real match got RELAY_FULL.
    pub max_sessions_per_source: usize,
    /// Members of one room from one source; 0 = unlimited. The default 8 lets a whole 4 v 4 (or a LAN party) play
    /// from behind one box with no setting. With 12 members per room, one source can no longer take a whole room:
    /// 4 places stay for the others, enough for a duel or a 2 v 2 whatever one participant does; in a larger match a
    /// participant who fills 8 places can still keep the others out, which only voids a match that participant could
    /// void anyway (with an error report).
    pub max_members_per_source: usize,
}

impl Default for RelayConfig {
    fn default() -> Self {
        RelayConfig {
            max_rooms: 256,
            max_sessions: 1024,
            max_members: 12,
            hello_per_s: 20,
            join_per_s: 20,
            member_pps: 2000,
            member_bytes_per_s: 4 << 20,
            room_pps: 12_000,
            room_bytes_per_s: 16 << 20,
            keepalive_s: 5,
            idle_s: 30,
            cookie_rotation: Duration::from_secs(3600),
            room_secret: None,
            max_sessions_per_source: 24,
            max_members_per_source: 8,
        }
    }
}

impl RelayConfig {
    pub fn validate(&self) -> Result<(), String> {
        if !(2..=254).contains(&self.max_members) {
            return Err("members per room must be 2..=254".into());
        }
        if !(1..=60).contains(&self.keepalive_s) {
            return Err("keepalive interval must be 1..=60 seconds".into());
        }
        if (self.idle_s as u32) < 3 * self.keepalive_s as u32 {
            return Err("idle timeout must be at least 3 times the keepalive interval".into());
        }
        if self.member_bytes_per_s != 0 && (self.member_bytes_per_s as usize) < wire::MAX_DATAGRAM
            || self.room_bytes_per_s != 0 && (self.room_bytes_per_s as usize) < wire::MAX_DATAGRAM
        {
            return Err("a bytes-per-second limit must be 0 (off) or at least 1472".into());
        }
        if self.cookie_rotation < crate::cookie::MIN_ROTATION {
            return Err("cookie secret rotation must be at least 60 seconds".into());
        }
        Ok(())
    }
}

/// Datagrams to send, packed in one buffer (no allocation per datagram).
#[derive(Default)]
pub struct Outbox {
    buf: Vec<u8>,
    items: Vec<(SocketAddr, usize, usize)>,
    /// The destinations of one DATA datagram, copied under the room's lock (kept between calls: no allocation).
    dests: Vec<(SocketAddr, Auth)>,
}

impl Outbox {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn clear(&mut self) {
        self.buf.clear();
        self.items.clear();
    }
    pub fn len(&self) -> usize {
        self.items.len()
    }
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
    pub fn iter(&self) -> impl Iterator<Item = (SocketAddr, &[u8])> + '_ {
        self.items.iter().map(|&(a, s, e)| (a, &self.buf[s..e]))
    }
    fn push(&mut self, to: SocketAddr, f: impl FnOnce(&mut Vec<u8>)) -> usize {
        let start = self.buf.len();
        f(&mut self.buf);
        let end = self.buf.len();
        debug_assert!(end - start <= wire::MAX_DATAGRAM);
        self.items.push((to, start, end));
        end - start
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum DropReason {
    /// Not an S3LR datagram, too short, too large, unknown type, source port 0.
    Invalid,
    OtherVersion,
    WrongDirection,
    RateHello,
    RateJoin,
    UnknownSession,
    WrongAddress,
    RebindConflict,
    SpoofedSource,
    RateMember,
    RateRoom,
    NoDestination,
    /// A status query that is not from this relay's service (wrong tag, too old, replayed, too frequent).
    Status,
}

impl DropReason {
    pub const ALL: [DropReason; 13] = [
        DropReason::Invalid,
        DropReason::OtherVersion,
        DropReason::WrongDirection,
        DropReason::RateHello,
        DropReason::RateJoin,
        DropReason::UnknownSession,
        DropReason::WrongAddress,
        DropReason::RebindConflict,
        DropReason::SpoofedSource,
        DropReason::RateMember,
        DropReason::RateRoom,
        DropReason::NoDestination,
        DropReason::Status,
    ];
    pub fn name(self) -> &'static str {
        match self {
            DropReason::Invalid => "invalid",
            DropReason::OtherVersion => "version",
            DropReason::WrongDirection => "direction",
            DropReason::RateHello => "rate_hello",
            DropReason::RateJoin => "rate_join",
            DropReason::UnknownSession => "unknown_session",
            DropReason::WrongAddress => "wrong_addr",
            DropReason::RebindConflict => "rebind_conflict",
            DropReason::SpoofedSource => "spoofed_src",
            DropReason::RateMember => "rate_member",
            DropReason::RateRoom => "rate_room",
            DropReason::NoDestination => "no_dest",
            DropReason::Status => "status",
        }
    }
}

const DROPS: usize = DropReason::ALL.len();

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Stats {
    pub rx: u64,
    pub tx: u64,
    /// Bytes of the datagrams received and sent (UDP payloads).
    pub rx_bytes: u64,
    pub tx_bytes: u64,
    pub forwarded: u64,
    pub joins: u64,
    pub leaves: u64,
    pub timeouts: u64,
    pub refused: u64,
    /// Of `refused`: JOINs refused for want of room (`RELAY_FULL`, and `ROOM_FULL` for a full room or a source's
    /// share of it).
    pub refused_full: u64,
    pub dropped: [u64; DROPS],
}

impl Stats {
    pub fn dropped_total(&self) -> u64 {
        self.dropped.iter().sum()
    }
}

/// The counters of one worker, on a cache line of their own (`stats` sums them).
#[repr(align(128))]
#[derive(Default)]
struct Counters {
    rx: AtomicU64,
    tx: AtomicU64,
    rx_bytes: AtomicU64,
    tx_bytes: AtomicU64,
    forwarded: AtomicU64,
    joins: AtomicU64,
    leaves: AtomicU64,
    timeouts: AtomicU64,
    refused: AtomicU64,
    refused_full: AtomicU64,
    dropped: [AtomicU64; DROPS],
    /// Nanoseconds this worker spent handling datagrams and sending (`server.rs`), for its load.
    busy_ns: AtomicU64,
}

fn add(c: &AtomicU64, n: u64) {
    c.fetch_add(n, Relaxed);
}

struct Member {
    session: u32,
    token: [u8; wire::TOKEN_LEN],
    vip: u32,
    addr: SocketAddr,
    name: Vec<u8>,
    nonce: u64,
    last: Instant,
    bucket: TokenBucket,
}

impl Member {
    fn auth(&self) -> Auth {
        Auth {
            session: self.session,
            token: self.token,
        }
    }
}

struct Room {
    code: Vec<u8>,
    /// vip -> member, in vip order (the order MEMBER messages and broadcasts follow).
    members: BTreeMap<u32, Member>,
    bucket: TokenBucket,
}

impl Room {
    fn member(&self, session: u32) -> Option<&Member> {
        self.members.values().find(|m| m.session == session)
    }
    fn member_mut(&mut self, session: u32) -> Option<&mut Member> {
        self.members.values_mut().find(|m| m.session == session)
    }
}

type RoomRef = Arc<Mutex<Room>>;

/// Who is where: changed under the admission lock only.
#[derive(Default)]
struct Admission {
    rooms: HashMap<Vec<u8>, RoomRef>,
    sessions: usize,
    by_addr: HashMap<SocketAddr, u32>,
    /// Sessions per source ([`source_of`]), for `max_sessions_per_source`.
    by_source: HashMap<IpAddr, usize>,
}

/// One second of the load history (`tick`).
struct Sample {
    at: Instant,
    stats: Stats,
    busy: Vec<u64>,
}

/// What `tick` keeps for the status answers: a few seconds of totals.
#[derive(Default)]
struct LoadHistory {
    samples: VecDeque<Sample>,
}

/// The seconds a status answer covers.
pub const LOAD_WINDOW_S: u64 = 5;

pub struct Relay {
    cfg: RelayConfig,
    cookies: RwLock<CookieKeys>,
    shards: Box<[RwLock<HashMap<u32, RoomRef>>]>,
    admission: Mutex<Admission>,
    hello_rl: SlotCounter,
    join_rl: SlotCounter,
    closed_rl: SlotTimes,
    epoch: Instant,
    counters: Box<[Counters]>,
    status_key: Option<StatusKey>,
    status_seen: Mutex<status::Replay>,
    status_rl: Mutex<TokenBucket>,
    history: Mutex<LoadHistory>,
}

fn vip_str(v: u32) -> std::net::Ipv4Addr {
    std::net::Ipv4Addr::from(v)
}

/// A lock that survives a panic of another holder (the state it guards is left consistent between statements).
fn lock<T>(m: &Mutex<T>) -> MutexGuard<'_, T> {
    m.lock().unwrap_or_else(|e| e.into_inner())
}

/// The source an address counts for in the per-source limits: an IPv4 address, or the /64 of an IPv6 one (one home
/// or one server gets a whole /64); `None` for loopback, never limited (a relay's own machine: local tests).
pub fn source_of(addr: SocketAddr) -> Option<IpAddr> {
    let ip = addr.ip().to_canonical();
    if ip.is_loopback() {
        return None;
    }
    Some(match ip {
        IpAddr::V6(a) => IpAddr::V6((u128::from(a) & (u128::MAX << 64)).into()),
        v4 => v4,
    })
}

fn shard_of(session: u32) -> usize {
    // session ids are random: their low bits spread evenly
    (session as usize) & (SHARDS - 1)
}

impl Relay {
    pub fn new(cfg: RelayConfig, now: Instant) -> Self {
        Self::with_cookies(cfg.clone(), CookieKeys::new(now, cfg.cookie_rotation), now)
    }

    pub fn with_cookies(cfg: RelayConfig, cookies: CookieKeys, now: Instant) -> Self {
        Self::with_workers(cfg, cookies, now, 1)
    }

    /// A relay whose datagrams `workers` threads handle at the same time ([`Relay::handle_on`]).
    pub fn with_workers(cfg: RelayConfig, cookies: CookieKeys, now: Instant, workers: usize) -> Self {
        let status_key = cfg.room_secret.as_ref().map(StatusKey::new);
        Relay {
            hello_rl: SlotCounter::new(cfg.hello_per_s),
            join_rl: SlotCounter::new(cfg.join_per_s),
            closed_rl: SlotTimes::new(Duration::from_secs(1)),
            cookies: RwLock::new(cookies),
            shards: (0..SHARDS).map(|_| RwLock::new(HashMap::new())).collect(),
            admission: Mutex::new(Admission::default()),
            epoch: now,
            counters: (0..workers.max(1)).map(|_| Counters::default()).collect(),
            status_key,
            status_seen: Mutex::new(status::Replay::default()),
            status_rl: Mutex::new(TokenBucket::new(status::ANSWERS_PER_S, 0, now)),
            history: Mutex::new(LoadHistory::default()),
            cfg,
        }
    }

    pub fn config(&self) -> &RelayConfig {
        &self.cfg
    }
    pub fn workers(&self) -> usize {
        self.counters.len()
    }
    /// The counters of every worker, summed.
    pub fn stats(&self) -> Stats {
        let mut s = Stats::default();
        for c in self.counters.iter() {
            s.rx += c.rx.load(Relaxed);
            s.tx += c.tx.load(Relaxed);
            s.rx_bytes += c.rx_bytes.load(Relaxed);
            s.tx_bytes += c.tx_bytes.load(Relaxed);
            s.forwarded += c.forwarded.load(Relaxed);
            s.joins += c.joins.load(Relaxed);
            s.leaves += c.leaves.load(Relaxed);
            s.timeouts += c.timeouts.load(Relaxed);
            s.refused += c.refused.load(Relaxed);
            s.refused_full += c.refused_full.load(Relaxed);
            for (d, x) in s.dropped.iter_mut().zip(c.dropped.iter()) {
                *d += x.load(Relaxed);
            }
        }
        s
    }
    /// Nanoseconds each worker spent busy, since the start.
    pub fn busy_ns(&self) -> Vec<u64> {
        self.counters.iter().map(|c| c.busy_ns.load(Relaxed)).collect()
    }
    /// Adds busy time to worker `w` (`server.rs`).
    pub fn add_busy(&self, w: usize, ns: u64) {
        add(&self.counters[w % self.counters.len()].busy_ns, ns);
    }
    pub fn session_count(&self) -> usize {
        lock(&self.admission).sessions
    }
    pub fn room_count(&self) -> usize {
        lock(&self.admission).rooms.len()
    }
    pub fn member_count(&self, room: &[u8]) -> usize {
        let a = lock(&self.admission);
        a.rooms.get(room).map_or(0, |r| lock(r).members.len())
    }
    /// `(room, members)` pairs, sorted by room code.
    pub fn room_sizes(&self) -> Vec<(String, usize)> {
        let a = lock(&self.admission);
        let mut v: Vec<_> = a
            .rooms
            .iter()
            .map(|(k, r)| (String::from_utf8_lossy(k).into_owned(), lock(r).members.len()))
            .collect();
        v.sort();
        v
    }

    fn second(&self, now: Instant) -> u64 {
        now.saturating_duration_since(self.epoch).as_secs()
    }

    fn counters(&self, w: usize) -> &Counters {
        &self.counters[w % self.counters.len()]
    }

    fn drop_(&self, c: &Counters, why: DropReason) {
        add(&c.dropped[why as usize], 1);
    }

    /// Sends a reply to the source of the datagram being handled. `request_len` is that datagram's size: a reply is
    /// never larger (anti-amplification; every reply of the protocol is at most the size of its request).
    fn reply(&self, c: &Counters, out: &mut Outbox, to: SocketAddr, request_len: usize, f: impl FnOnce(&mut Vec<u8>)) {
        let n = out.push(to, f);
        debug_assert!(n <= request_len, "reply larger than request");
        add(&c.tx, 1);
        add(&c.tx_bytes, n as u64);
    }

    fn send_member(&self, c: &Counters, out: &mut Outbox, to: SocketAddr, f: impl FnOnce(&mut Vec<u8>)) {
        let n = out.push(to, f);
        add(&c.tx, 1);
        add(&c.tx_bytes, n as u64);
    }

    /// Handles one received datagram (worker 0). `unix` is wall-clock seconds (cookie epochs only).
    pub fn handle(&self, now: Instant, unix: u64, src: SocketAddr, b: &[u8], out: &mut Outbox) {
        self.handle_on(0, now, unix, src, b, out)
    }

    /// Handles one received datagram on worker `w` (its counters).
    pub fn handle_on(&self, w: usize, now: Instant, unix: u64, src: SocketAddr, b: &[u8], out: &mut Outbox) {
        let c = self.counters(w);
        add(&c.rx, 1);
        add(&c.rx_bytes, b.len() as u64);
        if src.port() == 0 || src.ip().is_unspecified() || src.ip().is_multicast() {
            return self.drop_(c, DropReason::Invalid);
        }
        let msg = match wire::parse(b) {
            Ok(m) => m,
            Err(ParseError::OtherVersion { ty }) => {
                // Section 6.1: only a padded HELLO is answered (24 bytes out for at least 64 in).
                if ty == wire::ty::HELLO && b.len() >= wire::HELLO_LEN {
                    if !self.hello_rl.allow(&src.ip().to_canonical(), self.second(now)) {
                        return self.drop_(c, DropReason::RateHello);
                    }
                    let nonce = u64::from_be_bytes(b[8..16].try_into().expect("8 bytes"));
                    self.reply(c, out, src, b.len(), |o| {
                        wire::encode_join_nak(o, nonce, nak::VERSION, nak::detail::NONE)
                    });
                    return;
                }
                return self.drop_(c, DropReason::OtherVersion);
            }
            Err(_) => return self.drop_(c, DropReason::Invalid),
        };
        match msg {
            Msg::Hello { nonce } => {
                if !self.hello_rl.allow(&src.ip().to_canonical(), self.second(now)) {
                    return self.drop_(c, DropReason::RateHello);
                }
                let cookie = self
                    .cookies
                    .read()
                    .unwrap_or_else(|e| e.into_inner())
                    .issue(src, nonce, unix);
                self.reply(c, out, src, b.len(), |o| wire::encode_challenge(o, nonce, &cookie));
            }
            Msg::Join(j) => self.on_join(c, now, unix, src, b.len(), &j, out),
            Msg::Keepalive { auth, seq } => self.on_keepalive(c, now, src, b.len(), auth, seq, out),
            Msg::Leave { auth } => self.on_leave(c, now, src, auth, out),
            Msg::Data { auth, hdr, payload } => self.on_data(c, now, src, b, auth, hdr, payload.len(), out),
            Msg::StatusQuery => self.on_status(c, now, unix, src, b, out),
            _ => self.drop_(c, DropReason::WrongDirection),
        }
    }

    // ---- handshake --------------------------------------------------------------------------------------------

    #[allow(clippy::too_many_arguments)]
    fn nak(
        &self,
        c: &Counters,
        out: &mut Outbox,
        src: SocketAddr,
        len: usize,
        nonce: u64,
        reason: u8,
        detail: u8,
        why: &str,
    ) {
        add(&c.refused, 1);
        if reason == nak::ROOM_FULL || reason == nak::RELAY_FULL {
            add(&c.refused_full, 1);
        }
        if reason == nak::COOKIE {
            debug!(reason, %src, "join refused: {why}");
        } else {
            info!(reason, %src, "join refused: {why}");
        }
        self.reply(c, out, src, len, |o| wire::encode_join_nak(o, nonce, reason, detail));
    }

    fn join_ack(&self, m: &Member) -> JoinAck {
        JoinAck {
            nonce: m.nonce,
            session: m.session,
            token: m.token,
            vip: m.vip,
            prefix: 16,
            max_members: self.cfg.max_members as u8,
            keepalive_s: self.cfg.keepalive_s,
            idle_s: self.cfg.idle_s,
            max_payload: wire::MAX_PAYLOAD as u16,
        }
    }

    /// The room of `session`, if it exists (the member itself is checked under the room's lock).
    fn room_of(&self, session: u32) -> Option<RoomRef> {
        self.shards[shard_of(session)]
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(&session)
            .cloned()
    }

    #[allow(clippy::too_many_arguments)]
    fn on_join(
        &self,
        c: &Counters,
        now: Instant,
        unix: u64,
        src: SocketAddr,
        len: usize,
        j: &wire::JoinRaw<'_>,
        out: &mut Outbox,
    ) {
        use nak::detail::{NONE, SOURCE};
        if !self.join_rl.allow(&src.ip().to_canonical(), self.second(now)) {
            return self.drop_(c, DropReason::RateJoin);
        }
        // 1. cookie
        if !self
            .cookies
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .verify(src, j.nonce, &j.cookie, unix)
        {
            return self.nak(
                c,
                out,
                src,
                len,
                j.nonce,
                nak::COOKIE,
                NONE,
                "cookie invalid or expired",
            );
        }
        // 2-3. lengths, characters
        let (room, name) = match j.room_and_name() {
            Ok(x) => x,
            Err(e) => return self.nak(c, out, src, len, j.nonce, e.reason(), NONE, "invalid room or name"),
        };
        // A relay bound to a service: the room code must be one that service issued.
        if let Some(secret) = &self.cfg.room_secret {
            if !crate::roomauth::verify(secret.bytes(), room, unix) {
                return self.nak(
                    c,
                    out,
                    src,
                    len,
                    j.nonce,
                    nak::ROOM,
                    NONE,
                    "room not issued by this relay's service",
                );
            }
        }
        // From here on, one JOIN at a time: the limits and the last place of a room are decided under this lock.
        let mut adm = lock(&self.admission);
        // 4. retransmission of a JOIN whose JOIN_ACK was lost
        if let Some(&sid) = adm.by_addr.get(&src) {
            if let Some(r) = self.room_of(sid) {
                let r = lock(&r);
                if let Some(m) = r.member(sid) {
                    if m.nonce == j.nonce && r.code == room {
                        let ack = self.join_ack(m);
                        drop(r);
                        return self.reply(c, out, src, len, |o| wire::encode_join_ack(o, &ack));
                    }
                }
            }
            // 5. the same client joining again: the old session ends silently (MEMBER(left) to the others)
            self.remove_member(
                c,
                &mut adm,
                sid,
                out,
                None,
                "replaced by a new join from the same address",
            );
        }
        // 6. capacity (a limit of 0 is no limit)
        let existing = adm.rooms.get(room).cloned();
        let existing = existing.as_ref().map(|r| lock(r));
        if existing
            .as_ref()
            .is_some_and(|r| r.members.len() >= self.cfg.max_members)
        {
            return self.nak(c, out, src, len, j.nonce, nak::ROOM_FULL, NONE, "room full");
        }
        let (max_s, max_r) = (self.cfg.max_sessions, self.cfg.max_rooms);
        if max_s != 0 && adm.sessions >= max_s || existing.is_none() && max_r != 0 && adm.rooms.len() >= max_r {
            return self.nak(c, out, src, len, j.nonce, nak::RELAY_FULL, NONE, "relay full");
        }
        // 6b. per source (security review M4): the existing codes, with the detail byte saying the limit counts the
        // source (protocol 1.1.13; a client that does not know it reads the codes as before).
        if let Some(source) = source_of(src) {
            let max_room = self.cfg.max_members_per_source;
            let in_room = existing.as_ref().map_or(0, |r| {
                r.members.values().filter(|m| source_of(m.addr) == Some(source)).count()
            });
            if max_room != 0 && in_room >= max_room {
                return self.nak(
                    c,
                    out,
                    src,
                    len,
                    j.nonce,
                    nak::ROOM_FULL,
                    SOURCE,
                    "room full for this source",
                );
            }
            let max_relay = self.cfg.max_sessions_per_source;
            if max_relay != 0 && adm.by_source.get(&source).copied().unwrap_or(0) >= max_relay {
                return self.nak(
                    c,
                    out,
                    src,
                    len,
                    j.nonce,
                    nak::RELAY_FULL,
                    SOURCE,
                    "relay full for this source",
                );
            }
        }
        // 7. vip
        let vip = {
            let used = existing.as_ref().map(|r| &r.members);
            let is_free = |v: u32| !used.is_some_and(|u| u.contains_key(&v));
            let usable = |v: u32| v & 0xFF != 0 && v & 0xFF != 0xFF;
            if j.vip & wire::VNET_MASK == wire::VNET && usable(j.vip) && is_free(j.vip) {
                Some(j.vip)
            } else {
                (wire::VNET + 1..wire::VBCAST).find(|&v| usable(v) && is_free(v))
            }
        };
        let Some(vip) = vip else {
            return self.nak(c, out, src, len, j.nonce, nak::ROOM_FULL, NONE, "no free address");
        };
        drop(existing);
        // a session id no other session has (sessions are only added under the admission lock)
        let mut session = 0u32;
        while session == 0 || self.room_of(session).is_some() {
            let mut s = [0u8; 4];
            rand::fill(&mut s[..]);
            session = u32::from_be_bytes(s);
        }
        let mut token = [0u8; wire::TOKEN_LEN];
        rand::fill(&mut token[..]);
        let m = Member {
            session,
            token,
            vip,
            addr: src,
            name: name.to_vec(),
            nonce: j.nonce,
            last: now,
            bucket: TokenBucket::new(self.cfg.member_pps, self.cfg.member_bytes_per_s, now),
        };
        let ack = self.join_ack(&m);
        let auth = m.auth();
        let room_ref = adm
            .rooms
            .entry(room.to_vec())
            .or_insert_with(|| {
                Arc::new(Mutex::new(Room {
                    code: room.to_vec(),
                    members: BTreeMap::new(),
                    bucket: TokenBucket::new(self.cfg.room_pps, self.cfg.room_bytes_per_s, now),
                }))
            })
            .clone();
        let mut r = lock(&room_ref);
        r.members.insert(vip, m);
        let members = r.members.len();
        self.shards[shard_of(session)]
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session, room_ref.clone());
        adm.sessions += 1;
        adm.by_addr.insert(src, session);
        count_source(&mut adm.by_source, src, 1);
        add(&c.joins, 1);
        self.reply(c, out, src, len, |o| wire::encode_join_ack(o, &ack));
        // The room's tag, never its code (relay.md section 3); the name and the address are what an operator needs
        // to deal with abuse (PRIVACY-TEMPLATE.md says so).
        info!(
            room = %crate::roomauth::room_tag(room), vip = %vip_str(vip), name = ?String::from_utf8_lossy(name),
            %src, session = format_args!("{session:08x}"), members, "joined"
        );
        // MEMBER messages (section 3.11): about the newcomer to the others, about the others to the newcomer.
        for o in r.members.values().filter(|o| o.session != session) {
            let (oa, oauth, ovip) = (o.addr, o.auth(), o.vip);
            self.send_member(c, out, oa, |b| {
                wire::encode_member(b, &oauth, wire::MEMBER_JOINED, vip, name)
            });
            self.send_member(c, out, src, |b| {
                wire::encode_member(b, &auth, wire::MEMBER_JOINED, ovip, &o.name)
            });
        }
    }

    /// Ends a session: removes it everywhere, tells the others, optionally tells the member (CLOSED). The caller
    /// holds the admission lock (`adm`).
    fn remove_member(
        &self,
        c: &Counters,
        adm: &mut Admission,
        session: u32,
        out: &mut Outbox,
        closed_reason: Option<u8>,
        why: &str,
    ) {
        let Some(room_ref) = self.shards[shard_of(session)]
            .write()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&session)
        else {
            return;
        };
        let mut r = lock(&room_ref);
        let Some(vip) = r.members.iter().find(|(_, m)| m.session == session).map(|(v, _)| *v) else {
            return;
        };
        let m = r.members.remove(&vip).expect("the member just found");
        adm.sessions -= 1;
        if adm.by_addr.get(&m.addr) == Some(&session) {
            adm.by_addr.remove(&m.addr);
        }
        count_source(&mut adm.by_source, m.addr, -1);
        let remaining = r.members.len();
        if let Some(reason) = closed_reason {
            let a = m.auth();
            self.send_member(c, out, m.addr, |o| wire::encode_closed(o, &a, reason));
        }
        for o in r.members.values() {
            let (oa, oauth) = (o.addr, o.auth());
            self.send_member(c, out, oa, |buf| {
                wire::encode_member(buf, &oauth, wire::MEMBER_LEFT, m.vip, &m.name)
            });
        }
        let code = r.code.clone();
        drop(r);
        if remaining == 0 {
            adm.rooms.remove(&code);
        }
        info!(
            room = %crate::roomauth::room_tag(&code), vip = %vip_str(m.vip), name = ?String::from_utf8_lossy(&m.name),
            session = format_args!("{session:08x}"), members = remaining, why, "left"
        );
    }

    // ---- session messages ------------------------------------------------------------------------------------

    /// Answers CLOSED reason 2 to a datagram of an unknown session or with a wrong token, at most once per second per
    /// source address, when `answer` is set.
    #[allow(clippy::too_many_arguments)]
    fn unknown(
        &self,
        c: &Counters,
        now: Instant,
        src: SocketAddr,
        len: usize,
        auth: Auth,
        answer: bool,
        out: &mut Outbox,
    ) {
        self.drop_(c, DropReason::UnknownSession);
        if answer && self.closed_rl.allow(&src, now) {
            self.reply(c, out, src, len, |o| {
                wire::encode_closed(o, &auth, closed::UNKNOWN_SESSION)
            });
        }
    }

    /// The member of `auth` in `r`, its token compared in constant time.
    fn authenticated(r: &mut Room, auth: Auth) -> Option<&mut Member> {
        r.member_mut(auth.session)
            .filter(|m| bool::from(m.token.ct_eq(&auth.token)))
    }

    #[allow(clippy::too_many_arguments)]
    fn on_keepalive(
        &self,
        c: &Counters,
        now: Instant,
        src: SocketAddr,
        len: usize,
        auth: Auth,
        seq: u32,
        out: &mut Outbox,
    ) {
        let Some(room_ref) = self.room_of(auth.session) else {
            return self.unknown(c, now, src, len, auth, true, out);
        };
        {
            let mut r = lock(&room_ref);
            let members = r.members.len().min(255) as u8;
            let Some(m) = Self::authenticated(&mut r, auth) else {
                drop(r);
                return self.unknown(c, now, src, len, auth, true, out);
            };
            if m.addr == src {
                m.last = now;
                drop(r);
                return self.reply(c, out, src, len, |o| wire::encode_keepalive_ack(o, &auth, seq, members));
            }
        }
        // The client moved (NAT rebinding): who holds which address changes under the admission lock. Another member
        // already using that address stays untouched.
        let mut adm = lock(&self.admission);
        if adm.by_addr.contains_key(&src) {
            return self.drop_(c, DropReason::RebindConflict);
        }
        let mut r = lock(&room_ref);
        let members = r.members.len().min(255) as u8;
        let Some(m) = Self::authenticated(&mut r, auth) else {
            // ended meanwhile
            drop(r);
            drop(adm);
            return self.unknown(c, now, src, len, auth, true, out);
        };
        let old = m.addr;
        if old != src {
            info!(from = %old, to = %src, session = format_args!("{:08x}", auth.session), "member address changed");
            m.addr = src;
            adm.by_addr.remove(&old);
            adm.by_addr.insert(src, auth.session);
            count_source(&mut adm.by_source, old, -1);
            count_source(&mut adm.by_source, src, 1);
        }
        m.last = now;
        drop(r);
        drop(adm);
        self.reply(c, out, src, len, |o| wire::encode_keepalive_ack(o, &auth, seq, members));
    }

    fn on_leave(&self, c: &Counters, now: Instant, src: SocketAddr, auth: Auth, out: &mut Outbox) {
        // never answered, so never CLOSED either
        let mut adm = lock(&self.admission);
        let Some(room_ref) = self.room_of(auth.session) else {
            drop(adm);
            return self.unknown(c, now, src, 0, auth, false, out);
        };
        let addr = {
            let mut r = lock(&room_ref);
            Self::authenticated(&mut r, auth).map(|m| m.addr)
        };
        match addr {
            None => {
                drop(adm);
                self.unknown(c, now, src, 0, auth, false, out)
            }
            Some(a) if a != src => self.drop_(c, DropReason::WrongAddress),
            Some(_) => {
                add(&c.leaves, 1);
                self.remove_member(c, &mut adm, auth.session, out, None, "leave");
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn on_data(
        &self,
        c: &Counters,
        now: Instant,
        src: SocketAddr,
        b: &[u8],
        auth: Auth,
        hdr: wire::DataHeader,
        payload_len: usize,
        out: &mut Outbox,
    ) {
        debug_assert!(payload_len <= wire::MAX_PAYLOAD);
        let Some(room_ref) = self.room_of(auth.session) else {
            return self.unknown(c, now, src, b.len(), auth, true, out);
        };
        let mut dests = std::mem::take(&mut out.dests);
        dests.clear();
        {
            let mut r = lock(&room_ref);
            let Some(m) = Self::authenticated(&mut r, auth) else {
                drop(r);
                out.dests = dests;
                return self.unknown(c, now, src, b.len(), auth, true, out);
            };
            if m.addr != src {
                out.dests = dests;
                return self.drop_(c, DropReason::WrongAddress);
            }
            if hdr.src_vip != m.vip {
                out.dests = dests;
                return self.drop_(c, DropReason::SpoofedSource);
            }
            if !m.bucket.allow(now, b.len()) {
                out.dests = dests;
                return self.drop_(c, DropReason::RateMember);
            }
            m.last = now;
            let sender_vip = m.vip;
            if !r.bucket.allow(now, b.len()) {
                out.dests = dests;
                return self.drop_(c, DropReason::RateRoom);
            }
            // Destinations: every other member, or the one holding the destination vip.
            let broadcast = hdr.dst_vip == wire::VBCAST || hdr.dst_vip == wire::LIMITED_BCAST;
            if broadcast {
                dests.extend(
                    r.members
                        .iter()
                        .filter(|(&vip, _)| vip != sender_vip)
                        .map(|(_, o)| (o.addr, o.auth())),
                );
            } else if hdr.dst_vip != sender_vip {
                if let Some(o) = r.members.get(&hdr.dst_vip) {
                    dests.push((o.addr, o.auth()));
                }
            }
        }
        if dests.is_empty() {
            out.dests = dests;
            return self.drop_(c, DropReason::NoDestination);
        }
        let n = dests.len() as u64;
        for (to, a) in &dests {
            out.push(*to, |buf| {
                wire::put_header(buf, wire::ty::DATA);
                buf.extend_from_slice(&a.session.to_be_bytes());
                buf.extend_from_slice(&a.token);
                buf.extend_from_slice(&b[wire::SESSION_HEADER..]);
            });
        }
        out.dests = dests;
        add(&c.tx, n);
        add(&c.tx_bytes, n * b.len() as u64);
        add(&c.forwarded, n);
    }

    // ---- load -------------------------------------------------------------------------------------------------

    /// The load over the last [`LOAD_WINDOW_S`] seconds (`tick` keeps the history).
    pub fn load(&self, now: Instant) -> Load {
        let h = lock(&self.history);
        let (sessions, rooms) = {
            let a = lock(&self.admission);
            (a.sessions, a.rooms.len())
        };
        let mut l = Load {
            workers: self.counters.len(),
            sessions,
            rooms,
            max_sessions: self.cfg.max_sessions,
            max_rooms: self.cfg.max_rooms,
            max_members: self.cfg.max_members,
            uptime_s: now.saturating_duration_since(self.epoch).as_secs(),
            ..Load::default()
        };
        let (Some(first), Some(last)) = (h.samples.front(), h.samples.back()) else {
            l.worker_busy = vec![0; self.counters.len()];
            return l;
        };
        let span = last.at.saturating_duration_since(first.at);
        let secs = span.as_secs_f64();
        l.window_s = span.as_secs().min(255) as u8;
        if secs <= 0.0 {
            l.worker_busy = vec![0; self.counters.len()];
            return l;
        }
        let (a, b) = (&first.stats, &last.stats);
        let rate = |x: u64, y: u64| ((y.saturating_sub(x)) as f64 / secs).round() as u64;
        l.rx_pps = rate(a.rx, b.rx);
        l.tx_pps = rate(a.tx, b.tx);
        l.rx_bps = rate(a.rx_bytes, b.rx_bytes);
        l.tx_bps = rate(a.tx_bytes, b.tx_bytes);
        l.dropped_ps = rate(a.dropped_total(), b.dropped_total());
        l.refused = b.refused.saturating_sub(a.refused);
        l.refused_full = b.refused_full.saturating_sub(a.refused_full);
        l.worker_busy = first
            .busy
            .iter()
            .zip(last.busy.iter())
            .map(|(x, y)| {
                let f = (y.saturating_sub(*x)) as f64 / span.as_nanos().max(1) as f64;
                (f * 100.0).round().clamp(0.0, 100.0) as u8
            })
            .collect();
        l
    }

    /// A status query (`status.rs`): answered only by a relay bound to a service, only when its tag is that
    /// service's, recent and not seen before, at most a few times a second, and never larger than the query.
    fn on_status(&self, c: &Counters, now: Instant, unix: u64, src: SocketAddr, b: &[u8], out: &mut Outbox) {
        let Some(key) = &self.status_key else {
            // An open relay announces nothing: to it this is a datagram of an unknown type.
            return self.drop_(c, DropReason::Invalid);
        };
        // counted against the source's HELLO rate before any work (a service asks every few seconds)
        if !self.hello_rl.allow(&src.ip().to_canonical(), self.second(now)) {
            return self.drop_(c, DropReason::Status);
        }
        let unix_ms = unix.saturating_mul(1000);
        let Some(q) = key.verify_query(b, unix_ms) else {
            return self.drop_(c, DropReason::Status);
        };
        if !lock(&self.status_seen).fresh(q.nonce) || !lock(&self.status_rl).allow(now, 1) {
            return self.drop_(c, DropReason::Status);
        }
        let load = self.load(now);
        self.reply(c, out, src, b.len(), |o| key.encode_status(o, q.nonce, unix_ms, &load));
    }

    // ---- time -------------------------------------------------------------------------------------------------

    /// Periodic work, once a second: cookie secret rotation, idle expiry (CLOSED reason 1 to the member,
    /// MEMBER(left) to the others) and the load history.
    pub fn tick(&self, now: Instant, out: &mut Outbox) {
        self.cookies.write().unwrap_or_else(|e| e.into_inner()).tick(now);
        let c = self.counters(0);
        let idle = Duration::from_secs(self.cfg.idle_s as u64);
        {
            let mut adm = lock(&self.admission);
            let expired: Vec<u32> = adm
                .rooms
                .values()
                .flat_map(|r| {
                    lock(r)
                        .members
                        .values()
                        .filter(|m| now.saturating_duration_since(m.last) > idle)
                        .map(|m| m.session)
                        .collect::<Vec<_>>()
                })
                .collect();
            for sid in expired {
                add(&c.timeouts, 1);
                self.remove_member(c, &mut adm, sid, out, Some(closed::IDLE), "timeout");
            }
        }
        let sample = Sample {
            at: now,
            stats: self.stats(),
            busy: self.busy_ns(),
        };
        let mut h = lock(&self.history);
        h.samples.push_back(sample);
        while h.samples.len() > LOAD_WINDOW_S as usize + 1 {
            h.samples.pop_front();
        }
    }

    /// Graceful shutdown: CLOSED reason 4 to every member, and no state left.
    pub fn shutdown(&self, out: &mut Outbox) {
        let c = self.counters(0);
        let mut adm = lock(&self.admission);
        for r in adm.rooms.values() {
            for m in lock(r).members.values() {
                let a = m.auth();
                self.send_member(c, out, m.addr, |o| wire::encode_closed(o, &a, closed::SHUTDOWN));
            }
        }
        adm.rooms.clear();
        adm.sessions = 0;
        adm.by_addr.clear();
        adm.by_source.clear();
        for s in self.shards.iter() {
            s.write().unwrap_or_else(|e| e.into_inner()).clear();
        }
    }

    /// Sessions held by `addr`'s source ([`source_of`]), for the tests and the statistics.
    pub fn source_sessions(&self, addr: SocketAddr) -> usize {
        let a = lock(&self.admission);
        source_of(addr).and_then(|s| a.by_source.get(&s).copied()).unwrap_or(0)
    }
}

fn count_source(by_source: &mut HashMap<IpAddr, usize>, addr: SocketAddr, delta: isize) {
    let Some(source) = source_of(addr) else { return };
    let n = by_source.entry(source).or_insert(0);
    *n = n.saturating_add_signed(delta);
    if *n == 0 {
        by_source.remove(&source);
    }
}
