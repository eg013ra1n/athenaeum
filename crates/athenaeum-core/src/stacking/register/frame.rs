//! The per-frame registration driver: read a calibrated frame through
//! `PlaneReader`, detect on its luminance, align onto the reference's stars,
//! and turn the outcome into a `registration_results` row.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Instant;

use tracing::{debug, warn};

use super::align::{align, model_name, AlignError, Alignment};
use super::detect::{detect_stars, luminance, Star};
use super::RegistrationConfig;
use crate::geometry::{Linear, LinearKind, PixelMap};
use crate::integration::plane_reader::PlaneReader;
use crate::integration::IntegrationError;
use crate::registration::db::RegistrationRecord;

#[derive(Debug, Clone)]
pub struct ReferenceStars {
    pub stars: Vec<Star>,
    pub width: usize,
    pub height: usize,
}

#[derive(Debug, Clone)]
pub struct FrameRegistration {
    pub width: usize,
    pub height: usize,
    /// Registration stars found on the subject.
    pub detections: usize,
    pub outcome: Result<Alignment, AlignError>,
    pub duration_ms: u64,
}

/// All planes of a calibrated frame collapsed to one luminance plane.
pub(crate) fn read_luminance(path: &Path) -> Result<(Vec<f32>, usize, usize), IntegrationError> {
    let reader = PlaneReader::open(path)?;
    let (w, h) = (reader.width(), reader.height());
    let planes: Vec<Vec<f32>> = (0..reader.channels())
        .map(|p| reader.read_plane(p))
        .collect::<Result<_, _>>()?;
    let refs: Vec<&[f32]> = planes.iter().map(Vec::as_slice).collect();
    Ok((luminance(&refs), w, h))
}

pub fn reference_stars(
    path: &Path,
    cfg: &RegistrationConfig,
    pool: Option<&Arc<rayon::ThreadPool>>,
) -> Result<ReferenceStars, IntegrationError> {
    let (lum, width, height) = read_luminance(path)?;
    let stars = detect_stars(&lum, width, height, &cfg.detection, cfg.max_stars, pool);
    debug!(path = %path.display(), detections = stars.len(), "reference stars detected");
    Ok(ReferenceStars {
        stars,
        width,
        height,
    })
}

/// Register one subject onto the reference. I/O errors and cancellation
/// are `Err`; an alignment failure is a successful measurement of a frame
/// that cannot be registered (`outcome: Err(AlignError)`).
pub fn register_frame(
    reference: &ReferenceStars,
    subject: &Path,
    cfg: &RegistrationConfig,
    pool: Option<&Arc<rayon::ThreadPool>>,
    cancel: &AtomicBool,
) -> Result<FrameRegistration, IntegrationError> {
    let start = Instant::now();
    if cancel.load(Ordering::Relaxed) {
        return Err(IntegrationError::Cancelled);
    }
    let (lum, width, height) = read_luminance(subject)?;
    let stars = detect_stars(&lum, width, height, &cfg.detection, cfg.max_stars, pool);
    drop(lum);
    if cancel.load(Ordering::Relaxed) {
        return Err(IntegrationError::Cancelled);
    }
    let outcome = align(
        &stars,
        &reference.stars,
        (reference.width, reference.height),
        (width, height),
        cfg,
    );
    let duration_ms = start.elapsed().as_millis() as u64;
    match &outcome {
        Ok(a) => {
            debug!(
                path = %subject.display(),
                detections = stars.len(),
                inliers = a.inliers,
                rms_px = a.rms_px,
                model = %model_name(a.model, a.distortion_order),
                flipped = a.flipped,
                duration_ms,
                "frame registered"
            );
            for note in &a.warnings {
                warn!(path = %subject.display(), note = %note, "registration warning");
            }
        }
        Err(e) => {
            warn!(path = %subject.display(), detections = stars.len(), error = %e, duration_ms, "frame registration failed")
        }
    }
    Ok(FrameRegistration {
        width,
        height,
        detections: stars.len(),
        outcome,
        duration_ms,
    })
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
            distortion_order: None,
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
            rec.model = Some(model_name(a.model, a.distortion_order));
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
        let reference = reference_stars(&r, &cfg, None).unwrap();
        assert!(reference.stars.len() >= 110, "{}", reference.stars.len());
        assert_eq!((reference.width, reference.height), (640, 480));
        let reg = register_frame(&reference, &s, &cfg, None, &AtomicBool::new(false)).unwrap();
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

    #[test]
    fn rgb_subject_uses_luminance_and_cancel_is_honoured() {
        let dir = tempfile::tempdir().unwrap();
        let (r, s, _) = pair(dir.path(), 22, -3.0, 2.5, 0.0, 3);
        let cfg = RegistrationConfig::default();
        let reference = reference_stars(&r, &cfg, None).unwrap();
        let reg = register_frame(&reference, &s, &cfg, None, &AtomicBool::new(false)).unwrap();
        let a = reg.outcome.as_ref().unwrap();
        assert!(
            (a.translation.0 - 3.0).abs() < 0.05 && (a.translation.1 + 2.5).abs() < 0.05,
            "{:?}",
            a.translation
        );
        assert!(matches!(
            register_frame(&reference, &s, &cfg, None, &AtomicBool::new(true)),
            Err(IntegrationError::Cancelled)
        ));
        let bad = dir.path().join("nope.txt");
        std::fs::write(&bad, b"x").unwrap();
        assert!(matches!(
            register_frame(&reference, &bad, &cfg, None, &AtomicBool::new(false)),
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
        let reference = reference_stars(&r, &cfg, None).unwrap();
        let reg = register_frame(&reference, &s2, &cfg, None, &AtomicBool::new(false)).unwrap();
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
        let reference = reference_stars(&r, &RegistrationConfig::default(), None).unwrap();
        let reg = identity_registration(&reference);
        let a = reg.outcome.as_ref().unwrap();
        assert_eq!(a.map.forward(10.5, 20.25), (10.5, 20.25));
        let rec = to_record(1, 3, 3, true, &reg, "h", "t");
        assert!(rec.is_reference && rec.status == "reference");
        assert_eq!(rec.matched_stars, reference.stars.len() as i64);
        assert_eq!(rec.affine_a1, Some(1.0));
        assert_eq!(rec.model.as_deref(), Some("similarity"));
    }
}
