//! The aligned frames of a stacking run, for Blink (spec 2026-11-06 §5 and §6.7).
//!
//! Stage 5 writes one registered ("aligned") file per frame, resampled into the reference geometry, to
//! `<working_dir>/<subject>/registered/<group_key>/r_<stem>[_d].fits` and records it in `stacking_artifacts` (kind
//! `registered`). The file belongs to the subject's group and frame, not to a run: every later run of the group
//! overwrites the file and its row. So a run's aligned frame is offered only while that run is the latest one to hold
//! the group, and its location is the artifact row's absolute `path` - never a path rebuilt from the subject's current
//! name, which a rename changes.
//!
//! Two handlers: [`get_stacking_aligned_frames`] says, per frame, whether the aligned frame is available and, if not,
//! why; [`resolve_stacking_aligned_path`] gives the file to render. The hosts render it with the same pipeline as the
//! original preview (NaN outside the coverage renders black).

use std::collections::HashMap;
use std::path::{Path, PathBuf};

use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

use crate::api::{db, ApiError};
use crate::db::stacking::{find_artifact, get_run, StackSubject, StackingRunRow};
use crate::services::ServiceContext;
use crate::stacking::plan::registered_status;

/// Whether a run's aligned frame can be shown for one frame, and why not when it cannot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum AlignedState {
    /// The aligned file is on disk and belongs to this run.
    Available,
    /// The run tried to register the frame and failed.
    RegistrationFailed,
    /// The run never registered the frame: it was cut before Register, or it is not in the run at all.
    NotRegistered,
    /// The run registered the frame but its aligned file is gone (cleanup, a deleted working folder).
    Missing,
    /// A later run of the same group overwrote the aligned file.
    Replaced,
}

impl AlignedState {
    /// The reason a handler gives when asked for the file of a frame that has none.
    fn reason(self) -> &'static str {
        match self {
            AlignedState::Available => "available",
            AlignedState::RegistrationFailed => "registration failed",
            AlignedState::NotRegistered => "not registered",
            AlignedState::Missing => "the file is missing",
            AlignedState::Replaced => "a later run replaced it",
        }
    }
}

/// One frame's answer. `frame_id` is the id the caller asked with: a catalog frame id for a frame set, a project-frame
/// handle for a project (the same id `StackingRunFrameRow::frame_id` carries).
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct AlignedFrameStatus {
    pub frame_id: i64,
    pub state: AlignedState,
}

struct Resolved {
    frame_id: i64,
    state: AlignedState,
    /// `Some` exactly when `state` is `Available`.
    path: Option<PathBuf>,
}

impl Resolved {
    fn without_file(frame_id: i64, state: AlignedState) -> Self {
        Resolved {
            frame_id,
            state,
            path: None,
        }
    }
}

/// What a run recorded about one frame: the group it sat in and how Register went.
struct RunFrame {
    group_key: String,
    reg_status: Option<String>,
}

/// The run's row for `frame_id`, joined to its group. `None` when the run holds no such frame.
fn run_frame(
    conn: &Connection,
    subject: &StackSubject,
    run_id: i64,
    frame_id: i64,
) -> Result<Option<RunFrame>, ApiError> {
    let column = match subject {
        StackSubject::FrameSet(_) => "f.frame_id",
        StackSubject::Project(_) => "f.project_frame",
    };
    let sql = format!(
        "SELECT g.group_key, f.reg_status FROM stacking_run_frames f \
         JOIN stacking_run_groups g ON g.id = f.group_id \
         WHERE f.run_id = ?1 AND {column} = ?2"
    );
    Ok(conn
        .prepare_cached(&sql)?
        .query_row(params![run_id, frame_id], |r| {
            Ok(RunFrame {
                group_key: r.get(0)?,
                reg_status: r.get(1)?,
            })
        })
        .optional()?)
}

