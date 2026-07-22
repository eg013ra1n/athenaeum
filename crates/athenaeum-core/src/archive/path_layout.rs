//! Compute zip filenames and path-in-zip strings for the archive feature.

use crate::archive::models::FrameRole;
use std::path::{Path, PathBuf};

/// Sluggify text for inclusion in a filename: replace whitespace with `_`,
/// strip characters that are problematic on common filesystems.
pub fn sanitize_for_filename(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            // Reserved on Windows + many tools' breakage points
            '/' | '\\' | ':' | '*' | '?' | '"' | '<' | '>' | '|' => out.push('_'),
            c if c.is_whitespace() => out.push('_'),
            c if c.is_control() => {} // drop
            c => out.push(c),
        }
    }
    // Collapse multiple underscores
    while out.contains("__") {
        out = out.replace("__", "_");
    }
    let out = out.trim_matches('_').trim_end_matches(['.', ' ']).to_string();
    // Windows reserves CON/PRN/AUX/NUL/COM1-9/LPT1-9 as any path segment,
    // case-insensitive, with or without an extension. The device is resolved
    // from the component BEFORE THE FIRST DOT (`NUL.txt` ≡ `NUL`), so the
    // underscore must break THAT token — not the tail of the whole string.
    let base = out.split('.').next().unwrap_or("");
    let upper = base.to_ascii_uppercase();
    let reserved = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.len() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && matches!(upper.as_bytes()[3], b'1'..=b'9'));
    if reserved {
        match out.find('.') {
            // "NUL.txt" → "NUL_.txt": break the pre-first-dot component.
            Some(i) => format!("{}_{}", &out[..i], &out[i..]),
            None => format!("{out}_"),
        }
    } else {
        out
    }
}

/// Token, with a "Unknown" fallback when the value is None or empty.
fn token(value: Option<&str>) -> String {
    let s = value.unwrap_or("").trim();
    if s.is_empty() {
        "Unknown".to_string()
    } else {
        sanitize_for_filename(s)
    }
}

/// Compute the zip filename for a given frame role and frame-set metadata.
///
/// Format: `{Object}_{StartDate}_{EndDate}_{Telescope}_{Camera}_{FrameType}.zip`
/// All tokens fall back to "Unknown".
pub fn zip_filename(
    object: Option<&str>,
    start_date: Option<&str>,    // YYYY-MM-DD
    end_date: Option<&str>,
    telescope: Option<&str>,
    camera: Option<&str>,
    role: FrameRole,
) -> String {
    format!(
        "{}_{}_{}_{}_{}_{}.zip",
        token(object),
        token(start_date),
        token(end_date),
        token(telescope),
        token(camera),
        role.zip_suffix()
    )
}

/// Resolve unique scan-root prefix names. Given a list of scan_root absolute paths
/// (in arbitrary order), returns a map from path → unique basename. If two roots
/// share a basename, suffixes `_2`, `_3`, ... are appended in input order.
pub fn resolve_scan_root_prefixes(scan_root_paths: &[String]) -> std::collections::HashMap<String, String> {
    use std::collections::HashMap;
    let mut counts: HashMap<String, usize> = HashMap::new();
    let mut out: HashMap<String, String> = HashMap::new();

    for path in scan_root_paths {
        let basename = Path::new(path)
            .file_name()
            .and_then(|s| s.to_str())
            .map(|s| sanitize_for_filename(s))
            .unwrap_or_else(|| "Root".to_string());
        let n = counts.entry(basename.clone()).and_modify(|n| *n += 1).or_insert(1);
        let unique = if *n == 1 {
            basename
        } else {
            format!("{}_{}", basename, n)
        };
        out.insert(path.clone(), unique);
    }
    out
}

/// Compute the path-in-zip for a source file.
///
/// `<UniqueRootName>/<rel-path-from-root>` with forward slashes (zip convention).
/// If the source file is not under `scan_root`, falls back to just `<UniqueRootName>/<basename>`.
pub fn path_in_zip(unique_root_name: &str, scan_root: &Path, source_file: &Path) -> String {
    let rel = source_file.strip_prefix(scan_root).ok();
    let mut buf = PathBuf::from(unique_root_name);
    match rel {
        Some(p) => buf.push(p),
        None => {
            // Fallback: use just the file name
            if let Some(name) = source_file.file_name() {
                buf.push(name);
            }
        }
    }
    // Convert to forward slashes regardless of OS (zip convention).
    buf.to_string_lossy().replace('\\', "/")
}

