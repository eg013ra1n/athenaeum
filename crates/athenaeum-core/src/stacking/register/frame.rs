//! The per-frame registration driver: read a calibrated frame through
//! `PlaneReader`, detect on its luminance, align onto the reference's stars,
//! and turn the outcome into a `registration_results` row.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tracing::{debug, warn};

use super::align::{align, model_name, AlignError, Alignment, DistortionFit, SeedKind, SeedPolicy};
use super::detect::{detect_stars, luminance, stars_from_fits, Star};
use super::RegistrationConfig;
use crate::geometry::{Linear, LinearKind, PixelMap};
use crate::integration::plane_reader::PlaneReader;
use crate::integration::IntegrationError;
use crate::registration::db::RegistrationRecord;
use crate::stacking::psf_signal::StarFit;

#[derive(Debug, Clone)]
pub struct ReferenceStars {
    pub stars: Vec<Star>,
    pub width: usize,
    pub height: usize,
}

impl From<&DetectedStars> for ReferenceStars {
    fn from(d: &DetectedStars) -> Self {
        ReferenceStars {
            stars: d.stars.clone(),
            width: d.width,
            height: d.height,
        }
    }
}

/// One frame's star detections off its own pixels (perf tier 1 Task 9,
/// `register_frame`'s split half). Detection depends only on the frame's
/// own plane(s) and `cfg.registration` — never on a reference — so this
/// value is reusable verbatim for as many registration attempts as a
/// caller likes; [`crate::stacking::run`]'s two-pass dry pass caches one
/// of these per frame instead of re-reading and re-detecting the same
/// pixels for the persisting pass that follows it.
///
/// `read_ms`/`detect_ms` are the read+detect cost [`detect_frame_stars`]
/// measured to PRODUCE this value — carried here (rather than threaded as
/// extra parameters) so a caller holding an `Arc<DetectedStars>` from
/// earlier can still recover them (`register_frame` uses them to keep
/// `FrameRegistration::duration_ms` reporting the whole frame's cost).
/// [`register_detected`] does not read them: see its own doc comment for
/// why its own logged `read_ms`/`detect_ms` are unconditionally `0`.
///
/// `path` (fix round 1, Important finding 1) is the subject's own path —
/// carried for the same reason: [`register_detected`] has no `Path`
/// parameter of its own (its 6-argument signature is unchanged from the
/// brief), so without this field its `"frame registered"`/`"registration
/// warning"`/`"frame registration failed"` events would carry no
/// identifying field at all when fired from inside the Register fan-out's
/// scoped worker threads, which have no span context either.
#[derive(Debug, Clone)]
pub struct DetectedStars {
    pub stars: Vec<Star>,
    pub width: usize,
    pub height: usize,
    pub read_ms: u64,
    pub detect_ms: u64,
    pub path: PathBuf,
}

/// Test-only call log (perf tier 1 Task 9): every real call to
/// [`detect_frame_stars`] appends the path it was called with. A run-level
/// test can snapshot its length before and after a pipeline stage and
/// filter the new entries by path prefix (every test fixture writes its
/// calibrated frames under its own unique `tempfile::TempDir`) to prove a
/// frame's pixels are read and detected AT MOST ONCE across a run — a
/// cache hit in [`crate::stacking::run`]'s two-pass dry-pass cache reuses a
/// previously detected `Arc<DetectedStars>` and never reaches this
/// function at all.
///
/// A plain `AtomicUsize` was tried first and rejected: `cargo test`'s
/// default parallelism runs many OTHER `stacking::register`/`stacking::run`
/// tests' own `detect_frame_stars` calls concurrently with any one test's
/// measurement window, so a bare process-wide count is not that test's own
/// count — confirmed empirically (a raw counter delta read 7 where a
/// single-threaded run of the same test read 6, `--test-threads=8` over
/// just `stacking::run::` reproduced it every time). Logging the PATH
/// instead of just counting lets a test filter down to calls that are
/// provably its own, immune to how many other tests happen to be
/// registering frames at the same wall-clock moment.
#[cfg(test)]
pub(crate) static DETECT_FRAME_STARS_LOG: std::sync::Mutex<Vec<std::path::PathBuf>> =
    std::sync::Mutex::new(Vec::new());

#[derive(Debug, Clone)]
pub struct FrameRegistration {
    pub width: usize,
    pub height: usize,
    /// Registration stars found on the subject.
    pub detections: usize,
    pub outcome: Result<Alignment, AlignError>,
    pub duration_ms: u64,
}

