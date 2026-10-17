//! Sub-step profile timers (Plan A Task 13b, Step 0a / 13b-P): wall and thread-CPU time per Measure sub-step, reported in
//! the debug lines only. Pool queueing inflates a phase's wall 1.2-5.8x, so thread CPU is the ranking signal. Timers only:
//! no number, `.athf`, `metrics` or `.athm` byte depends on anything here (the plane's [`SubSteps`] rides outside the
//! serialized records).
//!
//! **Thread CPU** is `CLOCK_THREAD_CPUTIME_ID` (Unix; 0 elsewhere). A [`Span`] counts the thread that opened it, so open it
//! INSIDE the pool's `install` closure; a step that fans out also opens a [`SubSteps::leaf`] in each task, which adds the
//! task's CPU only when it runs on another thread than the span's owner (the owner's own tasks are in its span already).
//! Under `RAYON_NUM_THREADS=1` the span alone is the step's whole CPU.

use std::cell::Cell;
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Once;
use std::time::{Duration, Instant};

/// One timed sub-step of a plane's measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(usize)]
pub enum Step {
    /// The working-unit copies (read -> working units): `prepare_plane`, `plane_beta` and `finish_plane` each make one.
    WorkCopy,
    /// `domain::sky_level` on the native plane.
    SkyLevel,
    /// `saturation_plane` (its own time).
    SatPlane,
    /// `saturation_plane`'s queue wait: submit to the pool until its closure starts (wall only).
    SatWait,
    /// The background mesh build (`domain::background_mesh`: cell statistics and the map evaluation; the two live in one
    /// call of the image-analysis submodule and are not split here).
    Mesh,
    /// `N*`/`M*` from the mesh (`background_residual_from`).
    Residual,
    /// MRS noise (`noise_mrs`: wavelet layers and its serial loops, one call of the submodule).
    Mrs,
    /// Detection before pass 1: present mask, background subtraction, the noise-correlation read, the map median.
    DetectPrep,
    /// Detection pass 1 (3 px kernel).
    DetectPass1,
    /// The field width: sort and merge of pass 1, `readable_widths`, the envelope.
    DetectWidths,
    /// Detection pass 2 (at the field width; the 2x2 super-pixel build included).
    DetectPass2,
    /// The knot tests and the cap.
    DetectKnots,
    /// `FrameAggregates::of`'s signal totals (the RCR inside included).
    SignalTotals,
    /// `robust::rcr` alone (a share of `SignalTotals`).
    Rcr,
    /// `ring_skies`.
    RingSky,
    /// The aperture correction `c`.
    ApertureC,
    /// `snr_and_hfr`.
    SnrHfr,
}

/// Number of [`Step`]s.
pub const N_STEPS: usize = Step::SnrHfr as usize + 1;
const _: () = assert!(
    N_STEPS == 17,
    "N_STEPS must follow the Step enum (SnrHfr is its last variant)"
);

thread_local! {
    /// Set by `FrameAggregates` around the signal totals while a plane's timers are on, so `robust::rcr` reads the
    /// clocks only there (LN's and the tests' calls never do).
    static RCR_ON: Cell<bool> = const { Cell::new(false) };
    static TOKEN: u8 = const { 0 };
    static RCR: Cell<(u64, u64)> = const { Cell::new((0, 0)) };
}

/// A nonzero id of the calling thread (the address of a thread-local).
fn thread_token() -> usize {
    TOKEN.with(|t| t as *const u8 as usize)
}

/// The calling thread's CPU time in ns (0 where the platform has no per-thread clock).
#[cfg(unix)]
pub fn thread_cpu_ns() -> u64 {
    let mut ts = libc::timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    // SAFETY: `ts` is a valid out-pointer; the clock id is a constant the platform defines.
    let rc = unsafe { libc::clock_gettime(libc::CLOCK_THREAD_CPUTIME_ID, &mut ts) };
    if rc != 0 {
        static WARN: Once = Once::new();
        WARN.call_once(|| {
            tracing::warn!(
                error = %std::io::Error::last_os_error(),
                "the thread CPU clock failed; sub-step CPU timings read 0 from here on"
            );
        });
        return 0;
    }
    (ts.tv_sec as u64) * 1_000_000_000 + ts.tv_nsec as u64
}

#[cfg(not(unix))]
pub fn thread_cpu_ns() -> u64 {
    0
}

/// Wall and thread-CPU time per [`Step`], summed over every opened span; atomics, so a borrowed plane record and the
/// tasks of a fan-out all record through `&self`. `PartialEq` is always true and `Clone` copies the sums: timings never
/// take part in a comparison of measurements.
pub struct SubSteps {
    /// Whether the timers run at all: `tracing::enabled!(DEBUG)` when the plane's record was made (the timings are only
    /// ever reported on debug lines). Off, every span, leaf and `add_*` is inert and no clock is read.
    on: bool,
    wall_ns: [AtomicU64; N_STEPS],
    cpu_ns: [AtomicU64; N_STEPS],
    owner: [AtomicUsize; N_STEPS],
}

