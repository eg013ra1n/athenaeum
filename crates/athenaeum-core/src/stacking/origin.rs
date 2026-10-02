//! Where a frame of a stacking run comes from (wave 6, spec 2026-10-02
//! section 4.3). Every read of the catalog BY FRAME ID on the run path after
//! the frame source goes through here, so a frame whose id is not a catalog
//! `frames.id` can never reach a catalog query. 6b-1 has one variant; 6b-2
//! adds `Project { frame_uuid, publisher, landed_path, blake3, size_mtime,
//! manifest }`, and every `match` below then fails to compile until it has a
//! project arm (plan 6b-1 ruling B9): the compiler is the checklist.
//!
//! Not to be confused with `project_frames_local.origin` (`own | replica`),
//! which says who published a project frame, not where a run reads it from.

use std::collections::HashMap;
use std::sync::Arc;

use rusqlite::Connection;

use crate::api::ApiError;
use crate::integration::cfa::CfaGeometry;
use crate::plate_solve::storage::PlateSolveRecord;

#[derive(Debug, Clone, PartialEq)]
pub enum FrameOrigin {
    /// A catalog frame: `frames.id` is the run's frame id; `file_id` is
    /// `files.id` (the stage-1 hash's `SourceIdentity`).
    Catalog { file_id: i64 },
}

/// A run's frame id to origin map, shared by the plan, `HashMemo` and the run.
pub type FrameOrigins = Arc<HashMap<i64, FrameOrigin>>;

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
        let origin = FrameOrigin::Catalog { file_id: 0 };

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
}
