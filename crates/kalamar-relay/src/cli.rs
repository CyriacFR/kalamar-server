//! Command line and environment configuration. Every flag has a `KALAMAR_RELAY_*` environment variable; a flag on
//! the command line wins over the environment.

use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;

use clap::Parser;

use crate::relay::RelayConfig;

#[derive(Parser, Debug, Clone)]
#[command(
    name = "kalamar-relay",
    version,
    about = "UDP relay for LAN play of the supported game build (relay protocol v1)"
)]
pub struct Args {
    /// Address to listen on (an IPv4 or IPv6 address). 0.0.0.0 = every IPv4 interface; use 127.0.0.1 for local tests.
    #[arg(long, env = "KALAMAR_RELAY_BIND", default_value_t = IpAddr::V4(Ipv4Addr::UNSPECIFIED))]
    pub bind: IpAddr,

    /// UDP port to listen on.
    #[arg(long, env = "KALAMAR_RELAY_PORT", default_value_t = 47313)]
    pub port: u16,

    /// Threads that handle datagrams (0 = one per processor this program may use; 1 = one thread).
    #[arg(long, env = "KALAMAR_RELAY_WORKERS", default_value_t = 0)]
    pub workers: usize,

    /// Most rooms at the same time (0 = no cap). Default: a quarter of the default number of sessions (256
    /// rooms for 1024 sessions).
    #[arg(long, env = "KALAMAR_RELAY_MAX_ROOMS")]
    pub max_rooms: Option<usize>,

    /// Most sessions (members, all rooms together) at the same time (0 = no cap; the per-source limits and the
    /// HELLO and JOIN rates still bound what one address holds). Default: 1024 per worker.
    #[arg(long, env = "KALAMAR_RELAY_MAX_SESSIONS")]
    pub max_sessions: Option<usize>,

    /// Receive buffer asked for each socket, KiB (0 = the system's default): room for a moment the system does not
    /// run a worker, rather than lost datagrams. Linux gives at most `net.core.rmem_max`.
    #[arg(long, env = "KALAMAR_RELAY_RECV_BUFFER_KB", default_value_t = crate::server::DEFAULT_RECV_BUFFER_KB)]
    pub recv_buffer_kb: usize,

    /// Most members per room (2..=254).
    #[arg(long, env = "KALAMAR_RELAY_MAX_MEMBERS", default_value_t = 12)]
    pub max_members: usize,

    /// HELLO datagrams per second per source IP address (0 = unlimited); the excess is dropped.
    #[arg(long, env = "KALAMAR_RELAY_HELLO_RATE", default_value_t = 20)]
    pub hello_rate: u32,

    /// JOIN datagrams per second per source IP address (0 = unlimited); the excess is dropped.
    #[arg(long, env = "KALAMAR_RELAY_JOIN_RATE", default_value_t = 20)]
    pub join_rate: u32,

    /// DATA datagrams per second per member (0 = unlimited).
    #[arg(long, env = "KALAMAR_RELAY_MEMBER_PPS", default_value_t = 2000)]
    pub member_pps: u32,

    /// DATA bytes per second per member (0 = unlimited, else at least 1472).
    #[arg(long, env = "KALAMAR_RELAY_MEMBER_BYTES", default_value_t = 4 << 20)]
    pub member_bytes: u32,

    /// DATA datagrams per second per room, all members together (0 = unlimited).
    #[arg(long, env = "KALAMAR_RELAY_ROOM_PPS", default_value_t = 12_000)]
    pub room_pps: u32,

    /// DATA bytes per second per room (0 = unlimited, else at least 1472).
    #[arg(long, env = "KALAMAR_RELAY_ROOM_BYTES", default_value_t = 16 << 20)]
    pub room_bytes: u32,

    /// Keepalive interval announced to clients, seconds (1..=60).
    #[arg(long, env = "KALAMAR_RELAY_KEEPALIVE", default_value_t = 5)]
    pub keepalive: u8,

    /// Idle timeout, seconds: a member silent for longer is removed (at least 3 times the keepalive interval).
    #[arg(long, env = "KALAMAR_RELAY_IDLE_TIMEOUT", default_value_t = 30)]
    pub idle_timeout: u8,

