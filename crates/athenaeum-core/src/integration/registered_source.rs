//! A `FrameSource` that resamples calibrated frames through their
//! registration transforms as bands are requested — the hybrid
//! materialization of spec §0/§6.1: transforms persist, pixels do not.
//! Each band: map the band boundary back into every frame, read only that
//! row window, warp it into the band, hand the engine native f32.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Mutex;

use tracing::{debug, warn};

use super::banded::{BandPlanes, PlaneKind};
use super::plane_reader::PlaneReader;
use super::source::FrameSource;
use super::IntegrationError;
use crate::geometry::PixelMap;
use crate::resample::{source_window, warp_rows, Interpolation, Plane, SourceWindow};

pub struct RegisteredFrame {
    pub path: PathBuf,
    /// Subject → reference mapping (its inverse is what the warp uses).
    pub map: PixelMap,
}

pub struct RegisteredSource {
    frames: Vec<(PlaneReader, PixelMap)>,
    width: usize,
    height: usize,
    channels: usize,
    plane: usize,
    interp: Interpolation,
    clamping: f32,
    whole_threshold: f64,
}

impl RegisteredSource {
    /// Opens every frame; all must share one channel count and `plane`
    /// must exist in it. `ref_width × ref_height` is the output geometry
    /// (the reference frame's).
    pub fn open(
        frames: &[RegisteredFrame],
        ref_width: usize,
        ref_height: usize,
        plane: usize,
        interp: Interpolation,
        clamping: f32,
    ) -> Result<RegisteredSource, IntegrationError> {
        if frames.is_empty() {
            return Err(IntegrationError::BadInput("empty frame list".into()));
        }
        if ref_width == 0 || ref_height == 0 {
            return Err(IntegrationError::BadInput(
                "reference geometry must be non-zero".into(),
            ));
        }
        let mut opened = Vec::with_capacity(frames.len());
        let mut channels = None;
        for f in frames {
            let r = PlaneReader::open(&f.path)?;
            match channels {
                None => channels = Some(r.channels()),
                Some(c) if c != r.channels() => {
                    return Err(IntegrationError::BadInput(format!(
                        "{}: {} planes, the set has {c}",
                        f.path.display(),
                        r.channels()
                    )))
                }
                _ => {}
            }
            opened.push((r, f.map.clone()));
        }
        let channels = channels.unwrap_or(1);
        if plane >= channels {
            return Err(IntegrationError::BadInput(format!(
                "plane {plane} of a {channels}-plane set"
            )));
        }
        Ok(RegisteredSource {
            frames: opened,
            width: ref_width,
            height: ref_height,
            channels,
            plane,
            interp,
            clamping,
            whole_threshold: 0.6,
        })
    }

    /// Hands back every frame's displacement grid (ruling R-T4-6c).
    ///
    /// Called from [`Drop`], which is the right moment for all three
    /// stages that warp through this type, at the granularity each of
    /// them actually opens a source:
    ///
    /// - **integration** — one source per GROUP since ruling R-T4-7;
    ///   `integrate_group` re-points it at each plane with
    ///   [`RegisteredSource::set_plane`] instead of reopening, so a
    ///   colour group builds one grid per frame, not one per frame per
    ///   plane;
    /// - **local normalization** — one source per frame per plane
    ///   (`ln::normalize_frame` opens inside its own plane loop), and one
    ///   per member for the reference build;
    /// - and any other caller, when its own source goes out of scope.
    ///
    /// Without it a spline map's grid would outlive the source — every
    /// clone shares ONE cache, and the run's own `GroupMember`/
    /// `StackFrame` clones keep that cache alive to the end of the run, so
    /// "the source was dropped" frees nothing by itself. That is exactly
    /// how 208 frames' inverse grids survived integration and met
    /// drizzle's forward grids on the acceptance run.
    ///
    /// A no-op for linear and polynomial maps, which have no grids.
    pub fn release_grids(&self) -> usize {
        self.frames.iter().map(|(_, m)| m.release_grids()).sum()
    }

    pub fn channels(&self) -> usize {
        self.channels
    }
    pub fn plane(&self) -> usize {
        self.plane
    }

    /// Re-points this source at another plane of the SAME frames —
    /// `integrate_group`'s plane loop (ruling R-T4-7). Reopening instead
    /// would re-open every reader and, since M4c, rebuild every spline
    /// frame's displacement grid for a frame list that has not changed.
    pub fn set_plane(&mut self, plane: usize) -> Result<(), IntegrationError> {
        if plane >= self.channels {
            return Err(IntegrationError::BadInput(format!(
                "plane {plane} of a {}-plane set",
                self.channels
            )));
        }
        self.plane = plane;
        Ok(())
    }

