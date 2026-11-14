//! Time source of the service, in UTC Unix seconds. Every deadline and expiry reads it, so tests (and servers that
//! declare `test-clock`) can move time forward deterministically.

use std::sync::atomic::{AtomicI64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

pub trait Clock: Send + Sync {
    fn now(&self) -> i64;
    /// Moves a manual clock forward; `false` for the system clock.
    fn advance(&self, _secs: i64) -> bool {
        false
    }
}

pub struct SystemClock;

impl Clock for SystemClock {
    fn now(&self) -> i64 {
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs() as i64)
            .unwrap_or(0)
    }
}

/// Starts at the system time (or a given time) and only moves when told.
pub struct ManualClock(AtomicI64);

impl ManualClock {
    pub fn new(start: i64) -> Self {
        ManualClock(AtomicI64::new(start))
    }
    pub fn starting_now() -> Self {
        Self::new(SystemClock.now())
    }
}

impl Clock for ManualClock {
    fn now(&self) -> i64 {
        self.0.load(Ordering::SeqCst)
    }
    fn advance(&self, secs: i64) -> bool {
        self.0.fetch_add(secs.max(0), Ordering::SeqCst);
        true
    }
}
