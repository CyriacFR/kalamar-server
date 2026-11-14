//! Shared state: the configuration, the database and the live (in-memory) state behind one lock, the event streams
//! and presence.
//!
//! Every handler takes the lock, works synchronously (SQLite calls are short), queues the events it produces, and
//! releases it; nothing awaits while holding it. Parties, rooms, tickets and event streams live in memory only and
//! are lost on restart (launchers then see empty state and start again); accounts, matches and ratings are in the
//! database.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::{Arc, Mutex, MutexGuard};

use kalamar_protocol::{Role, Team};
use rusqlite::{Connection, OptionalExtension, params};
use serde_json::{Value, json};
use tokio::sync::mpsc;

use crate::clock::Clock;
use crate::config::Config;
use crate::netutil::Cidr;
use crate::ratelimit::Limiters;
use crate::util::rfc3339;

/// How long a player stays `online` after their last event connection closed.
pub const PRESENCE_GRACE_S: i64 = 60;

#[derive(Clone)]
pub struct AppState {
    /// The configuration in force; a reload replaces it (`reload.rs`). Read it with [`AppState::cfg`].
    pub current_cfg: Arc<std::sync::RwLock<Arc<Config>>>,
    /// The file a reload reads again (the binary sets it; tests reload from a value).
    pub config_path: Option<Arc<std::path::PathBuf>>,
    pub core: Arc<Mutex<Core>>,
    pub clock: Arc<dyn Clock>,
    pub limits: Arc<Limiters>,
    pub trusted: Arc<Vec<Cidr>>,
    /// OAuth loopback providers (`[auth.discord]`, `[[auth.oauth]]`), in the order of `auth.providers`.
    pub oauth: Arc<Vec<Arc<crate::auth::oauth::OAuthProvider>>>,
    /// E-mail and password accounts, when `[auth.password]` is on.
    pub password: Option<Arc<crate::auth::password::Passwords>>,
}

impl AppState {
    /// The configuration in force now.
    pub fn cfg(&self) -> Arc<Config> {
        self.current_cfg.read().unwrap_or_else(|p| p.into_inner()).clone()
    }

    /// Locks the core and sets its notion of "now".
    pub fn lock(&self) -> MutexGuard<'_, Core> {
        let mut g = self.core.lock().unwrap_or_else(|p| p.into_inner());
        g.now = self.clock.now();
        g
    }
}

pub struct Core {
    pub cfg: Arc<Config>,
    pub db: Connection,
    pub live: Live,
    pub secret: Vec<u8>,
    pub now: i64,
    /// The secrets of the bound relays, read at start and at each reload (`relays.rs`); never read from disk while a
    /// match is created.
    pub relay_keys: Arc<crate::relays::Keys>,
}

/// One event stream (`events.md`): numbered events, a replay buffer, and the sender of its connection if any.
pub struct Stream {
    pub id: String,
    pub user_id: String,
    pub session_id: String,
    pub seq: u64,
    pub buffer: VecDeque<(u64, Arc<str>)>,
    pub tx: Option<mpsc::Sender<Arc<str>>>,
    pub conn: u64,
    pub opened_at: i64,
    pub closed_at: Option<i64>,
}

pub struct OAuthPending {
    pub provider: String,
    /// The client network that began it (security review M5: at most [`crate::auth::oauth::MAX_PENDING_PER_CLIENT`]).
    pub client: std::net::IpAddr,
    pub challenge: String,
    pub redirect_uri: String,
    pub expires: i64,
}

pub struct Party {
    pub id: String,
    pub leader: String,
    /// (user, joined at), in joining order.
    pub members: Vec<(String, i64)>,
    pub created_at: i64,
}

#[derive(Clone)]
pub struct RoomMember {
    pub user_id: String,
    /// The member's team; for a spectator, the team it came from (unused while it sits in the stands).
    pub team: Team,
    pub ready: bool,
    pub joined_at: i64,
    /// 1.1.7: seated in the stands (`team: "spectator"` in the room object).
    pub spectator: bool,
    /// 1.1.7: came in through an invitation (the stands' `who = "invited"`).
    pub invited: bool,
}

