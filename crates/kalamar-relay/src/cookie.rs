//! Stateless handshake cookie (section 7.1).
//!
//! `cookie = HMAC-SHA256(secret, address bytes || port || nonce || epoch)[..16]`, `epoch = unix seconds / 30`.
//! A cookie is valid in the epoch it was issued in and the next one (30 to 60 s).
//!
//! The secret is rotated every `rotate_every`; the previous secret is kept for one more period so that cookies
//! issued just before a rotation still verify (the rotation period is at least 60 s, longer than a cookie lives).

use std::net::{IpAddr, SocketAddr};
use std::time::{Duration, Instant};

use hmac::{Hmac, KeyInit, Mac};
use sha2::Sha256;
use subtle::ConstantTimeEq;

use crate::wire::COOKIE_LEN;

pub const EPOCH_SECONDS: u64 = 30;
/// Shortest allowed rotation period: two epochs.
pub const MIN_ROTATION: Duration = Duration::from_secs(60);

type HmacSha256 = Hmac<Sha256>;

pub struct CookieKeys {
    current: [u8; 32],
    previous: Option<[u8; 32]>,
    rotated_at: Instant,
    rotate_every: Duration,
}

impl CookieKeys {
    /// A fresh random secret.
    pub fn new(now: Instant, rotate_every: Duration) -> Self {
        let mut s = [0u8; 32];
        rand::fill(&mut s[..]);
        Self::with_secret(s, now, rotate_every)
    }

    pub fn with_secret(secret: [u8; 32], now: Instant, rotate_every: Duration) -> Self {
        CookieKeys {
            current: secret,
            previous: None,
            rotated_at: now,
            rotate_every: rotate_every.max(MIN_ROTATION),
        }
    }

    /// Rotates the secret when its period is over. Returns true when it did.
    pub fn tick(&mut self, now: Instant) -> bool {
        if now.saturating_duration_since(self.rotated_at) < self.rotate_every {
            return false;
        }
        let mut s = [0u8; 32];
        rand::fill(&mut s[..]);
        self.previous = Some(std::mem::replace(&mut self.current, s));
        self.rotated_at = now;
        true
    }

    pub fn issue(&self, addr: SocketAddr, nonce: u64, unix: u64) -> [u8; COOKIE_LEN] {
        compute(&self.current, addr, nonce, unix / EPOCH_SECONDS)
    }

    pub fn verify(&self, addr: SocketAddr, nonce: u64, cookie: &[u8; COOKIE_LEN], unix: u64) -> bool {
        let e = unix / EPOCH_SECONDS;
        let mut ok = false;
        for secret in std::iter::once(&self.current).chain(self.previous.as_ref()) {
            for epoch in [e, e.wrapping_sub(1)] {
                // no early exit between candidates: the work done does not depend on which one matched
                ok |= bool::from(compute(secret, addr, nonce, epoch).ct_eq(cookie));
            }
        }
        ok
    }
}

/// The address bytes of a source: 4 for IPv4 (also an IPv4-mapped IPv6 address, as seen on a dual-stack
/// socket), 16 for IPv6.
fn ip_bytes(ip: IpAddr) -> ([u8; 16], usize) {
    let mut b = [0u8; 16];
    match ip.to_canonical() {
        IpAddr::V4(v4) => {
            b[..4].copy_from_slice(&v4.octets());
            (b, 4)
        }
        IpAddr::V6(v6) => {
            b.copy_from_slice(&v6.octets());
            (b, 16)
        }
    }
}

pub fn compute(secret: &[u8; 32], addr: SocketAddr, nonce: u64, epoch: u64) -> [u8; COOKIE_LEN] {
    let (ip, n) = ip_bytes(addr.ip());
    let mut mac = HmacSha256::new_from_slice(secret).expect("HMAC accepts a key of any length");
    mac.update(&ip[..n]);
    mac.update(&addr.port().to_be_bytes());
    mac.update(&nonce.to_be_bytes());
    mac.update(&epoch.to_be_bytes());
    let full = mac.finalize().into_bytes();
    let mut c = [0u8; COOKIE_LEN];
    c.copy_from_slice(&full[..COOKIE_LEN]);
    c
}

#[cfg(test)]
mod tests {
    use super::*;

    const SECRET: [u8; 32] = {
        let mut s = [0u8; 32];
        let mut i = 0;
        while i < 32 {
            s[i] = i as u8;
            i += 1;
        }
        s
    };
    const NONCE: u64 = 0x0123_4567_89AB_CDEF;

