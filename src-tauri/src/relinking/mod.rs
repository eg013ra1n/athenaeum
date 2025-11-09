/// Module for relinking files when directories move
///
/// This module provides functionality to update file paths in the database when
/// monitored directories change location. Files are matched by their FITS header
/// fingerprints to ensure accurate relinking even when filenames or directory
/// structures change.

use crate::fingerprint::compute_header_fingerprint;
use crate::fits_parser::{extract_fits_header, extract_xisf_header};
use crate::models::{FileFormat, RelinkResult};
use anyhow::{Context, Result};
use rusqlite::{params, Connection};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

/// Relink files from an old scan root path to a new location
///
/// This function:
/// 1. Builds a map of fingerprints -> (file_id, old_path) for files under old root
/// 2. Scans the new directory to find FITS/XISF files
/// 3. Matches files by their header fingerprints
/// 4. Updates file paths in the database for matched files
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `old_root_path` - The original scan root path (files with this prefix will be relinked)
/// * `new_root_path` - The new location where files have been moved
///
/// # Returns
///
/// `RelinkResult` with statistics about matched, new, and orphaned files
pub fn relink_files(
    conn: &Connection,
    old_root_path: &str,
    new_root_path: &str,
) -> Result<RelinkResult> {
    println!("Starting relink: '{}' -> '{}'", old_root_path, new_root_path);

    // Check if paths are the same - run verification mode instead
    if old_root_path == new_root_path {
        println!("Old and new paths are identical - performing file verification instead");
        return verify_files_at_location(conn, old_root_path);
    }

    // Step 1: Build fingerprint map for files under old root
    let mut fingerprint_map: HashMap<String, (i64, String)> = HashMap::new();

    let mut stmt = conn.prepare(
        "SELECT f.id, f.path, f.filename, fh.header_fingerprint
         FROM files f
         INNER JOIN fits_header fh ON f.id = fh.file_id
         WHERE f.path LIKE ?1 AND fh.header_fingerprint IS NOT NULL",
    )?;

    let old_root_prefix = format!("{}%", old_root_path);
    let rows = stmt.query_map(params![old_root_prefix], |row| {
        Ok((
            row.get::<_, i64>(0)?,      // id
            row.get::<_, String>(1)?,   // path
            row.get::<_, String>(2)?,   // filename
            row.get::<_, String>(3)?,   // header_fingerprint
        ))
    })?;

    for row in rows {
        let (file_id, path, _filename, fingerprint) = row?;
        fingerprint_map.insert(fingerprint, (file_id, path));
    }

    println!(
        "Found {} files with fingerprints under old root",
        fingerprint_map.len()
    );

    // Step 2: Scan new directory for FITS/XISF files
    let new_root = Path::new(new_root_path);
    if !new_root.exists() {
        anyhow::bail!("New root path does not exist: {}", new_root_path);
    }

    let mut files_matched = 0;
    let mut files_new = 0;
    let mut matched_file_ids = std::collections::HashSet::new();

    for entry in WalkDir::new(new_root)
        .follow_links(false)
        .into_iter()
        .filter_map(|e| e.ok())
    {
        let path = entry.path();
        if !path.is_file() {
            continue;
        }

        let extension = path
            .extension()
            .and_then(|s| s.to_str())
            .map(|s| s.to_lowercase());

        let format = match extension.as_deref() {
            Some("fits") | Some("fit") | Some("fts") => FileFormat::FITS,
            Some("xisf") => FileFormat::XISF,
            _ => continue,
        };

        // Extract header and compute fingerprint
        let header = match format {
            FileFormat::FITS => match extract_fits_header(path) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("Warning: Failed to read FITS header from {:?}: {}", path, e);
                    continue;
                }
            },
            FileFormat::XISF => match extract_xisf_header(path) {
                Ok(h) => h,
                Err(e) => {
                    eprintln!("Warning: Failed to read XISF header from {:?}: {}", path, e);
                    continue;
                }
            },
        };

        let fingerprint = compute_header_fingerprint(&header);

        // Try to match with old files
        if let Some((file_id, old_path)) = fingerprint_map.get(&fingerprint) {
            let new_path_str = path.to_string_lossy().to_string();

            // Update the file path in database
            conn.execute(
                "UPDATE files SET path = ?1 WHERE id = ?2",
                params![new_path_str, file_id],
            )
            .context("Failed to update file path")?;

            println!(
                "Matched: {} (id={}) -> {}",
                old_path, file_id, new_path_str
            );
            files_matched += 1;
            matched_file_ids.insert(*file_id);
        } else {
            // File not in database
            files_new += 1;
        }
    }

    // Step 3: Identify orphaned files (in DB but not found in new location)
    let orphaned_file_ids: Vec<i64> = fingerprint_map
        .values()
        .filter(|(file_id, _)| !matched_file_ids.contains(file_id))
        .map(|(file_id, _)| *file_id)
        .collect();

    let files_orphaned = orphaned_file_ids.len();

    println!(
        "Relinking complete: {} matched, {} new, {} orphaned",
        files_matched, files_new, files_orphaned
    );

    Ok(RelinkResult {
        files_matched,
        files_new,
        files_orphaned,
        orphaned_file_ids,
    })
}

