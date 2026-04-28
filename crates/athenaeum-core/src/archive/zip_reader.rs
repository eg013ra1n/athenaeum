//! Verify a built zip's contents against an expected entry list.

use anyhow::{Context, Result};
use std::collections::HashSet;
use std::fs::File;
use std::io::BufReader;
use std::path::Path;

/// Open `zip_path` and verify it contains exactly the expected entries (by path-in-zip).
/// Returns Ok(()) when all expected entries are present.
/// Returns Err with a message listing missing or extra entries.
pub fn verify_zip_contents(zip_path: &Path, expected_entries: &[String]) -> Result<()> {
    let file = File::open(zip_path)
        .with_context(|| format!("failed to open zip {}", zip_path.display()))?;
    let mut zr = zip::ZipArchive::new(BufReader::new(file))
        .with_context(|| format!("failed to parse zip {}", zip_path.display()))?;

    let mut found: HashSet<String> = HashSet::with_capacity(zr.len());
    for i in 0..zr.len() {
        let entry = zr.by_index(i)
            .with_context(|| format!("failed to read entry {} from zip", i))?;
        found.insert(entry.name().to_string());
    }

    let expected: HashSet<String> = expected_entries.iter().cloned().collect();

    let missing: Vec<&String> = expected.difference(&found).collect();
    if !missing.is_empty() {
        let names: Vec<&str> = missing.iter().map(|s| s.as_str()).collect();
        anyhow::bail!("zip {} missing entries: {:?}", zip_path.display(), names);
    }

    let extra: Vec<&String> = found.difference(&expected).collect();
    if !extra.is_empty() {
        let names: Vec<&str> = extra.iter().map(|s| s.as_str()).collect();
        anyhow::bail!("zip {} has unexpected entries: {:?}", zip_path.display(), names);
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::zip_writer::{build_zip, ZipEntry};
    use crate::archive::models::ArchiveCompression;
    use tempfile::TempDir;

    fn write_zip_with(tmp: &TempDir, entries: &[(&str, &[u8])]) -> std::path::PathBuf {
        let zip_path = tmp.path().join("v.zip");
        let mut zip_entries = Vec::new();
        let mut sources = Vec::new();
        for (i, (name, data)) in entries.iter().enumerate() {
            let p = tmp.path().join(format!("src_{}.bin", i));
            std::fs::write(&p, data).unwrap();
            sources.push(p.clone());
            zip_entries.push(ZipEntry { source_path: p, path_in_zip: (*name).to_string() });
        }
        build_zip(&zip_path, &zip_entries, ArchiveCompression::Store).unwrap();
        zip_path
    }

    #[test]
    fn verifies_match() {
        let tmp = TempDir::new().unwrap();
        let zp = write_zip_with(&tmp, &[("a", b"hi"), ("b", b"there")]);
        verify_zip_contents(&zp, &["a".into(), "b".into()]).unwrap();
    }

    #[test]
    fn detects_missing() {
        let tmp = TempDir::new().unwrap();
        let zp = write_zip_with(&tmp, &[("a", b"hi")]);
        let err = verify_zip_contents(&zp, &["a".into(), "b".into()]).unwrap_err();
        assert!(format!("{}", err).contains("missing entries"), "{}", err);
    }

    #[test]
    fn detects_extra() {
        let tmp = TempDir::new().unwrap();
        let zp = write_zip_with(&tmp, &[("a", b"hi"), ("b", b"x")]);
        let err = verify_zip_contents(&zp, &["a".into()]).unwrap_err();
        assert!(format!("{}", err).contains("unexpected entries"), "{}", err);
    }
}
