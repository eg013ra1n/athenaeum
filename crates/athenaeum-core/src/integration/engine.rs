//! Recipe orchestration (spec §4, §9): banded streaming + per-pixel combine.
//! Memory: N × band, sized per build by `integration::band_budget` from the
//! machine and the compute-queue ceiling (see that module) rather than a
//! compile-time constant. Parallelism: rayon over the pixels of the current
//! band via the shared image pool.

use super::banded::{BandPlanes, BandSource};
use super::combine::{combine_pixel, IntegrationRecipe};
use super::IntegrationError;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

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
    /// the engine can tell them apart.
    pub read_duration: std::time::Duration,
    /// Wall time spent in the parallel per-pixel combine across every band.
    pub combine_duration: std::time::Duration,
    /// Rows per band and how many bands the run used — the two numbers the
    /// band budget actually decides, reported so the build's log line does not
    /// have to re-derive them.
    pub band_rows: usize,
    pub bands: usize,
    /// Pixel bytes actually read (sum of `rows * width * bytes_per_sample`
    /// over every band and frame). Not the files' size on disk: headers and
    /// padding are never read, and a flat's pass 1 reads only its central
    /// third.
    pub bytes_read: u64,
}

pub struct EngineProgress<'a> {
    /// `(band_index_1based, bands_total, bytes_read_so_far, bytes_total)`.
    pub on_band: &'a dyn Fn(usize, usize, u64, u64),
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

/// Shared banded-combine core. `scale[i]`/`precal` transform frame i's
/// samples before combining: v' = (v - precal(i, pixel)) * scale[i].
/// `band_budget_bytes` is injectable (module-internal) so tests can force
/// multi-band runs on tiny images; production passes the machine-resolved
/// value from `integration::band_budget::resolve_budget_bytes`.
#[allow(clippy::too_many_arguments)]
fn run_banded(
    src: &mut BandSource,
    scales: &[f32],
    precal: Option<&FlatPrecal>,
    recipe: IntegrationRecipe,
    pool: &rayon::ThreadPool,
    cancel: &AtomicBool,
    progress: &EngineProgress<'_>,
    band_budget_bytes: usize,
) -> Result<IntegrationOutput, IntegrationError> {
    use rayon::prelude::*;
    let (w, h, n) = (src.width(), src.height(), src.frame_count());
    let band_rows = src.band_rows_for_budget(band_budget_bytes).min(h);
    let bands_total = h.div_ceil(band_rows);
    // Computed once, next to `band_rows`, and referenced from every band's
    // progress call below — this run's own share of the work (a flat's pass 1
    // has already happened by the time `run_banded` is called for pass 2, and
    // `integrate_flat_inner` adds its bytes on top via a wrapped `on_band`).
    let per_row_bytes = src.bytes_per_row();
    let bytes_total = (h * per_row_bytes) as u64;
    let mut out = vec![0f32; w * h];
    let rejected = AtomicUsize::new(0);
    // Non-finite accounting (audit C2). Plain counters shared across the rayon
    // rows, so Relaxed is enough — nothing else is published through them, and
    // the reads below happen after every worker has joined.
    let bad_samples: Vec<AtomicUsize> = (0..n).map(|_| AtomicUsize::new(0)).collect();
    let all_bad = AtomicUsize::new(0);
    let mut planes = BandPlanes::new(src);
    let mut read_duration = std::time::Duration::ZERO;
    let mut combine_duration = std::time::Duration::ZERO;
    let mut bytes_read: u64 = 0;

    for (band_idx, y0) in (0..h).step_by(band_rows).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(IntegrationError::Cancelled);
        }
        let rows = band_rows.min(h - y0);
        let t_read = std::time::Instant::now();
        src.read_band(y0, rows, &mut planes)?;
        read_duration += t_read.elapsed();
        bytes_read += (rows * per_row_bytes) as u64;

        // `y0` (the band's first global row) is captured by the closure below
        // for the precal MasterFrame row index (`gy = y0 + row_in_band`). It
        // must stay in scope for the closure's lifetime — do not hoist the
        // closure out of this `for` loop or split it into a free function
        // without threading `y0` through explicitly.
        let t_combine = std::time::Instant::now();
        let out_band = &mut out[y0 * w..(y0 + rows) * w];
        pool.install(|| {
            out_band
                .par_chunks_mut(w)                       // one row per work item
                .enumerate()
                .for_each(|(row_in_band, out_row)| {
                    let mut column: Vec<f32> = Vec::with_capacity(n);
                    for (x, out_px) in out_row.iter_mut().enumerate() {
                        column.clear();
                        let idx = row_in_band * w + x;
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
                        }
                    }
                });
        });
        combine_duration += t_combine.elapsed();
        (progress.on_band)(band_idx + 1, bands_total, bytes_read, bytes_total);
    }

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
        read_duration,
        combine_duration,
        band_rows,
        bands: bands_total,
        bytes_read,
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
    band_budget_bytes: usize,
) -> Result<IntegrationOutput, IntegrationError> {
    integrate_bias_like_inner(paths, recipe, pool, scratch_dir, cancel, progress, band_budget_bytes)
}

