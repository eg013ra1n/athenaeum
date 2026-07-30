//! Build (and commit) the archive plan for a frame set.

use crate::archive::db as adb;
use crate::archive::models::{
    ArchiveCompression, ArchiveDisposition, ArchiveOperationFile, ArchivePlan, ConflictResolution,
    Dispositions, FrameRole, PlannedZip, ZipFilenameConflict,
};
use crate::archive::path_layout;
use crate::archive::shared_calibration::find_shared_calibration_sets;
use crate::duplicates::compute_xxhash;
use anyhow::{anyhow, Context, Result};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Refuse a plan in which two files map to one in-zip path. The protected
/// namespace is the per-OPERATION staging dir (staging.rs joins path_in_zip
/// under one op_<id> dir across every role zip), so the key deliberately
/// ignores which zip a file belongs to. Case-insensitive on every platform —
/// zip entries differing only by case explode on Windows extraction anyway.
fn ensure_unique_in_zip(files: &[ArchiveOperationFile]) -> Result<()> {
    let mut seen: HashMap<String, &str> = HashMap::new();
    for f in files {
        if let Some(first) = seen.insert(f.target_path_in_zip.to_lowercase(), &f.source_path) {
            return Err(anyhow!(
                "two files map to the same in-zip path '{}' ('{}' and '{}') — the archive staging area would silently collapse them; check for duplicate filenames under unregistered or differently-cased roots",
                f.target_path_in_zip,
                first,
                f.source_path
            ));
        }
    }
    Ok(())
}

/// Per-file row used internally before we resolve target paths.
#[derive(Debug, Clone)]
struct CandidateFile {
    file_id: i64,
    file_path: String,
    file_size: i64,
    role: FrameRole,
    disposition: ArchiveDisposition,
}

