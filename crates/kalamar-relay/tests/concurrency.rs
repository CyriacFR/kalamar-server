//! Several workers at once (protocol 1.1.13: `--workers`): the races that could let a protection slip, each run many
//! times on real threads (the state machine has no sockets: the threads call `Relay::handle_on` as the workers do).
//! Addresses are of documentation ranges, so that the per-source limits apply (loopback is exempt from them).

use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering::SeqCst};
use std::sync::{Arc, Barrier};
use std::time::{Duration, Instant};

use kalamar_relay::cookie::CookieKeys;
use kalamar_relay::relay::{Outbox, Relay, RelayConfig};
use kalamar_relay::wire::{self, nak, Auth, DataHeader, Msg};

const UNIX: u64 = 1_800_000_000;

fn relay(cfg: RelayConfig, workers: usize) -> (Arc<Relay>, Instant) {
    let now = Instant::now();
    let cfg = RelayConfig {
        hello_per_s: 0,
        join_per_s: 0,
        ..cfg
    };
    let cookies = CookieKeys::with_secret([9; 32], now, cfg.cookie_rotation);
    (Arc::new(Relay::with_workers(cfg, cookies, now, workers)), now)
}

fn at(net: u8, host: u8, port: u16) -> SocketAddr {
    format!("198.51.{net}.{host}:{port}").parse().unwrap()
}

/// The JOIN of `from` for `room`, with its cookie (asked with a HELLO first).
fn join_bytes(r: &Relay, now: Instant, from: SocketAddr, room: &str, nonce: u64) -> Vec<u8> {
    let mut h = Vec::new();
    wire::encode_hello(&mut h, nonce);
    let mut out = Outbox::new();
    r.handle(now, UNIX, from, &h, &mut out);
    let cookie = out
        .iter()
        .find_map(|(_, d)| match wire::parse(d) {
            Ok(Msg::Challenge { cookie, .. }) => Some(cookie),
            _ => None,
        })
        .expect("CHALLENGE");
    let mut j = Vec::new();
    wire::encode_join(&mut j, nonce, &cookie, 0, room.as_bytes(), b"p");
    j
}

/// What the relay answered `from` in `out`: the session, or the JOIN_NAK's reason and detail.
fn answer(out: &Outbox, from: SocketAddr) -> Result<(Auth, u32), (u8, u8)> {
    out.iter()
        .filter(|(to, _)| *to == from)
        .find_map(|(_, d)| match wire::parse(d) {
            Ok(Msg::JoinAck(a)) => Some(Ok((
                Auth {
                    session: a.session,
                    token: a.token,
                },
                a.vip,
            ))),
            Ok(Msg::JoinNak { reason, detail, .. }) => Some(Err((reason, detail))),
            _ => None,
        })
        .expect("an answer to the JOIN")
}

fn join(r: &Relay, now: Instant, from: SocketAddr, room: &str) -> Result<(Auth, u32), (u8, u8)> {
    let j = join_bytes(r, now, from, room, 0x5000 + u64::from(from.port()));
    let mut out = Outbox::new();
    r.handle(now, UNIX, from, &j, &mut out);
    answer(&out, from)
}

/// `n` JOINs prepared one by one, then sent at the same instant from `n` threads (one worker each).
fn race(r: &Arc<Relay>, now: Instant, joins: Vec<(SocketAddr, Vec<u8>)>) -> Vec<Result<(Auth, u32), (u8, u8)>> {
    let gate = Barrier::new(joins.len());
    std::thread::scope(|s| {
        let handles: Vec<_> = joins
            .iter()
            .enumerate()
            .map(|(w, (from, j))| {
                let (r, gate) = (r.clone(), &gate);
                s.spawn(move || {
                    let mut out = Outbox::new();
                    gate.wait();
                    r.handle_on(w, now, UNIX, *from, j, &mut out);
                    answer(&out, *from)
                })
            })
            .collect();
        handles.into_iter().map(|h| h.join().unwrap()).collect()
    })
}

