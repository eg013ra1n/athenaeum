//! Prepared-frame bookkeeping for reviewed publishing (spec 2026-10-01).

use crate::services::ServiceContext;

/// Delete prepared rows and files of frames withheld while a run was active
/// (plan W3). Filled in by the withhold task; a no-op until then.
pub(crate) fn drop_withheld_prepared(ctx: &ServiceContext, project_id: &str) {
    let _ = (ctx, project_id);
}

/// Remove the calibrated files of these prepared rows — never an external
/// (attested) original. Returns how many are gone.
pub(crate) fn remove_prepared_files(rows: &[crate::db::collab_prepare::PreparedRow]) -> usize {
    let mut gone = 0;
    for r in rows {
        let (Some(p), false) = (&r.calibrated_path, r.external) else {
            continue;
        };
        match std::fs::remove_file(p) {
            Ok(()) => gone += 1,
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => gone += 1,
            Err(e) => {
                tracing::error!(project_id = %r.project_id, path = %p, error = %e, "prepared file not removed")
            }
        }
    }
    gone
}

/// The fits writer's temp name (`fits_writer/writer.rs`):
/// `<path>.fits.tmp.<pid>.<seq>` — a crash mid-write leaves one behind.
pub(crate) fn is_writer_temp(name: &str) -> bool {
    let Some(i) = name.rfind(".fits.tmp.") else {
        return false;
    };
    let mut parts = name[i + ".fits.tmp.".len()..].split('.');
    let digits = |s: &str| !s.is_empty() && s.bytes().all(|c| c.is_ascii_digit());
    matches!((parts.next(), parts.next(), parts.next()), (Some(a), Some(b), None) if digits(a) && digits(b))
}

/// Remove stale writer temps from the own folder (spec §4.3 crash windows).
/// Only that exact pattern; never anything else.
pub(crate) fn sweep_writer_temps(dir: &std::path::Path) -> usize {
    let entries = match std::fs::read_dir(dir) {
        Ok(e) => e,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return 0,
        Err(e) => {
            tracing::warn!(path = %dir.display(), error = %e, "own folder not read for stale writer temps");
            return 0;
        }
    };
    let mut removed = 0;
    for e in entries {
        let e = match e {
            Ok(e) => e,
            Err(err) => {
                tracing::warn!(path = %dir.display(), error = %err, "own folder entry not read for stale writer temps");
                continue;
            }
        };
        let name = e.file_name().to_string_lossy().to_string();
        if !is_writer_temp(&name) {
            continue;
        }
        match std::fs::remove_file(e.path()) {
            Ok(()) => removed += 1,
            Err(err) => {
                tracing::error!(path = %e.path().display(), error = %err, "stale writer temp not removed")
            }
        }
    }
    if removed > 0 {
        tracing::info!(path = %dir.display(), count = removed, "stale writer temps removed");
    }
    removed
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writer_temp_names_are_recognized_exactly() {
        assert!(is_writer_temp("c_L_0001.fits.tmp.4242.7"));
        assert!(is_writer_temp("c_L_0001.fits.athpub.fits.tmp.1.0"));
        assert!(!is_writer_temp("c_L_0001.fits"));
        assert!(!is_writer_temp("c_L_0001.fits.tmp.4242"));
        assert!(!is_writer_temp("c_L_0001.fits.tmp.x.7"));
        assert!(!is_writer_temp("notes.tmp.1.2"));
    }

    #[test]
    fn writer_temps_are_swept_and_nothing_else() {
        let tmp = tempfile::tempdir().unwrap();
        for n in [
            "c_a.fits",
            "c_a.fits.tmp.1.2",
            "c_b.fits.tmp.33.0",
            "c_b.fits.athpub",
            "readme.txt",
        ] {
            std::fs::write(tmp.path().join(n), b"x").unwrap();
        }
        assert_eq!(sweep_writer_temps(tmp.path()), 2);
        let mut left: Vec<_> = std::fs::read_dir(tmp.path())
            .unwrap()
            .map(|e| e.unwrap().file_name().into_string().unwrap())
            .collect();
        left.sort();
        assert_eq!(left, ["c_a.fits", "c_b.fits.athpub", "readme.txt"]);
        assert_eq!(
            sweep_writer_temps(&tmp.path().join("missing")),
            0,
            "a missing folder is not an error"
        );
    }

    #[test]
    fn removing_prepared_files_never_touches_an_external_original() {
        let tmp = tempfile::tempdir().unwrap();
        let calibrated = tmp.path().join("c_a.fits");
        let original = tmp.path().join("L_b.fits");
        std::fs::write(&calibrated, b"x").unwrap();
        std::fs::write(&original, b"x").unwrap();
        let row = |path: Option<&std::path::Path>, external: bool| {
            crate::db::collab_prepare::PreparedRow {
                project_id: "p1".into(),
                source_frame_id: 1,
                frame_uuid: "u".into(),
                calibrated_path: path.map(|p| p.to_string_lossy().into_owned()),
                external,
                own_dir: tmp.path().to_string_lossy().into_owned(),
                recipe_hash: "r".into(),
                xxh3: "x".into(),
                byte_size: 1,
                size_mtime_seen: None,
                prepared_at: String::new(),
                publish_run_id: "run".into(),
            }
        };
        let rows = [
            row(Some(&calibrated), false),
            // Never a real shape (an external row has no path), but the
            // original must survive even then.
            row(Some(&original), true),
            row(None, true),
            row(Some(&tmp.path().join("gone.fits")), false),
        ];
        assert_eq!(
            remove_prepared_files(&rows),
            2,
            "a missing file counts as removed"
        );
        assert!(!calibrated.exists());
        assert!(original.exists());
    }
}
