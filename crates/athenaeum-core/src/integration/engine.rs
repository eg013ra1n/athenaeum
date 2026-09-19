//! Recipe orchestration (spec §4, §9): banded streaming + per-pixel combine.
//! Memory: N × band, sized per build by `integration::band_budget` from the
//! machine and the compute-queue ceiling (see that module) rather than a
//! compile-time constant. Parallelism: rayon over the pixels of the current
//! band via the shared image pool.

use super::banded::{BandPlanes, BandSource};
use super::combine::{self, combine_pixel, IntegrationRecipe};
use super::io_policy::IoPolicy;
use super::registered_source::RegisteredSource;
use super::source::{FrameSource, RejectionBitSink, RejectionBitSource};
use super::stats::NormalizationPair;
use super::storage_class::StorageClass;
use super::IntegrationError;
use std::cell::RefCell;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

pub struct IntegrationOutput {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
    pub rejected_fraction: f64,
    pub flat_norm: Option<f64>,
    /// How many samples of frame i were dropped as non-finite (NaN/±Inf after
    /// pre-calibration and scaling). Indexed by the caller's `paths` order —
    /// same order `BandSource::open` was given. Never a build failure: FITS
    /// calls these "undefined pixels", so they are excluded from the stack the
    /// way an out-of-range rejection is, and reported.
    pub bad_samples_per_frame: Vec<usize>,
    /// Output pixels whose ENTIRE stack was non-finite — nothing left to
    /// combine, so 0.0 was written.
    pub all_bad_pixels: usize,
    /// Wall time spent inside `BandSource::read_band` across every band,
    /// including a flat's pass-1 reads. Separated from `combine_duration`
    /// because the two phases have completely different bottlenecks and only
    /// the engine can tell them apart. Since perf tier 1 Task 1, `band_loop`
    /// prefetches band N+1's read while band N combines, so this and
    /// `combine_duration` overlap in wall-clock time — `read_duration +
    /// combine_duration` may exceed the run's own elapsed time. Pass 1's
    /// central-third read (flats only) stays sequential and is unaffected.
    pub read_duration: std::time::Duration,
    /// Wall time spent in the parallel per-pixel combine across every band.
    /// See `read_duration`'s doc for the overlap this now has with it.
    pub combine_duration: std::time::Duration,
    /// Rows per band and how many bands the run used — the two numbers the
    /// band budget actually decides, reported so the build's log line does not
    /// have to re-derive them. For a FLAT these describe pass 2 (the full
    /// combine) only — pass 1's central-third read has no band count of its
    /// own — whereas `read_duration`/`bytes_read` above span BOTH passes. See
    /// [`EngineProgress::on_band`] for what that asymmetry does to a flat's
    /// live progress numbers.
    pub band_rows: usize,
    pub bands: usize,
    /// Pixel bytes actually read (sum of `rows * width * bytes_per_sample`
    /// over every band and frame). Not the files' size on disk: headers and
    /// padding are never read, and a flat's pass 1 reads only its central
    /// third.
    pub bytes_read: u64,
    /// Perf tier A Task 0 (audit §3.3, item I11): CPU time (thread-seconds,
    /// summed — not wall time) spent inside per-pixel combine work, one
    /// `Instant` delta per rayon leaf of `integrate_stack`'s row-processing
    /// `for_each_init` (each leaf's own `RowState::Drop`, see
    /// `engine.rs`'s `LeafCpuTimer`), added into a shared atomic. On a
    /// fully parallel run this approaches `combine_duration × pool
    /// threads`; it can never exceed that bound. `Duration::ZERO` for
    /// `run_banded`'s plain average/median combine path, which has no
    /// per-leaf timer of its own — that path is the simple, un-rejected
    /// master-build combine `combine_duration` already accounts for well
    /// enough on its own.
    pub combine_cpu_duration: std::time::Duration,
    /// `reject_linear_fit`'s outer convergence-loop pass count, summed over
    /// every pixel stack this call combined (perf tier A Task 0, audit
    /// §3.3). Zero unless `Rejection::LinearFitClip` was the active
    /// algorithm for at least one pixel. Divide by `width * height` for the
    /// per-plane mean the "plane integrated" log line reports
    /// (`rejection_iters_mean`).
    pub rejection_iters_total: u64,
    /// `medfit_line`'s own `rofunc` bracket/bisection evaluation count,
    /// same sum-over-every-pixel-stack contract as
    /// [`Self::rejection_iters_total`] (`medfit_evals_mean` on the same log
    /// line).
    pub medfit_evals_total: u64,
}

pub struct EngineProgress<'a> {
    /// `(band_index_1based, bands_total, bytes_read_so_far, bytes_total)`.
    ///
    /// For a FLAT, `band_index_1based`/`bands_total` count pass 2 (the full
    /// combine) only, while `bytes_read_so_far`/`bytes_total` span BOTH
    /// passes — `integrate_flat_inner` wraps the pass-2-only callback
    /// `run_banded` calls so the byte pair carries pass 1's central-third
    /// read as a baseline (pass 1 has no band count of its own to report).
    /// The two numbers on one call can therefore disagree sharply: at the
    /// moment `band_index_1based` first becomes 1 (pass 2's first band
    /// finishing), it reads as 1/N (~2%) while bytes_done/bytes_total are
    /// already at ~25% of the two-pass total — a real ~23-point gap between
    /// the two progress numbers on the SAME event, not a bug. A bias-like
    /// build (one pass) never sees this — its two numbers always agree.
    ///
    /// Fires MORE than once per band (fix round 2, I2): in addition to the
    /// existing call at band END, `BandSource::read_band_with_progress`
    /// calls this once per FRAME as its read completes, with `band_index_1based`/
    /// `bands_total` held at the band currently in flight and
    /// `bytes_read_so_far` climbing within it — Task 6 cut `bands_total` to
    /// as few as 2, so waiting for a whole band to end can mean minutes of
    /// silence otherwise. For a FLAT this callback also fires DURING pass 1
    /// (`integrate_flat_inner`'s own read loop, before pass 2 has started at
    /// all) with `band_index_1based` pinned at `0` — "no pass-2 band
    /// reached yet" — against the same two-pass `bytes_total` used
    /// everywhere else, so pass 1 is no longer a silent 25% of the run: a
    /// caller only needs `bytes_read_so_far`/`bytes_total` to see it move.
    pub on_band: &'a (dyn Fn(usize, usize, u64, u64) + Sync),

    /// Combine-phase tick (fix wave item 2, whole-branch review). `on_band`
    /// above has nothing left to say once a band's bytes are all in — its
    /// last call for a band already sits at that band's ceiling — but the
    /// parallel per-pixel combine that follows can then run for seconds
    /// (single digits on the profiling machine at 100 frames; scales as
    /// frames x pixels / cores) with no read events left to hang a percent
    /// off. Task 6 can resolve a build to exactly ONE band (routine at
    /// >=32 GB visible RAM), which turns that silence into the WHOLE
    /// combine: a progress indicator frozen at 100% reads as "finished and
    /// stuck", worse than one frozen partway.
    ///
    /// Fired from `band_loop`'s per-row tick, on both the master and the
    /// stacking path, via an `AtomicUsize` row counter, periodically (a row
    /// stride, not every row — see the counter's call site) rather than on
    /// every row, as `(rows_combined_so_far, total_rows, bytes_done, bytes_total)`:
    ///
    /// - `rows_combined_so_far`/`total_rows` are GLOBAL across the whole
    ///   run — every band's rows feed the same counter, in whatever order
    ///   rayon happens to finish them, not restarted per band — so a
    ///   single-band run still climbs smoothly through 0-100% instead of
    ///   jumping straight from the read's 100% to "writing" with nothing
    ///   between. This is a DIFFERENT meaning for `current`/`total` than
    ///   `on_band` gives them (band index / band count there, rows here);
    ///   that mismatch is exactly why this is a separate callback rather
    ///   than an overloaded call to `on_band` — one field cannot honestly
    ///   carry two incompatible meanings of its own parameters.
    /// - `bytes_done`/`bytes_total` are NOT a new measurement — they are
    ///   the exact `on_band` pair most recently established for this point
    ///   in the run (the band currently combining has already finished
    ///   reading), held constant for as long as this band's combine runs.
    ///   Combine reads nothing, so there is nothing honest to add to
    ///   "bytes of source read" here; they ride along only so a caller
    ///   building one event shape out of both callbacks has a byte pair to
    ///   put in it, not because either number moves during this callback's
    ///   lifetime. Trivially monotonic as a result — every combine tick of
    ///   a given band repeats the same values.
    pub on_combine: &'a (dyn Fn(usize, usize, u64, u64) + Sync),
}

pub enum FlatPrecal {
    MasterFrame { data: Vec<f32>, width: usize, height: usize },
    SyntheticBias(f32),
    None,
}

pub fn central_third_mean(data: &[f32], width: usize, height: usize) -> f64 {
    let (x0, x1) = (width / 3, (2 * width) / 3);
    let (y0, y1) = (height / 3, (2 * height) / 3);
    let mut sum = 0.0f64;
    let mut n = 0usize;
    for y in y0..y1.max(y0 + 1).min(height) {
        for x in x0..x1.max(x0 + 1).min(width) {
            sum += data[y * width + x] as f64;
            n += 1;
        }
    }
    if n == 0 { 0.0 } else { sum / n as f64 }
}

/// What a band-combine closure receives: the decoded band, the output rows
/// it must fill, the band's first global row, and the progress hook it must
/// call once per finished row, which reports `(rows_done_total, total_rows,
/// bytes_read, bytes_total)` to `on_combine`.
struct BandJob<'a> {
    planes: &'a BandPlanes,
    out_band: &'a mut [f32],
    y0: usize,
    rows: usize,
    width: usize,
}

struct BandStats {
    /// Wall time spent in band reads. Since perf tier 1's prefetch (the next
    /// band reads while the current one combines), this overlaps
    /// `combine_duration` — `read_duration + combine_duration` may exceed
    /// the loop's own elapsed time.
    read_duration: std::time::Duration,
    combine_duration: std::time::Duration,
    band_rows: usize,
    bands: usize,
    bytes_read: u64,
}

/// The rows per band `band_loop` will actually use for this source/policy.
///
/// Fix wave (whole-branch review, item 1): the halving below — and the
/// prefetch it exists for — is now gated on `StorageClass::Network`. On a
/// local SSD what `band_loop` calls a band "read" is a CPU-bound warp, not
/// an I/O wait (compute audit §1): overlapping it with the combine on the
/// SAME pool bought nothing, and the halved bands cost real margin —
/// smaller bands mean more of them, and each one re-reads the per-frame
/// header/probe overhead. On network storage the read genuinely is
/// latency-bound, so keeping two bands resident (the current one combining,
/// the next one being prefetched) is the point, and halving keeps peak
/// memory at the pre-task figure. Fix round 1, Important finding 1: this
/// used to be computed independently (and un-halved) by the flat's pass-1
/// forecast and the rejection-map scratch cap, so both drifted out of step
/// with what `band_loop` actually does the moment the halving landed — a
/// flat build's "band N of M" doubled M mid-run, and the no-maps scratch
/// pair over-allocated ~2x. Extracted here so the three call sites (this
/// one, the flat forecast, the scratch cap) share one number, gated the
/// same way, and can never drift apart again.
fn loop_band_rows<S: FrameSource + ?Sized>(src: &S, io: IoPolicy) -> usize {
    let budget = if matches!(io.storage, StorageClass::Network) {
        io.band_budget_bytes / 2
    } else {
        io.band_budget_bytes
    };
    src.band_rows_for_budget(budget).max(1).min(src.height())
}

/// The read / progress / cancel / timing skeleton shared by every banded
/// integration. `combine(job, tick)` fills `job.out_band` (rows × width) and
/// calls `tick()` once per finished row. `io.band_budget_bytes` is injectable
/// (module-internal) so tests can force multi-band runs on tiny images;
/// production passes the machine- and storage-resolved policy from
/// `integration::io_policy::resolve`.
#[allow(clippy::too_many_arguments)]
fn band_loop<S: FrameSource + ?Sized>(
    src: &S,
    pool: &rayon::ThreadPool,
    cancel: &AtomicBool,
    progress: &EngineProgress<'_>,
    io: IoPolicy,
    out: &mut [f32],
    combine: &(dyn Fn(BandJob<'_>, &(dyn Fn() + Sync)) -> Result<(), IntegrationError> + Sync),
) -> Result<BandStats, IntegrationError> {
    let (w, h) = (src.width(), src.height());
    // Perf tier 1 Task 1: two band buffers, the next band's read overlapping
    // this band's combine — via `loop_band_rows`, shared with the flat
    // pass-1 forecast and the rejection-map scratch cap (fix round 1,
    // Important finding 1) so the three can never drift apart again.
    let band_rows = loop_band_rows(src, io);
    let bands_total = h.div_ceil(band_rows);
    // Computed once, next to `band_rows`, and referenced from every band's
    // progress call below — this run's own (pass 2) share of the work. See
    // `EngineProgress::on_band`'s doc for why a flat's caller sees a bigger
    // total than this once `integrate_flat_inner` wraps it with pass 1.
    let per_row_bytes = src.bytes_per_row();
    let bytes_total = (h * per_row_bytes) as u64;
    // Fix wave item 1: whether this run overlaps the next band's read with
    // the current band's combine at all — see `loop_band_rows`'s doc for
    // the reasoning. When false, the loop below runs the pre-tier-1 shape:
    // read band N, then combine band N, with `has_next` forced false so the
    // scoped thread never spawns and `next` is never read. The
    // double-buffer machinery (`cur`/`next`, the swap at the loop's end)
    // stays in place either way so both shapes share one implementation.
    let prefetch = matches!(io.storage, StorageClass::Network);
    let mut cur = BandPlanes::new(src);
    let mut next = BandPlanes::new(src);
    let read_duration = std::sync::Mutex::new(std::time::Duration::ZERO);
    let mut combine_duration = std::time::Duration::ZERO;
    // Bytes the READER has reported so far — the live high-water mark
    // `on_band` quotes directly. `on_combine` does NOT read this live: it
    // quotes `bytes_at_band_start`, a snapshot of this same mutex taken
    // once at the start of each band's combine (below) — so a combine tick
    // emitted while the next band is being prefetched never reports fewer
    // bytes than the reader already had at that band's start (the "bytes
    // never regress" contract, now across two threads), without the ticks
    // themselves re-locking this mutex on every call.
    //
    // Fix round 3, Important 1 (this reasoning now spans the reader thread
    // AND the main/combine thread, not just concurrent readers within one
    // band): an `AtomicU64::fetch_max` high-water-mark guard here is NOT
    // enough on its own — two workers can both pass the "am I a new
    // maximum" check (each correctly, against the state at the moment they
    // checked) and then still race each other into the actual callback
    // afterward, since updating the atomic and invoking the callback are
    // two separate, unsynchronized steps — the exact TOCTOU shape a
    // fetch_max-then-emit pattern was trying to close, just moved one line
    // later. A `Mutex` makes "add my bytes, then emit" ONE critical
    // section: whichever thread holds the lock is the only one that can
    // advance `bytes_reported` and call the progress callback, so
    // emissions are ordered by lock-acquisition order, which is the same
    // order the bytes were added in — monotonic by construction, not by
    // discarding stale values after the fact.
    let bytes_reported = std::sync::Mutex::new(0u64);
    let rows_combined = AtomicUsize::new(0);
    // A stride, not every row: `on_combine` is wall-clock-throttled by the
    // caller (`masters.rs`, same as `on_band`) behind a `Mutex`, so calling
    // it on literally every row of a multi-thousand-row image would still
    // mean thousands of lock acquisitions this loop has no reason to pay
    // for. `done == h` below always fires regardless of the stride, so the
    // final tick is never skipped.
    const COMBINE_TICK_ROWS: usize = 64;

    // One band's read, callable from either thread (the caller of
    // `band_loop` itself for the very first band, or a scoped prefetch
    // thread for every band after it). Returns the accounted bytes of the
    // band it read.
    let read_one = |band_idx: usize, y0: usize, rows: usize, into: &mut BandPlanes| -> Result<u64, IntegrationError> {
        let t = std::time::Instant::now();
        let on_bytes = |just_read: u64| {
            let mut so_far = bytes_reported.lock().unwrap();
            *so_far += just_read;
            (progress.on_band)(band_idx + 1, bands_total, *so_far, bytes_total);
        };
        src.read_band_with_progress(y0, rows, into, io.read_concurrency, &on_bytes, cancel)?;
        *read_duration.lock().unwrap() += t.elapsed();
        Ok((rows * per_row_bytes) as u64)
    };

    let mut bytes_read: u64 = 0;
    if cancel.load(Ordering::Relaxed) {
        return Err(IntegrationError::Cancelled);
    }
    if prefetch {
        bytes_read += read_one(0, 0, band_rows.min(h), &mut cur)?;
    }

    for (band_idx, y0) in (0..h).step_by(band_rows).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(IntegrationError::Cancelled);
        }
        let rows = band_rows.min(h - y0);
        let next_y0 = y0 + rows;
        let has_next = prefetch && next_y0 < h;
        let next_rows = band_rows.min(h.saturating_sub(next_y0));

        if !prefetch {
            // No overlap on this run (see `loop_band_rows`'s doc): this IS
            // band N's own read, done synchronously before its combine
            // below — the pre-tier-1 shape.
            bytes_read += read_one(band_idx, y0, rows, &mut cur)?;
            if cancel.load(Ordering::Relaxed) {
                return Err(IntegrationError::Cancelled);
            }
        }

        // Fix wave item 1 (whole-branch review, CRITICAL — still holds):
        // a cancel raised while a read was in flight must not fall through
        // into the combine below; both checks that used to guard this are
        // now the ones just above (before this band's own read, already
        // done by the time this iteration starts) and the one right after
        // `combine_result?` below.
        //
        // `y0` (the band's first global row) rides in `BandJob` for callers
        // that need the global row index (e.g. the precal MasterFrame
        // path's `gy = y0 + row_in_band`).
        let combine_result: Result<(), IntegrationError>;
        let prefetch_result: Option<Result<u64, IntegrationError>>;
        {
            let next_ref = &mut next;
            let read_one = &read_one;
            // Fix round 1, Important finding 2: snapshot the high-water mark
            // ONCE, before the prefetch thread (if any) starts touching it
            // below, so every `tick` call during this band's combine quotes
            // the SAME value — the bytes through and including the band
            // about to be combined, never a value the next band's prefetch
            // reader might advance mid-combine. The pre-fix version had
            // `tick` lock-read-unlock `bytes_reported` and emit OUTSIDE the
            // lock on every call: two rayon workers ticking concurrently
            // could have the prefetch reader advance the counter between
            // their two reads, so the worker that read the SMALLER value
            // could still win the race to call `on_combine` FIRST — a
            // regression pinned by
            // `on_combine_bytes_done_never_regresses_under_real_concurrency`.
            // This snapshot needs zero lock traffic per tick and is
            // monotone across bands by construction: the NEXT band's
            // snapshot is taken only after THIS band's prefetch (if any)
            // has already been folded into `bytes_reported` below. When
            // `prefetch` is false this is simply the bytes through this
            // band's own read, just completed above.
            let bytes_at_band_start = *bytes_reported.lock().unwrap();
            let (c, p) = std::thread::scope(|scope| {
                let prefetch_handle = has_next.then(|| {
                    scope.spawn(move || read_one(band_idx + 1, next_y0, next_rows, next_ref))
                });
                let t_combine = std::time::Instant::now();
                let out_band = &mut out[y0 * w..(y0 + rows) * w];
                let tick = || {
                    // Fix wave item 2: one relaxed increment per ROW —
                    // `combine` calls this once per row, matching the
                    // review's "one relaxed increment and nothing else"
                    // requirement. `done` can arrive slightly out of order
                    // under concurrency — harmless here because the byte
                    // pair (`bytes_at_band_start`) is a plain captured copy,
                    // frozen for this whole band's combine, so the one hard
                    // monotonicity requirement (bytes, not rows) holds by
                    // construction, not by racing a lock.
                    let done = rows_combined.fetch_add(1, Ordering::Relaxed) + 1;
                    if done % COMBINE_TICK_ROWS == 0 || done == h {
                        (progress.on_combine)(done, h, bytes_at_band_start, bytes_total);
                    }
                };
                let c = pool.install(|| combine(BandJob { planes: &cur, out_band, y0, rows, width: w }, &tick));
                combine_duration += t_combine.elapsed();
                let p = prefetch_handle.map(|j| j.join().expect("band prefetch thread panicked"));
                (c, p)
            });
            combine_result = c;
            prefetch_result = p;
        }
        // Fix wave item 2: a prefetch that failed while the combine ALSO
        // failed must not be swallowed — the combine's own error is still
        // what the caller sees (this band was never going to make it into
        // the output either way), but a real I/O error on the NEXT band's
        // read is information worth keeping, not silently dropped the
        // moment `prefetch_result` stops being read below.
        if let (Err(_), Some(Err(e))) = (&combine_result, &prefetch_result) {
            tracing::warn!(
                error = %e,
                band = band_idx + 2,
                "band prefetch failed while the combine was already failing or cancelled"
            );
        }
        combine_result?;
        // Fix wave item 1: same reasoning as before — the combine is the
        // actually slow phase once a band is a meaningful fraction of the
        // image, and on a single-band run there is no future loop
        // iteration to catch a cancel raised during it.
        if cancel.load(Ordering::Relaxed) {
            // Fix wave item 2: same reasoning as just above — a cancel won
            // the race, but a prefetch failure underneath it is still worth
            // a log line rather than silent disposal.
            if let Some(Err(e)) = &prefetch_result {
                tracing::warn!(
                    error = %e,
                    band = band_idx + 2,
                    "band prefetch failed while the combine was already failing or cancelled"
                );
            }
            return Err(IntegrationError::Cancelled);
        }
        // Perf tier 1 Task 1 fix: this end-of-band call must read the SAME
        // `bytes_reported` high-water mark the in-flight prefetch's own
        // ticks just wrote (already joined above, since `thread::scope`
        // waits for it) — not the plain `bytes_read` accumulator, which is
        // still one band behind at this point (it only gains the
        // just-finished prefetch's bytes in the `if let Some(r) =
        // prefetch_result` step below). Using the stale accumulator here
        // regressed a real caller: a prefetch tick for band N+1 (reporting
        // through `bytes_reported`, already including N+1's bytes) fires
        // DURING band N's combine, chronologically before this "band N is
        // done" call — which then reported FEWER bytes than the tick that
        // just preceded it. Caught by
        // `flat_progress_ticks_span_both_passes_with_one_shared_total`
        // under `io(1)` (1-row bands, so almost every call is this
        // interleaving).
        let bytes_read_so_far = *bytes_reported.lock().unwrap();
        (progress.on_band)(band_idx + 1, bands_total, bytes_read_so_far, bytes_total);
        if let Some(r) = prefetch_result {
            bytes_read += r?;
            std::mem::swap(&mut cur, &mut next);
        }
    }

    Ok(BandStats {
        read_duration: read_duration.into_inner().unwrap(),
        combine_duration,
        band_rows,
        bands: bands_total,
        bytes_read,
    })
}

