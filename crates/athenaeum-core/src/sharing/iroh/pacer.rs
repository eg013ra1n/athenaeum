//! Device-wide upload pacer for the iroh-blobs provider throttle hook (W1).
//!
//! The provider asks permission for every ~16 KiB payload chunk it is about to
//! write (`ThrottleMode::Intercept` — the rpc reply is awaited inline by the
//! provider's writer, so DELAYING the reply is the throttle; an `Err` reply
//! ABORTS the transfer, so the policy here never errors). This type only
//! computes the delay; the consumer loop in [`super::build_router`] sleeps it on
//! a spawned task and then replies.
//!
//! One pacer per endpoint = one budget for the whole device, shared by every
//! peer and every concurrent GET — which is exactly the promise the setting
//! makes ("this device's total sync upload bandwidth"), and what keeps the
//! observatory's uplink usable for SSH while N peers pull at once.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Leaky-bucket pacer over a virtual clock.
///
/// `rate == 0` means unlimited: [`reserve`](Self::reserve) short-circuits to
/// zero delay without touching the clock, so the disabled path costs one atomic
/// load per chunk. A nonzero rate schedules each reservation at
/// `max(next_free, now)` and advances `next_free` by `size / rate` — idle time
/// therefore earns NO credit: after a quiet minute the first chunk goes out
/// immediately, but the second is paced as if the bucket were fresh, so a burst
/// can never "spend" saved-up budget and swamp the link the setting exists to
/// protect.
pub struct UploadPacer {
    /// Bytes per second; 0 = unlimited. Written by
    /// [`set_rate`](Self::set_rate) at startup and on a live settings change.
    rate: AtomicU64,
    /// The virtual instant at which the NEXT reservation may start. `None`
    /// until the first paced reservation, and reset by `set_rate` so a rate
    /// change never inherits a schedule computed at the old rate.
    next_free: Mutex<Option<Instant>>,
}

impl UploadPacer {
    pub fn new(bytes_per_sec: u64) -> Self {
        Self {
            rate: AtomicU64::new(bytes_per_sec),
            next_free: Mutex::new(None),
        }
    }

    /// The current limit in bytes/sec (0 = unlimited).
    pub fn rate(&self) -> u64 {
        self.rate.load(Ordering::Relaxed)
    }

    /// Apply a new limit immediately. Clears the virtual clock so the next
    /// reservation is scheduled purely at the NEW rate — chunks already
    /// sleeping keep their old (correct-at-the-time) delays.
    pub fn set_rate(&self, bytes_per_sec: u64) {
        self.rate.store(bytes_per_sec, Ordering::Relaxed);
        *self.next_free.lock().expect("pacer mutex poisoned") = None;
    }

    /// Reserve a transmission slot for `size` bytes and return how long the
    /// caller must wait before letting them out. Non-blocking — the sleep is
    /// the caller's job (on a spawned task, never on the provider-event
    /// consumer, which also carries acks).
    pub fn reserve(&self, size: u64) -> Duration {
        self.reserve_at(Instant::now(), size)
    }

