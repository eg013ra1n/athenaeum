//! Package reader/validator: parse the NDJSON manifest and re-verify every
//! payload file against its record.

use std::fs;
use std::path::Path;

use anyhow::{bail, Context, Result};

use super::manifest::ManifestRecord;
use super::{xxh3_full_file, MANIFEST_FILENAME};

/// Parse `manifest.ndjson` in `dir` into records. Blank lines are skipped;
/// unknown JSON keys are ignored (forward compatibility).
pub fn read_manifest(dir: &Path) -> Result<Vec<ManifestRecord>> {
    let manifest_path = dir.join(MANIFEST_FILENAME);
    let text = fs::read_to_string(&manifest_path)
        .with_context(|| format!("read manifest {}", manifest_path.display()))?;

    let mut records = Vec::new();
    for (i, line) in text.lines().enumerate() {
        if line.trim().is_empty() {
            continue;
        }
        let record: ManifestRecord = serde_json::from_str(line).with_context(|| {
            format!(
                "parse manifest line {} of {}",
                i + 1,
                manifest_path.display()
            )
        })?;
        records.push(record);
    }
    Ok(records)
}

/// Validate a package directory: for every manifest record, the payload file
/// must exist, match its recorded `byte_size`, and re-hash to the recorded
/// full-content `xxh3`. The first failure returns an error naming the
/// offending `rel_path`.
pub fn validate_package(dir: &Path) -> Result<()> {
    let records = read_manifest(dir)?;

    for record in &records {
        // Guard the untrusted rel_path before joining it onto `dir` — this
        // "verify a received package" helper must refuse a traversal/absolute
        // rel_path rather than stat/hash an arbitrary file on disk (finding L1).
        super::validate_rel_path(&record.rel_path)
            .with_context(|| format!("reject unsafe rel_path {}", record.rel_path))?;
        let path = dir.join(&record.rel_path);

        let meta = fs::metadata(&path)
            .with_context(|| format!("missing payload file for rel_path {}", record.rel_path))?;

        if meta.len() != record.byte_size {
            tracing::debug!(rel_path = %record.rel_path, "package payload size mismatch");
            bail!(
                "size mismatch for {}: manifest {} bytes, on disk {} bytes",
                record.rel_path,
                record.byte_size,
                meta.len()
            );
        }

        let actual = xxh3_full_file(&path)
            .with_context(|| format!("hash payload file {}", record.rel_path))?;
        if actual != record.xxh3 {
            tracing::debug!(rel_path = %record.rel_path, "package payload hash mismatch");
            bail!(
                "xxh3 mismatch for {}: manifest {}, on disk {}",
                record.rel_path,
                record.xxh3,
                actual
            );
        }
    }

    tracing::debug!(path = %dir.display(), count = records.len(), "package validated");
    Ok(())
}