/// Shared banded-combine core (the unweighted master path). `scale[i]`/`precal`
/// transform frame i's samples before combining: v' = (v - precal(i, pixel)) * scale[i].
#[allow(clippy::too_many_arguments)]
fn run_banded<S: FrameSource + ?Sized>(
    src: &S,
    scales: &[f32],
    precal: Option<&FlatPrecal>,
    recipe: IntegrationRecipe,
    pool: &rayon::ThreadPool,
    cancel: &AtomicBool,
    progress: &EngineProgress<'_>,
    io: IoPolicy,
) -> Result<IntegrationOutput, IntegrationError> {
    use rayon::prelude::*;
    let (w, h, n) = (src.width(), src.height(), src.frame_count());
    let mut out = vec![0f32; w * h];
    let rejected = AtomicUsize::new(0);
    // Non-finite accounting (audit C2). Plain counters shared across the rayon
    // rows, so Relaxed is enough — nothing else is published through them, and
    // the reads below happen after every worker has joined.
    let bad_samples: Vec<AtomicUsize> = (0..n).map(|_| AtomicUsize::new(0)).collect();
    let all_bad = AtomicUsize::new(0);
    // Perf tier A Task 0: `combine_pixel` runs the same `apply_rejection`
    // dispatch `combine_pixel_weighted` does, so a plain master build under
    // `Rejection::LinearFitClip` gets real counts too — no per-leaf CPU
    // timer on this simpler (`for_each`, no `RowState`) path, so
    // `combine_cpu_duration` stays `Duration::ZERO` below.
    let rejection_iters_total = AtomicU64::new(0);
    let medfit_evals_total = AtomicU64::new(0);

    let stats = band_loop(src, pool, cancel, progress, io, &mut out, &|job, tick| {
        let BandJob { planes, out_band, y0, width, .. } = job;
        out_band
            .par_chunks_mut(width)                       // one row per work item
            .enumerate()
            .for_each(|(row_in_band, out_row)| {
                let mut column: Vec<f32> = Vec::with_capacity(n);
                for (x, out_px) in out_row.iter_mut().enumerate() {
                    column.clear();
                    let idx = row_in_band * width + x;
                    for i in 0..n {
                        let mut v = planes.sample(i, idx);
                        if let Some(p) = precal {
                            match p {
                                FlatPrecal::MasterFrame { data, width, .. } => {
                                    let gy = y0 + row_in_band;
                                    v -= data[gy * *width + x];
                                }
                                FlatPrecal::SyntheticBias(b) => v -= *b,
                                FlatPrecal::None => {}
                            }
                        }
                        v *= scales[i];
                        if !v.is_finite() {
                            // FITS: NaN in float data means "undefined
                            // pixel" — excluded from the stack (with
                            // accounting), exactly like an out-of-range
                            // rejection. Passing it on would panic the
                            // winsorized estimator (`f64::clamp` with NaN
                            // bounds) or bake NaN into the master.
                            bad_samples[i].fetch_add(1, Ordering::Relaxed);
                            continue;
                        }
                        column.push(v);
                    }
                    if column.is_empty() {
                        *out_px = 0.0;
                        all_bad.fetch_add(1, Ordering::Relaxed);
                    } else {
                        let (val, rej) = combine_pixel(&mut column, recipe);
                        *out_px = val;
                        if rej > 0 { rejected.fetch_add(rej, Ordering::Relaxed); }
                        // Perf tier A Task 0: same per-pixel-stack flush as
                        // `integrate_stack`'s weighted path.
                        let (iters, evals) = combine::take_rejection_counters();
                        if iters > 0 {
                            rejection_iters_total.fetch_add(iters, Ordering::Relaxed);
                        }
                        if evals > 0 {
                            medfit_evals_total.fetch_add(evals, Ordering::Relaxed);
                        }
                    }
                }
                tick();
            });
        Ok(())
    })?;

    // Every input sample was finiteness-checked above, so a non-finite OUTPUT
    // could only come out of the combiner itself — unreachable by
    // construction. Checked anyway: a hard error beats shipping a poisoned
    // master that silently corrupts every light it calibrates.
    if let Some(bad) = out.iter().find(|v| !v.is_finite()) {
        return Err(IntegrationError::Decode(format!(
            "internal: non-finite value {bad} survived input filtering"
        )));
    }

    let total_samples = (w * h * n).max(1);
    Ok(IntegrationOutput {
        width: w,
        height: h,
        data: out,
        rejected_fraction: rejected.load(Ordering::Relaxed) as f64 / total_samples as f64,
        flat_norm: None,
        bad_samples_per_frame: bad_samples.into_iter().map(|a| a.into_inner()).collect(),
        all_bad_pixels: all_bad.into_inner(),
        read_duration: stats.read_duration,
        combine_duration: stats.combine_duration,
        band_rows: stats.band_rows,
        bands: stats.bands,
        bytes_read: stats.bytes_read,
        combine_cpu_duration: std::time::Duration::ZERO,
        rejection_iters_total: rejection_iters_total.load(Ordering::Relaxed),
        medfit_evals_total: medfit_evals_total.load(Ordering::Relaxed),
    })
}

#[allow(clippy::too_many_arguments)]
pub fn integrate_bias_like(
    paths: &[PathBuf],
    recipe: IntegrationRecipe,
    pool: &rayon::ThreadPool,
    scratch_dir: &Path,
    cancel: &AtomicBool,
    progress: EngineProgress<'_>,
    io: IoPolicy,
) -> Result<IntegrationOutput, IntegrationError> {
    integrate_bias_like_inner(paths, recipe, pool, scratch_dir, cancel, progress, io)
}

fn integrate_bias_like_inner(
    paths: &[PathBuf],
    recipe: IntegrationRecipe,
    pool: &rayon::ThreadPool,
    scratch_dir: &Path,
    cancel: &AtomicBool,
    progress: EngineProgress<'_>,
    io: IoPolicy,
) -> Result<IntegrationOutput, IntegrationError> {
    let src = BandSource::open_with_cancel(paths, scratch_dir, io.read_concurrency, cancel)?;
    let scales = vec![1.0f32; src.frame_count()];
    run_banded(&src, &scales, None, recipe, pool, cancel, &progress, io)
}

/// One frame's local-normalization row evaluator (M2, spec §5.2): given the
/// absolute output row `y` (band-relative rows must be offset by the band's
/// own `y0` before calling this — see `integrate_stack`'s band loop), fills
/// `a_row`/`b_row` (both the plane's `width` long) with the per-pixel
/// `(a, b)` local pair so the caller applies `v′ = a·v + b` in place of that
/// frame's global normalization pair. `FnMut`, not `Fn`: an instance is
/// never shared between workers (see `LocalNormRowFactory` below), so it is
/// free to own real mutable scratch with no locking.
///
/// Defined here, as an opaque closure, rather than taking
/// `crate::stacking::ln::grid::LnGrid` directly: `integration/` is gated on
/// the `render` feature alone, while the whole `stacking` tree additionally
/// requires `solver` — a direct dependency here would tie this module's
/// compilation to a feature it does not otherwise need.
pub type LocalNormRow<'a> = dyn FnMut(usize, &mut [f32], &mut [f32]) + 'a;

/// Factory for one frame's [`LocalNormRow`] (M2, fix round 1, item 3):
/// called once per rayon WORKER (via `for_each_init` in `integrate_stack`'s
/// band loop, never per row or per pixel) to produce a fresh, independent
/// evaluator that worker owns exclusively for the rest of the band. The
/// caller (`stacking::integrate::integrate_planes`, via `local_norm_row`)
/// builds one of these per frame per plane by closing over that frame's
/// `LnGrid`; every call to the factory allocates ITS OWN `LnScratch`, so
/// concurrent workers never contend on one frame's scratch buffer the way a
/// single shared `Fn` + `Mutex<LnScratch>` would.
pub type LocalNormRowFactory<'a> = dyn Fn() -> Box<LocalNormRow<'a>> + Sync + 'a;

/// Perf tier A Task 0 (audit §3.3, item I11): one rayon LEAF's own CPU-time
/// stopwatch. `start` is taken when [`RowState`] is built (`init_row_state`,
/// once per leaf — `for_each_init`'s own contract), and `Drop` folds the
/// elapsed time into `cpu_ns` (the `combine_cpu` atomic `integrate_stack`
/// captures for the whole call) the moment that leaf's `RowState` goes out
/// of scope — i.e. right after the last row that leaf's worker handles, not
/// once per row: `for_each_init` gives a fresh `T` per leaf and drops the
/// previous one when the next `init()` call replaces it, so this is the
/// natural place to flush without a second explicit "leaf finished" hook.
struct LeafCpuTimer<'c> {
    start: std::time::Instant,
    cpu_ns: &'c AtomicU64,
}

impl Drop for LeafCpuTimer<'_> {
    fn drop(&mut self) {
        let ns = self.start.elapsed().as_nanos() as u64;
        self.cpu_ns.fetch_add(ns, Ordering::Relaxed);
    }
}

/// One rayon WORKER's own state for `integrate_stack`'s band loop, built by
/// `init_row_state` and reused across every row that worker handles in the
/// band (`for_each_init`'s contract): the per-frame local-normalization
/// evaluators with their row buffers (M2, fix round 1 items 2 + 3), and
/// (M4c Task 3, fix round 1) the forced-rejection row cache — `n` row
/// pointers refilled once per row, empty when no forced source is present.
/// `_leaf_timer` (perf tier A Task 0) exists only for its `Drop` — nothing
/// in `process_row` ever reads it.
///
/// Perf tier A Task 9 (I5, audit §3.3): `local`'s `a_row`/`b_row` buffers
/// (`width` floats each, one pair per LN-active frame) used to be a fresh
/// `vec![0f32; width]` × 2 on EVERY leaf `init_row_state` built — the audit
/// measured 10.4 MB per leaf here on an LN-on set, ≈0.5 M page faults per
/// plane, since `for_each_init`'s `init` runs once per rayon LEAF, not once
/// per OS thread (a plane's leaves far outnumber the pool's threads). `Drop`
/// below hands those two buffers back to [`LN_ROW_BUFFERS`], this THREAD's
/// own pool, so the next leaf the SAME thread draws (`for_each_init` never
/// migrates a leaf's `T` to another thread) reuses the allocation instead of
/// paying for a fresh one. Pure scratch either way — see `LN_ROW_BUFFERS`'s
/// doc for why no stale content can ever be read.
struct RowState<'f, 'p, 'c> {
    local: Vec<(usize, Box<LocalNormRow<'f>>, Vec<f32>, Vec<f32>)>,
    forced_rows: Vec<Option<&'p [u64]>>,
    _leaf_timer: LeafCpuTimer<'c>,
}

thread_local! {
    /// Perf tier A Task 9 (I5): this OS thread's own pool of LN row buffers
    /// (`a_row`, `b_row`), one `(Vec<f32>, Vec<f32>)` per position in
    /// `local_factories` — the SAME fixed order every leaf of one
    /// `integrate_stack` call builds `RowState::local` in, so a pool slot
    /// always corresponds to the same LN-active frame within one call.
    /// `RowState`'s `Drop` impl returns a leaf's buffers here; the next
    /// `init_row_state` call on this thread (the same plane, a later band,
    /// or even a later plane/call — the buffers carry no lifetime, only
    /// their length matters) takes them back out. Safe to reuse blindly:
    /// `LnGrid::evaluate_row_into` (`stacking/ln/grid.rs`) unconditionally
    /// overwrites every element of both rows (`a_row[x] = va; b_row[x] =
    /// vb;` for every `x` in `0..ref_width`) before `process_row` ever
    /// reads a single element back out — no value survives a leaf, only
    /// the heap allocation does. `resize` (not `clear` + `resize`) is
    /// enough for the same reason: growing zero-fills the new tail, but
    /// nothing reads it before `evaluate_row_into` overwrites it either.
    static LN_ROW_BUFFERS: RefCell<Vec<(Vec<f32>, Vec<f32>)>> = RefCell::new(Vec::new());
}

impl Drop for RowState<'_, '_, '_> {
    fn drop(&mut self) {
        // Perf tier A Task 9 (I5): give this leaf's LN row buffers back to
        // this thread's own pool — skipped entirely (no thread-local touch
        // at all) when this call has no LN factories, i.e. every Plan 4 /
        // non-LN caller, since `self.local` is then always empty.
        if self.local.is_empty() {
            return;
        }
        LN_ROW_BUFFERS.with(|cell| {
            let mut pool = cell.borrow_mut();
            for (k, (_, _, a_row, b_row)) in self.local.drain(..).enumerate() {
                if k < pool.len() {
                    pool[k] = (a_row, b_row);
                } else {
                    pool.push((a_row, b_row));
                }
            }
        });
    }
}

/// Per-frame inputs of the stacking path, all indexed by the source's frame order.
pub struct StackParams<'a, 'f> {
    /// Rejection-normalization pair per frame (applied to the working copy).
    pub rejection: &'a [NormalizationPair],
    /// Output-normalization pair per frame (applied to the averaged values).
    pub output: &'a [NormalizationPair],
    /// Weight per frame (the plane's normalized weight; ≥ 0).
    pub weights: &'a [f32],
    /// Range rejection on RAW values: reject `raw <= range_low` (when Some) and `raw >= range_high` (when Some).
    pub range_low: Option<f32>,
    pub range_high: Option<f32>,
    /// Accumulate per-pixel low/high rejection counts.
    pub rejection_maps: bool,
    /// Per-frame local-normalization row evaluator FACTORY (M2, spec §5.2).
    /// `None` (the whole option) means no frame has a grid — every Plan 4
    /// caller passes this, and the band loop skips the local-normalization
    /// work entirely, byte-identical to before this field existed. A
    /// frame's own entry is `None` when that frame has no sidecar; it then
    /// keeps using its global `rejection`/`output` pair (see
    /// `local_for_rejection`/`local_for_output` below for which pair(s) a
    /// grid actually replaces). A `Some` entry is a FACTORY, not the
    /// evaluator itself (fix round 1, item 3) — `integrate_stack` calls it
    /// once per worker thread, never per row/pixel.
    ///
    /// `'f` is deliberately its OWN lifetime parameter, distinct from `'a`:
    /// the factories genuinely live as long as the caller's underlying LN
    /// grids (`stacking::integrate::integrate_planes` builds a fresh,
    /// short-lived `&'a [...]` array of them per plane, but each factory
    /// closure inside it borrows a `LnGrid` that outlives every plane of
    /// the call). Collapsing `'f` into `'a` made the per-plane array itself
    /// unbuildable — the borrow checker's dropck rules require a `dyn
    /// Trait`'s OWN embedded lifetime bound to be provable independently of
    /// how long the (short-lived) collection holding references to it is
    /// borrowed for.
    pub local: Option<&'a [Option<&'a LocalNormRowFactory<'f>>]>,
    /// Apply a frame's local pair to the working copy before rejection, in
    /// place of `rejection[i]`, for a frame that has one.
    pub local_for_rejection: bool,
    /// Apply a frame's local pair to the output sample, in place of
    /// `output[i]`, for a frame that has one.
    pub local_for_output: bool,
    /// M3 Task 2 (spec §6.2, ruling R-M3-8): receives one band's per-frame
    /// rejection bitmap after each band's per-pixel combine — `None` (every
    /// caller before this field existed, and every caller that doesn't want
    /// drizzle's survivor mask) skips the whole band-sized bit buffer and
    /// its allocation entirely; see `integrate_stack`'s band loop for how
    /// the `Some`/`None` cases are split so the `None` path pays no cost
    /// for a feature it isn't using.
    pub rejection_bits: Option<&'a (dyn RejectionBitSink + 'a)>,
    /// M4c Task 3 (spec §6.2's large-scale paragraph, ruling R-M4c-4):
    /// samples this source marks are dropped BEFORE any rejection algorithm
    /// runs — the algorithm then decides among what is left, and the forced
    /// samples are counted as rejected in the low/high maps, in
    /// `rejected_per_frame` and (when a sink is present) in the bitmaps,
    /// exactly the way a RANGE rejection is. They are NOT counted in
    /// `base.rejected_fraction`, which stays algorithm-only (see
    /// `StackOutput`'s own doc). `None` — every caller before this field
    /// existed, and every first-pass caller — leaves the per-pixel loop
    /// byte-identical: the forced test is a `bool && …` over a local the
    /// compiler proves false.
    pub forced_rejection: Option<&'a (dyn RejectionBitSource + 'a)>,
}

/// `base.rejected_fraction` counts ALGORITHM rejections only, exactly like
/// the unweighted master path's meaning of that field. `rejected_low` +
/// `rejected_high` below additionally count RANGE rejections — they are the
/// numbers to compare against an external "Total rejected samples" line;
/// divide by the sum of `samples_per_frame` for that fraction, not by
/// `base.rejected_fraction`'s denominator.
pub struct StackOutput {
    pub base: IntegrationOutput,
    /// Per-pixel rejected-sample counts, low and high sides (`Some` when requested).
    pub rejection_low: Option<Vec<f32>>,
    pub rejection_high: Option<Vec<f32>>,
    /// Rejected samples per frame (range + algorithm), indexed by frame.
    pub rejected_per_frame: Vec<u64>,
    /// Usable finite raw samples the frame offered, per frame — counted
    /// before range and algorithm rejection (a range-rejected sample is a
    /// sample, then a rejection).
    pub samples_per_frame: Vec<u64>,
    pub rejected_low: u64,
    pub rejected_high: u64,
}

