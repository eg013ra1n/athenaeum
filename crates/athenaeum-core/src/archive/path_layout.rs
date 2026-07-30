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
    let out = out.trim_matches('_').to_string();
    windows_safe_component(&out, "")
}

/// Windows-safety tail shared by every generated folder/file-name sanitizer:
/// trim trailing dots/spaces (Win32 silently strips them, desyncing the
/// on-disk name from the catalog's), defuse reserved DOS device basenames
/// (CON/PRN/AUX/NUL/COM0-9/LPT0-9 plus the superscript COM¹²³/LPT¹²³ forms —
/// resolved from the pre-first-dot token), and substitute `fallback` for a
/// component that sanitized away to nothing ("", ".", ".." all end here —
/// ".." would otherwise climb OUT of the chosen output folder).
pub fn windows_safe_component(s: &str, fallback: &str) -> String {
    let out = s.trim_end_matches(['.', ' ']).to_string();
    if out.is_empty() {
        return fallback.to_string();
    }
    let base = out.split('.').next().unwrap_or("");
    let upper = base.to_ascii_uppercase();
    let reserved = matches!(upper.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (upper.chars().count() == 4
            && (upper.starts_with("COM") || upper.starts_with("LPT"))
            && matches!(upper.chars().nth(3), Some('0'..='9' | '¹' | '²' | '³')));
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
/// Sanitizes FIRST: a value that is non-empty but sanitizes away to nothing
/// (e.g. "???", "..") must fall back to `Unknown` too, not leave an empty
/// token that collapses two `_` separators in the zip filename.
fn token(value: Option<&str>) -> String {
    let s = sanitize_for_filename(value.unwrap_or(""));
    if s.is_empty() {
        "Unknown".to_string()
    } else {
        s
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
    // The rel-path derivation must fold case exactly like `path_starts_with_fold`,
    // which is what DECIDES that this scan root matched in the first place. If a
    // root matches case-insensitively but the exact-case `strip_prefix` here does
    // not, every file drops to the basename fallback and the whole subtree
    // flattens into `<Root>/<basename>` inside the zip (then trips the planner's
    // in-zip collision guard, turning a case mismatch into a hard plan failure).
    let rel = source_file
        .strip_prefix(scan_root)
        .ok()
        .map(|p| p.to_path_buf())
        .or_else(|| strip_prefix_fold(source_file, scan_root));
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

/// Reverse of [`path_in_zip`] for the restore-suggestion UI: strip as many
/// trailing components off `source_path` as `path_in_zip` carries, yielding
/// the directory the archive layout was rooted at (the scan root's parent).
/// Component-COUNT based and separator-agnostic: `path_in_zip` is always
/// '/'-separated (zip convention) while `source_path` is a native OS path —
/// the old `strip_suffix(&path_in_zip)` string compare could never match a
/// '\'-separated source, so on Windows the "Original location" restore option
/// was permanently disabled and the dialog fell back to relocating the data
/// under an arbitrary scan root.
pub fn original_parent_for_restore(source_path: &str, path_in_zip: &str) -> Option<String> {
    let n = path_in_zip.split('/').filter(|c| !c.is_empty()).count();
    let mut end = source_path.trim_end_matches(['/', '\\']).len();
    for _ in 0..n {
        end = source_path[..end].rfind(['/', '\\'])?;
    }
    let parent = source_path[..end].trim_end_matches(['/', '\\']);
    // A separator-free remainder is never a usable absolute directory: it can
    // only be a drive-relative designator (`C:`, which resolves against the
    // process's per-drive CWD) or a bare name. Suggest nothing rather than a
    // destination that lands somewhere unpredictable.
    if parent.is_empty() || !parent.contains(['/', '\\']) {
        None
    } else {
        Some(parent.to_string())
    }
}

/// Component-wise "is `path` under (or equal to) `root`", case-folded on
/// case-insensitive hosts (Windows/macOS). Plain `Path::starts_with` is
/// exact-case, which classified `C:\astro\…` as OUTSIDE root `C:\Astro` even
/// though NTFS treats them as one directory — flipping a restore from
/// "put files back" to "relocate under root", and flattening archive layouts.
pub(crate) fn path_starts_with_fold(path: &Path, root: &Path) -> bool {
    if path.starts_with(root) {
        return true;
    }
    if !cfg!(any(windows, target_os = "macos")) {
        return false;
    }
    let comps = |p: &Path| -> Vec<String> {
        p.components()
            .map(|c| c.as_os_str().to_string_lossy().to_lowercase())
            .collect()
    };
    let (a, b) = (comps(path), comps(root));
    a.len() >= b.len() && a[..b.len()] == b[..]
}

/// Case-folded component-wise `Path::strip_prefix`, the rel-path counterpart of
/// [`path_starts_with_fold`] (same hosts, same comparison). Returns `None` off
/// case-insensitive hosts, and for anything that is not a STRICT descendant of
/// `root` — a path equal to the root has no remainder to place in a zip.
fn strip_prefix_fold(path: &Path, root: &Path) -> Option<PathBuf> {
    if !cfg!(any(windows, target_os = "macos")) {
        return None;
    }
    let p: Vec<_> = path.components().collect();
    let r: Vec<_> = root.components().collect();
    if p.len() <= r.len() {
        return None;
    }
    let fold = |c: &std::path::Component| c.as_os_str().to_string_lossy().to_lowercase();
    if p[..r.len()].iter().map(fold).ne(r.iter().map(fold)) {
        return None;
    }
    Some(p[r.len()..].iter().map(|c| c.as_os_str()).collect())
}

/// Join an always-'/'-separated `path_in_zip` under `root` component-wise, so
/// the resulting (and later CATALOG-PERSISTED) path uses only native
/// separators — `root.join(path_in_zip)` on Windows produced the mixed
/// spelling `C:\root\Lights/M31/x.fits` in `files.path`.
pub(crate) fn dest_under_root(root: &Path, path_in_zip: &str) -> PathBuf {
    let mut d = root.to_path_buf();
    for comp in path_in_zip.split('/').filter(|c| !c.is_empty()) {
        d.push(comp);
    }
    d
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
    // The date segment is a path component like the camera one: sanitize it the
    // same way (a malformed/short DATE-OBS prefix must not smuggle separators or
    // reserved forms into the archive layout) and fall back when it empties out.
    let date_raw = sanitize_for_filename(date_start.get(..10).unwrap_or(""));
    let date = if date_raw.is_empty() {
        "unknown-date".to_string()
    } else {
        date_raw
    };
    PathBuf::from("Calibration_Archive").join(cam).join(date)
}

/// Sanitize a `DATE-OBS`-derived 10-char date prefix into a single filename
/// token, falling back to `x` when it sanitizes away to nothing.
fn sanitized_date_token(date: &str) -> String {
    let t = sanitize_for_filename(date.get(..10).unwrap_or(""));
    if t.is_empty() {
        "x".to_string()
    } else {
        t
    }
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
    // DATE-OBS is file-supplied text: sanitize the date prefixes exactly like
    // `calibration_zip_dir` does, or a value such as `../../../etc` would put
    // separators into a name later fed to `PathBuf::join` (audit F8). A
    // well-formed ISO date survives verbatim (hyphens are preserved).
    parts.push(sanitized_date_token(date_start));
    parts.push(sanitized_date_token(date_end));
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
        // Microsoft's CURRENT reserved list also covers COM0/LPT0 and the
        // superscript COM¹²³/LPT¹²³ forms — defused like the rest.
        assert_eq!(sanitize_for_filename("COM0"), "COM0_");
        assert_eq!(sanitize_for_filename("lpt0"), "lpt0_");
        assert_eq!(sanitize_for_filename("COM³"), "COM³_");
        // Not reserved: COM10 (two digits), plain names, inner dots.
        assert_eq!(sanitize_for_filename("com10"), "com10");
        assert_eq!(sanitize_for_filename("M31"), "M31");
        assert_eq!(sanitize_for_filename("DMK 41AU02.AS"), "DMK_41AU02.AS");
    }

    #[test]
    fn windows_safe_component_covers_current_reserved_list() {
        assert_eq!(windows_safe_component("COM0", "X"), "COM0_");
        assert_eq!(windows_safe_component("LPT0", "X"), "LPT0_");
        assert_eq!(windows_safe_component("LPT²", "X"), "LPT²_");
        assert_eq!(windows_safe_component("COM10", "X"), "COM10");
        assert_eq!(windows_safe_component("M31.", "X"), "M31");
        assert_eq!(windows_safe_component("..", "X"), "X");
        assert_eq!(windows_safe_component("", "X"), "X");
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

    /// The fold-aware root match in the planner is only half the fix: if the
    /// rel-path strip stays exact-case, a matched root flattens its whole
    /// subtree onto the basename fallback.
    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn path_in_zip_strips_case_variant_root() {
        let z = path_in_zip(
            "Astro",
            Path::new("/data/Astro"),
            Path::new("/data/astro/M31/L/x.fits"),
        );
        assert_eq!(z, "Astro/M31/L/x.fits");
    }

    #[test]
    fn path_in_zip_non_descendant_still_falls_back_to_basename() {
        let z = path_in_zip(
            "Root",
            Path::new("/data/Astro"),
            Path::new("/elsewhere/x.fits"),
        );
        assert_eq!(z, "Root/x.fits");
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
    fn calibration_zip_filename_sanitizes_date_tokens() {
        // Well-formed ISO dates must survive verbatim (hyphens are preserved).
        let ok = calibration_zip_filename(
            Some("ASI2600MM"), "Dark", None, None,
            "2025-10-12T20:00:00Z", "2025-10-12T21:00:00Z",
        );
        assert_eq!(ok, "ASI2600MM_Dark_2025-10-12_2025-10-12.zip");

        // DATE-OBS is file-supplied: a traversal payload must not survive as
        // separators, so joining the name can never climb out of the archive
        // root (audit F8).
        let evil = calibration_zip_filename(
            Some("ASI2600MM"), "Dark", None, None,
            "../../../etc", "2025-10-12T21:00:00Z",
        );
        assert!(!evil.contains('/'), "no '/': {evil}");
        assert!(!evil.contains('\\'), "no '\\': {evil}");
        // The whole thing stays ONE path component, so joining it under a root
        // leaves that root as the parent — nothing climbed.
        let joined = Path::new("/archive/root").join(&evil);
        assert_eq!(joined.parent(), Some(Path::new("/archive/root")));
        assert!(
            !Path::new(&evil).components().any(|c| c.as_os_str() == ".."),
            "no '..' component: {evil}"
        );
    }

    #[test]
    fn calibration_zip_filename_collapses_missing_gain_and_exptime() {
        let f = calibration_zip_filename(
            None, "Bias", None, None, "2026-06-28T20:00:00Z", "2026-06-28T21:00:00Z",
        );
        assert_eq!(f, "UnknownCamera_Bias_2026-06-28_2026-06-28.zip");
    }

    #[test]
    fn original_parent_component_based_both_separator_styles() {
        // POSIX source vs '/'-separated zip path:
        assert_eq!(
            original_parent_for_restore("/data/Astro/M31/x.fits", "Astro/M31/x.fits"),
            Some("/data".to_string())
        );
        // Windows source vs the SAME '/'-separated zip path — the old string
        // strip_suffix could never match this:
        assert_eq!(
            original_parent_for_restore(r"C:\data\Astro\M31\x.fits", "Astro/M31/x.fits"),
            Some(r"C:\data".to_string())
        );
        // Stripping consumes the whole path -> None (parity with the old code).
        assert_eq!(
            original_parent_for_restore("/Astro/M31/x.fits", "Astro/M31/x.fits"),
            None
        );
        // Fallback two-component zip path over a shallow source: strips both
        // components, leaving the bare drive designator `C:` — drive-relative,
        // not an absolute directory, so no suggestion is offered.
        assert_eq!(
            original_parent_for_restore(r"C:\stray\x.fits", "Root/x.fits"),
            None
        );
    }

    #[test]
    fn starts_with_fold_is_component_wise() {
        assert!(path_starts_with_fold(
            Path::new("/photos/astro/x.fits"),
            Path::new("/photos/astro")
        ));
        assert!(!path_starts_with_fold(
            Path::new("/photos/astro2/x.fits"),
            Path::new("/photos/astro")
        ));
    }

    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn starts_with_fold_case_folds_on_case_insensitive_hosts() {
        assert!(path_starts_with_fold(
            Path::new("/data/astro/x.fits"),
            Path::new("/data/Astro")
        ));
    }

    #[test]
    fn dest_under_root_joins_component_wise() {
        let d = dest_under_root(Path::new("/r"), "Lights/M31/x.fits");
        let comps: Vec<_> = d
            .components()
            .map(|c| c.as_os_str().to_string_lossy().to_string())
            .collect();
        assert!(comps.ends_with(&["r".into(), "Lights".into(), "M31".into(), "x.fits".into()]));
    }

    #[test]
    fn add_suffix_works() {
        let p = Path::new("/tmp/M31_Lights.zip");
        assert_eq!(add_suffix(p, 2), Path::new("/tmp/M31_Lights (2).zip"));
        assert_eq!(add_suffix(p, 3), Path::new("/tmp/M31_Lights (3).zip"));
    }
}
