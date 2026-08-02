//! Fixed v1 master-file naming (spec §2). No token engine — the layout is a
//! deliberate constant; a user-configurable template is future work.

use crate::archive::path_layout::sanitize_for_filename;
use crate::fits_writer::keywords::FrameKind;
use std::path::{Path, PathBuf};

/// Relative path inside the library root, per the fixed v1 template:
/// `<INSTRUME>/<MasterType>/master_dark_300s_-10C_g100_bin1_2026-06-28.fits`
/// Flats insert the filter token after the type. Missing values collapse to
/// nothing (no "NaN" junk in filenames).
pub struct MasterPathParams<'a> {
    pub instrume: Option<&'a str>,
    pub master_kind: FrameKind, // MasterDark | MasterFlat | MasterBias | MasterDarkFlat
    pub filter: Option<&'a str>,
    pub exptime: Option<f64>,
    pub ccd_temp: Option<f64>,
    pub gain: Option<f64>,
    pub binning: Option<&'a str>,
    pub date: &'a str, // YYYY-MM-DD (calibration_set.date)
}

fn kind_folder(kind: FrameKind) -> &'static str {
    match kind {
        FrameKind::MasterDark => "MasterDark",
        FrameKind::MasterFlat => "MasterFlat",
        FrameKind::MasterBias => "MasterBias",
        FrameKind::MasterDarkFlat => "MasterDarkFlat",
        _ => "Master",
    }
}

fn kind_stem(kind: FrameKind) -> &'static str {
    match kind {
        FrameKind::MasterDark => "master_dark",
        FrameKind::MasterFlat => "master_flat",
        FrameKind::MasterBias => "master_bias",
        FrameKind::MasterDarkFlat => "master_darkflat",
        _ => "master",
    }
}