    /// The cookie secret is replaced this often, seconds (at least 60).
    #[arg(long, env = "KALAMAR_RELAY_COOKIE_ROTATION", default_value_t = 3600)]
    pub cookie_rotation: u64,

    /// Seconds between the one-line statistics (0 = off).
    #[arg(long, env = "KALAMAR_RELAY_STATS_INTERVAL", default_value_t = 30)]
    pub stats_interval: u64,

    /// Sessions from one source (an IPv4 address or an IPv6 /64), all rooms together (0 = unlimited; loopback is
    /// never limited).
    #[arg(long, env = "KALAMAR_RELAY_MAX_SESSIONS_PER_SOURCE", default_value_t = 24)]
    pub max_sessions_per_source: usize,

    /// Members of one room from one source (0 = unlimited; loopback is never limited). 8 lets a whole 4 v 4 play from
    /// behind one box; more members per room from one address (a LAN party with spectators) need more.
    #[arg(long, env = "KALAMAR_RELAY_MAX_MEMBERS_PER_SOURCE", default_value_t = 8)]
    pub max_members_per_source: usize,

    /// Bind this relay to one community service: a file holding the secret shared with it (hexadecimal, at least
    /// 32 digits). Only room codes that service issued can then be joined. Without it the relay refuses to start,
    /// unless `--open` says it is meant to be open.
    #[arg(long, env = "KALAMAR_RELAY_ROOM_SECRET_FILE")]
    pub room_secret_file: Option<std::path::PathBuf>,

    /// Run an open relay, without `--room-secret-file`: anyone who knows its address can open rooms on it and use
    /// its capacity (a relay for a LAN party, or for tests). A relay of a community service is bound to it instead.
    #[arg(long, env = "KALAMAR_RELAY_OPEN", conflicts_with = "room_secret_file")]
    pub open: bool,

    /// Write a new random room secret to this file and exit. The file must not exist yet; on Unix it is created
    /// readable by its owner only (0600). Give this relay `--room-secret-file <file>` and a copy of it to the service
    /// (`secret_file` of this relay's `[[relays]]` entry). One secret per relay: whoever holds a relay's secret can
    /// open rooms on it.
    #[arg(long)]
    pub new_room_secret: Option<std::path::PathBuf>,

    /// Log filter: error, warn, info, debug, trace, or a tracing directive such as `kalamar_relay=debug`.
    #[arg(long, env = "KALAMAR_RELAY_LOG_LEVEL", default_value = "info")]
    pub log_level: String,
}

/// Sessions per worker of the default cap ([`default_caps`]): a safety limit. Measured (`examples/relay_bench.rs`, the operator guide's "Capacity of a relay"), one worker relays about
/// 50 000 datagrams a second on Windows loopback: some 1 700 duels (3 400 sessions) or 60 matches of 4 v 4 (480
/// sessions). The cap sits between: the load a bound relay reports to its service is what stops new matches first.
pub const SESSIONS_PER_WORKER: usize = 1024;

/// The default caps of a relay of `workers` workers: [`SESSIONS_PER_WORKER`] sessions per worker, and a quarter as
/// many rooms. One worker gets 1024 sessions and 256 rooms.
pub fn default_caps(workers: usize) -> (usize, usize) {
    let sessions = SESSIONS_PER_WORKER * workers.max(1);
    (sessions, sessions / 4)
}

impl Args {
    pub fn socket_addr(&self) -> SocketAddr {
        SocketAddr::new(self.bind, self.port)
    }

    /// The workers `--workers` asks for (0: one per processor).
    pub fn worker_count(&self) -> usize {
        match self.workers {
            0 => crate::server::auto_workers(),
            n => n,
        }
    }

