//! Token-bucket rate limits kept in memory only (nothing about addresses is persisted), with the time to the next
//! token for `Retry-After`.

use std::collections::HashMap;
use std::hash::Hash;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::Instant;

use crate::config::RateLimits;

#[derive(Debug, Clone, Copy)]
struct Bucket {
    tokens: f64,
    last: Instant,
}

/// Token buckets keyed by `K`: `rate` tokens per second, at most `burst` stored.
pub struct Buckets<K> {
    rate: f64,
    burst: f64,
    map: Mutex<HashMap<K, Bucket>>,
}

impl<K: Hash + Eq + Clone> Buckets<K> {
    pub fn new(rate: f64, burst: f64) -> Self {
        Buckets {
            rate: rate.max(1e-9),
            burst: burst.max(1.0),
            map: Mutex::new(HashMap::new()),
        }
    }

    /// Takes one token for `key`: `Ok` or `Err(seconds until a token is back)`.
    pub fn take(&self, key: &K, now: Instant) -> Result<(), i64> {
        let mut map = self.map.lock().unwrap_or_else(|p| p.into_inner());
        if map.len() > 200_000 {
            // A flood of distinct keys: forget the full buckets first, then everything.
            let (rate, burst) = (self.rate, self.burst);
            map.retain(|_, b| b.tokens + now.saturating_duration_since(b.last).as_secs_f64() * rate < burst);
            if map.len() > 200_000 {
                map.clear();
            }
        }
        let b = map.entry(key.clone()).or_insert(Bucket {
            tokens: self.burst,
            last: now,
        });
        let elapsed = now.saturating_duration_since(b.last).as_secs_f64();
        b.tokens = (b.tokens + elapsed * self.rate).min(self.burst);
        b.last = now;
        if b.tokens >= 1.0 {
            b.tokens -= 1.0;
            Ok(())
        } else {
            Err(((1.0 - b.tokens) / self.rate).ceil().max(1.0) as i64)
        }
    }

    /// Gives back a token taken by [`Buckets::take`] (the attempt it paid for succeeded).
    pub fn refund(&self, key: &K) {
        let mut map = self.map.lock().unwrap_or_else(|p| p.into_inner());
        if let Some(b) = map.get_mut(key) {
            b.tokens = (b.tokens + 1.0).min(self.burst);
        }
    }

    /// Forgets `key`: its bucket is full again.
    pub fn forget(&self, key: &K) {
        self.map.lock().unwrap_or_else(|p| p.into_inner()).remove(key);
    }

    /// Forgets buckets that have refilled completely (they behave exactly like new ones).
    pub fn sweep(&self, now: Instant) {
        let mut map = self.map.lock().unwrap_or_else(|p| p.into_inner());
        let (rate, burst) = (self.rate, self.burst);
        map.retain(|_, b| b.tokens + now.saturating_duration_since(b.last).as_secs_f64() * rate < burst);
    }

    pub fn len(&self) -> usize {
        self.map.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// Every limiter of the service.
pub struct Limiters {
    pub per_ip: Buckets<IpAddr>,
    pub per_user: Buckets<String>,
    pub auth_per_ip: Buckets<IpAddr>,
    pub room_join: Buckets<String>,
    pub friend_requests: Buckets<String>,
    pub reports: Buckets<String>,
    /// `GET /v1/population` (public).
    pub population: Buckets<IpAddr>,
    /// 1.1.6: draft actions per player.
    pub draft: Buckets<String>,
    /// 1.1.7: places taken or left in the stands, lists of watchable matches, per player.
    pub spectate: Buckets<String>,
    /// Security review M5: new accounts per client network.
    pub accounts: Buckets<IpAddr>,
    /// Security review M4: room matches started per player.
    pub room_starts: Buckets<String>,
}

impl Limiters {
    pub fn new(l: &RateLimits) -> Self {
        Limiters {
            per_ip: Buckets::new(l.per_ip_rps, l.per_ip_burst),
            per_user: Buckets::new(l.per_user_rps, l.per_user_burst),
            auth_per_ip: Buckets::new(l.auth_per_ip_per_min / 60.0, l.auth_per_ip_per_min.max(1.0)),
            room_join: Buckets::new(l.room_join_per_min / 60.0, l.room_join_per_min.max(1.0)),
            friend_requests: Buckets::new(l.friend_requests_per_hour / 3600.0, l.friend_requests_per_hour.max(1.0)),
            reports: Buckets::new(l.reports_per_day / 86_400.0, l.reports_per_day.max(1.0)),
            population: Buckets::new(l.population_per_ip_per_min / 60.0, l.population_per_ip_per_min.max(1.0)),
            draft: Buckets::new(l.draft_actions_per_min / 60.0, l.draft_actions_per_min.max(1.0)),
            spectate: Buckets::new(l.spectate_per_min / 60.0, l.spectate_per_min.max(1.0)),
            accounts: Buckets::new(l.accounts_per_ip_per_day / 86_400.0, l.accounts_per_ip_per_day.max(1.0)),
            room_starts: Buckets::new(l.room_starts_per_hour / 3600.0, l.room_starts_per_hour.max(1.0)),
        }
    }

    pub fn sweep(&self, now: Instant) {
        self.per_ip.sweep(now);
        self.per_user.sweep(now);
        self.auth_per_ip.sweep(now);
        self.room_join.sweep(now);
        self.friend_requests.sweep(now);
        self.reports.sweep(now);
        self.population.sweep(now);
        self.draft.sweep(now);
        self.spectate.sweep(now);
        self.accounts.sweep(now);
        self.room_starts.sweep(now);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn bucket_burst_then_refill() {
        let b = Buckets::new(2.0, 3.0);
        let t0 = Instant::now();
        assert!(b.take(&1, t0).is_ok());
        assert!(b.take(&1, t0).is_ok());
        assert!(b.take(&1, t0).is_ok());
        assert_eq!(b.take(&1, t0), Err(1), "burst exhausted");
        assert!(b.take(&2, t0).is_ok(), "keys are independent");
        assert!(
            b.take(&1, t0 + Duration::from_millis(500)).is_ok(),
            "one token after 0.5 s at 2/s"
        );
        assert!(b.take(&1, t0 + Duration::from_millis(500)).is_err());
        b.sweep(t0 + Duration::from_secs(10));
        assert!(b.is_empty());
        let slow = Buckets::new(1.0 / 60.0, 1.0);
        assert!(slow.take(&1, t0).is_ok());
        assert_eq!(slow.take(&1, t0), Err(60));
        slow.refund(&1);
        assert!(slow.take(&1, t0).is_ok(), "a refunded token can be taken again");
        assert!(slow.take(&1, t0).is_err());
        slow.forget(&1);
        assert!(slow.take(&1, t0).is_ok(), "a forgotten key starts full");
    }
}