/// Weighted, normalized banded integration with survivor accounting (spec
/// §6.1–6.2). Per sample: finiteness (missing coverage is skipped, not
/// rejected), range rejection on the raw value, then the rejection copy
/// `raw·rs + ro` decides survival and the output copy `raw·os + oo` is what
/// the survivors' weighted mean (or median) is taken over.
#[allow(clippy::too_many_arguments)]
pub fn integrate_stack<'p, 'f, S: FrameSource + ?Sized>(
    src: &S,
    params: &StackParams<'p, 'f>,
    recipe: IntegrationRecipe,
    pool: &rayon::ThreadPool,
    cancel: &AtomicBool,
    progress: EngineProgress<'_>,
    io: IoPolicy,
) -> Result<StackOutput, IntegrationError> {
    use rayon::prelude::*;
    let (w, h, n) = (src.width(), src.height(), src.frame_count());
    if params.rejection.len() != n || params.output.len() != n || params.weights.len() != n {
        return Err(IntegrationError::BadInput(format!(
            "stack params for {} / {} / {} frames, source has {n}",
            params.rejection.len(),
            params.output.len(),
            params.weights.len()
        )));
    }
    if params
        .rejection
        .iter()
        .chain(params.output.iter())
        .any(|p| !p.scale.is_finite() || !p.offset.is_finite())
    {
        return Err(IntegrationError::BadInput("normalization pairs must be finite".into()));
    }
    if params.weights.iter().any(|wt| !wt.is_finite() || *wt < 0.0) {
        return Err(IntegrationError::BadInput("weights must be finite and non-negative".into()));
    }
    if params.weights.iter().all(|wt| *wt == 0.0) {
        return Err(IntegrationError::BadInput("every weight is zero".into()));
    }
    // M3 Task 2: a rejection-bit sink must agree with the source on both
    // dimensions it cares about, checked once here rather than once per
    // band — a mismatched `frames()` would silently misindex `record_band`'s
    // `[row][frame][word]` layout, and a mismatched `words_per_row()` would
    // silently truncate or overrun a row's bits.
    if let Some(sink) = params.rejection_bits {
        if sink.frames() != n {
            return Err(IntegrationError::BadInput(format!(
                "rejection bit sink expects {} frames, source has {n}",
                sink.frames()
            )));
        }
        let expected_words = w.div_ceil(64);
        if sink.words_per_row() != expected_words {
            return Err(IntegrationError::BadInput(format!(
                "rejection bit sink words_per_row {} != expected {expected_words} for width {w}",
                sink.words_per_row()
            )));
        }
    }
    // M4c Task 3: the same two checks for the forced-rejection source, for
    // the same reason — a mismatched `frames()` would answer another
    // frame's question and a mismatched `words_per_row()` would index past
    // a row's own words.
    if let Some(forced) = params.forced_rejection {
        if forced.frames() != n {
            return Err(IntegrationError::BadInput(format!(
                "forced-rejection source expects {} frames, source has {n}",
                forced.frames()
            )));
        }
        let expected_words = w.div_ceil(64);
        if forced.words_per_row() != expected_words {
            return Err(IntegrationError::BadInput(format!(
                "forced-rejection source words_per_row {} != expected {expected_words} for width {w}",
                forced.words_per_row()
            )));
        }
    }
    if n > u16::MAX as usize {
        return Err(IntegrationError::BadInput(format!("{n} frames exceed the 65535-frame stack limit")));
    }
    if let Some(local) = params.local {
        if local.len() != n {
            return Err(IntegrationError::BadInput(format!(
                "local-normalization grids for {} frames, source has {n}",
                local.len()
            )));
        }
    }
    // M2, ruling R2 (amended by C1): rejection-only local normalization
    // tolerates a frame with no grid — it falls back to the SAME global
    // rejection pair `ScaleZeroOffset` would give it (`stats::rejection_pair`
    // resolves `Local` to `output_pair(.., AdditiveWithScaling)` precisely
    // for this fallback), never an un-normalized identity pair — Task 5's
    // own pipeline already excludes a grid-less frame when LN drives OUTPUT
    // normalization instead, so no equivalent warning is needed for
    // `local_for_output`. Logged once per call, not per row/pixel.
    if params.local_for_rejection {
        let missing = match params.local {
            Some(local) => local.iter().filter(|g| g.is_none()).count(),
            None => n,
        };
        if missing > 0 {
            tracing::warn!(
                missing,
                frames = n,
                "local rejection normalization: some frames have no grid; \
                 falling back to their global rejection pair for those frames"
            );
        }
    }
    let want_local = params.local.is_some() && (params.local_for_rejection || params.local_for_output);
    // Fix round 1, item 1: split the frame set ONCE, here — never per row,
    // never per pixel — into the frames a grid actually applies to
    // (`local_factories`, paired with their factory) and every other frame
    // (`frames_without_local`). With `want_local` false (every Plan 4
    // caller, `local: None`) `local_factories` is empty and
    // `frames_without_local` is `0..n` in ascending order — the per-pixel
    // loop below then runs exactly one loop, in the exact frame order the
    // pre-M2 code always used, with zero local-normalization branching or
    // lookups anywhere in it. This is what keeps the Plan 4 `to_bits()` pin
    // byte-identical: not just "produces the same value" but "executes the
    // same instructions."
    let local_factories: Vec<(usize, &'p LocalNormRowFactory<'f>)> = if want_local {
        match params.local {
            Some(grids) => grids
                .iter()
                .enumerate()
                .filter_map(|(i, g)| g.map(|factory| (i, factory)))
                .collect(),
            None => Vec::new(),
        }
    } else {
        Vec::new()
    };
    let frames_without_local: Vec<usize> = {
        let mut has_local = vec![false; n];
        for &(i, _) in &local_factories {
            has_local[i] = true;
        }
        (0..n).filter(|&i| !has_local[i]).collect()
    };
    // A5 (final fix wave): frame index -> its position in `local_factories`
    // (and, since `init_local_state` below builds each worker's own
    // `local_state` by mapping `local_factories` in the same order, that
    // position is ALSO `local_state`'s position for the same frame) —
    // precomputed ONCE for the whole call, never per row/pixel, purely so
    // the mixed-order per-pixel loop can ask "does frame i have an active
    // grid, and where" in O(1). Only consulted when `local_state` is
    // non-empty; see that branch's own doc below for why this exists.
    let local_pos_by_frame: Vec<Option<usize>> = {
        let mut pos = vec![None; n];
        for (k, &(i, _)) in local_factories.iter().enumerate() {
            pos[i] = Some(k);
        }
        pos
    };
    let mut out = vec![0f32; w * h];
    let rejected = AtomicUsize::new(0);
    let rejected_low = AtomicU64::new(0);
    let rejected_high = AtomicU64::new(0);
    let bad_samples: Vec<AtomicUsize> = (0..n).map(|_| AtomicUsize::new(0)).collect();
    let rejected_per_frame: Vec<AtomicU64> = (0..n).map(|_| AtomicU64::new(0)).collect();
    let samples_per_frame: Vec<AtomicU64> = (0..n).map(|_| AtomicU64::new(0)).collect();
    let all_bad = AtomicUsize::new(0);
    // Perf tier A Task 0 (audit §3.3, item I11): nanoseconds of per-leaf
    // combine CPU time (`LeafCpuTimer`/`RowState::Drop`) and the summed
    // rejection-algorithm iteration/evaluation counts
    // (`combine::take_rejection_counters`, flushed once per pixel stack
    // right after `combine_pixel_weighted` below). Captured by the
    // `combine` closure passed to `band_loop` below like every other atomic
    // in this scope — `band_loop` itself never needs to know these exist.
    let combine_cpu = AtomicU64::new(0);
    let rejection_iters_total = AtomicU64::new(0);
    let medfit_evals_total = AtomicU64::new(0);
    let maps = params.rejection_maps;
    // Rejection-map row storage, banded via a per-band lock (not per pixel —
    // see the loop below): full-image sized when maps are requested, or a
    // single reusable band-sized scratch pair when they are not, so the
    // `combine` closure can always zip three real row iterators without a
    // branch inside the row loop. Allocated once per RUN either way. Fix
    // round 1, Important finding 1: must match what `band_loop` will
    // actually use (`loop_band_rows`, half the budget) — reading the
    // un-halved budget here over-allocated this no-maps scratch pair ~2x.
    let band_rows_cap = loop_band_rows(src, io);
    let map_low = std::sync::Mutex::new(if maps { vec![0f32; w * h] } else { vec![0f32; band_rows_cap * w] });
    let map_high = std::sync::Mutex::new(if maps { vec![0f32; w * h] } else { vec![0f32; band_rows_cap * w] });
    // M3 Task 2: `ceil(width / 64)`, the same value the sink itself reports
    // via `words_per_row()` (checked equal above) — computed once here so
    // the band closure never calls back into the sink for it.
    let bit_words = w.div_ceil(64);

    let stats = band_loop(src, pool, cancel, &progress, io, &mut out, &|job, tick| {
        let BandJob { planes, out_band, y0, rows, width } = job;
        let words = combine::mask_words(n);
        let mut low_guard = map_low.lock().unwrap();
        let mut high_guard = map_high.lock().unwrap();
        let low_band: &mut [f32] = if maps {
            &mut low_guard[y0 * width..(y0 + rows) * width]
        } else {
            &mut low_guard[..rows * width]
        };
        let high_band: &mut [f32] = if maps {
            &mut high_guard[y0 * width..(y0 + rows) * width]
        } else {
            &mut high_guard[..rows * width]
        };
        // Fix round 1, items 2 + 3: per-worker state, built once per rayon
        // job (`for_each_init`'s own contract — see its doc: called "only as
        // needed for a value to be paired with the group of items in each
        // rayon job", never per row or per pixel) and reused across every
        // row that worker processes within THIS band. One entry per
        // frame-with-a-grid: `(frame_index, its OWN evaluator instance from
        // the factory, a_row, b_row)` — calling the factory here, not
        // sharing one evaluator across workers, is what removes the need
        // for any lock on the hot path (a shared `Fn` + `Mutex<LnScratch>`
        // serialized every worker through one frame's scratch buffer).
        // M4c Task 3, fix round 1 (m3): the per-worker state now also
        // carries the forced-rejection row cache — `n` row pointers, filled
        // once per ROW and read per (pixel, frame), allocated here with the
        // rest of the worker's state instead of once per row. Empty (no
        // allocation at all) when no forced source is present, which is
        // every first-pass caller.
        //
        // Perf tier A Task 9 (I5): `a_row`/`b_row` are drawn from this
        // thread's own `LN_ROW_BUFFERS` pool instead of freshly allocated —
        // see that thread_local's doc and `RowState`'s `Drop` impl for the
        // reuse contract. `local_factories`' order (and therefore each
        // entry's pool slot) is fixed for the whole `integrate_stack` call.
        let init_row_state = || -> RowState<'f, 'p, '_> {
            let local = LN_ROW_BUFFERS.with(|cell| {
                let mut pool = cell.borrow_mut();
                local_factories
                    .iter()
                    .enumerate()
                    .map(|(k, &(i, factory))| {
                        let (mut a_row, mut b_row) = if k < pool.len() {
                            std::mem::replace(&mut pool[k], (Vec::new(), Vec::new()))
                        } else {
                            (Vec::new(), Vec::new())
                        };
                        a_row.resize(width, 0.0);
                        b_row.resize(width, 0.0);
                        (i, factory(), a_row, b_row)
                    })
                    .collect()
            });
            RowState {
                local,
                forced_rows: if params.forced_rejection.is_some() {
                    vec![None; n]
                } else {
                    Vec::new()
                },
                _leaf_timer: LeafCpuTimer {
                    start: std::time::Instant::now(),
                    cpu_ns: &combine_cpu,
                },
            }
        };
        // M3 Task 2: the whole per-row body, factored out so it can be
        // called from either of the two zip chains below (one with a 4th
        // `band_bits` chunk, one without) without duplicating ~200 lines of
        // per-pixel logic twice over. `bits_row` is `Some` only on the
        // sink-present chain; every `if let Some(bits) = &mut bits_row`
        // below is the ONLY new work this closure does relative to before
        // this field existed — on the `None` chain those checks still run
        // (a cheap `Option::is_none` each), but nothing is ever written
        // anywhere the OUTPUT or the rejection accounting reads from, so
        // the `None`-path numeric results (and the Plan 4 / M2 pins that
        // check them) are unaffected either way. The buffer/zip/allocation
        // itself is what stays entirely absent on the `None` chain (see the
        // `match` below).
        let process_row = |row_in_band: usize,
                            out_row: &mut [f32],
                            low_row: &mut [f32],
                            high_row: &mut [f32],
                            mut bits_row: Option<&mut [u64]>,
                            row_state: &mut RowState<'f, 'p, '_>| {
                let RowState { local: local_state, forced_rows, .. } = row_state;
                // Per-worker scratch, allocated once per ROW (not per pixel):
                // `work`/`out_vals`/`mask` feed `combine_pixel_weighted`,
                // `scratch` is its own reused survivor-value buffer (Task 1
                // fix round). `rej_vals`/`present` are FRAME-indexed (unlike
                // `work`, which the rejection routines compact forward
                // destructively — `work[kept..]` ends up holding duplicated
                // survivor entries, not the rejected ones, so a rejection
                // can never be recovered by walking `work` after the call;
                // see the fix-round-1 ruling in the plan doc). The
                // `row_*` counters below fold every per-sample update into
                // one atomic flush per frame per row instead of one atomic
                // per sample.
                let mut work: Vec<(f32, u16)> = Vec::with_capacity(n);
                let mut out_vals = vec![0f32; n];
                let mut mask = vec![0u64; words];
                let mut scratch: Vec<f32> = Vec::with_capacity(n);
                let mut rej_vals = vec![0f32; n];
                let mut present = vec![0u64; words];
                let mut row_samples = vec![0u32; n];
                let mut row_rejected = vec![0u32; n];
                let mut row_bad = vec![0u32; n];
                let mut row_rej_total = 0usize;
                let mut row_low = 0u64;
                let mut row_high = 0u64;
                let mut row_all_bad = 0usize;
                // M2: evaluate every local grid's row ONCE per row — never
                // once per pixel (`LnGrid::evaluate_row_into` is O(gw +
                // width), not O(width) per call) — into THIS WORKER's own
                // buffers from `local_state` (fix round 1, items 2 + 3: no
                // allocation and no lock here, both already paid for once,
                // outside this closure, by `init_local_state` above).
                // `y_abs` is the ABSOLUTE output row — `y0` is this band's
                // first row, `row_in_band` is relative to it — never the
                // band-relative row alone (fix round 1, item 5's own test
                // pins this against a forced multi-band run).
                let y_abs = y0 + row_in_band;
                for (_, eval, a_row, b_row) in local_state.iter_mut() {
                    eval(y_abs, a_row, b_row);
                }
                // M4c Task 3: this row's forced-rejection bits, fetched ONCE
                // per frame per row — never once per (pixel, frame), which
                // on a real stack would be billions of `&dyn` calls per
                // plane (see `RejectionBitSource::forced_row`'s own doc).
                // Fix round 1 (m3): into the worker's OWN buffer (allocated
                // by `init_row_state`), not a fresh `Vec` per row.
                // `has_forced` stays false for every caller that passes no
                // source, and the per-pixel test below is then a `bool &&`
                // the compiler folds away — the `None` path's instructions,
                // and therefore its byte-identical output, are unchanged.
                let has_forced = !forced_rows.is_empty();
                if let Some(src) = params.forced_rejection {
                    for (i, slot) in forced_rows.iter_mut().enumerate() {
                        *slot = src.forced_row(i, y_abs);
                    }
                }
                for (x, out_px) in out_row.iter_mut().enumerate() {
                    work.clear();
                    combine::mask_clear(&mut mask);
                    combine::mask_clear(&mut present);
                    let idx = row_in_band * width + x;
                    let mut low_here = 0u32;
                    let mut high_here = 0u32;
                    // Set when at least one sample at THIS pixel was forced
                    // out: the side/bitmap/per-frame attribution below then
                    // runs even if the algorithm itself rejected nothing.
                    let mut forced_any = false;
                    if local_state.is_empty() {
                        // Fix round 1, item 1: frames with NO active local
                        // override run the EXACT pre-M2 instructions — no
                        // local-normalization branch, lookup, or closure
                        // call anywhere in this loop body. With
                        // `want_local` false (every Plan 4 caller)
                        // `local_state` is empty and `frames_without_local`
                        // is `0..n` in ascending order, so this is the
                        // ENTIRE per-pixel frame loop, in the exact order
                        // the pre-M2 code always used — the Plan 4
                        // `to_bits()` pin's byte-identity comes from this,
                        // not just from the arithmetic happening to agree.
                        // Left textually untouched by A5 (below) on
                        // purpose: this is the hot path (LN off), and nothing
                        // about the ordering defect A5 fixes can reach it —
                        // it only ever had one list to walk.
                        for &i in &frames_without_local {
                            let raw = planes.sample(i, idx);
                            if !raw.is_finite() {
                                row_bad[i] += 1;
                                continue;
                            }
                            row_samples[i] += 1;
                            if let Some(lo) = params.range_low {
                                if raw <= lo {
                                    low_here += 1;
                                    row_rejected[i] += 1;
                                    if let Some(bits) = &mut bits_row {
                                        bits[i * bit_words + x / 64] |= 1u64 << (x % 64);
                                    }
                                    continue;
                                }
                            }
                            if let Some(hi) = params.range_high {
                                if raw >= hi {
                                    high_here += 1;
                                    row_rejected[i] += 1;
                                    if let Some(bits) = &mut bits_row {
                                        bits[i * bit_words + x / 64] |= 1u64 << (x % 64);
                                    }
                                    continue;
                                }
                            }
                            let rej = params.rejection[i].apply(raw);
                            let outv = params.output[i].apply(raw);
                            if !rej.is_finite() || !outv.is_finite() {
                                row_bad[i] += 1;
                                continue;
                            }
                            out_vals[i] = outv;
                            rej_vals[i] = rej;
                            combine::mask_set(&mut present, i);
                            // M4c Task 3: a forced sample is PRESENT (so the
                            // attribution below can decide its side against
                            // the survivors' median, exactly as an
                            // algorithm rejection's is) but never enters
                            // `work`, so no rejection routine ever sees it.
                            if has_forced
                                && forced_rows[i]
                                    .is_some_and(|wd| (wd[x / 64] >> (x % 64)) & 1 != 0)
                            {
                                forced_any = true;
                            } else {
                                work.push((rej, i as u16));
                            }
                        }
                    } else {
                        // A5 (final fix wave): frames visited `0..n` in
                        // ASCENDING index order regardless of whether each
                        // one has an active grid — `local_for_rejection`/
                        // `local_for_output` independently decide which
                        // consumer(s) the grid's `(a, b)` pair replaces for
                        // a frame that has one; the frame's global pair
                        // still applies to whichever consumer is not set,
                        // AND to every frame with no grid at all (`grid` is
                        // `None` for those — the `_` arms below). Previously
                        // this pushed every no-grid frame into `work`
                        // before any grid frame, in two separate
                        // concatenated lists — tied rejection samples then
                        // summed (and were attributed in
                        // `rejected_per_frame`) in whatever order the two
                        // lists happened to interleave, not the frame's own
                        // index order, breaking `combine_pixel_weighted`'s
                        // stable-sort contract for reproducible ties.
                        for i in 0..n {
                            let raw = planes.sample(i, idx);
                            if !raw.is_finite() {
                                row_bad[i] += 1;
                                continue;
                            }
                            row_samples[i] += 1;
                            if let Some(lo) = params.range_low {
                                if raw <= lo {
                                    low_here += 1;
                                    row_rejected[i] += 1;
                                    if let Some(bits) = &mut bits_row {
                                        bits[i * bit_words + x / 64] |= 1u64 << (x % 64);
                                    }
                                    continue;
                                }
                            }
                            if let Some(hi) = params.range_high {
                                if raw >= hi {
                                    high_here += 1;
                                    row_rejected[i] += 1;
                                    if let Some(bits) = &mut bits_row {
                                        bits[i * bit_words + x / 64] |= 1u64 << (x % 64);
                                    }
                                    continue;
                                }
                            }
                            let grid = local_pos_by_frame[i].map(|k| {
                                let (_, _eval, a_row, b_row) = &local_state[k];
                                (a_row[x], b_row[x])
                            });
                            let rej = match grid {
                                Some((a, b)) if params.local_for_rejection => a * raw + b,
                                _ => params.rejection[i].apply(raw),
                            };
                            let outv = match grid {
                                Some((a, b)) if params.local_for_output => a * raw + b,
                                _ => params.output[i].apply(raw),
                            };
                            if !rej.is_finite() || !outv.is_finite() {
                                row_bad[i] += 1;
                                continue;
                            }
                            out_vals[i] = outv;
                            rej_vals[i] = rej;
                            combine::mask_set(&mut present, i);
                            // M4c Task 3, same rule as the no-local branch
                            // above (this loop's own doc explains why the
                            // two are kept textually apart).
                            if has_forced
                                && forced_rows[i]
                                    .is_some_and(|wd| (wd[x / 64] >> (x % 64)) & 1 != 0)
                            {
                                forced_any = true;
                            } else {
                                work.push((rej, i as u16));
                            }
                        }
                    }
                    if work.is_empty() {
                        *out_px = 0.0;
                        row_all_bad += 1;
                        // M4c Task 3: every sample forced out (or every one
                        // range-rejected before this field existed) leaves
                        // nothing to combine — the pixel is `all_bad`, the
                        // same verdict an all-range-rejected pixel has
                        // always had. The forced samples are still counted
                        // as rejections, against the median of every
                        // present value, since no survivor exists to
                        // compare them with.
                        if forced_any {
                            scratch.clear();
                            for i in 0..n {
                                if combine::mask_get(&present, i) {
                                    scratch.push(rej_vals[i]);
                                }
                            }
                            scratch.sort_by(|a, b| a.total_cmp(b));
                            let median = scratch[scratch.len() / 2];
                            for i in 0..n {
                                if combine::mask_get(&present, i) {
                                    row_rejected[i] += 1;
                                    if let Some(bits) = &mut bits_row {
                                        bits[i * bit_words + x / 64] |= 1u64 << (x % 64);
                                    }
                                    if rej_vals[i] < median {
                                        low_here += 1
                                    } else {
                                        high_here += 1
                                    }
                                }
                            }
                        }
                    } else {
                        let (val, rej_count, work_sorted) = combine::combine_pixel_weighted(
                            &mut work,
                            &out_vals,
                            params.weights,
                            recipe,
                            &mut mask,
                            &mut scratch,
                        );
                        // Perf tier A Task 0: this thread's iteration/
                        // evaluation counts for exactly this pixel stack —
                        // zero for every rejection algorithm but
                        // `LinearFitClip`, since only that one touches the
                        // counters `take_rejection_counters` reads.
                        let (iters, evals) = combine::take_rejection_counters();
                        if iters > 0 {
                            rejection_iters_total.fetch_add(iters, Ordering::Relaxed);
                        }
                        if evals > 0 {
                            medfit_evals_total.fetch_add(evals, Ordering::Relaxed);
                        }
                        *out_px = val;
                        // M4c Task 3: `|| forced_any` — a pixel whose only
                        // rejection is a FORCED one still needs the
                        // attribution below (`rejected_per_frame`, the
                        // low/high maps, the bitmaps). `row_rej_total`
                        // stays algorithm-only: `rej_count` is 0 in that
                        // case, so `base.rejected_fraction` keeps its
                        // meaning.
                        if rej_count > 0 || forced_any {
                            row_rej_total += rej_count;
                            // Survivors are the compacted prefix; the rejected
                            // entries were overwritten by the compaction, so the
                            // side of each rejection is decided per FRAME from
                            // the values kept in `rej_vals`, against the
                            // survivors' median (all rejected → the median of
                            // every present value).
                            let kept = work.len() - rej_count;
                            // Perf tier A Task 9 (I5, audit §3.3): this
                            // "median" has always been a plain positional
                            // read — `scratch[scratch.len() / 2]`, no
                            // odd/even averaging — of the SAME values
                            // `work[..kept]` already holds. When
                            // `combine_pixel_weighted` reports that prefix
                            // already ascending by value (`work_sorted`,
                            // true for every rejection algorithm but
                            // `None`/`SigmaClip`, and only once something
                            // survived), that read is `work[kept / 2].0`
                            // directly — no second copy, no second sort.
                            // `None`/`SigmaClip` (survivors stay in
                            // push — i.e. frame-index — order, never
                            // value order) and the forced-only/all-rejected
                            // case (`kept == 0`, full `work` instead of a
                            // prefix, no sortedness promised) keep the
                            // original path: copy the relevant values out,
                            // sort them, and read the same position.
                            let median = if work_sorted && kept > 0 {
                                work[kept / 2].0
                            } else {
                                scratch.clear();
                                let source = if kept > 0 { &work[..kept] } else { &work[..] };
                                scratch.extend(source.iter().map(|&(v, _)| v));
                                scratch.sort_by(|a, b| a.total_cmp(b));
                                scratch[scratch.len() / 2]
                            };
                            for i in 0..n {
                                if combine::mask_get(&present, i) && !combine::mask_get(&mask, i) {
                                    row_rejected[i] += 1;
                                    if let Some(bits) = &mut bits_row {
                                        bits[i * bit_words + x / 64] |= 1u64 << (x % 64);
                                    }
                                    if rej_vals[i] < median {
                                        low_here += 1
                                    } else {
                                        high_here += 1
                                    }
                                }
                            }
                        }
                    }
                    row_low += low_here as u64;
                    row_high += high_here as u64;
                    if maps {
                        low_row[x] = low_here as f32;
                        high_row[x] = high_here as f32;
                    }
                }
                for i in 0..n {
                    if row_samples[i] > 0 {
                        samples_per_frame[i].fetch_add(row_samples[i] as u64, Ordering::Relaxed);
                    }
                    if row_rejected[i] > 0 {
                        rejected_per_frame[i].fetch_add(row_rejected[i] as u64, Ordering::Relaxed);
                    }
                    if row_bad[i] > 0 {
                        bad_samples[i].fetch_add(row_bad[i] as usize, Ordering::Relaxed);
                    }
                }
                if row_rej_total > 0 {
                    rejected.fetch_add(row_rej_total, Ordering::Relaxed);
                }
                if row_low > 0 {
                    rejected_low.fetch_add(row_low, Ordering::Relaxed);
                }
                if row_high > 0 {
                    rejected_high.fetch_add(row_high, Ordering::Relaxed);
                }
                if row_all_bad > 0 {
                    all_bad.fetch_add(row_all_bad, Ordering::Relaxed);
                }
                tick();
            };

        // M3 Task 2: the 4th `band_bits` chunk (and its allocation) exists
        // ONLY on this `Some` arm — mirrors how `maps` sizes `map_low`/
        // `map_high` above, but taken one step further: with no sink there
        // is no per-band bit buffer at all, not even a band-sized one, so a
        // caller that never asked for rejection bits (every Plan 4 / M2
        // caller) pays nothing for this field's existence.
        match params.rejection_bits {
            Some(sink) => {
                // Fix round 1, M6: `rows * n * bit_words * 8` bytes, freshly
                // allocated and zeroed once per band (not once per run, the
                // way `map_low`/`map_high` above are — those get reused
                // across bands via the outer `Mutex<Vec<_>>`; this one
                // doesn't need to be, since it is fully consumed by
                // `sink.record_band` before the next band starts). At 208
                // frames, 6248px width and a ~98-row band this is ≈16 MB,
                // reallocated ~40 times per plane — cheap next to a band's
                // own combine, but it is peak RSS neither R-M3-7's `need`
                // formula (drizzle's own memory refusal, Task 3) nor
                // `stacking::paths::estimate_bytes`'s run-footprint estimate
                // currently accounts for. Named here so whichever of those
                // two Task 5/6 end up tightening does not have to
                // rediscover it.
                let mut band_bits = vec![0u64; rows * n * bit_words];
                out_band
                    .par_chunks_mut(width)
                    .zip(low_band.par_chunks_mut(width))
                    .zip(high_band.par_chunks_mut(width))
                    .zip(band_bits.par_chunks_mut(n * bit_words))
                    .enumerate()
                    .for_each_init(
                        init_row_state,
                        |row_state, (row_in_band, (((out_row, low_row), high_row), bits_row))| {
                            process_row(row_in_band, out_row, low_row, high_row, Some(bits_row), row_state);
                        },
                    );
                sink.record_band(y0, rows, &band_bits)?;
            }
            None => {
                out_band
                    .par_chunks_mut(width)
                    .zip(low_band.par_chunks_mut(width))
                    .zip(high_band.par_chunks_mut(width))
                    .enumerate()
                    .for_each_init(init_row_state, |row_state, (row_in_band, ((out_row, low_row), high_row))| {
                        process_row(row_in_band, out_row, low_row, high_row, None, row_state);
                    });
            }
        }
        Ok(())
    })?;

    // Every input sample was finiteness-checked above, so a non-finite OUTPUT
    // could only come out of the combiner itself — unreachable by
    // construction (same guard as the unweighted path).
    if let Some(bad) = out.iter().find(|v| !v.is_finite()) {
        return Err(IntegrationError::Decode(format!(
            "internal: non-finite value {bad} survived input filtering"
        )));
    }

    let map_low = map_low.into_inner().unwrap();
    let map_high = map_high.into_inner().unwrap();
    let total_samples = (w * h * n).max(1);
    Ok(StackOutput {
        base: IntegrationOutput {
            width: w,
            height: h,
            data: out,
            rejected_fraction: rejected.load(Ordering::Relaxed) as f64 / total_samples as f64,
            flat_norm: None,
            bad_samples_per_frame: bad_samples.into_iter().map(|a| a.into_inner()).collect(),
            all_bad_pixels: all_bad.into_inner(),
            read_duration: stats.read_duration,
            combine_duration: stats.combine_duration,
            band_rows: stats.band_rows,
            bands: stats.bands,
            bytes_read: stats.bytes_read,
            combine_cpu_duration: std::time::Duration::from_nanos(
                combine_cpu.load(Ordering::Relaxed),
            ),
            rejection_iters_total: rejection_iters_total.load(Ordering::Relaxed),
            medfit_evals_total: medfit_evals_total.load(Ordering::Relaxed),
        },
        rejection_low: maps.then_some(map_low),
        rejection_high: maps.then_some(map_high),
        rejected_per_frame: rejected_per_frame.into_iter().map(|a| a.into_inner()).collect(),
        samples_per_frame: samples_per_frame.into_iter().map(|a| a.into_inner()).collect(),
        rejected_low: rejected_low.into_inner(),
        rejected_high: rejected_high.into_inner(),
    })
}

