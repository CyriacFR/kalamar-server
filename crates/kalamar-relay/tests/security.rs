//! Regression tests of the security review (`docs/security-model.md`), relay side: each failed before
//! its fix. No socket: the relay's state machine with a simulated clock (addresses of documentation ranges).

use std::net::SocketAddr;
use std::time::{Duration, Instant};

use kalamar_relay::relay::{source_of, Outbox, Relay, RelayConfig};
use kalamar_relay::roomauth::{seal, RoomSecret};
use kalamar_relay::wire::{self, nak, Msg};

const UNIX: u64 = 1_800_000_000;
const SECRET: [u8; 32] = [7; 32];

/// The code a service bound by `SECRET` gives the `i`-th match.
fn code(i: usize) -> String {
    seal(&SECRET, &format!("{i:0>13}"), UNIX).expect("sealed")
}

/// A HELLO, then a JOIN of `room`: the session, or the JOIN_NAK reason.
fn join(relay: &mut Relay, out: &mut Outbox, now: Instant, from: SocketAddr, room: &str) -> Result<wire::Auth, u8> {
    let nonce = 0xABCD_0000_0000_0000u64 + u64::from(from.port());
    let mut h = Vec::new();
    wire::encode_hello(&mut h, nonce);
    out.clear();
    relay.handle(now, UNIX, from, &h, out);
    let cookie = out
        .iter()
        .find_map(|(_, d)| match wire::parse(d) {
            Ok(Msg::Challenge { cookie, .. }) => Some(cookie),
            _ => None,
        })
        .ok_or(0u8)?;
    let mut j = Vec::new();
    wire::encode_join(&mut j, nonce, &cookie, 0, room.as_bytes(), b"x");
    out.clear();
    relay.handle(now, UNIX, from, &j, out);
    let r = out
        .iter()
        .find_map(|(_, d)| match wire::parse(d) {
            Ok(Msg::JoinAck(a)) => Some(Ok(wire::Auth {
                session: a.session,
                token: a.token,
            })),
            Ok(Msg::JoinNak { reason, .. }) => Some(Err(reason)),
            _ => None,
        })
        .unwrap_or(Err(0));
    out.clear();
    r
}

/// M4. With codes any two accounts could mint (a room match started and voided at once), one address held all 1024
/// sessions of a bound relay, kept by keepalives: every real match got RELAY_FULL for the codes' 24 to 48 hours; and a
/// participant filled their own room (12 sessions) to keep the other players out (ROOM_FULL). Now one source (an IPv4
/// address, an IPv6 /64) holds at most `max_sessions_per_source` sessions and `max_members_per_source` per room.
#[test]
fn one_address_can_neither_fill_a_bound_relay_nor_close_a_room() {
    // The relay's own HELLO and JOIN rates (per address and second) are not what is tested here.
    let cfg = RelayConfig {
        room_secret: Some(RoomSecret::new(SECRET.to_vec())),
        hello_per_s: 0,
        join_per_s: 0,
        ..RelayConfig::default()
    };
    let (per_source, per_room) = (cfg.max_sessions_per_source, cfg.max_members_per_source);
    assert!(per_source > 0 && per_room > 0 && per_room < cfg.max_members);
    let max_members = cfg.max_members;
    let mut now = Instant::now();
    let mut relay = Relay::new(cfg, now);
    let mut out = Outbox::new();
    let attacker = |port: u16| -> SocketAddr { format!("203.0.113.7:{port}").parse().unwrap() };
    let mut sessions = Vec::new();
    let mut refused = Vec::new();
    let mut port = 10_000u16;
    for c in 0..86 {
        for _ in 0..12 {
            port += 1;
            match join(&mut relay, &mut out, now, attacker(port), &code(c)) {
                Ok(a) => sessions.push(a),
                Err(r) => refused.push(r),
            }
        }
    }
    assert_eq!(
        sessions.len(),
        per_source,
        "one address holds {} sessions",
        sessions.len()
    );
    assert_eq!(relay.session_count(), per_source);
    assert!(refused.iter().all(|r| *r == nak::RELAY_FULL || *r == nak::ROOM_FULL));
    // A real match of the service, from elsewhere, still finds room; so do the other players of the attacker's rooms,
    // as many as the places the source limit leaves.
    assert!(join(
        &mut relay,
        &mut out,
        now,
        "198.51.100.33:50000".parse().unwrap(),
        &code(500)
    )
    .is_ok());
    for k in 0..(max_members - per_room) as u16 {
        let other: SocketAddr = format!("198.51.100.{}:50000", 40 + k).parse().unwrap();
        assert!(
            join(&mut relay, &mut out, now, other, &code(0)).is_ok(),
            "member {k} of a room the attacker filled"
        );
    }
    // An IPv6 /64 is one source: other interface ids of it get no more.
    let v6 = |i: u16| -> SocketAddr { format!("[2001:db8:7:7::{i:x}]:4000").parse().unwrap() };
    let mut room = Vec::new();
    for i in 1..=(per_room + 1) as u16 {
        room.push(join(&mut relay, &mut out, now, v6(i), &code(600)));
    }
    assert_eq!(room.iter().filter(|r| r.is_ok()).count(), per_room);
    assert_eq!(room.last(), Some(&Err(nak::ROOM_FULL)));
    assert_eq!(source_of(v6(1)), source_of(v6(0xffff)));
    // Loopback (the relay's own machine: local tests) is never limited.
    assert_eq!(source_of("127.0.0.1:5000".parse().unwrap()), None);
    for p in 0..5u16 {
        let local: SocketAddr = format!("127.0.0.1:{}", 6000 + p).parse().unwrap();
        assert!(join(&mut relay, &mut out, now, local, &code(700)).is_ok());
    }
    // Sessions that end give their place back.
    let before = relay.source_sessions(attacker(10_001));
    now += Duration::from_secs(60);
    relay.tick(now, &mut out);
    assert_eq!(relay.session_count(), 0, "idle sessions end");
    assert_eq!((before, relay.source_sessions(attacker(10_001))), (per_source, 0));
}

