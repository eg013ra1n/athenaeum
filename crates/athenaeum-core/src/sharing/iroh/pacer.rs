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
//!
//! Class-aware (collab v3 wave 3, spec §8, L1): the personal store's uploads
//! ([`UploadClass::Personal`]) always use the device bucket above, unchanged.
//! Collab uploads ([`UploadClass::Collab`]) share that bucket while no
//! personal upload runs; while one does, they are paced on their own
//! schedule at [`COLLAB_SHARE_WHILE_PERSONAL`] of the link — the cap when one
//! is set, else the personal rate observed over [`PERSONAL_RATE_WINDOW`] —
//! never below [`COLLAB_FLOOR_BYTES_PER_SEC`]. Collab never charges the
//! personal bucket, so a personal upload is never slowed by collab.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Which store an upload chunk belongs to (spec §8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UploadClass {
    /// The personal-sync store: always first.
    Personal,
    /// The collab store: yields to an active personal upload.
    Collab,
}

/// The share of the link collab uploads get while a personal upload runs
/// (spec §8: "a fixed low share of the link").
pub const COLLAB_SHARE_WHILE_PERSONAL: f64 = 0.10;
/// The floor of that share, so collab never stalls outright on a slow link.
pub const COLLAB_FLOOR_BYTES_PER_SEC: u64 = 64 * 1024;
/// The window over which the personal upload rate is observed when no cap is
/// set (the base of the collab share).
pub const PERSONAL_RATE_WINDOW: Duration = Duration::from_secs(2);

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
    /// The collab schedule while a personal upload runs (its own virtual
    /// clock, reset when the last personal upload ends).
    collab_next_free: Mutex<Option<Instant>>,
    /// How many personal uploads are running ([`PersonalUploadGuard`]s alive).
    personal_active: AtomicUsize,
    /// Personal chunks of the last [`PERSONAL_RATE_WINDOW`], `(at, bytes)`.
    personal_window: Mutex<VecDeque<(Instant, u64)>>,
}