/// Whether a later run of the run's subject holds `group_key`: its group row exists from the run's first moment, so a
/// later run that got no further than planning counts too (the safe side: never offer pixels a later run may have
/// replaced).
fn group_is_held_by_a_later_run(
    conn: &Connection,
    run: &StackingRunRow,
    group_key: &str,
) -> Result<bool, ApiError> {
    Ok(conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM stacking_runs r \
         JOIN stacking_run_groups g ON g.run_id = r.id \
         WHERE r.id > ?1 AND r.frames_set_id IS ?2 AND r.project_id IS ?3 AND g.group_key = ?4)",
        params![run.id, run.frames_set_id, run.project_id, group_key],
        |r| r.get(0),
    )?)
}

/// Whether `path` is a file on this device. A path that cannot be read for another reason than being absent is logged
/// and counts as missing.
fn file_exists(path: &Path, run_id: i64, frame_id: i64) -> bool {
    match std::fs::metadata(path) {
        Ok(meta) => meta.is_file(),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => false,
        Err(e) => {
            tracing::warn!(
                run_id,
                frame_id,
                path = %path.display(),
                error = %e,
                "aligned frame file cannot be read"
            );
            false
        }
    }
}

/// The state of each requested frame, in the request's order.
///
/// Decided in this order:
/// 1. the run holds no row for the frame: `NotRegistered`;
/// 2. its `reg_status` is `failed`: `RegistrationFailed`;
/// 3. a later run of the subject holds the frame's group: `Replaced`;
/// 4. the run registered it (`aligned`, `aligned_flipped` or `reference`: [`registered_status`]) and its `registered`
///    artifact row names a file on disk: `Available` - whether or not the frame is still in the stack, since a frame
///    the weight floor cut after Register keeps its aligned file and is worth blinking; no file: `Missing`;
/// 5. anything else (`skipped`: cut before Register): `NotRegistered`. Such a frame may still have an artifact row, from
///    an earlier run that registered it; that file is not this run's.
fn resolve(conn: &Connection, run_id: i64, frame_ids: &[i64]) -> Result<Vec<Resolved>, ApiError> {
    let run = get_run(conn, run_id)?
        .ok_or_else(|| ApiError::NotFound(format!("stacking run {run_id} not found")))?;
    let subject = run.subject();
    let mut held_by_a_later_run: HashMap<String, bool> = HashMap::new();
    let mut out = Vec::with_capacity(frame_ids.len());
    for &frame_id in frame_ids {
        let Some(row) = run_frame(conn, &subject, run_id, frame_id)? else {
            out.push(Resolved::without_file(
                frame_id,
                AlignedState::NotRegistered,
            ));
            continue;
        };
        let reg_status = row.reg_status.as_deref();
        if reg_status == Some("failed") {
            out.push(Resolved::without_file(
                frame_id,
                AlignedState::RegistrationFailed,
            ));
            continue;
        }
        let replaced = match held_by_a_later_run.get(&row.group_key) {
            Some(&known) => known,
            None => {
                let held = group_is_held_by_a_later_run(conn, &run, &row.group_key)?;
                held_by_a_later_run.insert(row.group_key.clone(), held);
                held
            }
        };
        if replaced {
            out.push(Resolved::without_file(frame_id, AlignedState::Replaced));
            continue;
        }
        if !reg_status.is_some_and(registered_status) {
            out.push(Resolved::without_file(
                frame_id,
                AlignedState::NotRegistered,
            ));
            continue;
        }
        let path = find_artifact(conn, &subject, &row.group_key, "registered", Some(frame_id))?
            .and_then(|artifact| artifact.path)
            .map(PathBuf::from)
            .filter(|p| file_exists(p, run_id, frame_id));
        out.push(match path {
            Some(path) => Resolved {
                frame_id,
                state: AlignedState::Available,
                path: Some(path),
            },
            None => Resolved::without_file(frame_id, AlignedState::Missing),
        });
    }
    Ok(out)
}