pub struct Room {
    pub id: String,
    pub code: String,
    pub owner: String,
    pub teams: [usize; 2],
    pub mode: String,
    pub stage: Option<String>,
    pub password: Option<String>,
    /// The loadout preset chosen in the form (1.1.3).
    pub loadout: Option<String>,
    /// The draft chosen in the form (1.1.6).
    pub draft: Option<String>,
    /// 1.1.7: places opened in the stands (0 to 2) and who may take them.
    pub spectators: usize,
    pub spectators_who: String,
    pub members: Vec<RoomMember>,
    pub kicked: HashSet<String>,
    pub match_id: Option<String>,
    pub created_at: i64,
    pub last_activity: i64,
}

#[derive(Clone)]
pub struct Ticket {
    pub id: String,
    pub queue_id: String,
    pub party_id: Option<String>,
    pub members: Vec<String>,
    pub created_at: i64,
    pub match_id: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MState {
    PendingAccept,
    /// 1.1.6: every player accepted; the stage (and mode) draft runs (`draft.md`).
    Drafting,
    Ready,
    Running,
    Completed,
    Void,
    Cancelled,
    /// 1.1: reports are in; a referee decides (`operators.md`, section 3).
    UnderReview,
}

impl MState {
    pub fn as_str(self) -> &'static str {
        match self {
            MState::PendingAccept => "pending_accept",
            MState::Drafting => "drafting",
            MState::Ready => "ready",
            MState::Running => "running",
            MState::Completed => "completed",
            MState::Void => "void",
            MState::Cancelled => "cancelled",
            MState::UnderReview => "under_review",
        }
    }
    pub fn parse(s: &str) -> Option<MState> {
        Some(match s {
            "pending_accept" => MState::PendingAccept,
            "drafting" => MState::Drafting,
            "ready" => MState::Ready,
            "running" => MState::Running,
            "completed" => MState::Completed,
            "void" => MState::Void,
            "cancelled" => MState::Cancelled,
            "under_review" => MState::UnderReview,
            _ => return None,
        })
    }
    pub fn ended(self) -> bool {
        matches!(self, MState::Completed | MState::Void | MState::Cancelled)
    }
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct MatchRules {
    pub teams: [usize; 2],
    pub mode: String,
    pub stage: Option<String>,
    pub options: Value,
    pub rated: bool,
    pub ladder: Option<String>,
    pub season: Option<String>,
    pub result_policy: String,
    /// 1.1: launcher-applied loadout constraints (`match-contract-2.md`, section 5).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub loadout: Option<Value>,
    /// 1.1.7: the match's places in the stands (`spectators.md`); absent when it has none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub spectators: Option<StandsRule>,
}

/// 1.1.7: the places in the stands of one match, as the origin opened them (`rules.spectators`).
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct StandsRule {
    /// Places (1 or 2).
    pub seats: usize,
    /// Who may take one: `anyone`, `invited`, `friends`, `referees`.
    pub who: String,
    /// The host's wait for missing spectators once every player is in (the game's `spectators_wait`).
    pub wait_s: u32,
    /// The places can still be taken (before the match's game block is final: `pending_accept`, `drafting`).
    #[serde(default)]
    pub open: bool,
}

/// 1.1.7: a spectator of a match: a member of the game session seated in the stands, never a player.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SpectatorSeat {
    pub user_id: Option<String>,
    pub display_name: String,
    pub member_name: String,
    /// Its part in the game session (a spectator may host a room's session when the operator allows it).
    pub role: Role,
    /// One of the operator's referees (`[spectators] referees`, or seated through the operator API).
    pub referee: bool,
    pub launched_at: Option<i64>,
    pub status: Option<Value>,
    pub status_state: Option<String>,
    pub status_at: Option<i64>,
    pub report: Option<Value>,
    pub report_at: Option<i64>,
    /// Left the place after the game block was final (the game may still wait for it: `spectators_wait`).
    #[serde(default)]
    pub left: bool,
}

#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct GameBlock {
    pub match_id: String,
    pub relay_host: String,
    pub relay_port: u16,
    pub players: usize,
    pub rules: String,
    pub host_name: String,
    pub timeouts: Option<String>,
    /// Match contract version (1.1: 2 when every participant declared it).
    #[serde(default = "contract_one")]
    pub contract: u32,
    /// Contract 2: the relay room (it may differ from `match_id`); contract 1 uses `match_id`.
    #[serde(default)]
    pub relay_room: Option<String>,
    #[serde(default)]
    pub keep_open: bool,
    #[serde(default)]
    pub continue_from: Option<String>,
    /// How long a kept-open game waits for its continuation (1.1.1).
    #[serde(default)]
    pub continue_window_s: Option<u32>,
    #[serde(default)]
    pub series: Option<SeriesRef>,
}