/// Two JOINs for the last place of a room, at the same instant: one gets it, the other ROOM_FULL; never 13 of 12.
#[test]
fn two_joins_race_for_the_last_place_of_a_room() {
    for round in 0..300u16 {
        let (r, now) = relay(RelayConfig::default(), 4);
        let room = format!("LAST-{round}");
        for k in 0..11u8 {
            assert!(join(&r, now, at(1, k + 1, 1000), &room).is_ok());
        }
        let racers: Vec<(SocketAddr, Vec<u8>)> = (0..3u8)
            .map(|k| {
                let a = at(2, k + 1, 2000);
                (a, join_bytes(&r, now, a, &room, 0x7000 + k as u64))
            })
            .collect();
        let got = race(&r, now, racers);
        assert_eq!(got.iter().filter(|g| g.is_ok()).count(), 1, "round {round}: {got:?}");
        assert!(got
            .iter()
            .filter_map(|g| g.err())
            .all(|e| e == (nak::ROOM_FULL, nak::detail::NONE)));
        assert_eq!(r.member_count(room.as_bytes()), 12);
        assert_eq!(r.session_count(), 12);
    }
}

/// The last session of the relay, the last room of the relay, the last place of a source: one JOIN each, whatever
/// the timing.
#[test]
fn the_relays_and_a_sources_last_places_go_to_one_join() {
    for round in 0..200u16 {
        // max_sessions: 4 sessions, 3 taken, 4 racers in 4 new rooms
        let cfg = RelayConfig {
            max_sessions: 4,
            max_rooms: 0,
            ..RelayConfig::default()
        };
        let (r, now) = relay(cfg, 4);
        for k in 0..3u8 {
            assert!(join(&r, now, at(3, k + 1, 1000), "TAKEN").is_ok());
        }
        let racers = (0..4u8)
            .map(|k| {
                let a = at(4, k + 1, 3000);
                (a, join_bytes(&r, now, a, &format!("NEW-{k}"), 0x8000 + k as u64))
            })
            .collect();
        let got = race(&r, now, racers);
        assert_eq!(got.iter().filter(|g| g.is_ok()).count(), 1, "round {round}: {got:?}");
        assert!(got
            .iter()
            .filter_map(|g| g.err())
            .all(|e| e == (nak::RELAY_FULL, nak::detail::NONE)));
        assert_eq!((r.session_count(), r.room_count()), (4, 2));

        // max_rooms: 2 rooms, 1 taken, 4 racers each opening a room
        let cfg = RelayConfig {
            max_rooms: 2,
            max_sessions: 0,
            ..RelayConfig::default()
        };
        let (r, now) = relay(cfg, 4);
        assert!(join(&r, now, at(5, 1, 1000), "TAKEN").is_ok());
        let racers = (0..4u8)
            .map(|k| {
                let a = at(6, k + 1, 3000);
                (a, join_bytes(&r, now, a, &format!("OPEN-{k}"), 0x9000 + k as u64))
            })
            .collect();
        let got = race(&r, now, racers);
        assert_eq!(got.iter().filter(|g| g.is_ok()).count(), 1, "round {round}: {got:?}");
        assert_eq!(r.room_count(), 2);

        // one source's last place in a room (8 per room) and on the relay (24): one address, many ports
        let (r, now) = relay(RelayConfig::default(), 4);
        for p in 0..7u16 {
            assert!(join(&r, now, at(7, 7, 1000 + p), "SRC").is_ok());
        }
        let racers = (0..4u16)
            .map(|p| {
                let a = at(7, 7, 5000 + p);
                (a, join_bytes(&r, now, a, "SRC", 0xA000 + p as u64))
            })
            .collect();
        let got = race(&r, now, racers);
        assert_eq!(got.iter().filter(|g| g.is_ok()).count(), 1, "round {round}: {got:?}");
        assert!(got
            .iter()
            .filter_map(|g| g.err())
            .all(|e| e == (nak::ROOM_FULL, nak::detail::SOURCE)));
        assert_eq!(r.source_sessions(at(7, 7, 1)), 8);
    }
}

