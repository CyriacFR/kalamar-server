//! The load a relay announces to the service bound to it (protocol 1.1.13, `relay.md` section 4.3).
//!
//! The service sends a `STATUS_QUERY`, the relay answers `STATUS`, both on the relay's own UDP port and both 128
//! bytes long. Only a relay bound to a service answers (an open relay publishes nothing more than before), and only a
//! query whose tag is that service's: the key is derived from the relay's room secret
//! (`HMAC-SHA-256(secret, "kalamar-relay-status-v1")`, never the secret itself), so that nobody else can ask. A query
//! is accepted within 60 seconds of its own clock and once (its nonce is remembered), at most
//! [`ANSWERS_PER_S`] a second, and an answer is never larger than its query: a recorded query replayed from a forged
//! address gets nothing, and the relay never amplifies. The answer carries the query's nonce under its own tag: the
//! service knows it comes from its relay and answers its question. What it reveals (counts and rates, never an
//! address, a name or a room) travels in clear: an observer on the path sees the traffic itself anyway.
//!
//! ```text
//! STATUS_QUERY (0x20)                       STATUS (0x21)
//!  0  header (S3LR, 1, 0x20, 0, 0)            0  header (S3LR, 1, 0x21, 0, 0)
//!  8  nonce                       u64         8  the query's nonce        u64
//! 16  the service's clock, Unix ms u64       16  the relay's clock, Unix ms u64
//! 24  zero                                   24  format (1)               u8
//!                                            25  workers                  u8   (255: 255 or more)
//!                                            26  window, seconds          u8   (the rates' period)
//!                                            27  mean worker load, %      u8
//!                                            28  sessions                 u32
//!                                            32  rooms                    u32
//!                                            36  most sessions            u32  (0: no cap)
//!                                            40  most rooms               u32  (0: no cap)
//!                                            44  datagrams received /s    u32
//!                                            48  datagrams sent /s        u32
//!                                            52  bytes received /s        u32
//!                                            56  bytes sent /s            u32
//!                                            60  datagrams dropped /s     u32
//!                                            64  JOINs refused (window)   u32
//!                                            68  of them for want of room u32
//!                                            72  uptime, seconds          u32
//!                                            76  members per room         u8, then 3 zero bytes
//!                                            80  load of workers 1-32, %  u8 each (0xFF: no such worker)
//! 112 tag                         16 bytes  112 tag                       16 bytes
//! ```
//!
//! The tag is the first 16 bytes of `HMAC-SHA-256(key, bytes 0..112)`. Counts larger than a field saturate.

use std::time::{SystemTime, UNIX_EPOCH};

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::wire::{self, STATUS_LEN};

const DOMAIN: &[u8] = b"kalamar-relay-status-v1";
const TAG_AT: usize = STATUS_LEN - 16;
/// A query older or newer than this (milliseconds, the two clocks compared) is not answered.
pub const MAX_SKEW_MS: u64 = 60_000;
/// Answers per second, all services together (a service asks every few seconds).
pub const ANSWERS_PER_S: u32 = 20;
/// Worker loads an answer carries one by one.
pub const WORKER_SLOTS: usize = 32;
/// Nonces remembered against replays (a query is accepted for at most two minutes).
const REMEMBERED: usize = 512;

type HmacSha256 = Hmac<Sha256>;

pub fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

/// A relay's load, as the relay measured it ([`crate::relay::Relay::load`]) or as an answer told it.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Load {
    pub workers: usize,
    /// The period of the rates below, seconds (0: not measured yet).
    pub window_s: u8,
    pub sessions: usize,
    pub rooms: usize,
    /// 0: no cap.
    pub max_sessions: usize,
    pub max_rooms: usize,
    pub max_members: usize,
    pub rx_pps: u64,
    pub tx_pps: u64,
    /// Bytes per second (UDP payloads).
    pub rx_bps: u64,
    pub tx_bps: u64,
    pub dropped_ps: u64,
    /// JOINs refused during the window, and of them those refused for want of room.
    pub refused: u64,
    pub refused_full: u64,
    pub uptime_s: u64,
    /// Each worker's share of time spent working, in percent (at most [`WORKER_SLOTS`] of them in an answer).
    pub worker_busy: Vec<u8>,
}

impl Load {
    /// The workers' mean load, in percent.
    pub fn mean_busy(&self) -> u8 {
        if self.worker_busy.is_empty() {
            return 0;
        }
        (self.worker_busy.iter().map(|&b| b as u32).sum::<u32>() / self.worker_busy.len() as u32) as u8
    }
}

/// What a query asks: its nonce.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Query {
    pub nonce: u64,
}

/// The key of the status messages of one relay, derived from its room secret.
#[derive(Clone)]
pub struct StatusKey([u8; 32]);

