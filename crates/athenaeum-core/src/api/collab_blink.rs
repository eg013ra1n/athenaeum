//! Spec 2026-10-01 §9.1 — what Blink shows for a project frame. Every path
//! comes from the catalog/collab DB, never from the client.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::api::{db, ApiError};
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

pub fn resolve_collab_frame_path(
    ctx: &ServiceContext,
    project_id: &str,
    r: &CollabFrameRef,
) -> Result<(PathBuf, BlinkSource), ApiError> {
    let d = db(ctx)?;
    let conn = d.conn();
    crate::api::collab_exchange::live_project(&conn, project_id)?;
    if let Some(fid) = r.frame_id {
        let sets = crate::db::collab::linked_set_ids(&conn, project_id)?;
        let in_project = !sets.is_empty()
            && crate::api::collab::union_light_frames(&conn, &sets)?
                .iter()
                .any(|(id, _)| *id == fid);
        if !in_project {
            tracing::warn!(
                project_id,
                frame_id = fid,
                "blink: frame is not part of this project"
            );
            return Err(ApiError::Forbidden(
                "this frame is not part of the project".into(),
            ));
        }
        if let Some(p) = crate::db::collab_prepare::get_prepared(&conn, project_id, fid)? {
            if let (Some(path), false) = (p.calibrated_path, p.external) {
                let path = PathBuf::from(path);
                if path.exists() {
                    return Ok((path, BlinkSource::Calibrated));
                }
            }
        }
        if let Some(own) =
            crate::db::collab_frames::own_by_source_frame(&conn, project_id)?.get(&fid)
        {
            let external = own
                .recipe_hash
                .as_deref()
                .is_some_and(|h| h.starts_with("external:"));
            if let (Some(path), false) = (own.landed_path.as_ref(), external) {
                let path = PathBuf::from(path);
                if path.exists() {
                    return Ok((path, BlinkSource::Calibrated));
                }
            }
        }
        let raw: String = conn
            .query_row(
                "SELECT fi.path FROM frames f JOIN files fi ON fi.id = f.file_id WHERE f.id = ?1",
                [fid],
                |row| row.get(0),
            )
            .map_err(|_| ApiError::NotFound(format!("frame {fid} has no file")))?;
        let raw = PathBuf::from(raw);
        if !raw.exists() {
            return Err(ApiError::NotFound(format!(
                "{} is not on this device",
                raw.display()
            )));
        }
        return Ok((raw, BlinkSource::Raw));
    }
    let uuid = r
        .frame_uuid
        .as_deref()
        .ok_or_else(|| ApiError::Invalid("a frame id or uuid is required".into()))?;
    let row = crate::db::collab_frames::get(&conn, project_id, uuid)?
        .ok_or_else(|| ApiError::NotFound(format!("frame {uuid} is not in this project")))?;
    let held = matches!(row.local_state.as_db_str(), "held" | "own_held");
    let path = row
        .landed_path
        .as_ref()
        .map(PathBuf::from)
        .filter(|p| held && p.exists());
    match path {
        Some(p) => Ok((p, BlinkSource::Replica)),
        None => Err(ApiError::NotFound(format!(
            "frame {uuid} is not on this device"
        ))),
    }
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
    let mut out = Vec::with_capacity(refs.len());
    let mut dropped = 0usize;
    for r in refs {
        let (path, source) = match resolve_collab_frame_path(ctx, project_id, r) {
            Ok(v) => v,
            Err(ApiError::Forbidden(m)) => return Err(ApiError::Forbidden(m)),
            Err(e) => {
                tracing::debug!(project_id, error = %e, "blink: frame skipped");
                dropped += 1;
                continue;
            }
        };
        let d = db(ctx)?;
        let conn = d.conn();
        let (entry, publisher_name) = if let Some(fid) = r.frame_id {
            // db/operations.rs ≈3169: Vec<(frame_id, File, Frame)>.
            let (_, catalog_file, frame) = crate::db::get_frames_with_files_by_ids(&conn, &[fid])?
                .into_iter()
                .next()
                .ok_or_else(|| ApiError::NotFound(format!("frame {fid}")))?;
            let file = if source == BlinkSource::Calibrated {
                // Keep the catalog Frame (metadata, star metrics); swap the File.
                match synthetic_file(&path) {
                    Some(f) => f,
                    None => {
                        dropped += 1;
                        continue;
                    }
                }
            } else {
                catalog_file
            };
            (
                FileWithFrame {
                    file,
                    frame: Some(frame),
                },
                None,
            )
        } else {
            let uuid = r.frame_uuid.as_deref().unwrap_or_default();
            let row = crate::db::collab_frames::get(&conn, project_id, uuid)?
                .ok_or_else(|| ApiError::NotFound(uuid.into()))?;
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
            let Some(file) = synthetic_file(&path) else {
                dropped += 1;
                continue;
            };
            (
                FileWithFrame {
                    file,
                    frame: Some(frame),
                },
                Some(row.publisher_display.clone()),
            )
        };
        out.push(CollabBlinkEntry {
            key: r
                .frame_uuid
                .clone()
                .unwrap_or_else(|| format!("f{}", r.frame_id.unwrap_or_default())),
            source,
            entry,
            frame_uuid: r.frame_uuid.clone(),
            source_frame_id: r.frame_id,
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
    use crate::api::collab::tests::publish::{fixture, own_dir, PID};

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

    #[tokio::test]
    async fn a_frame_outside_the_project_is_refused_and_a_missing_ref_dropped() {
        let fx = fixture(1).await;
        let outsider = CollabFrameRef {
            frame_id: Some(987_654),
            frame_uuid: None,
        };
        assert!(matches!(
            resolve_collab_frame_path(&fx.ctx, PID, &outsider),
            Err(ApiError::Forbidden(_) | ApiError::NotFound(_))
        ));
        let gone = CollabFrameRef {
            frame_id: None,
            frame_uuid: Some("no-such-uuid".into()),
        };
        assert!(get_collab_blink_frames(&fx.ctx, PID, &[gone])
            .unwrap()
            .is_empty());
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
}
