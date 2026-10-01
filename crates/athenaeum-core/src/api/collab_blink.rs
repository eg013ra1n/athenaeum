//! Spec 2026-10-01 §9.1 — what Blink shows for a project frame. Every path
//! comes from the catalog/collab DB, never from the client.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use rusqlite::{Connection, OptionalExtension};

use crate::api::{db, ApiError};
use crate::db::collab_frames::{FrameOrigin, LocalFrameRow, LocalState};
use crate::models::{File, FileFormat, FileWithFrame, Frame};
use crate::services::ServiceContext;

#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabFrameRef {
    pub frame_id: Option<i64>,
    pub frame_uuid: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum BlinkSource {
    Raw,
    Calibrated,
    Replica,
}

#[derive(Debug, Clone, Serialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct CollabBlinkEntry {
    pub key: String,
    pub source: BlinkSource,
    pub entry: FileWithFrame,
    pub frame_uuid: Option<String>,
    pub source_frame_id: Option<i64>,
    pub publisher_name: Option<String>,
}

/// What one ref resolved to: the file to render, how it is labelled, and the
/// own/replica row when it came in by uuid.
struct Resolved {
    path: PathBuf,
    source: BlinkSource,
    row: Option<LocalFrameRow>,
}

/// Built once per call: the live-project check and the Collaboration root.
struct ProjectScope {
    root: Option<PathBuf>,
}

impl ProjectScope {
    fn new(ctx: &ServiceContext, project_id: &str) -> Result<Self, ApiError> {
        {
            let d = db(ctx)?;
            let conn = d.conn();
            crate::api::collab_exchange::live_project(&conn, project_id)?;
        }
        let root = crate::api::scan_roots::get_collaboration_dir(ctx)?.map(PathBuf::from);
        Ok(Self { root })
    }
}

/// `Some(attested)` when `fid` is a LIGHT of a set linked to the project.
/// One indexed lookup — never the whole project's light list.
fn member_attested(
    conn: &Connection,
    project_id: &str,
    fid: i64,
) -> Result<Option<bool>, ApiError> {
    let set_id: Option<i64> = conn
        .query_row(
            "SELECT ino.frames_set_id FROM session_members sm \
             JOIN sessions s ON s.id = sm.session_id \
             JOIN imaging_nights ino ON ino.id = s.imaging_night_id \
             JOIN frames f ON f.id = sm.frame_id \
             WHERE sm.frame_id = ?1 AND f.imagetyp = 'Light' \
               AND ino.frames_set_id IN (SELECT frames_set_id FROM project_links WHERE project_id = ?2) \
             LIMIT 1",
            rusqlite::params![fid, project_id],
            |r| r.get(0),
        )
        .optional()
        .map_err(|e| ApiError::Internal(format!("blink: membership lookup failed: {e}")))?;
    match set_id {
        None => Ok(None),
        Some(s) => Ok(Some(crate::db::collab::frames_set_attested(conn, s)?)),
    }
}

fn raw_path_of(conn: &Connection, fid: i64) -> Result<PathBuf, ApiError> {
    let raw: String = conn
        .query_row(
            "SELECT fi.path FROM frames f JOIN files fi ON fi.id = f.file_id WHERE f.id = ?1",
            [fid],
            |row| row.get(0),
        )
        .map_err(|e| match e {
            rusqlite::Error::QueryReturnedNoRows => {
                ApiError::NotFound(format!("frame {fid} has no file"))
            }
            other => ApiError::Internal(format!("blink: raw path lookup failed: {other}")),
        })?;
    let raw = PathBuf::from(raw);
    if !raw.exists() {
        return Err(ApiError::NotFound(format!(
            "{} is not on this device",
            raw.display()
        )));
    }
    Ok(raw)
}

fn is_external(row: &LocalFrameRow) -> bool {
    row.recipe_hash
        .as_deref()
        .is_some_and(|h| h.starts_with("external:"))
}