    /// Fraction of a frame's height above which a band reads the whole
    /// plane instead of a window (spec §6.1: 0.6).
    pub fn set_whole_threshold(&mut self, t: f64) {
        self.whole_threshold = t.clamp(0.05, 1.0);
    }

    /// Resample one frame's band into `dst` (`rows × width` f32). Returns
    /// the source bytes read. `raw_scratch`/`src_buf` are a per-WORKER pair
    /// (Task 3, Plan 4) the caller reuses across every frame/band it fills —
    /// grown, never freed, instead of a fresh f32 allocation on every call.
    /// The `Whole` window fallback (a full-height read) shares the same
    /// buffers, just at their largest extent.
    fn fill_frame(
        &self,
        i: usize,
        y0: usize,
        rows: usize,
        dst: &mut [f32],
        raw_scratch: &mut Vec<u8>,
        src_buf: &mut Vec<f32>,
    ) -> Result<u64, IntegrationError> {
        let (reader, map) = &self.frames[i];
        let (sw, sh) = (reader.width(), reader.height());
        let window = source_window(
            map,
            self.width,
            y0,
            rows,
            sw,
            sh,
            self.interp.radius(),
            self.whole_threshold,
        );
        let (sy0, sy1) = match window {
            SourceWindow::Rows { y0, y1 } => (y0, y1),
            SourceWindow::Whole => (0, sh),
        };
        let src_rows = sy1.saturating_sub(sy0);
        if src_rows == 0 {
            dst.fill(f32::NAN);
            debug!(frame = i, y0, rows, "band maps outside the source");
            return Ok(0);
        }
        src_buf.resize(src_rows * sw, 0.0);
        reader.read_rows_with_scratch(self.plane, sy0, src_rows, &mut src_buf[..src_rows * sw], raw_scratch)?;
        let plane = Plane {
            data: &src_buf[..src_rows * sw],
            width: sw,
            height: src_rows,
            y_offset: sy0,
            full_height: sh,
        };
        warp_rows(
            &plane,
            map,
            self.width,
            y0,
            rows,
            self.interp,
            self.clamping,
            dst,
        );
        Ok((src_rows * sw * reader.kind().bytes_per_sample()) as u64)
    }
}

impl Drop for RegisteredSource {
    /// See [`RegisteredSource::release_grids`] — the stage that opened
    /// this source is done warping through it.
    fn drop(&mut self) {
        self.release_grids();
    }
}

fn store_f32_le(buf: &mut Vec<u8>, samples: &[f32]) {
    buf.resize(samples.len() * 4, 0);
    for (chunk, v) in buf.chunks_exact_mut(4).zip(samples.iter()) {
        chunk.copy_from_slice(&v.to_le_bytes());
    }
}

impl FrameSource for RegisteredSource {
    fn width(&self) -> usize {
        self.width
    }
    fn height(&self) -> usize {
        self.height
    }
    fn frame_count(&self) -> usize {
        self.frames.len()
    }
    fn plane_kinds(&self) -> Vec<PlaneKind> {
        vec![PlaneKind::F32Le; self.frames.len()]
    }
    fn bytes_per_row(&self) -> usize {
        self.frames.len() * self.width * 4
    }

    /// Charges the output band AND the f32 scratch the resample writes first
    /// (twice `bytes_per_row`); the `Whole` window fallback additionally holds
    /// one f32 source plane per in-flight worker, which is not budgeted here —
    /// the engine's per-job budget (a quarter of RAM divided by the compute
    /// concurrency) is what leaves room for it.
    fn band_rows_for_budget(&self, budget_bytes: usize) -> usize {
        let per_row = self
            .bytes_per_row()
            .saturating_mul(2)
            .saturating_add(self.width.saturating_mul(8))
            .max(1);
        (budget_bytes / per_row).max(1)
    }

