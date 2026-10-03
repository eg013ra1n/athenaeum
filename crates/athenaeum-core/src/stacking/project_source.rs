//! A collaboration project as a stacking subject's frame source (spec
//! 2026-10-02 §4.3, §7, §8; plan 6b-2 Task 6a): the project's stackable
//! landed frames grouped by colour × canonical filter × exposure class
//! (W7: camera and binning are not keys, W9: colour modes and filters never
//! mix), each a `stack_project_frames` handle with a `FrameOrigin::Project`
//! carrying everything a run reads about it. Reads `project_frames_local`
//! and the landed files' headers — never the catalog — and mints handles
//! (`db::stacking::project_frame_handles`, one IMMEDIATE transaction).

use std::collections::{BTreeMap, HashMap};
use std::path::Path;
use std::sync::Arc;

use rusqlite::Connection;

use crate::api::ApiError;
use crate::collab::frame_meta::{parse_project_frame_meta, ProjectFrameMeta};
use crate::collab::storage::sweep::Stamp;
use crate::db::collab_frames::{self as collab_frames, LocalFrameRow};
use crate::stacking::config::StackingConfig;
use crate::stacking::groups::{
    camera_summary, classify_exposures, f_ratio, median_frame_scale, sanitized_or, sort_groups,
    ClassInput, ColorMode, GroupFrame, IntegrationGroup, ScaleSource,
};
use crate::stacking::origin::{FrameOrigin, FrameOrigins};

/// One published, accepted frame of the project as the plan reports it
/// (spec §7's `projectFrames`): stackable or not, and why not.
#[derive(Debug, Clone)]
pub struct SourceFrame {
    /// The frame's handle — minted only for a stackable frame.
    pub handle: Option<i64>,
    pub frame_uuid: String,
    pub file_name: String,
    pub publisher: String,
    pub filter_canonical: String,
    pub exptime_sec: f64,
    pub color_mode: ColorMode,
    /// `None` when the manifest row did not parse (the frame is not stacked).
    pub meta: Option<ProjectFrameMeta>,
    /// Why the frame is not stacked on this device; `None` when it is.
    pub not_stackable: Option<String>,
}

/// A project's frames as one plan or run reads them.
pub struct ProjectSource {
    pub groups: Vec<IntegrationGroup>,
    pub origins: FrameOrigins,
    pub frames: Vec<SourceFrame>,
}

/// A project group's key (spec §8): `<mono|osc|cfa>__<canonical>__<class>`,
/// the filter token sanitized exactly as a frame set's, so the key is a
/// safe path component. No binning and no camera (W7).
pub fn project_group_key(color_token: &str, canonical: &str, class_token: &str) -> String {
    format!(
        "{color_token}__{}__{class_token}",
        sanitized_or(Some(canonical), "NoFilter")
    )
}

/// Whether a path names an XISF file (by extension, as the scanner decides).
pub fn is_xisf(path: &Path) -> bool {
    path.extension()
        .and_then(|e| e.to_str())
        .is_some_and(|e| e.eq_ignore_ascii_case("xisf"))
}

/// `geometry="w:h[:c]"` of the first `<Image` element of an XISF XML header.
pub fn xisf_geometry(xml: &str) -> Option<(usize, usize, usize)> {
    let image = &xml[xml.find("<Image")?..];
    let start = image.find("geometry=\"")? + "geometry=\"".len();
    let rest = &image[start..];
    let value = &rest[..rest.find('"')?];
    let mut parts = value.split(':').map(|s| s.trim().parse::<usize>().ok());
    let w = parts.next()??;
    let h = parts.next()??;
    let c = parts.next().flatten().unwrap_or(1);
    Some((w, h, c))
}

