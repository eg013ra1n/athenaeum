//! Shared archive-root resolution — which folder a ZIP archive operation
//! writes into. Originally lived duplicated in both `athenaeum-tauri`'s
//! `commands::archive` and `athenaeum-web`'s `routes::archive`; moved here
//! (Task 14) so `api::masters::archive_originals` (calibration-set
//! archive-of-originals, which has no per-call `archive_root_path` UI
//! argument) can share the exact same default-root selection logic as the
//! frame-set archive flow, instead of a third copy.

use crate::settings::SettingsManager;
use anyhow::Result;
use rusqlite::Connection;

/// Migrate the legacy single-folder `archive.root_path` setting into the
/// `archive_roots` table on first call. Idempotent: if the table already has
/// rows, does nothing.
pub fn migrate_legacy_archive_root(conn: &Connection, settings: &SettingsManager) -> Result<()> {
    let count: i64 = conn.query_row("SELECT COUNT(*) FROM archive_roots", [], |r| r.get(0))?;
    if count > 0 {
        return Ok(());
    }
    if let Some(legacy) = settings.get_archive_root_path(conn)? {
        if !legacy.trim().is_empty() {
            conn.execute(
                "INSERT OR IGNORE INTO archive_roots (path, label, is_default) VALUES (?1, NULL, 1)",
                [&legacy],
            )?;
        }
    }
    Ok(())
}

/// Resolve which archive root path to use for an operation. If the caller
/// passed an explicit path, validate it's a known archive root. Otherwise
/// pick the default (or the only one if just one exists). Errors when no
/// archive roots are configured or no default is set with multiple roots.
pub fn resolve_archive_root(
    conn: &Connection,
    settings: &SettingsManager,
    requested: Option<&str>,
) -> Result<String> {
    migrate_legacy_archive_root(conn, settings)?;
    if let Some(p) = requested {
        let known = |candidate: &str| -> Result<bool> {
            Ok(conn.query_row(
                "SELECT COUNT(*) FROM archive_roots WHERE path = ?1",
                [candidate],
                |r| r.get::<_, i64>(0),
            )? > 0)
        };
        if known(p)? {
            return Ok(p.to_string());
        }
        // The caller may hand back a different spelling of a configured root
        // (case variant on a case-insensitive FS, verbatim prefix, trailing
        // separator). Retry with the canonical normalized form before rejecting.
        if let Ok(c) = std::path::Path::new(p).canonicalize() {
            let normalized = crate::api::scan_roots::normalize_path(&c)
                .to_string_lossy()
                .to_string();
            if known(&normalized)? {
                return Ok(normalized);
            }
        }
        anyhow::bail!("'{}' is not a configured archive folder", p);
    }
    let rows: Vec<(String, i32)> = {
        let mut stmt = conn.prepare("SELECT path, is_default FROM archive_roots ORDER BY id")?;
        let mapped = stmt
            .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, i32>(1)?)))?
            .collect::<rusqlite::Result<Vec<_>>>()?;
        mapped
    };
    if rows.is_empty() {
        anyhow::bail!("no archive folders configured — add one in File Manager → Archive Folders");
    }
    if rows.len() == 1 {
        return Ok(rows[0].0.clone());
    }
    if let Some((path, _)) = rows.iter().find(|(_, d)| *d == 1) {
        return Ok(path.clone());
    }
    anyhow::bail!("multiple archive folders configured but no default — pick a destination explicitly");
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;

    fn test_ctx() -> (Connection, SettingsManager) {
        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        (conn, SettingsManager::new())
    }

    #[test]
    fn resolve_errors_when_no_roots_configured() {
        let (conn, settings) = test_ctx();
        let err = resolve_archive_root(&conn, &settings, None).unwrap_err();
        assert!(format!("{err:#}").contains("no archive folders configured"));
    }

    #[test]
    fn resolve_picks_only_root_when_single() {
        let (conn, settings) = test_ctx();
        conn.execute(
            "INSERT INTO archive_roots (path, is_default) VALUES ('/arch', 0)",
            [],
        ).unwrap();
        assert_eq!(resolve_archive_root(&conn, &settings, None).unwrap(), "/arch");
    }

    #[test]
    fn resolve_requires_default_with_multiple_roots() {
        let (conn, settings) = test_ctx();
        conn.execute(
            "INSERT INTO archive_roots (path, is_default) VALUES ('/a', 0), ('/b', 0)",
            [],
        ).unwrap();
        let err = resolve_archive_root(&conn, &settings, None).unwrap_err();
        assert!(format!("{err:#}").contains("no default"));

        conn.execute("UPDATE archive_roots SET is_default = 1 WHERE path = '/b'", []).unwrap();
        assert_eq!(resolve_archive_root(&conn, &settings, None).unwrap(), "/b");
    }

    #[test]
    fn resolve_validates_explicit_request() {
        let (conn, settings) = test_ctx();
        conn.execute(
            "INSERT INTO archive_roots (path, is_default) VALUES ('/arch', 1)",
            [],
        ).unwrap();
        assert_eq!(
            resolve_archive_root(&conn, &settings, Some("/arch")).unwrap(),
            "/arch",
        );
        let err = resolve_archive_root(&conn, &settings, Some("/not-known")).unwrap_err();
        assert!(format!("{err:#}").contains("not a configured archive folder"));
    }

    #[test]
    fn resolve_accepts_respelled_configured_root() {
        let (conn, settings) = test_ctx();
        let dir = tempfile::tempdir().unwrap();
        let canonical = dir
            .path()
            .canonicalize()
            .unwrap()
            .to_string_lossy()
            .to_string();
        conn.execute(
            "INSERT INTO archive_roots (path, label, is_default) VALUES (?1, NULL, 1)",
            [&canonical],
        )
        .unwrap();
        // Same folder, different spelling (trailing separator) — must resolve.
        let respelled = format!("{}{}", canonical, std::path::MAIN_SEPARATOR);
        let resolved = resolve_archive_root(&conn, &settings, Some(&respelled)).unwrap();
        assert_eq!(resolved, canonical);
        // A genuinely unknown folder still errors.
        let other = tempfile::tempdir().unwrap();
        assert!(
            resolve_archive_root(&conn, &settings, Some(other.path().to_str().unwrap())).is_err()
        );
    }
}