/// Members leave one by one (the room closes) while three workers relay their datagrams: no datagram goes to a
/// member after its LEAVE was handled, nothing panics, and nothing is left.
#[test]
fn a_room_closes_while_its_datagrams_are_relayed() {
    for round in 0..40u16 {
        let (r, now) = relay(RelayConfig::default(), 4);
        let members: Vec<(SocketAddr, Auth, u32)> = (0..8u8)
            .map(|k| {
                let a = at(8, k + 1, 4000);
                let (auth, vip) = join(&r, now, a, "CLOSING").unwrap();
                (a, auth, vip)
            })
            .collect();
        let left: Vec<AtomicBool> = (0..8).map(|_| AtomicBool::new(false)).collect();
        let done = AtomicBool::new(false);
        let late = AtomicUsize::new(0);
        std::thread::scope(|s| {
            for w in 0..3usize {
                let (r, members, left, done, late) = (r.clone(), &members, &left, &done, &late);
                s.spawn(move || {
                    let mut out = Outbox::new();
                    let mut i = w;
                    while !done.load(SeqCst) {
                        let (from, auth, vip) = members[i % 8];
                        i += 1;
                        // who had left before this datagram was handed to the relay
                        let gone: Vec<SocketAddr> =
                            (0..8).filter(|&k| left[k].load(SeqCst)).map(|k| members[k].0).collect();
                        let h = DataHeader {
                            src_vip: vip,
                            src_port: 1,
                            dst_vip: wire::VBCAST,
                            dst_port: 2,
                        };
                        let mut d = Vec::new();
                        wire::encode_data(&mut d, &auth, &h, b"payload");
                        out.clear();
                        r.handle_on(w + 1, now, UNIX, from, &d, &mut out);
                        let to_gone = out
                            .iter()
                            .filter(|(to, b)| gone.contains(to) && b[5] == wire::ty::DATA)
                            .count();
                        late.fetch_add(to_gone, SeqCst);
                    }
                });
            }
            let (r, members, left, done) = (r.clone(), &members, &left, &done);
            s.spawn(move || {
                let mut out = Outbox::new();
                for (k, (from, auth, _)) in members.iter().enumerate() {
                    std::thread::sleep(Duration::from_micros(300));
                    let mut l = Vec::new();
                    wire::encode_leave(&mut l, auth);
                    r.handle_on(0, now, UNIX, *from, &l, &mut out);
                    left[k].store(true, SeqCst);
                }
                std::thread::sleep(Duration::from_millis(2));
                done.store(true, SeqCst);
            });
        });
        assert_eq!(
            late.load(SeqCst),
            0,
            "round {round}: datagrams to members that had left"
        );
        assert_eq!((r.session_count(), r.room_count()), (0, 0));
        assert_eq!(r.source_sessions(at(8, 1, 1)), 0);
        assert_eq!(r.stats().leaves, 8);
    }
}