/// Every plane of a calibrated frame, read from disk but not yet combined
/// to luminance (perf tier A Task 3). Kept separate — rather than folded
/// straight into an owned `Vec<f32>` the way this used to work — so a
/// caller can build the [`luminance`] slice itself and keep `planes` alive
/// across its own [`detect_stars`] call: `luminance`'s single-plane case
/// now borrows instead of copying (see its doc comment), and that borrow's
/// lifetime is tied to `planes`, which would otherwise be dropped at the
/// end of a `read_luminance`-shaped helper before `detect_stars` ever ran.
fn read_planes(path: &Path) -> Result<(Vec<Vec<f32>>, usize, usize), IntegrationError> {
    let reader = PlaneReader::open(path)?;
    let (w, h) = (reader.width(), reader.height());
    let planes: Vec<Vec<f32>> = (0..reader.channels())
        .map(|p| reader.read_plane(p))
        .collect::<Result<_, _>>()?;
    Ok((planes, w, h))
}

/// `fits`: Tier C Task 3 (spec §7, ruling C-6) — the reference frame's own
/// Measure-accepted PSF fits for plane 0, when the caller has already
/// established the frame is single-plane AND a fresh `fits` artifact
/// exists for it (`stacking::run` resolves both before calling this — this
/// function has no DB access of its own). `Some` skips the read+detect
/// entirely in favour of [`stars_from_fits`]; `None` is today's detection,
/// unconditionally — an OSC reference (ruling C-6) or any frame the
/// caller could not, or chose not to, resolve fits for.
pub fn reference_stars(
    path: &Path,
    cfg: &RegistrationConfig,
    pool: Option<&Arc<rayon::ThreadPool>>,
    fits: Option<&[StarFit]>,
) -> Result<ReferenceStars, IntegrationError> {
    if let Some(fits) = fits {
        let t = Instant::now();
        let reader = PlaneReader::open(path)?;
        let (width, height) = (reader.width(), reader.height());
        let read_ms = t.elapsed().as_millis() as u64;
        let t = Instant::now();
        let stars = stars_from_fits(fits, &cfg.detection, cfg.max_stars);
        let detect_ms = t.elapsed().as_millis() as u64;
        debug!(
            path = %path.display(),
            detections = stars.len(),
            read_ms,
            detect_ms,
            star_source = "fits",
            "reference stars detected"
        );
        return Ok(ReferenceStars {
            stars,
            width,
            height,
        });
    }
    let t = Instant::now();
    let (planes, width, height) = read_planes(path)?;
    let refs: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
    let lum = luminance(&refs);
    let read_ms = t.elapsed().as_millis() as u64;
    let t = Instant::now();
    let stars = detect_stars(&lum, width, height, &cfg.detection, cfg.max_stars, pool);
    let detect_ms = t.elapsed().as_millis() as u64;
    debug!(
        path = %path.display(),
        detections = stars.len(),
        read_ms,
        detect_ms,
        star_source = "detected",
        "reference stars detected"
    );
    Ok(ReferenceStars {
        stars,
        width,
        height,
    })
}

/// Read one subject's plane(s) and detect its stars — the I/O+CPU half of
/// [`register_frame`], split out (perf tier 1 Task 9) so a caller that
/// already knows the frame will be registered more than once (the two-pass
/// dry pass, then the persisting pass) can detect it exactly ONCE and reuse
/// the result. Depends only on `path` and `cfg` — never on a reference —
/// which is exactly what makes that reuse safe: the reference the caller
/// eventually aligns against has no bearing on what stars this frame has.
///
/// Logs its own `"frame stars detected"` debug (path, detections, and the
/// read/detect split) — the ONE place that timing is captured now, whether
/// this is called directly by a caller wanting fresh detections, or once
/// per frame from inside a caching pass. [`register_detected`]'s own log
/// line no longer repeats it (see that function's doc comment).
///
/// `fits`: Tier C Task 3 (spec §7, ruling C-6) — this frame's own
/// Measure-accepted PSF fits for plane 0, when the caller has already
/// established the frame is single-plane AND a fresh `fits` artifact
/// exists for it (`stacking::run` resolves both — this function has no DB
/// access of its own, mirroring [`reference_stars`]'s own `fits`
/// parameter). `Some` reads only the file's HEADER (`PlaneReader::open`,
/// no pixel plane) and converts the fits via [`stars_from_fits`] instead
/// of reading+detecting; `None` is today's full detection, unconditionally
/// — an OSC frame (ruling C-6), a stale/missing artifact, or any other
/// caller that has not resolved fits for this path.
pub fn detect_frame_stars(
    path: &Path,
    cfg: &RegistrationConfig,
    pool: Option<&Arc<rayon::ThreadPool>>,
    fits: Option<&[StarFit]>,
) -> Result<DetectedStars, IntegrationError> {
    #[cfg(test)]
    DETECT_FRAME_STARS_LOG
        .lock()
        .unwrap()
        .push(path.to_path_buf());
    if let Some(fits) = fits {
        let t = Instant::now();
        let reader = PlaneReader::open(path)?;
        let (width, height) = (reader.width(), reader.height());
        let read_ms = t.elapsed().as_millis() as u64;
        let t = Instant::now();
        let stars = stars_from_fits(fits, &cfg.detection, cfg.max_stars);
        let detect_ms = t.elapsed().as_millis() as u64;
        debug!(
            path = %path.display(),
            detections = stars.len(),
            read_ms,
            detect_ms,
            star_source = "fits",
            "frame stars detected"
        );
        return Ok(DetectedStars {
            stars,
            width,
            height,
            read_ms,
            detect_ms,
            path: path.to_path_buf(),
        });
    }
    let t = Instant::now();
    let (planes, width, height) = read_planes(path)?;
    let refs: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
    let lum = luminance(&refs);
    let read_ms = t.elapsed().as_millis() as u64;
    let t = Instant::now();
    let stars = detect_stars(&lum, width, height, &cfg.detection, cfg.max_stars, pool);
    let detect_ms = t.elapsed().as_millis() as u64;
    debug!(
        path = %path.display(),
        detections = stars.len(),
        read_ms,
        detect_ms,
        star_source = "detected",
        "frame stars detected"
    );
    Ok(DetectedStars {
        stars,
        width,
        height,
        read_ms,
        detect_ms,
        path: path.to_path_buf(),
    })
}