/// `(width, height, planes)` from a file's header alone: a FITS primary
/// image through `probe_fits` (its handle dropped at once — a run never
/// holds a landed file open), an XISF through its XML header. `None` when
/// neither reader can tell.
pub fn file_shape_quick(path: &Path) -> Option<(usize, usize, usize)> {
    if is_xisf(path) {
        let raw = match crate::fits_parser::read_xisf_header(path) {
            Ok(raw) => raw,
            Err(e) => {
                tracing::warn!(path = %path.display(), error = %format!("{e:#}"), "xisf header unreadable");
                return None;
            }
        };
        xisf_geometry(&String::from_utf8_lossy(&raw))
    } else {
        crate::integration::banded::probe_fits(path)
            .map(|(_file, info)| (info.w, info.h, info.naxis3))
    }
}

/// The colour token of a project group key (plan 6b-2 C13): `mono`; `osc`
/// for a colour frame that is three-plane after Prepare; `cfa` for one
/// that stays a one-plane mosaic (`debayerOsc` off and the file a mosaic).
/// The manifest's `bayerpat` decides colour vs mono (spec §5.1); the file is
/// probed only when `debayerOsc` is off.
fn color_token(
    project_id: &str,
    frame_uuid: &str,
    meta: &ProjectFrameMeta,
    landed: &Path,
    debayer_osc: bool,
) -> &'static str {
    if meta.bayerpat.is_none() {
        return "mono";
    }
    match osc_or_cfa(landed, debayer_osc) {
        Some(token) => token,
        None => {
            tracing::warn!(project_id, frame_uuid, path = %landed.display(), "file shape unreadable; colour token assumed cfa");
            "cfa"
        }
    }
}

/// The `osc` vs `cfa` half of the C13 rule, shared by projects and attested
/// frame sets: `osc` when the debayer is on or the file is already
/// three-plane, `cfa` for a one-plane mosaic kept as a mosaic; `None` when
/// the file's shape cannot be read (the caller warns and decides).
fn osc_or_cfa(landed: &Path, debayer_osc: bool) -> Option<&'static str> {
    if debayer_osc {
        return Some("osc");
    }
    match file_shape_quick(landed) {
        Some((_, _, 3)) => Some("osc"),
        Some(_) => Some("cfa"),
        None => None,
    }
}

/// The colour token of an attested frame set's light (spec §13, plan 6b-2
/// C13): a catalog BAYERPAT means colour (then [`osc_or_cfa`]); without one
/// the file's own header decides: three planes are `osc`, anything else
/// `mono`. An unreadable file warns and falls back to the catalog rule.
pub(crate) fn attested_color_token(
    frames_set_id: i64,
    frame_id: i64,
    catalog_bayerpat: bool,
    path: &Path,
    debayer_osc: bool,
) -> &'static str {
    if catalog_bayerpat {
        return osc_or_cfa(path, debayer_osc).unwrap_or_else(|| {
            tracing::warn!(set_id = frames_set_id, frame_id, path = %path.display(), "file shape unreadable; colour token assumed cfa");
            "cfa"
        });
    }
    match file_shape_quick(path) {
        Some((_, _, 3)) => "osc",
        Some(_) => "mono",
        None => {
            tracing::warn!(set_id = frames_set_id, frame_id, path = %path.display(), "file shape unreadable; colour token falls back to the catalog (mono)");
            "mono"
        }
    }
}

struct Member {
    frame: GroupFrame,
    canonical: String,
    color: ColorMode,
    instrume: Option<String>,
    f_ratio: Option<f64>,
}