/// Verify files at a location (used when relinking to same directory)
///
/// This checks which files in the database still exist on disk at the current location.
/// Returns a result showing how many files are still present vs. orphaned.
fn verify_files_at_location(
    conn: &Connection,
    root_path: &str,
) -> Result<RelinkResult> {
    let mut fingerprint_map: HashMap<String, (i64, String)> = HashMap::new();

    let mut stmt = conn.prepare(
        "SELECT f.id, f.path, f.filename, fh.header_fingerprint
         FROM files f
         INNER JOIN fits_header fh ON f.id = fh.file_id
         WHERE f.path LIKE ?1 AND fh.header_fingerprint IS NOT NULL",
    )?;

    let root_prefix = format!("{}%", root_path);
    let rows = stmt.query_map(params![root_prefix], |row| {
        Ok((
            row.get::<_, i64>(0)?,      // id
            row.get::<_, String>(1)?,   // path
            row.get::<_, String>(2)?,   // filename
            row.get::<_, String>(3)?,   // header_fingerprint
        ))
    })?;

    for row in rows {
        let (file_id, path, _filename, fingerprint) = row?;
        fingerprint_map.insert(fingerprint, (file_id, path));
    }

    let total_files = fingerprint_map.len();
    println!("Found {} files with fingerprints at location", total_files);

    let mut files_found = 0;
    let mut missing_file_ids = Vec::new();

    // Check which files still exist on disk
    for (_fingerprint, (file_id, path)) in fingerprint_map.iter() {
        if std::path::Path::new(path).exists() {
            files_found += 1;
        } else {
            missing_file_ids.push(*file_id);
        }
    }

    let files_missing = missing_file_ids.len();

    println!(
        "Verification complete: {} files still exist, {} missing",
        files_found, files_missing
    );

    Ok(RelinkResult {
        files_matched: files_found,
        files_new: 0, // No new files when verifying same location
        files_orphaned: files_missing,
        orphaned_file_ids: missing_file_ids,
    })
}

/// Get details about orphaned files for user review
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `file_ids` - List of orphaned file IDs
///
/// # Returns
///
/// Vector of `OrphanedFile` structs with file details
pub fn get_orphaned_file_details(
    conn: &Connection,
    file_ids: &[i64],
) -> Result<Vec<crate::models::OrphanedFile>> {
    if file_ids.is_empty() {
        return Ok(Vec::new());
    }

    let placeholders = file_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let query = format!(
        "SELECT f.id, f.path, f.filename, f.size, f.modified_at,
                EXISTS(SELECT 1 FROM frames fr WHERE fr.file_id = f.id) as has_frame,
                (SELECT fr.object FROM frames fr WHERE fr.file_id = f.id LIMIT 1) as object,
                (SELECT fr.date_obs FROM frames fr WHERE fr.file_id = f.id LIMIT 1) as date_obs
         FROM files f
         WHERE f.id IN ({})",
        placeholders
    );

    let mut stmt = conn.prepare(&query)?;
    let params: Vec<&dyn rusqlite::ToSql> = file_ids
        .iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();

    let rows = stmt.query_map(params.as_slice(), |row| {
        Ok(crate::models::OrphanedFile {
            id: row.get(0)?,
            path: row.get(1)?,
            filename: row.get(2)?,
            size: row.get(3)?,
            modified_at: row.get(4)?,
            has_frame: row.get::<_, i64>(5)? != 0,
            object: row.get(6).ok(),
            date_obs: row.get(7).ok(),
        })
    })?;

    let mut orphaned_files = Vec::new();
    for row in rows {
        orphaned_files.push(row?);
    }

    Ok(orphaned_files)
}

/// Delete orphaned files from the database
///
/// # Arguments
///
/// * `conn` - Database connection
/// * `file_ids` - List of file IDs to delete
///
/// # Returns
///
/// Number of files deleted
pub fn delete_orphaned_files(conn: &Connection, file_ids: &[i64]) -> Result<usize> {
    if file_ids.is_empty() {
        return Ok(0);
    }

    let placeholders = file_ids.iter().map(|_| "?").collect::<Vec<_>>().join(",");
    let query = format!("DELETE FROM files WHERE id IN ({})", placeholders);

    let params: Vec<&dyn rusqlite::ToSql> = file_ids
        .iter()
        .map(|id| id as &dyn rusqlite::ToSql)
        .collect();

    let deleted = conn.execute(&query, params.as_slice())?;

    Ok(deleted)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_orphaned_file_details_empty() {
        let conn = Connection::open_in_memory().unwrap();
        let result = get_orphaned_file_details(&conn, &[]).unwrap();
        assert_eq!(result.len(), 0);
    }

    #[test]
    fn test_delete_orphaned_files_empty() {
        let conn = Connection::open_in_memory().unwrap();
        let result = delete_orphaned_files(&conn, &[]).unwrap();
        assert_eq!(result, 0);
    }
}
