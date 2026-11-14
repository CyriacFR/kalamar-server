//! State-machine tests with simulated clients and a manual clock (no sockets).

mod common;

use std::time::Duration;

use common::*;
use kalamar_relay::relay::{DropReason, RelayConfig};
use kalamar_relay::wire::{self, closed, nak, Msg};

fn joined(r: JoinResult) -> Session {
    match r {
        JoinResult::Joined(s) => s,
        other => panic!("expected a join, got {other:?}"),
    }
}

// ---- handshake ----------------------------------------------------------------------------------------------

#[test]
fn join_assigns_session_token_and_lowest_vip() {
    let mut w = World::new(small_cfg());
    let mut a = Client::new(1, "Player");
    let mut b = Client::new(2, "Omar");
    let sa = joined(a.join(&mut w, "ROOM-1", 0));
    let sb = joined(b.join(&mut w, "ROOM-1", 0));
    assert_eq!(sa.vip, vip(0, 1));
    assert_eq!(sb.vip, vip(0, 2));
    assert_ne!(sa.auth.session, 0);
    assert_ne!(sa.auth.session, sb.auth.session);
    assert_ne!(sa.auth.token, sb.auth.token);
    assert_eq!(w.relay.member_count(b"ROOM-1"), 2);
    w.assert_clean();
}

#[test]
fn join_ack_fields() {
    let mut w = World::new(small_cfg());
    let c = addr(1);
    w.send(c, &hello_bytes(77));
    let cookie = match wire::parse(&w.take(c)[0]).unwrap() {
        Msg::Challenge { nonce, cookie } => {
            assert_eq!(nonce, 77);
            cookie
        }
        m => panic!("{m:?}"),
    };
    w.send(c, &join_bytes(77, &cookie, 0, "R", "x"));
    let d = w.take(c);
    assert_eq!(d[0].len(), 40);
    let Msg::JoinAck(a) = wire::parse(&d[0]).unwrap() else {
        panic!()
    };
    assert_eq!(
        (a.nonce, a.prefix, a.max_members, a.keepalive_s, a.idle_s, a.max_payload),
        (77, 16, 12, 5, 30, 1440)
    );
    w.assert_clean();
}

#[test]
fn member_messages_both_ways() {
    let mut w = World::new(small_cfg());
    let mut a = Client::new(1, "Player");
    let mut b = Client::new(2, "Omar");
    a.join(&mut w, "R", 0);
    assert!(w.take(a.addr).is_empty());
    b.join(&mut w, "R", 0);
    // a hears about b joining; b (after its ack) hears about a
    let to_a = w.take(a.addr);
    let Msg::Member {
        auth,
        event,
        vip: v,
        name,
    } = wire::parse(&to_a[0]).unwrap()
    else {
        panic!()
    };
    assert_eq!((auth, event, v, name), (a.auth(), 1, b.vip(), &b"Omar"[..]));
    // b's inbox was drained by join(); join it again to see the MEMBER about a: re-run with a third member
    let mut c = Client::new(3, "Third");
    c.join(&mut w, "R", 0);
    w.take(a.addr);
    w.take(b.addr);
    c.leave(&mut w);
    for x in [&a, &b] {
        let m = w.take(x.addr);
        let Msg::Member {
            event, vip: v, name, ..
        } = wire::parse(&m[0]).unwrap()
        else {
            panic!()
        };
        assert_eq!((event, v, name), (2, c.vip(), &b"Third"[..]));
    }
    w.assert_clean();
}

#[test]
fn newcomer_is_told_about_existing_members() {
    let mut w = World::new(small_cfg());
    let mut a = Client::new(1, "Player");
    a.join(&mut w, "R", 0);
    // do the handshake by hand to keep the post-ack datagrams
    let c = addr(2);
    w.send(c, &hello_bytes(5));
    let Msg::Challenge { cookie, .. } = wire::parse(&w.take(c)[0]).unwrap() else {
        panic!()
    };
    w.send(c, &join_bytes(5, &cookie, 0, "R", "Newbie"));
    let d = w.take(c);
    assert_eq!(d.len(), 2);
    assert!(matches!(wire::parse(&d[0]), Ok(Msg::JoinAck(_))));
    let Ok(Msg::Member {
        event: 1, vip: v, name, ..
    }) = wire::parse(&d[1])
    else {
        panic!()
    };
    assert_eq!((v, name), (a.vip(), &b"Player"[..]));
    w.assert_clean();
}

#[test]
fn bad_cookie_gets_nak_2_and_creates_nothing() {
    let mut w = World::new(small_cfg());
    let c = addr(1);
    w.send(c, &join_bytes(1, &[9; 16], 0, "R", "x"));
    let d = w.take(c);
    assert_eq!(d.len(), 1);
    assert_eq!(
        wire::parse(&d[0]),
        Ok(Msg::JoinNak {
            version: 1,
            nonce: 1,
            reason: nak::COOKIE,
            detail: nak::detail::NONE
        })
    );
    assert_eq!((w.relay.session_count(), w.relay.room_count()), (0, 0));
    w.assert_clean();
}

#[test]
fn cookie_is_bound_to_source_and_nonce_and_expires() {
    let mut w = World::new(small_cfg());
    let (c1, c2) = (addr(1), addr(2));
    w.send(c1, &hello_bytes(5));
    let Msg::Challenge { cookie, .. } = wire::parse(&w.take(c1)[0]).unwrap() else {
        panic!()
    };
    // another source address, same cookie and nonce
    w.send(c2, &join_bytes(5, &cookie, 0, "R", "x"));
    assert!(matches!(
        wire::parse(&w.take(c2)[0]),
        Ok(Msg::JoinNak { reason: 2, .. })
    ));
    // another nonce
    w.send(c1, &join_bytes(6, &cookie, 0, "R", "x"));
    assert!(matches!(
        wire::parse(&w.take(c1)[0]),
        Ok(Msg::JoinNak { reason: 2, .. })
    ));
    // too old
    w.advance(Duration::from_secs(61));
    w.send(c1, &join_bytes(5, &cookie, 0, "R", "x"));
    assert!(matches!(
        wire::parse(&w.take(c1)[0]),
        Ok(Msg::JoinNak { reason: 2, .. })
    ));
    assert_eq!(w.relay.session_count(), 0);
    w.assert_clean();
}