/// Many workers join, relay, rebind, leave and time out in many rooms at once: the limits hold all along and the
/// bookkeeping is exact at the end (sessions, rooms, per-source counts), and no reply to an address that is not a
/// member is ever larger than its request.
#[test]
fn limits_and_counts_hold_under_concurrent_churn() {
    let cfg = RelayConfig {
        max_sessions: 60,
        max_rooms: 12,
        max_members: 6,
        max_members_per_source: 3,
        max_sessions_per_source: 10,
        ..RelayConfig::default()
    };
    let (r, now) = relay(cfg.clone(), 6);
    let stop = AtomicBool::new(false);
    let violations = AtomicUsize::new(0);
    std::thread::scope(|s| {
        for w in 0..6usize {
            let (r, stop, violations, cfg) = (r.clone(), &stop, &violations, &cfg);
            s.spawn(move || {
                let mut x = 0x9E37_79B9_7F4A_7C15u64 ^ (w as u64 + 1);
                let mut rnd = move |n: u64| {
                    x ^= x << 13;
                    x ^= x >> 7;
                    x ^= x << 17;
                    x % n
                };
                let mut mine: Vec<(SocketAddr, Auth, u32)> = Vec::new();
                let mut out = Outbox::new();
                let mut port = 10_000 + w as u16 * 1000;
                let mut moved_port = 40_000 + w as u16 * 1000;
                for _ in 0..3000 {
                    if stop.load(SeqCst) {
                        break;
                    }
                    match rnd(10) {
                        0..=2 => {
                            port += 1;
                            // few sources, so that the per-source limits bite
                            let from = at(9, 1 + rnd(4) as u8, port);
                            let room = format!("CHURN-{}", rnd(16));
                            let j = join_bytes(&r, now, from, &room, port as u64);
                            out.clear();
                            r.handle_on(w, now, UNIX, from, &j, &mut out);
                            for (to, b) in out.iter() {
                                if to == from && b[5] != wire::ty::JOIN_ACK && b.len() > j.len() {
                                    violations.fetch_add(1, SeqCst);
                                }
                            }
                            if let Ok((auth, vip)) = answer(&out, from) {
                                mine.push((from, auth, vip));
                            }
                        }
                        3 if !mine.is_empty() => {
                            let (from, auth, _) = mine.swap_remove(rnd(mine.len() as u64) as usize);
                            let mut l = Vec::new();
                            wire::encode_leave(&mut l, &auth);
                            out.clear();
                            r.handle_on(w, now, UNIX, from, &l, &mut out);
                        }
                        4 if !mine.is_empty() => {
                            // NAT rebinding: a keepalive from an address and port no JOIN uses
                            let k = rnd(mine.len() as u64) as usize;
                            let (_, auth, vip) = mine[k];
                            moved_port += 1;
                            let moved = at(10, 1 + rnd(4) as u8, moved_port);
                            let mut ka = Vec::new();
                            wire::encode_keepalive(&mut ka, &auth, 1);
                            out.clear();
                            r.handle_on(w, now, UNIX, moved, &ka, &mut out);
                            if out.iter().any(|(_, b)| b[5] == wire::ty::KEEPALIVE_ACK) {
                                mine[k] = (moved, auth, vip);
                            }
                        }
                        _ if !mine.is_empty() => {
                            let (from, auth, vip) = mine[rnd(mine.len() as u64) as usize];
                            let h = DataHeader {
                                src_vip: vip,
                                src_port: 1,
                                dst_vip: wire::VBCAST,
                                dst_port: 2,
                            };
                            let mut d = Vec::new();
                            wire::encode_data(&mut d, &auth, &h, b"x");
                            out.clear();
                            r.handle_on(w, now, UNIX, from, &d, &mut out);
                        }
                        _ => {}
                    }
                    assert!(r.session_count() <= cfg.max_sessions);
                    assert!(r.room_count() <= cfg.max_rooms);
                }
            });
        }
        // the periodic work at the same time (nothing is idle yet: it only takes the locks)
        let (r, stop) = (r.clone(), &stop);
        s.spawn(move || {
            let mut out = Outbox::new();
            for _ in 0..200 {
                r.tick(now, &mut out);
                std::thread::sleep(Duration::from_micros(200));
            }
            let _ = stop;
        });
    });
    stop.store(true, SeqCst);
    assert_eq!(violations.load(SeqCst), 0);
    let sizes = r.room_sizes();
    assert!(sizes.iter().all(|(_, n)| (1..=cfg.max_members).contains(n)));
    assert_eq!(sizes.iter().map(|(_, n)| n).sum::<usize>(), r.session_count());
    let s = r.stats();
    assert_eq!(
        s.joins - s.leaves - s.timeouts,
        r.session_count() as u64,
        "every session counted once ({s:?})"
    );
    let held: usize = (1..=4u8)
        .map(|h| r.source_sessions(at(9, h, 1)) + r.source_sessions(at(10, h, 1)))
        .sum();
    assert_eq!(held, r.session_count(), "per-source counts match the sessions");
    // everything times out: nothing left, per source either
    let mut out = Outbox::new();
    r.tick(now + Duration::from_secs(120), &mut out);
    assert_eq!((r.session_count(), r.room_count()), (0, 0));
    for h in 1..=4u8 {
        assert_eq!(r.source_sessions(at(9, h, 1)) + r.source_sessions(at(10, h, 1)), 0);
    }
}