/// Align one frame's already-detected stars onto the reference — the
/// pixel-CPU half of [`register_frame`] (perf tier 1 Task 9): no I/O, no
/// star detection, just [`align`]. `read_ms`/`detect_ms` on its own
/// `"frame registered"`/`"frame registration failed"` event are therefore
/// UNCONDITIONALLY `0` — this function never reads or detects, whether
/// `detected` was produced moments ago (a fresh [`register_frame`] call)
/// or reused from an earlier detection (a cache hit in
/// [`crate::stacking::run`]'s two-pass dry pass); that cost is captured
/// once, on [`detect_frame_stars`]'s own `"frame stars detected"` event,
/// wherever it was actually paid. `FrameRegistration::duration_ms` here is
/// align time only — [`register_frame`] adds the detect+read time back on
/// top of it so a caller reading `duration_ms` alone still sees the whole
/// frame's cost.
///
/// `hint`, `policy` and `scale_gate` are M4b's three per-frame inputs,
/// passed straight through to [`align`]: an optional plate-solve seed,
/// which seed leads (ruling R-T6-9), and the scale window this particular
/// frame is judged against.
///
/// Fix round 1, Important finding 1: `path` on all three logged events
/// (`"frame registered"`, `"registration warning"`, `"frame registration
/// failed"`) comes from `detected.path` — this function has no `Path`
/// parameter of its own, so without it these events, fired from inside
/// the Register fan-out's scoped worker threads (no span context), would
/// carry no identifying field at all. The logging spec's registration
/// entry has always said these events "reuse `path`".
pub fn register_detected(
    reference: &ReferenceStars,
    detected: &DetectedStars,
    cfg: &RegistrationConfig,
    hint: Option<&Linear>,
    policy: SeedPolicy,
    scale_gate: (f64, f64),
) -> FrameRegistration {
    let start = Instant::now();
    let outcome = align(
        &detected.stars,
        &reference.stars,
        (reference.width, reference.height),
        (detected.width, detected.height),
        cfg,
        hint,
        policy,
        scale_gate,
    );
    let align_ms = start.elapsed().as_millis() as u64;
    let duration_ms = align_ms;
    match &outcome {
        Ok(a) => {
            debug!(
                path = %detected.path.display(),
                detections = detected.stars.len(),
                inliers = a.inliers,
                rms_px = a.rms_px,
                model = %model_name(a.model, a.distortion, a.seed),
                flipped = a.flipped,
                read_ms = 0,
                detect_ms = 0,
                align_ms,
                duration_ms,
                "frame registered"
            );
            for note in &a.warnings {
                warn!(
                    path = %detected.path.display(),
                    note = %note,
                    "registration warning"
                );
            }
        }
        Err(e) => {
            warn!(
                path = %detected.path.display(),
                detections = detected.stars.len(),
                error = %e,
                read_ms = 0,
                detect_ms = 0,
                align_ms,
                duration_ms,
                "frame registration failed"
            )
        }
    }
    FrameRegistration {
        width: detected.width,
        height: detected.height,
        detections: detected.stars.len(),
        outcome,
        duration_ms,
    }
}