#[test]
fn cookie_survives_secret_rotation_once() {
    let mut cfg = small_cfg();
    cfg.cookie_rotation = Duration::from_secs(60);
    let mut w = World::new(cfg);
    let c = addr(1);
    w.send(c, &hello_bytes(5));
    let Msg::Challenge { cookie, .. } = wire::parse(&w.take(c)[0]).unwrap() else {
        panic!()
    };
    // the rotation happens in tick() at +60 s, but the cookie lives 30 to 60 s: use a fresh one near the rotation
    w.advance(Duration::from_secs(59));
    w.send(c, &hello_bytes(6));
    let Msg::Challenge { cookie: c2, .. } = wire::parse(&w.take(c)[0]).unwrap() else {
        panic!()
    };
    w.advance(Duration::from_secs(1)); // rotation
    w.send(c, &join_bytes(6, &c2, 0, "R", "x"));
    assert!(
        matches!(wire::parse(&w.take(c)[0]), Ok(Msg::JoinAck(_))),
        "cookie issued before rotation still works"
    );
    // the very first cookie is by now 60 s old: expired regardless of the rotation
    w.send(addr(9), &join_bytes(5, &cookie, 0, "R", "x"));
    assert!(w
        .take(addr(9))
        .iter()
        .all(|d| !matches!(wire::parse(d), Ok(Msg::JoinAck(_)))));
    // after two rotations the old secret is gone: a new cookie from before cannot be replayed
    w.advance(Duration::from_secs(120));
    w.take(c);
    w.send(c, &join_bytes(6, &c2, 0, "R2", "x"));
    assert!(matches!(wire::parse(&w.take(c)[0]), Ok(Msg::JoinNak { reason: 2, .. })));
    w.assert_clean();
}

#[test]
fn cookies_differ_between_relay_instances() {
    let mut w1 = World::new(small_cfg());
    let mut w2 = World::new(small_cfg());
    let c = addr(1);
    w1.send(c, &hello_bytes(5));
    w2.send(c, &hello_bytes(5));
    assert_ne!(w1.take(c), w2.take(c));
}

#[test]
fn malformed_joins_get_the_specified_reasons() {
    let mut w = World::new(small_cfg());
    let mut c = Client::new(1, "x");
    let c_addr = c.addr;
    let mut try_join = |w: &mut World, room: &str, name: &[u8], patch: &dyn Fn(&mut Vec<u8>)| {
        c.nonce += 1;
        w.send(c_addr, &hello_bytes(c.nonce));
        let Msg::Challenge { cookie, .. } = wire::parse(&w.take(c_addr)[0]).unwrap() else {
            panic!()
        };
        let mut v = Vec::new();
        wire::encode_join(&mut v, c.nonce, &cookie, 0, room.as_bytes(), name);
        patch(&mut v);
        w.send(c_addr, &v);
        match wire::parse(&w.take(c_addr)[0]).unwrap() {
            Msg::JoinNak { reason, .. } => reason,
            _ => 0,
        }
    };
    let none = |_: &mut Vec<u8>| {};
    assert_eq!(try_join(&mut w, "lower", b"x", &none), nak::ROOM);
    assert_eq!(try_join(&mut w, "A B", b"x", &none), nak::ROOM);
    assert_eq!(try_join(&mut w, "ROOM", b"a\x00b", &none), nak::NAME);
    assert_eq!(try_join(&mut w, "ROOM", b"a\x7fb", &none), nak::NAME);
    assert_eq!(try_join(&mut w, "ROOM", b"a\x1fb", &none), nak::NAME);
    assert_eq!(try_join(&mut w, "", b"x", &none), nak::MALFORMED);
    assert_eq!(try_join(&mut w, &"A".repeat(33), b"x", &none), nak::MALFORMED);
    assert_eq!(try_join(&mut w, "ROOM", &[b'n'; 33], &none), nak::MALFORMED);
    assert_eq!(
        try_join(&mut w, "ROOM", b"x", &|v| v[36] = 40),
        nak::MALFORMED,
        "room length beyond the datagram"
    );
    assert_eq!(try_join(&mut w, "ROOM", b"x", &|v| v[36] = 0), nak::MALFORMED);
    assert_eq!(try_join(&mut w, "ROOM", b"x", &|v| v[37] = 33), nak::MALFORMED);
    assert_eq!(w.relay.session_count(), 0);
    w.assert_clean();
}

#[test]
fn utf8_name_and_max_lengths_are_accepted() {
    let mut w = World::new(small_cfg());
    let mut c = Client::new(1, "Calamar\u{e9}");
    joined(c.join(&mut w, &"A".repeat(32), 0));
    let mut d = Client::new(2, &"n".repeat(32));
    joined(d.join(&mut w, &"A".repeat(32), 0));
    // the first client hears the second one's name unchanged
    let m = w.take(c.addr);
    let Ok(Msg::Member { name, .. }) = wire::parse(&m[0]) else {
        panic!()
    };
    assert_eq!(name, "n".repeat(32).as_bytes());
    w.assert_clean();
}

#[test]
fn short_hello_and_short_join_are_ignored() {
    let mut w = World::new(small_cfg());
    let c = addr(1);
    let mut h = hello_bytes(1);
    h.truncate(63);
    w.send(c, &h);
    let mut j = join_bytes(1, &[0; 16], 0, "R", "x");
    j.truncate(63);
    w.send(c, &j);
    assert!(w.take(c).is_empty());
    w.assert_clean();
}

#[test]
fn hello_with_padding_beyond_64_is_accepted() {
    let mut w = World::new(small_cfg());
    let c = addr(1);
    let mut h = hello_bytes(1);
    h.resize(1472, 0);
    w.send(c, &h);
    let d = w.take(c);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].len(), 32);
}

#[test]
fn other_version_hello_gets_nak_1_everything_else_is_dropped() {
    let mut w = World::new(small_cfg());
    let c = addr(1);
    let mut v = Vec::new();
    wire::encode_hello_v(&mut v, 2, 99);
    w.send(c, &v);
    let d = w.take(c);
    assert_eq!(d.len(), 1);
    assert_eq!(d[0].len(), 24);
    assert_eq!(d[0][4], wire::VERSION, "the NAK carries the relay's own version");
    assert_eq!(
        wire::parse(&d[0]),
        Ok(Msg::JoinNak {
            version: 1,
            nonce: 99,
            reason: nak::VERSION,
            detail: nak::detail::NONE
        })
    );
    // a short HELLO of another version, or any other type of another version, gets nothing
    let mut short = v.clone();
    short.truncate(40);
    w.send(c, &short);
    let mut other = join_bytes(1, &[0; 16], 0, "R", "x");
    other[4] = 2;
    w.send(c, &other);
    let mut ka = Vec::new();
    wire::encode_keepalive(
        &mut ka,
        &wire::Auth {
            session: 1,
            token: [0; 8],
        },
        1,
    );
    ka[4] = 0;
    w.send(c, &ka);
    assert!(w.take(c).is_empty());
    w.assert_clean();
}