fn contract_one() -> u32 {
    1
}

/// The series a match is a game of.
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
pub struct SeriesRef {
    pub id: String,
    pub index: usize,
    pub best_of: u32,
}

#[derive(Debug, Clone)]
pub struct Seat {
    pub user_id: Option<String>,
    pub display_name: String,
    pub member_name: String,
    pub team: Team,
    pub role: Role,
    pub party_id: Option<String>,
    pub ticket_id: Option<String>,
    pub accept: &'static str,
    pub launched_at: Option<i64>,
    pub status: Option<Value>,
    pub status_state: Option<String>,
    pub status_at: Option<i64>,
    pub saw_in_match: bool,
    pub report: Option<Value>,
    pub report_at: Option<i64>,
}

#[derive(Debug, Clone)]
pub struct LiveMatch {
    pub id: String,
    pub origin_kind: &'static str,
    pub origin_id: String,
    pub state: MState,
    pub rules: MatchRules,
    pub game: GameBlock,
    pub seats: Vec<Seat>,
    pub created_at: i64,
    pub ready_at: Option<i64>,
    pub accept_deadline: Option<i64>,
    pub launch_deadline: Option<i64>,
    pub result_deadline: Option<i64>,
    pub first_result_at: Option<i64>,
    pub updated_at: i64,
    pub ended_at: Option<i64>,
    pub cancel_reason: Option<String>,
    pub outcome: Option<Value>,
    /// 1.1.6: the stage (and mode) draft of the match, kept with it for its history.
    pub draft: Option<crate::draft::Draft>,
    /// 1.1.7: the spectators (`spectators.md`), never among the players (`seats`).
    pub spectators: Vec<SpectatorSeat>,
    /// The result policy as it was defined when the match was created: a reload that changes or removes it does not
    /// change how this match is decided (`rules.result_policy` keeps its id).
    pub policy: Arc<crate::config::PolicyEntry>,
}

impl LiveMatch {
    /// The player seat of `user` (spectators are not players: [`LiveMatch::spectator_of`]).
    pub fn seat_of(&self, user: &str) -> Option<usize> {
        self.seats.iter().position(|s| s.user_id.as_deref() == Some(user))
    }
    /// 1.1.7: the place in the stands of `user`.
    pub fn spectator_of(&self, user: &str) -> Option<usize> {
        self.spectators.iter().position(|s| s.user_id.as_deref() == Some(user))
    }
    /// A player or a spectator of the match.
    pub fn involves(&self, user: &str) -> bool {
        self.seat_of(user).is_some() || self.spectator_of(user).is_some()
    }
    /// Every account of the match: players, then spectators.
    pub fn users(&self) -> Vec<String> {
        self.seats
            .iter()
            .filter_map(|s| s.user_id.clone())
            .chain(self.spectators.iter().filter_map(|s| s.user_id.clone()))
            .collect()
    }
}

