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
//! personal upload runs; while one does, they are ALSO paced on their own
//! schedule at [`COLLAB_SHARE_WHILE_PERSONAL`] of the link:
//!
//! - with a cap, the share is `cap × 10 %` and every collab chunk still
//!   reserves on the device bucket (the wait is the longer of the two), so
//!   collab's share comes OUT of the cap — the device total never exceeds it
//!   and personal keeps ~90 %;
//! - without a cap, the share is 10 % of the personal rate observed over
//!   [`PERSONAL_RATE_WINDOW`], never below [`COLLAB_FLOOR_BYTES_PER_SEC`], and
//!   nothing charges a bucket (there is none) — personal is never slowed.
//!
//! When a personal upload starts while none ran, the device bucket's
//! schedule is restarted from the present, so collab chunks reserved before
//! it never delay the personal upload's first chunk.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
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
    /// Whether collab booked slots on the device bucket since personal last
    /// did (or since the last restart). Only then does a new personal upload
    /// restart the schedule — personal-only traffic stays the wave-2 bucket.
    collab_booked: AtomicBool,
    /// Personal chunks of the last [`PERSONAL_RATE_WINDOW`] (recorded only
    /// while no cap is set — the observed rate is used only then).
    personal_window: Mutex<RateWindow>,
}

/// Personal chunks `(at, bytes)` of the last [`PERSONAL_RATE_WINDOW`] and
/// their running byte sum, so reading the rate is O(1) amortized.
#[derive(Default)]
struct RateWindow {
    chunks: VecDeque<(Instant, u64)>,
    bytes: u64,
}

impl RateWindow {
    /// Drop the chunks older than [`PERSONAL_RATE_WINDOW`] before `now`.
    fn evict(&mut self, now: Instant) {
        while let Some(&(t, b)) = self.chunks.front() {
            if now.saturating_duration_since(t) <= PERSONAL_RATE_WINDOW {
                break;
            }
            self.chunks.pop_front();
            self.bytes -= b;
        }
    }
}