#[test]
fn wrong_way_unknown_and_garbage_are_dropped_silently() {
    let mut w = World::new(small_cfg());
    let c = addr(1);
    let mut v = Vec::new();
    wire::encode_challenge(&mut v, 1, &[0; 16]);
    w.send(c, &v);
    v.clear();
    wire::encode_join_nak(&mut v, 1, 4, 0);
    w.send(c, &v);
    v.clear();
    wire::encode_member(
        &mut v,
        &wire::Auth {
            session: 1,
            token: [0; 8],
        },
        1,
        1,
        b"x",
    );
    w.send(c, &v);
    for t in [0u8, 0x0A, 0x12, 0x20, 0xFF] {
        let mut u = hello_bytes(1);
        u[5] = t;
        w.send(c, &u);
    }
    w.send(c, b"");
    w.send(c, b"S3LR");
    w.send(c, &[0u8; 100]);
    w.send(c, &vec![b'S'; 1473]);
    let mut big = hello_bytes(1);
    big.resize(1473, 0);
    w.send(c, &big);
    assert!(w.take(c).is_empty());
    assert!(w.relay.stats().dropped_total() >= 12);
    w.assert_clean();
}

#[test]
fn port_zero_source_is_ignored() {
    let mut w = World::new(small_cfg());
    let zero = "198.51.100.1:0".parse().unwrap();
    w.send(zero, &hello_bytes(1));
    assert!(w.take(zero).is_empty());
}

// ---- anti-amplification and statelessness ------------------------------------------------------------------

#[test]
fn hello_flood_from_many_spoofed_sources_keeps_no_state() {
    let mut w = World::new(small_cfg());
    for n in 0..20_000u32 {
        // a distinct source address each time (distinct IPs, so the per-IP limit does not interfere)
        let src: std::net::SocketAddr = format!(
            "203.{}.{}.{}:{}",
            (n >> 16) & 255,
            (n >> 8) & 255,
            n & 255,
            1000 + n % 5000
        )
        .parse()
        .unwrap();
        w.send(src, &hello_bytes(n as u64));
        let d = w.take(src);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].len(), 32, "CHALLENGE");
        w.send(src, &join_bytes(n as u64, &[7; 16], 0, "R", "x")); // wrong cookie
        let d = w.take(src);
        assert_eq!(d.len(), 1);
        assert_eq!(d[0].len(), 24, "JOIN_NAK");
    }
    assert_eq!((w.relay.session_count(), w.relay.room_count()), (0, 0));
    w.assert_clean();
    assert_eq!(w.pre_proof_replies, 40_000);
}

#[test]
fn nothing_is_sent_to_an_address_that_never_completed_the_handshake() {
    let mut w = World::new(small_cfg());
    let mut a = Client::new(1, "A");
    let mut b = Client::new(2, "B");
    a.join(&mut w, "R", 0);
    b.join(&mut w, "R", 0);
    // a stranger tries everything without a valid handshake
    let stranger = addr(500);
    let mut ka = Vec::new();
    wire::encode_keepalive(&mut ka, &a.auth(), 1);
    w.send(stranger, &ka); // right session and token but another address: that is a rebind (authenticated)
                           // so use a wrong token for the pure stranger
    let stranger2 = addr(501);
    let mut bad = wire::Auth {
        session: a.auth().session,
        token: [0; 8],
    };
    ka.clear();
    wire::encode_keepalive(&mut ka, &bad, 1);
    w.send(stranger2, &ka);
    bad.session ^= 1;
    let mut d = Vec::new();
    wire::encode_data(
        &mut d,
        &bad,
        &wire::DataHeader {
            src_vip: a.vip(),
            src_port: 1,
            dst_vip: wire::VBCAST,
            dst_port: 1,
        },
        b"x",
    );
    w.send(stranger2, &d);
    w.advance(Duration::from_secs(40));
    w.assert_clean();
}

#[test]
fn closed_for_unknown_session_is_limited_and_never_larger_than_the_request() {
    let mut w = World::new(small_cfg());
    let c = addr(1);
    let ghost = wire::Auth {
        session: 0xDEAD_BEEF,
        token: [1; 8],
    };
    let mut ka = Vec::new();
    wire::encode_keepalive(&mut ka, &ghost, 1);
    for _ in 0..10 {
        w.send(c, &ka);
    }
    let d = w.take(c);
    assert_eq!(d.len(), 1, "one CLOSED per second per address");
    assert_eq!(d[0].len(), 24);
    assert_eq!(
        wire::parse(&d[0]),
        Ok(Msg::Closed {
            auth: ghost,
            reason: closed::UNKNOWN_SESSION
        })
    );
    w.advance(Duration::from_secs(1));
    w.send(c, &ka);
    assert_eq!(w.take(c).len(), 1);
    // LEAVE is never answered, and undersized KEEPALIVE / DATA are not answered either
    let mut lv = Vec::new();
    wire::encode_leave(&mut lv, &ghost);
    w.advance(Duration::from_secs(2));
    w.send(c, &lv);
    w.send(c, &ka[..27]);
    w.send(c, &ka[..20]);
    let mut dt = Vec::new();
    wire::encode_data(
        &mut dt,
        &ghost,
        &wire::DataHeader {
            src_vip: 1,
            src_port: 1,
            dst_vip: 2,
            dst_port: 2,
        },
        b"",
    );
    w.send(c, &dt[..31]);
    assert!(w.take(c).is_empty());
    w.send(c, &dt); // a 32-byte DATA: the 24-byte CLOSED fits
    assert_eq!(w.take(c)[0].len(), 24);
    w.assert_clean();
}

// ---- forwarding ----------------------------------------------------------------------------------------------