impl Default for SubSteps {
    fn default() -> Self {
        Self::with_timers(tracing::enabled!(
            target: "athenaeum_core::stacking::measure",
            tracing::Level::DEBUG
        ))
    }
}

impl Clone for SubSteps {
    fn clone(&self) -> Self {
        let c = SubSteps::with_timers(self.on);
        for i in 0..N_STEPS {
            c.wall_ns[i].store(self.wall_ns[i].load(Ordering::Relaxed), Ordering::Relaxed);
            c.cpu_ns[i].store(self.cpu_ns[i].load(Ordering::Relaxed), Ordering::Relaxed);
        }
        c
    }
}

/// Timings never print either: a measurement's `Debug` text stands in for its identity in the bit-for-bit pins.
impl std::fmt::Debug for SubSteps {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SubSteps")
    }
}

impl PartialEq for SubSteps {
    fn eq(&self, _: &Self) -> bool {
        true
    }
}

impl SubSteps {
    /// A record whose timers run (`true`) or are inert (`false`); [`Default`] picks by the debug level.
    pub fn with_timers(on: bool) -> Self {
        SubSteps {
            on,
            wall_ns: Default::default(),
            cpu_ns: Default::default(),
            owner: Default::default(),
        }
    }

    /// Whether the timers run.
    pub fn is_on(&self) -> bool {
        self.on
    }

    /// Opens a span: wall and the calling thread's CPU, added to `step` when the guard drops.
    pub fn span(&self, step: Step) -> Span<'_> {
        if !self.on {
            return Span::inert(self, step);
        }
        self.owner[step as usize].store(thread_token(), Ordering::Relaxed);
        Span {
            sub: self,
            step,
            wall: Some(Instant::now()),
            cpu0: thread_cpu_ns(),
            on: true,
        }
    }

    /// Opens a span that records CPU only (the wall is taken elsewhere, or the step is a leaf of a wall-timed one).
    pub fn cpu_span(&self, step: Step) -> Span<'_> {
        if !self.on {
            return Span::inert(self, step);
        }
        self.owner[step as usize].store(thread_token(), Ordering::Relaxed);
        Span {
            sub: self,
            step,
            wall: None,
            cpu0: thread_cpu_ns(),
            on: true,
        }
    }

    /// A fan-out task of `step`: its CPU is added when the task runs on a thread other than the span owner's.
    pub fn leaf(&self, step: Step) -> Leaf<'_> {
        if !self.on {
            return Leaf {
                sub: self,
                step,
                cpu0: None,
            };
        }
        let owner = self.owner[step as usize].load(Ordering::Relaxed);
        let other = owner != 0 && owner != thread_token();
        Leaf {
            sub: self,
            step,
            cpu0: if other { Some(thread_cpu_ns()) } else { None },
        }
    }

    /// Adds a wall duration to `step`.
    pub fn add_wall(&self, step: Step, d: Duration) {
        if !self.on {
            return;
        }
        self.wall_ns[step as usize].fetch_add(d.as_nanos() as u64, Ordering::Relaxed);
    }

    /// Adds `ns` of thread CPU to `step`.
    pub fn add_cpu_ns(&self, step: Step, ns: u64) {
        if !self.on {
            return;
        }
        self.cpu_ns[step as usize].fetch_add(ns, Ordering::Relaxed);
    }

    /// Wall in whole microseconds.
    pub fn wall_us(&self, step: Step) -> u64 {
        self.wall_ns[step as usize].load(Ordering::Relaxed) / 1_000
    }

    /// Thread CPU in whole microseconds.
    pub fn cpu_us(&self, step: Step) -> u64 {
        self.cpu_ns[step as usize].load(Ordering::Relaxed) / 1_000
    }
}

/// See [`SubSteps::span`].
pub struct Span<'a> {
    sub: &'a SubSteps,
    step: Step,
    wall: Option<Instant>,
    cpu0: u64,
    on: bool,
}

impl<'a> Span<'a> {
    fn inert(sub: &'a SubSteps, step: Step) -> Self {
        Span {
            sub,
            step,
            wall: None,
            cpu0: 0,
            on: false,
        }
    }
}

impl Drop for Span<'_> {
    fn drop(&mut self) {
        if !self.on {
            return;
        }
        if let Some(t) = self.wall {
            self.sub.add_wall(self.step, t.elapsed());
        }
        self.sub
            .add_cpu_ns(self.step, thread_cpu_ns().saturating_sub(self.cpu0));
    }
}

/// See [`SubSteps::leaf`].
pub struct Leaf<'a> {
    sub: &'a SubSteps,
    step: Step,
    cpu0: Option<u64>,
}

