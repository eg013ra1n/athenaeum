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
    if matches!(p.master_kind, FrameKind::MasterFlat | FrameKind::MasterDarkFlat) {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fits_writer::keywords::FrameKind;

    #[test]
    fn dark_path_shape() {
        let p = master_relative_path(&MasterPathParams {
            instrume: Some("ZWO ASI2600MM Pro"), master_kind: FrameKind::MasterDark,
            filter: None, exptime: Some(300.0), ccd_temp: Some(-10.2),
            gain: Some(100.0), binning: Some("1x1"), date: "2026-06-28",
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
            instrume: Some("cam"), master_kind: FrameKind::MasterFlat,
            filter: Some("Ha"), exptime: Some(1.55), ccd_temp: None,
            gain: None, binning: None, date: "2026-07-01",
        });
        assert_eq!(
            p.to_string_lossy(),
            "cam/MasterFlat/master_flat_Ha_1.55s_2026-07-01.fits"
        );
    }

    #[test]
    fn unknown_camera_bucket() {
        let p = master_relative_path(&MasterPathParams {
            instrume: None, master_kind: FrameKind::MasterBias,
            filter: None, exptime: None, ccd_temp: None,
            gain: None, binning: None, date: "2026-01-01",
        });
        assert!(p.starts_with("UnknownCamera/MasterBias/"), "{p:?}");
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
}