    /// Deterministic seam for tests: same math as [`reserve`](Self::reserve)
    /// with an explicit `now`, so pacing arithmetic is pinned without sleeping.
    fn reserve_at(&self, now: Instant, size: u64) -> Duration {
        let rate = self.rate.load(Ordering::Relaxed);
        if rate == 0 {
            return Duration::ZERO;
        }
        let cost = Duration::from_secs_f64(size as f64 / rate as f64);
        let mut next_free = self.next_free.lock().expect("pacer mutex poisoned");
        // `max(_, now)`: an idle gap moves the schedule forward to the present
        // instead of banking it as burst credit.
        let start = next_free.map_or(now, |nf| nf.max(now));
        *next_free = Some(start + cost);
        start.saturating_duration_since(now)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const KIB16: u64 = 16 * 1024;

    #[test]
    fn pacer_zero_rate_reserves_zero_delay() {
        let pacer = UploadPacer::new(0);
        let now = Instant::now();
        for _ in 0..100 {
            assert_eq!(pacer.reserve_at(now, KIB16), Duration::ZERO);
        }
        // And the disabled path never builds a schedule that could delay the
        // first chunk after the limit is later enabled.
        assert!(pacer.next_free.lock().unwrap().is_none());
    }

    /// 64 × 16 KiB = 1 MiB at 1 MB/s (decimal) ⇒ the LAST reservation starts
    /// ~1.048 s of virtual time in — cumulative budget, not per-chunk.
    #[test]
    fn pacer_paces_cumulative_chunks_at_rate() {
        let pacer = UploadPacer::new(1_000_000);
        let now = Instant::now();
        let mut last = Duration::ZERO;
        for _ in 0..64 {
            last = pacer.reserve_at(now, KIB16);
        }
        let expected = Duration::from_secs_f64((63 * KIB16) as f64 / 1_000_000.0);
        let diff = if last > expected {
            last - expected
        } else {
            expected - last
        };
        assert!(
            diff < Duration::from_millis(5),
            "cumulative pacing: last delay {last:?}, expected ≈{expected:?}"
        );
    }

    #[test]
    fn pacer_idle_gap_earns_no_credit() {
        let pacer = UploadPacer::new(1_000_000);
        let t0 = Instant::now();
        // Build up a schedule…
        for _ in 0..64 {
            pacer.reserve_at(t0, KIB16);
        }
        // …then go idle far past it. The first post-idle chunk goes NOW (zero
        // delay), and the schedule restarts from `now` — not from the stale
        // virtual clock, and with no banked credit for the quiet time.
        let t1 = t0 + Duration::from_secs(10);
        assert_eq!(
            pacer.reserve_at(t1, KIB16),
            Duration::ZERO,
            "first chunk after idle is immediate"
        );
        let second = pacer.reserve_at(t1, KIB16);
        let expected = Duration::from_secs_f64(KIB16 as f64 / 1_000_000.0);
        assert!(
            second >= expected - Duration::from_millis(1)
                && second <= expected + Duration::from_millis(1),
            "second chunk is paced from the restarted schedule, got {second:?}"
        );
    }

    #[test]
    fn pacer_set_rate_applies_to_next_reserve() {
        let pacer = UploadPacer::new(1_000_000);
        let now = Instant::now();
        for _ in 0..64 {
            pacer.reserve_at(now, KIB16);
        }
        // Live change: the old schedule is discarded, the new rate governs.
        pacer.set_rate(0);
        assert_eq!(
            pacer.reserve_at(now, KIB16),
            Duration::ZERO,
            "0 = unlimited immediately"
        );
        pacer.set_rate(2_000_000);
        assert_eq!(pacer.rate(), 2_000_000);
        assert_eq!(
            pacer.reserve_at(now, KIB16),
            Duration::ZERO,
            "fresh schedule after a rate change"
        );
        let second = pacer.reserve_at(now, KIB16);
        let expected = Duration::from_secs_f64(KIB16 as f64 / 2_000_000.0);
        assert!(
            (second.as_secs_f64() - expected.as_secs_f64()).abs() < 0.001,
            "paced at the NEW rate, got {second:?}"
        );
    }

    /// The budget is one per pacer, not per caller: interleaved reservations
    /// from "two peers" sum onto one schedule. This is what makes the setting a
    /// DEVICE-wide cap rather than a per-connection one that N peers multiply.
    #[test]
    fn pacer_is_shared_budget_across_callers() {
        let pacer = UploadPacer::new(1_000_000);
        let now = Instant::now();
        let mut last = Duration::ZERO;
        // 32 chunks attributed to "peer A", 32 to "peer B", interleaved.
        for _ in 0..32 {
            pacer.reserve_at(now, KIB16); // A
            last = pacer.reserve_at(now, KIB16); // B
        }
        let expected = Duration::from_secs_f64((63 * KIB16) as f64 / 1_000_000.0);
        assert!(
            last >= expected - Duration::from_millis(5),
            "two interleaved callers share ONE budget: last delay {last:?}, expected ≥≈{expected:?}"
        );
    }
}