fn trio(w: &mut World) -> (Client, Client, Client) {
    let mut a = Client::new(1, "A");
    let mut b = Client::new(2, "B");
    let mut c = Client::new(3, "C");
    a.join(w, "ROOM", 0);
    b.join(w, "ROOM", 0);
    c.join(w, "ROOM", 0);
    for x in [&a, &b, &c] {
        w.take(x.addr);
    }
    (a, b, c)
}

#[test]
fn broadcast_reaches_everyone_but_the_sender_with_rewritten_header() {
    let mut w = World::new(small_cfg());
    let (a, b, c) = trio(&mut w);
    a.send_data(&mut w, wire::VBCAST, b"hello game");
    assert!(a.take_data(&mut w).is_empty(), "no echo");
    for x in [&b, &c] {
        let got = x.take_data(&mut w);
        assert_eq!(got, vec![(a.vip(), wire::VBCAST, b"hello game".to_vec())]);
    }
    // 255.255.255.255 is the same broadcast
    b.send_data(&mut w, wire::LIMITED_BCAST, b"x");
    assert_eq!(a.take_data(&mut w).len(), 1);
    assert_eq!(c.take_data(&mut w).len(), 1);
    assert!(b.take_data(&mut w).is_empty());
    w.assert_clean();
}

#[test]
fn forwarded_bytes_are_the_received_bytes_but_for_the_session_header() {
    let mut w = World::new(small_cfg());
    let (a, b, _c) = trio(&mut w);
    let payload: Vec<u8> = (0..=255u8).cycle().take(wire::MAX_PAYLOAD).collect();
    let sent = a.data_bytes(b.vip(), &payload);
    assert_eq!(sent.len(), 1472);
    w.send(a.addr, &sent);
    let got = w.take(b.addr);
    assert_eq!(got.len(), 1);
    assert_eq!(&got[0][..8], &sent[..8]);
    assert_eq!(&got[0][20..], &sent[20..]);
    assert_eq!(got[0][8..12], b.auth().session.to_be_bytes());
    assert_eq!(got[0][12..20], b.auth().token);
}

#[test]
fn unicast_reaches_only_the_destination() {
    let mut w = World::new(small_cfg());
    let (a, b, c) = trio(&mut w);
    a.send_data(&mut w, b.vip(), b"just you");
    assert_eq!(b.take_data(&mut w).len(), 1);
    assert!(c.take_data(&mut w).is_empty());
    assert!(a.take_data(&mut w).is_empty());
    // unknown destination and own address: dropped
    a.send_data(&mut w, vip(9, 9), b"nobody");
    a.send_data(&mut w, a.vip(), b"myself");
    for x in [&a, &b, &c] {
        assert!(x.take_data(&mut w).is_empty());
    }
    assert_eq!(w.relay.stats().dropped[DropReason::NoDestination as usize], 2);
    w.assert_clean();
}

#[test]
fn zero_length_payload_is_forwarded() {
    let mut w = World::new(small_cfg());
    let (a, b, _c) = trio(&mut w);
    a.send_data(&mut w, b.vip(), b"");
    assert_eq!(b.take_data(&mut w), vec![(a.vip(), b.vip(), vec![])]);
}

#[test]
fn spoofed_source_vip_is_dropped() {
    let mut w = World::new(small_cfg());
    let (a, b, _c) = trio(&mut w);
    let h = wire::DataHeader {
        src_vip: b.vip(),
        src_port: 1,
        dst_vip: wire::VBCAST,
        dst_port: 1,
    };
    let mut v = Vec::new();
    wire::encode_data(&mut v, &a.auth(), &h, b"fake");
    w.send(a.addr, &v);
    assert!(b.take_data(&mut w).is_empty());
    assert_eq!(w.relay.stats().dropped[DropReason::SpoofedSource as usize], 1);
}

#[test]
fn data_with_wrong_token_or_from_another_address_is_dropped() {
    let mut w = World::new(small_cfg());
    let (a, b, _c) = trio(&mut w);
    // right session id, wrong token (constant-time compare path)
    let mut bad = a.auth();
    bad.token[7] ^= 1;
    let h = wire::DataHeader {
        src_vip: a.vip(),
        src_port: 1,
        dst_vip: b.vip(),
        dst_port: 1,
    };
    let mut v = Vec::new();
    wire::encode_data(&mut v, &bad, &h, b"x");
    w.send(a.addr, &v);
    // right credentials, another address: DATA must come from the member's address
    let mut v = Vec::new();
    wire::encode_data(&mut v, &a.auth(), &h, b"x");
    w.send(addr(900), &v);
    assert!(b.take_data(&mut w).is_empty());
    // the CLOSED answers go to the senders of the bad datagrams only
    let _ = w.take(a.addr);
    let _ = w.take(addr(900));
    w.assert_clean();
}

#[test]
fn leave_from_another_address_is_ignored_and_never_answered() {
    let mut w = World::new(small_cfg());
    let (a, _b, _c) = trio(&mut w);
    let mut lv = Vec::new();
    wire::encode_leave(&mut lv, &a.auth());
    w.send(addr(900), &lv);
    assert_eq!(w.relay.session_count(), 3);
    assert!(w.take(addr(900)).is_empty());
    a.leave(&mut w);
    assert_eq!(w.relay.session_count(), 2);
    assert!(w.take(a.addr).is_empty(), "LEAVE is not answered");
    // and the session is gone: a second LEAVE and a KEEPALIVE
    a.leave(&mut w);
    assert!(w.take(a.addr).is_empty());
    w.assert_clean();
}

// ---- isolation --------------------------------------------------------------------------------------------