impl Drop for Leaf<'_> {
    fn drop(&mut self) {
        if let Some(c0) = self.cpu0 {
            self.sub
                .add_cpu_ns(self.step, thread_cpu_ns().saturating_sub(c0));
        }
    }
}

/// An optional sink's span: nothing when there is no sink.
pub fn span<'a>(sub: Option<&'a SubSteps>, step: Step) -> Option<Span<'a>> {
    sub.filter(|s| s.on).map(|s| s.span(step))
}

/// An optional sink's leaf.
pub fn leaf<'a>(sub: Option<&'a SubSteps>, step: Step) -> Option<Leaf<'a>> {
    sub.filter(|s| s.on).map(|s| s.leaf(step))
}

/// Accumulates one `robust::rcr` call into the calling thread's running sum (wall ns, CPU ns); [`take_rcr`] reads and
/// resets it. `rcr` runs serially on the thread that calls `signal_totals`, so a thread-local needs no plumbing.
pub(crate) struct RcrTimer(Option<(Instant, u64)>);

impl RcrTimer {
    /// Reads the clocks only inside [`rcr_timing`]'s window (the plane's timers on); inert otherwise.
    pub(crate) fn start() -> Self {
        RcrTimer(
            RCR_ON
                .with(Cell::get)
                .then(|| (Instant::now(), thread_cpu_ns())),
        )
    }
}

/// Runs `f` with `rcr` timing on or off for the calling thread (restoring the previous state); returns `f`'s value.
pub(crate) fn rcr_timing<R>(on: bool, f: impl FnOnce() -> R) -> R {
    let prev = RCR_ON.with(|c| c.replace(on));
    let out = f();
    RCR_ON.with(|c| c.set(prev));
    out
}

impl Drop for RcrTimer {
    fn drop(&mut self) {
        let Some((t, c0)) = self.0 else { return };
        let (w, c) = (
            t.elapsed().as_nanos() as u64,
            thread_cpu_ns().saturating_sub(c0),
        );
        RCR.with(|r| {
            let (a, b) = r.get();
            r.set((a + w, b + c));
        });
    }
}

/// The calling thread's accumulated `(wall ns, cpu ns)` of `rcr` since the last call; resets it.
pub(crate) fn take_rcr() -> (u64, u64) {
    RCR.with(|r| r.replace((0, 0)))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A span counts its own thread; a leaf counts only on another thread than the span's owner.
    #[test]
    fn a_leaf_counts_cpu_only_off_the_owner_thread() {
        fn burn() {
            let mut x = 0u64;
            for i in 0..30_000_000u64 {
                x = x.wrapping_add(i ^ (x >> 3));
            }
            std::hint::black_box(x);
        }
        let sub = SubSteps::with_timers(true);
        {
            let _span = sub.span(Step::RingSky);
            let _own = sub.leaf(Step::RingSky);
            burn();
            std::thread::scope(|s| {
                s.spawn(|| {
                    let _other = sub.leaf(Step::RingSky);
                    burn();
                });
            });
        }
        assert!(sub.wall_us(Step::RingSky) > 0);
        #[cfg(unix)]
        {
            // The owner's burn (span) plus the other thread's burn (leaf): about twice one burn, never the owner's leaf.
            let one = {
                let t = thread_cpu_ns();
                burn();
                (thread_cpu_ns() - t) / 1_000
            };
            // Roughly two burns; P/E cores and scheduling make the ratio wide, so the bound is only "clearly more than
            // the owner's single burn" and "not absurdly more" (the owner's leaf would add a third).
            let got = sub.cpu_us(Step::RingSky);
            assert!(
                got > one * 6 / 5 && got < one * 8,
                "{got} us vs one burn {one} us"
            );
        }
        assert_eq!(sub.wall_us(Step::Mesh), 0);
        assert_eq!(sub.clone(), sub);
    }

    /// Timers off: no span, leaf or add records anything and the rcr timer is inert.
    #[test]
    fn inert_timers_record_nothing() {
        let sub = SubSteps::with_timers(false);
        {
            let _s = sub.span(Step::Mesh);
            let _l = sub.leaf(Step::Mesh);
            let _c = sub.cpu_span(Step::Mesh);
            sub.add_wall(Step::Mesh, Duration::from_millis(5));
            sub.add_cpu_ns(Step::Mesh, 5_000_000);
            assert!(
                span(Some(&sub), Step::Mesh).is_none() && leaf(Some(&sub), Step::Mesh).is_none()
            );
        }
        assert_eq!((sub.wall_us(Step::Mesh), sub.cpu_us(Step::Mesh)), (0, 0));
        rcr_timing(false, || {
            let _t = RcrTimer::start();
        });
        assert_eq!(take_rcr(), (0, 0));
        rcr_timing(true, || {
            let _t = RcrTimer::start();
        });
        assert!(take_rcr().0 > 0 || cfg!(not(unix)));
    }
}