    fn hex(s: &str) -> [u8; 16] {
        let mut o = [0u8; 16];
        for i in 0..16 {
            o[i] = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).unwrap();
        }
        o
    }

    /// Reference values computed with Python's hmac/hashlib from the formula of section 7.1.
    #[test]
    fn matches_independent_computation() {
        let a4: SocketAddr = "192.0.2.7:5000".parse().unwrap();
        assert_eq!(
            compute(&SECRET, a4, NONCE, 123456789),
            hex("66c2298cbae5de89bec191e24cfc2aaf")
        );
        let a6: SocketAddr = "[2001:db8::1]:5000".parse().unwrap();
        assert_eq!(
            compute(&SECRET, a6, NONCE, 123456789),
            hex("d62fdf264b6b563bccda91a2f5e04bb6")
        );
    }

    #[test]
    fn v4_mapped_v6_is_the_same_as_v4() {
        let a4: SocketAddr = "192.0.2.7:5000".parse().unwrap();
        let m: SocketAddr = "[::ffff:192.0.2.7]:5000".parse().unwrap();
        assert_eq!(compute(&SECRET, a4, NONCE, 5), compute(&SECRET, m, NONCE, 5));
    }

    #[test]
    fn lifetime_is_30_to_60_seconds() {
        let t = Instant::now();
        let k = CookieKeys::with_secret(SECRET, t, Duration::from_secs(3600));
        let a: SocketAddr = "198.51.100.9:40000".parse().unwrap();
        let unix = 1_000_000_000 / 30 * 30; // start of an epoch
        let c = k.issue(a, NONCE, unix);
        assert!(k.verify(a, NONCE, &c, unix));
        assert!(k.verify(a, NONCE, &c, unix + 29));
        assert!(k.verify(a, NONCE, &c, unix + 59), "valid through the next epoch");
        assert!(!k.verify(a, NONCE, &c, unix + 60), "expired after two epochs");
        // issued at the end of an epoch: still at least 30 s
        let c2 = k.issue(a, NONCE, unix + 29);
        assert!(k.verify(a, NONCE, &c2, unix + 29 + 30));
        assert!(!k.verify(a, NONCE, &c2, unix + 29 + 60));
        // not valid before it was issued
        assert!(!k.verify(a, NONCE, &c, unix - 1));
    }

    #[test]
    fn bound_to_address_port_and_nonce() {
        let t = Instant::now();
        let k = CookieKeys::with_secret(SECRET, t, Duration::from_secs(3600));
        let a: SocketAddr = "198.51.100.9:40000".parse().unwrap();
        let c = k.issue(a, NONCE, 1000);
        assert!(k.verify(a, NONCE, &c, 1000));
        assert!(!k.verify("198.51.100.10:40000".parse().unwrap(), NONCE, &c, 1000));
        assert!(!k.verify("198.51.100.9:40001".parse().unwrap(), NONCE, &c, 1000));
        assert!(!k.verify(a, NONCE + 1, &c, 1000));
        let mut bad = c;
        bad[15] ^= 1;
        assert!(!k.verify(a, NONCE, &bad, 1000));
    }

    #[test]
    fn secret_rotation_keeps_one_previous_secret() {
        let t = Instant::now();
        let mut k = CookieKeys::with_secret(SECRET, t, Duration::from_secs(120));
        let a: SocketAddr = "198.51.100.9:40000".parse().unwrap();
        let c = k.issue(a, NONCE, 1000);
        assert!(!k.tick(t + Duration::from_secs(119)));
        assert!(k.tick(t + Duration::from_secs(120)));
        assert!(
            k.verify(a, NONCE, &c, 1000),
            "cookie issued before the rotation still verifies"
        );
        assert_ne!(k.issue(a, NONCE, 1000), c, "new cookies use the new secret");
        assert!(k.tick(t + Duration::from_secs(240)));
        assert!(
            !k.verify(a, NONCE, &c, 1000),
            "two rotations later the old secret is gone"
        );
    }

    #[test]
    fn rotation_period_has_a_floor() {
        let t = Instant::now();
        let mut k = CookieKeys::with_secret(SECRET, t, Duration::from_secs(1));
        assert!(!k.tick(t + Duration::from_secs(59)));
        assert!(k.tick(t + Duration::from_secs(60)));
    }

    #[test]
    fn random_secrets_differ() {
        let t = Instant::now();
        let a = CookieKeys::new(t, MIN_ROTATION);
        let b = CookieKeys::new(t, MIN_ROTATION);
        assert_ne!(a.current, b.current);
    }
}
