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
    out.trim_matches('_').to_string()
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
    fn add_suffix_works() {
        let p = Path::new("/tmp/M31_Lights.zip");
        assert_eq!(add_suffix(p, 2), Path::new("/tmp/M31_Lights (2).zip"));
        assert_eq!(add_suffix(p, 3), Path::new("/tmp/M31_Lights (3).zip"));
    }
}