impl std::fmt::Debug for StatusKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("StatusKey(..)")
    }
}

fn sat32(n: u64) -> [u8; 4] {
    (n.min(u32::MAX as u64) as u32).to_be_bytes()
}

fn be32(b: &[u8], o: usize) -> u64 {
    u32::from_be_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]]) as u64
}

fn be64(b: &[u8], o: usize) -> u64 {
    let mut a = [0u8; 8];
    a.copy_from_slice(&b[o..o + 8]);
    u64::from_be_bytes(a)
}

impl StatusKey {
    pub fn new(secret: &crate::roomauth::RoomSecret) -> StatusKey {
        let mut m = <HmacSha256 as KeyInit>::new_from_slice(secret.bytes()).expect("HMAC takes any key length");
        m.update(DOMAIN);
        StatusKey(m.finalize().into_bytes().into())
    }

    fn tag(&self, body: &[u8]) -> [u8; 16] {
        let mut m = <HmacSha256 as KeyInit>::new_from_slice(&self.0).expect("HMAC takes any key length");
        m.update(body);
        let full = m.finalize().into_bytes();
        let mut t = [0u8; 16];
        t.copy_from_slice(&full[..16]);
        t
    }

    fn sealed(&self, b: &[u8], ty: u8) -> bool {
        b.len() >= STATUS_LEN
            && b[..4] == wire::MAGIC
            && b[4] == wire::VERSION
            && b[5] == ty
            && bool::from(self.tag(&b[..TAG_AT]).ct_eq(&b[TAG_AT..STATUS_LEN]))
    }

    /// A query from `nonce` at the service's clock `unix_ms`.
    pub fn encode_query(&self, out: &mut Vec<u8>, nonce: u64, unix_ms: u64) {
        let start = out.len();
        wire::put_header(out, wire::ty::STATUS_QUERY);
        out.extend_from_slice(&nonce.to_be_bytes());
        out.extend_from_slice(&unix_ms.to_be_bytes());
        out.resize(start + TAG_AT, 0);
        let t = self.tag(&out[start..]);
        out.extend_from_slice(&t);
    }

    /// The query in `b` when its tag is this key's and its clock within [`MAX_SKEW_MS`] of `unix_ms`.
    pub fn verify_query(&self, b: &[u8], unix_ms: u64) -> Option<Query> {
        if !self.sealed(b, wire::ty::STATUS_QUERY) {
            return None;
        }
        let at = be64(b, 16);
        (at.abs_diff(unix_ms) <= MAX_SKEW_MS).then(|| Query { nonce: be64(b, 8) })
    }

    /// The answer to the query `nonce`.
    pub fn encode_status(&self, out: &mut Vec<u8>, nonce: u64, unix_ms: u64, l: &Load) {
        let start = out.len();
        wire::put_header(out, wire::ty::STATUS);
        out.extend_from_slice(&nonce.to_be_bytes());
        out.extend_from_slice(&unix_ms.to_be_bytes());
        out.extend_from_slice(&[1, l.workers.min(255) as u8, l.window_s, l.mean_busy()]);
        for v in [
            l.sessions as u64,
            l.rooms as u64,
            l.max_sessions as u64,
            l.max_rooms as u64,
            l.rx_pps,
            l.tx_pps,
            l.rx_bps,
            l.tx_bps,
            l.dropped_ps,
            l.refused,
            l.refused_full,
            l.uptime_s,
        ] {
            out.extend_from_slice(&sat32(v));
        }
        out.extend_from_slice(&[l.max_members.min(255) as u8, 0, 0, 0]);
        for i in 0..WORKER_SLOTS {
            out.push(l.worker_busy.get(i).copied().map_or(0xFF, |b| b.min(100)));
        }
        debug_assert_eq!(out.len() - start, TAG_AT);
        let t = self.tag(&out[start..]);
        out.extend_from_slice(&t);
    }

    /// The load in the answer `b` to the query `nonce`, when its tag is this key's.
    pub fn parse_status(&self, b: &[u8], nonce: u64) -> Option<Load> {
        if !self.sealed(b, wire::ty::STATUS) || be64(b, 8) != nonce || b[24] != 1 {
            return None;
        }
        let worker_busy: Vec<u8> = b[80..80 + WORKER_SLOTS]
            .iter()
            .copied()
            .take_while(|&x| x != 0xFF)
            .map(|x| x.min(100))
            .collect();
        Some(Load {
            workers: b[25] as usize,
            window_s: b[26],
            sessions: be32(b, 28) as usize,
            rooms: be32(b, 32) as usize,
            max_sessions: be32(b, 36) as usize,
            max_rooms: be32(b, 40) as usize,
            rx_pps: be32(b, 44),
            tx_pps: be32(b, 48),
            rx_bps: be32(b, 52),
            tx_bps: be32(b, 56),
            dropped_ps: be32(b, 60),
            refused: be32(b, 64),
            refused_full: be32(b, 68),
            uptime_s: be32(b, 72),
            max_members: b[76] as usize,
            // more workers than slots: the mean of byte 27 stands for all of them
            worker_busy: if (b[25] as usize) > WORKER_SLOTS {
                vec![b[27].min(100); b[25] as usize]
            } else {
                worker_busy
            },
        })
    }
}

