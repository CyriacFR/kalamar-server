//! Fuzz-style tests: random and mutated datagrams from many addresses must never panic the relay, never make it
//! send to an unproven address, never make it amplify, and never break its internal limits.

mod common;

use std::net::SocketAddr;
use std::time::Duration;

use common::*;
use kalamar_relay::relay::RelayConfig;
use kalamar_relay::wire::{self, Auth, DataHeader};

fn src(r: &mut Rng) -> SocketAddr {
    // a small population so that sessions, rebinds and retransmissions actually collide
    addr(1 + r.below(24) as u32)
}

#[test]
fn pure_random_datagrams() {
    let mut w = World::new(RelayConfig::default());
    let mut r = Rng(0x9E37_79B9_7F4A_7C15);
    for _ in 0..200_000 {
        let n = match r.below(4) {
            0 => r.below(8),
            1 => r.below(64),
            2 => r.below(200),
            _ => r.below(1600),
        };
        let b = r.bytes(n);
        let s = src(&mut r);
        w.send(s, &b);
        w.inbox.clear();
    }
    w.assert_clean();
    assert_eq!(w.relay.session_count(), 0, "random bytes never open a session");
}

#[test]
fn random_bytes_behind_a_valid_header() {
    let mut w = World::new(RelayConfig::default());
    let mut r = Rng(0xD1B5_4A32_D192_ED03);
    let types = [1u8, 2, 3, 4, 5, 6, 7, 8, 9, 0x10, 0x11, 0, 0x12, 0xFF];
    for _ in 0..200_000 {
        let lim = if r.below(2) == 0 { 120 } else { 1465 };
        let n = 8 + r.below(lim);
        let mut b = r.bytes(n);
        b[..4].copy_from_slice(b"S3LR");
        b[4] = if r.below(8) == 0 { r.next() as u8 } else { 1 };
        b[5] = types[r.below(types.len())];
        let s = src(&mut r);
        w.send(s, &b);
        w.inbox.clear();
    }
    w.assert_clean();
}

/// Valid traffic from live sessions with random byte flips, truncations, extensions and wrong sources.
#[test]
fn mutated_valid_traffic_with_live_sessions() {
    let cfg = RelayConfig {
        max_members: 6,
        max_rooms: 4,
        max_sessions: 16,
        hello_per_s: 0,
        join_per_s: 0,
        member_pps: 500,
        room_pps: 800,
        ..RelayConfig::default()
    };
    let mut w = World::new(cfg.clone());
    let mut r = Rng(0x2545_F491_4F6C_DD1D);
    let mut clients: Vec<Client> = (1..=24).map(|n| Client::new(n, "fuzz")).collect();
    let rooms = ["A", "B", "C", "D", "E"];

    for step in 0..60_000u32 {
        let i = r.below(clients.len());
        let kind = r.below(10);
        let bytes: Vec<u8> = match kind {
            0 | 1 => {
                // (re)join, sometimes with a mangled cookie
                let room = rooms[r.below(rooms.len())];
                let c = &mut clients[i];
                if r.below(4) == 0 {
                    c.join_with_cookie(&mut w, room, r.next() as u32, &[r.next() as u8; 16]);
                } else {
                    let want = if r.below(2) == 0 {
                        0
                    } else {
                        vip(r.below(3) as u8, 1 + r.below(250) as u8)
                    };
                    c.join(&mut w, room, want);
                }
                continue;
            }
            2 => hello_bytes(r.next()),
            3 => {
                let c = &clients[i];
                match c.session {
                    Some(_) => {
                        let mut v = Vec::new();
                        wire::encode_keepalive(&mut v, &c.auth(), step);
                        v
                    }
                    None => continue,
                }
            }
            4..=6 => {
                let c = &clients[i];
                if c.session.is_none() {
                    continue;
                }
                let dst = match r.below(4) {
                    0 => wire::VBCAST,
                    1 => wire::LIMITED_BCAST,
                    2 => vip(0, 1 + r.below(8) as u8),
                    _ => r.next() as u32,
                };
                let len = r.below(1441);
                let p = r.bytes(len);
                c.data_bytes(dst, &p)
            }
            7 => {
                let c = &clients[i];
                if c.session.is_none() {
                    continue;
                }
                let mut v = Vec::new();
                wire::encode_leave(&mut v, &c.auth());
                v
            }
            8 => {
                // someone else's credentials
                let j = r.below(clients.len());
                match clients[j].session {
                    Some(s) => c_data(&s.auth, s.vip, &mut r),
                    None => continue,
                }
            }
            _ => {
                let mut v = Vec::new();
                wire::encode_keepalive(
                    &mut v,
                    &Auth {
                        session: r.next() as u32,
                        token: [r.next() as u8; 8],
                    },
                    1,
                );
                v
            }
        };
        let mut b = bytes;
        // mutate
        match r.below(6) {
            0 => {
                for _ in 0..1 + r.below(4) {
                    if !b.is_empty() {
                        let k = r.below(b.len());
                        b[k] ^= 1 << r.below(8);
                    }
                }
            }
            1 => b.truncate(r.below(b.len() + 1)),
            2 => {
                let k = r.below(300);
                b.extend(r.bytes(k))
            }
            _ => {}
        }
        // from the owner's address, or from a random one
        let from = if r.below(5) == 0 { src(&mut r) } else { clients[i].addr };
        w.send(from, &b);
        if step % 500 == 0 {
            w.advance(Duration::from_secs(1 + r.below(12) as u64));
        }
        if w.inbox.len() > 64 {
            w.inbox.clear();
        }
        // limits hold at every step
        assert!(w.relay.session_count() <= cfg.max_sessions);
        assert!(w.relay.room_count() <= cfg.max_rooms);
        for (_, n) in w.relay.room_sizes() {
            assert!(n <= cfg.max_members);
        }
    }
    w.assert_clean();
    let s = w.relay.stats();
    assert!(
        s.joins > 100,
        "the fuzz run actually joined sessions ({} joins)",
        s.joins
    );
    assert!(s.forwarded > 100, "and forwarded data ({})", s.forwarded);
}