/// Compute the archive directory for a calibration-set archive-of-originals
/// plan (Task 14): `Calibration_Archive/<Camera>/<YYYY-MM-DD>`, relative to
/// the archive root. Falls back to `UnknownCamera` / `unknown-date` the same
/// way `zip_filename`'s tokens do, but as path segments rather than
/// underscore-joined tokens (so the FrameRole zip-suffix convention doesn't
/// apply here).
pub fn calibration_zip_dir(instrume: Option<&str>, date_start: &str) -> PathBuf {
    let cam = instrume
        .map(sanitize_for_filename)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "UnknownCamera".into());
    let date = date_start.get(..10).unwrap_or("unknown-date");
    PathBuf::from("Calibration_Archive").join(cam).join(date)
}

/// Compute the zip filename for a calibration-set archive-of-originals plan
/// (Task 14): `<Camera>_<Type>_g<gain>_<exptime>s_<date_start>_<date_end>.zip`.
/// Missing optional tokens (gain, exptime) simply collapse out rather than
/// falling back to a placeholder — unlike `zip_filename`'s all-tokens-always
/// shape, since a calibration set may genuinely have no gain recorded (e.g.
/// older CCD-only data).
pub fn calibration_zip_filename(
    instrume: Option<&str>,
    imagetyp: &str,
    gain: Option<f64>,
    exptime: Option<f64>,
    date_start: &str,
    date_end: &str,
) -> String {
    let cam = instrume
        .map(sanitize_for_filename)
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "UnknownCamera".into());
    let mut parts = vec![cam, sanitize_for_filename(imagetyp)];
    if let Some(g) = gain {
        parts.push(format!("g{}", g.round() as i64));
    }
    if let Some(e) = exptime {
        parts.push(format!("{}s", e));
    }
    parts.push(date_start.get(..10).unwrap_or("x").to_string());
    parts.push(date_end.get(..10).unwrap_or("x").to_string());
    format!("{}.zip", parts.join("_"))
}

