// Black Hole (soft delete) operations

use crate::models::{BlackHoleEntry, FolderSimilarity};
use anyhow::Result;
use chrono::Utc;
use rusqlite::{params, Connection};
use std::collections::HashMap;

/// Add a file to the black hole (soft delete)
pub fn add_to_black_hole(
    conn: &Connection,
    file_id: i64,
    from_where: &str,
    original_path: &str,
) -> Result<i64> {
    let now = Utc::now().to_rfc3339();

    conn.execute(
        "INSERT INTO black_hole (file_id, from_where, moved_at, original_path)
         VALUES (?1, ?2, ?3, ?4)",
        params![file_id, from_where, now, original_path],
    )?;

    Ok(conn.last_insert_rowid())
}

/// Get all files in the black hole, optionally filtered by source
pub fn get_black_hole_files(
    conn: &Connection,
    filter_by_source: Option<String>,
) -> Result<Vec<BlackHoleEntry>> {
    let query = if let Some(source) = filter_by_source {
        format!(
            "SELECT bh.id, bh.file_id, f.filename, bh.original_path, bh.from_where, bh.moved_at, f.size
             FROM black_hole bh
             JOIN files f ON bh.file_id = f.id
             WHERE bh.from_where = '{}'
             ORDER BY bh.moved_at DESC",
            source
        )
    } else {
        "SELECT bh.id, bh.file_id, f.filename, bh.original_path, bh.from_where, bh.moved_at, f.size
         FROM black_hole bh
         JOIN files f ON bh.file_id = f.id
         ORDER BY bh.moved_at DESC"
            .to_string()
    };

    let mut stmt = conn.prepare(&query)?;

    let entries = stmt.query_map([], |row| {
        let moved_at_str: String = row.get(5)?;
        let moved_at = chrono::DateTime::parse_from_rfc3339(&moved_at_str)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or_else(|_| Utc::now());

        Ok(BlackHoleEntry {
            id: Some(row.get(0)?),
            file_id: row.get(1)?,
            filename: row.get(2)?,
            original_path: row.get(3)?,
            from_where: row.get(4)?,
            moved_at,
            file_size: row.get(6)?,
        })
    })?;

    entries.collect::<Result<Vec<_>, _>>().map_err(Into::into)
}

/// Remove a file from the black hole (restore)
pub fn remove_from_black_hole(conn: &Connection, file_id: i64) -> Result<()> {
    conn.execute(
        "DELETE FROM black_hole WHERE file_id = ?1",
        params![file_id],
    )?;
    Ok(())
}

/// Permanently delete a file from disk and database (send to void)
pub fn send_to_void(conn: &Connection, file_id: i64) -> Result<()> {
    // Get file path before deletion
    let path: String = conn.query_row(
        "SELECT path FROM files WHERE id = ?1",
        params![file_id],
        |row| row.get(0),
    )?;

    // Delete physical file
    if std::path::Path::new(&path).exists() {
        std::fs::remove_file(&path)?;
    }

    // Delete from black_hole table (CASCADE will handle this, but explicit is clearer)
    conn.execute(
        "DELETE FROM black_hole WHERE file_id = ?1",
        params![file_id],
    )?;

    // Delete from files table (will cascade to frames, frame_tags, etc.)
    conn.execute(
        "DELETE FROM files WHERE id = ?1",
        params![file_id],
    )?;

    Ok(())
}

/// Permanently delete all files in black hole (send all to void)
pub fn send_all_to_void(conn: &Connection) -> Result<usize> {
    // Get all file IDs in black hole
    let mut stmt = conn.prepare("SELECT file_id FROM black_hole")?;
    let file_ids: Vec<i64> = stmt
        .query_map([], |row| row.get(0))?
        .collect::<Result<Vec<_>, _>>()?;

    let count = file_ids.len();

    // Delete each file
    for file_id in file_ids {
        // Ignore errors (file might already be gone)
        let _ = send_to_void(conn, file_id);
    }

    Ok(count)
}

/// Find folders with high similarity (many duplicate files)
pub fn find_duplicate_folders(
    conn: &Connection,
    similarity_threshold: f64,
) -> Result<Vec<FolderSimilarity>> {
    // Get all unique folder paths from files
    let mut folder_files: HashMap<String, Vec<(i64, String, i64)>> = HashMap::new();

    let mut stmt = conn.prepare(
        "SELECT id, path, metadata_hash, size
         FROM files
         WHERE metadata_hash IS NOT NULL
         AND NOT EXISTS (SELECT 1 FROM black_hole bh WHERE bh.file_id = files.id)"
    )?;

    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
        ))
    })?;

    for row in rows {
        let (id, path, hash, size) = row?;
        if let Some(parent) = std::path::Path::new(&path).parent() {
            let folder = parent.to_string_lossy().to_string();
            folder_files
                .entry(folder)
                .or_insert_with(Vec::new)
                .push((id, hash, size));
        }
    }

    let mut similarities = Vec::new();
    let folders: Vec<_> = folder_files.keys().cloned().collect();

    // Compare all folder pairs
    for i in 0..folders.len() {
        for j in (i + 1)..folders.len() {
            let folder_a = &folders[i];
            let folder_b = &folders[j];

            let files_a = &folder_files[folder_a];
            let files_b = &folder_files[folder_b];

            // Find common hashes
            let hashes_a: HashMap<_, _> = files_a.iter().map(|(id, hash, size)| (hash.clone(), (*id, *size))).collect();
            let hashes_b: HashMap<_, _> = files_b.iter().map(|(id, hash, size)| (hash.clone(), (*id, *size))).collect();

            let mut shared_count = 0;
            let mut shared_size = 0i64;
            let mut shared_file_ids = Vec::new();

            for (hash, (id_a, size)) in &hashes_a {
                if hashes_b.contains_key(hash) {
                    shared_count += 1;
                    shared_size += size;
                    shared_file_ids.push(*id_a);
                }
            }

            if shared_count > 0 {
                let min_files = files_a.len().min(files_b.len()) as f64;
                let similarity_percent = (shared_count as f64 / min_files) * 100.0;

                if similarity_percent >= similarity_threshold {
                    similarities.push(FolderSimilarity {
                        folder_a: folder_a.clone(),
                        folder_b: folder_b.clone(),
                        similarity_percent,
                        shared_files: shared_count as i32,
                        shared_size,
                        unique_a: (files_a.len() - shared_count) as i32,
                        unique_b: (files_b.len() - shared_count) as i32,
                        shared_file_ids,
                    });
                }
            }
        }
    }

    // Sort by similarity percentage (highest first)
    similarities.sort_by(|a, b| {
        b.similarity_percent
            .partial_cmp(&a.similarity_percent)
            .unwrap_or(std::cmp::Ordering::Equal)
    });

    Ok(similarities)
}