#[test]
fn rooms_are_isolated() {
    let mut w = World::new(small_cfg());
    let mut a1 = Client::new(1, "a1");
    let mut a2 = Client::new(2, "a2");
    let mut b1 = Client::new(3, "b1");
    let mut b2 = Client::new(4, "b2");
    a1.join(&mut w, "ROOM-A", 0);
    a2.join(&mut w, "ROOM-A", 0);
    b1.join(&mut w, "ROOM-B", 0);
    b2.join(&mut w, "ROOM-B", 0);
    // same vips in both rooms
    assert_eq!(
        (a1.vip(), a2.vip(), b1.vip(), b2.vip()),
        (vip(0, 1), vip(0, 2), vip(0, 1), vip(0, 2))
    );
    for x in [&a1, &a2, &b1, &b2] {
        w.take(x.addr);
    }
    a1.send_data(&mut w, wire::VBCAST, b"in A");
    assert_eq!(a2.take_data(&mut w).len(), 1);
    assert!(b1.take_data(&mut w).is_empty() && b2.take_data(&mut w).is_empty());
    a1.send_data(&mut w, vip(0, 2), b"unicast in A");
    assert_eq!(a2.take_data(&mut w).len(), 1);
    assert!(
        b2.take_data(&mut w).is_empty(),
        "same vip in another room is not reachable"
    );
    // a member of B using A's credentials on its own address cannot inject into A
    let mut v = Vec::new();
    wire::encode_data(
        &mut v,
        &a1.auth(),
        &wire::DataHeader {
            src_vip: a1.vip(),
            src_port: 1,
            dst_vip: wire::VBCAST,
            dst_port: 1,
        },
        b"x",
    );
    w.send(b1.addr, &v);
    assert!(a2.take_data(&mut w).is_empty());
    // leaving/timeouts in one room tell nobody in the other
    a1.leave(&mut w);
    assert!(w.take(b1.addr).is_empty() && w.take(b2.addr).is_empty());
    w.assert_clean();
}

// ---- keepalive, expiry, rebind -------------------------------------------------------------------------

#[test]
fn keepalive_ack_counts_members() {
    let mut w = World::new(small_cfg());
    let (mut a, _b, _c) = trio(&mut w);
    assert_eq!(a.keepalive(&mut w), Some(3));
}

#[test]
fn idle_members_expire_after_30_seconds_and_keepalives_prevent_it() {
    let mut w = World::new(small_cfg());
    let (mut a, b, mut c) = trio(&mut w);
    let mut to_a = Vec::new();
    for _ in 0..8 {
        w.advance(Duration::from_secs(5));
        to_a.extend(w.take(a.addr));
        assert!(a.keepalive(&mut w).is_some());
        c.keepalive(&mut w);
    }
    to_a.extend(w.take(a.addr));
    // 40 s passed: b never spoke
    assert_eq!(w.relay.session_count(), 2);
    let to_b = w.take(b.addr);
    assert!(to_b
        .iter()
        .any(|d| matches!(wire::parse(d), Ok(Msg::Closed { reason: 1, auth }) if auth == b.auth())));
    assert!(to_a
        .iter()
        .any(|d| matches!(wire::parse(d), Ok(Msg::Member { event: 2, vip: v, .. }) if v == b.vip())));
    // b's session is unknown now
    let mut b = b;
    assert_eq!(b.keepalive(&mut w), None);
    w.assert_clean();
}

#[test]
fn expiry_boundary_is_the_idle_timeout() {
    let mut w = World::new(small_cfg());
    let mut a = Client::new(1, "A");
    a.join(&mut w, "R", 0);
    w.advance(Duration::from_secs(30));
    assert_eq!(w.relay.session_count(), 1, "exactly 30 s is not yet over");
    w.advance(Duration::from_secs(1));
    assert_eq!(w.relay.session_count(), 0);
    assert_eq!(w.relay.room_count(), 0, "an empty room ceases to exist");
}

#[test]
fn data_counts_as_a_sign_of_life() {
    let mut w = World::new(small_cfg());
    let (a, _b, _c) = trio(&mut w);
    for _ in 0..10 {
        w.advance(Duration::from_secs(10));
        a.send_data(&mut w, wire::VBCAST, b"x");
    }
    assert_eq!(w.relay.session_count(), 1, "only the sender survives 100 s");
}

#[test]
fn dropped_data_does_not_keep_a_session_alive() {
    let mut w = World::new(small_cfg());
    let (a, _b, _c) = trio(&mut w);
    // DATA with a spoofed source vip is dropped before the liveness update
    let h = wire::DataHeader {
        src_vip: vip(7, 7),
        src_port: 1,
        dst_vip: wire::VBCAST,
        dst_port: 1,
    };
    let mut v = Vec::new();
    wire::encode_data(&mut v, &a.auth(), &h, b"x");
    for _ in 0..10 {
        w.advance(Duration::from_secs(4));
        w.send(a.addr, &v);
    }
    assert_eq!(w.relay.session_count(), 0);
}

#[test]
fn keepalive_from_a_new_address_rebinds_the_member() {
    let mut w = World::new(small_cfg());
    let (a, b, _c) = trio(&mut w);
    let new_addr = addr(777);
    let mut ka = Vec::new();
    wire::encode_keepalive(&mut ka, &a.auth(), 42);
    w.send(new_addr, &ka);
    let d = w.take(new_addr);
    assert_eq!(
        wire::parse(&d[0]),
        Ok(Msg::KeepaliveAck {
            auth: a.auth(),
            seq: 42,
            members: 3
        })
    );
    // traffic now goes to the new address, and only from it
    b.send_data(&mut w, a.vip(), b"to a");
    assert_eq!(w.take(new_addr).len(), 1);
    assert!(w.take(a.addr).is_empty());
    let dat = a.data_bytes(b.vip(), b"from old");
    w.send(a.addr, &dat);
    assert!(b.take_data(&mut w).is_empty(), "DATA from the old address is dropped");
    w.send(new_addr, &dat);
    assert_eq!(b.take_data(&mut w).len(), 1);
    // broadcast too
    b.send_data(&mut w, wire::VBCAST, b"all");
    assert_eq!(w.take(new_addr).len(), 1);
    assert!(w.take(a.addr).is_empty());
    w.assert_clean();
}

#[test]
fn rebind_onto_another_members_address_is_refused() {
    let mut w = World::new(small_cfg());
    let (a, b, _c) = trio(&mut w);
    let mut ka = Vec::new();
    wire::encode_keepalive(&mut ka, &a.auth(), 1);
    w.send(b.addr, &ka); // a's credentials from b's address
    assert!(w.take(b.addr).is_empty());
    b.send_data(&mut w, a.vip(), b"still at the old place");
    assert_eq!(w.take(a.addr).len(), 1);
    assert_eq!(w.relay.stats().dropped[DropReason::RebindConflict as usize], 1);
    w.assert_clean();
}

#[test]
fn leave_after_rebind_must_come_from_the_new_address() {
    let mut w = World::new(small_cfg());
    let (a, _b, _c) = trio(&mut w);
    let new_addr = addr(777);
    let mut ka = Vec::new();
    wire::encode_keepalive(&mut ka, &a.auth(), 1);
    w.send(new_addr, &ka);
    a.leave(&mut w); // from the old address
    assert_eq!(w.relay.session_count(), 3);
    let mut lv = Vec::new();
    wire::encode_leave(&mut lv, &a.auth());
    w.send(new_addr, &lv);
    assert_eq!(w.relay.session_count(), 2);
}