/// Trim trailing zeros: 300.0 -> "300", 1.55 -> "1.55".
fn fmt_num(v: f64) -> String {
    let s = format!("{v:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

pub fn master_relative_path(p: &MasterPathParams) -> PathBuf {
    let camera = p
        .instrume
        .map(sanitize_for_filename)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "UnknownCamera".to_string());
    let mut parts: Vec<String> = vec![kind_stem(p.master_kind).to_string()];
    if matches!(
        p.master_kind,
        FrameKind::MasterFlat | FrameKind::MasterDarkFlat
    ) {
        if let Some(f) = p.filter {
            let f = sanitize_for_filename(f);
            if !f.is_empty() {
                parts.push(f);
            }
        }
    }
    if let Some(e) = p.exptime {
        parts.push(format!("{}s", fmt_num(e)));
    }
    if let Some(t) = p.ccd_temp {
        parts.push(format!("{}C", fmt_num(t.round())));
    }
    if let Some(g) = p.gain {
        parts.push(format!("g{}", fmt_num(g)));
    }
    if let Some(b) = p.binning {
        let b = sanitize_for_filename(b);
        if !b.is_empty() {
            parts.push(format!("bin{b}"));
        }
    }
    parts.push(p.date.to_string());
    PathBuf::from(camera)
        .join(kind_folder(p.master_kind))
        .join(format!("{}.fits", parts.join("_")))
}

/// Sanitize `s`, falling back to `fallback` when that collapses to empty
/// (all-whitespace/reserved-char input) — same "no empty path segment"
/// guard [`master_relative_path`] applies to its camera token.
fn sanitized_or(s: &str, fallback: &str) -> String {
    let sanitized = sanitize_for_filename(s);
    if sanitized.is_empty() {
        fallback.to_string()
    } else {
        sanitized
    }
}

/// Relative path inside the Calibration Library root for a calibrated LIGHT
/// output (design spec 2026-07-05-light-calibration-design.md §3):
/// `<OBJECT sanitized>/<INSTRUME sanitized>/<DATE-OBS date>/c_<original filename>`.
/// Uses the same sanitizer + "Unknown…" fallback idiom as
/// [`master_relative_path`]. `date_obs_date` (YYYY-MM-DD) and
/// `original_filename` are taken as-is — the date is already filesystem-safe
/// and the filename came from a real file on disk, so re-sanitizing it would
/// only risk mangling a name that's already valid. The caller joins the
/// library root and applies [`resolve_collision`].
pub fn calibrated_light_relative_path(
    object: &str,
    instrume: &str,
    date_obs_date: &str,
    original_filename: &str,
) -> PathBuf {
    let object = sanitized_or(object, "UnknownObject");
    let instrume = sanitized_or(instrume, "UnknownCamera");
    PathBuf::from(object)
        .join(instrume)
        .join(date_obs_date)
        .join(format!("c_{original_filename}"))
}

/// First non-existing variant of `abs`: abs, then stem_2.fits, stem_3.fits…
pub fn resolve_collision(abs: &Path) -> PathBuf {
    if !abs.exists() {
        return abs.to_path_buf();
    }
    let stem = abs.file_stem().and_then(|s| s.to_str()).unwrap_or("master");
    let ext = abs.extension().and_then(|s| s.to_str()).unwrap_or("fits");
    for n in 2u32.. {
        let candidate = abs.with_file_name(format!("{stem}_{n}.{ext}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    unreachable!()
}

/// Like [`resolve_collision`], but a candidate is free only when it is BOTH
/// absent on disk AND not claimed by the catalog domain the output registers
/// into (`is_taken`). A catalog row that outlived its on-disk file otherwise
/// wedges every future build on a UNIQUE-path constraint (2026-07-22 audit
/// F4b) — and the failure path used to delete the freshly built file,
/// making the state permanent.
pub fn resolve_collision_free(abs: &Path, is_taken: &dyn Fn(&str) -> bool) -> PathBuf {
    let free = |p: &Path| !p.exists() && !is_taken(&p.to_string_lossy());
    if free(abs) {
        return abs.to_path_buf();
    }
    let stem = abs.file_stem().and_then(|s| s.to_str()).unwrap_or("master");
    let ext = abs.extension().and_then(|s| s.to_str()).unwrap_or("fits");
    for n in 2u32.. {
        let candidate = abs.with_file_name(format!("{stem}_{n}.{ext}"));
        if free(&candidate) {
            return candidate;
        }
    }
    unreachable!()
}

/// Resolve AND atomically claim an output path: the chosen name is created as
/// a zero-byte placeholder with `create_new`, so two concurrent builds can
/// never resolve to the same target. [`resolve_collision_free`] only *looks*
/// (check-then-write) — at `compute.max_concurrent > 1` both builds saw the
/// same name free and the loser's atomic rename silently replaced the
/// winner's master, leaving catalog metadata describing foreign pixels
/// (2026-08-02 audit I7). Claiming makes the winner's name unavailable to
/// everyone else the instant it is chosen.
///
/// The caller's own atomic tmp+rename overwrites the placeholder; on any
/// failure before those bytes land, call [`release_claim`]. The parent
/// directory must exist — a claim is a file creation, not a resolution.
///
/// `is_taken` is the catalog-side predicate [`resolve_collision_free`]
/// documents (audit F4b); a name it rejects is skipped WITHOUT minting a
/// placeholder for it.
pub fn claim_collision_free(
    abs: &Path,
    is_taken: &dyn Fn(&str) -> bool,
) -> std::io::Result<PathBuf> {
    fn try_claim(p: &Path) -> std::io::Result<bool> {
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(p)
        {
            Ok(_) => Ok(true),
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => Ok(false),
            Err(e) => Err(e),
        }
    }
    if !is_taken(&abs.to_string_lossy()) && try_claim(abs)? {
        return Ok(abs.to_path_buf());
    }
    let stem = abs.file_stem().and_then(|s| s.to_str()).unwrap_or("master");
    let ext = abs.extension().and_then(|s| s.to_str()).unwrap_or("fits");
    for n in 2u32.. {
        let candidate = abs.with_file_name(format!("{stem}_{n}.{ext}"));
        if !is_taken(&candidate.to_string_lossy()) && try_claim(&candidate)? {
            return Ok(candidate);
        }
    }
    unreachable!()
}

/// Remove a still-EMPTY claim placeholder minted by [`claim_collision_free`],
/// and only that: a real output is never zero bytes, so this can never delete
/// a fully built file (the F4b stance documented on
/// [`resolve_collision_free`] — the failure path used to delete the freshly
/// built master, making the divergence permanent). A missing path or a
/// non-empty file is a silent no-op, so it is safe to call on any exit.
pub fn release_claim(p: &Path) {
    if let Ok(m) = std::fs::metadata(p) {
        if m.len() == 0 {
            let _ = std::fs::remove_file(p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::keywords::FrameKind;

    #[test]
    fn dark_path_shape() {
        let p = master_relative_path(&MasterPathParams {
            instrume: Some("ZWO ASI2600MM Pro"),
            master_kind: FrameKind::MasterDark,
            filter: None,
            exptime: Some(300.0),
            ccd_temp: Some(-10.2),
            gain: Some(100.0),
            binning: Some("1x1"),
            date: "2026-06-28",
        });
        // sanitize_for_filename replaces whitespace with '_' (see
        // archive::path_layout::sanitize_for_filename), so the camera token
        // is "ZWO_ASI2600MM_Pro", not the raw "ZWO ASI2600MM Pro".
        assert_eq!(
            p.to_string_lossy(),
            "ZWO_ASI2600MM_Pro/MasterDark/master_dark_300s_-10C_g100_bin1x1_2026-06-28.fits"
        );
    }

    #[test]
    fn flat_includes_filter_and_missing_fields_collapse() {
        let p = master_relative_path(&MasterPathParams {
            instrume: Some("cam"),
            master_kind: FrameKind::MasterFlat,
            filter: Some("Ha"),
            exptime: Some(1.55),
            ccd_temp: None,
            gain: None,
            binning: None,
            date: "2026-07-01",
        });
        assert_eq!(
            p.to_string_lossy(),
            "cam/MasterFlat/master_flat_Ha_1.55s_2026-07-01.fits"
        );
    }

    #[test]
    fn unknown_camera_bucket() {
        let p = master_relative_path(&MasterPathParams {
            instrume: None,
            master_kind: FrameKind::MasterBias,
            filter: None,
            exptime: None,
            ccd_temp: None,
            gain: None,
            binning: None,
            date: "2026-01-01",
        });
        assert!(p.starts_with("UnknownCamera/MasterBias/"), "{p:?}");
    }

    #[test]
    fn relative_path_sanitizes_and_prefixes() {
        // Assert against whatever the existing sanitizer produces for these
        // inputs (called directly), not a hardcoded literal — the point is
        // that calibrated_light_relative_path reuses the same sanitizer
        // master paths use, not a second implementation of it.
        let object = sanitize_for_filename("M 31");
        let instrume = sanitize_for_filename("ZWO ASI2600MM Pro");
        let p = calibrated_light_relative_path(
            "M 31",
            "ZWO ASI2600MM Pro",
            "2026-06-01",
            "L_0001.fits",
        );
        assert_eq!(
            p,
            PathBuf::from(format!("{object}/{instrume}/2026-06-01/c_L_0001.fits"))
        );
    }

    #[test]
    fn collision_suffixes() {
        let dir = tempfile::tempdir().unwrap();
        let base = dir.path().join("m.fits");
        assert_eq!(resolve_collision(&base), base);
        std::fs::write(&base, b"x").unwrap();
        assert_eq!(resolve_collision(&base), dir.path().join("m_2.fits"));
        std::fs::write(dir.path().join("m_2.fits"), b"x").unwrap();
        assert_eq!(resolve_collision(&base), dir.path().join("m_3.fits"));
    }

    #[test]
    fn resolve_collision_free_skips_catalog_taken_paths() {
        let dir = tempfile::tempdir().unwrap();
        let abs = dir.path().join("master_dark_300s.fits");

        // Disk-free + catalog-free → as-is.
        let taken_none = |_: &str| false;
        assert_eq!(resolve_collision_free(&abs, &taken_none), abs);

        // Disk-free but a catalog row survived its file (audit F4b): today's
        // disk-only resolve_collision returns `abs` and registration dies on
        // UNIQUE files.path forever — the resolver must suffix past it.
        let phantom = abs.to_string_lossy().to_string();
        let taken_phantom = move |p: &str| p == phantom;
        assert_eq!(
            resolve_collision_free(&abs, &taken_phantom),
            dir.path().join("master_dark_300s_2.fits")
        );

        // Disk-taken behaves like resolve_collision.
        std::fs::write(&abs, b"x").unwrap();
        assert_eq!(
            resolve_collision_free(&abs, &taken_none),
            dir.path().join("master_dark_300s_2.fits")
        );
    }

    #[test]
    fn claim_collision_free_hands_concurrent_builds_different_names() {
        let dir = tempfile::tempdir().unwrap();
        let abs = dir.path().join("master_dark_300s.fits");
        let taken_none = |_: &str| false;

        // First builder wins the base name — and the name is now HELD on disk
        // as a zero-byte placeholder, not merely "observed to be free".
        let first = claim_collision_free(&abs, &taken_none).unwrap();
        assert_eq!(first, abs);
        assert_eq!(std::fs::metadata(&first).unwrap().len(), 0);

        // Second builder resolving the same base name (audit I7: with
        // check-then-write resolve_collision_free it got `abs` BACK and its
        // rename silently overwrote the first builder's master).
        let second = claim_collision_free(&abs, &taken_none).unwrap();
        assert_ne!(second, first);
        assert_eq!(second, dir.path().join("master_dark_300s_2.fits"));
        assert_eq!(std::fs::metadata(&second).unwrap().len(), 0);

        // …and a third keeps counting.
        let third = claim_collision_free(&abs, &taken_none).unwrap();
        assert_eq!(third, dir.path().join("master_dark_300s_3.fits"));
    }

    #[test]
    fn claim_collision_free_skips_catalog_taken_names() {
        let dir = tempfile::tempdir().unwrap();
        let abs = dir.path().join("m.fits");
        let phantom = abs.to_string_lossy().to_string();
        let taken_phantom = move |p: &str| p == phantom;

        // Same F4b rule as resolve_collision_free: a catalog row that outlived
        // its file makes the name unusable — and no placeholder is minted for
        // it either (claiming it would strand a 0-byte file forever).
        let got = claim_collision_free(&abs, &taken_phantom).unwrap();
        assert_eq!(got, dir.path().join("m_2.fits"));
        assert!(!abs.exists(), "catalog-taken name must not be claimed");
    }

    #[test]
    fn release_claim_removes_only_empty_placeholders() {
        let dir = tempfile::tempdir().unwrap();
        let claim = dir.path().join("claim.fits");
        let built = dir.path().join("built.fits");
        claim_collision_free(&claim, &|_: &str| false).unwrap();
        std::fs::write(&built, b"SIMPLE  =                    T").unwrap();

        release_claim(&claim);
        assert!(!claim.exists(), "an empty claim must be released");

        // The "never delete a fully built file" stance (F4b) holds by
        // construction: a real master is never zero bytes.
        release_claim(&built);
        assert!(built.exists(), "a non-empty output must survive");

        // Idempotent on an already-gone path.
        release_claim(&claim);
    }
}