#[derive(Default)]
pub struct Live {
    pub streams: HashMap<String, Stream>,
    pub user_streams: HashMap<String, Vec<String>>,
    pub next_conn: u64,
    /// Event tickets: hash -> (user, session, expires).
    pub ws_tickets: HashMap<Vec<u8>, (String, String, i64)>,
    pub oauth_states: HashMap<String, OAuthPending>,
    /// Users whose last stream closed, and when.
    pub offline_since: HashMap<String, i64>,
    /// Presence last announced to friends.
    pub presence: HashMap<String, &'static str>,
    pub parties: HashMap<String, Party>,
    pub user_party: HashMap<String, String>,
    /// (party, invitee) -> (inviter, expires).
    pub party_invites: HashMap<(String, String), (String, i64)>,
    pub rooms: HashMap<String, Room>,
    pub user_room: HashMap<String, String>,
    pub room_codes: HashMap<String, String>,
    /// (room, invitee) -> (inviter, expires).
    pub room_invites: HashMap<(String, String), (String, i64)>,
    pub tickets: HashMap<String, Ticket>,
    pub user_ticket: HashMap<String, String>,
    pub matches: HashMap<String, LiveMatch>,
    pub user_match: HashMap<String, String>,
    /// Recent waits (seconds) per queue, for the estimate.
    pub queue_waits: HashMap<String, VecDeque<i64>>,
    pub last_sweep: i64,
    pub last_purge: i64,
    /// Outgoing webhook events (`webhooks.rs`), when configured.
    pub webhooks: Option<tokio::sync::mpsc::UnboundedSender<(String, Value)>>,
    /// Protocol 1.1.
    pub series: HashMap<String, crate::series::Series>,
    pub activities: HashMap<String, crate::activities::ActLive>,
    /// Last authenticated request per account (1.1.4 `population`; forgotten after two minutes).
    pub last_seen: HashMap<String, i64>,
    /// The last answer of `GET /v1/population` and when it was computed.
    pub population: Option<(i64, Value)>,
    /// Addresses each password account logged in from, and until when they stay exempt from that account's lock
    /// (memory only, `auth.password.lockout.known_address_days`).
    pub known_ips: HashMap<String, Vec<(std::net::IpAddr, i64)>>,
    /// 1.1.6: the queue tickets of matches in their draft (memory only), searching again if a player abandons it.
    pub draft_tickets: HashMap<String, Vec<Ticket>>,
    /// `meta.revision` (`meta.changed` carries it): the clock in milliseconds when the offer last changed (the start,
    /// a reload), so that it keeps growing from one start to the next as long as the clock does (`reload.rs`).
    pub meta_revision: u64,
    /// The `admin reload` requests already served whose answer could not be written yet (`reload::poll`): never
    /// served twice.
    pub reloads_served: Vec<(i64, bool, String)>,
    /// Relays (by `host:port`) that failed their last reachability checks, and how many in a row (`relays.rs`).
    pub relay_fails: HashMap<String, u32>,
    /// 1.1.13: what each bound relay (by id) last said of its load, and whether it still answers (`relays.rs`).
    pub relay_load: HashMap<String, crate::relays::LoadState>,
    /// 1.1.13: whether new matches were refused because every relay was full (logged once per episode).
    pub relays_all_full: bool,
}

impl Core {
    pub fn new(cfg: Arc<Config>, db: Connection, secret: Vec<u8>, now: i64) -> Core {
        Core {
            cfg,
            db,
            live: Live::default(),
            secret,
            now,
            relay_keys: Arc::default(),
        }
    }

    // ----- events -----

    /// Queues an event for every stream of `user`.
    pub fn push(&mut self, user: &str, kind: &str, data: Value) {
        let Some(ids) = self.live.user_streams.get(user).cloned() else {
            return;
        };
        let at = rfc3339(self.now);
        let cap = self.cfg.events.buffer;
        for id in ids {
            let Some(s) = self.live.streams.get_mut(&id) else {
                continue;
            };
            s.seq += 1;
            let frame: Arc<str> = Arc::from(
                json!({"seq": s.seq, "type": kind, "at": at, "data": data})
                    .to_string()
                    .as_str(),
            );
            s.buffer.push_back((s.seq, frame.clone()));
            while s.buffer.len() > cap {
                s.buffer.pop_front();
            }
            if let Some(tx) = &s.tx
                && tx.try_send(frame).is_err()
            {
                // A client that does not read: drop its connection; it resumes from the buffer.
                s.tx = None;
            }
        }
    }

    pub fn push_many(&mut self, users: &[String], kind: &str, data: &Value) {
        for u in users {
            self.push(u, kind, data.clone());
        }
    }

    pub fn is_connected(&self, user: &str) -> bool {
        self.live.user_streams.get(user).is_some_and(|ids| {
            ids.iter()
                .any(|id| self.live.streams.get(id).is_some_and(|s| s.tx.is_some()))
        })
    }

    /// Closes the streams of a session (or of every session of a user) with a `bye`.
    pub fn close_streams(&mut self, user: &str, session: Option<&str>, reason: &str) {
        let ids: Vec<String> = self.live.user_streams.get(user).cloned().unwrap_or_default();
        for id in ids {
            let matches = self
                .live
                .streams
                .get(&id)
                .is_some_and(|s| session.is_none_or(|x| s.session_id == x));
            if !matches {
                continue;
            }
            if let Some(s) = self.live.streams.remove(&id)
                && let Some(tx) = s.tx
            {
                let _ = tx.try_send(Arc::from(json!({"type": "bye", "reason": reason}).to_string().as_str()));
            }
            if let Some(v) = self.live.user_streams.get_mut(user) {
                v.retain(|x| x != &id);
            }
        }
        if self.live.user_streams.get(user).is_some_and(Vec::is_empty) {
            self.live.user_streams.remove(user);
            self.live
                .offline_since
                .insert(user.to_string(), self.now - PRESENCE_GRACE_S);
        }
    }