impl UploadPacer {
    pub fn new(bytes_per_sec: u64) -> Self {
        Self {
            rate: AtomicU64::new(bytes_per_sec),
            next_free: Mutex::new(None),
            collab_next_free: Mutex::new(None),
            personal_active: AtomicUsize::new(0),
            personal_window: Mutex::new(VecDeque::new()),
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

    /// Reserve a slot for `size` bytes of `class`; see the module doc for the
    /// policy. Non-blocking, like [`reserve`](Self::reserve).
    pub fn reserve_class(&self, size: u64, class: UploadClass) -> Duration {
        self.reserve_class_at(Instant::now(), size, class)
    }

    /// Deterministic seam of [`reserve_class`](Self::reserve_class).
    pub(crate) fn reserve_class_at(&self, now: Instant, size: u64, class: UploadClass) -> Duration {
        match class {
            UploadClass::Personal => {
                self.record_personal_at(now, size);
                self.reserve_at(now, size)
            }
            UploadClass::Collab if self.personal_active.load(Ordering::Relaxed) == 0 => {
                self.reserve_at(now, size)
            }
            UploadClass::Collab => {
                let cap = self.rate();
                let base = if cap > 0 {
                    cap
                } else {
                    self.personal_rate_at(now)
                };
                let share = ((base as f64 * COLLAB_SHARE_WHILE_PERSONAL) as u64)
                    .max(COLLAB_FLOOR_BYTES_PER_SEC);
                let mut next = self.collab_next_free.lock().expect("pacer mutex poisoned");
                let start = next.map_or(now, |t| t.max(now));
                *next = Some(start + Duration::from_secs_f64(size as f64 / share as f64));
                start.saturating_duration_since(now)
            }
        }
    }

    /// Record `size` personal bytes let out at `at` (the observed-rate base).
    pub(crate) fn record_personal_at(&self, at: Instant, size: u64) {
        let mut w = self.personal_window.lock().expect("pacer mutex poisoned");
        w.push_back((at, size));
        while w
            .front()
            .is_some_and(|(t, _)| at.saturating_duration_since(*t) > PERSONAL_RATE_WINDOW)
        {
            w.pop_front();
        }
    }

    /// Personal bytes/sec over the last [`PERSONAL_RATE_WINDOW`] before `now`.
    fn personal_rate_at(&self, now: Instant) -> u64 {
        let w = self.personal_window.lock().expect("pacer mutex poisoned");
        let bytes: u64 = w
            .iter()
            .filter(|(t, _)| now.saturating_duration_since(*t) <= PERSONAL_RATE_WINDOW)
            .map(|(_, b)| b)
            .sum();
        (bytes as f64 / PERSONAL_RATE_WINDOW.as_secs_f64()) as u64
    }

    /// Mark a personal upload active for the guard's lifetime (held by the
    /// personal provider consumer for every payload-carrying get).
    pub fn personal_upload(self: &Arc<Self>) -> PersonalUploadGuard {
        self.personal_active.fetch_add(1, Ordering::Relaxed);
        PersonalUploadGuard {
            pacer: Arc::clone(self),
        }
    }

    /// How many personal uploads are running.
    pub fn personal_active(&self) -> usize {
        self.personal_active.load(Ordering::Relaxed)
    }
}

/// RAII marker of one running personal upload ([`UploadPacer::personal_upload`]).
/// When the last one drops, the collab schedule is cleared so collab returns
/// to the shared bucket without a stale delay.
pub struct PersonalUploadGuard {
    pacer: Arc<UploadPacer>,
}

impl Drop for PersonalUploadGuard {
    fn drop(&mut self) {
        let prev = self.pacer.personal_active.fetch_sub(1, Ordering::Relaxed);
        if prev == 1 {
            match self.pacer.collab_next_free.lock() {
                Ok(mut next) => *next = None,
                Err(e) => {
                    tracing::error!(error = %e, "upload pacer poisoned; collab schedule kept")
                }
            }
        }
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
    #[test]
    fn collab_uses_the_shared_bucket_when_no_personal_upload_runs() {
        let p = Arc::new(UploadPacer::new(0));
        assert_eq!(
            p.reserve_class(16 * 1024, UploadClass::Collab),
            Duration::ZERO
        );
        // With a cap, collab draws on the SAME bucket as personal.
        let p = Arc::new(UploadPacer::new(1_000_000));
        let now = Instant::now();
        assert_eq!(
            p.reserve_class_at(now, 500_000, UploadClass::Personal),
            Duration::ZERO
        );
        let wait = p.reserve_class_at(now, 1, UploadClass::Collab);
        assert!((wait.as_millis() as i64 - 500).abs() <= 5, "{wait:?}");
    }

    #[test]
    fn collab_paces_at_ten_percent_of_the_cap_while_personal_uploads_run() {
        let p = Arc::new(UploadPacer::new(1_000_000)); // 1 MB/s cap
        let _g = p.personal_upload();
        let now = Instant::now();
        // 10 % of 1 MB/s = 100 kB/s: two 50 kB collab chunks → the second waits 0.5 s
        assert_eq!(
            p.reserve_class_at(now, 50_000, UploadClass::Collab),
            Duration::ZERO
        );
        let wait = p.reserve_class_at(now, 50_000, UploadClass::Collab);
        assert!((wait.as_millis() as i64 - 500).abs() <= 5, "{wait:?}");
    }

    #[test]
    fn without_a_cap_collab_follows_the_observed_personal_rate() {
        let p = Arc::new(UploadPacer::new(0));
        let g = p.personal_upload();
        let now = Instant::now();
        // personal moved 2 MB in the last 2 s → 1 MB/s → collab gets 100 kB/s
        p.record_personal_at(now - Duration::from_millis(1500), 2_000_000);
        assert_eq!(
            p.reserve_class_at(now, 100_000, UploadClass::Collab),
            Duration::ZERO
        );
        let wait = p.reserve_class_at(now, 100_000, UploadClass::Collab);
        assert!((wait.as_millis() as i64 - 1000).abs() <= 10, "{wait:?}");
        drop(g);
        assert_eq!(p.personal_active(), 0);
        assert_eq!(
            p.reserve_class_at(now, 100_000, UploadClass::Collab),
            Duration::ZERO
        );
    }

    #[test]
    fn a_slow_personal_upload_leaves_collab_the_floor() {
        let p = Arc::new(UploadPacer::new(0));
        let _g = p.personal_upload();
        let now = Instant::now();
        // No personal bytes observed yet: collab gets the 64 KiB/s floor.
        assert_eq!(
            p.reserve_class_at(now, COLLAB_FLOOR_BYTES_PER_SEC, UploadClass::Collab),
            Duration::ZERO
        );
        let wait = p.reserve_class_at(now, 1, UploadClass::Collab);
        assert!((wait.as_millis() as i64 - 1000).abs() <= 5, "{wait:?}");
    }

    #[test]
    fn personal_is_never_slowed_by_collab() {
        let p = Arc::new(UploadPacer::new(0));
        let _g = p.personal_upload();
        for _ in 0..10 {
            assert_eq!(
                p.reserve_class(16 * 1024, UploadClass::Personal),
                Duration::ZERO
            );
        }
        // With a cap, a burst of collab chunks never delays the personal bucket.
        let p = Arc::new(UploadPacer::new(1_000_000));
        let _g = p.personal_upload();
        let now = Instant::now();
        for _ in 0..10 {
            p.reserve_class_at(now, 100_000, UploadClass::Collab);
        }
        assert_eq!(
            p.reserve_class_at(now, KIB16, UploadClass::Personal),
            Duration::ZERO
        );
    }

    #[test]
    fn two_personal_uploads_keep_collab_paced_until_both_end() {
        let p = Arc::new(UploadPacer::new(1_000_000));
        let a = p.personal_upload();
        let b = p.personal_upload();
        assert_eq!(p.personal_active(), 2);
        let now = Instant::now();
        p.reserve_class_at(now, 100_000, UploadClass::Collab);
        drop(a);
        // Still one personal upload: collab stays on its paced schedule.
        assert!(p.reserve_class_at(now, 1, UploadClass::Collab) > Duration::ZERO);
        drop(b);
        assert_eq!(p.personal_active(), 0);
    }
}
