//! Where a frame of a stacking run comes from (wave 6, spec 2026-10-02
//! section 4.3). Every read of the catalog BY FRAME ID on the run path after
//! the frame source goes through here, so a frame whose id is not a catalog
//! `frames.id` can never reach a catalog query. There are two variants,
//! `Catalog` and `Project`, and every `match` on them needs both arms (plan
//! 6b-1 ruling B9): the compiler is the checklist.
//!
//! Not to be confused with `project_frames_local.origin` (`own | replica`),
//! which says who published a project frame, not where a run reads it from.
//! `db::collab_frames::FrameOrigin` (`Own | Replica`) is the other
//! `FrameOrigin`; never import both unqualified in one file.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::Connection;

use crate::api::ApiError;
use crate::collab::frame_meta::ProjectFrameMeta;
use crate::integration::cfa::CfaGeometry;
use crate::plate_solve::storage::PlateSolveRecord;
use crate::stacking::groups::GroupFrame;
use crate::stacking::solve::{stored_outcome, SolveOutcome};

#[derive(Debug, Clone, PartialEq)]
pub enum FrameOrigin {
    /// A catalog frame: `frames.id` is the run's frame id; `file_id` is
    /// `files.id` (the stage-1 hash's `SourceIdentity`). `attested` is the
    /// frame set's `calibrated_externally` (spec §13, R3): such a light is
    /// prepared, never calibrated.
    Catalog { file_id: i64, attested: bool },
    /// A collaboration project frame (spec §4.3): the run's frame id is its
    /// `stack_project_frames` handle. Everything a run reads about it is
    /// here — the landed replica (`landed_path`, its content `blake3` and
    /// `byte_size`, the `size:mtime` stamp the identity check compares
    /// against) and its manifest `meta`, parsed once. `publisher` is the
    /// publisher's display name.
    Project {
        project_id: String,
        frame_uuid: String,
        publisher: String,
        landed_path: String,
        blake3: String,
        byte_size: i64,
        size_mtime: String,
        meta: ProjectFrameMeta,
    },
}

/// Whether stage 1 PREPARES this frame instead of calibrating it (spec
/// §5.1, §13): every project frame, and an attested set's lights.
pub(crate) fn needs_prepare(origin: &FrameOrigin) -> bool {
    match origin {
        FrameOrigin::Catalog { attested, .. } => *attested,
        FrameOrigin::Project { .. } => true,
    }
}

/// Whether a frame's Prepare output stem must carry its frame id: an
/// attested catalog light's file name is not unique by itself (`X.fits`
/// debayered and `X_d.fits` converted would both be `p_X_d.fits`), while a
/// project frame's `c_<uuid>` stem is.
pub(crate) fn prepare_stem_needs_frame_id(origin: &FrameOrigin) -> bool {
    match origin {
        FrameOrigin::Catalog { .. } => true,
        FrameOrigin::Project { .. } => false,
    }
}

/// What Prepare reads for one frame, and the uuid its log line carries.
pub(crate) struct PrepareInput {
    pub(crate) path: PathBuf,
    pub(crate) frame_uuid: Option<String>,
}

/// The file Prepare reads: an attested light's catalog file (the frame
/// source's `GroupFrame.path`), or a project frame's landed replica.
pub(crate) fn prepare_input(origin: &FrameOrigin, frame: &GroupFrame) -> PrepareInput {
    match origin {
        FrameOrigin::Catalog { .. } => PrepareInput {
            path: PathBuf::from(&frame.path),
            frame_uuid: None,
        },
        FrameOrigin::Project {
            landed_path,
            frame_uuid,
            ..
        } => PrepareInput {
            path: PathBuf::from(landed_path),
            frame_uuid: Some(frame_uuid.clone()),
        },
    }
}