/// 1.1.13: a refusal by a per-source limit says so (detail 1 in byte 17 of the JOIN_NAK), a full room or relay does
/// not (0): a client can tell its player which it is.
#[test]
fn a_per_source_refusal_carries_its_detail() {
    let cfg = RelayConfig {
        room_secret: Some(RoomSecret::new(SECRET.to_vec())),
        hello_per_s: 0,
        join_per_s: 0,
        max_sessions: 30,
        ..RelayConfig::default()
    };
    let now = Instant::now();
    let mut relay = Relay::new(cfg, now);
    let mut out = Outbox::new();
    let nak_of = |relay: &mut Relay, out: &mut Outbox, from: SocketAddr, room: &str| -> Option<(u8, u8)> {
        let nonce = 0x5EED_0000_0000_0000u64 + u64::from(from.port());
        let mut h = Vec::new();
        wire::encode_hello(&mut h, nonce);
        out.clear();
        relay.handle(now, UNIX, from, &h, out);
        let cookie = out.iter().find_map(|(_, d)| match wire::parse(d) {
            Ok(Msg::Challenge { cookie, .. }) => Some(cookie),
            _ => None,
        })?;
        let mut j = Vec::new();
        wire::encode_join(&mut j, nonce, &cookie, 0, room.as_bytes(), b"x");
        out.clear();
        relay.handle(now, UNIX, from, &j, out);
        out.iter().find_map(|(_, d)| match wire::parse(d) {
            Ok(Msg::JoinNak { reason, detail, .. }) => Some((reason, detail)),
            _ => None,
        })
    };
    let one = |port: u16| -> SocketAddr { format!("203.0.113.9:{port}").parse().unwrap() };
    // 8 members of a room from one address, then the 9th: ROOM_FULL, detail 1
    for p in 0..8 {
        assert_eq!(nak_of(&mut relay, &mut out, one(1000 + p), &code(1)), None);
    }
    assert_eq!(
        nak_of(&mut relay, &mut out, one(1100), &code(1)),
        Some((nak::ROOM_FULL, nak::detail::SOURCE))
    );
    // 24 sessions of the relay from one address, then the 25th: RELAY_FULL, detail 1
    for p in 0..16 {
        assert_eq!(
            nak_of(&mut relay, &mut out, one(2000 + p), &code(2 + usize::from(p / 8))),
            None
        );
    }
    assert_eq!(
        nak_of(&mut relay, &mut out, one(2100), &code(9)),
        Some((nak::RELAY_FULL, nak::detail::SOURCE))
    );
    // the relay itself full (30 sessions): RELAY_FULL, detail 0, whoever asks
    for p in 0..6u16 {
        let other: SocketAddr = format!("198.51.100.{}:3000", 10 + p).parse().unwrap();
        assert_eq!(nak_of(&mut relay, &mut out, other, &code(20)), None);
    }
    let late: SocketAddr = "198.51.100.99:3000".parse().unwrap();
    assert_eq!(
        nak_of(&mut relay, &mut out, late, &code(21)),
        Some((nak::RELAY_FULL, nak::detail::NONE))
    );
}