    // ----- users -----

    pub fn display_name(&self, user: &str) -> String {
        self.db
            .query_row("SELECT display_name FROM users WHERE id = ?1", [user], |r| r.get(0))
            .optional()
            .ok()
            .flatten()
            .unwrap_or_default()
    }

    pub fn user_json(&self, user: &str) -> Value {
        json!({"id": user, "display_name": self.display_name(user)})
    }

    pub fn user_exists(&self, user: &str) -> bool {
        self.db
            .query_row("SELECT 1 FROM users WHERE id = ?1", [user], |_| Ok(()))
            .optional()
            .ok()
            .flatten()
            .is_some()
    }

    pub fn friends_of(&self, user: &str) -> Vec<String> {
        let mut st = match self.db.prepare_cached(
            "SELECT user_b FROM friendships WHERE user_a = ?1 UNION SELECT user_a FROM friendships WHERE user_b = ?1",
        ) {
            Ok(s) => s,
            Err(_) => return Vec::new(),
        };
        st.query_map([user], |r| r.get::<_, String>(0))
            .map(|it| it.flatten().collect())
            .unwrap_or_default()
    }

    pub fn are_friends(&self, a: &str, b: &str) -> bool {
        let (x, y) = if a < b { (a, b) } else { (b, a) };
        self.db
            .query_row(
                "SELECT 1 FROM friendships WHERE user_a = ?1 AND user_b = ?2",
                params![x, y],
                |_| Ok(()),
            )
            .optional()
            .ok()
            .flatten()
            .is_some()
    }

    /// Either side blocked the other.
    pub fn blocked_between(&self, a: &str, b: &str) -> bool {
        self.db
            .query_row(
                "SELECT 1 FROM blocks WHERE (blocker = ?1 AND blocked = ?2) OR (blocker = ?2 AND blocked = ?1)",
                params![a, b],
                |_| Ok(()),
            )
            .optional()
            .ok()
            .flatten()
            .is_some()
    }

    // ----- presence -----

    pub fn presence_of(&self, user: &str) -> &'static str {
        let share: bool = self
            .db
            .query_row("SELECT share_presence FROM users WHERE id = ?1", [user], |r| {
                r.get::<_, i64>(0)
            })
            .map(|v| v != 0)
            .unwrap_or(false);
        if !share {
            return "offline";
        }
        if let Some(m) = self.live.user_match.get(user).and_then(|id| self.live.matches.get(id))
            && matches!(m.state, MState::Drafting | MState::Ready | MState::Running)
        {
            return "in_match";
        }
        if self.live.user_ticket.contains_key(user) {
            return "in_queue";
        }
        if self.live.user_streams.get(user).is_some_and(|v| !v.is_empty()) {
            return "online";
        }
        match self.live.offline_since.get(user) {
            Some(t) if self.now - t < PRESENCE_GRACE_S => "online",
            _ => "offline",
        }
    }

    /// Recomputes `user`'s presence and tells their friends when it changed.
    pub fn refresh_presence(&mut self, user: &str) {
        if !self.cfg.features.presence {
            return;
        }
        let p = self.presence_of(user);
        let old = self.live.presence.insert(user.to_string(), p).unwrap_or("offline");
        if old != p {
            let data = json!({"user_id": user, "presence": p});
            for f in self.friends_of(user) {
                self.push(&f, "friend.presence", data.clone());
            }
        }
        if p == "offline" {
            self.live.presence.remove(user);
        }
    }

    /// The user's restriction, if any: `(banned_until, reason)`.
    pub fn ban_of(&self, user: &str) -> Option<(i64, Option<String>)> {
        self.db
            .query_row(
                "SELECT banned_until, ban_reason FROM users WHERE id = ?1",
                [user],
                |r| Ok((r.get::<_, Option<i64>>(0)?, r.get::<_, Option<String>>(1)?)),
            )
            .optional()
            .ok()
            .flatten()
            .and_then(|(until, reason)| until.filter(|u| *u > self.now).map(|u| (u, reason)))
    }
}
