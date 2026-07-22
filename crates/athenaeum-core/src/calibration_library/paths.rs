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
}