fn resolve_with(
    conn: &Connection,
    scope: &ProjectScope,
    project_id: &str,
    r: &CollabFrameRef,
) -> Result<Resolved, ApiError> {
    if let Some(fid) = r.frame_id {
        let Some(attested) = member_attested(conn, project_id, fid)? else {
            tracing::warn!(
                project_id,
                frame_id = fid,
                "blink: frame is not part of this project"
            );
            return Err(ApiError::Forbidden(
                "this frame is not part of the project".into(),
            ));
        };
        // A prepared file wins only while it is current (plan W2) — the same
        // rule the My frames list shows it by.
        if let Some(p) = crate::db::collab_prepare::get_prepared(conn, project_id, fid)? {
            if !p.external && crate::api::collab::prepared_is_current(conn, &p, attested) {
                if let Some(path) = p.calibrated_path.as_deref().map(PathBuf::from) {
                    if path.exists() {
                        return Ok(Resolved {
                            path,
                            source: BlinkSource::Calibrated,
                            row: None,
                        });
                    }
                }
            }
        }
        if let Some(own) =
            crate::db::collab_frames::own_row_for_source_frame(conn, project_id, fid)?
        {
            if let (Some(path), false) = (own.landed_path.as_deref(), is_external(&own)) {
                let path = PathBuf::from(path);
                if path.exists() {
                    return Ok(Resolved {
                        path,
                        source: BlinkSource::Calibrated,
                        row: None,
                    });
                }
            }
        }
        return Ok(Resolved {
            path: raw_path_of(conn, fid)?,
            source: BlinkSource::Raw,
            row: None,
        });
    }
    let uuid = r
        .frame_uuid
        .as_deref()
        .ok_or_else(|| ApiError::Invalid("a frame id or uuid is required".into()))?;
    let row = crate::db::collab_frames::get(conn, project_id, uuid)?
        .ok_or_else(|| ApiError::NotFound(format!("frame {uuid} is not in this project")))?;
    let not_here = || ApiError::NotFound(format!("frame {uuid} is not on this device"));
    if !matches!(row.local_state, LocalState::Held | LocalState::OwnHeld) {
        return Err(not_here());
    }
    if row.origin == FrameOrigin::Own {
        // Spec §9.1: calibrated, or the original when attested.
        if is_external(&row) {
            let fid = row.source_frame_id.ok_or_else(not_here)?;
            let path = raw_path_of(conn, fid)?;
            return Ok(Resolved {
                path,
                source: BlinkSource::Raw,
                row: Some(row),
            });
        }
        let path = row
            .landed_path
            .as_deref()
            .map(PathBuf::from)
            .filter(|p| p.exists())
            .ok_or_else(not_here)?;
        return Ok(Resolved {
            path,
            source: BlinkSource::Calibrated,
            row: Some(row),
        });
    }
    let path = row
        .landed_path
        .as_deref()
        .map(PathBuf::from)
        .filter(|p| {
            crate::api::collab_exchange::inside_root(scope.root.as_deref(), &row, p) && p.exists()
        })
        .ok_or_else(not_here)?;
    Ok(Resolved {
        path,
        source: BlinkSource::Replica,
        row: Some(row),
    })
}

pub fn resolve_collab_frame_path(
    ctx: &ServiceContext,
    project_id: &str,
    r: &CollabFrameRef,
) -> Result<(PathBuf, BlinkSource), ApiError> {
    let scope = ProjectScope::new(ctx, project_id)?;
    let d = db(ctx)?;
    let conn = d.conn();
    let res = resolve_with(&conn, &scope, project_id, r)?;
    Ok((res.path, res.source))
}

fn synthetic_file(path: &Path) -> Option<File> {
    let ext = path.extension()?.to_string_lossy().to_ascii_lowercase();
    let format = match ext.as_str() {
        "fits" | "fit" | "fts" => FileFormat::FITS,
        "xisf" => FileFormat::XISF,
        _ => return None,
    };
    let meta = std::fs::metadata(path).ok()?;
    let modified: chrono::DateTime<chrono::Utc> = meta.modified().ok()?.into();
    Some(File {
        id: None,
        path: path.to_string_lossy().into_owned(),
        filename: path.file_name()?.to_string_lossy().into_owned(),
        size: meta.len() as i64,
        modified_at: modified,
        format,
        created_at: modified,
        content_hash: None,
        archived_in_operation: None,
        archive_zip_path: None,
        archive_path_in_zip: None,
        uuid: None,
        updated_at: None,
    })
}

fn parse_date_obs(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s)
        .map(|d| d.with_timezone(&chrono::Utc))
        .or_else(|_| {
            chrono::NaiveDateTime::parse_from_str(s, "%Y-%m-%dT%H:%M:%S%.f").map(|n| n.and_utc())
        })
        .ok()
}