/// The CFA mosaic geometry a FILE's own header declares (plan 6b-2 C12):
/// BAYERPAT and XBAYROFF/YBAYROFF, never the catalog. FITS through its
/// header cards, XISF through the scanner's own reader (FITSKeyword or the
/// native `<ColorFilterArray>`). A missing offset is 0 (with a `debug!`),
/// an unrecognized pattern is no mosaic (with a `warn!`) — the rules of
/// `light_resolve::resolve_cfa_geometry`. ROWORDER is not geometry: BAYERPAT
/// describes the mosaic in file row order.
pub(crate) fn cfa_geometry_of_file(path: &Path) -> anyhow::Result<Option<CfaGeometry>> {
    let (bayerpat, xoff, yoff) = if crate::stacking::project_source::is_xisf(path) {
        let f = crate::fits_parser::parse_xisf(path, 0)?;
        (f.bayerpat, f.xbayroff, f.ybayroff)
    } else {
        let header = crate::fits_parser::FitsHeader::from_path(path)?;
        let keys = crate::fits_parser::stored_header::parse_stored_header_keys(
            crate::models::FileFormat::FITS,
            &header.to_header_text(),
        );
        let f = crate::fits_parser::stored_header::cfa_fields_from_keys(&keys);
        (f.bayerpat, f.xbayroff, f.ybayroff)
    };
    let Some(raw) = bayerpat.as_deref().map(str::trim).filter(|s| !s.is_empty()) else {
        return Ok(None);
    };
    let Some(pattern) = crate::fits_writer::keywords::Bayer::parse(raw) else {
        tracing::warn!(path = %path.display(), bayerpat = %raw, "unrecognized bayer pattern in the file's header; treated as no mosaic");
        return Ok(None);
    };
    if xoff.is_none() || yoff.is_none() {
        tracing::debug!(path = %path.display(), "cfa phase not declared in the file's header; assumes 0");
    }
    Ok(Some(CfaGeometry {
        pattern,
        xoff: xoff.unwrap_or(0),
        yoff: yoff.unwrap_or(0),
    }))
}

/// A run's frame id to origin map, shared by the plan, `HashMemo` and the run.
pub type FrameOrigins = Arc<HashMap<i64, FrameOrigin>>;

/// Why a frame leaves a stacking run — stored verbatim as its
/// `stacking_run_frames.exclusion_reason`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ExclusionReason(pub(crate) String);

/// Spec §5.1: the live exchange landed a new version (or removed the file)
/// while the run was reading it.
pub(crate) const CHANGED_DURING_THE_RUN: &str = "changed during the run";

/// The landed-file identity check (plan 6b-2 C9): a project frame's landed
/// file must still match the `size:mtime` its row recorded (2 s mtime
/// tolerance, `collab::storage::sweep`). Called at every stage boundary
/// that reads it; reads never hold a file open across stages.
pub(crate) fn landed_unchanged(origin: &FrameOrigin) -> Result<(), ExclusionReason> {
    use crate::collab::storage::sweep::{stat_verdict, Stamp, StatVerdict};
    match origin {
        FrameOrigin::Catalog { .. } => Ok(()),
        FrameOrigin::Project {
            project_id,
            frame_uuid,
            landed_path,
            size_mtime,
            ..
        } => match stat_verdict(Path::new(landed_path), Stamp::parse(size_mtime)) {
            StatVerdict::Same => Ok(()),
            StatVerdict::Unreadable(e) => {
                tracing::warn!(project_id = %project_id, frame_uuid = %frame_uuid, path = %landed_path, error = %e, "landed project frame unreadable");
                Err(ExclusionReason(format!("landed file unreadable: {e}")))
            }
            verdict => {
                let outcome = match verdict {
                    StatVerdict::Missing => "missing",
                    _ => "drifted",
                };
                tracing::warn!(project_id = %project_id, frame_uuid = %frame_uuid, path = %landed_path, outcome, "landed project frame changed during the run");
                Err(ExclusionReason(CHANGED_DURING_THE_RUN.to_string()))
            }
        },
    }
}

/// The origin of one frame of the run. A frame without one is an error: it
/// is never read as a catalog frame by default.
pub(crate) fn origin_of(origins: &FrameOrigins, frame_id: i64) -> Result<&FrameOrigin, ApiError> {
    origins.get(&frame_id).ok_or_else(|| {
        tracing::error!(frame_id, "stacking frame has no origin");
        ApiError::Internal(format!("stacking frame {frame_id} has no origin"))
    })
}

