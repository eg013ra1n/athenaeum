//! One full-jitter exponential back-off for the whole live exchange (spec
//! §4.1 reconnect, §4.6 per-request retry, §7.3 provider dials; plan P3).
//! `reset_all` is Sync now's "clears every back-off" (L10).

use std::sync::OnceLock;
use std::time::Duration;

use crate::geometry::ransac::SplitMix64;

pub const BACKOFF_BASE: Duration = Duration::from_secs(1);
pub const BACKOFF_CAP: Duration = Duration::from_secs(60);
/// Full jitter can draw ~0; the floor keeps a failing loop from spinning.
pub const BACKOFF_FLOOR: Duration = Duration::from_millis(100);

pub struct Backoff {
    attempt: u32,
    rng: SplitMix64,
}

impl Default for Backoff {
    fn default() -> Self {
        Self::new()
    }
}

impl Backoff {
    pub fn new() -> Self {
        let seed = uuid::Uuid::new_v4().as_u128() as u64;
        Self::with_seed(seed)
    }

    pub fn with_seed(seed: u64) -> Self {
        Self {
            attempt: 0,
            rng: SplitMix64(seed),
        }
    }

    pub fn attempt(&self) -> u32 {
        self.attempt
    }

    pub fn reset(&mut self) {
        self.attempt = 0;
    }

    /// Uniform in `[0, min(cap, base · 2^attempt)]`, floored; advances the attempt.
    pub fn next_delay(&mut self) -> Duration {
        let exp = 1u32 << self.attempt.min(16);
        let upper = BACKOFF_CAP.min(BACKOFF_BASE.saturating_mul(exp));
        self.attempt = self.attempt.saturating_add(1);
        let drawn = upper.mul_f64(self.rng.next_f64());
        drawn.max(BACKOFF_FLOOR)
    }
}

fn reset_tx() -> &'static tokio::sync::watch::Sender<u64> {
    static TX: OnceLock<tokio::sync::watch::Sender<u64>> = OnceLock::new();
    TX.get_or_init(|| tokio::sync::watch::channel(0u64).0)
}

/// Wake every sleeping retry and make it start over (Sync now, P26).
pub fn reset_all() {
    reset_tx().send_modify(|n| *n = n.wrapping_add(1));
    tracing::info!("collab back-offs cleared");
}

/// Serialises the tests that fire [`reset_all`] against the tests that
/// count a retry loop's attempts. The reset channel is process-global, so a
/// reset fired by one test while another's retry sleeps restarts that
/// retry's back-off (by design, P26) and breaks its attempt count — a flake
/// seen in the full core suite on 2026-09-26.
#[cfg(test)]
pub(crate) static RESET_ALL_TEST_SERIAL: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

pub fn reset_signal() -> tokio::sync::watch::Receiver<u64> {
    let mut rx = reset_tx().subscribe();
    rx.mark_unchanged();
    rx
}

/// Sleep `delay`, or less if [`reset_all`] fires. `true` = cut short.
pub async fn sleep_or_reset(
    delay: Duration,
    reset: &mut tokio::sync::watch::Receiver<u64>,
) -> bool {
    tokio::select! {
        _ = tokio::time::sleep(delay) => false,
        changed = reset.changed() => changed.is_ok(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delays_stay_inside_the_full_jitter_envelope_and_cap_at_sixty_seconds() {
        let mut b = Backoff::with_seed(7);
        for attempt in 0..20u32 {
            let upper = BACKOFF_CAP.min(BACKOFF_BASE.saturating_mul(1u32 << attempt.min(16)));
            let d = b.next_delay();
            assert!(
                d >= BACKOFF_FLOOR && d <= upper.max(BACKOFF_FLOOR),
                "attempt {attempt}: {d:?} > {upper:?}"
            );
        }
        assert!(b.attempt() >= 6);
        b.reset();
        assert_eq!(b.attempt(), 0);
        assert!(b.next_delay() <= BACKOFF_BASE);
    }

    #[test]
    fn same_seed_same_sequence() {
        let a: Vec<_> = {
            let mut b = Backoff::with_seed(42);
            (0..8).map(|_| b.next_delay()).collect()
        };
        let c: Vec<_> = {
            let mut b = Backoff::with_seed(42);
            (0..8).map(|_| b.next_delay()).collect()
        };
        assert_eq!(a, c);
    }

    #[tokio::test]
    async fn reset_all_cuts_a_sleep_short() {
        let _serial = RESET_ALL_TEST_SERIAL.lock().await;
        let mut rx = reset_signal();
        let sleeper =
            tokio::spawn(async move { sleep_or_reset(Duration::from_secs(30), &mut rx).await });
        tokio::time::sleep(Duration::from_millis(20)).await;
        reset_all();
        let cut = tokio::time::timeout(Duration::from_secs(2), sleeper)
            .await
            .unwrap()
            .unwrap();
        assert!(cut);
    }
}