/// The nonces of the queries answered lately: a replayed query is not answered again.
#[derive(Default)]
pub struct Replay {
    seen: std::collections::VecDeque<u64>,
}

impl Replay {
    /// Whether `nonce` is new (it is then remembered).
    pub fn fresh(&mut self, nonce: u64) -> bool {
        if self.seen.contains(&nonce) {
            return false;
        }
        if self.seen.len() >= REMEMBERED {
            self.seen.pop_front();
        }
        self.seen.push_back(nonce);
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::roomauth::RoomSecret;

    fn key() -> StatusKey {
        StatusKey::new(&RoomSecret::new(b"0123456789abcdef0123456789abcdef".to_vec()))
    }

    #[test]
    fn a_query_and_its_answer_round_trip() {
        let k = key();
        let mut q = Vec::new();
        k.encode_query(&mut q, 42, 1_000_000);
        assert_eq!(q.len(), STATUS_LEN);
        assert_eq!(k.verify_query(&q, 1_000_000 + MAX_SKEW_MS), Some(Query { nonce: 42 }));
        assert_eq!(k.verify_query(&q, 1_000_000 + MAX_SKEW_MS + 1), None, "too old");
        assert_eq!(k.verify_query(&q, 1_000_000 - MAX_SKEW_MS - 1), None, "from the future");
        let l = Load {
            workers: 3,
            window_s: 5,
            sessions: 120,
            rooms: 15,
            max_sessions: 4096,
            max_rooms: 0,
            max_members: 12,
            rx_pps: 12_600,
            tx_pps: 12_590,
            rx_bps: 2_400_000,
            tx_bps: 2_390_000,
            dropped_ps: 3,
            refused: 2,
            refused_full: 1,
            uptime_s: 86_400,
            worker_busy: vec![10, 20, 30],
        };
        let mut a = Vec::new();
        k.encode_status(&mut a, 42, 1_000_500, &l);
        assert_eq!(a.len(), q.len(), "never larger than the query");
        assert_eq!(k.parse_status(&a, 42), Some(l.clone()));
        assert_eq!(k.parse_status(&a, 43), None, "another query's answer");
        assert_eq!(l.mean_busy(), 20);
    }

    #[test]
    fn only_the_secrets_holder_asks_and_answers() {
        let k = key();
        let other = StatusKey::new(&RoomSecret::new(b"another secret of 32 bytes, too.".to_vec()));
        let mut q = Vec::new();
        other.encode_query(&mut q, 1, 5_000);
        assert_eq!(k.verify_query(&q, 5_000), None);
        let mut q = Vec::new();
        k.encode_query(&mut q, 1, 5_000);
        for i in 0..STATUS_LEN {
            let mut x = q.clone();
            x[i] ^= 0x01;
            assert_eq!(k.verify_query(&x, 5_000), None, "byte {i} changed");
        }
        assert_eq!(k.verify_query(&q[..STATUS_LEN - 1], 5_000), None);
        // the room secret itself is not the key
        let raw = StatusKey(*b"0123456789abcdef0123456789abcdef");
        assert_eq!(raw.verify_query(&q, 5_000), None);
        // an answer is not a query
        let mut a = Vec::new();
        k.encode_status(&mut a, 1, 5_000, &Load::default());
        assert_eq!(k.verify_query(&a, 5_000), None);
        assert_eq!(other.parse_status(&a, 1), None);
    }

    #[test]
    fn many_workers_fit() {
        let k = key();
        let l = Load {
            workers: 64,
            worker_busy: (0..64).map(|i| (i % 100) as u8).collect(),
            ..Load::default()
        };
        let mut a = Vec::new();
        k.encode_status(&mut a, 9, 0, &l);
        let back = k.parse_status(&a, 9).unwrap();
        assert_eq!((back.workers, back.worker_busy.len()), (64, 64));
        assert!(back.worker_busy.iter().all(|&b| b == l.mean_busy()));
    }

    #[test]
    fn replayed_nonces_are_refused() {
        let mut r = Replay::default();
        assert!(r.fresh(1));
        assert!(!r.fresh(1));
        for n in 2..(REMEMBERED as u64 + 2) {
            assert!(r.fresh(n));
        }
        assert!(
            r.fresh(1),
            "forgotten after many others (by then far older than the accepted skew)"
        );
    }
}
