//! Prepared-frame bookkeeping for reviewed publishing (spec 2026-10-01).

use crate::services::ServiceContext;

/// Delete prepared rows and files of frames withheld while a run was active
/// (plan W3). Filled in by the withhold task; a no-op until then.
pub(crate) fn drop_withheld_prepared(ctx: &ServiceContext, project_id: &str) {
    let _ = (ctx, project_id);
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
}