fn c_data(auth: &Auth, vip: u32, r: &mut Rng) -> Vec<u8> {
    let mut v = Vec::new();
    let h = DataHeader {
        src_vip: vip,
        src_port: 1,
        dst_vip: wire::VBCAST,
        dst_port: r.next() as u16,
    };
    let k = r.below(50);
    wire::encode_data(&mut v, auth, &h, &r.bytes(k));
    v
}

#[test]
fn every_prefix_and_single_byte_mutation_of_every_message_type() {
    let mut w = World::new(RelayConfig::default());
    let mut a = Client::new(1, "A");
    let mut b = Client::new(2, "B");
    a.join(&mut w, "R", 0);
    b.join(&mut w, "R", 0);
    let mut msgs = vec![
        hello_bytes(7),
        join_bytes(7, &[1; 16], 0, "ROOM", "name"),
        a.data_bytes(b.vip(), b"payload"),
        a.data_bytes(wire::VBCAST, b""),
    ];
    let mut v = Vec::new();
    wire::encode_keepalive(&mut v, &a.auth(), 1);
    msgs.push(v.clone());
    v.clear();
    wire::encode_closed(&mut v, &a.auth(), 1);
    msgs.push(v.clone());
    v.clear();
    wire::encode_member(&mut v, &a.auth(), 1, 5, b"x");
    msgs.push(v.clone());
    // a copy of each message from every sender state: unknown, member a, member b
    for m in &msgs {
        for n in 0..=m.len() {
            for s in [addr(300), a.addr, b.addr] {
                w.send(s, &m[..n]);
            }
        }
        for k in 0..m.len() {
            for bit in 0..8 {
                let mut x = m.clone();
                x[k] ^= 1 << bit;
                w.send(a.addr, &x);
                w.send(addr(301), &x);
            }
        }
        w.inbox.clear();
    }
    w.assert_clean();
}

#[test]
fn oversized_datagrams_are_dropped_without_allocation_growth() {
    let mut w = World::new(RelayConfig::default());
    let big = vec![b'S'; 65_507];
    for _ in 0..100 {
        w.send(addr(1), &big);
    }
    let mut h = hello_bytes(1);
    h.resize(1473, 0);
    w.send(addr(1), &h);
    assert!(w.take(addr(1)).is_empty());
    assert_eq!(w.relay.stats().tx, 0);
}