    fn read_band_with_progress(
        &self,
        y0: usize,
        rows: usize,
        out: &mut BandPlanes,
        concurrency: usize,
        on_bytes: &(dyn Fn(u64) + Sync),
        cancel: &AtomicBool,
    ) -> Result<(), IntegrationError> {
        if y0 + rows > self.height {
            return Err(IntegrationError::BadInput(format!(
                "band {y0}+{rows} beyond height {}",
                self.height
            )));
        }
        assert_eq!(
            out.width(),
            self.width,
            "BandPlanes built from a different source"
        );
        out.set_rows(rows);
        let n = self.frames.len();
        let workers = concurrency.max(1).min(n);
        let w = self.width;
        // Resample into per-frame f32 scratch first, then store; the store
        // is the only place that touches `out`, so the parallel part never
        // needs a mutable borrow of it.
        let mut scratch: Vec<Vec<f32>> = (0..n).map(|_| vec![0f32; rows * w]).collect();
        // `on_bytes` reports the engine's own accounted share (`rows ×
        // bytes_per_row`, see the `FrameSource::read_band_with_progress`
        // contract) — never the true disk traffic, which can differ (a
        // `Whole`-window read, a narrower BITPIX). The real figure is kept
        // here only for the debug log below.
        let disk_bytes = AtomicU64::new(0);
        if workers == 1 {
            // A single worker's scratch pair, reused across every frame in
            // the band (Task 3, Plan 4) — see `fill_frame`'s doc.
            let mut raw_scratch: Vec<u8> = Vec::new();
            let mut src_buf: Vec<f32> = Vec::new();
            for (i, dst) in scratch.iter_mut().enumerate() {
                if cancel.load(Ordering::Relaxed) {
                    return Err(IntegrationError::Cancelled);
                }
                let disk = self.fill_frame(i, y0, rows, dst, &mut raw_scratch, &mut src_buf)?;
                disk_bytes.fetch_add(disk, Ordering::Relaxed);
                on_bytes((rows * self.width * 4) as u64);
            }
        } else {
            let first_err: Mutex<Option<IntegrationError>> = Mutex::new(None);
            let abort = AtomicBool::new(false);
            // A worker that bails because `cancel` (or `abort`, set only by an
            // erroring worker and therefore already covered by the error path
            // below) is raised must be reported as `Cancelled`, never as `Ok`
            // over a half-filled `scratch` — relying on the post-join re-check
            // to catch a flag that could, in principle, have been lowered
            // again would leave the guarantee resting on an unwritten
            // assumption. `saw_cancel` makes it structural: any worker that
            // actually saw the cancel stamps it before returning, and the
            // final check ORs that in alongside a fresh read of `cancel`.
            let saw_cancel = AtomicBool::new(false);
            let mut groups: Vec<Vec<(usize, &mut Vec<f32>)>> =
                (0..workers).map(|_| Vec::new()).collect();
            for (i, dst) in scratch.iter_mut().enumerate() {
                groups[i % workers].push((i, dst));
            }
            std::thread::scope(|scope| {
                for group in groups {
                    let first_err = &first_err;
                    let abort = &abort;
                    let saw_cancel = &saw_cancel;
                    let disk_bytes = &disk_bytes;
                    scope.spawn(move || {
                        // This worker's own scratch pair (Task 3, Plan 4) —
                        // never shared with another thread's group.
                        let mut raw_scratch: Vec<u8> = Vec::new();
                        let mut src_buf: Vec<f32> = Vec::new();
                        for (i, dst) in group {
                            if cancel.load(Ordering::Relaxed) || abort.load(Ordering::Relaxed) {
                                saw_cancel.store(true, Ordering::Relaxed);
                                return;
                            }
                            match self.fill_frame(i, y0, rows, dst, &mut raw_scratch, &mut src_buf) {
                                Ok(bytes) => {
                                    disk_bytes.fetch_add(bytes, Ordering::Relaxed);
                                    on_bytes((rows * w * 4) as u64);
                                }
                                Err(e) => {
                                    abort.store(true, Ordering::Relaxed);
                                    let mut slot = first_err.lock().unwrap();
                                    if slot.is_none() {
                                        *slot = Some(e);
                                    } else {
                                        warn!(
                                            frame = i,
                                            error = %e,
                                            "registered band worker error discarded, an earlier error is reported"
                                        );
                                    }
                                    return;
                                }
                            }
                        }
                    });
                }
            });
            if let Some(e) = first_err.into_inner().unwrap() {
                return Err(e);
            }
            if saw_cancel.load(Ordering::Relaxed) || cancel.load(Ordering::Relaxed) {
                return Err(IntegrationError::Cancelled);
            }
        }
        for (i, s) in scratch.iter().enumerate() {
            store_f32_le(out.buf_mut(i), s);
        }
        debug!(
            y0,
            rows,
            frames = n,
            workers,
            disk_bytes = disk_bytes.load(Ordering::Relaxed),
            "registered band resampled"
        );
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::write_fits_f32;
    use crate::geometry::{Linear, LinearKind, PixelMap};
    use crate::integration::combine::{IntegrationRecipe, Rejection};
    use crate::integration::engine::{integrate_registered, EngineProgress};
    use crate::integration::io_policy::IoPolicy;
    use crate::integration::source::FrameSource;
    use crate::integration::storage_class::StorageClass;
    use crate::resample::Interpolation;
    use crate::test_support::{centroid, gaussian_field};
    use std::sync::atomic::AtomicBool;

    const W: usize = 200;
    const H: usize = 150;
    const BG: f32 = 100.0;
    const STARS: [(f64, f64, f64); 4] = [
        (40.3, 30.7, 4000.0),
        (120.0, 60.0, 2500.0),
        (170.6, 120.2, 6000.0),
        (60.0, 110.0, 3000.0),
    ];
    const SHIFTS: [(f64, f64); 3] = [(0.0, 0.0), (0.4, -0.7), (-12.3, 5.6)];

    /// Subject frame i holds the stars at `(x + dx_i, y + dy_i)`; its forward
    /// map (subject → reference) subtracts the shift.
    fn frames(dir: &std::path::Path) -> Vec<RegisteredFrame> {
        SHIFTS
            .iter()
            .enumerate()
            .map(|(i, &(dx, dy))| {
                let stars: Vec<_> = STARS.iter().map(|&(x, y, a)| (x + dx, y + dy, a)).collect();
                let data = gaussian_field(W, H, &stars, 1.8, BG);
                let path = dir.join(format!("sub{i}.fits"));
                write_fits_f32(&path, W, H, 1, &data, &[]).unwrap();
                let fwd = Linear {
                    kind: LinearKind::Affine,
                    m: [[1.0, 0.0, -dx], [0.0, 1.0, -dy], [0.0, 0.0, 1.0]],
                };
                RegisteredFrame {
                    path,
                    map: PixelMap::linear(fwd).unwrap(),
                }
            })
            .collect()
    }

    /// Ruling R-T4-6c/d: a source that has warped a band holds its frames'
    /// displacement grids, and dropping it hands them back — observed on
    /// the CALLER's own clone of the map, which is the only thing that
    /// makes the release meaningful (every clone shares one cache, and the
    /// run keeps clones of its own until the run ends).
    ///
    /// This is the pin for the integration and local-normalization halves
    /// of the release; the writer and drizzle have their own.
    #[test]
    fn dropping_a_source_hands_back_its_frames_displacement_grids() {
        let _quiet = crate::geometry::pixel_map::grid_counters::exclusive();
        use crate::geometry::{DistortionModel, ThinPlateSpline};

        let dir = tempfile::tempdir().unwrap();
        let data = gaussian_field(W, H, &STARS, 1.8, BG);
        let path = dir.path().join("spline.fits");
        write_fits_f32(&path, W, H, 1, &data, &[]).unwrap();

        // A spline map — the only arm with a grid to release.
        let nodes: Vec<(f64, f64)> = (0..36)
            .map(|i| (18.0 + (i % 6) as f64 * 30.0, 16.0 + (i / 6) as f64 * 22.0))
            .collect();
        let dx: Vec<f64> = nodes.iter().map(|(x, _)| 0.3 * (x / 40.0).sin()).collect();
        let dy: Vec<f64> = nodes.iter().map(|(_, y)| 0.2 * (y / 35.0).cos()).collect();
        let ndx: Vec<f64> = dx.iter().map(|v| -v).collect();
        let ndy: Vec<f64> = dy.iter().map(|v| -v).collect();
        let model = DistortionModel::tps(
            ThinPlateSpline::fit(&nodes, &dx, &dy, 0.0).unwrap(),
            ThinPlateSpline::fit(&nodes, &ndx, &ndy, 0.0).unwrap(),
            [0.0, 0.0, W as f64, H as f64],
        );
        let map = PixelMap::with_distortion_model(Linear::identity(), model).unwrap();
        assert_eq!(
            map.distortion.as_ref().unwrap().grids_built(),
            (false, false)
        );

        {
            let src = RegisteredSource::open(
                &[RegisteredFrame {
                    path: path.clone(),
                    map: map.clone(),
                }],
                W,
                H,
                0,
                Interpolation::BicubicBSpline,
                0.3,
            )
            .unwrap();
            let mut band = BandPlanes::new(&src);
            src.read_band_with_progress(0, 16, &mut band, 1, &|_| {}, &AtomicBool::new(false))
                .unwrap();
            assert_eq!(
                map.distortion.as_ref().unwrap().grids_built(),
                (false, true),
                "warping a band must have built the INVERSE grid, and only it"
            );
        }

        assert_eq!(
            map.distortion.as_ref().unwrap().grids_built(),
            (false, false),
            "dropping the source must hand the grid back to every clone"
        );
    }

    /// Ruling R-T4-7: walking a colour source's planes through
    /// [`RegisteredSource::set_plane`] builds ONE displacement grid per
    /// frame, not one per frame per plane.
    ///
    /// `set_plane` had no production caller at all until this round —
    /// `integrate_group` reopened a source per plane, which re-opened every
    /// reader and (since M4c) rebuilt every spline frame's grid twice over
    /// for a frame list that had not changed. The run-level pin cannot see
    /// this: its group is mono, so its `builds` count is identical either
    /// way; the saving is exactly ×3 on colour data, which is what this
    /// measures.
    #[test]
    fn walking_the_planes_of_one_source_builds_one_grid_per_frame() {
        use crate::geometry::pixel_map::grid_counters;
        use crate::geometry::{DistortionModel, ThinPlateSpline};

        let _quiet = grid_counters::exclusive();
        let dir = tempfile::tempdir().unwrap();
        let one = gaussian_field(W, H, &STARS, 1.8, BG);
        let three: Vec<f32> = one.iter().chain(&one).chain(&one).copied().collect();
        let path = dir.path().join("rgb.fits");
        write_fits_f32(&path, W, H, 3, &three, &[]).unwrap();

        let nodes: Vec<(f64, f64)> = (0..36)
            .map(|i| (18.0 + (i % 6) as f64 * 30.0, 16.0 + (i / 6) as f64 * 22.0))
            .collect();
        let dx: Vec<f64> = nodes.iter().map(|(x, _)| 0.3 * (x / 40.0).sin()).collect();
        let dy: Vec<f64> = nodes.iter().map(|(_, y)| 0.2 * (y / 35.0).cos()).collect();
        let ndx: Vec<f64> = dx.iter().map(|v| -v).collect();
        let ndy: Vec<f64> = dy.iter().map(|v| -v).collect();
        let model = DistortionModel::tps(
            ThinPlateSpline::fit(&nodes, &dx, &dy, 0.0).unwrap(),
            ThinPlateSpline::fit(&nodes, &ndx, &ndy, 0.0).unwrap(),
            [0.0, 0.0, W as f64, H as f64],
        );
        let map = PixelMap::with_distortion_model(Linear::identity(), model).unwrap();

        let before = grid_counters::snapshot().0;
        {
            let mut src = RegisteredSource::open(
                &[RegisteredFrame {
                    path: path.clone(),
                    map: map.clone(),
                }],
                W,
                H,
                0,
                Interpolation::BicubicBSpline,
                0.3,
            )
            .unwrap();
            assert_eq!(src.channels(), 3);
            for p in 0..3 {
                src.set_plane(p).unwrap();
                let mut band = BandPlanes::new(&src);
                src.read_band_with_progress(0, 16, &mut band, 1, &|_| {}, &AtomicBool::new(false))
                    .unwrap();
            }
        }
        let built = grid_counters::snapshot().0 - before;
        assert_eq!(
            built, 1,
            "three planes of one source must share ONE grid, built {built}"
        );
        assert_eq!(
            map.distortion.as_ref().unwrap().grids_built(),
            (false, false)
        );
    }

    fn io(budget: usize) -> IoPolicy {
        IoPolicy {
            band_budget_bytes: budget,
            read_concurrency: 2,
            storage: StorageClass::Local,
        }
    }

    #[test]
    fn source_reports_reference_geometry_and_f32_planes() {
        let dir = tempfile::tempdir().unwrap();
        let mut src =
            RegisteredSource::open(&frames(dir.path()), W, H, 0, Interpolation::Lanczos3, 0.3)
                .unwrap();
        assert_eq!(
            (src.width(), src.height(), src.frame_count(), src.channels()),
            (W, H, 3, 1)
        );
        assert_eq!(src.bytes_per_row(), 3 * W * 4);
        // 3 frames × 200 px × 4 B × 2 (out + scratch) + 200 × 8 B headroom
        // = 6400 B per row.
        assert_eq!(src.band_rows_for_budget(80_000), 12);
        assert!(src
            .plane_kinds()
            .iter()
            .all(|k| matches!(k, PlaneKind::F32Le)));
        assert!(matches!(
            src.set_plane(1),
            Err(IntegrationError::BadInput(_))
        ));
    }

    #[test]
    fn a_band_read_equals_a_full_warp_of_each_frame() {
        let dir = tempfile::tempdir().unwrap();
        let fr = frames(dir.path());
        let src = RegisteredSource::open(&fr, W, H, 0, Interpolation::BicubicBSpline, 0.3).unwrap();
        let mut planes = BandPlanes::new(&src);
        let reported = std::sync::atomic::AtomicU64::new(0);
        src.read_band_with_progress(
            37,
            20,
            &mut planes,
            2,
            &|b| {
                reported.fetch_add(b, std::sync::atomic::Ordering::Relaxed);
            },
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(
            reported.load(std::sync::atomic::Ordering::Relaxed),
            20 * W as u64 * 4 * 3,
            "on_bytes must sum to rows × bytes_per_row over the band"
        );
        assert_eq!(planes.rows(), 20);
        for (i, f) in fr.iter().enumerate() {
            let reader = PlaneReader::open(&f.path).unwrap();
            let full = reader.read_plane(0).unwrap();
            let plane = crate::resample::Plane::full(&full, W, H);
            let mut expect = vec![0f32; H * W];
            crate::resample::warp_rows(
                &plane,
                &f.map,
                W,
                0,
                H,
                Interpolation::BicubicBSpline,
                0.3,
                &mut expect,
            );
            let mut got = vec![0f32; 20 * W];
            planes.decode_frame_into(i, &mut got);
            for (k, g) in got.iter().enumerate() {
                let e = expect[37 * W + k];
                assert!(
                    (g.is_nan() && e.is_nan()) || (g - e).abs() < 1e-5,
                    "frame {i} sample {k}: {g} vs {e}"
                );
            }
        }
    }

    #[test]
    fn integrating_shifted_frames_yields_an_aligned_average_with_exact_coverage_accounting() {
        let dir = tempfile::tempdir().unwrap();
        let src =
            RegisteredSource::open(&frames(dir.path()), W, H, 0, Interpolation::Lanczos3, 0.3)
                .unwrap();
        let pool = rayon::ThreadPoolBuilder::new()
            .num_threads(2)
            .build()
            .unwrap();
        let progress = EngineProgress {
            on_band: &|_, _, _, _| {},
            on_combine: &|_, _, _, _| {},
        };
        let out = integrate_registered(
            &src,
            IntegrationRecipe::average(Rejection::None),
            &pool,
            &AtomicBool::new(false),
            progress,
            io(80_000), // 20-row bands → 8 bands, so the window logic is exercised
        )
        .unwrap();
        assert_eq!((out.width, out.height), (W, H));
        assert!(
            out.bands >= 8,
            "expected a multi-band run, got {}",
            out.bands
        );
        for &(sx, sy, _) in &STARS {
            let (cx, cy) = centroid(&out.data, W, sx, sy, 7, BG);
            assert!(
                (cx - sx).abs() < 0.02 && (cy - sy).abs() < 0.02,
                "centroid ({cx},{cy}) vs ({sx},{sy})"
            );
        }
        // Frame 0 covers everything; frame 1 loses the last column (x + 0.4 > 199)
        // and the first row (y − 0.7 < 0): 150 + 200 − 1; frame 2 loses 13
        // columns (x − 12.3 < 0) and 6 rows (y + 5.6 > 149): 13·150 + 6·200 − 13·6.
        assert_eq!(out.bad_samples_per_frame, vec![0, 349, 3072]);
        assert_eq!(out.all_bad_pixels, 0, "frame 0 covers every pixel");
        assert!(out.data.iter().all(|v| v.is_finite()));
        // The frame-0-only corner must equal frame 0's own background.
        assert!((out.data[0] - BG).abs() < 0.5);
    }

    #[test]
    fn cancel_between_frames_is_honoured() {
        let dir = tempfile::tempdir().unwrap();
        let src =
            RegisteredSource::open(&frames(dir.path()), W, H, 0, Interpolation::Bilinear, 0.3)
                .unwrap();
        let mut planes = BandPlanes::new(&src);
        let cancel = AtomicBool::new(true);
        let r = src.read_band_with_progress(0, 10, &mut planes, 1, &|_| {}, &cancel);
        assert!(matches!(r, Err(IntegrationError::Cancelled)));
    }

    #[test]
    fn parallel_cancel_never_returns_a_half_filled_band() {
        let dir = tempfile::tempdir().unwrap();
        let src =
            RegisteredSource::open(&frames(dir.path()), W, H, 0, Interpolation::Bilinear, 0.3)
                .unwrap();
        let mut planes = BandPlanes::new(&src);
        let cancel = AtomicBool::new(true);
        // Three workers, the flag already raised: every worker skips its
        // frames, and the skip must surface as Cancelled — never as an Ok
        // over zero-filled buffers.
        let r = src.read_band_with_progress(0, 10, &mut planes, 3, &|_| {}, &cancel);
        assert!(matches!(r, Err(IntegrationError::Cancelled)));
    }

    #[test]
    fn a_band_outside_every_source_row_is_all_nan_not_an_error() {
        let dir = tempfile::tempdir().unwrap();
        let far = Linear {
            kind: LinearKind::Affine,
            m: [[1.0, 0.0, 0.0], [0.0, 1.0, -5000.0], [0.0, 0.0, 1.0]],
        };
        let fr = vec![RegisteredFrame {
            path: frames(dir.path())[0].path.clone(),
            map: PixelMap::linear(far).unwrap(),
        }];
        let src = RegisteredSource::open(&fr, W, H, 0, Interpolation::Bilinear, 0.3).unwrap();
        let mut planes = BandPlanes::new(&src);
        src.read_band_with_progress(0, 10, &mut planes, 1, &|_| {}, &AtomicBool::new(false))
            .unwrap();
        let mut got = vec![0f32; 10 * W];
        planes.decode_frame_into(0, &mut got);
        assert!(got.iter().all(|v| v.is_nan()));
    }

    // ── Cross-scale pin (M4b Task 4, ruling R-M4b-6): a coarse subject
    // registered onto a finer reference is up-sampled by this resampler's
    // own inverse-mapped gather at the frame's OWN level — no new code is
    // needed for mixed pixel scales, the existing `PixelMap`/`warp_rows`
    // machinery already does the right thing "by construction". ──

    const CROSS_SCALE_SUB_W: usize = 200;
    const CROSS_SCALE_SUB_H: usize = 150;
    const CROSS_SCALE_REF_W: usize = 400;
    const CROSS_SCALE_REF_H: usize = 300;

    /// Subject → reference: a pure ×2 registration scale (no rotation, no
    /// translation) — a coarse frame registered onto a reference of twice
    /// the linear resolution, the same shape a real WCS-seeded
    /// mixed-pixel-scale registration produces. Shared by every cross-scale
    /// pin below (M4b Task 4, ruling R-M4b-6) and by the sibling pins in
    /// `stacking::drizzle::tests`.
    fn cross_scale_map() -> PixelMap {
        let fwd = Linear {
            kind: LinearKind::Affine,
            m: [[2.0, 0.0, 0.0], [0.0, 2.0, 0.0], [0.0, 0.0, 1.0]],
        };
        PixelMap::linear(fwd).unwrap()
    }

    // ── Cross-scale pin, level (M4b Task 4, ruling R-M4b-6). ──

    #[test]
    fn a_coarse_frame_registered_at_double_scale_upsamples_the_level_with_a_nan_margin() {
        const LEVEL: f32 = 0.25;

        let dir = tempfile::tempdir().unwrap();
        let flat = vec![LEVEL; CROSS_SCALE_SUB_W * CROSS_SCALE_SUB_H];
        let flat_path = dir.path().join("flat.fits");
        write_fits_f32(
            &flat_path,
            CROSS_SCALE_SUB_W,
            CROSS_SCALE_SUB_H,
            1,
            &flat,
            &[],
        )
        .unwrap();
        let flat_src = RegisteredSource::open(
            &[RegisteredFrame {
                path: flat_path,
                map: cross_scale_map(),
            }],
            CROSS_SCALE_REF_W,
            CROSS_SCALE_REF_H,
            0,
            Interpolation::BicubicBSpline,
            0.3,
        )
        .unwrap();
        let mut flat_planes = BandPlanes::new(&flat_src);
        flat_src
            .read_band_with_progress(
                0,
                CROSS_SCALE_REF_H,
                &mut flat_planes,
                1,
                &|_| {},
                &AtomicBool::new(false),
            )
            .unwrap();
        let mut flat_out = vec![0f32; CROSS_SCALE_REF_H * CROSS_SCALE_REF_W];
        flat_planes.decode_frame_into(0, &mut flat_out);

        // The subject's own last valid pixel (199, 149) maps to (398, 298)
        // — the interior — leaving a one-pixel margin (x=399 or y=299,
        // subject x/y = 199.5) that must read NaN, never a wrong level.
        for y in 0..CROSS_SCALE_REF_H {
            for x in 0..CROSS_SCALE_REF_W {
                let v = flat_out[y * CROSS_SCALE_REF_W + x];
                if x <= 2 * CROSS_SCALE_SUB_W - 2 && y <= 2 * CROSS_SCALE_SUB_H - 2 {
                    assert!(
                        (v - LEVEL).abs() < 1e-6,
                        "interior ({x},{y}) v={v}, expected {LEVEL}"
                    );
                } else {
                    assert!(v.is_nan(), "margin ({x},{y}) v={v}, expected NaN");
                }
            }
        }
    }

    // ── Cross-scale pin, resolution (M4b Task 4, ruling R-M4b-6). ──

    #[test]
    fn a_coarse_frame_registered_at_double_scale_preserves_centroid_and_broadens_sigma_by_the_scale()
    {
        const BG: f32 = 100.0;
        const SIGMA_SUB: f64 = 1.5;
        const STAR: (f64, f64, f64) = (100.0, 75.0, 5000.0);

        let dir = tempfile::tempdir().unwrap();
        let star_data = gaussian_field(
            CROSS_SCALE_SUB_W,
            CROSS_SCALE_SUB_H,
            &[STAR],
            SIGMA_SUB,
            BG,
        );
        let star_path = dir.path().join("star.fits");
        write_fits_f32(
            &star_path,
            CROSS_SCALE_SUB_W,
            CROSS_SCALE_SUB_H,
            1,
            &star_data,
            &[],
        )
        .unwrap();
        let star_src = RegisteredSource::open(
            &[RegisteredFrame {
                path: star_path,
                map: cross_scale_map(),
            }],
            CROSS_SCALE_REF_W,
            CROSS_SCALE_REF_H,
            0,
            Interpolation::BicubicBSpline,
            0.3,
        )
        .unwrap();
        let mut star_planes = BandPlanes::new(&star_src);
        star_src
            .read_band_with_progress(
                0,
                CROSS_SCALE_REF_H,
                &mut star_planes,
                1,
                &|_| {},
                &AtomicBool::new(false),
            )
            .unwrap();
        let mut star_out = vec![0f32; CROSS_SCALE_REF_H * CROSS_SCALE_REF_W];
        star_planes.decode_frame_into(0, &mut star_out);

        let (ex, ey) = (2.0 * STAR.0, 2.0 * STAR.1);
        let (cx, cy) = centroid(&star_out, CROSS_SCALE_REF_W, ex, ey, 15, BG);
        assert!(
            (cx - ex).abs() < 0.05 && (cy - ey).abs() < 0.05,
            "centroid ({cx},{cy}) vs expected ({ex},{ey})"
        );

        // Second-moment sigma estimate: for an isotropic 2-D Gaussian,
        // E[r²] over the background-subtracted flux equals 2·sigma² — a
        // window 5× the expected reference sigma wide, so the < 0.1% tail
        // left outside it is negligible.
        let r = (5.0 * 2.0 * SIGMA_SUB).round() as i64;
        let (icx, icy) = (ex.round() as i64, ey.round() as i64);
        let (mut sxx, mut syy, mut sw) = (0.0f64, 0.0f64, 0.0f64);
        for y in (icy - r)..=(icy + r) {
            for x in (icx - r)..=(icx + r) {
                let v = star_out[y as usize * CROSS_SCALE_REF_W + x as usize];
                if !v.is_finite() {
                    continue;
                }
                // The fixture is noise-free (`gaussian_field` on a flat
                // background, no `add_noise`), so this clamp only ever
                // discards the kernel's own small negative ringing lobes,
                // never real negative-going noise — on a noisy field the
                // same clamp would bias the measured sigma low by
                // truncating negative-flux samples the ideal estimator
                // would keep.
                let val = (v - BG).max(0.0) as f64;
                let (dx, dy) = (x as f64 - ex, y as f64 - ey);
                sxx += val * dx * dx;
                syy += val * dy * dy;
                sw += val;
            }
        }
        let sigma_ref = (((sxx + syy) / sw) / 2.0).sqrt();
        // The honest prediction is NOT a bare ×2 scale of the source sigma:
        // the BicubicBSpline kernel is the SMOOTHING B-spline (see
        // `stacking::ln::grid`'s module doc for the same kernel's
        // documented smoothing bias) — it convolves the source signal with
        // its own kernel, of variance 1/3 source-px², BEFORE the ×2
        // geometric scale stretches the result into reference pixels.
        // Variances add under convolution, so the reference sigma is
        // `scale · sqrt(sigma_sub² + kernel_var)`, not `scale · sigma_sub`:
        // `2 · sqrt(1.5² + 1/3) = 3.21460...`, matching the measured
        // 3.2145 to four significant figures. A symmetric band around the
        // naive `2 · sigma_sub = 3.0` would admit up to ~8% SHARPENING —
        // exactly what ruling R-M4b-6 forbids — so the tolerance below is
        // centred on the honest prediction, not on the naive one.
        let kernel_var_source_px2 = 1.0 / 3.0;
        let expected_sigma = 2.0 * (SIGMA_SUB * SIGMA_SUB + kernel_var_source_px2).sqrt();
        assert!(
            (sigma_ref - expected_sigma).abs() < 0.02,
            "measured sigma {sigma_ref} (2nd-moment estimator) vs expected {expected_sigma} \
             (subject sigma {SIGMA_SUB} convolved with the B-spline kernel's own variance, \
             × the ×2 registration scale)"
        );
    }
}