/// Register one subject onto the reference: [`detect_frame_stars`] then
/// [`register_detected`]. I/O errors and cancellation are `Err`; an
/// alignment failure is a successful measurement of a frame that cannot be
/// registered (`outcome: Err(AlignError)`).
///
/// Always detects (`fits: None` to [`detect_frame_stars`]) — this
/// composition has no production caller in `stacking::run` (which needs
/// the fits-resolution split below for its own caching), only tests and
/// dev probes, so it carries no `fits` parameter of its own; a caller that
/// wants Tier C Task 3's mono reuse calls [`detect_frame_stars`] directly.
#[allow(clippy::too_many_arguments)]
pub fn register_frame(
    reference: &ReferenceStars,
    subject: &Path,
    cfg: &RegistrationConfig,
    pool: Option<&Arc<rayon::ThreadPool>>,
    cancel: &AtomicBool,
    hint: Option<&Linear>,
    policy: SeedPolicy,
    scale_gate: (f64, f64),
) -> Result<FrameRegistration, IntegrationError> {
    if cancel.load(Ordering::Relaxed) {
        return Err(IntegrationError::Cancelled);
    }
    let detected = detect_frame_stars(subject, cfg, pool, None)?;
    if cancel.load(Ordering::Relaxed) {
        return Err(IntegrationError::Cancelled);
    }
    let mut reg = register_detected(reference, &detected, cfg, hint, policy, scale_gate);
    // `register_detected`'s own `duration_ms` is align-only; add back the
    // detect+read time `detect_frame_stars` already spent (and already
    // logged on its own "frame stars detected" event) so this function's
    // `FrameRegistration.duration_ms` still reports the whole frame's
    // cost, exactly as before this split.
    reg.duration_ms += detected.read_ms + detected.detect_ms;
    Ok(reg)
}

/// The reference frame's own row: an identity map over its stars.
pub fn identity_registration(reference: &ReferenceStars) -> FrameRegistration {
    let map = PixelMap::linear(Linear::identity()).expect("identity is invertible");
    FrameRegistration {
        width: reference.width,
        height: reference.height,
        detections: reference.stars.len(),
        outcome: Ok(Alignment {
            map,
            model: LinearKind::Similarity,
            distortion: DistortionFit::None,
            seed: SeedKind::Quads,
            seed_matches: 0,
            pairs: reference.stars.len(),
            repaired: 0,
            inliers: reference.stars.len(),
            inlier_ratio: 1.0,
            rms_px: 0.0,
            sigma_rms_px: 0.0,
            peak_px: (0.0, 0.0),
            scale: 1.0,
            rotation_deg: 0.0,
            translation: (0.0, 0.0),
            flipped: false,
            quality_score: 1.0,
            overlap: 1.0,
            regularity: 1.0,
            ransac_iterations: 0,
            refit_rounds: 0,
            local_rounds: 0,
            warnings: Vec::new(),
        }),
        duration_ms: 0,
    }
}

/// The `registration_results` row for one outcome (spec §9.1). The legacy
/// `affine_*` columns carry the linear part's top two rows even for a
/// homography (its projective row lives in `transform_json`); the WCS
/// columns stay `None` — v2 does not plate-solve.
pub fn to_record(
    frames_set_id: i64,
    frame_id: i64,
    reference_frame_id: i64,
    is_reference: bool,
    reg: &FrameRegistration,
    config_hash: &str,
    registered_at: &str,
) -> RegistrationRecord {
    let mut rec = RegistrationRecord {
        frames_set_id,
        frame_id,
        reference_frame_id,
        is_reference,
        compute_time_ms: reg.duration_ms as i64,
        registered_at: registered_at.to_string(),
        config_hash: Some(config_hash.to_string()),
        source_kind: Some("calibrated".to_string()),
        ..Default::default()
    };
    match &reg.outcome {
        Ok(a) => {
            let m = a.map.linear.m;
            rec.affine_a1 = Some(m[0][0]);
            rec.affine_b1 = Some(m[0][1]);
            rec.affine_c1 = Some(m[0][2]);
            rec.affine_a2 = Some(m[1][0]);
            rec.affine_b2 = Some(m[1][1]);
            rec.affine_c2 = Some(m[1][2]);
            rec.matched_stars = a.inliers as i64;
            rec.rms_residual_px = a.rms_px;
            rec.status = if is_reference {
                "reference"
            } else if a.flipped {
                "aligned_flipped"
            } else {
                "aligned"
            }
            .to_string();
            rec.model = Some(model_name(a.model, a.distortion, a.seed));
            rec.transform_json = Some(a.map.to_json());
            rec.inlier_ratio = Some(a.inlier_ratio);
            rec.peak_error_px = Some(a.peak_px.0.max(a.peak_px.1));
            rec.scale = Some(a.scale);
            rec.rotation_deg = Some(a.rotation_deg);
            rec.flipped = a.flipped;
        }
        Err(e) => {
            rec.status = "failed".to_string();
            rec.error = Some(e.to_string());
        }
    }
    rec
}