// ---- rejoin ------------------------------------------------------------------------------------------------

#[test]
fn retransmitted_join_gets_the_same_ack_and_no_second_session() {
    let mut w = World::new(small_cfg());
    let c = addr(1);
    w.send(c, &hello_bytes(5));
    let Msg::Challenge { cookie, .. } = wire::parse(&w.take(c)[0]).unwrap() else {
        panic!()
    };
    let j = join_bytes(5, &cookie, 0, "R", "x");
    w.send(c, &j);
    let first = w.take(c);
    w.send(c, &j);
    let second = w.take(c);
    assert_eq!(first, second);
    assert_eq!(w.relay.session_count(), 1);
    w.assert_clean();
}

#[test]
fn rejoin_from_the_same_address_replaces_the_old_session() {
    let mut w = World::new(small_cfg());
    let mut a = Client::new(1, "A");
    let mut b = Client::new(2, "B");
    let s1 = joined(a.join(&mut w, "R", 0));
    b.join(&mut w, "R", 0);
    w.take(b.addr);
    let s2 = joined(a.join(&mut w, "R", s1.vip));
    assert_ne!(s1.auth.session, s2.auth.session);
    assert_eq!(s2.vip, s1.vip, "requested vip is free again once the old session ended");
    assert_eq!(w.relay.member_count(b"R"), 2);
    let to_b = w.take(b.addr);
    // MEMBER(left) for the old session, then MEMBER(joined) for the new one
    let events: Vec<u8> = to_b
        .iter()
        .filter_map(|d| match wire::parse(d) {
            Ok(Msg::Member { event, .. }) => Some(event),
            _ => None,
        })
        .collect();
    assert_eq!(events, vec![2, 1]);
    // the old credentials are dead
    let mut ka = Vec::new();
    wire::encode_keepalive(&mut ka, &s1.auth, 1);
    w.send(a.addr, &ka);
    assert!(matches!(
        wire::parse(&w.take(a.addr)[0]),
        Ok(Msg::Closed { reason: 2, .. })
    ));
    w.assert_clean();
}

#[test]
fn rejoin_in_another_room_from_the_same_address_ends_the_old_session() {
    let mut w = World::new(small_cfg());
    let mut a = Client::new(1, "A");
    a.join(&mut w, "ONE", 0);
    a.join(&mut w, "TWO", 0);
    assert_eq!(w.relay.session_count(), 1);
    assert_eq!(w.relay.room_sizes(), vec![("TWO".to_string(), 1)]);
}

// ---- vip assignment ---------------------------------------------------------------------------------------

#[test]
fn requested_vip_is_honored_when_valid_and_free() {
    let mut w = World::new(small_cfg());
    let mut a = Client::new(1, "A");
    assert_eq!(joined(a.join(&mut w, "R", vip(5, 17))).vip, vip(5, 17));
    let mut b = Client::new(2, "B");
    assert_eq!(
        joined(b.join(&mut w, "R", vip(5, 17))).vip,
        vip(0, 1),
        "taken: lowest free"
    );
    let bad = [
        0x0A0C_0001u32,
        vip(0, 0),
        vip(3, 0),
        vip(3, 255),
        vip(255, 255),
        0xC0A8_0001,
        0xFFFF_FFFF,
        0x0A0D_0000,
    ];
    for (i, want) in bad.into_iter().enumerate() {
        let mut c = Client::new(10 + i as u32, "C");
        let v = joined(c.join(&mut w, "R", want)).vip;
        assert!(
            v & 0xFFFF_0000 == wire::VNET && v & 0xFF != 0 && v & 0xFF != 0xFF,
            "{want:#x} -> {v:#x}"
        );
        assert_ne!(v, want);
    }
    w.assert_clean();
}

#[test]
fn lowest_free_vip_skips_dot_zero_and_dot_255() {
    let mut cfg = small_cfg();
    cfg.max_members = 254;
    cfg.max_sessions = 2000;
    let mut w = World::new(cfg);
    let mut seen = std::collections::HashSet::new();
    for n in 0..254u32 {
        let mut c = Client::new(n + 1, "x");
        let v = joined(c.join(&mut w, "BIG", 0)).vip;
        assert!(seen.insert(v));
        assert!(v & 0xFF != 0 && v & 0xFF != 255);
    }
    // the 254th member got 10.13.1.1 .. (first 254 usable: .0.1-.0.254)
    assert!(seen.contains(&vip(0, 254)));
    assert_eq!(seen.len(), 254);
}

#[test]
fn vips_are_stable_across_a_relay_restart() {
    // The relay keeps no persistent state: stability comes from the client asking for its previous vip again
    // (JOIN "requested vip"), which the new relay grants when it is free.
    let mut w = World::new(small_cfg());
    let mut clients: Vec<Client> = (1..=3).map(|n| Client::new(n, "p")).collect();
    // join in a scrambled order so that vips are not simply ascending by address
    let mut vips = vec![0; 3];
    for &i in &[2usize, 0, 1] {
        vips[i] = joined(clients[i].join(&mut w, "PEERTEST", 0)).vip;
    }
    assert_eq!(vips, vec![vip(0, 2), vip(0, 3), vip(0, 1)]);
    // restart: a brand new relay (new secret, no sessions)
    let mut w2 = World::new(small_cfg());
    // the clients reconnect in another order, each requesting its previous vip
    for &i in &[1usize, 2, 0] {
        let got = joined(clients[i].join(&mut w2, "PEERTEST", vips[i])).vip;
        assert_eq!(got, vips[i], "client {i} keeps its virtual IP");
    }
    w2.assert_clean();
}

#[test]
fn stale_cookie_from_before_a_restart_is_refused_so_the_client_restarts_its_handshake() {
    let mut w = World::new(small_cfg());
    let c = addr(1);
    w.send(c, &hello_bytes(5));
    let Msg::Challenge { cookie, .. } = wire::parse(&w.take(c)[0]).unwrap() else {
        panic!()
    };
    let mut w2 = World::new(small_cfg());
    w2.send(c, &join_bytes(5, &cookie, 0, "R", "x"));
    assert!(matches!(
        wire::parse(&w2.take(c)[0]),
        Ok(Msg::JoinNak { reason: 2, .. })
    ));
}