/// Add a numeric suffix to a zip path before the `.zip` extension.
/// e.g. `/tmp/M31_Lights.zip` + 2 → `/tmp/M31_Lights (2).zip`
pub fn add_suffix(path: &Path, n: u32) -> PathBuf {
    let parent = path.parent().unwrap_or(Path::new(""));
    let stem = path.file_stem().and_then(|s| s.to_str()).unwrap_or("archive");
    let ext = path.extension().and_then(|s| s.to_str()).unwrap_or("zip");
    parent.join(format!("{} ({}).{}", stem, n, ext))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitize_strips_problematic_chars() {
        assert_eq!(sanitize_for_filename("Hello World"), "Hello_World");
        assert_eq!(sanitize_for_filename("foo/bar:baz"), "foo_bar_baz");
        assert_eq!(sanitize_for_filename("a   b"), "a_b");
    }

    #[test]
    fn sanitize_trims_trailing_dots_and_guards_reserved_names() {
        // Windows strips trailing dots at create time; the sanitizer must match,
        // or DB paths diverge from disk (audit F3).
        assert_eq!(sanitize_for_filename("Sh2-155."), "Sh2-155");
        assert_eq!(sanitize_for_filename("NGC 7000 "), "NGC_7000");
        // Dot-segments must not survive as path components (library-root escape).
        assert_eq!(sanitize_for_filename("."), "");
        assert_eq!(sanitize_for_filename(".."), "");
        // Reserved device names (any case, with or without extension) are illegal
        // as any Windows path segment — defuse with a trailing underscore.
        assert_eq!(sanitize_for_filename("NUL"), "NUL_");
        assert_eq!(sanitize_for_filename("nul"), "nul_");
        assert_eq!(sanitize_for_filename("COM3"), "COM3_");
        assert_eq!(sanitize_for_filename("lpt9.fits"), "lpt9_.fits");
        assert_eq!(sanitize_for_filename("NUL.txt"), "NUL_.txt");
        // Not reserved: COM0, COM10, plain names, inner dots.
        assert_eq!(sanitize_for_filename("COM0"), "COM0");
        assert_eq!(sanitize_for_filename("com10"), "com10");
        assert_eq!(sanitize_for_filename("M31"), "M31");
        assert_eq!(sanitize_for_filename("DMK 41AU02.AS"), "DMK_41AU02.AS");
    }

    #[test]
    fn zip_filename_fallbacks() {
        let f = zip_filename(None, None, None, None, None, FrameRole::Light);
        assert_eq!(f, "Unknown_Unknown_Unknown_Unknown_Unknown_Lights.zip");

        let f = zip_filename(
            Some("M 31"), Some("2025-10-12"), Some("2025-10-15"),
            Some("RedCat 51"), Some("ASI2600MM"), FrameRole::Flat,
        );
        assert_eq!(f, "M_31_2025-10-12_2025-10-15_RedCat_51_ASI2600MM_Flats.zip");
    }

    #[test]
    fn resolve_scan_root_prefixes_unique_basenames() {
        let paths = vec!["/Photos/Lights".to_string(), "/Photos/Cal".to_string()];
        let map = resolve_scan_root_prefixes(&paths);
        assert_eq!(map.get("/Photos/Lights").unwrap(), "Lights");
        assert_eq!(map.get("/Photos/Cal").unwrap(), "Cal");
    }

    #[test]
    fn resolve_scan_root_prefixes_duplicate_basenames() {
        let paths = vec![
            "/Disk1/Astro".to_string(),
            "/Disk2/Astro".to_string(),
            "/Disk3/Astro".to_string(),
        ];
        let map = resolve_scan_root_prefixes(&paths);
        let mut values: Vec<String> = map.values().cloned().collect();
        values.sort();
        assert_eq!(values, vec!["Astro", "Astro_2", "Astro_3"]);
    }

    #[test]
    fn path_in_zip_relative() {
        let zip_path = path_in_zip(
            "Lights",
            Path::new("/Photos/Lights"),
            Path::new("/Photos/Lights/M31/2025-10-12/L_001.fits"),
        );
        assert_eq!(zip_path, "Lights/M31/2025-10-12/L_001.fits");
    }

    #[test]
    fn path_in_zip_outside_scan_root_falls_back_to_basename() {
        let zip_path = path_in_zip(
            "Lights",
            Path::new("/Photos/Lights"),
            Path::new("/Other/foo.fits"),
        );
        assert_eq!(zip_path, "Lights/foo.fits");
    }

    #[test]
    fn calibration_zip_dir_uses_camera_and_date() {
        let dir = calibration_zip_dir(Some("ASI2600MM"), "2026-06-28T20:00:00Z");
        assert_eq!(dir, PathBuf::from("Calibration_Archive/ASI2600MM/2026-06-28"));
    }

    #[test]
    fn calibration_zip_dir_falls_back_when_missing() {
        let dir = calibration_zip_dir(None, "");
        assert_eq!(dir, PathBuf::from("Calibration_Archive/UnknownCamera/unknown-date"));
    }

    #[test]
    fn calibration_zip_filename_includes_all_tokens() {
        let f = calibration_zip_filename(
            Some("ASI2600MM"), "Dark", Some(100.0), Some(300.0),
            "2026-06-28T20:00:00Z", "2026-06-28T21:00:00Z",
        );
        assert_eq!(f, "ASI2600MM_Dark_g100_300s_2026-06-28_2026-06-28.zip");
    }

    #[test]
    fn calibration_zip_filename_collapses_missing_gain_and_exptime() {
        let f = calibration_zip_filename(
            None, "Bias", None, None, "2026-06-28T20:00:00Z", "2026-06-28T21:00:00Z",
        );
        assert_eq!(f, "UnknownCamera_Bias_2026-06-28_2026-06-28.zip");
    }

    #[test]
    fn add_suffix_works() {
        let p = Path::new("/tmp/M31_Lights.zip");
        assert_eq!(add_suffix(p, 2), Path::new("/tmp/M31_Lights (2).zip"));
        assert_eq!(add_suffix(p, 3), Path::new("/tmp/M31_Lights (3).zip"));
    }
}