/// The stored plate solve of a frame, if any. Callers keep their own error
/// policy: Register warns and carries on, Output fails the run.
pub(crate) fn stored_solve(
    conn: &Connection,
    origin: &FrameOrigin,
    frame_id: i64,
) -> anyhow::Result<Option<PlateSolveRecord>> {
    match origin {
        FrameOrigin::Catalog { .. } => crate::plate_solve::storage::get_plate_solve(conn, frame_id),
        // Spec §5.2: the manifest's WCS first, then the run's own cached
        // solve of this content.
        FrameOrigin::Project {
            project_id,
            blake3,
            meta,
            ..
        } => match &meta.wcs {
            Some(wcs) => Ok(Some(wcs.to_record(meta.pixel_scale_arcsec))),
            None => Ok(match stored_outcome(conn, project_id, frame_id, blake3)? {
                Some(SolveOutcome::Solved(record)) => Some(record),
                _ => None,
            }),
        },
    }
}

/// What the Solve sub-step needs about a frame it must solve: a project
/// frame whose manifest carries no WCS (spec §5.2).
pub(crate) struct SolvePlan {
    pub(crate) project_id: String,
    pub(crate) frame_uuid: String,
    pub(crate) blake3: String,
    pub(crate) meta: ProjectFrameMeta,
}

/// `Some` only for a project frame without a manifest WCS. A run never
/// solves a catalog frame.
pub(crate) fn solve_plan(origin: &FrameOrigin) -> Option<SolvePlan> {
    match origin {
        FrameOrigin::Catalog { .. } => None,
        FrameOrigin::Project { meta, .. } if meta.wcs.is_some() => None,
        FrameOrigin::Project {
            project_id,
            frame_uuid,
            blake3,
            meta,
            ..
        } => Some(SolvePlan {
            project_id: project_id.clone(),
            frame_uuid: frame_uuid.clone(),
            blake3: blake3.clone(),
            meta: meta.clone(),
        }),
    }
}

/// The run's cached solve outcome for a frame's current content, which the
/// plan's "will be solved" and "could not be solved" warnings read. `None`
/// for a catalog frame, and for a frame never solved.
pub(crate) fn local_solve_outcome(
    conn: &Connection,
    origin: &FrameOrigin,
    frame_id: i64,
) -> anyhow::Result<Option<SolveOutcome>> {
    match origin {
        FrameOrigin::Catalog { .. } => Ok(None),
        FrameOrigin::Project {
            project_id, blake3, ..
        } => stored_outcome(conn, project_id, frame_id, blake3),
    }
}