#[test]
fn old_session_after_restart_gets_closed_so_the_client_rejoins_at_once() {
    let mut w = World::new(small_cfg());
    let (a, _b, _c) = trio(&mut w);
    let auth = a.auth();
    let mut w2 = World::new(small_cfg());
    w2.send(a.addr, &{
        let mut v = Vec::new();
        wire::encode_keepalive(&mut v, &auth, 9);
        v
    });
    let d = w2.take(a.addr);
    assert_eq!(
        wire::parse(&d[0]),
        Ok(Msg::Closed {
            auth,
            reason: closed::UNKNOWN_SESSION
        })
    );
}

// ---- limits ---------------------------------------------------------------------------------------------------

#[test]
fn room_full_gives_nak_4_and_a_slot_reopens_after_a_leave() {
    let mut cfg = small_cfg();
    cfg.max_members = 3;
    let mut w = World::new(cfg);
    let mut cs: Vec<Client> = (1..=4).map(|n| Client::new(n, "p")).collect();
    for c in cs.iter_mut().take(3) {
        joined(c.join(&mut w, "R", 0));
    }
    assert_eq!(cs[3].join(&mut w, "R", 0), JoinResult::Refused(nak::ROOM_FULL));
    // another room is unaffected
    joined(cs[3].join(&mut w, "OTHER", 0));
    cs[0].leave(&mut w);
    let mut d = Client::new(9, "late");
    joined(d.join(&mut w, "R", 0));
    w.assert_clean();
}

#[test]
fn max_rooms_and_max_sessions_give_nak_5() {
    let mut cfg = small_cfg();
    cfg.max_rooms = 2;
    cfg.max_sessions = 4;
    let mut w = World::new(cfg);
    let mut cs: Vec<Client> = (1..=8).map(|n| Client::new(n, "p")).collect();
    joined(cs[0].join(&mut w, "A", 0));
    joined(cs[1].join(&mut w, "B", 0));
    assert_eq!(
        cs[2].join(&mut w, "C", 0),
        JoinResult::Refused(nak::RELAY_FULL),
        "third room"
    );
    joined(cs[2].join(&mut w, "A", 0)); // existing room is fine
    joined(cs[3].join(&mut w, "B", 0));
    assert_eq!(
        cs[4].join(&mut w, "A", 0),
        JoinResult::Refused(nak::RELAY_FULL),
        "fifth session"
    );
    assert_eq!(w.relay.session_count(), 4);
    assert_eq!(w.relay.room_count(), 2);
    w.assert_clean();
}

#[test]
fn rejoin_at_capacity_does_not_need_a_free_slot() {
    let mut cfg = small_cfg();
    cfg.max_sessions = 2;
    cfg.max_members = 2;
    let mut w = World::new(cfg);
    let mut a = Client::new(1, "A");
    let mut b = Client::new(2, "B");
    joined(a.join(&mut w, "R", 0));
    joined(b.join(&mut w, "R", 0));
    joined(a.join(&mut w, "R", 0)); // the old session of the same address ends first
    assert_eq!(w.relay.session_count(), 2);
}

#[test]
fn hello_and_join_are_rate_limited_per_source_ip() {
    let mut w = World::new(small_cfg());
    let ip = |port: u16| -> std::net::SocketAddr { format!("198.51.100.77:{port}").parse().unwrap() };
    let mut answered = 0;
    for i in 0..50u16 {
        w.send(ip(2000 + i), &hello_bytes(i as u64));
        answered += w.take(ip(2000 + i)).len();
    }
    assert_eq!(answered, 20, "20 HELLO per second per IP");
    assert_eq!(w.relay.stats().dropped[DropReason::RateHello as usize], 30);
    // another IP is not affected
    w.send(addr(5), &hello_bytes(1));
    assert_eq!(w.take(addr(5)).len(), 1);
    // the window moves on
    w.advance(Duration::from_secs(1));
    w.send(ip(1), &hello_bytes(1));
    assert_eq!(w.take(ip(1)).len(), 1);
    // JOIN has its own budget of 20 per second
    let mut replies = 0;
    for i in 0..50u16 {
        w.send(ip(3000 + i), &join_bytes(1, &[0; 16], 0, "R", "x"));
        replies += w.take(ip(3000 + i)).len();
    }
    assert_eq!(replies, 20);
    assert_eq!(w.relay.stats().dropped[DropReason::RateJoin as usize], 30);
    w.assert_clean();
}

#[test]
fn per_member_data_rate_limit() {
    let mut cfg = small_cfg();
    cfg.member_pps = 100;
    cfg.room_pps = 0;
    let mut w = World::new(cfg);
    let (a, b, _c) = trio(&mut w);
    for _ in 0..300 {
        a.send_data(&mut w, b.vip(), b"x");
    }
    assert_eq!(b.take_data(&mut w).len(), 100, "one second deep bucket");
    assert_eq!(w.relay.stats().dropped[DropReason::RateMember as usize], 200);
    // the other member has its own bucket
    b.send_data(&mut w, a.vip(), b"x");
    assert_eq!(a.take_data(&mut w).len(), 1);
    w.advance(Duration::from_secs(1));
    a.send_data(&mut w, b.vip(), b"x");
    assert_eq!(b.take_data(&mut w).len(), 1);
}

#[test]
fn per_member_byte_rate_limit() {
    let mut cfg = small_cfg();
    cfg.member_bytes_per_s = 10_000;
    cfg.member_pps = 0;
    cfg.room_pps = 0;
    let mut w = World::new(cfg);
    let (a, b, _c) = trio(&mut w);
    let payload = vec![0u8; 1000]; // 1032 bytes each
    for _ in 0..20 {
        a.send_data(&mut w, b.vip(), &payload);
    }
    assert_eq!(b.take_data(&mut w).len(), 9);
}