pub fn get_collab_blink_frames(
    ctx: &ServiceContext,
    project_id: &str,
    refs: &[CollabFrameRef],
) -> Result<Vec<CollabBlinkEntry>, ApiError> {
    // The whole call is refused when the project is not live.
    let scope = ProjectScope::new(ctx, project_id)?;
    let d = db(ctx)?;
    let conn = d.conn();
    let mut out = Vec::with_capacity(refs.len());
    let mut dropped = 0usize;
    for r in refs {
        let resolved = match resolve_with(&conn, &scope, project_id, r) {
            Ok(v) => v,
            // Only "not on this device" drops an entry; everything else
            // (Forbidden, Internal, Invalid) refuses the call.
            Err(ApiError::NotFound(m)) => {
                tracing::debug!(project_id, reason = %m, "blink: frame left out");
                dropped += 1;
                continue;
            }
            Err(e) => return Err(e),
        };
        let Resolved { path, source, row } = resolved;
        let (frame, file, publisher_name, source_frame_id) = if let Some(fid) = r.frame_id {
            let (_, catalog_file, frame) = crate::db::get_frames_with_files_by_ids(&conn, &[fid])?
                .into_iter()
                .next()
                .ok_or_else(|| ApiError::NotFound(format!("frame {fid}")))?;
            let file = if source == BlinkSource::Raw {
                Some(catalog_file)
            } else {
                synthetic_file(&path)
            };
            (frame, file, None, Some(fid))
        } else {
            let row =
                row.ok_or_else(|| ApiError::Internal("blink: uuid ref without a row".into()))?;
            let uuid = row.frame_uuid.as_str();
            let wire = crate::api::collab_exchange::parse_manifest_wire(
                project_id,
                uuid,
                &row.manifest_json,
                "blink",
            );
            let meta = |k: &str| {
                wire.as_ref()
                    .and_then(|w| w.meta.get(k))
                    .and_then(|v| v.as_str())
                    .map(str::to_string)
            };
            let frame = Frame {
                filter: Some(row.filter_canonical.clone()),
                exptime: wire.as_ref().map(|w| w.exptime_sec),
                date_obs: wire
                    .as_ref()
                    .and_then(|w| w.date_obs.as_deref())
                    .and_then(parse_date_obs),
                instrume: meta("instrume"),
                telescop: meta("telescope"),
                bayerpat: meta("bayerpat"),
                imagetyp: Some(crate::models::ImageType::Light),
                ..Default::default()
            };
            (
                frame,
                synthetic_file(&path),
                Some(row.publisher_display.clone()),
                row.source_frame_id,
            )
        };
        let Some(file) = file else {
            tracing::debug!(project_id, path = %path.display(), "blink: no readable FITS/XISF file, frame left out");
            dropped += 1;
            continue;
        };
        out.push(CollabBlinkEntry {
            key: r
                .frame_uuid
                .clone()
                .unwrap_or_else(|| format!("f{}", r.frame_id.unwrap_or_default())),
            source,
            entry: FileWithFrame {
                file,
                frame: Some(frame),
            },
            frame_uuid: r.frame_uuid.clone(),
            source_frame_id,
            publisher_name,
        });
    }
    if dropped > 0 {
        tracing::warn!(
            project_id,
            count = dropped,
            "blink: frames not on this device were left out"
        );
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::collab::tests::publish::{fixture, mount_hub, own_dir, PID};

    #[tokio::test]
    async fn an_own_frame_resolves_raw_then_calibrated() {
        let fx = fixture(1).await;
        let r = CollabFrameRef {
            frame_id: Some(fx.frame_ids[0]),
            frame_uuid: None,
        };
        let (p, s) = resolve_collab_frame_path(&fx.ctx, PID, &r).unwrap();
        assert_eq!((s, p), (BlinkSource::Raw, fx.lights[0].clone()));
        crate::api::collab::calibrate_collab_frames(&fx.ctx, PID, None, None)
            .await
            .unwrap();
        let (p, s) = resolve_collab_frame_path(&fx.ctx, PID, &r).unwrap();
        assert_eq!(s, BlinkSource::Calibrated);
        assert!(p.starts_with(own_dir(&fx)));
        let e = get_collab_blink_frames(&fx.ctx, PID, &[r]).unwrap();
        assert_eq!(e.len(), 1);
        assert!(
            e[0].entry.file.id.is_none(),
            "a calibrated file is not catalogued"
        );
        assert_eq!(
            e[0].entry.frame.as_ref().and_then(|f| f.id),
            Some(fx.frame_ids[0]),
            "metadata from the source frame"
        );
    }

    /// A real LIGHT in a frame set that is NOT linked to the project.
    fn unlinked_light(conn: &rusqlite::Connection) -> i64 {
        conn.execute("INSERT INTO frames_set (name) VALUES ('Other Set')", [])
            .unwrap();
        let set_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO imaging_nights (frames_set_id, start_time, end_time) VALUES (?1, '2026-07-01T20:00:00Z', '2026-07-02T03:00:00Z')",
            [set_id],
        )
        .unwrap();
        let night = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO sessions (imaging_night_id, instrume) VALUES (?1, 'X')",
            [night],
        )
        .unwrap();
        let session = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO files (path, filename, size, modified_at, format) VALUES ('/nowhere/o.fits', 'o.fits', 1, '2026-07-01T21:00:00Z', 'FITS')",
            [],
        )
        .unwrap();
        let file = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO frames (file_id, imagetyp) VALUES (?1, 'Light')",
            [file],
        )
        .unwrap();
        let frame = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)",
            [session, frame],
        )
        .unwrap();
        frame
    }

    #[tokio::test]
    async fn a_frame_outside_the_project_is_forbidden_and_refuses_the_whole_list() {
        let fx = fixture(1).await;
        let outsider = {
            let conn = crate::api::db(&fx.ctx).unwrap().conn();
            unlinked_light(&conn)
        };
        let out = CollabFrameRef {
            frame_id: Some(outsider),
            frame_uuid: None,
        };
        assert!(matches!(
            resolve_collab_frame_path(&fx.ctx, PID, &out),
            Err(ApiError::Forbidden(_))
        ));
        let ok = CollabFrameRef {
            frame_id: Some(fx.frame_ids[0]),
            frame_uuid: None,
        };
        assert!(matches!(
            get_collab_blink_frames(&fx.ctx, PID, &[ok, out]),
            Err(ApiError::Forbidden(_))
        ));
    }

    #[tokio::test]
    async fn a_missing_ref_is_dropped_and_a_lost_project_refuses() {
        let fx = fixture(1).await;
        let gone = CollabFrameRef {
            frame_id: None,
            frame_uuid: Some("no-such-uuid".into()),
        };
        assert!(get_collab_blink_frames(&fx.ctx, PID, &[gone.clone()])
            .unwrap()
            .is_empty());
        assert!(matches!(
            get_collab_blink_frames(&fx.ctx, "no-such-project", &[gone]),
            Err(ApiError::NotFound(_))
        ));
        let none = CollabFrameRef {
            frame_id: None,
            frame_uuid: None,
        };
        assert!(matches!(
            get_collab_blink_frames(&fx.ctx, PID, &[none]),
            Err(ApiError::Invalid(_))
        ));
    }

    #[tokio::test]
    async fn a_stale_prepared_file_is_not_served() {
        let fx = fixture(1).await;
        let r = CollabFrameRef {
            frame_id: Some(fx.frame_ids[0]),
            frame_uuid: None,
        };
        crate::api::collab::calibrate_collab_frames(&fx.ctx, PID, None, None)
            .await
            .unwrap();
        assert_eq!(
            resolve_collab_frame_path(&fx.ctx, PID, &r).unwrap().1,
            BlinkSource::Calibrated
        );
        {
            let conn = crate::api::db(&fx.ctx).unwrap().conn();
            conn.execute(
                "UPDATE collab_prepared_frames SET recipe_hash = 'stale'",
                [],
            )
            .unwrap();
        }
        let (p, s) = resolve_collab_frame_path(&fx.ctx, PID, &r).unwrap();
        assert_eq!((s, p), (BlinkSource::Raw, fx.lights[0].clone()));
    }

    #[tokio::test]
    async fn a_published_own_frame_resolves_to_its_landed_file() {
        let fx = fixture(1).await;
        mount_hub(&fx.server, "published").await;
        crate::api::collab::calibrate_collab_frames(&fx.ctx, PID, None, None)
            .await
            .unwrap();
        crate::api::collab::publish_collab_frames(&fx.ctx, PID, None, None)
            .await
            .unwrap();
        let landed: String = {
            let conn = crate::api::db(&fx.ctx).unwrap().conn();
            conn.query_row(
                "SELECT landed_path FROM project_frames_local WHERE project_id = ?1 AND origin = 'own'",
                [PID],
                |r| r.get(0),
            )
            .unwrap()
        };
        let by_id = CollabFrameRef {
            frame_id: Some(fx.frame_ids[0]),
            frame_uuid: None,
        };
        let (p, s) = resolve_collab_frame_path(&fx.ctx, PID, &by_id).unwrap();
        assert_eq!(
            (s, p),
            (BlinkSource::Calibrated, std::path::PathBuf::from(&landed))
        );
        // By uuid an own row is calibrated too, and names its source frame.
        let uuid: String = {
            let conn = crate::api::db(&fx.ctx).unwrap().conn();
            conn.query_row(
                "SELECT frame_uuid FROM project_frames_local WHERE project_id = ?1 AND origin = 'own'",
                [PID],
                |r| r.get(0),
            )
            .unwrap()
        };
        let by_uuid = CollabFrameRef {
            frame_id: None,
            frame_uuid: Some(uuid),
        };
        let e = get_collab_blink_frames(&fx.ctx, PID, &[by_uuid]).unwrap();
        assert_eq!(e[0].source, BlinkSource::Calibrated);
        assert_eq!(e[0].source_frame_id, Some(fx.frame_ids[0]));
    }

    #[tokio::test]
    async fn an_external_frame_always_shows_its_original() {
        let fx = fixture(1).await;
        {
            let conn = crate::api::db(&fx.ctx).unwrap().conn();
            conn.execute(
                "UPDATE frames_set SET calibrated_externally = 1 WHERE id = ?1",
                [fx.set_id],
            )
            .unwrap();
        }
        crate::api::collab::calibrate_collab_frames(&fx.ctx, PID, None, None)
            .await
            .unwrap();
        let r = CollabFrameRef {
            frame_id: Some(fx.frame_ids[0]),
            frame_uuid: None,
        };
        let (p, s) = resolve_collab_frame_path(&fx.ctx, PID, &r).unwrap();
        assert_eq!((s, p), (BlinkSource::Raw, fx.lights[0].clone()));
    }

    #[tokio::test]
    async fn a_held_replica_resolves_by_uuid() {
        // test_support::landed_rig: frames[i] = (project_id, uuid, landed_path), state Held.
        let rig = crate::api::collab_live::test_support::landed_rig(1).await;
        let (pid, uuid, landed) = rig.frames[0].clone();
        let r = CollabFrameRef {
            frame_id: None,
            frame_uuid: Some(uuid),
        };
        let (p, s) = resolve_collab_frame_path(&rig.ctx, &pid, &r).unwrap();
        assert_eq!(s, BlinkSource::Replica);
        assert_eq!(p, std::path::PathBuf::from(landed));
    }

    #[tokio::test]
    async fn a_replica_entry_carries_manifest_metadata_and_a_synthetic_file() {
        let rig = crate::api::collab_live::test_support::landed_rig(1).await;
        let (pid, uuid, landed) = rig.frames[0].clone();
        let r = CollabFrameRef {
            frame_id: None,
            frame_uuid: Some(uuid.clone()),
        };
        let e = get_collab_blink_frames(&rig.ctx, &pid, &[r]).unwrap();
        assert_eq!(e.len(), 1);
        assert_eq!(e[0].source, BlinkSource::Replica);
        assert_eq!(e[0].frame_uuid.as_deref(), Some(uuid.as_str()));
        assert!(e[0].publisher_name.is_some());
        assert!(e[0].entry.file.id.is_none());
        assert_eq!(std::path::PathBuf::from(&e[0].entry.file.path), landed);
        let f = e[0].entry.frame.as_ref().unwrap();
        assert!(f.filter.is_some());
        assert_eq!(f.imagetyp, Some(crate::models::ImageType::Light));
    }

    #[tokio::test]
    async fn a_row_that_is_not_held_is_not_served() {
        let rig = crate::api::collab_live::test_support::landed_rig(1).await;
        let (pid, uuid, _) = rig.frames[0].clone();
        {
            let conn = crate::api::db(&rig.ctx).unwrap().conn();
            conn.execute(
                "UPDATE project_frames_local SET local_state = 'wanted' WHERE project_id = ?1 AND frame_uuid = ?2",
                [&pid, &uuid],
            )
            .unwrap();
        }
        let r = CollabFrameRef {
            frame_id: None,
            frame_uuid: Some(uuid),
        };
        assert!(matches!(
            resolve_collab_frame_path(&rig.ctx, &pid, &r),
            Err(ApiError::NotFound(_))
        ));
        assert!(get_collab_blink_frames(&rig.ctx, &pid, &[r])
            .unwrap()
            .is_empty());
    }
}