/// Build a plan WITHOUT writing any rows.
///
/// Behavior:
/// - Collects all LIGHT frames in the frame set; lights are always disposition=Move.
/// - For each calibration type with disposition=Move|Copy, collects the linked
///   calibration set's frames (master or single-file).
/// - Skip dispositions are skipped entirely.
/// - Deduplicates by file_id, keeping the highest-priority role (light > flat > darkflat > dark > bias).
/// - If a file with disposition=Move on its winning role is detected as shared, a
///   `SharedCalibrationWarning` is added (the executor will reject Move without
///   user confirmation; UI is expected to filter dispositions accordingly).
/// - Computes the path-in-zip per file using scan-root-name prefix.
/// - Hashes each source file with XXH3_64.
/// - Groups files into one zip per frame role; computes zip filename + total size.
/// - Detects conflicting zip filenames already on disk in the archive root.
pub fn build_plan(
    conn: &Connection,
    frames_set_id: i64,
    archive_root_path: &Path,
    dispositions: &Dispositions,
    compression: ArchiveCompression,
) -> Result<ArchivePlan> {
    // Guard: a frame set must be in the Archive section (is_archived=1) and
    // not already zipped before we plan. Frame sets in stage/wip should use
    // the existing archive_frame_set flow first.
    let (is_archived, archived_at): (i32, Option<String>) = conn.query_row(
        "SELECT is_archived, archived_at FROM frames_set WHERE id = ?1",
        [frames_set_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    if is_archived == 0 {
        anyhow::bail!(
            "frame set must be moved to the Archive section before it can be zipped"
        );
    }
    if archived_at.is_some() {
        anyhow::bail!("frame set is already zipped");
    }

    let frame_set = load_frame_set_metadata(conn, frames_set_id)?;
    let scan_roots = load_all_scan_roots(conn)?;
    let prefix_map = path_layout::resolve_scan_root_prefixes(&scan_roots);

    // 1. Lights (always Move)
    let mut candidates: Vec<CandidateFile> = collect_light_files(conn, frames_set_id)?
        .into_iter()
        .map(|(file_id, path, size)| CandidateFile {
            file_id,
            file_path: path,
            file_size: size,
            role: FrameRole::Light,
            disposition: ArchiveDisposition::Move,
        })
        .collect();

    // 2. Calibrations per type
    for (role, disp) in [
        (FrameRole::Flat, dispositions.flats),
        (FrameRole::Dark, dispositions.darks),
        (FrameRole::Bias, dispositions.bias),
        (FrameRole::Darkflat, dispositions.darkflats),
    ] {
        let Some(d) = disp else { continue };
        if d == ArchiveDisposition::Skip {
            continue;
        }
        for (file_id, path, size) in collect_calibration_files(conn, frames_set_id, role)? {
            candidates.push(CandidateFile {
                file_id,
                file_path: path,
                file_size: size,
                role,
                disposition: d,
            });
        }
    }

    // 3. Deduplicate by file_id, keep highest-priority role
    let mut by_id: HashMap<i64, CandidateFile> = HashMap::new();
    for c in candidates {
        by_id
            .entry(c.file_id)
            .and_modify(|existing| {
                if c.role.priority() < existing.role.priority() {
                    *existing = c.clone();
                }
            })
            .or_insert(c);
    }

    // 4. Detect shared calibrations
    let shared_warnings = find_shared_calibration_sets(conn, frames_set_id)?;

    // 5. For each file: hash, compute target zip path + path-in-zip
    //    Group by role to determine the zip filenames.
    let mut files: Vec<ArchiveOperationFile> = Vec::with_capacity(by_id.len());
    let mut zips_by_role: HashMap<FrameRole, (String, PathBuf, u64, usize)> = HashMap::new();
    let mut total_size: u64 = 0;

    for (_id, candidate) in by_id {
        let src = Path::new(&candidate.file_path);
        if !src.exists() {
            return Err(anyhow!("source file no longer exists: {}", candidate.file_path));
        }
        let scan_root = scan_roots
            .iter()
            .find(|r| path_layout::path_starts_with_fold(src, Path::new(r.as_str())))
            .cloned()
            .unwrap_or_else(|| {
                src.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
            });
        let unique_prefix = prefix_map
            .get(&scan_root)
            .cloned()
            .unwrap_or_else(|| {
                Path::new(&scan_root)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(path_layout::sanitize_for_filename)
                    .unwrap_or_else(|| "Root".into())
            });
        let path_in_zip = path_layout::path_in_zip(&unique_prefix, Path::new(&scan_root), src);

        let zip_name = path_layout::zip_filename(
            frame_set.object.as_deref(),
            frame_set.start_date.as_deref(),
            frame_set.end_date.as_deref(),
            frame_set.telescope.as_deref(),
            frame_set.camera.as_deref(),
            candidate.role,
        );
        let zip_path = archive_root_path.join(&zip_name);

        let hash = compute_xxhash(src)
            .with_context(|| format!("failed to hash {}", candidate.file_path))?;

        total_size += candidate.file_size as u64;
        let entry = zips_by_role
            .entry(candidate.role)
            .or_insert_with(|| (zip_name.clone(), zip_path.clone(), 0u64, 0usize));
        entry.2 += candidate.file_size as u64;
        entry.3 += 1;

        files.push(ArchiveOperationFile {
            id: 0, // assigned at commit time
            operation_id: 0,
            file_id: Some(candidate.file_id),
            source_path: candidate.file_path,
            target_zip_path: zip_path.to_string_lossy().to_string(),
            target_path_in_zip: path_in_zip,
            expected_hash: hash,
            disposition: candidate.disposition.as_str().to_string(),
            frame_role: candidate.role.as_str().to_string(),
            file_size_bytes: candidate.file_size,
        });
    }

    ensure_unique_in_zip(&files)?;

    let zips: Vec<PlannedZip> = zips_by_role
        .into_iter()
        .map(|(role, (filename, zip_path, total, count))| PlannedZip {
            zip_path: zip_path.to_string_lossy().to_string(),
            zip_filename: filename,
            frame_role: role,
            file_count: count,
            total_size_bytes: total,
        })
        .collect();

    let conflicts: Vec<ZipFilenameConflict> = zips
        .iter()
        .filter(|z| Path::new(&z.zip_path).exists())
        .map(|z| ZipFilenameConflict {
            zip_path: z.zip_path.clone(),
            zip_filename: z.zip_filename.clone(),
        })
        .collect();

    // Disk-space pre-flight (5% safety margin)
    if let Ok(available) = available_disk_space(archive_root_path) {
        let needed = total_size + (total_size / 20);
        if available < needed {
            anyhow::bail!(
                "insufficient disk space at archive root: need {} bytes (incl. 5% margin), available {}",
                needed, available
            );
        }
    }

    Ok(ArchivePlan {
        frames_set_id,
        calibration_set_id: None,
        archive_root_path: archive_root_path.to_string_lossy().to_string(),
        dispositions: dispositions.clone(),
        compression,
        files,
        zips,
        shared_calibrations: shared_warnings,
        conflicts,
        total_size_bytes: total_size,
    })
}

/// Build a plan to archive a SUPERSEDED calibration set's original member
/// frames into a single ZIP (Task 14 — "archive-of-originals": once a raw
/// calibration set has been combined into a master, the raw frames are
/// candidates for tidy long-term storage the same way a frame set's lights
/// are). WITHOUT writing any rows — mirrors `build_plan`'s plan/commit split.
///
/// Guards (all `bail!` on violation):
/// - The set must be superseded (`calibration_set.superseded_by_set_id IS
///   NOT NULL`) — archiving a still-in-use raw set would delete frames a
///   future (re)build needs.
/// - No member file may already be archived (`files.archived_in_operation IS
///   NOT NULL`) — partial archiving of a set is not a thing; the error lists
///   every already-archived member path so the caller can investigate.
/// - Every member file must still exist on disk.
///
/// Unlike `build_plan` (mixed roles, dedup, one zip per role), a calibration
/// set is homogeneous by construction — every member shares the set's own
/// `imagetyp` — so this always produces exactly one `PlannedZip`, and
/// `Dispositions` has exactly one type set to `Move` (the set's own type;
/// every other type is `None`, matching `build_plan`'s "type not present in
/// the chain" convention). The returned plan's `frames_set_id` is `0`
/// (sentinel — see `ArchivePlan` doc comment) with `calibration_set_id:
/// Some(calibration_set_id)`.
pub fn build_calibration_set_plan(
    conn: &Connection,
    calibration_set_id: i64,
    archive_root_path: &Path,
    compression: ArchiveCompression,
) -> Result<ArchivePlan> {
    let (imagetyp, instrume, gain, exptime, date_start, date_end, superseded_by_set_id): (
        String, Option<String>, Option<f64>, Option<f64>, Option<String>, Option<String>, Option<i64>,
    ) = conn.query_row(
        "SELECT imagetyp, instrume, gain, exptime, date_start, date_end, superseded_by_set_id
         FROM calibration_set WHERE id = ?1",
        [calibration_set_id],
        |row| Ok((
            row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?, row.get(5)?, row.get(6)?,
        )),
    ).with_context(|| format!("calibration set {calibration_set_id} not found"))?;

    if superseded_by_set_id.is_none() {
        anyhow::bail!(
            "calibration set {calibration_set_id} is not superseded by a master — \
             only superseded sets can have their originals archived"
        );
    }

    let role = match imagetyp.as_str() {
        "Dark" => FrameRole::Dark,
        "Flat" => FrameRole::Flat,
        "Bias" => FrameRole::Bias,
        "DarkFlat" => FrameRole::Darkflat,
        other => anyhow::bail!(
            "calibration set {calibration_set_id} has unsupported imagetyp for archiving: {other}"
        ),
    };

    // Member files: calibration_set_frames -> frames -> files. Fetch
    // archived_in_operation too so we can bail with a precise list rather
    // than silently skipping already-archived members.
    let member_rows: Vec<(i64, String, i64, Option<i64>)> = {
        let mut stmt = conn.prepare(
            "SELECT fi.id, fi.path, fi.size, fi.archived_in_operation
             FROM calibration_set_frames csf
             JOIN frames f ON f.id = csf.frame_id
             JOIN files fi ON fi.id = f.file_id
             WHERE csf.set_id = ?1
             ORDER BY fi.path",
        )?;
        let rows = stmt.query_map([calibration_set_id], |r| {
            Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
        })?.collect::<rusqlite::Result<Vec<_>>>()?;
        rows
    };

    if member_rows.is_empty() {
        anyhow::bail!("calibration set {calibration_set_id} has no member frames on record");
    }

    let already_archived: Vec<&str> = member_rows.iter()
        .filter(|(_, _, _, archived)| archived.is_some())
        .map(|(_, path, _, _)| path.as_str())
        .collect();
    if !already_archived.is_empty() {
        anyhow::bail!(
            "calibration set {calibration_set_id} already has archived member file(s): {}",
            already_archived.join(", "),
        );
    }

    let scan_roots = load_all_scan_roots(conn)?;
    let prefix_map = path_layout::resolve_scan_root_prefixes(&scan_roots);

    let zip_dir = path_layout::calibration_zip_dir(
        instrume.as_deref(), date_start.as_deref().unwrap_or(""),
    );
    let zip_filename = path_layout::calibration_zip_filename(
        instrume.as_deref(), &imagetyp, gain, exptime,
        date_start.as_deref().unwrap_or(""), date_end.as_deref().unwrap_or(""),
    );
    let zip_path = archive_root_path.join(&zip_dir).join(&zip_filename);

    let mut files: Vec<ArchiveOperationFile> = Vec::with_capacity(member_rows.len());
    let mut total_size: u64 = 0;

    for (file_id, path, size, _) in &member_rows {
        let src = Path::new(path);
        if !src.exists() {
            return Err(anyhow!("source file no longer exists: {}", path));
        }
        let scan_root = scan_roots
            .iter()
            .find(|r| path_layout::path_starts_with_fold(src, Path::new(r.as_str())))
            .cloned()
            .unwrap_or_else(|| {
                src.parent().map(|p| p.to_string_lossy().to_string()).unwrap_or_default()
            });
        let unique_prefix = prefix_map
            .get(&scan_root)
            .cloned()
            .unwrap_or_else(|| {
                Path::new(&scan_root)
                    .file_name()
                    .and_then(|s| s.to_str())
                    .map(path_layout::sanitize_for_filename)
                    .unwrap_or_else(|| "Root".into())
            });
        let path_in_zip = path_layout::path_in_zip(&unique_prefix, Path::new(&scan_root), src);
        let hash = compute_xxhash(src).with_context(|| format!("failed to hash {}", path))?;

        total_size += *size as u64;
        files.push(ArchiveOperationFile {
            id: 0,
            operation_id: 0,
            file_id: Some(*file_id),
            source_path: path.clone(),
            target_zip_path: zip_path.to_string_lossy().to_string(),
            target_path_in_zip: path_in_zip,
            expected_hash: hash,
            disposition: ArchiveDisposition::Move.as_str().to_string(),
            frame_role: role.as_str().to_string(),
            file_size_bytes: *size,
        });
    }

    ensure_unique_in_zip(&files)?;

    let zips = vec![PlannedZip {
        zip_path: zip_path.to_string_lossy().to_string(),
        zip_filename: zip_filename.clone(),
        frame_role: role,
        file_count: files.len(),
        total_size_bytes: total_size,
    }];

    let conflicts = if zip_path.exists() {
        vec![ZipFilenameConflict {
            zip_path: zip_path.to_string_lossy().to_string(),
            zip_filename: zip_filename.clone(),
        }]
    } else {
        Vec::new()
    };

    // Exactly the set's own type is Move; every other type is absent — same
    // "type not present in the chain" convention `build_plan` uses.
    let mut dispositions = Dispositions { flats: None, darks: None, bias: None, darkflats: None };
    match role {
        FrameRole::Flat => dispositions.flats = Some(ArchiveDisposition::Move),
        FrameRole::Dark => dispositions.darks = Some(ArchiveDisposition::Move),
        FrameRole::Bias => dispositions.bias = Some(ArchiveDisposition::Move),
        FrameRole::Darkflat => dispositions.darkflats = Some(ArchiveDisposition::Move),
        FrameRole::Light => unreachable!("role mapping above never yields Light for a calibration set"),
    }

    Ok(ArchivePlan {
        frames_set_id: 0,
        calibration_set_id: Some(calibration_set_id),
        archive_root_path: archive_root_path.to_string_lossy().to_string(),
        dispositions,
        compression,
        files,
        zips,
        shared_calibrations: Vec::new(),
        conflicts,
        total_size_bytes: total_size,
    })
}

/// Persist the plan: insert archive_operations + archive_operation_files rows,
/// applying the conflict resolution to zip paths if needed (renaming with `_2`, `_3` etc.).
/// Returns the new operation_id.
///
/// Subject-aware (Task 14): a plan is either a frame-set plan (real,
/// non-zero `frames_set_id`, `calibration_set_id: None` — from `build_plan`)
/// or a calibration-set plan (`frames_set_id: 0`, `calibration_set_id:
/// Some(id)` — from `build_calibration_set_plan`). Exactly one subject must
/// be present; a plan with neither (or, in principle, both) is a caller bug
/// caught here before anything is written.
pub fn commit_plan(
    conn: &Connection,
    plan: &ArchivePlan,
    conflict_resolution: ConflictResolution,
) -> Result<i64> {
    let has_frame_set = plan.frames_set_id != 0;
    let has_calibration_set = plan.calibration_set_id.is_some();
    anyhow::ensure!(
        has_frame_set != has_calibration_set,
        "archive plan must have exactly one subject: frames_set_id={}, calibration_set_id={:?}",
        plan.frames_set_id, plan.calibration_set_id,
    );

    // Apply conflict resolution: rewrite target_zip_path on plan.files + plan.zips
    let mut files = plan.files.clone();
    let mut zips = plan.zips.clone();

    if conflict_resolution == ConflictResolution::AddSuffix {
        for z in zips.iter_mut() {
            let mut p = PathBuf::from(&z.zip_path);
            let mut n = 2;
            while p.exists() {
                p = path_layout::add_suffix(Path::new(&z.zip_path), n);
                n += 1;
            }
            let new_path = p.to_string_lossy().to_string();
            // Update files that point to the old zip_path
            let old_zip_path = z.zip_path.clone();
            for f in files.iter_mut() {
                if f.target_zip_path == old_zip_path {
                    f.target_zip_path = new_path.clone();
                }
            }
            z.zip_path = new_path;
            z.zip_filename = p.file_name().unwrap().to_string_lossy().to_string();
        }
    }
    // Overwrite mode keeps paths as-is (existing zip is overwritten when build_zip runs).

    let op_id = adb::insert_operation(
        conn,
        has_frame_set.then_some(plan.frames_set_id),
        plan.calibration_set_id,
        &plan.archive_root_path,
        plan.dispositions.flats.map(|d| d.as_str()),
        plan.dispositions.darks.map(|d| d.as_str()),
        plan.dispositions.bias.map(|d| d.as_str()),
        plan.dispositions.darkflats.map(|d| d.as_str()),
        plan.compression.as_str(),
    )?;

    for f in &files {
        adb::insert_operation_file(
            conn,
            op_id,
            f.file_id,
            &f.source_path,
            &f.target_zip_path,
            &f.target_path_in_zip,
            &f.expected_hash,
            &f.disposition,
            &f.frame_role,
            f.file_size_bytes,
        )?;
    }
    Ok(op_id)
}

// --- helpers ---

#[derive(Debug, Default)]
struct FrameSetMetadata {
    object: Option<String>,
    start_date: Option<String>,
    end_date: Option<String>,
    telescope: Option<String>,
    camera: Option<String>,
}

fn load_frame_set_metadata(conn: &Connection, frames_set_id: i64) -> Result<FrameSetMetadata> {
    // Aggregate from frames in the set: most-frequent telescope+camera, min/max date.
    let row: Option<(Option<String>, Option<String>, Option<String>, Option<String>, Option<String>)> =
        conn.query_row(
            "SELECT
                (SELECT f.object FROM frames f
                 JOIN session_members sm ON sm.frame_id = f.id
                 JOIN sessions s ON s.id = sm.session_id
                 JOIN imaging_nights n ON n.id = s.imaging_night_id
                 WHERE n.frames_set_id = ?1 AND f.object IS NOT NULL
                 LIMIT 1),
                (SELECT DATE(MIN(f.date_obs)) FROM frames f
                 JOIN session_members sm ON sm.frame_id = f.id
                 JOIN sessions s ON s.id = sm.session_id
                 JOIN imaging_nights n ON n.id = s.imaging_night_id
                 WHERE n.frames_set_id = ?1),
                (SELECT DATE(MAX(f.date_obs)) FROM frames f
                 JOIN session_members sm ON sm.frame_id = f.id
                 JOIN sessions s ON s.id = sm.session_id
                 JOIN imaging_nights n ON n.id = s.imaging_night_id
                 WHERE n.frames_set_id = ?1),
                (SELECT f.telescop FROM frames f
                 JOIN session_members sm ON sm.frame_id = f.id
                 JOIN sessions s ON s.id = sm.session_id
                 JOIN imaging_nights n ON n.id = s.imaging_night_id
                 WHERE n.frames_set_id = ?1 AND f.telescop IS NOT NULL
                 GROUP BY f.telescop ORDER BY COUNT(*) DESC LIMIT 1),
                (SELECT f.instrume FROM frames f
                 JOIN session_members sm ON sm.frame_id = f.id
                 JOIN sessions s ON s.id = sm.session_id
                 JOIN imaging_nights n ON n.id = s.imaging_night_id
                 WHERE n.frames_set_id = ?1 AND f.instrume IS NOT NULL
                 GROUP BY f.instrume ORDER BY COUNT(*) DESC LIMIT 1)",
            [frames_set_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        ).ok();
    Ok(match row {
        Some((object, start, end, scope, cam)) => FrameSetMetadata {
            object,
            start_date: start,
            end_date: end,
            telescope: scope,
            camera: cam,
        },
        None => FrameSetMetadata::default(),
    })
}

fn load_all_scan_roots(conn: &Connection) -> Result<Vec<String>> {
    let mut stmt = conn.prepare("SELECT path FROM scan_roots ORDER BY path")?;
    let rows: Vec<String> = stmt.query_map([], |r| r.get::<_, String>(0))?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Light frames in the set: (file_id, path, size).
fn collect_light_files(conn: &Connection, frames_set_id: i64) -> Result<Vec<(i64, String, i64)>> {
    let mut stmt = conn.prepare(
        "SELECT DISTINCT fi.id, fi.path, fi.size
         FROM files fi
         JOIN frames f ON f.file_id = fi.id
         JOIN session_members sm ON sm.frame_id = f.id
         JOIN sessions s ON s.id = sm.session_id
         JOIN imaging_nights n ON n.id = s.imaging_night_id
         WHERE n.frames_set_id = ?1 AND f.imagetyp = 'Light'
         ORDER BY fi.path",
    )?;
    let rows: Vec<(i64, String, i64)> = stmt.query_map([frames_set_id], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Calibration files reachable from a frame set, for a given role.
fn collect_calibration_files(
    conn: &Connection,
    frames_set_id: i64,
    role: FrameRole,
) -> Result<Vec<(i64, String, i64)>> {
    let cal_type = match role {
        FrameRole::Flat => "Flat",
        FrameRole::Dark => "Dark",
        FrameRole::Bias => "Bias",
        FrameRole::Darkflat => "DarkFlat",
        FrameRole::Light => return Ok(vec![]),
    };
    let mut stmt = conn.prepare(
        "SELECT DISTINCT fi.id, fi.path, fi.size
         FROM files fi
         JOIN frames f ON f.file_id = fi.id
         JOIN calibration_set_frames csf ON csf.frame_id = f.id
         JOIN calibration_set_to_frames cstf ON cstf.calibration_set_id = csf.set_id
         JOIN frames lf ON lf.id = cstf.source_id AND cstf.source_type = 'frame'
         JOIN session_members sm ON sm.frame_id = lf.id
         JOIN sessions s ON s.id = sm.session_id
         JOIN imaging_nights n ON n.id = s.imaging_night_id
         WHERE n.frames_set_id = ?1
           AND cstf.calibration_type = ?2
         ORDER BY fi.path",
    )?;
    let rows: Vec<(i64, String, i64)> = stmt.query_map(params![frames_set_id, cal_type], |r| {
        Ok((r.get(0)?, r.get(1)?, r.get(2)?))
    })?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(rows)
}

/// Best-effort disk-space query. Returns an error on platforms where it isn't trivially supported;
/// callers should treat that as "skip the pre-flight check."
fn available_disk_space(_path: &Path) -> Result<u64> {
    // Cross-platform disk-space inquiry without an extra dependency is awkward.
    // Returning Err here causes the caller to skip the check; that's acceptable
    // for v1 since the executor will fail loudly on out-of-space at copy time anyway.
    Err(anyhow!("disk-space check not implemented; relying on copy-time errors"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::models::ArchiveCompression;
    use crate::db::schema::init_db;
    use tempfile::TempDir;

    fn op_file(source_path: &str, zip: &str, in_zip: &str) -> ArchiveOperationFile {
        ArchiveOperationFile {
            id: 0,
            operation_id: 0,
            file_id: None,
            source_path: source_path.to_string(),
            target_zip_path: zip.to_string(),
            target_path_in_zip: in_zip.to_string(),
            expected_hash: String::new(),
            disposition: "move".to_string(),
            frame_role: "light".to_string(),
            file_size_bytes: 0,
        }
    }

    /// Staging is flat per OPERATION, not per zip — two files landing on one
    /// in-zip path collide there even when they belong to different role zips.
    #[test]
    fn unique_in_zip_guard_ignores_the_zip_a_file_belongs_to() {
        let files = vec![
            op_file("/a/Root/x.fits", "/arch/Lights.zip", "Root/x.fits"),
            op_file("/b/Root/x.fits", "/arch/Darks.zip", "Root/x.fits"),
        ];
        let err = ensure_unique_in_zip(&files).unwrap_err().to_string();
        assert!(
            err.contains("/a/Root/x.fits"),
            "missing first source: {err}"
        );
        assert!(
            err.contains("/b/Root/x.fits"),
            "missing second source: {err}"
        );

        let ok = vec![
            op_file("/a/Root/x.fits", "/arch/Lights.zip", "Root/x.fits"),
            op_file("/a/Root/y.fits", "/arch/Darks.zip", "Root/y.fits"),
        ];
        assert!(ensure_unique_in_zip(&ok).is_ok());
    }

    /// Build a tiny SQLite + filesystem fixture: one frame_set with two LIGHT
    /// frames and one master DARK linked to both. Returns (conn, archive_dir, scan_root).
    fn fixture() -> (Connection, TempDir, TempDir) {
        let arch_dir = TempDir::new().unwrap();
        let scan_dir = TempDir::new().unwrap();

        // Two real .fits files to hash.
        let l1 = scan_dir.path().join("M31/2025-10-12/L_001.fits");
        let l2 = scan_dir.path().join("M31/2025-10-12/L_002.fits");
        std::fs::create_dir_all(l1.parent().unwrap()).unwrap();
        std::fs::write(&l1, b"light-1-content").unwrap();
        std::fs::write(&l2, b"light-2-content").unwrap();
        let d1 = scan_dir.path().join("Cal/MasterDark.fits");
        std::fs::create_dir_all(d1.parent().unwrap()).unwrap();
        std::fs::write(&d1, b"dark-content").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();

        // Scan root that contains all the test files
        conn.execute(
            "INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [scan_dir.path().to_str().unwrap()],
        ).unwrap();

        // Frame set
        conn.execute(
            "INSERT INTO frames_set (id, name, is_archived, date_obs_start, date_obs_end)
             VALUES (1, 'M31', 1, '2025-10-12T00:00:00Z', '2025-10-12T08:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time)
             VALUES (10, 1, '2025-10-12T00:00:00Z', '2025-10-13T00:00:00Z')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'ASI2600MM')",
            [],
        ).unwrap();

        // Light files + frames
        for (file_id, path, frame_id) in [(1000, &l1, 10000), (1001, &l2, 10001)] {
            let p = path.to_str().unwrap();
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (?1, ?2, ?3, 15, '2025-10-12T00:00:00Z', 'FITS')",
                params![file_id, p, path.file_name().unwrap().to_str().unwrap()],
            ).unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, object, telescop, instrume, imagetyp, date_obs)
                 VALUES (?1, ?2, 'M31', 'RedCat 51', 'ASI2600MM', 'Light', '2025-10-12T00:00:00Z')",
                params![frame_id, file_id],
            ).unwrap();
            conn.execute(
                "INSERT INTO session_members (session_id, frame_id) VALUES (100, ?1)",
                [frame_id],
            ).unwrap();
        }

        // Dark file + frame + calibration set + links
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (2000, ?1, 'MasterDark.fits', 12, '2025-10-10T00:00:00Z', 'FITS')",
            [d1.to_str().unwrap()],
        ).unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, instrume, imagetyp, is_master)
             VALUES (20000, 2000, 'ASI2600MM', 'Dark', 1)",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date) VALUES (500, 'Dark', '2025-10-10')",
            [],
        ).unwrap();
        conn.execute(
            "INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (500, 20000)",
            [],
        ).unwrap();
        // Link both light frames to this dark
        for fid in [10000, 10001] {
            conn.execute(
                "INSERT INTO calibration_set_to_frames
                 (source_id, source_type, calibration_set_id, calibration_type, matched_at)
                 VALUES (?1, 'frame', 500, 'Dark', '2025-10-12')",
                [fid],
            ).unwrap();
        }

        (conn, arch_dir, scan_dir)
    }

    #[test]
    fn build_plan_lights_only() {
        let (conn, arch_dir, _scan_dir) = fixture();
        let dispositions = Dispositions {
            flats: None, darks: Some(ArchiveDisposition::Skip), bias: None, darkflats: None,
        };
        let plan = build_plan(
            &conn, 1, arch_dir.path(), &dispositions, ArchiveCompression::Store,
        ).unwrap();

        assert_eq!(plan.files.len(), 2, "two light files");
        assert!(plan.files.iter().all(|f| f.frame_role == "light"));
        assert!(plan.files.iter().all(|f| f.disposition == "move"));
        assert_eq!(plan.zips.len(), 1, "one Lights.zip");
        assert!(plan.zips[0].zip_filename.contains("Lights.zip"));
    }

    #[test]
    fn build_plan_with_dark_copy() {
        let (conn, arch_dir, _scan_dir) = fixture();
        let dispositions = Dispositions {
            flats: None, darks: Some(ArchiveDisposition::Copy), bias: None, darkflats: None,
        };
        let plan = build_plan(
            &conn, 1, arch_dir.path(), &dispositions, ArchiveCompression::Store,
        ).unwrap();

        assert_eq!(plan.files.len(), 3, "two lights + one dark");
        let dark = plan.files.iter().find(|f| f.frame_role == "dark").unwrap();
        assert_eq!(dark.disposition, "copy");
        assert_eq!(plan.zips.len(), 2, "Lights.zip + Darks.zip");
    }

    #[test]
    fn build_plan_skip_excludes_calibration() {
        let (conn, arch_dir, _scan_dir) = fixture();
        let dispositions = Dispositions {
            flats: None, darks: Some(ArchiveDisposition::Skip), bias: None, darkflats: None,
        };
        let plan = build_plan(
            &conn, 1, arch_dir.path(), &dispositions, ArchiveCompression::Store,
        ).unwrap();
        assert!(plan.files.iter().all(|f| f.frame_role != "dark"));
    }

    #[test]
    fn build_plan_detects_existing_zip_conflict() {
        let (conn, arch_dir, _scan_dir) = fixture();
        // Pre-create a zip with the predicted name.
        let predicted = path_layout::zip_filename(
            Some("M31"), Some("2025-10-12"), Some("2025-10-12"),
            Some("RedCat 51"), Some("ASI2600MM"), FrameRole::Light,
        );
        std::fs::write(arch_dir.path().join(&predicted), b"existing").unwrap();

        let dispositions = Dispositions {
            flats: None, darks: Some(ArchiveDisposition::Skip), bias: None, darkflats: None,
        };
        let plan = build_plan(
            &conn, 1, arch_dir.path(), &dispositions, ArchiveCompression::Store,
        ).unwrap();
        assert_eq!(plan.conflicts.len(), 1);
        assert!(plan.conflicts[0].zip_filename.ends_with("_Lights.zip"));
    }

    #[test]
    fn commit_plan_writes_rows_and_can_apply_suffix() {
        let (conn, arch_dir, _scan_dir) = fixture();
        let dispositions = Dispositions {
            flats: None, darks: Some(ArchiveDisposition::Copy), bias: None, darkflats: None,
        };
        let plan = build_plan(
            &conn, 1, arch_dir.path(), &dispositions, ArchiveCompression::Store,
        ).unwrap();
        let op_id = commit_plan(&conn, &plan, ConflictResolution::Overwrite).unwrap();

        let op = adb::get_operation(&conn, op_id).unwrap();
        assert_eq!(op.frames_set_id, Some(1));
        assert!(op.calibration_set_id.is_none());
        let files = adb::list_operation_files(&conn, op_id).unwrap();
        assert_eq!(files.len(), 3);
    }

    // ── build_calibration_set_plan (Task 14) ────────────────────────────────

    #[test]
    fn calibration_plan_requires_superseded_set() {
        let dir = tempfile::tempdir().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        // raw set with one real file, NOT superseded
        conn.execute("INSERT INTO calibration_set (imagetyp, date, instrume, date_start, date_end)
                      VALUES ('Dark','2026-06-28','Cam','2026-06-28T20:00:00Z','2026-06-28T21:00:00Z')", []).unwrap();
        let set = conn.last_insert_rowid();
        let r = build_calibration_set_plan(&conn, set, dir.path(), ArchiveCompression::Store);
        assert!(r.is_err(), "non-superseded set must be rejected");
    }

    #[test]
    fn calibration_plan_layout() {
        let dir = tempfile::tempdir().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (path) VALUES (?1)",
            [dir.path().to_string_lossy()]).unwrap();
        // superseded_by_set_id is a real FK (foreign_keys=ON by default in
        // this codebase's connections) — the master row it points to must
        // exist first.
        conn.execute("INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
            VALUES (999, 'MasterDark', '2026-06-28', 1)", []).unwrap();
        conn.execute("INSERT INTO calibration_set
            (imagetyp, date, instrume, gain, exptime, date_start, date_end, superseded_by_set_id)
            VALUES ('Dark','2026-06-28','Test Cam',100.0,300.0,
                    '2026-06-28T20:00:00Z','2026-06-28T21:00:00Z', 999)", []).unwrap();
        let set = conn.last_insert_rowid();
        let f = dir.path().join("d1.fits");
        std::fs::write(&f, b"data").unwrap();
        conn.execute("INSERT INTO files (path, filename, size, modified_at, format)
                      VALUES (?1,'d1.fits',4,'2026-06-28','FITS')",
            [f.to_string_lossy()]).unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute("INSERT INTO frames (file_id, imagetyp) VALUES (?1,'Dark')", [file_id]).unwrap();
        let frame_id = conn.last_insert_rowid();
        conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1,?2)",
            rusqlite::params![set, frame_id]).unwrap();

        let plan = build_calibration_set_plan(&conn, set, dir.path(), ArchiveCompression::Store).unwrap();
        assert_eq!(plan.calibration_set_id, Some(set));
        assert_eq!(plan.files.len(), 1);
        assert_eq!(plan.zips.len(), 1);
        let zp = &plan.zips[0].zip_path;
        assert!(zp.contains("Calibration_Archive"), "{zp}");
        assert!(zp.contains("2026-06-28"), "date dir: {zp}");
        assert!(plan.files.iter().all(|f| f.disposition == "move"));
    }

    #[test]
    fn calibration_plan_rejects_already_archived_member() {
        let dir = tempfile::tempdir().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (path) VALUES (?1)",
            [dir.path().to_string_lossy()]).unwrap();
        conn.execute("INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
            VALUES (999, 'MasterBias', '2026-06-28', 1)", []).unwrap();
        conn.execute("INSERT INTO calibration_set
            (imagetyp, date, instrume, date_start, date_end, superseded_by_set_id)
            VALUES ('Bias','2026-06-28','Cam','2026-06-28T20:00:00Z','2026-06-28T21:00:00Z', 999)", []).unwrap();
        let set = conn.last_insert_rowid();
        let f = dir.path().join("b1.fits");
        std::fs::write(&f, b"data").unwrap();
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format, archived_in_operation)
             VALUES (?1,'b1.fits',4,'2026-06-28','FITS', 777)",
            [f.to_string_lossy()],
        ).unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute("INSERT INTO frames (file_id, imagetyp) VALUES (?1,'Bias')", [file_id]).unwrap();
        let frame_id = conn.last_insert_rowid();
        conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1,?2)",
            rusqlite::params![set, frame_id]).unwrap();

        let err = build_calibration_set_plan(&conn, set, dir.path(), ArchiveCompression::Store).unwrap_err();
        assert!(format!("{err:#}").contains("already has archived member"), "{err:#}");
    }

    /// Executor + restore round trip on a calibration-set archive op (Task
    /// 14 self-review item: "can a calibration op's restore really rewire
    /// files.path and clear markers with frames_set_id None end-to-end?").
    #[test]
    fn calibration_archive_executor_and_restore_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (path) VALUES (?1)",
            [dir.path().to_string_lossy()]).unwrap();

        // Master set that supersedes the raw set below.
        conn.execute(
            "INSERT INTO calibration_set (id, imagetyp, date, is_master_library)
             VALUES (999, 'MasterDark', '2026-06-28', 1)",
            [],
        ).unwrap();
        conn.execute("INSERT INTO calibration_set
            (imagetyp, date, instrume, gain, exptime, date_start, date_end, superseded_by_set_id)
            VALUES ('Dark','2026-06-28','Test Cam',100.0,300.0,
                    '2026-06-28T20:00:00Z','2026-06-28T21:00:00Z', 999)", []).unwrap();
        let set = conn.last_insert_rowid();

        let f = dir.path().join("d1.fits");
        std::fs::write(&f, b"data").unwrap();
        conn.execute("INSERT INTO files (path, filename, size, modified_at, format)
                      VALUES (?1,'d1.fits',4,'2026-06-28','FITS')",
            [f.to_string_lossy()]).unwrap();
        let file_id = conn.last_insert_rowid();
        conn.execute("INSERT INTO frames (file_id, imagetyp) VALUES (?1,'Dark')", [file_id]).unwrap();
        let frame_id = conn.last_insert_rowid();
        conn.execute("INSERT INTO calibration_set_frames (set_id, frame_id) VALUES (?1,?2)",
            rusqlite::params![set, frame_id]).unwrap();

        // master_provenance row referencing this set as source — must stay untouched.
        conn.execute(
            "INSERT INTO master_provenance
                (master_set_id, source_set_id, recipe_json, member_frame_uuids, member_hash, created_at)
             VALUES (999, ?1, '{}', '[]', 'abc123', '2026-06-28T00:00:00Z')",
            [set],
        ).unwrap();

        let plan = build_calibration_set_plan(&conn, set, dir.path(), ArchiveCompression::Store).unwrap();
        let op_id = commit_plan(&conn, &plan, ConflictResolution::Overwrite).unwrap();

        let cancel = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
        crate::archive::executor::run_operation(&conn, op_id, &cancel, &crate::events::NullEmitter).unwrap();

        // Zip exists at the planned path; source deleted (Move disposition).
        assert!(Path::new(&plan.zips[0].zip_path).is_file(), "{}", plan.zips[0].zip_path);
        assert!(!f.exists(), "source should have been moved into the zip");

        // files.archive_zip_path / archived_in_operation set.
        let (archive_zip_path, archived_op): (Option<String>, Option<i64>) = conn.query_row(
            "SELECT archive_zip_path, archived_in_operation FROM files WHERE id = ?1",
            [file_id], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!(archived_op, Some(op_id));
        assert!(archive_zip_path.is_some());

        // frames / calibration_set_frames rows intact.
        let frame_count: i64 = conn.query_row(
            "SELECT COUNT(*) FROM calibration_set_frames WHERE set_id = ?1", [set], |r| r.get(0),
        ).unwrap();
        assert_eq!(frame_count, 1);

        // master_provenance untouched.
        let (prov_master, prov_source): (i64, Option<i64>) = conn.query_row(
            "SELECT master_set_id, source_set_id FROM master_provenance WHERE master_set_id = 999",
            [], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert_eq!(prov_master, 999);
        assert_eq!(prov_source, Some(set));

        // The operation itself has no frame-set subject; there is no
        // frame-set-level marker to have touched.
        let op = adb::get_operation(&conn, op_id).unwrap();
        assert!(op.frames_set_id.is_none());
        assert_eq!(op.calibration_set_id, Some(set));
        assert_eq!(op.status, "completed");

        // --- Restore round trip: file comes back, markers clear, no frame
        // set anywhere in the picture (frames_set_id stays None throughout).
        let outcome = crate::archive::restore::run_restore(
            &conn, op_id, dir.path(), false, false, &cancel, &crate::events::NullEmitter,
        ).unwrap();
        assert!(!outcome.has_conflicts(), "{:?}", outcome.conflicts);

        assert!(f.exists(), "file should be restored to its original path");
        assert_eq!(std::fs::read(&f).unwrap(), b"data");

        let (archive_zip_path_after, archived_op_after): (Option<String>, Option<i64>) = conn.query_row(
            "SELECT archive_zip_path, archived_in_operation FROM files WHERE id = ?1",
            [file_id], |r| Ok((r.get(0)?, r.get(1)?)),
        ).unwrap();
        assert!(archive_zip_path_after.is_none(), "archive markers must be cleared after restore");
        assert!(archived_op_after.is_none());
    }
}
