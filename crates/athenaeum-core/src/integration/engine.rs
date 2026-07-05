//! Recipe orchestration (spec §4, §9): banded streaming + per-pixel combine.
//! Memory: N × band (default 256 MiB budget). Parallelism: rayon over the
//! pixels of the current band via the shared image pool.

use super::banded::{band_rows_for_budget, BandSource};
use super::combine::{combine_pixel, CombineMethod};
use super::IntegrationError;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

// Shared band-memory budget (§4). `pub(crate)` so the light-calibration engine
// (`calibration_library::light_cal`) sizes its bands with the same policy
// instead of redefining the constant.
pub(crate) const BAND_BUDGET_BYTES: usize = 256 * 1024 * 1024;

pub struct IntegrationOutput {
    pub width: usize,
    pub height: usize,
    pub data: Vec<f32>,
    pub rejected_fraction: f64,
    pub flat_norm: Option<f64>,
}

pub struct EngineProgress<'a> {
    pub on_band: &'a dyn Fn(usize, usize),
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
/// multi-band runs on tiny images; production passes `BAND_BUDGET_BYTES`.
#[allow(clippy::too_many_arguments)]
fn run_banded(
    src: &mut BandSource,
    scales: &[f32],
    precal: Option<&FlatPrecal>,
    method: CombineMethod,
    pool: &rayon::ThreadPool,
    cancel: &AtomicBool,
    progress: &EngineProgress<'_>,
    band_budget_bytes: usize,
) -> Result<IntegrationOutput, IntegrationError> {
    use rayon::prelude::*;
    let (w, h, n) = (src.width(), src.height(), src.frame_count());
    let band_rows = band_rows_for_budget(w, n, band_budget_bytes).min(h);
    let bands_total = h.div_ceil(band_rows);
    let mut out = vec![0f32; w * h];
    let rejected = AtomicUsize::new(0);
    let mut band_bufs: Vec<Vec<f32>> = vec![Vec::new(); n];

    for (band_idx, y0) in (0..h).step_by(band_rows).enumerate() {
        if cancel.load(Ordering::Relaxed) {
            return Err(IntegrationError::Cancelled);
        }
        let rows = band_rows.min(h - y0);
        src.read_band(y0, rows, &mut band_bufs)?;

        // `y0` (the band's first global row) is captured by the closure below
        // for the precal MasterFrame row index (`gy = y0 + row_in_band`). It
        // must stay in scope for the closure's lifetime — do not hoist the
        // closure out of this `for` loop or split it into a free function
        // without threading `y0` through explicitly.
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
                        for (i, frame) in band_bufs.iter().enumerate() {
                            let mut v = frame[idx];
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
                            column.push(v);
                        }
                        let (val, rej) = combine_pixel(&mut column, method);
                        *out_px = val;
                        if rej > 0 { rejected.fetch_add(rej, Ordering::Relaxed); }
                    }
                });
        });
        (progress.on_band)(band_idx + 1, bands_total);
    }

    let total_samples = (w * h * n).max(1);
    Ok(IntegrationOutput {
        width: w,
        height: h,
        data: out,
        rejected_fraction: rejected.load(Ordering::Relaxed) as f64 / total_samples as f64,
        flat_norm: None,
    })
}

pub fn integrate_bias_like(
    paths: &[PathBuf],
    method: CombineMethod,
    pool: &rayon::ThreadPool,
    scratch_dir: &Path,
    cancel: &AtomicBool,
    progress: EngineProgress<'_>,
) -> Result<IntegrationOutput, IntegrationError> {
    integrate_bias_like_inner(paths, method, pool, scratch_dir, cancel, progress, BAND_BUDGET_BYTES)
}

fn integrate_bias_like_inner(
    paths: &[PathBuf],
    method: CombineMethod,
    pool: &rayon::ThreadPool,
    scratch_dir: &Path,
    cancel: &AtomicBool,
    progress: EngineProgress<'_>,
    band_budget_bytes: usize,
) -> Result<IntegrationOutput, IntegrationError> {
    let mut src = BandSource::open(paths, scratch_dir)?;
    let scales = vec![1.0f32; src.frame_count()];
    run_banded(&mut src, &scales, None, method, pool, cancel, &progress, band_budget_bytes)
}