#[cfg(test)]
mod tests {
    use super::super::align::SCALE_RANGE;
    use super::*;
    use crate::fits_writer::write_fits_f32;
    use crate::geometry::ransac::SplitMix64;
    use crate::test_support::{add_noise, gaussian_field};
    use std::path::PathBuf;

    fn stars(seed: u64, n: usize, w: f64, h: f64) -> Vec<(f64, f64, f64)> {
        let mut rng = SplitMix64(seed);
        (0..n)
            .map(|_| {
                (
                    30.0 + rng.next_f64() * (w - 60.0),
                    30.0 + rng.next_f64() * (h - 60.0),
                    0.1 + rng.next_f64() * 0.4,
                )
            })
            .collect()
    }

    /// Reference = the field; subject = the same stars shifted by (dx, dy)
    /// and rotated by `rot_deg` about the centre (subject → reference is the
    /// inverse of that), written as 1- or 3-plane FITS.
    fn pair(
        dir: &std::path::Path,
        seed: u64,
        dx: f64,
        dy: f64,
        rot_deg: f64,
        planes: usize,
    ) -> (PathBuf, PathBuf, Vec<(f64, f64, f64)>) {
        let (w, h) = (640usize, 480usize);
        let refs = stars(seed, 160, w as f64, h as f64);
        let (s, c) = rot_deg.to_radians().sin_cos();
        let (cx, cy) = (w as f64 / 2.0, h as f64 / 2.0);
        let subs: Vec<(f64, f64, f64)> = refs
            .iter()
            .map(|&(x, y, a)| {
                let (u, v) = (x - cx, y - cy);
                (cx + c * u - s * v + dx, cy + s * u + c * v + dy, a)
            })
            .collect();
        let write = |name: &str, list: &[(f64, f64, f64)]| {
            let mut plane = gaussian_field(w, h, list, 1.6, 0.08);
            add_noise(&mut plane, 0.002, seed + 11);
            let mut all = Vec::new();
            for k in 0..planes {
                all.extend(plane.iter().map(|v| v * (1.0 - 0.2 * k as f32)));
            }
            let p = dir.join(name);
            write_fits_f32(&p, w, h, planes, &all, &[]).unwrap();
            p
        };
        let r = write("reference.fits", &refs);
        let s = write("subject.fits", &subs);
        (r, s, refs)
    }

    #[test]
    fn registers_a_shifted_rotated_subject_onto_the_reference() {
        let dir = tempfile::tempdir().unwrap();
        let (r, s, _) = pair(dir.path(), 21, 7.3, -4.1, 1.5, 1);
        let cfg = RegistrationConfig::default();
        let reference = reference_stars(&r, &cfg, None, None).unwrap();
        assert!(reference.stars.len() >= 110, "{}", reference.stars.len());
        assert_eq!((reference.width, reference.height), (640, 480));
        let reg = register_frame(
            &reference,
            &s,
            &cfg,
            None,
            &AtomicBool::new(false),
            None,
            SeedPolicy::QuadFirst,
            SCALE_RANGE,
        )
        .unwrap();
        let a = reg.outcome.as_ref().expect("registration succeeded");
        assert!(
            a.inliers >= 100 && a.rms_px < 0.15,
            "inliers {} rms {}",
            a.inliers,
            a.rms_px
        );
        // A subject pixel maps back onto the reference: the subject star that
        // sits at the reference star (200, 200) rotated+shifted lands on (200, 200).
        let (s_, c_) = 1.5f64.to_radians().sin_cos();
        let (u, v) = (200.0 - 320.0, 200.0 - 240.0);
        let (sx, sy) = (320.0 + c_ * u - s_ * v + 7.3, 240.0 + s_ * u + c_ * v - 4.1);
        let (fx, fy) = a.map.forward(sx, sy);
        assert!(
            (fx - 200.0).abs() < 0.05 && (fy - 200.0).abs() < 0.05,
            "forward {fx} {fy}"
        );
        assert!(
            (a.rotation_deg + 1.5).abs() < 0.01,
            "rotation {}",
            a.rotation_deg
        );
        assert!((a.scale - 1.0).abs() < 1e-3);
        assert!(reg.detections >= 110 && reg.duration_ms < 60_000);
        let rec = to_record(7, 42, 41, false, &reg, "hash", "2026-09-09T00:00:00Z");
        assert_eq!(
            (rec.frames_set_id, rec.frame_id, rec.reference_frame_id),
            (7, 42, 41)
        );
        assert_eq!(rec.status, "aligned");
        assert_eq!(rec.model.as_deref(), Some("homography"));
        assert_eq!(rec.source_kind.as_deref(), Some("calibrated"));
        assert_eq!(rec.config_hash.as_deref(), Some("hash"));
        assert!(!rec.flipped && rec.transform_json.is_some());
        assert_eq!(rec.matched_stars, a.inliers as i64);
        assert!((rec.rms_residual_px - a.rms_px).abs() < 1e-12);
        assert_eq!(
            rec.affine_a1
                .map(|v| (v - a.map.linear.m[0][0]).abs() < 1e-12),
            Some(true)
        );
        let back = PixelMap::from_json(rec.transform_json.as_deref().unwrap()).unwrap();
        let (bx, by) = back.forward(sx, sy);
        assert!((bx - fx).abs() < 1e-9 && (by - fy).abs() < 1e-9);
        assert!(rec.crpix1.is_none() && rec.error.is_none());
    }