impl UploadPacer {
    pub fn new(bytes_per_sec: u64) -> Self {
        Self {
            rate: AtomicU64::new(bytes_per_sec),
            next_free: Mutex::new(None),
            collab_next_free: Mutex::new(None),
            personal_active: AtomicUsize::new(0),
            collab_booked: AtomicBool::new(false),
            personal_window: Mutex::new(RateWindow::default()),
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
                // With a cap the personal path is exactly the wave-2 bucket;
                // the observed rate is recorded only when it is used (no cap).
                if self.rate() == 0 {
                    self.record_personal_at(now, size);
                } else {
                    self.collab_booked.store(false, Ordering::Relaxed);
                }
                self.reserve_at(now, size)
            }
            UploadClass::Collab if self.personal_active.load(Ordering::Relaxed) == 0 => {
                self.note_collab_booking();
                self.reserve_at(now, size)
            }
            UploadClass::Collab => {
                let cap = self.rate();
                let share = if cap > 0 {
                    // With a cap the share is exactly cap × share: the floor
                    // is not applied, so it never lifts collab above its share
                    // of a small cap (`max(1)` only keeps the divisor nonzero).
                    ((cap as f64 * COLLAB_SHARE_WHILE_PERSONAL) as u64).max(1)
                } else {
                    ((self.personal_rate_at(now) as f64 * COLLAB_SHARE_WHILE_PERSONAL) as u64)
                        .max(COLLAB_FLOOR_BYTES_PER_SEC)
                };
                let collab_wait = {
                    let mut next = self.collab_next_free.lock().expect("pacer mutex poisoned");
                    let start = next.map_or(now, |t| t.max(now));
                    *next = Some(start + Duration::from_secs_f64(size as f64 / share as f64));
                    start.saturating_duration_since(now)
                };
                // With a cap, collab bytes are device bytes too: they take
                // their slot on the shared bucket (a no-op without a cap).
                self.note_collab_booking();
                let shared_wait = self.reserve_at(now, size);
                collab_wait.max(shared_wait)
            }
        }
    }

    /// Collab is about to book a slot on the device bucket (only a cap makes
    /// that a real booking).
    fn note_collab_booking(&self) {
        if self.rate() > 0 {
            self.collab_booked.store(true, Ordering::Relaxed);
        }
    }

    /// Record `size` personal bytes let out at `at` (the observed-rate base).
    pub(crate) fn record_personal_at(&self, at: Instant, size: u64) {
        let mut w = self.personal_window.lock().expect("pacer mutex poisoned");
        w.chunks.push_back((at, size));
        w.bytes += size;
        w.evict(at);
    }

    /// Personal bytes/sec over the last [`PERSONAL_RATE_WINDOW`] before `now`.
    fn personal_rate_at(&self, now: Instant) -> u64 {
        let mut w = self.personal_window.lock().expect("pacer mutex poisoned");
        w.evict(now);
        (w.bytes as f64 / PERSONAL_RATE_WINDOW.as_secs_f64()) as u64
    }

    /// Mark a personal upload active for the guard's lifetime (held by the
    /// personal provider consumer for every payload-carrying get). The first
    /// one (0 → 1) restarts the device bucket's schedule from the present
    /// when collab booked slots on it since personal last did, so those collab
    /// chunks never delay its first chunk. Without collab bookings the bucket
    /// is left alone: personal-only traffic is exactly the wave-2 bucket.
    pub fn personal_upload(self: &Arc<Self>) -> PersonalUploadGuard {
        if self.personal_active.fetch_add(1, Ordering::Relaxed) == 0
            && self.collab_booked.swap(false, Ordering::Relaxed)
        {
            match self.next_free.lock() {
                Ok(mut next_free) => *next_free = None,
                Err(e) => {
                    tracing::error!(error = %e, "upload pacer poisoned; device schedule kept")
                }
            }
        }
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
    }

    /// With a cap, both classes running flat out: the device total stays
    /// within the cap and personal keeps ~90 % of it. Simulated in virtual
    /// time — each flow asks for its next chunk the moment the previous one's
    /// reservation lets it out, like the provider's writer does.
    #[test]
    fn with_a_cap_collab_comes_out_of_it_and_personal_keeps_ninety_percent() {
        const CAP: u64 = 1_000_000;
        let p = Arc::new(UploadPacer::new(CAP));
        let _g = p.personal_upload();
        let t0 = Instant::now();
        let horizon = Duration::from_secs(20);
        let (mut personal_at, mut collab_at) = (t0, t0);
        let (mut personal_bytes, mut collab_bytes) = (0u64, 0u64);
        loop {
            let (class, at) = if personal_at <= collab_at {
                (UploadClass::Personal, personal_at)
            } else {
                (UploadClass::Collab, collab_at)
            };
            if at - t0 >= horizon {
                break;
            }
            let sent_at = at + p.reserve_class_at(at, KIB16, class);
            let counted = if sent_at - t0 < horizon { KIB16 } else { 0 };
            match class {
                UploadClass::Personal => {
                    personal_bytes += counted;
                    personal_at = sent_at;
                }
                UploadClass::Collab => {
                    collab_bytes += counted;
                    collab_at = sent_at;
                }
            }
        }
        let budget = CAP * horizon.as_secs();
        let total = personal_bytes + collab_bytes;
        assert!(
            total <= budget + 2 * KIB16,
            "device total {total} over the cap budget {budget}"
        );
        assert!(
            personal_bytes as f64 >= 0.88 * budget as f64,
            "personal got {personal_bytes} of {budget} (collab {collab_bytes})"
        );
        assert!(collab_bytes > 0, "collab still moves");
    }

    /// Personal-only traffic under a cap is exactly the wave-2 bucket: the
    /// class-aware entry point returns what `reserve_at` alone would, and
    /// records nothing.
    #[test]
    fn capped_personal_only_matches_the_plain_bucket() {
        let classed = Arc::new(UploadPacer::new(1_000_000));
        let _g = classed.personal_upload();
        let plain = UploadPacer::new(1_000_000);
        let now = Instant::now();
        for i in 0..64u64 {
            let at = now + Duration::from_millis(i * 3);
            assert_eq!(
                classed.reserve_class_at(at, KIB16, UploadClass::Personal),
                plain.reserve_at(at, KIB16)
            );
        }
        assert!(
            classed.personal_window.lock().unwrap().chunks.is_empty(),
            "nothing recorded under a cap"
        );
    }

    #[test]
    fn a_new_personal_upload_is_not_delayed_by_earlier_collab_reservations() {
        let p = Arc::new(UploadPacer::new(1_000_000));
        let now = Instant::now();
        // Collab alone filled the shared bucket for the next second.
        for _ in 0..61 {
            p.reserve_class_at(now, KIB16, UploadClass::Collab);
        }
        assert!(p.reserve_class_at(now, 1, UploadClass::Collab) > Duration::from_millis(900));
        let _g = p.personal_upload();
        assert_eq!(
            p.reserve_class_at(now, KIB16, UploadClass::Personal),
            Duration::ZERO,
            "the first personal chunk goes out now"
        );
    }

    /// Fix round 2: personal-only traffic — two transfers, the second
    /// starting while the first's schedule is still booked, then a third
    /// after an idle gap — waits exactly what the plain wave-2 bucket does.
    #[test]
    fn personal_only_transfers_are_the_wave_two_bucket_exactly() {
        let classed = Arc::new(UploadPacer::new(1_000_000));
        let plain = UploadPacer::new(1_000_000);
        let t0 = Instant::now();
        for (start_ms, chunks) in [(0u64, 64u64), (100, 32), (10_000, 16)] {
            let _g = classed.personal_upload();
            for i in 0..chunks {
                let at = t0 + Duration::from_millis(start_ms + i);
                assert_eq!(
                    classed.reserve_class_at(at, KIB16, UploadClass::Personal),
                    plain.reserve_at(at, KIB16),
                    "transfer at {start_ms} ms, chunk {i}"
                );
            }
        }
    }

    #[test]
    fn the_observed_rate_is_a_running_sum_over_the_window() {
        let p = UploadPacer::new(0);
        let now = Instant::now();
        p.record_personal_at(now - Duration::from_millis(3000), 1_000_000); // aged out
        p.record_personal_at(now - Duration::from_millis(1000), 2_000_000);
        p.record_personal_at(now, 2_000_000);
        assert_eq!(p.personal_rate_at(now), 2_000_000);
        assert_eq!(p.personal_window.lock().unwrap().chunks.len(), 2);
        // A second and a half later the older in-window chunk has aged out.
        assert_eq!(
            p.personal_rate_at(now + Duration::from_millis(1500)),
            1_000_000
        );
        assert_eq!(p.personal_window.lock().unwrap().bytes, 2_000_000);
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
