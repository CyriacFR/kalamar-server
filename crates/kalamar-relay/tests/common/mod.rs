//! Simulated network for the state-machine tests: a relay, a manual clock, per-address inboxes, simulated clients,
//! and a checker that watches every datagram the relay emits.
#![allow(dead_code)]

use std::collections::{HashMap, HashSet, VecDeque};
use std::net::SocketAddr;
use std::time::{Duration, Instant};

use kalamar_relay::cookie::CookieKeys;
use kalamar_relay::relay::{Outbox, Relay, RelayConfig};
use kalamar_relay::wire::{self, Auth, DataHeader, Msg};

pub fn addr(n: u32) -> SocketAddr {
    // 192.0.2.0/24 and 198.51.100.0/24 are documentation ranges: nothing real is ever contacted.
    format!("198.51.{}.{}:{}", (n >> 8) & 0xFF, n & 0xFF, 40000 + (n % 20000))
        .parse()
        .unwrap()
}

pub struct World {
    pub relay: Relay,
    pub now: Instant,
    pub unix: u64,
    pub out: Outbox,
    pub inbox: HashMap<SocketAddr, VecDeque<Vec<u8>>>,
    /// Addresses that received a JOIN_ACK or an authenticated KEEPALIVE_ACK: proven to receive datagrams.
    pub proven: HashSet<SocketAddr>,
    /// Number of datagrams the relay sent to an address before it was proven, answering its own request.
    pub pre_proof_replies: usize,
    /// Every violation of the "never send to an unproven address / never amplify" rules, as text.
    pub violations: Vec<String>,
}

impl World {
    pub fn new(cfg: RelayConfig) -> Self {
        let now = Instant::now();
        let relay = Relay::new(cfg, now);
        Self::with_relay(relay, now)
    }

    pub fn with_relay(relay: Relay, now: Instant) -> Self {
        World {
            relay,
            now,
            unix: 1_800_000_000,
            out: Outbox::new(),
            inbox: HashMap::new(),
            proven: HashSet::new(),
            pre_proof_replies: 0,
            violations: Vec::new(),
        }
    }

    pub fn with_secret(cfg: RelayConfig, secret: [u8; 32]) -> Self {
        let now = Instant::now();
        let relay = Relay::with_cookies(
            cfg.clone(),
            CookieKeys::with_secret(secret, now, cfg.cookie_rotation),
            now,
        );
        Self::with_relay(relay, now)
    }

    /// Delivers `bytes` from `src` to the relay and routes what it emits.
    pub fn send(&mut self, src: SocketAddr, bytes: &[u8]) {
        self.relay.handle(self.now, self.unix, src, bytes, &mut self.out);
        self.collect(Some((src, bytes.len())));
    }

    fn collect(&mut self, trigger: Option<(SocketAddr, usize)>) {
        let mut to_src_bytes = 0usize;
        let mut newly_proven = Vec::new();
        for (to, data) in self.out.iter() {
            assert!(data.len() <= wire::MAX_DATAGRAM, "oversized datagram sent");
            let is_trigger_src = trigger.is_some_and(|(s, _)| s == to);
            if !self.proven.contains(&to) && !newly_proven.contains(&to) {
                match trigger {
                    Some((s, in_len)) if is_trigger_src => {
                        to_src_bytes += data.len();
                        if data.len() > in_len || to_src_bytes > in_len {
                            self.violations
                                .push(format!("amplification toward {s}: {to_src_bytes} > {in_len}"));
                        }
                        self.pre_proof_replies += 1;
                        // JOIN_ACK and KEEPALIVE_ACK prove the address: what follows (MEMBER messages) is traffic
                        // the member asked for by joining (section 7.2)
                        if data[5] == wire::ty::JOIN_ACK || data[5] == wire::ty::KEEPALIVE_ACK {
                            newly_proven.push(to);
                        }
                    }
                    _ => self.violations.push(format!(
                        "datagram type {:#x} sent to unproven address {to} (trigger {trigger:?})",
                        data[5]
                    )),
                }
            }
            self.inbox.entry(to).or_default().push_back(data.to_vec());
        }
        self.proven.extend(newly_proven);
        self.out.clear();
    }

    pub fn advance(&mut self, d: Duration) {
        // step in one-second ticks like the server does
        let mut left = d;
        while !left.is_zero() {
            let step = left.min(Duration::from_secs(1));
            self.now += step;
            self.unix += step.as_secs();
            left -= step;
            self.relay.tick(self.now, &mut self.out);
            self.collect(None);
        }
    }

    pub fn take(&mut self, a: SocketAddr) -> Vec<Vec<u8>> {
        self.inbox
            .get_mut(&a)
            .map(|q| q.drain(..).collect())
            .unwrap_or_default()
    }

    pub fn assert_clean(&self) {
        assert!(self.violations.is_empty(), "violations: {:#?}", self.violations);
    }
}

pub fn parse_all(v: &[Vec<u8>]) -> Vec<Msg<'_>> {
    v.iter()
        .map(|b| wire::parse(b).expect("relay sent an unparsable datagram"))
        .collect()
}

pub fn join_bytes(nonce: u64, cookie: &[u8; 16], vip: u32, room: &str, name: &str) -> Vec<u8> {
    let mut v = Vec::new();
    wire::encode_join(&mut v, nonce, cookie, vip, room.as_bytes(), name.as_bytes());
    v
}