fn integrate_bias_like_inner(
    paths: &[PathBuf],
    recipe: IntegrationRecipe,
    pool: &rayon::ThreadPool,
    scratch_dir: &Path,
    cancel: &AtomicBool,
    progress: EngineProgress<'_>,
    band_budget_bytes: usize,
) -> Result<IntegrationOutput, IntegrationError> {
    let mut src = BandSource::open(paths, scratch_dir)?;
    let scales = vec![1.0f32; src.frame_count()];
    run_banded(&mut src, &scales, None, recipe, pool, cancel, &progress, band_budget_bytes)
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
    band_budget_bytes: usize,
) -> Result<IntegrationOutput, IntegrationError> {
    integrate_flat_inner(paths, precal, recipe, pool, scratch_dir, cancel, progress, band_budget_bytes)
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
    band_budget_bytes: usize,
) -> Result<IntegrationOutput, IntegrationError> {
    let mut src = BandSource::open(paths, scratch_dir)?;
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
    let band_rows = src.band_rows_for_budget(band_budget_bytes).min(cy1 - cy0);
    let mut planes = BandPlanes::new(&src);
    // Computed once, next to `band_rows` — pass 1 only ever reads the
    // central-third rows, so its share of the total is that row count, not
    // the full height `run_banded` (pass 2, below) will report on its own.
    let per_row_bytes = src.bytes_per_row();
    let pass1_total_bytes = ((cy1 - cy0) * per_row_bytes) as u64;
    let mut pass1_read = std::time::Duration::ZERO;
    let mut pass1_bytes_read: u64 = 0;
    let mut y = cy0;
    while y < cy1 {
        if cancel.load(Ordering::Relaxed) { return Err(IntegrationError::Cancelled); }
        let rows = band_rows.min(cy1 - y);
        let t_read = std::time::Instant::now();
        src.read_band(y, rows, &mut planes)?;
        pass1_read += t_read.elapsed();
        pass1_bytes_read += (rows * per_row_bytes) as u64;
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
    // takes `&mut self`, so pass 1 (above) and pass 2 reuse the SAME `src` —
    // readers just seek back to the start and stream again; no need to reopen.
    //
    // `run_banded` only knows its own (pass 2) height, not pass 1's
    // central-third read that already happened above it — wrap the caller's
    // `on_band` so the byte pair it sees spans both passes.
    let wrapped_on_band = |cur: usize, total: usize, bytes_done: u64, bytes_total: u64| {
        (progress.on_band)(cur, total, pass1_bytes_read + bytes_done, pass1_total_bytes + bytes_total);
    };
    let wrapped_progress = EngineProgress { on_band: &wrapped_on_band };
    let mut out = run_banded(&mut src, &scales, Some(precal), recipe, pool, cancel, &wrapped_progress, band_budget_bytes)?;
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
    use crate::integration::combine::{IntegrationRecipe, Rejection};
    use std::sync::atomic::AtomicBool;

    fn pool() -> rayon::ThreadPool {
        rayon::ThreadPoolBuilder::new().num_threads(2).build().unwrap()
    }
    fn nop() -> impl Fn(usize, usize, u64, u64) { |_, _, _, _| {} }

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
            EngineProgress { on_band: &on_band },
            MIN_BUDGET_BYTES,
        ).unwrap();
        assert_eq!((out.width, out.height), (w, h));
        let hot = out.data[5 * w + 5];
        assert!(hot < 200.0, "hot pixel must be rejected, got {hot}");
        assert!(out.rejected_fraction > 0.0);
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
            EngineProgress { on_band: &on_band },
            MIN_BUDGET_BYTES,
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
            EngineProgress { on_band: &on_band },
            MIN_BUDGET_BYTES,
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
            EngineProgress { on_band: &on_band },
            MIN_BUDGET_BYTES,
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
            EngineProgress { on_band: &on_band },
            MIN_BUDGET_BYTES,
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
            EngineProgress { on_band: &on_band },
            MIN_BUDGET_BYTES,
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
            EngineProgress { on_band: &on_band },
            1, // band_rows_for_budget(1) floors to 1 row/band => 48 bands
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
            EngineProgress { on_band: &on_band },
            MIN_BUDGET_BYTES,
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
            EngineProgress { on_band: &on_band },
            MIN_BUDGET_BYTES,
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
            EngineProgress { on_band: &on_band },
            MIN_BUDGET_BYTES,
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
            EngineProgress { on_band: &on_band },
            MIN_BUDGET_BYTES,
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
        // band_rows_for_budget's per-row cost is (frames+2)*width*4 =
        // (3+2)*32*4 = 640; budget 12_800 -> band_rows = 12_800/640 = 20
        // exactly, so the 48-row image runs as 20/20/8-row bands.
        let out = integrate_bias_like(
            &paths,
            IntegrationRecipe::median(Rejection::None),
            &pool(),
            dir.path(),
            &AtomicBool::new(false),
            EngineProgress { on_band: &on_band },
            12_800,
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
            EngineProgress { on_band: &on_band },
            MIN_BUDGET_BYTES,
        )
        .unwrap();
        assert_eq!(
            out.bytes_read,
            ((48 + 16) * 32 * 4 * 3) as u64,
            "pass 1 (16 central rows) + pass 2 (48 full rows), 3 frames, 4 bytes/sample"
        );
    }

    /// Review fix-round-1 finding I1 (judgment call #2): the `on_band`
    /// wrapper in `integrate_flat_inner` must report a byte pair that spans
    /// BOTH passes, not just pass 2's own share — `run_banded` has no way to
    /// see pass 1's read on its own. Forces a multi-band pass 2
    /// (`band_budget_bytes = 1` clamps to 1-row bands, so `on_band` fires 48
    /// times) so the wrapping is exercised across more than a single call.
    #[test]
    fn flat_progress_bytes_span_both_passes_via_the_wrapped_callback() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (32, 48);
        let paths = vec![
            write(dir.path(), "f1.fits", w, h, |_, _| 1000.0),
            write(dir.path(), "f2.fits", w, h, |_, _| 1000.0),
            write(dir.path(), "f3.fits", w, h, |_, _| 1000.0),
        ];
        let calls: std::cell::RefCell<Vec<(usize, usize, u64, u64)>> = std::cell::RefCell::new(Vec::new());
        let on_band = |cur: usize, total: usize, done: u64, all: u64| {
            calls.borrow_mut().push((cur, total, done, all));
        };
        integrate_flat_inner(
            &paths,
            &FlatPrecal::None,
            IntegrationRecipe::median(Rejection::None),
            &pool(),
            dir.path(),
            &AtomicBool::new(false),
            EngineProgress { on_band: &on_band },
            1,
        )
        .unwrap();
        let calls = calls.into_inner();
        assert!(!calls.is_empty(), "on_band must fire at least once");
        // Pass 1 (16 rows) + pass 2 (48 rows), 3 frames, 4 bytes/sample —
        // same total the previous test pins on the returned field.
        let expected_total = ((48 + 16) * 32 * 4 * 3) as u64;
        for &(_, _, _done, all) in &calls {
            assert_eq!(all, expected_total, "bytes_total reported to on_band must span both passes");
        }
        let (_, _, last_done, _) = *calls.last().unwrap();
        assert_eq!(
            last_done, expected_total,
            "after the final band, bytes_read_so_far must equal the full two-pass total"
        );
        // The very first pass-2 band call must already carry pass 1's bytes
        // as a baseline — otherwise the wrapping would just be re-reporting
        // pass 2 alone against a bigger denominator.
        let pass1_bytes = 16u64 * 32 * 4 * 3;
        let (_, _, first_done, _) = calls[0];
        assert!(
            first_done > pass1_bytes,
            "first band's bytes_done ({first_done}) must include pass 1's {pass1_bytes} bytes as a baseline"
        );
    }
}