/// The project's frame source (spec §4.3, §7, §8): every published,
/// accepted row is reported; the stackable ones (plan 6b-2 C5) get a handle,
/// a `GroupFrame` from their manifest and a `FrameOrigin::Project`, and are
/// grouped by colour token × canonical filter × exposure class. Groups sort
/// as a frame set's do (frames desc, total exposure desc, key).
pub fn project_frame_source(
    conn: &Connection,
    project_id: &str,
    cfg: &StackingConfig,
) -> Result<ProjectSource, ApiError> {
    let rows = collab_frames::list_for_project(conn, project_id)?;
    let mut frames: Vec<SourceFrame> = Vec::new();
    let mut stackable: Vec<(usize, LocalFrameRow, ProjectFrameMeta, Stamp)> = Vec::new();
    for row in rows {
        if !collab_frames::part_of_project(&row) {
            continue;
        }
        let meta = match parse_project_frame_meta(&row.manifest_json) {
            Ok(m) => Some(m),
            Err(e) => {
                tracing::warn!(project_id, frame_uuid = %row.frame_uuid, error = %format!("{e:#}"), "project frame metadata unreadable; frame left out");
                None
            }
        };
        let stamp = row.size_mtime_seen.as_deref().and_then(Stamp::parse);
        let verdict: Result<Stamp, String> = match (&meta, collab_frames::stackable(&row), stamp) {
            (None, _, _) => Err("unreadable frame metadata".to_string()),
            (Some(_), Err(reason), _) => Err(reason.reason().to_string()),
            // `stackable` already refused a NULL stamp ("waiting for its new
            // version"). A non-NULL one that does not parse is corruption,
            // which no later sync repairs (review I3).
            (Some(_), Ok(()), None) => {
                tracing::warn!(project_id, frame_uuid = %row.frame_uuid, stamp = ?row.size_mtime_seen, "landed stamp unreadable; frame left out");
                Err("unreadable size:mtime stamp".to_string())
            }
            (Some(_), Ok(()), Some(stamp)) => Ok(stamp),
        };
        let color_mode = if meta.as_ref().is_some_and(|m| m.bayerpat.is_some()) {
            ColorMode::Osc
        } else {
            ColorMode::Mono
        };
        frames.push(SourceFrame {
            handle: None,
            frame_uuid: row.frame_uuid.clone(),
            file_name: row.file_name.clone(),
            publisher: row.publisher_display.clone(),
            filter_canonical: row.filter_canonical.clone(),
            exptime_sec: meta.as_ref().map_or(0.0, |m| m.exptime_sec),
            color_mode,
            meta: meta.clone(),
            not_stackable: verdict.as_ref().err().cloned(),
        });
        if let (Ok(stamp), Some(meta)) = (verdict, meta) {
            stackable.push((frames.len() - 1, row, meta, stamp));
        }
    }

    let uuids: Vec<&str> = stackable
        .iter()
        .map(|(_, r, _, _)| r.frame_uuid.as_str())
        .collect();
    let handles = crate::db::stacking::project_frame_handles(conn, project_id, &uuids)?;

    let mut origins: HashMap<i64, FrameOrigin> = HashMap::with_capacity(stackable.len());
    let mut buckets: BTreeMap<(&'static str, String), Vec<Member>> = BTreeMap::new();
    for (idx, row, meta, stamp) in stackable {
        let handle = *handles.get(&row.frame_uuid).ok_or_else(|| {
            ApiError::Internal(format!(
                "no handle minted for project frame {}",
                row.frame_uuid
            ))
        })?;
        frames[idx].handle = Some(handle);
        let landed = row.landed_path.clone().ok_or_else(|| {
            ApiError::Internal(format!(
                "stackable frame {} has no landed path",
                row.frame_uuid
            ))
        })?;
        let (width, height) = match (meta.naxis1, meta.naxis2) {
            (Some(w), Some(h)) => (w, h),
            _ => match file_shape_quick(Path::new(&landed)) {
                Some((w, h, _)) => (w as i64, h as i64),
                None => {
                    tracing::warn!(project_id, frame_uuid = %row.frame_uuid, path = %landed, "file shape unreadable; dimensions recorded as 0x0");
                    (0, 0)
                }
            },
        };
        let token = color_token(
            project_id,
            &row.frame_uuid,
            &meta,
            Path::new(&landed),
            cfg.calibration.debayer_osc,
        );
        let frame = GroupFrame {
            frame_id: handle,
            file_id: 0,
            filename: row.file_name.clone(),
            path: landed.clone(),
            size: stamp.size as i64,
            modified_at: chrono::DateTime::<chrono::Utc>::from_timestamp(stamp.mtime, 0)
                .map(|d| d.to_rfc3339())
                .unwrap_or_default(),
            exposure_s: Some(meta.exptime_sec),
            date_obs: meta.date_obs.clone(),
            width,
            height,
            pixel_scale_arcsec: meta.pixel_scale_arcsec,
            scale_source: meta.pixel_scale_arcsec.map(|_| {
                if meta.wcs.is_some() {
                    ScaleSource::Solve
                } else {
                    ScaleSource::Header
                }
            }),
        };
        origins.insert(
            handle,
            FrameOrigin::Project {
                project_id: project_id.to_string(),
                frame_uuid: row.frame_uuid.clone(),
                publisher: row.publisher_display.clone(),
                landed_path: landed,
                blake3: row.blake3.clone(),
                byte_size: row.byte_size,
                size_mtime: stamp.encode(),
                meta: meta.clone(),
            },
        );
        buckets
            .entry((token, sanitized_or(Some(&row.filter_canonical), "NoFilter")))
            .or_default()
            .push(Member {
                frame,
                canonical: row.filter_canonical.clone(),
                color: if token == "mono" {
                    ColorMode::Mono
                } else {
                    ColorMode::Osc
                },
                instrume: meta.instrume.clone(),
                f_ratio: f_ratio(meta.focal_len, meta.aperture),
            });
    }

    let mut groups = Vec::new();
    for ((token, _filter_token), mut members) in buckets {
        members.sort_by(|a, b| {
            a.frame
                .date_obs
                .cmp(&b.frame.date_obs)
                .then(a.frame.frame_id.cmp(&b.frame.frame_id))
        });
        let inputs: Vec<ClassInput> = members
            .iter()
            .map(|m| ClassInput {
                exposure_s: m.frame.exposure_s,
                f_ratio: m.f_ratio,
            })
            .collect();
        for class in classify_exposures(&inputs, &cfg.grouping) {
            let group_frames: Vec<GroupFrame> = class
                .members
                .iter()
                .map(|&i| members[i].frame.clone())
                .collect();
            let total_exposure_s = group_frames
                .iter()
                .map(|f| f.exposure_s.unwrap_or(0.0))
                .sum();
            let pixel_scale_arcsec = median_frame_scale(&group_frames);
            let first = &members[class.members[0]];
            let (instrume, cameras) = camera_summary(
                class
                    .members
                    .iter()
                    .map(|&i| members[i].instrume.as_deref()),
            );
            groups.push(IntegrationGroup {
                key: project_group_key(token, &first.canonical, &class.token),
                instrume,
                color_mode: first.color,
                filter: Some(first.canonical.clone()),
                binning: 1,
                cameras,
                exposure_s: class.exposure_s,
                exposure_class: class.token,
                frames: group_frames,
                total_exposure_s,
                pixel_scale_arcsec,
            });
        }
    }
    sort_groups(&mut groups);

    let stackable_count = origins.len();
    tracing::debug!(
        project_id,
        count = stackable_count,
        not_stackable = frames.len() - stackable_count,
        groups = groups.len(),
        "project frame source built"
    );
    Ok(ProjectSource {
        groups,
        origins: Arc::new(origins),
        frames,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::stacking::insert_test_project;
    use crate::stacking::groups::ScaleSource;
    use crate::stacking::test_fixtures::{
        add_project_frame, manifest_wcs_json, project_frame_spec, ProjectFile,
    };

    fn test_conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        crate::db::SqliteConnectionManager::setup_connection(&c).unwrap();
        crate::db::schema::init_db(&c).unwrap();
        c
    }

    /// Spec §15.1 "Seam", Review Focus 2: catalog `frames` / `plate_solves`
    /// rows whose ids equal the handles, with different data, are never
    /// read — every field comes from the manifest and the landed file.
    #[test]
    fn the_project_frame_source_reads_the_manifest_never_the_catalog() {
        let conn = test_conn();
        insert_test_project(&conn, "p1");
        let dir = tempfile::tempdir().unwrap();
        for uuid in ["r1", "r2", "r3"] {
            let mut spec = project_frame_spec(uuid);
            spec.filter = "R";
            spec.wcs = Some(manifest_wcs_json(64, 48, 83.8, -5.4, 1.5));
            add_project_frame(&conn, dir.path(), "p1", &spec);
        }
        let handles =
            crate::db::stacking::project_frame_handles(&conn, "p1", &["r1", "r2", "r3"]).unwrap();
        for (i, h) in handles.values().enumerate() {
            conn.execute(
                "INSERT INTO files (id, path, filename, size, modified_at, format)
                 VALUES (?1, ?2, 'decoy.fits', 7, '2020-01-01T00:00:00Z', 'FITS')",
                rusqlite::params![h, format!("/decoy/{i}.fits")],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO frames (id, file_id, imagetyp, filter, exptime, naxis1, naxis2, bayerpat)
                 VALUES (?1, ?1, 'Light', 'Z', 999.0, 7, 7, 'RGGB')",
                [h],
            )
            .unwrap();
            crate::stacking::test_fixtures::seed_plate_solve_scale(&conn, *h, 9.9);
        }
        let cfg = StackingConfig::default();
        let source = project_frame_source(&conn, "p1", &cfg).unwrap();
        assert_eq!(source.groups.len(), 1, "{:?}", source.groups);
        let g = &source.groups[0];
        assert_eq!(g.frames.len(), 3);
        assert_eq!(g.key, "mono__R__300s");
        assert_eq!(
            (g.filter.as_deref(), g.binning, g.color_mode),
            (Some("R"), 1, ColorMode::Mono)
        );
        for f in &g.frames {
            assert_eq!(f.exposure_s, Some(300.0));
            assert_eq!((f.width, f.height), (64, 48));
            assert_eq!(f.pixel_scale_arcsec, Some(1.5));
            assert_eq!(f.scale_source, Some(ScaleSource::Solve));
            assert_eq!(f.file_id, 0);
            assert!(
                Path::new(&f.path).starts_with(dir.path().join("ann")),
                "{}",
                f.path
            );
            match source.origins.get(&f.frame_id).unwrap() {
                FrameOrigin::Project {
                    project_id, meta, ..
                } => {
                    assert_eq!(project_id, "p1");
                    assert!(meta.wcs.is_some());
                }
                other => panic!("{other:?}"),
            }
        }

        // The dispatchers a run reads through never touch the decoy rows.
        let mut memo = crate::stacking::plan::HashMemo::new(source.origins.clone());
        for f in &g.frames {
            let origin = source.origins.get(&f.frame_id).unwrap();
            assert_eq!(
                crate::stacking::origin::cfa_geometry(&conn, origin, f.frame_id).unwrap(),
                None,
                "the decoy row says RGGB; the landed file is mono"
            );
            let solve = crate::stacking::origin::stored_solve(&conn, origin, f.frame_id)
                .unwrap()
                .expect("the manifest WCS yields a record");
            assert_eq!(solve.pixel_scale_arcsec, 1.5, "never the decoy 9.9");
            let FrameOrigin::Project {
                blake3, byte_size, ..
            } = origin
            else {
                panic!("{origin:?}")
            };
            assert_eq!(
                memo.calibration_hash_checked(&conn, &cfg, f).unwrap(),
                crate::stacking::plan::prepare_hash_for_project(&cfg, blake3, *byte_size)
            );
        }
        let again = project_frame_source(&conn, "p1", &cfg).unwrap();
        let ids = |s: &ProjectSource| {
            s.groups[0]
                .frames
                .iter()
                .map(|f| f.frame_id)
                .collect::<Vec<_>>()
        };
        assert_eq!(ids(&source), ids(&again), "handles are stable across plans");
    }

    /// Spec §7: only stackable frames are grouped; every published, accepted
    /// frame is reported with why it is not stacked; a coordinator-excluded
    /// frame is absent altogether (R12). A landed stamp that does not parse is
    /// corruption, never "waiting for its new version" (review I3).
    #[test]
    fn only_stackable_frames_are_grouped_and_the_rest_are_reported() {
        let conn = test_conn();
        insert_test_project(&conn, "p1");
        let dir = tempfile::tempdir().unwrap();
        add_project_frame(&conn, dir.path(), "p1", &project_frame_spec("a"));
        let mut missing = project_frame_spec("b");
        missing.file = ProjectFile::Missing;
        add_project_frame(&conn, dir.path(), "p1", &missing);
        let mut excluded = project_frame_spec("c");
        excluded.accepted = false;
        add_project_frame(&conn, dir.path(), "p1", &excluded);
        add_project_frame(&conn, dir.path(), "p1", &project_frame_spec("d"));
        conn.execute(
            "UPDATE project_frames_local SET own_staged = 1 WHERE frame_uuid = 'd'",
            [],
        )
        .unwrap();
        add_project_frame(&conn, dir.path(), "p1", &project_frame_spec("e"));
        conn.execute(
            "UPDATE project_frames_local SET size_mtime_seen = 'garbage' WHERE frame_uuid = 'e'",
            [],
        )
        .unwrap();

        let source = project_frame_source(&conn, "p1", &StackingConfig::default()).unwrap();
        let grouped: Vec<&str> = source
            .groups
            .iter()
            .flat_map(|g| g.frames.iter())
            .map(|f| f.filename.as_str())
            .collect();
        assert_eq!(grouped, vec!["c_a.fits"]);
        let reported: Vec<(&str, Option<&str>, bool)> = source
            .frames
            .iter()
            .map(|f| {
                (
                    f.frame_uuid.as_str(),
                    f.not_stackable.as_deref(),
                    f.handle.is_some(),
                )
            })
            .collect();
        assert_eq!(
            reported,
            vec![
                ("a", None, true),
                ("b", Some("not on this device"), false),
                ("d", Some("being re-staged"), false),
                ("e", Some("unreadable size:mtime stamp"), false),
            ]
        );
    }

    /// Plan 6b-2 C13: colour tokens — every OSC frame is `osc` with the
    /// debayer on; with it off, a one-plane mosaic is `cfa` and an already
    /// three-plane replica stays `osc`; the two never share a group.
    #[test]
    fn colour_tokens_follow_the_plane_rule() {
        let conn = test_conn();
        insert_test_project(&conn, "p1");
        let dir = tempfile::tempdir().unwrap();
        add_project_frame(&conn, dir.path(), "p1", &project_frame_spec("m1"));
        let mut mosaic = project_frame_spec("o1");
        mosaic.file = ProjectFile::Mosaic32;
        mosaic.bayerpat = Some("RGGB");
        add_project_frame(&conn, dir.path(), "p1", &mosaic);
        let mut rgb = project_frame_spec("o2");
        rgb.file = ProjectFile::Rgb32;
        rgb.bayerpat = Some("RGGB");
        add_project_frame(&conn, dir.path(), "p1", &rgb);

        let keys = |debayer_osc: bool| {
            let mut cfg = StackingConfig::default();
            cfg.calibration.debayer_osc = debayer_osc;
            let mut keys: Vec<(String, usize)> = project_frame_source(&conn, "p1", &cfg)
                .unwrap()
                .groups
                .into_iter()
                .map(|g| (g.key, g.frames.len()))
                .collect();
            keys.sort();
            keys
        };
        assert_eq!(
            keys(true),
            vec![("mono__L__300s".into(), 1), ("osc__L__300s".into(), 2)]
        );
        assert_eq!(
            keys(false),
            vec![
                ("cfa__L__300s".into(), 1),
                ("mono__L__300s".into(), 1),
                ("osc__L__300s".into(), 1)
            ]
        );
    }

    #[test]
    fn xisf_geometry_reads_the_first_image_element() {
        let xml = r#"<xisf><Image geometry="64:48:3" sampleFormat="Float32" location="attachment:4096:36864"/></xisf>"#;
        assert_eq!(xisf_geometry(xml), Some((64, 48, 3)));
        assert_eq!(
            xisf_geometry(r#"<Image geometry="10:20"/>"#),
            Some((10, 20, 1))
        );
        assert_eq!(xisf_geometry("<xisf/>"), None);
        assert_eq!(
            project_group_key("osc", "Ha OIII", "st300"),
            "osc__Ha_OIII__st300"
        );
        assert_eq!(project_group_key("mono", "None", "all"), "mono__None__all");
    }
}