pub fn hello_bytes(nonce: u64) -> Vec<u8> {
    let mut v = Vec::new();
    wire::encode_hello(&mut v, nonce);
    v
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Session {
    pub auth: Auth,
    pub vip: u32,
    pub nonce: u64,
}

/// A simulated game client (the part of the engine's client that talks to the relay).
pub struct Client {
    pub addr: SocketAddr,
    pub name: String,
    pub session: Option<Session>,
    pub nonce: u64,
    seq: u32,
}

#[derive(Debug, PartialEq, Eq)]
pub enum JoinResult {
    Joined(Session),
    Refused(u8),
    NoAnswer,
}

impl Client {
    pub fn new(n: u32, name: &str) -> Self {
        Client {
            addr: addr(n),
            name: name.into(),
            session: None,
            nonce: 0x1000_0000_0000_0000 + n as u64,
            seq: 0,
        }
    }

    /// Full handshake: HELLO, CHALLENGE, JOIN, JOIN_ACK/NAK. Uses a fresh nonce each time.
    pub fn join(&mut self, w: &mut World, room: &str, want_vip: u32) -> JoinResult {
        self.nonce = self.nonce.wrapping_add(0x100);
        w.send(self.addr, &hello_bytes(self.nonce));
        let got = w.take(self.addr);
        let Some(cookie) = got.iter().find_map(|d| match wire::parse(d) {
            Ok(Msg::Challenge { nonce, cookie }) if nonce == self.nonce => Some(cookie),
            _ => None,
        }) else {
            return JoinResult::NoAnswer;
        };
        self.join_with_cookie(w, room, want_vip, &cookie)
    }

    pub fn join_with_cookie(&mut self, w: &mut World, room: &str, want_vip: u32, cookie: &[u8; 16]) -> JoinResult {
        w.send(self.addr, &join_bytes(self.nonce, cookie, want_vip, room, &self.name));
        for d in w.take(self.addr) {
            match wire::parse(&d) {
                Ok(Msg::JoinAck(a)) if a.nonce == self.nonce => {
                    let s = Session {
                        auth: Auth {
                            session: a.session,
                            token: a.token,
                        },
                        vip: a.vip,
                        nonce: self.nonce,
                    };
                    self.session = Some(s);
                    return JoinResult::Joined(s);
                }
                Ok(Msg::JoinNak { nonce, reason, .. }) if nonce == self.nonce => return JoinResult::Refused(reason),
                _ => {}
            }
        }
        JoinResult::NoAnswer
    }

    pub fn auth(&self) -> Auth {
        self.session.expect("not joined").auth
    }
    pub fn vip(&self) -> u32 {
        self.session.expect("not joined").vip
    }

    pub fn keepalive(&mut self, w: &mut World) -> Option<u8> {
        self.seq += 1;
        let mut v = Vec::new();
        wire::encode_keepalive(&mut v, &self.auth(), self.seq);
        w.send(self.addr, &v);
        w.take(self.addr).iter().find_map(|d| match wire::parse(d) {
            Ok(Msg::KeepaliveAck { seq, members, .. }) if seq == self.seq => Some(members),
            _ => None,
        })
    }

    pub fn data_bytes(&self, dst_vip: u32, payload: &[u8]) -> Vec<u8> {
        let h = DataHeader {
            src_vip: self.vip(),
            src_port: 49152,
            dst_vip,
            dst_port: 35000,
        };
        let mut v = Vec::new();
        wire::encode_data(&mut v, &self.auth(), &h, payload);
        v
    }

    pub fn send_data(&self, w: &mut World, dst_vip: u32, payload: &[u8]) {
        w.send(self.addr, &self.data_bytes(dst_vip, payload));
    }

    pub fn leave(&self, w: &mut World) {
        let mut v = Vec::new();
        wire::encode_leave(&mut v, &self.auth());
        w.send(self.addr, &v);
    }

    /// The DATA datagrams waiting for this client, as (source vip, destination vip, payload); other messages are
    /// returned by `take_all`.
    pub fn take_data(&self, w: &mut World) -> Vec<(u32, u32, Vec<u8>)> {
        let me = self.auth();
        w.take(self.addr)
            .iter()
            .filter_map(|d| match wire::parse(d) {
                Ok(Msg::Data { auth, hdr, payload }) => {
                    assert_eq!(auth, me, "DATA must carry the recipient's session header");
                    Some((hdr.src_vip, hdr.dst_vip, payload.to_vec()))
                }
                _ => None,
            })
            .collect()
    }
}

pub fn vip(a: u8, b: u8) -> u32 {
    u32::from_be_bytes([10, 13, a, b])
}

pub fn small_cfg() -> RelayConfig {
    RelayConfig::default()
}

/// Small xorshift generator: deterministic fuzzing without depending on a particular `rand` version.
pub struct Rng(pub u64);
impl Rng {
    pub fn next(&mut self) -> u64 {
        let mut x = self.0;
        x ^= x << 13;
        x ^= x >> 7;
        x ^= x << 17;
        self.0 = x;
        x
    }
    pub fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }
    pub fn bytes(&mut self, n: usize) -> Vec<u8> {
        (0..n).map(|_| self.next() as u8).collect()
    }
}