/// For each frame of a run (ids in the run's namespace: catalog frame ids for a frame set, project-frame handles for a
/// project), whether its aligned frame is available. One call per Blink open.
pub fn get_stacking_aligned_frames(
    ctx: &ServiceContext,
    run_id: i64,
    frame_ids: &[i64],
) -> Result<Vec<AlignedFrameStatus>, ApiError> {
    let db_handle = db(ctx)?;
    let conn = db_handle.conn();
    Ok(resolve(&conn, run_id, frame_ids)?
        .into_iter()
        .map(|r| AlignedFrameStatus {
            frame_id: r.frame_id,
            state: r.state,
        })
        .collect())
}

/// The aligned file of one frame of a run, to render: the artifact row's own `path`. An error unless the frame's state
/// is `Available`.
pub fn resolve_stacking_aligned_path(
    ctx: &ServiceContext,
    run_id: i64,
    frame_id: i64,
) -> Result<PathBuf, ApiError> {
    let db_handle = db(ctx)?;
    let conn = db_handle.conn();
    let resolved = resolve(&conn, run_id, &[frame_id])?
        .pop()
        .ok_or_else(|| ApiError::Internal("aligned frame lookup returned nothing".into()))?;
    match resolved.path {
        Some(path) if resolved.state == AlignedState::Available => Ok(path),
        _ => Err(ApiError::NotFound(format!(
            "frame {frame_id} has no aligned frame in stacking run {run_id}: {}",
            resolved.state.reason()
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Arc;

    use crate::api::PathPolicy;
    use crate::db::stacking::{
        get_run, insert_group, insert_run, list_frame_rows, list_groups, project_frame_handles,
        upsert_artifact, upsert_frame_row, NewArtifact, NewFrameRow, NewGroup, NewRun,
    };
    use crate::events::{NullEmitter, ProgressEmitter};
    use crate::stacking::config::{CleanupPolicy, ExposureMode, StackingConfig};
    use crate::stacking::test_fixtures::{
        self, date_obs_at, seed_star_group, star_light_spec, wait_for_run, STAR_FIELD_HEIGHT,
        STAR_FIELD_WIDTH,
    };
    use crate::stacking::weights::WeightMode;

    const SET_NAME: &str = "LDN 1272";
    const FOUR_SHIFTS: [(f64, f64); 4] = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    /// Four clean frames and a fifth with three times their noise. Under the noise weight the clean frames land
    /// between 0.4 and 1.0 of the best (their noise estimates differ with the sub), the noisy one at 0.2: under
    /// [`CUT_FLOOR`], over the default floor (0.05).
    const NOISY: [f32; 5] = [4.0, 4.0, 4.0, 4.0, 12.0];
    const CUT_FLOOR: f64 = 0.3;

    /// A frame set of star-field lights with their master dark and flat linked, and the context that drives real runs
    /// on it.
    struct Seeded {
        _tmp: tempfile::TempDir,
        ctx: Arc<ServiceContext>,
        fixture: test_fixtures::Fixture,
        /// The star frames first, then any starless flat-field frames.
        ids: Vec<i64>,
        _working: tempfile::TempDir,
        _output: tempfile::TempDir,
    }

    /// `noise.len()` star frames shifted by [`FOUR_SHIFTS`] (cycled), and `starless` flat-field frames after them: a
    /// constant background and noise, no stars, so registration cannot match them.
    fn seeded(noise: &[f32], starless: usize) -> Seeded {
        let tmp = tempfile::tempdir().unwrap();
        let db_path = tmp.path().join("catalog.db");
        let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
        let shifts: Vec<(f64, f64)> = (0..noise.len())
            .map(|i| FOUR_SHIFTS[i % FOUR_SHIFTS.len()])
            .collect();
        let (fixture, mut ids, working, output) =
            seed_star_group(&db_path, SET_NAME, &shifts, noise);
        for k in 0..starless {
            let date_obs = date_obs_at(noise.len() + k);
            let stem = format!("flat{k}");
            let spec = star_light_spec(&stem, &date_obs);
            let (id, _) = test_fixtures::add_light_with_field(
                &fixture,
                &spec,
                &[],
                600.0,
                4.0,
                777 + k as u64,
            );
            ids.push(id);
        }
        test_fixtures::add_master_dark_and_flat(
            &fixture,
            &ids,
            STAR_FIELD_WIDTH,
            STAR_FIELD_HEIGHT,
        );
        Seeded {
            _tmp: tmp,
            ctx,
            fixture,
            ids,
            _working: working,
            _output: output,
        }
    }

    fn config() -> StackingConfig {
        let mut cfg = StackingConfig::default();
        cfg.output.cleanup = CleanupPolicy::KeepAll;
        cfg
    }

    /// [`config`] with a weight mode that is not in the PSF family: the weight floor then runs at Measure, before
    /// Register, where the PSF family defers it to the end of Register (`weights::defers_weight_gate`).
    fn config_with_measure_time_floor(min_weight_fraction: Option<f64>) -> StackingConfig {
        let mut cfg = config();
        cfg.measurement.weight_mode = WeightMode::Noise;
        if let Some(fraction) = min_weight_fraction {
            cfg.selection.min_weight_fraction = fraction;
        }
        cfg
    }

    /// A real run of the set, finished and `done`.
    fn run_once(s: &Seeded, cfg: &StackingConfig) -> i64 {
        let started = crate::stacking::run::start_stacking(
            s.ctx.clone(),
            Arc::new(NullEmitter) as Arc<dyn ProgressEmitter>,
            &PathPolicy::AllowAll,
            "test".to_string(),
            StackSubject::FrameSet(s.fixture.set_id),
            Some(cfg.clone()),
            None,
        )
        .expect("the run starts");
        wait_for_run(&s.ctx, started.run_id);
        let row = get_run(&s.fixture.conn, started.run_id).unwrap().unwrap();
        assert_eq!(row.status, "done", "{row:?}");
        started.run_id
    }

    fn states(s: &Seeded, run_id: i64) -> Vec<AlignedState> {
        get_stacking_aligned_frames(&s.ctx, run_id, &s.ids)
            .unwrap()
            .into_iter()
            .map(|r| r.state)
            .collect()
    }

    /// Each frame's recorded weight and exclusion reason, for an assertion message.
    fn weights(s: &Seeded, run_id: i64) -> Vec<(i64, Option<f64>, Option<String>)> {
        list_frame_rows(&s.fixture.conn, run_id)
            .unwrap()
            .into_iter()
            .map(|r| (r.frame_id, r.weight, r.exclusion_reason))
            .collect()
    }

    fn run_frame_row(
        s: &Seeded,
        run_id: i64,
        frame_id: i64,
    ) -> crate::db::stacking::StackingRunFrameRow {
        list_frame_rows(&s.fixture.conn, run_id)
            .unwrap()
            .into_iter()
            .find(|r| r.frame_id == frame_id)
            .unwrap_or_else(|| panic!("run {run_id} holds no row for frame {frame_id}"))
    }

    fn registered_artifact(
        s: &Seeded,
        run_id: i64,
        frame_id: i64,
    ) -> Option<crate::db::stacking::StackingArtifactRow> {
        let group_key = list_groups(&s.fixture.conn, run_id).unwrap()[0]
            .group_key
            .clone();
        find_artifact(
            &s.fixture.conn,
            &StackSubject::FrameSet(s.fixture.set_id),
            &group_key,
            "registered",
            Some(frame_id),
        )
        .unwrap()
    }

    #[test]
    fn aligned_available_after_run() {
        let s = seeded(&[4.0; 4], 0);
        let run_id = run_once(&s, &config());
        assert_eq!(states(&s, run_id), vec![AlignedState::Available; 4]);

        // The file is the artifact row's own, and it is a registered frame.
        let path = resolve_stacking_aligned_path(&s.ctx, run_id, s.ids[1]).unwrap();
        let artifact = registered_artifact(&s, run_id, s.ids[1]).unwrap();
        assert_eq!(Some(path.to_str().unwrap()), artifact.path.as_deref());
        assert!(path.is_file(), "{path:?}");
        assert!(
            path.file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .starts_with("r_"),
            "{path:?}"
        );

        // The answer follows the request: its order, a frame the run does not hold, an unknown run.
        let asked = [s.ids[2], 987_654, s.ids[0]];
        let got = get_stacking_aligned_frames(&s.ctx, run_id, &asked).unwrap();
        assert_eq!(
            got.iter()
                .map(|r| (r.frame_id, r.state))
                .collect::<Vec<_>>(),
            vec![
                (s.ids[2], AlignedState::Available),
                (987_654, AlignedState::NotRegistered),
                (s.ids[0], AlignedState::Available),
            ]
        );
        assert!(matches!(
            get_stacking_aligned_frames(&s.ctx, run_id + 100, &asked),
            Err(ApiError::NotFound(_))
        ));
        assert!(matches!(
            resolve_stacking_aligned_path(&s.ctx, run_id, 987_654),
            Err(ApiError::NotFound(_))
        ));
    }

    #[test]
    fn aligned_failed_frame_reports_registration_failed() {
        let s = seeded(&[4.0; 4], 1);
        let flat = s.ids[4];
        // The same weight for every frame, so the starless frame reaches Register and fails there rather than being
        // cut by weight first.
        let mut cfg = config();
        cfg.measurement.weight_mode = WeightMode::None;
        let run_id = run_once(&s, &cfg);

        assert_eq!(
            run_frame_row(&s, run_id, flat).reg_status.as_deref(),
            Some("failed")
        );
        let mut expected = vec![AlignedState::Available; 4];
        expected.push(AlignedState::RegistrationFailed);
        assert_eq!(states(&s, run_id), expected);
        assert!(matches!(
            resolve_stacking_aligned_path(&s.ctx, run_id, flat),
            Err(ApiError::NotFound(m)) if m.contains("registration failed")
        ));
    }

    #[test]
    fn aligned_weight_cut_reports_not_registered() {
        let s = seeded(&NOISY, 0);
        let noisy = s.ids[4];
        let run_id = run_once(&s, &config_with_measure_time_floor(Some(CUT_FLOOR)));

        // Cut at the Measure-time weight gate, before Register: skipped, and no aligned file was ever written.
        let row = run_frame_row(&s, run_id, noisy);
        assert!(!row.included, "{row:?}");
        assert_eq!(row.reg_status.as_deref(), Some("skipped"), "{row:?}");
        assert!(registered_artifact(&s, run_id, noisy).is_none());

        let mut expected = vec![AlignedState::Available; 4];
        expected.push(AlignedState::NotRegistered);
        assert_eq!(states(&s, run_id), expected, "{:?}", weights(&s, run_id));
    }

    /// The PSF family (the default) cuts by the weight floor after Register, on the frame-common weight: the frame is
    /// out of the stack but was registered, its aligned file is on disk, and it is worth blinking.
    #[test]
    fn aligned_available_after_post_register_weight_cut() {
        let s = seeded(&NOISY, 0);
        let noisy = s.ids[4];
        let mut cfg = config();
        cfg.selection.min_weight_fraction = CUT_FLOOR;
        let run_id = run_once(&s, &cfg);

        let row = run_frame_row(&s, run_id, noisy);
        assert!(!row.included, "{row:?}");
        assert!(
            row.exclusion_reason
                .as_deref()
                .is_some_and(|r| r.contains("below 0.30 of the group maximum")),
            "{row:?}"
        );
        assert_eq!(row.reg_status.as_deref(), Some("aligned"), "{row:?}");
        assert!(registered_artifact(&s, run_id, noisy).is_some());

        assert_eq!(states(&s, run_id), vec![AlignedState::Available; 5]);
        assert!(resolve_stacking_aligned_path(&s.ctx, run_id, noisy)
            .unwrap()
            .is_file());
    }

    #[test]
    fn aligned_missing_when_file_deleted() {
        let s = seeded(&[4.0; 4], 0);
        let run_id = run_once(&s, &config());
        let path = resolve_stacking_aligned_path(&s.ctx, run_id, s.ids[2]).unwrap();
        std::fs::remove_file(&path).unwrap();

        let mut expected = vec![AlignedState::Available; 4];
        expected[2] = AlignedState::Missing;
        assert_eq!(states(&s, run_id), expected);
        assert!(matches!(
            resolve_stacking_aligned_path(&s.ctx, run_id, s.ids[2]),
            Err(ApiError::NotFound(m)) if m.contains("missing")
        ));
    }

    #[test]
    fn aligned_replaced_by_later_run_of_group() {
        let s = seeded(&[4.0; 4], 0);
        let first = run_once(&s, &config());
        assert_eq!(states(&s, first), vec![AlignedState::Available; 4]);

        let second = run_once(&s, &config());
        assert_eq!(states(&s, first), vec![AlignedState::Replaced; 4]);
        assert_eq!(states(&s, second), vec![AlignedState::Available; 4]);
        assert!(matches!(
            resolve_stacking_aligned_path(&s.ctx, first, s.ids[0]),
            Err(ApiError::NotFound(m)) if m.contains("later run")
        ));
        assert!(resolve_stacking_aligned_path(&s.ctx, second, s.ids[0]).is_ok());
    }

    #[test]
    fn aligned_older_run_kept_when_later_run_lacks_group() {
        let s = seeded(&[4.0; 4], 0);
        let first = run_once(&s, &config());
        // Another exposure grouping names another group, so the second run writes elsewhere and holds no group of the
        // first one's.
        let mut other = config();
        other.grouping.exposure_mode = ExposureMode::NoSplit;
        let second = run_once(&s, &other);
        let key_of = |run_id| {
            list_groups(&s.fixture.conn, run_id).unwrap()[0]
                .group_key
                .clone()
        };
        assert_ne!(key_of(first), key_of(second));

        assert_eq!(states(&s, first), vec![AlignedState::Available; 4]);
        assert_eq!(states(&s, second), vec![AlignedState::Available; 4]);
        let old = resolve_stacking_aligned_path(&s.ctx, first, s.ids[0]).unwrap();
        let new = resolve_stacking_aligned_path(&s.ctx, second, s.ids[0]).unwrap();
        assert_ne!(old, new);
    }

    #[test]
    fn aligned_path_survives_set_rename() {
        let s = seeded(&[4.0; 4], 0);
        let run_id = run_once(&s, &config());
        let before = resolve_stacking_aligned_path(&s.ctx, run_id, s.ids[3]).unwrap();

        s.fixture
            .conn
            .execute(
                "UPDATE frames_set SET name = 'Renamed set' WHERE id = ?1",
                [s.fixture.set_id],
            )
            .unwrap();

        assert_eq!(states(&s, run_id), vec![AlignedState::Available; 4]);
        // The row's path, not a folder rebuilt from the new name.
        let after = resolve_stacking_aligned_path(&s.ctx, run_id, s.ids[3]).unwrap();
        assert_eq!(before, after);
        assert!(after.is_file(), "{after:?}");
    }

    /// An earlier run's registered file is not a later run's: a frame the later run cut before Register keeps its
    /// artifact row, but that run never registered it.
    #[test]
    fn aligned_frame_cut_before_register_in_a_later_run_is_not_registered() {
        let s = seeded(&NOISY, 0);
        let noisy = s.ids[4];
        let first = run_once(&s, &config_with_measure_time_floor(None));
        assert_eq!(
            states(&s, first),
            vec![AlignedState::Available; 5],
            "the noisy frame passes the default floor"
        );

        let second = run_once(&s, &config_with_measure_time_floor(Some(CUT_FLOOR)));
        assert_eq!(
            run_frame_row(&s, second, noisy).reg_status.as_deref(),
            Some("skipped")
        );
        assert!(
            registered_artifact(&s, second, noisy).is_some(),
            "the first run's row is still there"
        );

        let mut expected = vec![AlignedState::Available; 4];
        expected.push(AlignedState::NotRegistered);
        assert_eq!(states(&s, second), expected);
        assert_eq!(states(&s, first), vec![AlignedState::Replaced; 5]);
    }

    /// A project's frames are project-frame handles: the lookups read the `project_frame` columns, not `frame_id`.
    #[test]
    fn aligned_project_frames_are_looked_up_by_their_handle() {
        let tmp = tempfile::tempdir().unwrap();
        let ctx = ServiceContext::new_for_tests(tmp.path().join("catalog.db"));
        let db_handle = db(&ctx).unwrap();
        let conn = db_handle.conn();
        crate::db::stacking::insert_test_project(&conn, "p1");
        let handles = project_frame_handles(&conn, "p1", &["u-a", "u-b", "u-c", "u-d"]).unwrap();
        let (a, b, c, d) = (
            handles["u-a"],
            handles["u-b"],
            handles["u-c"],
            handles["u-d"],
        );
        let subject = StackSubject::Project("p1".to_string());
        let key = "mono__NoFilter__bin1__60s";
        let new_run = || NewRun {
            subject: &subject,
            config_json: "{}",
            config_hash: "h",
            reference_frame_id: None,
            reference_mode: "auto",
            working_dir: "/w",
            output_dir: "/o",
        };
        let run_id = insert_run(&conn, &new_run()).unwrap();
        let new_group = |run_id| NewGroup {
            run_id,
            group_key: key,
            instrume: None,
            color_mode: "mono",
            filter: None,
            binning: Some(1),
            width: None,
            height: None,
            exposure: None,
            exposure_class: None,
            frame_count: 4,
            included_count: 4,
        };
        let group_id = insert_group(&conn, &new_group(run_id)).unwrap();
        for (handle, reg_status) in [
            (a, "aligned"),
            (b, "aligned"),
            (c, "skipped"),
            (d, "aligned_flipped"),
        ] {
            upsert_frame_row(
                &conn,
                &subject,
                &NewFrameRow {
                    run_id,
                    group_id,
                    frame_id: handle,
                    included: true,
                    exclusion_reason: None,
                    weight: None,
                    weight_channels_json: None,
                    metrics_json: None,
                    reg_status: Some(reg_status),
                    reg_model: None,
                    reg_rms_px: None,
                    reg_inliers: None,
                    reg_inlier_ratio: None,
                    reg_flipped: None,
                    rejected_fraction: None,
                },
            )
            .unwrap();
        }
        // Frame `a` has its aligned file; `b` was registered but has no artifact row; `c` was never registered; `d` was
        // registered across a meridian flip (`aligned_flipped`) and has its aligned file too.
        let file = tmp.path().join("r_a.fits");
        let flipped_file = tmp.path().join("r_d.fits");
        for (frame, path) in [(a, &file), (d, &flipped_file)] {
            std::fs::write(path, b"SIMPLE").unwrap();
            upsert_artifact(
                &conn,
                &NewArtifact {
                    subject: &subject,
                    frame_id: Some(frame),
                    group_key: key,
                    kind: "registered",
                    path: path.to_str(),
                    config_hash: "h",
                    size: Some(6),
                    modified_at: None,
                    payload_json: None,
                },
            )
            .unwrap();
        }
        drop(conn);

        let got = get_stacking_aligned_frames(&ctx, run_id, &[a, b, c, d]).unwrap();
        assert_eq!(
            got.iter().map(|r| r.state).collect::<Vec<_>>(),
            vec![
                AlignedState::Available,
                AlignedState::Missing,
                AlignedState::NotRegistered,
                AlignedState::Available
            ]
        );
        assert_eq!(
            resolve_stacking_aligned_path(&ctx, run_id, a).unwrap(),
            file
        );
        assert_eq!(
            resolve_stacking_aligned_path(&ctx, run_id, d).unwrap(),
            flipped_file
        );

        // A later run of the project that holds the group replaces the earlier run's frames.
        let conn = db_handle.conn();
        let later = insert_run(&conn, &new_run()).unwrap();
        insert_group(&conn, &new_group(later)).unwrap();
        drop(conn);
        assert_eq!(
            get_stacking_aligned_frames(&ctx, run_id, &[a]).unwrap()[0].state,
            AlignedState::Replaced
        );
    }
}