#[test]
fn per_room_data_rate_limit_is_shared_by_members_and_not_by_rooms() {
    let mut cfg = small_cfg();
    cfg.room_pps = 50;
    let mut w = World::new(cfg);
    let (a, b, c) = trio(&mut w);
    let mut other = Client::new(40, "o");
    let mut other2 = Client::new(41, "o2");
    other.join(&mut w, "ELSEWHERE", 0);
    other2.join(&mut w, "ELSEWHERE", 0);
    w.take(other.addr);
    w.take(other2.addr);
    for _ in 0..40 {
        a.send_data(&mut w, c.vip(), b"x");
        b.send_data(&mut w, c.vip(), b"x");
    }
    assert_eq!(c.take_data(&mut w).len(), 50);
    assert_eq!(w.relay.stats().dropped[DropReason::RateRoom as usize], 30);
    // the other room still has its own budget
    other.send_data(&mut w, other2.vip(), b"x");
    assert_eq!(other2.take_data(&mut w).len(), 1);
}

#[test]
fn fan_out_is_bounded_by_room_size() {
    let mut cfg = small_cfg();
    cfg.max_members = 5;
    let mut w = World::new(cfg);
    let mut cs: Vec<Client> = (1..=5).map(|n| Client::new(n, "p")).collect();
    for c in cs.iter_mut() {
        c.join(&mut w, "R", 0);
    }
    for c in &cs {
        w.take(c.addr);
    }
    let sent_before = w.relay.stats().tx;
    cs[0].send_data(&mut w, wire::VBCAST, &vec![1u8; 1440]);
    assert_eq!(w.relay.stats().tx - sent_before, 4, "at most room size - 1 copies");
}

// ---- shutdown -----------------------------------------------------------------------------------------------

#[test]
fn shutdown_sends_closed_4_to_every_member_and_clears_state() {
    let mut w = World::new(small_cfg());
    let (a, b, c) = trio(&mut w);
    w.relay.shutdown(&mut w.out);
    // route outputs
    let outs: Vec<_> = w.out.iter().map(|(to, d)| (to, d.to_vec())).collect();
    assert_eq!(outs.len(), 3);
    for (x, auth) in [(a.addr, a.auth()), (b.addr, b.auth()), (c.addr, c.auth())] {
        let d = outs.iter().find(|(to, _)| *to == x).unwrap();
        assert_eq!(
            wire::parse(&d.1),
            Ok(Msg::Closed {
                auth,
                reason: closed::SHUTDOWN
            })
        );
    }
    assert_eq!((w.relay.session_count(), w.relay.room_count()), (0, 0));
}

#[test]
fn config_defaults_follow_the_specification() {
    let c = RelayConfig::default();
    assert_eq!((c.max_members, c.max_rooms, c.max_sessions), (12, 256, 1024));
    assert_eq!(
        (c.hello_per_s, c.join_per_s, c.member_pps, c.member_bytes_per_s),
        (20, 20, 2000, 4 << 20)
    );
    assert_eq!((c.keepalive_s, c.idle_s), (5, 30));
    assert!(c.validate().is_ok());
}

/// A relay bound to a service (`--room-secret-file`): only room codes sealed with the shared secret open a room.
#[test]
fn a_bound_relay_only_opens_rooms_its_service_issued() {
    let secret = b"0123456789abcdef0123456789abcdef".to_vec();
    let mut w = World::new(RelayConfig {
        room_secret: Some(kalamar_relay::roomauth::RoomSecret::new(secret.clone())),
        ..small_cfg()
    });
    let sealed = kalamar_relay::roomauth::seal(&secret, "K7Q2MXA9P84DZB3W8R6TNY5HCF", w.unix).unwrap();
    let mut a = Client::new(1, "Ana");
    let mut b = Client::new(2, "Bo");
    let mut c = Client::new(3, "Cleo");
    // Any other code is refused with the "room" reason and creates nothing.
    for room in ["FRIENDS-ROOM", "K7Q2MXA9P84DZB3W8R6TNY5HCF"] {
        assert!(matches!(c.join(&mut w, room, 0), JoinResult::Refused(r) if r == wire::nak::ROOM));
    }
    let other = kalamar_relay::roomauth::seal(b"someone else's secret 0123456789", "K7Q2MXA9P84DZ", w.unix).unwrap();
    assert!(matches!(c.join(&mut w, &other, 0), JoinResult::Refused(r) if r == wire::nak::ROOM));
    assert_eq!(w.relay.room_count(), 0);
    // The service's code works as any room does.
    joined(a.join(&mut w, &sealed, 0));
    joined(b.join(&mut w, &sealed, 0));
    assert_eq!(w.relay.member_count(sealed.as_bytes()), 2);
    // Two days later the same code opens nothing.
    w.unix += 2 * 86_400;
    assert!(matches!(c.join(&mut w, &sealed, 0), JoinResult::Refused(r) if r == wire::nak::ROOM));
    w.assert_clean();
}

/// relay.md section 3: room codes never reach the logs. The relay logs a tag of the room (to follow it through the
/// lines), the member's name and address (what an operator needs against abuse, said in PRIVACY-TEMPLATE.md).
#[test]
fn logs_carry_a_room_tag_never_the_code() {
    use std::io::Write;
    use std::sync::{Arc, Mutex};
    #[derive(Clone)]
    struct Buf(Arc<Mutex<Vec<u8>>>);
    impl Write for Buf {
        fn write(&mut self, b: &[u8]) -> std::io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(b);
            Ok(b.len())
        }
        fn flush(&mut self) -> std::io::Result<()> {
            Ok(())
        }
    }
    let buf = Buf(Arc::new(Mutex::new(Vec::new())));
    let writer = buf.clone();
    let sub = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(move || writer.clone())
        .finish();
    let room = "K7Q2MXA9P84DZB3W8R6TNY5HCF";
    // A global subscriber (only this test sets one): a subscriber of this thread alone can miss events whose callsite
    // another test thread registered first. The other tests' lines land in the buffer too; they log no code either.
    tracing::subscriber::set_global_default(sub).expect("only this test sets a subscriber");
    {
        let mut w = World::new(small_cfg());
        let mut a = Client::new(1, "Ana");
        let mut b = Client::new(2, "Bo");
        joined(a.join(&mut w, room, 0));
        joined(b.join(&mut w, room, 0));
        a.leave(&mut w);
        w.advance(Duration::from_secs(40));
    }
    let text = String::from_utf8(buf.0.lock().unwrap().clone()).unwrap();
    assert!(text.contains("joined") && text.contains("left"), "{text}");
    assert!(
        !text.contains(room) && !text.contains(&room[..13]),
        "a room code was logged:\n{text}"
    );
    assert!(
        text.contains(&kalamar_relay::roomauth::room_tag(room.as_bytes())),
        "{text}"
    );
}