/// The frame's CFA mosaic geometry (Bayer drizzle), if it has one.
pub(crate) fn cfa_geometry(
    conn: &Connection,
    origin: &FrameOrigin,
    frame_id: i64,
) -> anyhow::Result<Option<CfaGeometry>> {
    match origin {
        FrameOrigin::Catalog { .. } => {
            crate::calibration_library::light_resolve::resolve_cfa_geometry(conn, frame_id)
        }
        FrameOrigin::Project { landed_path, .. } => cfa_geometry_of_file(Path::new(landed_path)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::stacking::test_fixtures::{self, LightSpec};

    fn light_spec(stem: &str) -> LightSpec<'_> {
        LightSpec {
            stem,
            instrume: "cam",
            filter: Some("L"),
            binning: 1,
            width: 64,
            height: 48,
            exptime: 60.0,
            date_obs: "2025-01-01T20:00:00",
            bayerpat: Some("RGGB"),
            write_file: false,
        }
    }

    #[test]
    fn a_frame_without_an_origin_is_an_error_not_a_catalog_read() {
        let origins: FrameOrigins = Arc::new(HashMap::new());
        assert!(origin_of(&origins, 5).is_err());
    }

    #[test]
    fn the_catalog_arms_read_what_they_read_today() {
        let f = test_fixtures::frame_set("LDN 1272");
        let (frame_id, _) = test_fixtures::add_light(&f, &light_spec("a"));
        let (bare_id, _) = test_fixtures::add_light(&f, &light_spec("b"));
        test_fixtures::seed_plate_solve_scale(&f.conn, frame_id, 1.5);
        let origin = FrameOrigin::Catalog {
            file_id: 0,
            attested: false,
        };

        let via_origin = stored_solve(&f.conn, &origin, frame_id).unwrap();
        let direct = crate::plate_solve::storage::get_plate_solve(&f.conn, frame_id).unwrap();
        assert!(via_origin.is_some());
        assert_eq!(format!("{via_origin:?}"), format!("{direct:?}"));
        assert!(stored_solve(&f.conn, &origin, bare_id).unwrap().is_none());

        for id in [frame_id, bare_id] {
            let via_origin = cfa_geometry(&f.conn, &origin, id).unwrap();
            let direct =
                crate::calibration_library::light_resolve::resolve_cfa_geometry(&f.conn, id)
                    .unwrap();
            assert_eq!(format!("{via_origin:?}"), format!("{direct:?}"));
        }
    }

    /// Plan 6b-2 C12: CFA geometry from the file's own header — never the
    /// catalog — with an odd phase preserved.
    #[test]
    fn cfa_geometry_comes_from_the_files_own_header() {
        use crate::fits_writer::keywords::Bayer;
        use crate::fits_writer::{write_fits_f32, Card, CardValue};
        let dir = tempfile::tempdir().unwrap();
        let mosaic = dir.path().join("m.fits");
        let cards = vec![
            Card::new("BAYERPAT", CardValue::Str("GRBG".into())).unwrap(),
            Card::new("XBAYROFF", CardValue::Integer(1)).unwrap(),
            Card::new("YBAYROFF", CardValue::Integer(0)).unwrap(),
        ];
        write_fits_f32(&mosaic, 8, 8, 1, &vec![0.1; 64], &cards).unwrap();
        assert_eq!(
            cfa_geometry_of_file(&mosaic).unwrap(),
            Some(CfaGeometry {
                pattern: Bayer::Grbg,
                xoff: 1,
                yoff: 0
            })
        );
        let mono = dir.path().join("n.fits");
        write_fits_f32(&mono, 8, 8, 1, &vec![0.1; 64], &[]).unwrap();
        assert_eq!(cfa_geometry_of_file(&mono).unwrap(), None);
    }

    /// The XISF branch of `cfa_geometry_of_file`: the same header fields,
    /// read through the scanner's own XISF reader.
    #[test]
    fn cfa_geometry_of_an_xisf_file_reads_its_keywords() {
        use crate::fits_writer::keywords::Bayer;
        use crate::fits_writer::{write_xisf_f32, Card, CardValue};
        let dir = tempfile::tempdir().unwrap();
        let mosaic = dir.path().join("m.xisf");
        let cards = vec![
            Card::new("BAYERPAT", CardValue::Str("GRBG".into())).unwrap(),
            Card::new("XBAYROFF", CardValue::Integer(1)).unwrap(),
            Card::new("YBAYROFF", CardValue::Integer(0)).unwrap(),
        ];
        write_xisf_f32(&mosaic, 8, 8, 1, &vec![0.1; 64], &cards).unwrap();
        assert_eq!(
            cfa_geometry_of_file(&mosaic).unwrap(),
            Some(CfaGeometry {
                pattern: Bayer::Grbg,
                xoff: 1,
                yoff: 0
            })
        );
        let mono = dir.path().join("n.xisf");
        write_xisf_f32(&mono, 8, 8, 1, &vec![0.1; 64], &[]).unwrap();
        assert_eq!(cfa_geometry_of_file(&mono).unwrap(), None);
    }

    /// Plan 6b-2 C8: no function in the production half of `run.rs` or
    /// `plan.rs` names a `StackSubject` or `FrameOrigin` variant outside the
    /// named funnels and the allowlist — so a third subject, or a later
    /// trait extraction, touches exactly these functions.
    #[test]
    fn subject_and_origin_variants_stay_in_their_funnels() {
        const FUNNELS: &[&str] = &[
            "frame_source",
            "subject_layout",
            "subject_label",
            "stored_reference_frame",
            "hash_by_origin",
            "stored_solve",
            "cfa_geometry",
            "resolve_reference",
            "resolve_calibrate_job",
            "stored_registrations",
            "store_registration",
            "stage_one_kind",
            "landed_unchanged",
        ];
        const ALLOWLIST: &[(&str, &str)] = &[
            (
                "heal_interrupted_runs",
                "log-only: the frame-set and project heal lines carry different fields",
            ),
            (
                "start_stacking",
                "frame-set-only guard: a project start is refused until 6b-3",
            ),
            (
                "build_plan",
                "the subject dispatch to the two plan arms, and its per-subject log line",
            ),
            (
                "test_context",
                "#[cfg(test)] constructor of a frame-set run",
            ),
            (
                "project_test_context",
                "#[cfg(test)] constructor of a project run",
            ),
        ];
        for (file, text) in [
            ("run.rs", include_str!("run.rs")),
            ("plan.rs", include_str!("plan.rs")),
        ] {
            let production = text
                .split("\nmod tests {")
                .next()
                .expect("split yields a first part");
            let allowed: Vec<&str> = FUNNELS
                .iter()
                .copied()
                .chain(ALLOWLIST.iter().map(|(f, _)| *f))
                .collect();
            let violations = variant_violations(production, &allowed);
            assert!(
                violations.is_empty(),
                "{file}: a function names a subject/origin variant outside the funnels: {violations:?}"
            );
        }
    }

    /// Plan 6b-2 C9: a catalog frame always passes; a project frame passes
    /// while its landed file matches the recorded stamp, and is excluded
    /// "changed during the run" once rewritten or removed.
    #[test]
    fn landed_unchanged_follows_the_recorded_stamp() {
        let catalog = FrameOrigin::Catalog {
            file_id: 1,
            attested: false,
        };
        assert_eq!(landed_unchanged(&catalog), Ok(()));

        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        crate::db::stacking::insert_test_project(&conn, "p1");
        let dir = tempfile::tempdir().unwrap();
        let path = test_fixtures::add_project_frame(
            &conn,
            dir.path(),
            "p1",
            &test_fixtures::project_frame_spec("a"),
        );
        let source = crate::stacking::project_source::project_frame_source(
            &conn,
            "p1",
            &crate::stacking::config::StackingConfig::default(),
        )
        .unwrap();
        let origin = source.origins.values().next().unwrap().clone();
        assert_eq!(landed_unchanged(&origin), Ok(()));
        std::fs::write(&path, vec![0u8; 4321]).unwrap();
        assert_eq!(
            landed_unchanged(&origin),
            Err(ExclusionReason(CHANGED_DURING_THE_RUN.to_string()))
        );
        std::fs::remove_file(&path).unwrap();
        assert_eq!(
            landed_unchanged(&origin),
            Err(ExclusionReason(CHANGED_DURING_THE_RUN.to_string()))
        );
    }

    /// The scanner behind the funnel test: `(line number, enclosing fn, line)`
    /// for every non-comment line naming a `StackSubject::` or
    /// `FrameOrigin::` variant whose enclosing function is not in `allowed`.
    /// A column-0 `}` ends the enclosing function (an item between two
    /// functions has none).
    fn variant_violations(source: &str, allowed: &[&str]) -> Vec<(usize, String, String)> {
        let mut out = Vec::new();
        let mut current_fn = String::new();
        for (n, line) in source.lines().enumerate() {
            if line.starts_with('}') {
                current_fn.clear();
            }
            if let Some(name) = fn_name(line) {
                current_fn = name;
            }
            if line.trim_start().starts_with("//") {
                continue;
            }
            if (line.contains("StackSubject::") || line.contains("FrameOrigin::"))
                && !allowed.contains(&current_fn.as_str())
            {
                out.push((n + 1, current_fn.clone(), line.trim().to_string()));
            }
        }
        out
    }

    /// The funnel scanner catches the three shapes a naive scan misses.
    #[test]
    fn the_funnel_scanner_catches_its_three_blind_spots() {
        let allowed = ["funnel"];
        // 1. A use after the funnel closed at column 0, outside any fn.
        let after_close =
            "fn funnel() {\n    let a = 1;\n}\nconst X: () = { let _ = FrameOrigin::Project; };\n";
        assert_eq!(variant_violations(after_close, &allowed).len(), 1);
        // 2. A restricted-visibility fn.
        let restricted = "pub(super) fn stray() {\n    let _ = StackSubject::Project;\n}\n";
        assert_eq!(variant_violations(restricted, &allowed)[0].1, "stray");
        let in_path = "pub(in crate::stacking) fn stray2() {\n    FrameOrigin::Catalog;\n}\n";
        assert_eq!(variant_violations(in_path, &allowed)[0].1, "stray2");
        // 3. An attribute on the same line as `fn`.
        let attr = "#[inline] fn stray3() {\n    FrameOrigin::Catalog;\n}\n";
        assert_eq!(variant_violations(attr, &allowed)[0].1, "stray3");
        // And the allowed shapes stay quiet.
        let ok = "pub(crate) fn funnel() {\n    FrameOrigin::Catalog;\n}\n";
        assert!(variant_violations(ok, &allowed).is_empty());
    }

    /// The name of the function a source line declares, if it declares one.
    /// Leading attributes (`#[...]`), any `pub(...)` visibility and the
    /// `async`/`const`/`unsafe` qualifiers are skipped.
    fn fn_name(line: &str) -> Option<String> {
        let mut t = line.trim_start();
        loop {
            let before = t;
            if t.starts_with("#[") {
                t = t[t.find(']')? + 1..].trim_start();
            }
            if let Some(rest) = t.strip_prefix("pub(") {
                t = rest[rest.find(')')? + 1..].trim_start();
            }
            for prefix in ["pub ", "async ", "const ", "unsafe "] {
                if let Some(rest) = t.strip_prefix(prefix) {
                    t = rest;
                }
            }
            if t == before {
                break;
            }
        }
        let rest = t.strip_prefix("fn ")?;
        let name: String = rest
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        (!name.is_empty()).then_some(name)
    }

    /// Plan 6b-2 C14: a project frame's WCS. The manifest's comes first, then
    /// the run's own cached solve of THIS content (a solve of other content is
    /// never served). The test also covers which frames the Solve sub-step
    /// takes, and the outcome the plan reads.
    #[test]
    fn a_project_frames_wcs_is_the_manifests_then_the_runs_own_solve() {
        use crate::stacking::solve::{write_outcome, FakeSolver, SolveOutcome};
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        crate::db::stacking::insert_test_project(&conn, "p1");
        let dir = tempfile::tempdir().unwrap();
        test_fixtures::add_project_frame(
            &conn,
            dir.path(),
            "p1",
            &test_fixtures::project_frame_spec("a"),
        );
        let mut with_wcs = test_fixtures::project_frame_spec("m");
        with_wcs.wcs = Some(test_fixtures::manifest_wcs_json(64, 48, 10.0, 20.0, 1.5));
        test_fixtures::add_project_frame(&conn, dir.path(), "p1", &with_wcs);
        let source = crate::stacking::project_source::project_frame_source(
            &conn,
            "p1",
            &crate::stacking::config::StackingConfig::default(),
        )
        .unwrap();
        let of = |uuid: &str| -> (i64, FrameOrigin, String) {
            source
                .origins
                .iter()
                .find_map(|(id, o)| match o {
                    FrameOrigin::Project {
                        frame_uuid, blake3, ..
                    } if frame_uuid == uuid => Some((*id, o.clone(), blake3.clone())),
                    _ => None,
                })
                .unwrap()
        };
        let (a_id, a, a_blake3) = of("a");
        let (m_id, m, _) = of("m");

        assert!(stored_solve(&conn, &a, a_id).unwrap().is_none());
        assert!(solve_plan(&a).is_some_and(|p| p.frame_uuid == "a" && p.blake3 == a_blake3));
        assert!(local_solve_outcome(&conn, &a, a_id).unwrap().is_none());

        write_outcome(
            &conn,
            "p1",
            a_id,
            &a_blake3,
            "h",
            SolveOutcome::Solved(FakeSolver::fixture_record()),
        )
        .unwrap();
        assert_eq!(stored_solve(&conn, &a, a_id).unwrap().unwrap().crval1, 83.8);
        assert!(matches!(
            local_solve_outcome(&conn, &a, a_id).unwrap(),
            Some(SolveOutcome::Solved(_))
        ));

        write_outcome(
            &conn,
            "p1",
            a_id,
            "other-content",
            "h",
            SolveOutcome::Solved(FakeSolver::fixture_record()),
        )
        .unwrap();
        assert!(
            stored_solve(&conn, &a, a_id).unwrap().is_none(),
            "a solve of other content is not served"
        );

        assert_eq!(stored_solve(&conn, &m, m_id).unwrap().unwrap().crval1, 10.0);
        assert!(solve_plan(&m).is_none());

        let catalog = FrameOrigin::Catalog {
            file_id: 1,
            attested: false,
        };
        assert!(solve_plan(&catalog).is_none());
        assert!(local_solve_outcome(&conn, &catalog, 1).unwrap().is_none());
    }
}