    /// Perf tier 1 Task 9: `register_frame` = `detect_frame_stars` +
    /// `register_detected`, byte-for-byte — the split must not change what
    /// gets detected or how it aligns, only how many times a caller who
    /// already has the detections can skip redoing them.
    #[test]
    fn register_detected_equals_register_frame() {
        let dir = tempfile::tempdir().unwrap();
        let (r, s, _) = pair(dir.path(), 25, 5.0, -3.0, 1.0, 1);
        let cfg = RegistrationConfig::default();
        let reference = reference_stars(&r, &cfg, None, None).unwrap();
        let cancel = AtomicBool::new(false);
        let direct = register_frame(
            &reference,
            &s,
            &cfg,
            None,
            &cancel,
            None,
            SeedPolicy::QuadFirst,
            SCALE_RANGE,
        )
        .unwrap();
        let detected = detect_frame_stars(&s, &cfg, None, None).unwrap();
        let split = register_detected(
            &reference,
            &detected,
            &cfg,
            None,
            SeedPolicy::QuadFirst,
            SCALE_RANGE,
        );
        let (a, b) = (direct.outcome.unwrap(), split.outcome.unwrap());
        assert_eq!(a.map.to_json(), b.map.to_json());
        assert_eq!(a.inliers, b.inliers);
        assert_eq!(a.rms_px, b.rms_px);
        assert_eq!(direct.detections, split.detections);
        assert_eq!(direct.width, split.width);
        assert_eq!(direct.height, split.height);
    }

    #[test]
    fn rgb_subject_uses_luminance_and_cancel_is_honoured() {
        let dir = tempfile::tempdir().unwrap();
        let (r, s, _) = pair(dir.path(), 22, -3.0, 2.5, 0.0, 3);
        let cfg = RegistrationConfig::default();
        let reference = reference_stars(&r, &cfg, None, None).unwrap();
        let reg = register_frame(
            &reference,
            &s,
            &cfg,
            None,
            &AtomicBool::new(false),
            None,
            SeedPolicy::QuadFirst,
            SCALE_RANGE,
        )
        .unwrap();
        let a = reg.outcome.as_ref().unwrap();
        assert!(
            (a.translation.0 - 3.0).abs() < 0.05 && (a.translation.1 + 2.5).abs() < 0.05,
            "{:?}",
            a.translation
        );
        assert!(matches!(
            register_frame(
                &reference,
                &s,
                &cfg,
                None,
                &AtomicBool::new(true),
                None,
                SeedPolicy::QuadFirst,
                SCALE_RANGE
            ),
            Err(IntegrationError::Cancelled)
        ));
        let bad = dir.path().join("nope.txt");
        std::fs::write(&bad, b"x").unwrap();
        assert!(matches!(
            register_frame(
                &reference,
                &bad,
                &cfg,
                None,
                &AtomicBool::new(false),
                None,
                SeedPolicy::QuadFirst,
                SCALE_RANGE
            ),
            Err(IntegrationError::BadInput(_))
        ));
    }

    #[test]
    fn an_unrelated_field_fails_with_a_reason_and_a_failed_record() {
        let dir = tempfile::tempdir().unwrap();
        let (r, _, _) = pair(dir.path(), 23, 0.0, 0.0, 0.0, 1);
        let other = dir.path().join("other");
        std::fs::create_dir_all(&other).unwrap();
        let (_, s2, _) = pair(&other, 99, 0.0, 0.0, 0.0, 1);
        let cfg = RegistrationConfig::default();
        let reference = reference_stars(&r, &cfg, None, None).unwrap();
        let reg = register_frame(
            &reference,
            &s2,
            &cfg,
            None,
            &AtomicBool::new(false),
            None,
            SeedPolicy::QuadFirst,
            SCALE_RANGE,
        )
        .unwrap();
        let err = reg
            .outcome
            .as_ref()
            .err()
            .expect("unrelated field must fail");
        let rec = to_record(1, 2, 3, false, &reg, "h", "t");
        assert_eq!(rec.status, "failed");
        assert_eq!(rec.error.as_deref(), Some(format!("{err}").as_str()));
        assert!(rec.transform_json.is_none() && rec.model.is_none() && rec.affine_a1.is_none());
        assert_eq!(rec.matched_stars, 0);
    }

