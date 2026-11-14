//! Rate limiting with memory that does not grow with the number of sources.
//!
//! Before a source has proven its address the relay must not keep a record per source (an attacker spoofing
//! millions of addresses would otherwise fill memory). [`SlotCounter`] and [`SlotTimes`] therefore hash the
//! source into a fixed array with a randomly keyed hash: two sources may share a slot (a harmless extra drop),
//! and an attacker cannot aim at a victim's slot because the key is secret.
//!
//! The relay's workers share them: each slot has a lock of its own, so that two sources rarely wait for each other.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hash};
use std::sync::Mutex;
use std::time::{Duration, Instant};

pub const SLOTS: usize = 4096;

/// At most `limit` events per one-second window per slot. A limit of 0 means unlimited.
pub struct SlotCounter {
    hasher: RandomState,
    slots: Box<[Mutex<(u64, u32)>]>,
    limit: u32,
}

impl SlotCounter {
    pub fn new(limit: u32) -> Self {
        SlotCounter {
            hasher: RandomState::new(),
            slots: (0..SLOTS).map(|_| Mutex::new((u64::MAX, 0))).collect(),
            limit,
        }
    }

    /// `second` is any monotonically increasing whole-second clock.
    pub fn allow<K: Hash>(&self, key: &K, second: u64) -> bool {
        if self.limit == 0 {
            return true;
        }
        let i = (self.hasher.hash_one(key) as usize) % SLOTS;
        let mut s = self.slots[i].lock().unwrap_or_else(|e| e.into_inner());
        if s.0 != second {
            *s = (second, 0);
        }
        if s.1 >= self.limit {
            return false;
        }
        s.1 += 1;
        true
    }

    pub fn slot_count(&self) -> usize {
        self.slots.len()
    }
}

/// At most one event per `period` per slot.
pub struct SlotTimes {
    hasher: RandomState,
    slots: Box<[Mutex<Option<Instant>>]>,
    period: Duration,
}

impl SlotTimes {
    pub fn new(period: Duration) -> Self {
        SlotTimes {
            hasher: RandomState::new(),
            slots: (0..SLOTS).map(|_| Mutex::new(None)).collect(),
            period,
        }
    }

    pub fn allow<K: Hash>(&self, key: &K, now: Instant) -> bool {
        let i = (self.hasher.hash_one(key) as usize) % SLOTS;
        let mut s = self.slots[i].lock().unwrap_or_else(|e| e.into_inner());
        match *s {
            Some(t) if now.saturating_duration_since(t) < self.period => false,
            _ => {
                *s = Some(now);
                true
            }
        }
    }
}

/// Packets-per-second and bytes-per-second token buckets, each one second deep (the burst is one second of
/// the rate). A rate of 0 means unlimited.
pub struct TokenBucket {
    pps: f64,
    bps: f64,
    pkts: f64,
    bytes: f64,
    last: Instant,
}

impl TokenBucket {
    pub fn new(pps: u32, bytes_per_s: u32, now: Instant) -> Self {
        TokenBucket {
            pps: pps as f64,
            bps: bytes_per_s as f64,
            pkts: pps as f64,
            bytes: bytes_per_s as f64,
            last: now,
        }
    }

    pub fn allow(&mut self, now: Instant, len: usize) -> bool {
        let dt = now.saturating_duration_since(self.last).as_secs_f64();
        self.last = self.last.max(now);
        if self.pps > 0.0 {
            self.pkts = (self.pkts + dt * self.pps).min(self.pps);
        }
        if self.bps > 0.0 {
            self.bytes = (self.bytes + dt * self.bps).min(self.bps);
        }
        if (self.pps > 0.0 && self.pkts < 1.0) || (self.bps > 0.0 && self.bytes < len as f64) {
            return false;
        }
        if self.pps > 0.0 {
            self.pkts -= 1.0;
        }
        if self.bps > 0.0 {
            self.bytes -= len as f64;
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counter_limits_per_second_and_resets() {
        let c = SlotCounter::new(3);
        let k = "10.0.0.1";
        assert!((0..3).all(|_| c.allow(&k, 10)));
        assert!(!c.allow(&k, 10));
        assert!(c.allow(&k, 11));
    }

    #[test]
    fn counter_memory_is_fixed() {
        let c = SlotCounter::new(20);
        for i in 0..100_000u32 {
            c.allow(&i, 1);
        }
        assert_eq!(c.slot_count(), SLOTS);
    }

    #[test]
    fn zero_limit_is_unlimited() {
        let c = SlotCounter::new(0);
        assert!((0..10_000).all(|_| c.allow(&1u8, 1)));
    }

    #[test]
    fn times_allow_once_per_period() {
        let t = Instant::now();
        let s = SlotTimes::new(Duration::from_secs(1));
        assert!(s.allow(&1u8, t));
        assert!(!s.allow(&1u8, t + Duration::from_millis(999)));
        assert!(s.allow(&1u8, t + Duration::from_secs(1)));
    }

    #[test]
    fn bucket_packets_and_bytes() {
        let t = Instant::now();
        let mut b = TokenBucket::new(10, 1000, t);
        assert!((0..10).all(|_| b.allow(t, 50)));
        assert!(!b.allow(t, 50), "packet budget spent");
        assert!(b.allow(t + Duration::from_millis(100), 50), "one packet refilled");
        let mut b = TokenBucket::new(1000, 1000, t);
        assert!(b.allow(t, 800));
        assert!(!b.allow(t, 800), "byte budget spent");
        assert!(b.allow(t + Duration::from_secs(1), 800));
        // burst never exceeds one second of the rate
        let mut b = TokenBucket::new(5, 100_000, t);
        let later = t + Duration::from_secs(100);
        assert_eq!((0..20).filter(|_| b.allow(later, 1)).count(), 5);
    }
}