    pub fn relay_config(&self) -> Result<RelayConfig, String> {
        let (sessions, rooms) = default_caps(self.worker_count());
        let cfg = RelayConfig {
            max_rooms: self.max_rooms.unwrap_or(rooms),
            max_sessions: self.max_sessions.unwrap_or(sessions),
            max_members: self.max_members,
            hello_per_s: self.hello_rate,
            join_per_s: self.join_rate,
            member_pps: self.member_pps,
            member_bytes_per_s: self.member_bytes,
            room_pps: self.room_pps,
            room_bytes_per_s: self.room_bytes,
            keepalive_s: self.keepalive,
            idle_s: self.idle_timeout,
            cookie_rotation: Duration::from_secs(self.cookie_rotation),
            room_secret: match &self.room_secret_file {
                Some(f) => Some(crate::roomauth::RoomSecret::from_file(f)?),
                None if self.open => None,
                // Security review L-S5: the shipped units started an open relay by default.
                None => {
                    return Err(concat!(
                        "no --room-secret-file: a relay of a community service is bound to it by a secret ",
                        "(kalamar-relay --new-room-secret <file>, then --room-secret-file <file> here and ",
                        "secret_file in the service's [[relays]] entry). For a relay meant to be open to ",
                        "anyone, say so with --open"
                    )
                    .into());
                }
            },
            max_sessions_per_source: self.max_sessions_per_source,
            max_members_per_source: self.max_members_per_source,
        };
        cfg.validate()?;
        Ok(cfg)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn defaults_are_the_documented_ones() {
        let a = Args::parse_from(["kalamar-relay", "--open", "--workers", "1"]);
        assert_eq!(a.socket_addr(), "0.0.0.0:47313".parse().unwrap());
        assert_eq!(
            a.relay_config().unwrap(),
            RelayConfig::default(),
            "one worker: the caps of 1.1.12"
        );
        let a = Args::parse_from(["kalamar-relay", "--open"]);
        assert_eq!(a.workers, 0);
        assert_eq!(a.worker_count(), crate::server::auto_workers());
        let c = a.relay_config().unwrap();
        assert_eq!((c.max_sessions, c.max_rooms), default_caps(a.worker_count()));
        let c = Args::parse_from(["kalamar-relay", "--open", "--workers", "4"])
            .relay_config()
            .unwrap();
        assert_eq!(
            (c.max_sessions, c.max_rooms),
            (4 * SESSIONS_PER_WORKER, SESSIONS_PER_WORKER)
        );
    }

    /// 0 is no cap (the per-source limits and the rates stay).
    #[test]
    fn zero_means_no_cap() {
        let c = Args::parse_from(["kalamar-relay", "--open", "--max-sessions", "0", "--max-rooms", "0"])
            .relay_config()
            .unwrap();
        assert_eq!((c.max_sessions, c.max_rooms), (0, 0));
        assert_eq!((c.max_sessions_per_source, c.max_members_per_source), (24, 8));
        assert_eq!((c.hello_per_s, c.join_per_s), (20, 20));
    }

    /// Security review L-S5: without its secret the relay refuses to start, unless it is meant to be open.
    #[test]
    fn a_relay_without_its_secret_refuses_to_start_unless_open() {
        let e = Args::parse_from(["kalamar-relay"]).relay_config().unwrap_err();
        assert!(e.contains("--room-secret-file") && e.contains("--open"), "{e}");
        assert!(Args::parse_from(["kalamar-relay", "--open"]).relay_config().is_ok());
        assert!(Args::try_parse_from(["kalamar-relay", "--open", "--room-secret-file", "k"]).is_err());
    }

    #[test]
    fn flags_override_and_are_validated() {
        let a = Args::parse_from([
            "kalamar-relay",
            "--open",
            "--bind",
            "127.0.0.1",
            "--port",
            "5000",
            "--max-members",
            "4",
        ]);
        assert_eq!(a.socket_addr(), "127.0.0.1:5000".parse().unwrap());
        assert_eq!(a.relay_config().unwrap().max_members, 4);
        assert!(Args::parse_from(["kalamar-relay", "--open", "--max-members", "1"])
            .relay_config()
            .is_err());
        assert!(Args::parse_from(["kalamar-relay", "--open", "--max-members", "255"])
            .relay_config()
            .is_err());
        assert!(Args::parse_from(["kalamar-relay", "--open", "--idle-timeout", "10"])
            .relay_config()
            .is_err());
        assert!(Args::parse_from(["kalamar-relay", "--open", "--member-bytes", "100"])
            .relay_config()
            .is_err());
        assert!(Args::parse_from(["kalamar-relay", "--open", "--cookie-rotation", "10"])
            .relay_config()
            .is_err());
        assert!(Args::try_parse_from(["kalamar-relay", "--bind", "nonsense"]).is_err());
    }
}