/// Integrates lazily resampled registered frames (spec §6.1): every band is
/// resampled from the calibrated files through their transforms on the
/// way in, so no registered file is ever written. Thin wrapper over
/// `integrate_stack`: identity normalization pairs, unit weights, no range
/// rejection, no rejection maps.
#[allow(clippy::too_many_arguments)]
pub fn integrate_registered(
    src: &RegisteredSource,
    recipe: IntegrationRecipe,
    pool: &rayon::ThreadPool,
    cancel: &AtomicBool,
    progress: EngineProgress<'_>,
    io: IoPolicy,
) -> Result<IntegrationOutput, IntegrationError> {
    let n = src.frame_count();
    let identity = vec![NormalizationPair::IDENTITY; n];
    let unit_weights = vec![1.0f32; n];
    let params = StackParams {
        rejection: &identity,
        output: &identity,
        weights: &unit_weights,
        range_low: None,
        range_high: None,
        rejection_maps: false,
        local: None,
        local_for_rejection: false,
        local_for_output: false,
        rejection_bits: None,
        forced_rejection: None,
    };
    Ok(integrate_stack(src, &params, recipe, pool, cancel, progress, io)?.base)
}

#[allow(clippy::too_many_arguments)]
pub fn integrate_flat(
    paths: &[PathBuf],
    precal: &FlatPrecal,
    recipe: IntegrationRecipe,
    pool: &rayon::ThreadPool,
    scratch_dir: &Path,
    cancel: &AtomicBool,
    progress: EngineProgress<'_>,
    io: IoPolicy,
) -> Result<IntegrationOutput, IntegrationError> {
    integrate_flat_inner(paths, precal, recipe, pool, scratch_dir, cancel, progress, io)
}

