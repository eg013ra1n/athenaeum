//! Process-wide debounce for refusal-triggered authorized-peer refreshes
//! (sync delivery-forever, task 7).
//!
//! When a receiver gate refuses an UNKNOWN peer, that is a strong hint our cached
//! authorized-device set is stale — the peer may be a machine just added to the
//! account. [`RefusalRefresher`] rate-limits the resulting hub refresh so a burst
//! of refusals from one un-refreshed peer (whose retry loop redelivers on a short
//! cadence) triggers at most ONE hub round-trip per `min_gap`.
//!
//! The whole state is a single `Mutex<Option<Instant>>` holding the last time a
//! refresh fired. [`should_fire`](RefusalRefresher::should_fire) is an atomic
//! check-and-stamp: it returns `true` (and stamps `now`) only when no refresh has
//! fired within `min_gap`, so concurrent callers from both gates never double-fire.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A rate-limiter shared process-wide (one instance on
/// [`SyncRuntime`](crate::sync::SyncRuntime)) across both receiver gates. See the
/// module docs.
#[derive(Debug)]
pub struct RefusalRefresher {
    min_gap: Duration,
    last: Mutex<Option<Instant>>,
}

impl RefusalRefresher {
    /// Build a refresher that fires at most once per `min_gap`.
    pub fn new(min_gap: Duration) -> Self {
        Self {
            min_gap,
            last: Mutex::new(None),
        }
    }

    /// Atomically decide whether a refresh may fire now. Returns `true` — and
    /// stamps the current instant as the last-fired time — only when at least
    /// `min_gap` has elapsed since the previous fire (or none has fired yet).
    /// Returns `false` otherwise, without changing state. The check and the stamp
    /// happen under one lock, so two concurrent callers can never both fire.
    pub fn should_fire(&self) -> bool {
        let now = Instant::now();
        // Poison-tolerant: the stored `Instant` is plain data, so recover the
        // guard rather than panic if a prior holder unwound.
        let mut last = self.last.lock().unwrap_or_else(|e| e.into_inner());
        let fire = match *last {
            Some(prev) => now.duration_since(prev) >= self.min_gap,
            None => true,
        };
        if fire {
            *last = Some(now);
        }
        fire
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fires_once_per_gap() {
        let r = RefusalRefresher::new(Duration::from_millis(50));
        assert!(r.should_fire());
        assert!(!r.should_fire());
        std::thread::sleep(Duration::from_millis(60));
        assert!(r.should_fire());
    }

    #[test]
    fn first_call_always_fires() {
        let r = RefusalRefresher::new(Duration::from_secs(3600));
        assert!(r.should_fire());
    }

    #[test]
    fn concurrent_callers_fire_at_most_once_per_gap() {
        use std::sync::Arc;
        let r = Arc::new(RefusalRefresher::new(Duration::from_secs(3600)));
        let handles: Vec<_> = (0..16)
            .map(|_| {
                let r = Arc::clone(&r);
                std::thread::spawn(move || r.should_fire())
            })
            .collect();
        let fires = handles
            .into_iter()
            .map(|h| h.join().unwrap())
            .filter(|&fired| fired)
            .count();
        assert_eq!(fires, 1, "exactly one concurrent caller may fire within the gap");
    }
}