    #[test]
    fn the_reference_row_is_the_identity() {
        let dir = tempfile::tempdir().unwrap();
        let (r, _, _) = pair(dir.path(), 24, 0.0, 0.0, 0.0, 1);
        let reference = reference_stars(&r, &RegistrationConfig::default(), None, None).unwrap();
        let reg = identity_registration(&reference);
        let a = reg.outcome.as_ref().unwrap();
        assert_eq!(a.map.forward(10.5, 20.25), (10.5, 20.25));
        let rec = to_record(1, 3, 3, true, &reg, "h", "t");
        assert!(rec.is_reference && rec.status == "reference");
        assert_eq!(rec.matched_stars, reference.stars.len() as i64);
        assert_eq!(rec.affine_a1, Some(1.0));
        assert_eq!(rec.model.as_deref(), Some("similarity"));
    }

    /// Tier C Task 3 (spec §7, ruling C-6) — DELTA pin: on a mono frame,
    /// aligning with the star list [`stars_from_fits`] builds from
    /// Measure's own accepted PSF fits must land within the pipeline's own
    /// tolerance of aligning with today's fresh detection — not
    /// bit-identical (a different star LIST by design, spec §7), but close.
    /// The subject's own fits come from
    /// [`crate::stacking::measure::measure_frame_with_fits`] — the SAME
    /// function stage 3 calls to produce what `stacking::run` persists as
    /// the `fits` artifact `detect_frame_stars`'s new `fits` path reads
    /// back, so this pin exercises the real Measure -> Register seam, not
    /// a hand-built fixture.
    #[test]
    fn fit_derived_stars_align_within_tolerance_of_detected_stars() {
        // The fixture's own known truth: `pair()` builds the subject as
        // `subject = R(ROT_DEG)·(reference - centre) + centre + (DX, DY)`,
        // so the RECOVERED alignment (subject -> reference, a plain
        // similarity with NO centre of its own) is
        // `reference = R(-ROT_DEG)·subject + [centre - R(-ROT_DEG)·(centre
        // + (DX, DY))]` — a rotation of `-ROT_DEG` (pivot-independent, the
        // SAME sign convention every other `pair()`-based test in this
        // module pins, e.g. `registers_a_shifted_rotated_subject_onto_
        // the_reference`), but a translation that is NOT simply
        // `(-DX, -DY)` once the pivot (the frame's own centre) is not the
        // origin — the naive `(-DX, -DY)` reading was fix round 1's own
        // first attempt at this pin and measurably wrong (observed
        // (-10.86, 10.89) against a claimed truth of (-6, 4) on this exact
        // fixture); this derivation reproduces the observed value.
        const DX: f64 = 6.0;
        const DY: f64 = -4.0;
        const ROT_DEG: f64 = 1.2;
        let dir = tempfile::tempdir().unwrap();
        let (r, s, _) = pair(dir.path(), 31, DX, DY, ROT_DEG, 1);
        let cfg = RegistrationConfig::default();
        let reference = reference_stars(&r, &cfg, None, None).unwrap();
        let true_rotation_deg = -ROT_DEG;
        let true_translation = {
            let (cx, cy) = (reference.width as f64 / 2.0, reference.height as f64 / 2.0);
            let theta = true_rotation_deg.to_radians();
            let (sin_t, cos_t) = theta.sin_cos();
            let (tx, ty) = (cx + DX, cy + DY);
            let rotated = (cos_t * tx - sin_t * ty, sin_t * tx + cos_t * ty);
            (cx - rotated.0, cy - rotated.1)
        };

        let detected = detect_frame_stars(&s, &cfg, None, None).unwrap();
        assert!(!detected.stars.is_empty(), "fixture must detect stars");
        let detected_reg = register_detected(
            &reference,
            &detected,
            &cfg,
            None,
            SeedPolicy::QuadFirst,
            SCALE_RANGE,
        );
        let detected_align = detected_reg
            .outcome
            .expect("the detected-star list must register");

        let measure_opts = crate::stacking::measure::MeasureOptions::default();
        let (_, fits_by_plane) = crate::stacking::measure::measure_frame_with_fits(
            &s,
            &measure_opts,
            None,
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(fits_by_plane.len(), 1, "the fixture is single-plane");

        let fits_detected = detect_frame_stars(&s, &cfg, None, Some(&fits_by_plane[0])).unwrap();
        assert!(
            !fits_detected.stars.is_empty(),
            "the fit-derived list must not be empty"
        );
        let fits_reg = register_detected(
            &reference,
            &fits_detected,
            &cfg,
            None,
            SeedPolicy::QuadFirst,
            SCALE_RANGE,
        );
        let fits_align = fits_reg
            .outcome
            .expect("the fit-derived star list must register");

        assert!(
            (fits_align.rms_px - detected_align.rms_px).abs() <= 0.05,
            "rms_px: detected {} vs fits {}",
            detected_align.rms_px,
            fits_align.rms_px
        );
        assert!(
            fits_align.inliers as f64 >= 0.9 * detected_align.inliers as f64,
            "inliers: detected {} vs fits {}",
            detected_align.inliers,
            fits_align.inliers
        );

        // Fix round 1 (ruling C-18, item 3): rms/inliers alone are
        // offset-blind — a wrong translation or rotation that happens to
        // land the same number of stars within the RANSAC tolerance would
        // still pass those two checks. Assert the recovered geometry
        // itself, both against the fixture's own known truth and against
        // each other.
        const TRANSLATION_TOL_PX: f64 = 0.15;
        const ROTATION_TOL_DEG: f64 = 0.05;
        for (source, a) in [("detected", &detected_align), ("fits", &fits_align)] {
            assert!(
                (a.translation.0 - true_translation.0).abs() < TRANSLATION_TOL_PX
                    && (a.translation.1 - true_translation.1).abs() < TRANSLATION_TOL_PX,
                "{source}: translation {:?} vs truth {:?}",
                a.translation,
                true_translation
            );
            assert!(
                (a.rotation_deg - true_rotation_deg).abs() < ROTATION_TOL_DEG,
                "{source}: rotation {} vs truth {}",
                a.rotation_deg,
                true_rotation_deg
            );
        }
        assert!(
            (fits_align.translation.0 - detected_align.translation.0).abs() < TRANSLATION_TOL_PX
                && (fits_align.translation.1 - detected_align.translation.1).abs()
                    < TRANSLATION_TOL_PX,
            "translation must agree between the two star sources: detected {:?} vs fits {:?}",
            detected_align.translation,
            fits_align.translation
        );
        assert!(
            (fits_align.rotation_deg - detected_align.rotation_deg).abs() < ROTATION_TOL_DEG,
            "rotation must agree between the two star sources: detected {} vs fits {}",
            detected_align.rotation_deg,
            fits_align.rotation_deg
        );
    }

    /// The fits path is a genuine shortcut, not a slower detour dressed up
    /// as one — pinned STRUCTURALLY (fix round 1, ruling C-18, item 4)
    /// rather than by a timing bound (a `read_ms` threshold is inherently
    /// flaky on a loaded CI runner and proves nothing about WHICH code ran,
    /// only how long it took). The subject file here is FLAT — no stars at
    /// all, confirmed by [`empty_and_flat_planes_yield_no_stars`] to detect
    /// as an empty list — so a real detection could only return `[]`. If
    /// [`detect_frame_stars`] instead returns EXACTLY [`stars_from_fits`]'s
    /// own output for a synthetic fits list, the fits path is structurally
    /// the only thing that could have produced it.
    #[test]
    fn detect_frame_stars_with_fits_returns_exactly_the_fit_derived_list() {
        let dir = tempfile::tempdir().unwrap();
        let (w, h) = (64usize, 64usize);
        let flat = vec![0.05f32; w * h];
        let path = dir.path().join("blank.fits");
        write_fits_f32(&path, w, h, 1, &flat, &[]).unwrap();

        let cfg = RegistrationConfig::default();
        let fits: Vec<StarFit> = (0..5)
            .map(|i| StarFit {
                x: 10.0 + i as f64 * 5.0,
                y: 20.0 + i as f64 * 3.0,
                background: 0.0,
                amplitude: 5000.0,
                fwhm_x: 3.0,
                fwhm_y: 3.0,
                fwtm_x: 6.0,
                fwtm_y: 6.0,
                theta: 0.0,
                beta: 4.0,
                residual: 0.01,
                signal: 40000.0 + i as f64 * 1000.0,
                area: 28.0,
            })
            .collect();
        let expected = stars_from_fits(&fits, &cfg.detection, cfg.max_stars);
        assert!(!expected.is_empty(), "fixture fits must survive the cuts");

        let out = detect_frame_stars(&path, &cfg, None, Some(&fits)).unwrap();
        assert_eq!(
            out.stars, expected,
            "a flat field cannot have produced anything but the fit-derived list"
        );
        // `read_ms` is a report line only now (fix round 1, item 4) — not
        // asserted. It should still be a header-only `PlaneReader::open`
        // (microseconds), never a full-plane read, but timing is not a
        // structural proof and the assertion above is.
        eprintln!(
            "detect_frame_stars_with_fits_returns_exactly_the_fit_derived_list: \
             read_ms={} (report only)",
            out.read_ms
        );
    }
}