#[allow(clippy::too_many_arguments)]
fn integrate_flat_inner(
    paths: &[PathBuf],
    precal: &FlatPrecal,
    recipe: IntegrationRecipe,
    pool: &rayon::ThreadPool,
    scratch_dir: &Path,
    cancel: &AtomicBool,
    progress: EngineProgress<'_>,
    io: IoPolicy,
) -> Result<IntegrationOutput, IntegrationError> {
    let src = BandSource::open_with_cancel(paths, scratch_dir, io.read_concurrency, cancel)?;
    let (w, h, n) = (src.width(), src.height(), src.frame_count());
    if let FlatPrecal::MasterFrame { width, height, .. } = precal {
        if (*width, *height) != (w, h) {
            return Err(IntegrationError::BadInput(format!(
                "pre-calibration master is {width}x{height}, flats are {w}x{h}"
            )));
        }
    }

    // Pass 1: per-frame central-third mean AFTER precal subtraction.
    let (cy0, cy1) = (h / 3, ((2 * h) / 3).max(h / 3 + 1).min(h));
    let (cx0, cx1) = (w / 3, ((2 * w) / 3).max(w / 3 + 1).min(w));
    let mut sums = vec![0f64; n];
    let mut counts = vec![0usize; n];
    // Deliberately the UN-halved budget: this pass reads and sums one band
    // at a time with no double buffering, so it never needs the prefetch
    // `loop_band_rows` sizes for — that halving applies only inside
    // `band_loop` (pass 2, below), and only on network storage.
    let band_rows = src.band_rows_for_budget(io.band_budget_bytes).min(cy1 - cy0);
    let mut planes = BandPlanes::new(&src);
    // Computed once, next to `band_rows` — pass 1 only ever reads the
    // central-third rows, so its share of the total is that row count, not
    // the full height `run_banded` (pass 2, below) will report on its own.
    let per_row_bytes = src.bytes_per_row();
    let pass1_total_bytes = ((cy1 - cy0) * per_row_bytes) as u64;
    // Fix round 2, I2: pass 1 used to report NOTHING to `progress.on_band`
    // (its only contribution was the baseline folded into pass 2's FIRST
    // call, via `wrapped_on_band` below) — a silent 25% of a flat's bytes,
    // per the observed asymmetry in `EngineProgress::on_band`'s doc. Forecast
    // pass 2's own band count/total up front purely so pass 1's ticks below
    // have a real two-pass total to report against; `loop_band_rows` is a
    // pure function of `src`/`io`, called here AND by `band_loop` itself
    // (pass 2, inside `run_banded` below) — fix round 1, Important finding
    // 1: the two used to read the budget differently (this one un-halved,
    // `band_loop`'s halved since perf tier 1 Task 1's prefetch), so the
    // forecast and the real band count disagreed by ~2x mid-run. Sharing one
    // function is what makes them provably identical, not merely intended
    // to be — pinned by `flat_pass1_forecast_matches_pass2_actual_band_total`.
    let pass2_band_rows_forecast = loop_band_rows(&src, io);
    let bands_total_forecast = h.div_ceil(pass2_band_rows_forecast);
    let two_pass_total_bytes = pass1_total_bytes + (h * per_row_bytes) as u64;
    let mut pass1_read = std::time::Duration::ZERO;
    let mut pass1_bytes_read: u64 = 0;
    let mut y = cy0;
    while y < cy1 {
        if cancel.load(Ordering::Relaxed) { return Err(IntegrationError::Cancelled); }
        let rows = band_rows.min(cy1 - y);
        let t_read = std::time::Instant::now();
        // `band_index_1based` pinned at 0 — "no pass-2 band reached yet" —
        // against the same `bands_total_forecast`/`two_pass_total_bytes`
        // pass 2's own calls use once it starts (see `wrapped_on_band`
        // below), so a caller sees ONE continuously climbing bytes fraction
        // across both passes rather than a blackout followed by a jump.
        //
        // Fix round 3, Important 1: same `Mutex`-as-one-critical-section fix
        // as `run_banded`'s tick (see its comment) — an `AtomicU64::fetch_max`
        // high-water mark is NOT enough on its own: two workers can both
        // pass the "am I a new maximum" check and then still race each
        // other into the actual `on_band` call afterward. The `Mutex` makes
        // "add my bytes, then emit" one critical section, so emissions are
        // ordered by lock-acquisition order — the same order the bytes were
        // added in.
        let bytes_before_this_chunk = pass1_bytes_read;
        let chunk_bytes_so_far = std::sync::Mutex::new(0u64);
        let on_bytes = |just_read: u64| {
            let mut so_far = chunk_bytes_so_far.lock().unwrap();
            *so_far += just_read;
            (progress.on_band)(0, bands_total_forecast, bytes_before_this_chunk + *so_far, two_pass_total_bytes);
        };
        src.read_band_with_progress(y, rows, &mut planes, io.read_concurrency, &on_bytes, cancel)?;
        pass1_read += t_read.elapsed();
        pass1_bytes_read += (rows * per_row_bytes) as u64;
        // Fix wave item 1: mirrors `run_banded`'s post-read check — pass 1
        // has no combine phase of its own to guard a second time, but its
        // last chunk has no future loop iteration either, so the same
        // "don't wait for a top-of-loop check that may never come" reasoning
        // applies to the read it just finished.
        if cancel.load(Ordering::Relaxed) { return Err(IntegrationError::Cancelled); }
        for i in 0..n {
            for r in 0..rows {
                let gy = y + r;
                for x in cx0..cx1 {
                    let mut v = planes.sample(i, r * w + x) as f64;
                    match precal {
                        FlatPrecal::MasterFrame { data, width, .. } => v -= data[gy * *width + x] as f64,
                        FlatPrecal::SyntheticBias(b) => v -= *b as f64,
                        FlatPrecal::None => {}
                    }
                    // Same undefined-pixel policy as the combine pass: a single
                    // non-finite sample must not poison this frame's mean —
                    // that mean IS its normalization scale, so a NaN here would
                    // scale EVERY sample of the frame to NaN and the whole
                    // master would come out as zeros. Counting happens in pass
                    // 2 (which walks the full image), not here.
                    if !v.is_finite() { continue; }
                    sums[i] += v;
                    counts[i] += 1;
                }
            }
        }
        y += rows;
    }
    let means: Vec<f64> = sums.iter().zip(&counts).map(|(s, &c)| s / c.max(1) as f64).collect();
    for (i, m) in means.iter().enumerate() {
        if *m <= 0.0 {
            return Err(IntegrationError::BadInput(format!(
                "flat frame {} has non-positive central mean {m:.1} after pre-calibration — wrong precal master?",
                paths[i].display()
            )));
        }
    }
    // Normalize each frame to the mean of means (flux equalization).
    let target: f64 = means.iter().sum::<f64>() / n as f64;
    let scales: Vec<f32> = means.iter().map(|m| (target / m) as f32).collect();

    // Pass 2: full combine with precal + scale applied. `BandSource::read_band`
    // is positional (`&self`, `pread`-style — no cursor), so pass 1 (above)
    // and pass 2 just reuse the SAME `src`; there is nothing to seek back and
    // no need to reopen.
    //
    // `run_banded` only knows its own (pass 2) height, not pass 1's
    // central-third read that already happened above it — wrap the caller's
    // `on_band` so the byte pair it sees spans both passes (this is the
    // mechanism `EngineProgress::on_band`'s doc describes: band count stays
    // pass-2-only while the byte pair jumps ahead by pass 1's share).
    let wrapped_on_band = |cur: usize, total: usize, bytes_done: u64, bytes_total: u64| {
        (progress.on_band)(cur, total, pass1_bytes_read + bytes_done, pass1_total_bytes + bytes_total);
    };
    // Same wrapping for the combine-phase callback (fix wave item 2) — pass
    // 2's `run_banded` only knows its own bytes, so the byte pair it hands
    // `on_combine` needs pass 1's share folded in too, exactly like
    // `wrapped_on_band` above. The row pair (`cur`/`total`) needs no such
    // adjustment: `on_combine`'s rows are pass-2-only already (pass 1 has no
    // combine phase of its own to count rows for).
    let wrapped_on_combine = |cur: usize, total: usize, bytes_done: u64, bytes_total: u64| {
        (progress.on_combine)(cur, total, pass1_bytes_read + bytes_done, pass1_total_bytes + bytes_total);
    };
    let wrapped_progress = EngineProgress { on_band: &wrapped_on_band, on_combine: &wrapped_on_combine };
    let mut out = run_banded(&src, &scales, Some(precal), recipe, pool, cancel, &wrapped_progress, io)?;
    out.flat_norm = Some(central_third_mean(&out.data, w, h));
    out.read_duration += pass1_read;
    out.bytes_read += pass1_bytes_read;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::write_fits_f32;
    use crate::integration::band_budget::MIN_BUDGET_BYTES;
    use crate::integration::banded::PlaneKind;
    use crate::integration::combine::{IntegrationRecipe, Rejection};
    use crate::integration::storage_class::StorageClass;
    use std::sync::atomic::AtomicBool;

    fn pool() -> rayon::ThreadPool {
        rayon::ThreadPoolBuilder::new().num_threads(2).build().unwrap()
    }
    fn nop() -> impl Fn(usize, usize, u64, u64) { |_, _, _, _| {} }

    /// Every engine test cares about the memory budget only; concurrency and
    /// storage class are Task 6's concern, so this fixes them to an arbitrary
    /// valid value. `Local` (the un-gated default) — most of this module's
    /// tests pin the pre-tier-1 serial shape, which is exactly what a local
    /// build runs since the fix wave gated the prefetch on network storage.
    fn io(band_budget_bytes: usize) -> IoPolicy {
        IoPolicy { band_budget_bytes, read_concurrency: 1, storage: StorageClass::Local }
    }

    /// Same as `io`, but `Network` — for the tests that specifically pin the
    /// band-prefetch/double-buffer behaviour, which the fix wave gated on
    /// network storage (`loop_band_rows`'s doc).
    fn io_network(band_budget_bytes: usize) -> IoPolicy {
        IoPolicy { band_budget_bytes, read_concurrency: 1, storage: StorageClass::Network }
    }

    fn write(dir: &std::path::Path, name: &str, w: usize, h: usize, f: impl Fn(usize, usize) -> f32) -> std::path::PathBuf {
        let mut d = vec![0f32; w * h];
        for y in 0..h { for x in 0..w { d[y * w + x] = f(x, y); } }
        let p = dir.join(name);
        write_fits_f32(&p, w, h, 1, &d, &[]).unwrap();
        p
    }

    #[test]
    fn dark_master_is_mean_with_outlier_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (48, 33); // non-multiple of band size on purpose
        let mut paths: Vec<_> = (0..16)
            .map(|i| write(dir.path(), &format!("d{i}.fits"), w, h, |_, _| 100.0 + (i % 4) as f32))
            .collect();
        // one frame with a hot pixel at (5,5)
        paths.push(write(dir.path(), "hot.fits", w, h, |x, y| if (x, y) == (5, 5) { 9000.0 } else { 101.0 }));
        let on_band = nop();
        let out = integrate_bias_like(
            &paths,
            IntegrationRecipe::average(Rejection::WinsorizedSigma { sigma_low: 3.0, sigma_high: 3.0 }),
            &pool(), dir.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &on_band, on_combine: &nop() },
            io(MIN_BUDGET_BYTES),
        ).unwrap();
        assert_eq!((out.width, out.height), (w, h));
        let hot = out.data[5 * w + 5];
        assert!(hot < 200.0, "hot pixel must be rejected, got {hot}");
        assert!(out.rejected_fraction > 0.0);
        assert!(out.flat_norm.is_none());
    }

    /// The master path's only BIT-EXACT Winsorized pin (M4c Task 2, rulings
    /// R-M4c-3 and R-T2-1 — the tree had none, which is why the reference
    /// loop could be introduced without a single fingerprint moving).
    ///
    /// 16 frames of 1000 ADU — the `resolve_recipe` default for n >= 15 is
    /// exactly this recipe — with three planted pixels:
    ///
    /// - a cosmic ray (9000 in one frame) and a dead sample (200 in another)
    ///   on stacks whose other 15 samples are TIED. Their MAD is 0, so
    ///   without the zero-MAD fallback of ruling R-T2-1 the rejection is off
    ///   and these come out at 1500.0 and 950.0 — the regression this pin
    ///   exists to catch;
    /// - a spread pixel (1000..1004, three frames each) with a mild outlier
    ///   at 1100 in the last frame, so the pin also covers a stack the
    ///   fallback must NOT touch.
    ///
    /// Every value below is exact in f32: the survivors are integers and
    /// their means land on integers.
    #[test]
    fn winsorized_master_is_bit_exact_on_a_planted_integer_stack() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8usize, 4usize);
        let mut paths = Vec::new();
        for i in 0..16usize {
            let mut data = vec![1000.0f32; w * h];
            data[0] = if i < 15 { 1000.0 + (i % 5) as f32 } else { 1100.0 };
            if i == 3 { data[w + 2] = 9000.0; }
            if i == 7 { data[2 * w + 5] = 200.0; }
            let p = dir.path().join(format!("w{i:02}.fits"));
            write_fits_f32(&p, w, h, 1, &data, &[]).unwrap();
            paths.push(p);
        }
        let out = integrate_bias_like(
            &paths,
            IntegrationRecipe::average(Rejection::WinsorizedSigma { sigma_low: 3.0, sigma_high: 3.0 }),
            &pool(), dir.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &nop(), on_combine: &nop() },
            io(MIN_BUDGET_BYTES),
        ).unwrap();
        let px = |x: usize, y: usize| out.data[y * w + x];
        assert_eq!(px(2, 1).to_bits(), 1000.0f32.to_bits(),
            "cosmic ray on a majority-tied stack: got {} (1500.0 = the zero-MAD hole)", px(2, 1));
        assert_eq!(px(5, 2).to_bits(), 1000.0f32.to_bits(),
            "dead sample on a majority-tied stack: got {} (950.0 = the zero-MAD hole)", px(5, 2));
        assert_eq!(px(0, 0).to_bits(), 1002.0f32.to_bits(),
            "spread pixel: the 15 survivors average 1002 exactly, got {}", px(0, 0));
        for y in 0..h {
            for x in 0..w {
                if (x, y) == (2, 1) || (x, y) == (5, 2) || (x, y) == (0, 0) { continue; }
                assert_eq!(px(x, y).to_bits(), 1000.0f32.to_bits(), "untouched pixel ({x},{y})");
            }
        }
        // Three algorithm rejections out of 16 x 32 samples.
        assert_eq!(out.rejected_fraction, 3.0 / 512.0);
        assert!(out.flat_norm.is_none());
    }

    #[test]
    fn flat_normalization_equalizes_exposure_drift() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (30, 30);
        // Same vignetting shape, different levels (sky brightness drift 1x/2x/4x).
        let shape = |x: usize, _y: usize| 1000.0 + (x as f32) * 10.0;
        let paths = vec![
            write(dir.path(), "f1.fits", w, h, |x, y| shape(x, y)),
            write(dir.path(), "f2.fits", w, h, |x, y| shape(x, y) * 2.0),
            write(dir.path(), "f3.fits", w, h, |x, y| shape(x, y) * 4.0),
        ];
        let on_band = nop();
        let out = integrate_flat(
            &paths, &FlatPrecal::None, IntegrationRecipe::median(Rejection::None),
            &pool(), dir.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &on_band, on_combine: &nop() },
            io(MIN_BUDGET_BYTES),
        ).unwrap();
        // After per-frame normalization all three frames agree, so the master
        // must reproduce the SHAPE: ratio of two positions equals shape ratio.
        let a = out.data[15 * w + 5];
        let b = out.data[15 * w + 25];
        let expect = shape(5, 15) / shape(25, 15);
        assert!(((a / b) - expect).abs() < 0.01, "shape preserved: {} vs {expect}", a / b);
        let fnorm = out.flat_norm.expect("flats carry flat_norm");
        assert!((fnorm - central_third_mean(&out.data, w, h)).abs() < 1e-6);
    }

    #[test]
    fn flat_precal_subtracts_master() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (24, 24);
        let paths = vec![
            write(dir.path(), "f1.fits", w, h, |_, _| 1500.0),
            write(dir.path(), "f2.fits", w, h, |_, _| 1500.0),
            write(dir.path(), "f3.fits", w, h, |_, _| 1500.0),
        ];
        let precal = FlatPrecal::MasterFrame { data: vec![500.0; w * h], width: w, height: h };
        let on_band = nop();
        let out = integrate_flat(
            &paths, &precal, IntegrationRecipe::median(Rejection::None),
            &pool(), dir.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &on_band, on_combine: &nop() },
            io(MIN_BUDGET_BYTES),
        ).unwrap();
        assert!(out.data.iter().all(|&v| (v - 1000.0).abs() < 0.01),
            "1500 - 500 precal = 1000 everywhere");
    }

    #[test]
    fn synthetic_bias_constant() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (16, 16);
        let paths = vec![
            write(dir.path(), "f1.fits", w, h, |_, _| 1100.0),
            write(dir.path(), "f2.fits", w, h, |_, _| 1100.0),
            write(dir.path(), "f3.fits", w, h, |_, _| 1100.0),
        ];
        let on_band = nop();
        let out = integrate_flat(
            &paths, &FlatPrecal::SyntheticBias(100.0), IntegrationRecipe::median(Rejection::None),
            &pool(), dir.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &on_band, on_combine: &nop() },
            io(MIN_BUDGET_BYTES),
        ).unwrap();
        assert!(out.data.iter().all(|&v| (v - 1000.0).abs() < 0.01));
    }

    #[test]
    fn cancel_mid_run_returns_cancelled() {
        let dir = tempfile::tempdir().unwrap();
        let paths: Vec<_> = (0..4).map(|i| write(dir.path(), &format!("c{i}.fits"), 64, 256, |_, _| 1.0)).collect();
        let cancel = AtomicBool::new(true); // pre-set: first band check trips
        let on_band = nop();
        let r = integrate_bias_like(
            &paths, IntegrationRecipe::average(Rejection::None), &pool(), dir.path(), &cancel,
            EngineProgress { on_band: &on_band, on_combine: &nop() },
            io(MIN_BUDGET_BYTES),
        );
        assert!(matches!(r, Err(IntegrationError::Cancelled)));
    }

    #[test]
    fn negatives_pass_through_unclipped() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8, 8);
        let paths = vec![
            write(dir.path(), "n1.fits", w, h, |_, _| -5.0),
            write(dir.path(), "n2.fits", w, h, |_, _| -5.0),
            write(dir.path(), "n3.fits", w, h, |_, _| -5.0),
        ];
        let on_band = nop();
        let out = integrate_bias_like(
            &paths, IntegrationRecipe::average(Rejection::None), &pool(), dir.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &on_band, on_combine: &nop() },
            io(MIN_BUDGET_BYTES),
        ).unwrap();
        assert!(out.data.iter().all(|&v| v == -5.0), "no clipping policy");
    }

    /// Multi-band precal row indexing: with band_budget_bytes=1 the budget
    /// floors to 1-row bands (the 2026-08-02 audit deleted the old 16-row
    /// floor), so h=48 really runs as 48 bands — every band but the first has
    /// `row_in_band` pinned at 0, which is a stronger check than a 3-band run
    /// gives. The master is a row gradient (master[y] = y), the flats are
    /// 1000 + y, so after subtraction every sample is exactly 1000.0 — but
    /// ONLY if the MasterFrame index uses the GLOBAL row
    /// (`gy = y0 + row_in_band`). A regression that drops `y0` reads
    /// `master[row_in_band]` instead, which is `master[0] = 0` for every
    /// band past the first — every output row but row 0 comes out as
    /// `1000 + y` instead of `1000`, and this test catches it while every
    /// single-band test still passes.
    #[test]
    fn multi_band_precal_uses_global_row_index() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (32, 48);
        let paths = vec![
            write(dir.path(), "f1.fits", w, h, |_, y| 1000.0 + y as f32),
            write(dir.path(), "f2.fits", w, h, |_, y| 1000.0 + y as f32),
            write(dir.path(), "f3.fits", w, h, |_, y| 1000.0 + y as f32),
        ];
        let mut master = vec![0f32; w * h];
        for y in 0..h { for x in 0..w { master[y * w + x] = y as f32; } }
        let precal = FlatPrecal::MasterFrame { data: master, width: w, height: h };
        let on_band = nop();
        let out = integrate_flat_inner(
            &paths, &precal, IntegrationRecipe::median(Rejection::None),
            &pool(), dir.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &on_band, on_combine: &nop() },
            io(1), // band_rows_for_budget(1) floors to 1 row/band => 48 bands
        ).unwrap();
        for (i, &v) in out.data.iter().enumerate() {
            assert!(
                (v - 1000.0).abs() < 1e-3,
                "pixel {i} (row {}): got {v}, want 1000.0 — precal master row index broken past band 1",
                i / w
            );
        }
        let fnorm = out.flat_norm.expect("flats carry flat_norm");
        assert!((fnorm - 1000.0).abs() < 1e-3, "flat_norm {fnorm} != 1000.0");
    }

    /// Composition order at non-unity scales: normalization must be
    /// (v - precal) * scale, not (v * scale) - precal. Post-subtraction means
    /// are 1000/2000/1500 → target 1500 → scales 1.5/0.75/1.0. Correct math
    /// gives every normalized sample = 1500.0; the swapped order gives
    /// 1750/1375/1500 whose MEAN is 1541.67 (Median would NOT discriminate —
    /// median of {1375,1500,1750} is 1500 — hence Mean here).
    #[test]
    fn precal_applies_before_scale() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (16, 16);
        let paths = vec![
            write(dir.path(), "f1.fits", w, h, |_, _| 1500.0),
            write(dir.path(), "f2.fits", w, h, |_, _| 2500.0),
            write(dir.path(), "f3.fits", w, h, |_, _| 2000.0),
        ];
        let on_band = nop();
        let out = integrate_flat_inner(
            &paths, &FlatPrecal::SyntheticBias(500.0), IntegrationRecipe::average(Rejection::None),
            &pool(), dir.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &on_band, on_combine: &nop() },
            io(MIN_BUDGET_BYTES),
        ).unwrap();
        assert!(
            out.data.iter().all(|&v| (v - 1500.0).abs() < 1e-3),
            "every normalized sample must be (v - 500) * scale = 1500; got {}",
            out.data[0]
        );
    }

    /// Audit C2: a single NaN/Inf sample used to either PANIC the build
    /// (`f64::clamp` with NaN bounds inside the winsorized estimator — the
    /// Auto recipe for N>=15) or bake a non-finite value into the master.
    /// Policy: non-finite samples are FITS "undefined pixels" — dropped from
    /// the per-pixel stack with accounting, never a build failure.
    #[test]
    fn non_finite_samples_are_excluded_not_propagated() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (4usize, 4usize);
        let mut paths = Vec::new();
        for i in 0..16 {
            let mut data = vec![100.0f32; w * h];
            if i == 3 { data[5] = f32::NAN; }   // one bad sample in ONE frame
            data[9] = f32::INFINITY;            // pixel 9: bad in EVERY frame
            let p = dir.path().join(format!("f{i}.fits"));
            write_fits_f32(&p, w, h, 1, &data, &[]).unwrap();
            paths.push(p);
        }
        let on_band = nop();
        let out = integrate_bias_like(
            &paths,
            IntegrationRecipe::average(Rejection::WinsorizedSigma { sigma_low: 3.0, sigma_high: 3.0 }),
            &pool(), dir.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &on_band, on_combine: &nop() },
            io(MIN_BUDGET_BYTES),
        ).unwrap();
        assert!(out.data.iter().all(|v| v.is_finite()), "master must never contain non-finite pixels");
        assert!((out.data[5] - 100.0).abs() < 1e-3, "pixel 5 combines the 15 good samples");
        assert_eq!(out.data[9], 0.0, "all-bad pixel becomes 0");
        assert_eq!(out.bad_samples_per_frame[3], 2, "frame 3: its own NaN + the shared Inf pixel");
        assert_eq!(out.bad_samples_per_frame[0], 1, "every other frame: just the shared Inf pixel");
        assert_eq!(out.all_bad_pixels, 1);
    }

    /// The flat path's pass 1 (per-frame central-third mean, the source of the
    /// normalization scale) must skip non-finite samples too — otherwise ONE
    /// NaN poisons that frame's mean, hence its scale, hence EVERY one of its
    /// samples, and the "exclude with accounting" policy degenerates into an
    /// all-zero master. The NaN here sits inside the central third on purpose.
    #[test]
    fn flat_non_finite_sample_does_not_poison_the_frame_scale() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (24usize, 24usize);
        let paths = vec![
            write(dir.path(), "f1.fits", w, h, |_, _| 1000.0),
            write(dir.path(), "f2.fits", w, h, |x, y| if (x, y) == (12, 12) { f32::NAN } else { 1000.0 }),
            write(dir.path(), "f3.fits", w, h, |_, _| 1000.0),
        ];
        let on_band = nop();
        let out = integrate_flat(
            &paths, &FlatPrecal::None, IntegrationRecipe::median(Rejection::None),
            &pool(), dir.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &on_band, on_combine: &nop() },
            io(MIN_BUDGET_BYTES),
        ).unwrap();
        assert!(out.data.iter().all(|v| v.is_finite()));
        assert!(out.data.iter().all(|&v| (v - 1000.0).abs() < 1e-3),
            "flat master stays at the (equal) frame level; got {}", out.data[0]);
        assert_eq!(out.bad_samples_per_frame, vec![0, 1, 0]);
        assert_eq!(out.all_bad_pixels, 0, "the pixel still has 2 valid samples");
        assert!(out.flat_norm.is_some_and(|n| (n - 1000.0).abs() < 1e-3));
    }

    /// The harness and, from Task 7, the build's completion log line report
    /// where the time went. Both numbers come out of the engine because only
    /// it can separate the two phases.
    #[test]
    fn integration_output_reports_read_and_combine_time() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (32, 48);
        let paths = vec![
            write(dir.path(), "t1.fits", w, h, |_, _| 10.0),
            write(dir.path(), "t2.fits", w, h, |_, _| 20.0),
            write(dir.path(), "t3.fits", w, h, |_, _| 30.0),
        ];
        let on_band = nop();
        let out = integrate_bias_like(
            &paths,
            IntegrationRecipe::median(Rejection::None),
            &pool(),
            dir.path(),
            &AtomicBool::new(false),
            EngineProgress { on_band: &on_band, on_combine: &nop() },
            io(MIN_BUDGET_BYTES),
        )
        .unwrap();
        assert!(out.read_duration > std::time::Duration::ZERO, "read time not recorded");
        assert!(out.combine_duration > std::time::Duration::ZERO, "combine time not recorded");
        assert!(out.data.iter().all(|&v| v == 20.0), "median unchanged by instrumentation");
    }

    /// Review fix-round-1 finding I1: `band_rows`/`bands`/`bytes_read` were
    /// never asserted against a real number, only `read_duration` and
    /// `combine_duration` were. h=48 is deliberately NOT a multiple of the
    /// forced 20-row band size, so the run is 20+20+8 rows — a naive
    /// `bytes_read` accumulator that used the nominal `band_rows` for every
    /// band (instead of the band's actual, possibly-short, row count) would
    /// overcount by exactly the shortfall on the last band and this test
    /// would catch it.
    #[test]
    fn bias_like_reports_exact_band_geometry_and_bytes_across_a_short_last_band() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (32, 48);
        let paths = vec![
            write(dir.path(), "b1.fits", w, h, |_, _| 10.0),
            write(dir.path(), "b2.fits", w, h, |_, _| 20.0),
            write(dir.path(), "b3.fits", w, h, |_, _| 30.0),
        ];
        let on_band = nop();
        // band_rows_for_budget's per-row cost is sum(width * bytes_per_sample
        // over every frame) + width*8 headroom = 3*32*4 + 32*8 = 384 + 256 =
        // 640; budget 12_800 -> band_rows = 12_800/640 = 20 exactly, so the
        // 48-row image runs as 20/20/8-row bands. `io()` is `Local`, so the
        // fix wave's network-only gate leaves this budget un-halved.
        let out = integrate_bias_like(
            &paths,
            IntegrationRecipe::median(Rejection::None),
            &pool(),
            dir.path(),
            &AtomicBool::new(false),
            EngineProgress { on_band: &on_band, on_combine: &nop() },
            io(12_800),
        )
        .unwrap();
        assert_eq!(out.band_rows, 20, "budget math must give exactly 20-row bands here");
        assert_eq!(out.bands, 48usize.div_ceil(out.band_rows), "3 bands: 20 + 20 + 8");
        assert_eq!(
            out.bytes_read,
            (48 * 32 * 4 * 3) as u64,
            "the short last band (8 rows) must be counted at its real length, not the nominal 20 \
             (3 frames, f32 source, 4 bytes/sample)"
        );
    }

    /// Review fix-round-1 finding I1 (judgment call #1): `bytes_read` for a
    /// flat must fold in pass 1's central-third read on top of pass 2's full
    /// height — pinned numerically, not just by field presence. cy0 = h/3 =
    /// 16, cy1 = ((2*h)/3).max(h/3+1).min(h) = 32, so pass 1 reads exactly
    /// 16 rows; pass 2 (`run_banded`) reads the full 48.
    #[test]
    fn flat_bytes_read_includes_pass_one_central_third_plus_pass_two_full_height() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (32, 48);
        let paths = vec![
            write(dir.path(), "f1.fits", w, h, |_, _| 1000.0),
            write(dir.path(), "f2.fits", w, h, |_, _| 1000.0),
            write(dir.path(), "f3.fits", w, h, |_, _| 1000.0),
        ];
        let on_band = nop();
        let out = integrate_flat_inner(
            &paths,
            &FlatPrecal::None,
            IntegrationRecipe::median(Rejection::None),
            &pool(),
            dir.path(),
            &AtomicBool::new(false),
            EngineProgress { on_band: &on_band, on_combine: &nop() },
            io(MIN_BUDGET_BYTES),
        )
        .unwrap();
        assert_eq!(
            out.bytes_read,
            ((48 + 16) * 32 * 4 * 3) as u64,
            "pass 1 (16 central rows) + pass 2 (48 full rows), 3 frames, 4 bytes/sample"
        );
    }

    /// Fix round 3, Important 1: `fetch_add` is atomic, but the EMISSION
    /// that follows it is not ordered against it — two workers can both
    /// complete their add and then race into `on_band` in the opposite
    /// order, so a caller sees `bytes_done` go backwards. `read_concurrency
    /// > 1` is every real build (`io.read_concurrency` never comes back as 1
    /// outside a 1-3-frame precal/light-cal read with no `IoPolicy` in
    /// scope); the module's shared `io()` helper hardcodes
    /// `read_concurrency: 1` precisely so every OTHER test in this module
    /// stays on the deterministic single-thread fast path — which is
    /// exactly why this bug shipped with no test noticing it. This test
    /// deliberately builds its own `IoPolicy` instead of using `io()`.
    #[test]
    fn on_band_bytes_done_never_regresses_under_real_concurrency() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (4, 4);
        let n = 64;
        let paths: Vec<_> = (0..n)
            .map(|i| write(dir.path(), &format!("f{i}.fits"), w, h, |_, _| 1000.0))
            .collect();
        let calls: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());
        let on_band = |_cur: usize, _total: usize, done: u64, _all: u64| {
            calls.lock().unwrap().push(done);
        };
        let concurrent_io = IoPolicy {
            // One band for this tiny image — every tick below belongs to
            // it, isolating the concurrency race from any band-boundary
            // effect.
            band_budget_bytes: MIN_BUDGET_BYTES,
            read_concurrency: 16,
            storage: StorageClass::Local,
        };
        integrate_bias_like_inner(
            &paths,
            IntegrationRecipe::median(Rejection::None),
            &pool(),
            dir.path(),
            &AtomicBool::new(false),
            EngineProgress { on_band: &on_band, on_combine: &nop() },
            concurrent_io,
        )
        .unwrap();
        let calls = calls.into_inner().unwrap();
        assert!(calls.len() >= 2, "expected multiple ticks, got {calls:?}");
        for pair in calls.windows(2) {
            assert!(
                pair[1] >= pair[0],
                "bytes_done regressed under concurrency: {} then {} in the sequence {calls:?}",
                pair[0], pair[1]
            );
        }
    }

    /// Fix round 1, Important finding 2: the pre-fix combine tick read
    /// `bytes_reported` and emitted OUTSIDE the lock (lock, copy, unlock,
    /// THEN call `on_combine`) — two rayon workers ticking concurrently,
    /// with the next band's prefetch reader advancing the counter between
    /// their two reads, could emit out of order (worker A reads a small
    /// value, the counter advances, worker B reads a larger value and wins
    /// the race to call `on_combine` FIRST, A's smaller value lands after).
    ///
    /// A real-file, real-thread-pool version of this pin (many frames,
    /// `read_concurrency: 16`, multi-band, shaped exactly like the sibling
    /// `on_band` pin above) was tried first and did not reproduce the race:
    /// over 35 runs against both the buggy and the fixed code it always
    /// passed, because on this machine the prefetch's own read finishes
    /// long before a band's combine crosses even one 64-row checkpoint, so
    /// `bytes_reported` has already settled to its final value by the time
    /// any tick reads it — nothing left to race over. Reproducing the
    /// actual bug needs the prefetch to still be mid-flight AND two ticks
    /// racing to read/emit while it is, which real I/O on tiny fixture
    /// files can't reliably arrange. This calls `band_loop` (private, same
    /// module) directly against two test doubles that engineer exactly
    /// that window: `TwoStepPrefetchSource::read_band_with_progress`
    /// reports a prefetch's bytes in two chunks with a real sleep between
    /// them (so `bytes_reported` visibly climbs mid-combine), and the
    /// `combine` closure races two threads through 64 `tick()` calls each
    /// — thread A claims the FIRST 64 (checkpoint 64, reading
    /// `bytes_reported` before the prefetch's second chunk lands) then
    /// SLEEPS before returning; thread B starts later, claims the NEXT 64
    /// (checkpoint 128, reading a larger `bytes_reported`) and returns
    /// immediately — so under the pre-fix code B's larger value is pushed
    /// to `calls` BEFORE A's smaller, delayed one, exactly the observed
    /// regression. Verified RED against the pre-fix tick (temporarily
    /// reverted, byte-identical to the code before this fix round) and
    /// GREEN after — see the fix-round report.
    #[test]
    fn on_combine_bytes_done_never_regresses_under_real_concurrency() {
        struct TwoStepPrefetchSource {
            width: usize,
            height: usize,
        }
        impl FrameSource for TwoStepPrefetchSource {
            fn width(&self) -> usize { self.width }
            fn height(&self) -> usize { self.height }
            fn frame_count(&self) -> usize { 1 }
            fn plane_kinds(&self) -> Vec<PlaneKind> { vec![PlaneKind::F32Le] }
            fn bytes_per_row(&self) -> usize { self.width * 4 }
            fn band_rows_for_budget(&self, budget_bytes: usize) -> usize {
                (budget_bytes / (self.width * 4).max(1)).max(1)
            }
            fn read_band_with_progress(
                &self,
                y0: usize,
                rows: usize,
                out: &mut BandPlanes,
                _concurrency: usize,
                on_bytes: &(dyn Fn(u64) + Sync),
                _cancel: &AtomicBool,
            ) -> Result<(), IntegrationError> {
                out.set_rows(rows);
                let total = (rows * self.width * 4) as u64;
                if y0 == 0 {
                    // The pre-loop read of band 0: instant, no delay.
                    on_bytes(total);
                } else {
                    // Every later band's prefetch: a short delay before
                    // ANY progress (so the checkpoint that starts first
                    // below reads the untouched baseline), then report
                    // half, then a longer delay before the rest — a real
                    // in-flight read the racing ticks below can observe
                    // at three different stages (baseline / half / full).
                    std::thread::sleep(std::time::Duration::from_millis(10));
                    let half = total / 2;
                    on_bytes(half);
                    std::thread::sleep(std::time::Duration::from_millis(80));
                    on_bytes(total - half);
                }
                Ok(())
            }
        }

        let (w, h) = (8usize, 128usize);
        let src = TwoStepPrefetchSource { width: w, height: h };
        // Halves to 64-row bands (`band_rows_for_budget` above), 2 bands:
        // band 0 [0,64), band 1 [64,128) — band 0's combine (below) drives
        // the tick counter through BOTH checkpoints (64 and 128) itself,
        // concurrently with band 1's two-step prefetch.
        // This test exercises the prefetch itself (band 1's read racing
        // band 0's combine), so it needs the fix wave's gate open —
        // `Network`, not the module's default `Local`.
        let policy = IoPolicy {
            band_budget_bytes: 2 * (w * 4 * 64),
            read_concurrency: 1,
            storage: StorageClass::Network,
        };
        let mut out = vec![0f32; w * h];
        let calls: std::sync::Mutex<Vec<u64>> = std::sync::Mutex::new(Vec::new());
        let on_combine = |rows_done: usize, _total: usize, bytes_done: u64, _all: u64| {
            if rows_done == 64 {
                // The pre-fix bug: reads `bytes_reported` early (before
                // the prefetch's second chunk lands), then — in the buggy
                // code — calls back OUTSIDE the lock. Sleeping here plays
                // the part of a slow caller, giving the LATER checkpoint
                // (128) time to read a bigger value and push first.
                std::thread::sleep(std::time::Duration::from_millis(150));
            }
            calls.lock().unwrap().push(bytes_done);
        };
        let combine = |job: BandJob<'_>, tick: &(dyn Fn() + Sync)| -> Result<(), IntegrationError> {
            for v in job.out_band.iter_mut() {
                *v = 0.0;
            }
            if job.y0 == 0 {
                std::thread::scope(|scope| {
                    // Claims the first 64 tick() calls (checkpoint 64):
                    // starts immediately, so it wins the race to the low
                    // end of the shared counter.
                    scope.spawn(|| {
                        for _ in 0..64 {
                            tick();
                        }
                    });
                    // Claims the next 64 (checkpoint 128): starts late
                    // enough that the first thread has already claimed
                    // 1..=64, and late enough that the prefetch's FIRST
                    // chunk has landed but not its second.
                    scope.spawn(|| {
                        std::thread::sleep(std::time::Duration::from_millis(20));
                        for _ in 0..64 {
                            tick();
                        }
                    });
                });
            }
            Ok(())
        };
        band_loop(
            &src,
            &pool(),
            &AtomicBool::new(false),
            &EngineProgress { on_band: &nop(), on_combine: &on_combine },
            policy,
            &mut out,
            &combine,
        )
        .unwrap();
        let calls = calls.into_inner().unwrap();
        assert_eq!(calls.len(), 2, "expected exactly 2 on_combine ticks (64, 128), got {calls:?}");
        for pair in calls.windows(2) {
            assert!(
                pair[1] >= pair[0],
                "on_combine bytes_done regressed under concurrency: {} then {} in the sequence {calls:?}",
                pair[0], pair[1]
            );
        }
    }

    /// Fix round 2, I2: Task 6 cut a set's band count to as few as 2, and
    /// `on_band` used to fire only once per band END — a caller watching for
    /// progress during a single, possibly multi-minute band saw nothing at
    /// all until it finished. `read_band_with_progress` now ticks once per
    /// FRAME as it is read, in addition to that existing end-of-band call.
    #[test]
    fn on_band_ticks_once_per_frame_not_just_once_per_band() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8, 8);
        let paths: Vec<_> = (0..5)
            .map(|i| write(dir.path(), &format!("f{i}.fits"), w, h, |_, _| 1000.0))
            .collect();
        let calls: std::sync::Mutex<Vec<(usize, usize, u64, u64)>> = std::sync::Mutex::new(Vec::new());
        let on_band = |cur: usize, total: usize, done: u64, all: u64| {
            calls.lock().unwrap().push((cur, total, done, all));
        };
        integrate_bias_like_inner(
            &paths,
            IntegrationRecipe::median(Rejection::None),
            &pool(),
            dir.path(),
            &AtomicBool::new(false),
            EngineProgress { on_band: &on_band, on_combine: &nop() },
            // Budget far exceeds this tiny image: exactly one band results,
            // so every call below belongs to band 1 of 1 — isolating the
            // per-frame ticks from any band-boundary effect.
            io(MIN_BUDGET_BYTES),
        )
        .unwrap();
        let calls = calls.into_inner().unwrap();
        // 5 per-frame ticks (during the read) + 1 end-of-band call.
        assert_eq!(
            calls.len(), 6,
            "expected one tick per frame plus the end-of-band call, got {calls:?}"
        );
        assert!(
            calls.iter().all(|&(cur, total, _, _)| cur == 1 && total == 1),
            "this image fits one band — every call must report band 1 of 1: {calls:?}"
        );
        let bytes_total = calls[0].3;
        assert!(bytes_total > 0);
        for pair in calls.windows(2) {
            assert!(
                pair[1].2 >= pair[0].2,
                "bytes_done must never regress: {:?} then {:?}", pair[0], pair[1]
            );
        }
        assert_eq!(
            calls.last().unwrap().2, bytes_total,
            "the final call (end of band) must reach the full total"
        );
    }

    /// Review fix-round-1 finding I1 (judgment call #2): the `on_band`
    /// wrapper in `integrate_flat_inner` must report a byte pair that spans
    /// BOTH passes, not just pass 2's own share — `run_banded` has no way to
    /// see pass 1's read on its own. Forces a multi-band pass 2
    /// (`band_budget_bytes = 1` clamps to 1-row bands, so `on_band` fires
    /// many times) so the wrapping is exercised across more than a single
    /// call.
    ///
    /// Fix round 2, I2: pass 1 now ALSO calls `on_band` directly — bypassing
    /// the wrapper entirely, since it computes the two-pass total itself and
    /// needs no baseline added — so `calls[0]` is no longer guaranteed to be
    /// pass 2's first band. The old "first call already carries pass 1's
    /// bytes as a baseline" assertion is replaced with the properties that
    /// actually matter now: every call agrees on the two-pass total, bytes
    /// never regress, at least one call lands DURING pass 1 (proving it is
    /// no longer silent), and the run ends at the full total.
    #[test]
    fn flat_progress_ticks_span_both_passes_with_one_shared_total() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (32, 48);
        let paths = vec![
            write(dir.path(), "f1.fits", w, h, |_, _| 1000.0),
            write(dir.path(), "f2.fits", w, h, |_, _| 1000.0),
            write(dir.path(), "f3.fits", w, h, |_, _| 1000.0),
        ];
        let calls: std::sync::Mutex<Vec<(usize, usize, u64, u64)>> = std::sync::Mutex::new(Vec::new());
        let on_band = |cur: usize, total: usize, done: u64, all: u64| {
            calls.lock().unwrap().push((cur, total, done, all));
        };
        integrate_flat_inner(
            &paths,
            &FlatPrecal::None,
            IntegrationRecipe::median(Rejection::None),
            &pool(),
            dir.path(),
            &AtomicBool::new(false),
            EngineProgress { on_band: &on_band, on_combine: &nop() },
            io(1),
        )
        .unwrap();
        let calls = calls.into_inner().unwrap();
        assert!(!calls.is_empty(), "on_band must fire at least once");
        // Pass 1 (16 rows) + pass 2 (48 rows), 3 frames, 4 bytes/sample —
        // same total the previous test pins on the returned field.
        let expected_total = ((48 + 16) * 32 * 4 * 3) as u64;
        for &(_, _, _done, all) in &calls {
            assert_eq!(all, expected_total, "bytes_total reported to on_band must span both passes, every call");
        }
        let (_, _, last_done, _) = *calls.last().unwrap();
        assert_eq!(
            last_done, expected_total,
            "after the final band, bytes_read_so_far must equal the full two-pass total"
        );
        // Pass 1 is no longer silent (fix round 2, I2): at least one call
        // must land strictly before pass 1's own share is fully read,
        // pinned at band_index 0 ("no pass-2 band reached yet").
        let pass1_bytes = 16u64 * 32 * 4 * 3;
        assert!(
            calls.iter().any(|&(cur, _, done, _)| cur == 0 && done < pass1_bytes),
            "expected at least one pass-1 tick (band_index 0) reporting partial \
             progress before pass 1's {pass1_bytes} bytes are fully read; got {calls:?}"
        );
        // `io(1)` clamps concurrency to 1, so this whole run is one
        // sequential thread — bytes_done must never regress tick to tick.
        for pair in calls.windows(2) {
            assert!(
                pair[1].2 >= pair[0].2,
                "bytes_done must never regress across ticks: {:?} then {:?}", pair[0], pair[1]
            );
        }
    }

    /// Fix round 1, Important finding 1: pass 1's forecast band total (the
    /// `total` it announces via `on_band` calls tagged `cur == 0`) must
    /// equal pass 2's ACTUAL band total (the `total` `band_loop` itself
    /// reports on `cur >= 1` calls) — both must come from `loop_band_rows`,
    /// the one function shared since this fix. `io(1)` (used by the sibling
    /// test above) floors both to a single row either way, which is exactly
    /// what hid the pre-fix drift; this uses `io_network` (fix wave: the
    /// halving `loop_band_rows` applies is now gated on `StorageClass::
    /// Network`, and this test exists specifically to pin that the forecast
    /// and `band_loop` never drift apart when it fires) with the SAME
    /// multi-band budget the geometry pin two tests up already established
    /// gives 3 bands (20+20+8 rows) post-halving.
    #[test]
    fn flat_pass1_forecast_matches_pass2_actual_band_total() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (32, 48);
        let paths = vec![
            write(dir.path(), "f1.fits", w, h, |_, _| 1000.0),
            write(dir.path(), "f2.fits", w, h, |_, _| 1000.0),
            write(dir.path(), "f3.fits", w, h, |_, _| 1000.0),
        ];
        let calls: std::sync::Mutex<Vec<(usize, usize)>> = std::sync::Mutex::new(Vec::new());
        let on_band = |cur: usize, total: usize, _done: u64, _all: u64| {
            calls.lock().unwrap().push((cur, total));
        };
        integrate_flat_inner(
            &paths,
            &FlatPrecal::None,
            IntegrationRecipe::median(Rejection::None),
            &pool(),
            dir.path(),
            &AtomicBool::new(false),
            EngineProgress { on_band: &on_band, on_combine: &nop() },
            io_network(25_600), // -> pass 2 band_rows 20 (post-halving), 3 bands: 20+20+8
        )
        .unwrap();
        let calls = calls.into_inner().unwrap();
        let pass1_total = calls
            .iter()
            .find(|&&(cur, _)| cur == 0)
            .map(|&(_, total)| total)
            .expect("expected at least one pass-1 tick (cur == 0)");
        let pass2_total = calls
            .iter()
            .find(|&&(cur, _)| cur >= 1)
            .map(|&(_, total)| total)
            .expect("expected at least one pass-2 tick (cur >= 1)");
        assert_eq!(
            pass1_total, pass2_total,
            "pass 1's forecast band total must equal pass 2's actual band total \
             (pre-fix: the forecast read the un-halved budget, band_loop the halved one)"
        );
        assert_eq!(pass2_total, 3, "sanity: this budget yields 3 bands for pass 2 (20+20+8 rows)");
    }

    #[test]
    fn stack_path_weights_normalizes_and_counts_rejections_per_side_and_frame() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (16usize, 8usize);
        // Four frames: 0 and 1 flat 0.20; 2 flat 0.40 (twice the level — the
        // output pair maps it back onto 0.20); 3 flat 0.20 with a hot pixel
        // 0.95 at (3,2) and a dead pixel 0.0 at (5,5) (range-low).
        let paths = vec![
            write(dir.path(), "a.fits", w, h, |_, _| 0.20),
            write(dir.path(), "b.fits", w, h, |_, _| 0.20),
            write(dir.path(), "c.fits", w, h, |_, _| 0.40),
            write(dir.path(), "d.fits", w, h, |x, y| {
                if (x, y) == (3, 2) { 0.95 } else if (x, y) == (5, 5) { 0.0 } else { 0.20 }
            }),
        ];
        let src = BandSource::open(&paths, dir.path(), 1).unwrap();
        let ident = NormalizationPair::IDENTITY;
        let half = NormalizationPair { scale: 0.5, offset: 0.0 };
        let params = StackParams {
            rejection: &[ident, ident, half, ident],
            output: &[ident, ident, half, ident],
            weights: &[1.0, 1.0, 1.0, 3.0],
            range_low: Some(0.0),
            range_high: None,
            rejection_maps: true,
            local: None,
            local_for_rejection: false,
            local_for_output: false,
            rejection_bits: None,
            forced_rejection: None,
        };
        let progress = EngineProgress { on_band: &nop(), on_combine: &nop() };
        // sigma_high 1.0, not the brief's 2.0 (measured deviation, Task 3):
        // `combine::stddev` is Bessel-corrected (n-1 denominator). With n=4
        // and 3 of the 4 rejection-normalized values EXACTLY equal (0.20 here),
        // one outlier's z-score against that sample std is always exactly
        // (n-1)/sqrt(n) = 3/sqrt(4) = 1.5 — independent of the outlier's
        // magnitude (the classic small-n sigma-clip "masking" effect: the
        // outlier inflates its own std enough to hide). sigma_high=2.0 > 1.5
        // therefore never rejects the hot pixel here, at ANY hot-pixel value —
        // confirmed empirically: at 2.0 the hot pixel came out as the
        // weighted mean of all 4 survivors, 0.575, not 0.20. sigma_high=1.5
        // (the exact boundary) still doesn't reject either: the survivor test
        // is `xf <= hi`, and at that threshold the outlier sits AT `hi`, not
        // past it. sigma_high=1.0 gives clear margin (z=1.5 > 1.0) while
        // sigma_low stays at the brief's 3.0 (the three inliers' own z ≈
        // -0.577 is nowhere near either threshold, so they are unaffected).
        let out = integrate_stack(
            &src,
            &params,
            IntegrationRecipe::average(Rejection::SigmaClip { sigma_low: 3.0, sigma_high: 1.0 }),
            &pool(),
            &AtomicBool::new(false),
            progress,
            io(1 << 20),
        )
        .unwrap();
        let px = |x: usize, y: usize| out.base.data[y * w + x];
        // An ordinary pixel: all four survive, weighted mean of 0.20s = 0.20.
        assert!((px(0, 0) - 0.20).abs() < 1e-6, "{}", px(0, 0));
        // Hot pixel: frame 3 rejected high; the rest average to 0.20.
        assert!((px(3, 2) - 0.20).abs() < 1e-6, "{}", px(3, 2));
        assert_eq!(out.rejection_high.as_ref().unwrap()[2 * w + 3], 1.0);
        assert_eq!(out.rejection_low.as_ref().unwrap()[2 * w + 3], 0.0);
        // Dead pixel: frame 3 range-rejected low, counted low.
        assert!((px(5, 5) - 0.20).abs() < 1e-6, "{}", px(5, 5));
        assert_eq!(out.rejection_low.as_ref().unwrap()[5 * w + 5], 1.0);
        assert_eq!(out.rejected_per_frame, vec![0, 0, 0, 2]);
        assert_eq!(out.samples_per_frame, vec![128, 128, 128, 128]);
        assert_eq!((out.rejected_low, out.rejected_high), (1, 1));
        assert!(out.base.data.iter().all(|v| (v - 0.20).abs() < 1e-6));
    }

    #[test]
    fn stack_path_with_unit_inputs_equals_the_master_path() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (24usize, 10usize);
        let paths: Vec<_> = (0..7)
            .map(|i| {
                write(dir.path(), &format!("f{i}.fits"), w, h, move |x, y| {
                    0.1 + 0.01 * ((x * 7 + y * 3 + i * 11) % 13) as f32 + if (x + y + i) % 17 == 0 { 0.3 } else { 0.0 }
                })
            })
            .collect();
        let src = BandSource::open(&paths, dir.path(), 1).unwrap();
        let recipe = IntegrationRecipe::average(Rejection::WinsorizedSigma { sigma_low: 4.0, sigma_high: 3.0 });
        let master = integrate_bias_like(&paths, recipe, &pool(), dir.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &nop(), on_combine: &nop() }, io(1 << 20)).unwrap();
        let ident = vec![NormalizationPair::IDENTITY; 7];
        let params = StackParams {
            rejection: &ident, output: &ident, weights: &[1.0; 7],
            range_low: None, range_high: None, rejection_maps: false,
            local: None, local_for_rejection: false, local_for_output: false,
            rejection_bits: None,
            forced_rejection: None,
        };
        let stack = integrate_stack(&src, &params, recipe, &pool(), &AtomicBool::new(false),
            EngineProgress { on_band: &nop(), on_combine: &nop() }, io(1 << 20)).unwrap();
        assert_eq!(master.data.len(), stack.base.data.len());
        for (i, (a, b)) in master.data.iter().zip(&stack.base.data).enumerate() {
            assert_eq!(a.to_bits(), b.to_bits(), "pixel {i}: {a} vs {b}");
        }
        assert_eq!(master.rejected_fraction, stack.base.rejected_fraction);
        assert!(stack.rejection_low.is_none() && stack.rejection_high.is_none());
    }

    #[test]
    fn a_rejection_in_the_first_frame_under_a_sorting_algorithm_reaches_the_maps() {
        // Frame 0 has a COLD pixel (0.02, not range-rejected) at (3,2); frames
        // 1–3 are flat 0.20. PercentileClip sorts ascending first, so the cold
        // sample sits at work[0] and the forward compaction of the three
        // survivors overwrites it — the defect the review found: the rejection
        // used to vanish from the maps and the per-frame counts.
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (16usize, 8usize);
        let paths = vec![
            write(dir.path(), "a.fits", w, h, |x, y| if (x, y) == (3, 2) { 0.02 } else { 0.20 }),
            write(dir.path(), "b.fits", w, h, |_, _| 0.20),
            write(dir.path(), "c.fits", w, h, |_, _| 0.20),
            write(dir.path(), "d.fits", w, h, |_, _| 0.20),
        ];
        let src = BandSource::open(&paths, dir.path(), 1).unwrap();
        let ident = vec![NormalizationPair::IDENTITY; 4];
        let params = StackParams {
            rejection: &ident,
            output: &ident,
            weights: &[1.0; 4],
            range_low: Some(0.0),
            range_high: None,
            rejection_maps: true,
            local: None,
            local_for_rejection: false,
            local_for_output: false,
            rejection_bits: None,
            forced_rejection: None,
        };
        let out = integrate_stack(
            &src,
            &params,
            IntegrationRecipe::average(Rejection::PercentileClip { low: 0.5, high: 0.5 }),
            &pool(),
            &AtomicBool::new(false),
            EngineProgress { on_band: &nop(), on_combine: &nop() },
            io(1 << 20),
        )
        .unwrap();
        assert!((out.base.data[2 * w + 3] - 0.20).abs() < 1e-6);
        assert_eq!(out.rejection_low.as_ref().unwrap()[2 * w + 3], 1.0);
        assert_eq!(out.rejection_high.as_ref().unwrap()[2 * w + 3], 0.0);
        assert_eq!(out.rejected_per_frame, vec![1, 0, 0, 0]);
        assert_eq!((out.rejected_low, out.rejected_high), (1, 0));
        assert_eq!(out.rejection_low.as_ref().unwrap().iter().sum::<f32>(), 1.0);
        assert_eq!(out.rejection_high.as_ref().unwrap().iter().sum::<f32>(), 0.0);
    }

    #[test]
    fn rejection_maps_land_on_the_right_global_rows_across_bands() {
        // 16×64, a tiny band budget → several bands; a cold pixel at (3,40)
        // and a hot pixel at (5,50) in frame 0 must land at their global rows.
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (16usize, 64usize);
        let paths = vec![
            write(dir.path(), "a.fits", w, h, |x, y| match (x, y) {
                (3, 40) => 0.02,
                (5, 50) => 0.95,
                _ => 0.20,
            }),
            write(dir.path(), "b.fits", w, h, |_, _| 0.20),
            write(dir.path(), "c.fits", w, h, |_, _| 0.20),
            write(dir.path(), "d.fits", w, h, |_, _| 0.20),
        ];
        let src = BandSource::open(&paths, dir.path(), 1).unwrap();
        let ident = vec![NormalizationPair::IDENTITY; 4];
        let params = StackParams {
            rejection: &ident,
            output: &ident,
            weights: &[1.0; 4],
            range_low: None,
            range_high: None,
            rejection_maps: true,
            local: None,
            local_for_rejection: false,
            local_for_output: false,
            rejection_bits: None,
            forced_rejection: None,
        };
        let out = integrate_stack(
            &src,
            &params,
            IntegrationRecipe::average(Rejection::PercentileClip { low: 0.5, high: 0.5 }),
            &pool(),
            &AtomicBool::new(false),
            EngineProgress { on_band: &nop(), on_combine: &nop() },
            io(4096),
        )
        .unwrap();
        assert!(out.base.bands >= 2, "expected a multi-band run, got {}", out.base.bands);
        let low = out.rejection_low.as_ref().unwrap();
        let high = out.rejection_high.as_ref().unwrap();
        assert_eq!(low[40 * w + 3], 1.0);
        assert_eq!(high[50 * w + 5], 1.0);
        assert_eq!(low.iter().sum::<f32>(), 1.0);
        assert_eq!(high.iter().sum::<f32>(), 1.0);
        assert_eq!(out.rejected_per_frame, vec![2, 0, 0, 0]);
        assert!(out.base.data.iter().all(|v| (v - 0.20).abs() < 1e-6));
    }

    /// M2 Task 6, brief test (a): frame 2 = 0.5·frame 1 + a linear gradient;
    /// frame 2's local pair (`A = 2`, `B = -2·gradient(x, y)`) maps it back
    /// onto frame 1 exactly (`v′ = A·v + B`), so with `local_for_output` the
    /// two-frame average must equal frame 1 everywhere, not the ~25% low
    /// value a global pair (which can only apply one scale/offset for the
    /// WHOLE frame) would leave behind once the spatially varying half is
    /// removed. Frame 1 has no grid (`local[0] = None`) and keeps using its
    /// global identity pair, exactly as `StackParams::local`'s doc describes.
    ///
    /// Fix round 1, item 5: the budget is deliberately small so
    /// `out.base.bands >= 2` (asserted below) — the original version of this
    /// test used a 1 MiB budget, which gives exactly one band (`y0 = 0`
    /// always), so `y_abs = y0 + row_in_band` was indistinguishable from
    /// `row_in_band` alone. Verified by temporarily reverting `integrate_stack`
    /// to `let y_abs = row_in_band;` and re-running: every row past the
    /// first band failed (wrong gradient sample), confirming this version
    /// actually exercises the absolute-row rule; reverted back before
    /// committing (see the fix round 1 report section).
    #[test]
    fn local_grids_replace_the_output_pair_for_a_frame_that_has_one() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (16usize, 12usize);
        let c = 1000.0f32;
        let gradient = |x: usize, y: usize| 0.5f32 * x as f32 + 0.25f32 * y as f32;
        let paths = vec![
            write(dir.path(), "f1.fits", w, h, |_, _| c),
            write(dir.path(), "f2.fits", w, h, move |x, y| 0.5 * c + gradient(x, y)),
        ];
        let src = BandSource::open(&paths, dir.path(), 1).unwrap();
        let ident = vec![NormalizationPair::IDENTITY; 2];
        // Frame 2's local row evaluator FACTORY: constant A = 2, B =
        // -2·gradient(x, y) — a real `LnGrid` reproduces an affine function
        // of the node values exactly (see `stacking::ln::grid::LnGrid`'s own
        // doc and its `linear_ramp_is_reproduced_by_the_spline_between_nodes`
        // test); this closure stands in for that without pulling `stacking`
        // (gated on `solver` too) into `integration/`'s own test module. A
        // factory, not a direct evaluator (fix round 1, item 3): calling it
        // allocates nothing shared, so it is trivially safe to call once per
        // worker thread — this test only ever calls it on one thread, but
        // the shape must still match `StackParams::local`.
        let local_frame2_factory = move || -> Box<LocalNormRow<'static>> {
            Box::new(move |y: usize, a_row: &mut [f32], b_row: &mut [f32]| {
                for (x, (a, b)) in a_row.iter_mut().zip(b_row.iter_mut()).enumerate() {
                    *a = 2.0;
                    *b = -2.0 * gradient(x, y);
                }
            })
        };
        let local: Vec<Option<&LocalNormRowFactory<'_>>> =
            vec![None, Some(&local_frame2_factory)];
        let params = StackParams {
            rejection: &ident,
            output: &ident,
            weights: &[1.0, 1.0],
            range_low: None,
            range_high: None,
            rejection_maps: false,
            local: Some(&local),
            local_for_rejection: false,
            local_for_output: true,
            rejection_bits: None,
            forced_rejection: None,
        };
        // budget: `per_row_bytes` for 2 f32 frames at width 16 is
        // 2*16*4 + 16*8 = 256; 2_000 / 256 -> 7-row bands, so 12 rows run
        // as 2 bands (7 + 5) — comfortably `>= 2`.
        let out = integrate_stack(
            &src,
            &params,
            IntegrationRecipe::average(Rejection::None),
            &pool(),
            &AtomicBool::new(false),
            EngineProgress { on_band: &nop(), on_combine: &nop() },
            io(2_000),
        )
        .unwrap();
        assert!(out.base.bands >= 2, "expected a multi-band run, got {}", out.base.bands);
        for (i, &v) in out.base.data.iter().enumerate() {
            assert!((v - c).abs() < 1e-4, "pixel {i} (row {}): {v} vs {c}", i / w);
        }
    }

    /// Perf tier A Task 0 (audit §3.3, item I11): `LinearFitClip` over 24
    /// frames with genuine per-frame scatter (so the outer convergence loop
    /// and `medfit_line`'s bisection actually run more than the n<2
    /// early-out) must record real CPU time and real iteration/evaluation
    /// counts — instrumentation only, no output number this test cares
    /// about moves.
    #[test]
    fn combine_cpu_and_rejection_counters_are_recorded_for_linear_fit_clip() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (4usize, 4usize);
        let n = 24usize;
        let mut paths = Vec::with_capacity(n);
        for i in 0..n {
            let v = 1.0 + ((i as f32 * 37.0) % 23.0) / 23.0 * 0.6;
            paths.push(write(dir.path(), &format!("f{i}.fits"), w, h, move |_, _| v));
        }
        let src = BandSource::open(&paths, dir.path(), 1).unwrap();
        let ident = vec![NormalizationPair::IDENTITY; n];
        let weights = vec![1.0f32; n];
        let recipe = IntegrationRecipe::average(Rejection::LinearFitClip {
            sigma_low: 5.0,
            sigma_high: 3.5,
        });
        let params = StackParams {
            rejection: &ident,
            output: &ident,
            weights: &weights,
            range_low: None,
            range_high: None,
            rejection_maps: false,
            local: None,
            local_for_rejection: false,
            local_for_output: false,
            rejection_bits: None,
            forced_rejection: None,
        };
        let worker_pool = pool();
        let threads = worker_pool.current_num_threads() as u32;
        let out = integrate_stack(
            &src,
            &params,
            recipe,
            &worker_pool,
            &AtomicBool::new(false),
            EngineProgress { on_band: &nop(), on_combine: &nop() },
            io(1 << 20),
        )
        .unwrap();
        assert!(
            out.base.combine_cpu_duration > std::time::Duration::ZERO,
            "combine CPU time not recorded"
        );
        assert!(
            out.base.combine_cpu_duration <= out.base.combine_duration * threads,
            "combine CPU time {:?} exceeds wall combine time {:?} x {threads} threads",
            out.base.combine_cpu_duration,
            out.base.combine_duration
        );
        assert!(
            out.base.rejection_iters_total > 0,
            "reject_linear_fit's outer-loop pass count not recorded"
        );
        assert!(
            out.base.medfit_evals_total > 0,
            "medfit_line's rofunc evaluation count not recorded"
        );
    }

    /// M2 Task 6, brief test (b): 24 frames share one background level plus
    /// a per-frame additive offset that is NOT correlated with sort order
    /// (a stand-in for mismatched per-frame backgrounds real LN grids
    /// correct) — one frame additionally carries a small "hot pixel" bump.
    /// Measured directly against `combine::combine_pixel` before writing
    /// this test: with the offsets left in (global identity pair only),
    /// `LinearFitClip`'s own residual dispersion is inflated enough by the
    /// scatter that the bump's residual never clears `sigma_high · s`, so it
    /// is never rejected; once each frame's grid removes its own offset
    /// (`local_for_rejection`, `A = 1`, `B = -offset`), every OTHER frame's
    /// working value collapses to the same constant and the bump stands out
    /// immediately. `local_for_output` stays off — this test is about which
    /// samples the rejection step SEES, not what the surviving average is.
    #[test]
    fn local_rejection_normalization_catches_a_hot_pixel_hidden_by_per_frame_drift() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (4usize, 4usize);
        let n = 24usize;
        let base = 1.0f32;
        let offsets: Vec<f32> = (0..n)
            .map(|i| ((i as f32 * 37.0) % 23.0) / 23.0 * 0.6)
            .collect();
        let hot_idx = n - 1;
        let hot_bump = 0.15f32;
        let mut paths = Vec::with_capacity(n);
        for i in 0..n {
            let mut v = base + offsets[i];
            if i == hot_idx {
                v += hot_bump;
            }
            paths.push(write(dir.path(), &format!("f{i}.fits"), w, h, move |_, _| v));
        }
        let src = BandSource::open(&paths, dir.path(), 1).unwrap();
        let ident = vec![NormalizationPair::IDENTITY; n];
        let weights = vec![1.0f32; n];
        let recipe = IntegrationRecipe::average(Rejection::LinearFitClip {
            sigma_low: 5.0,
            sigma_high: 3.5,
        });

        // Without LN: the global identity pair cannot remove the per-frame
        // drift, and the drift's own scatter hides the hot pixel from
        // LinearFitClip.
        let params_global = StackParams {
            rejection: &ident,
            output: &ident,
            weights: &weights,
            range_low: None,
            range_high: None,
            rejection_maps: false,
            local: None,
            local_for_rejection: false,
            local_for_output: false,
            rejection_bits: None,
            forced_rejection: None,
        };
        let out_global = integrate_stack(
            &src,
            &params_global,
            recipe,
            &pool(),
            &AtomicBool::new(false),
            EngineProgress { on_band: &nop(), on_combine: &nop() },
            io(1 << 20),
        )
        .unwrap();
        assert_eq!(
            out_global.rejected_per_frame[hot_idx], 0,
            "without local normalization the per-frame drift must hide the hot pixel from linear-fit rejection"
        );

        // With LN: every frame's grid is constant (A = 1, B = -offset) —
        // subtracting exactly the drift this fixture added. Factories, not
        // direct evaluators (fix round 1, item 3): each produces a fresh
        // closure that owns nothing shared, so calling one more than once
        // (as the real engine now does, once per worker thread) is trivially
        // safe here too.
        let factories: Vec<_> = offsets
            .iter()
            .map(|&o| {
                move || -> Box<LocalNormRow<'static>> {
                    Box::new(move |_y: usize, a_row: &mut [f32], b_row: &mut [f32]| {
                        a_row.fill(1.0);
                        b_row.fill(-o);
                    })
                }
            })
            .collect();
        let local: Vec<Option<&LocalNormRowFactory<'_>>> = factories
            .iter()
            .map(|f| Some(f as &LocalNormRowFactory<'_>))
            .collect();
        let params_local = StackParams {
            rejection: &ident,
            output: &ident,
            weights: &weights,
            range_low: None,
            range_high: None,
            rejection_maps: false,
            local: Some(&local),
            local_for_rejection: true,
            local_for_output: false,
            rejection_bits: None,
            forced_rejection: None,
        };
        let out_local = integrate_stack(
            &src,
            &params_local,
            recipe,
            &pool(),
            &AtomicBool::new(false),
            EngineProgress { on_band: &nop(), on_combine: &nop() },
            io(1 << 20),
        )
        .unwrap();
        assert!(
            out_local.rejected_per_frame[hot_idx] > 0,
            "with local rejection normalization the hot pixel must be caught, got {:?}",
            out_local.rejected_per_frame
        );
        // Minor item 6: `local_for_rejection` (with `local_for_output`
        // false) must not leak into the OUTPUT sample — the survivors'
        // contribution to the average must still be their GLOBAL (here:
        // identity, i.e. raw) value. Pin the exact rejection pattern first
        // (exactly one rejection, on `hot_idx`) so the hand-computed
        // "mean of every other frame's raw value" below is the one true
        // expected answer, not a coincidence.
        // Every one of the fixture's w×h pixels carries the same constant
        // per frame, so the rejection fires at every pixel identically —
        // `rejected_per_frame` sums over the whole image, not one sample.
        let total_pixels = (w * h) as u64;
        assert_eq!(out_local.rejected_per_frame[hot_idx], total_pixels);
        assert_eq!(out_local.rejected_per_frame.iter().sum::<u64>(), total_pixels);
        let raw_values: Vec<f32> = (0..n)
            .map(|i| {
                let mut v = base + offsets[i];
                if i == hot_idx {
                    v += hot_bump;
                }
                v
            })
            .collect();
        let survivors_sum: f32 = (0..n).filter(|&i| i != hot_idx).map(|i| raw_values[i]).sum();
        let expected = survivors_sum / (n - 1) as f32;
        assert!(
            out_local.base.data.iter().all(|&v| (v - expected).abs() < 1e-6),
            "local_for_rejection leaked into the output sample: {:?} vs expected {expected}",
            out_local.base.data
        );
    }

    /// M3 Task 2: a [`RejectionBitSink`] must see exactly the (frame, pixel)
    /// pairs the engine's own `rejected_per_frame` accounting already
    /// counts, and its presence must not change a single OUTPUT byte —
    /// pinned by comparing the SAME run with `rejection_bits: None` against
    /// one with a recording sink. Sigma parameters match
    /// `stack_path_weights_normalizes_and_counts_rejections_per_side_and_frame`
    /// above (n=4, small-n masking analysis in that test's own comment):
    /// `sigma_high = 1.0` reliably rejects a huge single-frame outlier that
    /// `sigma_high >= 1.5` would mask. Fix round 1, M5: `(w, h) = (16, 64)`
    /// with a tiny `io(4096)` band budget (the same shape
    /// `rejection_maps_land_on_the_right_global_rows_across_bands` above
    /// already proves multi-bands) forces `record_band`'s `y0` argument to
    /// be non-zero for the hot pixel's band — a bug that passed `0` instead
    /// of the true `y0` out of the band closure would not have been caught
    /// by the single-band shape this test used before.
    #[test]
    fn rejection_bit_sink_records_exactly_the_rejected_samples_and_does_not_change_the_output() {
        struct RecordingSink {
            frames: usize,
            words: usize,
            calls: std::sync::Mutex<Vec<(usize, usize, Vec<u64>)>>,
        }
        impl RejectionBitSink for RecordingSink {
            fn words_per_row(&self) -> usize {
                self.words
            }
            fn frames(&self) -> usize {
                self.frames
            }
            fn record_band(&self, y0: usize, rows: usize, bits: &[u64]) -> Result<(), IntegrationError> {
                self.calls.lock().unwrap().push((y0, rows, bits.to_vec()));
                Ok(())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (16usize, 64usize);
        let hot_idx = 3usize;
        let (hot_x, hot_y) = (7usize, 40usize);
        let paths = vec![
            write(dir.path(), "a.fits", w, h, |_, _| 100.0),
            write(dir.path(), "b.fits", w, h, |_, _| 100.0),
            write(dir.path(), "c.fits", w, h, |_, _| 100.0),
            write(dir.path(), "hot.fits", w, h, |x, y| {
                if (x, y) == (hot_x, hot_y) { 9000.0 } else { 100.0 }
            }),
        ];
        let src = BandSource::open(&paths, dir.path(), 1).unwrap();
        let ident = vec![NormalizationPair::IDENTITY; 4];
        let weights = vec![1.0f32; 4];
        let recipe = IntegrationRecipe::average(Rejection::SigmaClip { sigma_low: 3.0, sigma_high: 1.0 });

        let params_none = StackParams {
            rejection: &ident,
            output: &ident,
            weights: &weights,
            range_low: None,
            range_high: None,
            rejection_maps: false,
            local: None,
            local_for_rejection: false,
            local_for_output: false,
            rejection_bits: None,
            forced_rejection: None,
        };
        let out_none = integrate_stack(
            &src,
            &params_none,
            recipe,
            &pool(),
            &AtomicBool::new(false),
            EngineProgress { on_band: &nop(), on_combine: &nop() },
            io(4096),
        )
        .unwrap();
        assert!(
            out_none.base.bands >= 2,
            "expected a multi-band run, got {}",
            out_none.base.bands
        );
        assert_eq!(
            out_none.rejected_per_frame[hot_idx], 1,
            "sanity: the hot pixel must actually be rejected in the no-sink baseline, got {:?}",
            out_none.rejected_per_frame
        );

        let sink = RecordingSink { frames: 4, words: w.div_ceil(64), calls: std::sync::Mutex::new(Vec::new()) };
        let params_sink = StackParams { rejection_bits: Some(&sink), ..params_none };
        let out_sink = integrate_stack(
            &src,
            &params_sink,
            recipe,
            &pool(),
            &AtomicBool::new(false),
            EngineProgress { on_band: &nop(), on_combine: &nop() },
            io(4096),
        )
        .unwrap();

        // The sink's presence must not move a single OUTPUT byte.
        assert_eq!(out_none.base.data.len(), out_sink.base.data.len());
        for (i, (a, b)) in out_none.base.data.iter().zip(&out_sink.base.data).enumerate() {
            assert_eq!(a.to_bits(), b.to_bits(), "pixel {i}: {a} vs {b} — sink presence changed the output");
        }
        assert_eq!(out_none.rejected_per_frame, out_sink.rejected_per_frame);
        assert_eq!((out_none.rejected_low, out_none.rejected_high), (out_sink.rejected_low, out_sink.rejected_high));

        // Exactly one (frame, x, y) bit set across the whole run, and it
        // must have arrived on a call whose y0 is genuinely non-zero (the
        // hot row is 40, well past the first several small bands).
        let words = w.div_ceil(64);
        let mut found = Vec::new();
        let mut saw_nonzero_y0 = false;
        for (y0, rows, bits) in sink.calls.lock().unwrap().iter() {
            if *y0 > 0 {
                saw_nonzero_y0 = true;
            }
            assert_eq!(bits.len(), rows * 4 * words, "band buffer sized rows x frames x words");
            for row_in_band in 0..*rows {
                for frame in 0..4 {
                    for word in 0..words {
                        let bitset = bits[(row_in_band * 4 + frame) * words + word];
                        if bitset == 0 {
                            continue;
                        }
                        for bit in 0..64 {
                            if bitset & (1u64 << bit) != 0 {
                                found.push((frame, y0 + row_in_band, word * 64 + bit));
                            }
                        }
                    }
                }
            }
        }
        assert!(saw_nonzero_y0, "expected at least one band with y0 > 0");
        assert_eq!(
            found,
            vec![(hot_idx, hot_y, hot_x)],
            "expected exactly one bit at (frame {hot_idx}, row {hot_y}, col {hot_x}): {found:?}"
        );
    }

    /// One frame's forced bits, from an explicit `(frame, x, y)` list.
    struct ListSource {
        frames: usize,
        words: usize,
        /// `[frame][y]` → that row's words, `None` when the row is clear.
        rows: Vec<Vec<Option<Vec<u64>>>>,
    }

    impl ListSource {
        fn new(frames: usize, width: usize, height: usize, forced: &[(usize, usize, usize)]) -> Self {
            let words = width.div_ceil(64);
            let mut rows: Vec<Vec<Option<Vec<u64>>>> =
                (0..frames).map(|_| (0..height).map(|_| None).collect()).collect();
            for &(frame, x, y) in forced {
                let row = rows[frame][y].get_or_insert_with(|| vec![0u64; words]);
                row[x / 64] |= 1u64 << (x % 64);
            }
            ListSource { frames, words, rows }
        }
    }

    impl RejectionBitSource for ListSource {
        fn words_per_row(&self) -> usize {
            self.words
        }
        fn frames(&self) -> usize {
            self.frames
        }
        fn forced_row(&self, frame: usize, y: usize) -> Option<&[u64]> {
            self.rows[frame][y].as_deref()
        }
    }

    /// M4c Task 3 (ruling R-M4c-4): a forced sample is dropped BEFORE any
    /// rejection algorithm runs — the survivors are combined as usual, and
    /// the forced sample is counted as a rejection in `rejected_low`/
    /// `rejected_high`, in `rejected_per_frame`, in the low/high maps and
    /// in the bitmaps, but NOT in `base.rejected_fraction` (algorithm-only,
    /// exactly like a range rejection). Three frames at three DIFFERENT
    /// levels with `Rejection::None`, so the output pixel alone says which
    /// samples were combined.
    #[test]
    fn forced_rejection_drops_a_sample_before_the_algorithm_and_counts_it() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (16usize, 8usize);
        let paths = vec![
            write(dir.path(), "a.fits", w, h, |_, _| 0.10),
            write(dir.path(), "b.fits", w, h, |_, _| 0.40),
            write(dir.path(), "c.fits", w, h, |_, _| 0.20),
        ];
        let src = BandSource::open(&paths, dir.path(), 1).unwrap();
        let ident = vec![NormalizationPair::IDENTITY; 3];
        let weights = vec![1.0f32; 3];
        let recipe = IntegrationRecipe::average(Rejection::None);
        let params_none = StackParams {
            rejection: &ident,
            output: &ident,
            weights: &weights,
            range_low: None,
            range_high: None,
            rejection_maps: true,
            local: None,
            local_for_rejection: false,
            local_for_output: false,
            rejection_bits: None,
            forced_rejection: None,
        };
        let out_none = integrate_stack(
            &src,
            &params_none,
            recipe,
            &pool(),
            &AtomicBool::new(false),
            EngineProgress { on_band: &nop(), on_combine: &nop() },
            io(1 << 20),
        )
        .unwrap();
        let mean_of_three = (0.10 + 0.40 + 0.20) / 3.0;
        assert!(
            (out_none.base.data[5 * w + 5] - mean_of_three).abs() < 1e-6,
            "baseline pixel {} != {mean_of_three}",
            out_none.base.data[5 * w + 5]
        );
        assert_eq!((out_none.rejected_low, out_none.rejected_high), (0, 0));

        // Frame 1 (the 0.40 one) forced out at (5, 5) only.
        let forced = ListSource::new(3, w, h, &[(1, 5, 5)]);
        let params = StackParams {
            forced_rejection: Some(&forced),
            ..params_none
        };
        let out = integrate_stack(
            &src,
            &params,
            recipe,
            &pool(),
            &AtomicBool::new(false),
            EngineProgress { on_band: &nop(), on_combine: &nop() },
            io(1 << 20),
        )
        .unwrap();
        let mean_of_two = (0.10 + 0.20) / 2.0;
        assert!(
            (out.base.data[5 * w + 5] - mean_of_two).abs() < 1e-6,
            "forced pixel {} != the mean of frames 0 and 2 ({mean_of_two})",
            out.base.data[5 * w + 5]
        );
        // Every OTHER pixel is untouched.
        for y in 0..h {
            for x in 0..w {
                if (x, y) == (5, 5) {
                    continue;
                }
                assert_eq!(
                    out.base.data[y * w + x].to_bits(),
                    out_none.base.data[y * w + x].to_bits(),
                    "pixel ({x}, {y}) moved without being forced"
                );
            }
        }
        assert_eq!(out.rejected_per_frame, vec![0, 1, 0]);
        assert_eq!(out.samples_per_frame, vec![128, 128, 128]);
        // 0.40 sits above the survivors' median (0.20) → the high side.
        assert_eq!((out.rejected_low, out.rejected_high), (0, 1));
        assert_eq!(out.rejection_high.as_ref().unwrap()[5 * w + 5], 1.0);
        assert_eq!(out.rejection_low.as_ref().unwrap()[5 * w + 5], 0.0);
        assert_eq!(
            out.base.rejected_fraction, 0.0,
            "a forced rejection is not an ALGORITHM rejection"
        );
    }

    /// Fix round 1 (m2): the forced source is asked for ABSOLUTE image
    /// rows, so a bit in the SECOND band must land on the row it names. A
    /// tiny band budget forces several bands (the same shape
    /// `rejection_maps_land_on_the_right_global_rows_across_bands` and the
    /// sink test use), and the forced bit sits at row 40 — well past the
    /// first band. A band-relative row would have hit row 40 of some later
    /// band's chunk, i.e. a different output pixel entirely.
    #[test]
    fn forced_rejection_lands_on_the_right_absolute_row_across_bands() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (16usize, 64usize);
        let (fx, fy) = (7usize, 40usize);
        let paths = vec![
            write(dir.path(), "a.fits", w, h, |_, _| 0.10),
            write(dir.path(), "b.fits", w, h, |_, _| 0.40),
            write(dir.path(), "c.fits", w, h, |_, _| 0.20),
        ];
        let src = BandSource::open(&paths, dir.path(), 1).unwrap();
        let ident = vec![NormalizationPair::IDENTITY; 3];
        let weights = vec![1.0f32; 3];
        let forced = ListSource::new(3, w, h, &[(1, fx, fy)]);
        let params = StackParams {
            rejection: &ident,
            output: &ident,
            weights: &weights,
            range_low: None,
            range_high: None,
            rejection_maps: true,
            local: None,
            local_for_rejection: false,
            local_for_output: false,
            rejection_bits: None,
            forced_rejection: Some(&forced),
        };
        let out = integrate_stack(
            &src,
            &params,
            IntegrationRecipe::average(Rejection::None),
            &pool(),
            &AtomicBool::new(false),
            EngineProgress { on_band: &nop(), on_combine: &nop() },
            io(4096),
        )
        .unwrap();
        assert!(
            out.base.bands >= 2,
            "expected a multi-band run, got {}",
            out.base.bands
        );
        let mean_of_two = (0.10 + 0.20) / 2.0;
        let mean_of_three = (0.10 + 0.40 + 0.20) / 3.0;
        assert!(
            (out.base.data[fy * w + fx] - mean_of_two).abs() < 1e-6,
            "the forced pixel ({fx}, {fy}) is the mean of frames 0 and 2, got {}",
            out.base.data[fy * w + fx]
        );
        for y in 0..h {
            for x in 0..w {
                if (x, y) == (fx, fy) {
                    continue;
                }
                assert!(
                    (out.base.data[y * w + x] - mean_of_three).abs() < 1e-6,
                    "pixel ({x}, {y}) must be untouched, got {}",
                    out.base.data[y * w + x]
                );
            }
        }
        assert_eq!(out.rejection_high.as_ref().unwrap()[fy * w + fx], 1.0);
        assert_eq!(
            out.rejection_high
                .as_ref()
                .unwrap()
                .iter()
                .filter(|&&v| v != 0.0)
                .count(),
            1,
            "exactly one pixel of the whole image carries a rejection"
        );
        assert_eq!(out.rejected_per_frame, vec![0, 1, 0]);
    }

    /// The forced samples must also reach the bitmap sink — so a `.rej`
    /// written during a pass that itself forces rejections describes what
    /// that pass actually combined.
    #[test]
    fn forced_rejections_reach_the_bitmap_sink() {
        struct Collect {
            frames: usize,
            words: usize,
            bits: std::sync::Mutex<Vec<(usize, usize, usize)>>,
        }
        impl RejectionBitSink for Collect {
            fn words_per_row(&self) -> usize {
                self.words
            }
            fn frames(&self) -> usize {
                self.frames
            }
            fn record_band(&self, y0: usize, rows: usize, bits: &[u64]) -> Result<(), IntegrationError> {
                let mut out = self.bits.lock().unwrap();
                for row in 0..rows {
                    for frame in 0..self.frames {
                        for word in 0..self.words {
                            let set = bits[(row * self.frames + frame) * self.words + word];
                            for bit in 0..64 {
                                if set & (1u64 << bit) != 0 {
                                    out.push((frame, word * 64 + bit, y0 + row));
                                }
                            }
                        }
                    }
                }
                Ok(())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (16usize, 8usize);
        let paths = vec![
            write(dir.path(), "a.fits", w, h, |_, _| 0.10),
            write(dir.path(), "b.fits", w, h, |_, _| 0.40),
            write(dir.path(), "c.fits", w, h, |_, _| 0.20),
        ];
        let src = BandSource::open(&paths, dir.path(), 1).unwrap();
        let ident = vec![NormalizationPair::IDENTITY; 3];
        let weights = vec![1.0f32; 3];
        let forced = ListSource::new(3, w, h, &[(1, 5, 5), (2, 0, 0)]);
        let sink = Collect {
            frames: 3,
            words: w.div_ceil(64),
            bits: std::sync::Mutex::new(Vec::new()),
        };
        let params = StackParams {
            rejection: &ident,
            output: &ident,
            weights: &weights,
            range_low: None,
            range_high: None,
            rejection_maps: false,
            local: None,
            local_for_rejection: false,
            local_for_output: false,
            rejection_bits: Some(&sink),
            forced_rejection: Some(&forced),
        };
        integrate_stack(
            &src,
            &params,
            IntegrationRecipe::average(Rejection::None),
            &pool(),
            &AtomicBool::new(false),
            EngineProgress { on_band: &nop(), on_combine: &nop() },
            io(1 << 20),
        )
        .unwrap();
        let mut got = sink.bits.lock().unwrap().clone();
        got.sort_unstable();
        assert_eq!(got, vec![(1, 5, 5), (2, 0, 0)]);
    }

    /// Forcing EVERY sample at a pixel leaves nothing to combine: the pixel
    /// is `all_bad` (the same verdict an all-range-rejected pixel has always
    /// had) and every forced sample is still counted.
    #[test]
    fn forcing_every_sample_leaves_an_all_bad_pixel_with_the_rejections_counted() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8usize, 4usize);
        let paths = vec![
            write(dir.path(), "a.fits", w, h, |_, _| 0.10),
            write(dir.path(), "b.fits", w, h, |_, _| 0.20),
            write(dir.path(), "c.fits", w, h, |_, _| 0.30),
        ];
        let src = BandSource::open(&paths, dir.path(), 1).unwrap();
        let ident = vec![NormalizationPair::IDENTITY; 3];
        let weights = vec![1.0f32; 3];
        let forced = ListSource::new(3, w, h, &[(0, 2, 1), (1, 2, 1), (2, 2, 1)]);
        let params = StackParams {
            rejection: &ident,
            output: &ident,
            weights: &weights,
            range_low: None,
            range_high: None,
            rejection_maps: false,
            local: None,
            local_for_rejection: false,
            local_for_output: false,
            rejection_bits: None,
            forced_rejection: Some(&forced),
        };
        let out = integrate_stack(
            &src,
            &params,
            IntegrationRecipe::average(Rejection::None),
            &pool(),
            &AtomicBool::new(false),
            EngineProgress { on_band: &nop(), on_combine: &nop() },
            io(1 << 20),
        )
        .unwrap();
        assert_eq!(out.base.data[1 * w + 2], 0.0);
        assert_eq!(out.base.all_bad_pixels, 1);
        assert_eq!(out.rejected_per_frame, vec![1, 1, 1]);
        // Median of the three present values is 0.20: 0.10 low, 0.20 and
        // 0.30 high (the `< median` test puts a tie on the high side).
        assert_eq!((out.rejected_low, out.rejected_high), (1, 2));
    }

    /// A forced-rejection source that disagrees with the frame source about
    /// either dimension is refused up front, not silently misindexed.
    #[test]
    fn a_mismatched_forced_rejection_source_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (8usize, 4usize);
        let paths = vec![
            write(dir.path(), "a.fits", w, h, |_, _| 0.10),
            write(dir.path(), "b.fits", w, h, |_, _| 0.20),
            write(dir.path(), "c.fits", w, h, |_, _| 0.30),
        ];
        let src = BandSource::open(&paths, dir.path(), 1).unwrap();
        let ident = vec![NormalizationPair::IDENTITY; 3];
        let weights = vec![1.0f32; 3];
        let base = StackParams {
            rejection: &ident,
            output: &ident,
            weights: &weights,
            range_low: None,
            range_high: None,
            rejection_maps: false,
            local: None,
            local_for_rejection: false,
            local_for_output: false,
            rejection_bits: None,
            forced_rejection: None,
        };
        let run = |params: &StackParams<'_, '_>| {
            integrate_stack(
                &src,
                params,
                IntegrationRecipe::average(Rejection::None),
                &pool(),
                &AtomicBool::new(false),
                EngineProgress { on_band: &nop(), on_combine: &nop() },
                io(1 << 20),
            )
        };

        let wrong_frames = ListSource::new(2, w, h, &[]);
        match run(&StackParams { forced_rejection: Some(&wrong_frames), ..base }) {
            Err(IntegrationError::BadInput(msg)) => {
                assert!(msg.contains("expects 2 frames"), "{msg}")
            }
            Err(e) => panic!("expected a frame-count refusal, got {e}"),
            Ok(_) => panic!("expected a frame-count refusal, the run succeeded"),
        }

        struct WrongWords;
        impl RejectionBitSource for WrongWords {
            fn words_per_row(&self) -> usize {
                7
            }
            fn frames(&self) -> usize {
                3
            }
            fn forced_row(&self, _frame: usize, _y: usize) -> Option<&[u64]> {
                None
            }
        }
        match run(&StackParams { forced_rejection: Some(&WrongWords), ..base }) {
            Err(IntegrationError::BadInput(msg)) => {
                assert!(msg.contains("words_per_row 7"), "{msg}")
            }
            Err(e) => panic!("expected a words-per-row refusal, got {e}"),
            Ok(_) => panic!("expected a words-per-row refusal, the run succeeded"),
        }
    }

    /// Perf tier 1 Task 1, fix wave triage item 12 (renamed from
    /// `band_prefetch_overlaps_read_and_combine_without_changing_the_
    /// output`, whose name and docstring claimed a wall-clock overlap this
    /// test never actually asserts — see the ruling in the body below):
    /// with a network-storage budget that forces many (prefetching) bands,
    /// the output must equal a single-band run's, bit for bit. Uses
    /// `io_network` — the fix wave gated the prefetch this test exists to
    /// exercise on `StorageClass::Network`, so `io`'s `Local` default would
    /// silently turn this into a no-op serial run.
    #[test]
    fn band_prefetch_does_not_change_the_output() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (64, 512);
        let paths: Vec<_> = (0..6)
            .map(|i| write(dir.path(), &format!("p{i}.fits"), w, h, move |x, y| (x * 3 + y * 7 + i * 11) as f32 * 0.01))
            .collect();
        let on_band = nop();
        let run = |budget: usize| {
            integrate_bias_like(
                &paths,
                IntegrationRecipe::median(Rejection::None),
                &pool(),
                dir.path(),
                &AtomicBool::new(false),
                EngineProgress { on_band: &on_band, on_combine: &nop() },
                io_network(budget),
            )
            .unwrap()
        };
        let single = run(usize::MAX / 4);
        // `io_network` runs this budget through `loop_band_rows`'s
        // network-only halving (its doc), so 4x the per-2-row-band cost
        // here — 4 * (6*64*4 + 8*64) B — lands on 2-row bands once
        // `band_rows_for_budget` sees the halved (2x) figure.
        let many = run(4 * (6 * w * 4 + 8 * w));
        assert!(many.bands >= 64, "expected many bands, got {}", many.bands);
        assert_eq!(single.data, many.data, "prefetch changed the output");
        // Controller ruling on this task's brief: a wall-clock `elapsed <
        // read_duration + combine_duration` assertion here is flaky — on a
        // 64-band run of tiny (2-row) bands, per-band scoped-thread spawn
        // overhead can swamp the microseconds of real read/combine work
        // and swing the comparison either way run to run (measured on this
        // machine: it failed under the OLD serial loop too, and it also
        // failed under this task's correct, overlapping implementation —
        // the margin is a few hundred microseconds either way, well inside
        // this machine's scheduling noise). The actual
        // overlap property — band N+1's read starts before band N's
        // combine finishes — is pinned deterministically instead by
        // `band_prefetch_starts_the_next_read_before_the_current_combine_finishes`
        // below, via event ordering against a `FrameSource` double rather
        // than a timer.
    }

    /// Perf tier 1 Task 1, deterministic companion to the pin above: band
    /// N+1's read must START before band N's combine FINISHES. A
    /// `FrameSource` double logs read start/end under its own mutex; the
    /// combine closure sleeps a few ms (comfortably longer than the fake
    /// read's own near-zero cost or any thread-spawn jitter) so the
    /// ordering is decided by the overlap itself, not by scheduling luck.
    /// Run against the PRE-task serial loop, this fails: the old code only
    /// starts band N+1's read after band N's combine has already returned.
    #[test]
    fn band_prefetch_starts_the_next_read_before_the_current_combine_finishes() {
        struct EventSource {
            width: usize,
            height: usize,
            n: usize,
            events: std::sync::Mutex<Vec<String>>,
        }
        impl EventSource {
            fn note(&self, s: impl Into<String>) {
                self.events.lock().unwrap().push(s.into());
            }
        }
        impl FrameSource for EventSource {
            fn width(&self) -> usize { self.width }
            fn height(&self) -> usize { self.height }
            fn frame_count(&self) -> usize { self.n }
            fn plane_kinds(&self) -> Vec<PlaneKind> { vec![PlaneKind::F32Le; self.n] }
            fn bytes_per_row(&self) -> usize { self.width * 4 * self.n }
            fn band_rows_for_budget(&self, budget_bytes: usize) -> usize {
                (budget_bytes / (self.width * 4 * self.n).max(1)).max(1)
            }
            fn read_band_with_progress(
                &self,
                y0: usize,
                rows: usize,
                out: &mut BandPlanes,
                _concurrency: usize,
                on_bytes: &(dyn Fn(u64) + Sync),
                _cancel: &AtomicBool,
            ) -> Result<(), IntegrationError> {
                self.note(format!("read start {y0}"));
                out.set_rows(rows);
                on_bytes((rows * self.width * 4 * self.n) as u64);
                self.note(format!("read end {y0}"));
                Ok(())
            }
        }

        let (w, h) = (8usize, 8usize);
        let src = EventSource { width: w, height: h, n: 2, events: std::sync::Mutex::new(Vec::new()) };
        // per-row cost = w*4*n = 64; `Network` storage is required here — the
        // fix wave gates the prefetch this test pins on it — and `band_loop`
        // then halves the budget it sizes bands against, so 256/2 = 128 ->
        // band_rows = 2, giving 4 bands (y0 = 0, 2, 4, 6) — enough for a
        // middle band to prefetch while its predecessor combines.
        let policy = IoPolicy { band_budget_bytes: 256, read_concurrency: 1, storage: StorageClass::Network };
        let mut out = vec![0f32; w * h];
        let combine = |job: BandJob<'_>, tick: &(dyn Fn() + Sync)| -> Result<(), IntegrationError> {
            src.note(format!("combine start {}", job.y0));
            std::thread::sleep(std::time::Duration::from_millis(20));
            for v in job.out_band.iter_mut() {
                *v = 0.0;
            }
            tick();
            src.note(format!("combine end {}", job.y0));
            Ok(())
        };
        let stats = band_loop(
            &src,
            &pool(),
            &AtomicBool::new(false),
            &EngineProgress { on_band: &nop(), on_combine: &nop() },
            policy,
            &mut out,
            &combine,
        )
        .unwrap();
        assert!(stats.bands >= 3, "need at least 3 bands to exercise a middle overlap, got {}", stats.bands);
        let events = src.events.into_inner().unwrap();
        let pos = |needle: &str| events.iter().position(|e| e == needle).unwrap_or_else(|| panic!("missing event {needle:?} in {events:?}"));
        let combine_end_0 = pos("combine end 0");
        // Band index 1 starts at y0 = band_rows = 2.
        let read_start_1 = pos("read start 2");
        assert!(
            read_start_1 < combine_end_0,
            "band 1's read did not start before band 0's combine finished: {events:?}"
        );
    }

    /// Fix wave item 1's companion pin, the other side of the gate: on
    /// `Local` storage `band_loop` must run the pre-tier-1 shape — no
    /// halving in `loop_band_rows` and no prefetch thread ever spawned, so
    /// a band's read completes strictly before its OWN combine starts, and
    /// the next band's read never starts until the previous band's combine
    /// has already finished. Same `EventSource` double, budget and
    /// dimensions as the `Network` sibling above, `Local` swapped in: the
    /// un-halved geometry gives twice the rows per band (4 rather than 2)
    /// and half the bands (2 rather than 4), and the event log is fully
    /// deterministic rather than merely ordered, since nothing overlaps.
    #[test]
    fn band_loop_stays_serial_and_un_halved_on_local_storage() {
        struct EventSource {
            width: usize,
            height: usize,
            n: usize,
            events: std::sync::Mutex<Vec<String>>,
        }
        impl EventSource {
            fn note(&self, s: impl Into<String>) {
                self.events.lock().unwrap().push(s.into());
            }
        }
        impl FrameSource for EventSource {
            fn width(&self) -> usize { self.width }
            fn height(&self) -> usize { self.height }
            fn frame_count(&self) -> usize { self.n }
            fn plane_kinds(&self) -> Vec<PlaneKind> { vec![PlaneKind::F32Le; self.n] }
            fn bytes_per_row(&self) -> usize { self.width * 4 * self.n }
            fn band_rows_for_budget(&self, budget_bytes: usize) -> usize {
                (budget_bytes / (self.width * 4 * self.n).max(1)).max(1)
            }
            fn read_band_with_progress(
                &self,
                y0: usize,
                rows: usize,
                out: &mut BandPlanes,
                _concurrency: usize,
                on_bytes: &(dyn Fn(u64) + Sync),
                _cancel: &AtomicBool,
            ) -> Result<(), IntegrationError> {
                self.note(format!("read start {y0}"));
                out.set_rows(rows);
                on_bytes((rows * self.width * 4 * self.n) as u64);
                self.note(format!("read end {y0}"));
                Ok(())
            }
        }

        let (w, h) = (8usize, 8usize);
        let src = EventSource { width: w, height: h, n: 2, events: std::sync::Mutex::new(Vec::new()) };
        // per-row cost = w*4*n = 64; `Local` storage never halves (the
        // gate), so 256 -> band_rows = 4, giving 2 bands (y0 = 0, 4) — half
        // the `Network` sibling's band count at the same budget, pinning
        // the un-halved geometry directly.
        let policy = IoPolicy { band_budget_bytes: 256, read_concurrency: 1, storage: StorageClass::Local };
        let mut out = vec![0f32; w * h];
        let combine = |job: BandJob<'_>, tick: &(dyn Fn() + Sync)| -> Result<(), IntegrationError> {
            src.note(format!("combine start {}", job.y0));
            for v in job.out_band.iter_mut() {
                *v = 0.0;
            }
            tick();
            src.note(format!("combine end {}", job.y0));
            Ok(())
        };
        let stats = band_loop(
            &src,
            &pool(),
            &AtomicBool::new(false),
            &EngineProgress { on_band: &nop(), on_combine: &nop() },
            policy,
            &mut out,
            &combine,
        )
        .unwrap();
        assert_eq!(stats.band_rows, 4, "Local storage must not halve the band budget");
        assert_eq!(stats.bands, 2, "8 rows at 4 rows/band is 2 bands");
        let events = src.events.into_inner().unwrap();
        assert_eq!(
            events,
            vec![
                "read start 0", "read end 0", "combine start 0", "combine end 0",
                "read start 4", "read end 4", "combine start 4", "combine end 4",
            ],
            "Local storage must run strictly serially — read N, combine N, read N+1, … — \
             with no overlap: {events:?}"
        );
    }

    /// Fix wave item 2: a prefetch that fails while the combine is ALSO
    /// failing must not be swallowed — `band_loop` still has to return
    /// something, and it must be the combine's own error (the first
    /// failure it learns about, via `combine_result?`), never the
    /// prefetch's. This double makes band 0's combine and band 1's
    /// prefetch fail concurrently on every run, regardless of scheduling,
    /// so the `tracing::warn!` call site this pins is genuinely exercised
    /// — engine.rs has no log-capture harness (`docs/logging/README.md`'s
    /// pattern lives in `logging/mod.rs` and needs a process-global
    /// subscriber + env lock this module's parallel tests would fight
    /// over), so this test asserts the returned error only and leaves the
    /// warn line to manual inspection (`RUST_LOG=warn cargo test -p
    /// athenaeum-core --lib band_prefetch_error_is_not_swallowed -- \
    /// --nocapture`).
    #[test]
    fn band_prefetch_error_is_not_swallowed_when_the_combine_also_fails() {
        struct FailingSource {
            width: usize,
            height: usize,
        }
        impl FrameSource for FailingSource {
            fn width(&self) -> usize { self.width }
            fn height(&self) -> usize { self.height }
            fn frame_count(&self) -> usize { 1 }
            fn plane_kinds(&self) -> Vec<PlaneKind> { vec![PlaneKind::F32Le] }
            fn bytes_per_row(&self) -> usize { self.width * 4 }
            fn band_rows_for_budget(&self, budget_bytes: usize) -> usize {
                (budget_bytes / (self.width * 4).max(1)).max(1)
            }
            fn read_band_with_progress(
                &self,
                y0: usize,
                rows: usize,
                out: &mut BandPlanes,
                _concurrency: usize,
                on_bytes: &(dyn Fn(u64) + Sync),
                _cancel: &AtomicBool,
            ) -> Result<(), IntegrationError> {
                out.set_rows(rows);
                if y0 == 0 {
                    // Band 0's own (pre-loop) read: succeeds, so its combine
                    // is reached and can fail on schedule below.
                    on_bytes((rows * self.width * 4) as u64);
                    Ok(())
                } else {
                    // Every later band's prefetch: fails, as a real I/O
                    // error on the next band's read would.
                    Err(IntegrationError::Decode("prefetch read failed (test double)".into()))
                }
            }
        }

        let (w, h) = (8usize, 16usize);
        let src = FailingSource { width: w, height: h };
        let policy = IoPolicy { band_budget_bytes: w * 4, read_concurrency: 1, storage: StorageClass::Network };
        let mut out = vec![0f32; w * h];
        let combine = |_job: BandJob<'_>, _tick: &(dyn Fn() + Sync)| -> Result<(), IntegrationError> {
            // Band 0's own combine ALSO fails, concurrently with band 1's
            // prefetch above.
            Err(IntegrationError::BadInput("combine failed (test double)".into()))
        };
        let result = band_loop(
            &src,
            &pool(),
            &AtomicBool::new(false),
            &EngineProgress { on_band: &nop(), on_combine: &nop() },
            policy,
            &mut out,
            &combine,
        );
        match result {
            Err(IntegrationError::BadInput(msg)) => {
                assert!(msg.contains("combine failed"), "expected the combine's own error, got {msg}");
            }
            Err(e) => panic!("expected the combine's BadInput error to win over the prefetch's, got {e}"),
            Ok(_) => panic!(
                "expected the combine's error to win over the prefetch's, but the run succeeded"
            ),
        }
    }
}
