//! Prepared-frame bookkeeping for reviewed publishing (spec 2026-10-01).

use crate::services::ServiceContext;

/// Spec 2026-10-01 §4.5 — Don't publish (`withheld = true`) / Release.
pub async fn set_collab_frames_withheld(
    ctx: &ServiceContext,
    project_id: &str,
    frame_ids: &[i64],
    withheld: bool,
) -> Result<u32, crate::api::ApiError> {
    use crate::api::ApiError;
    // Known narrow race, accepted: a run may start after this read and before
    // the transaction commits; its seed then fails on the deleted file and the
    // frame is held back for that run only.
    let run_active = crate::api::collab_publish_run::is_active(ctx, project_id);
    let (changed, gone) = {
        let d = crate::api::db(ctx)?;
        let conn = d.conn();
        crate::api::collab_exchange::live_project(&conn, project_id)?;
        conn.execute_batch("BEGIN IMMEDIATE")
            .map_err(|e| crate::api::collab::internal(e.into()))?;
        let res = (|| -> Result<(usize, Vec<crate::db::collab_prepare::PreparedRow>), ApiError> {
            if withheld {
                let own = crate::db::collab_frames::own_by_source_frame(&conn, project_id)
                    .map_err(crate::api::collab::internal)?;
                let in_project = frame_ids.iter().filter(|id| own.contains_key(id)).count();
                if in_project > 0 {
                    return Err(ApiError::Invalid(format!(
                        "{in_project} of these frames are already in the project and cannot be withheld"
                    )));
                }
            }
            let n = crate::db::collab_prepare::set_withheld(&conn, project_id, frame_ids, withheld)
                .map_err(crate::api::collab::internal)?;
            let gone = if withheld && !run_active {
                crate::db::collab_prepare::delete_prepared(&conn, project_id, frame_ids)
                    .map_err(crate::api::collab::internal)?
            } else {
                Vec::new()
            };
            Ok((n, gone))
        })();
        match res {
            Ok(v) => {
                conn.execute_batch("COMMIT")
                    .map_err(|e| crate::api::collab::internal(e.into()))?;
                v
            }
            Err(e) => {
                if let Err(rb) = conn.execute_batch("ROLLBACK") {
                    tracing::error!(project_id, error = %rb, "withhold rollback failed");
                }
                tracing::warn!(project_id, error = %e, "withhold refused");
                return Err(e);
            }
        }
    };
    let removed = remove_prepared_files(&gone);
    tracing::info!(
        project_id,
        withheld,
        count = changed,
        removed,
        deferred = run_active,
        "collab frames withheld or released"
    );
    if !withheld && changed > 0 {
        crate::api::collab_autopublish::request_auto_publish(Some(project_id));
    }
    Ok(changed as u32)
}

/// Plan W3 — at a run's end: prepared rows of frames withheld meanwhile go,
/// with their (non-external) files. Logs, never fails the run.
pub(crate) fn drop_withheld_prepared(ctx: &ServiceContext, project_id: &str) {
    match crate::api::db(ctx) {
        Ok(d) => drop_withheld_prepared_db(d, project_id),
        Err(e) => tracing::error!(project_id, error = %e, "withheld prepared frames not dropped"),
    }
}

/// The DB-level core of [`drop_withheld_prepared`]; the interrupted-run guard
/// (which holds no `ServiceContext`) calls it too.
pub(crate) fn drop_withheld_prepared_db(db: &crate::db::Database, project_id: &str) {
    let gone = (|| -> anyhow::Result<Vec<crate::db::collab_prepare::PreparedRow>> {
        let conn = db.conn();
        let withheld: Vec<i64> = crate::db::collab_prepare::withheld_ids(&conn, project_id)?
            .into_iter()
            .collect();
        crate::db::collab_prepare::delete_prepared(&conn, project_id, &withheld)
    })();
    match gone {
        Ok(rows) if !rows.is_empty() => {
            let removed = remove_prepared_files(&rows);
            tracing::info!(
                project_id,
                count = rows.len(),
                removed,
                "withheld prepared frames dropped after the run"
            );
        }
        Ok(_) => {}
        Err(e) => tracing::error!(project_id, error = %e, "withheld prepared frames not dropped"),
    }
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