pub fn integrate_flat(
    paths: &[PathBuf],
    precal: &FlatPrecal,
    method: CombineMethod,
    pool: &rayon::ThreadPool,
    scratch_dir: &Path,
    cancel: &AtomicBool,
    progress: EngineProgress<'_>,
) -> Result<IntegrationOutput, IntegrationError> {
    integrate_flat_inner(paths, precal, method, pool, scratch_dir, cancel, progress, BAND_BUDGET_BYTES)
}

#[allow(clippy::too_many_arguments)]
fn integrate_flat_inner(
    paths: &[PathBuf],
    precal: &FlatPrecal,
    method: CombineMethod,
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
    let band_rows = band_rows_for_budget(w, n, band_budget_bytes).min(cy1 - cy0);
    let mut band_bufs: Vec<Vec<f32>> = vec![Vec::new(); n];
    let mut y = cy0;
    while y < cy1 {
        if cancel.load(Ordering::Relaxed) { return Err(IntegrationError::Cancelled); }
        let rows = band_rows.min(cy1 - y);
        src.read_band(y, rows, &mut band_bufs)?;
        for (i, frame) in band_bufs.iter().enumerate() {
            for r in 0..rows {
                let gy = y + r;
                for x in cx0..cx1 {
                    let mut v = frame[r * w + x] as f64;
                    match precal {
                        FlatPrecal::MasterFrame { data, width, .. } => v -= data[gy * *width + x] as f64,
                        FlatPrecal::SyntheticBias(b) => v -= *b as f64,
                        FlatPrecal::None => {}
                    }
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
    let mut out = run_banded(&mut src, &scales, Some(precal), method, pool, cancel, &progress, band_budget_bytes)?;
    out.flat_norm = Some(central_third_mean(&out.data, w, h));
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::write_fits_f32;
    use crate::integration::combine::CombineMethod;
    use std::sync::atomic::AtomicBool;

    fn pool() -> rayon::ThreadPool {
        rayon::ThreadPoolBuilder::new().num_threads(2).build().unwrap()
    }
    fn nop() -> impl Fn(usize, usize) { |_, _| {} }

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
            CombineMethod::WinsorizedSigmaClip { sigma_low: 3.0, sigma_high: 3.0 },
            &pool(), dir.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &on_band },
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
            &paths, &FlatPrecal::None, CombineMethod::Median,
            &pool(), dir.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &on_band },
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
            &paths, &precal, CombineMethod::Median,
            &pool(), dir.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &on_band },
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
            &paths, &FlatPrecal::SyntheticBias(100.0), CombineMethod::Median,
            &pool(), dir.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &on_band },
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
            &paths, CombineMethod::Mean, &pool(), dir.path(), &cancel,
            EngineProgress { on_band: &on_band },
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
            &paths, CombineMethod::Mean, &pool(), dir.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &on_band },
        ).unwrap();
        assert!(out.data.iter().all(|&v| v == -5.0), "no clipping policy");
    }

    /// Multi-band precal row indexing: with band_budget_bytes=1 the budget
    /// clamps to 16-row bands, so h=48 runs as 3 bands. The master is a row
    /// gradient (master[y] = y), the flats are 1000 + y, so after subtraction
    /// every sample is exactly 1000.0 — but ONLY if the MasterFrame index uses
    /// the GLOBAL row (`gy = y0 + row_in_band`). A regression that drops `y0`
    /// reuses master rows 0..16 in bands 2 and 3 (output rows 16.. become
    /// 1000 + 16k) and fails here while all single-band tests still pass.
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
            &paths, &precal, CombineMethod::Median,
            &pool(), dir.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &on_band },
            1, // band_rows_for_budget(..).max(16) => 16-row bands => 3 bands
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
            &paths, &FlatPrecal::SyntheticBias(500.0), CombineMethod::Mean,
            &pool(), dir.path(), &AtomicBool::new(false),
            EngineProgress { on_band: &on_band },
            BAND_BUDGET_BYTES,
        ).unwrap();
        assert!(
            out.data.iter().all(|&v| (v - 1500.0).abs() < 1e-3),
            "every normalized sample must be (v - 500) * scale = 1500; got {}",
            out.data[0]
        );
    }
}
