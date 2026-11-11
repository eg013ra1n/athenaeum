//! Tests of stage 0.5; the shared rig is in `run/tests.rs`.

use super::*;
use crate::stacking::run::tests::*;
use crate::stacking::test_fixtures::{
    self, seed_star_group, wait_for_run, STAR_FIELD_HEIGHT, STAR_FIELD_WIDTH,
};

// ── Stage 0.5: the run builds/rebuilds its own masters ────────────────

/// Owner requirement 2026-09-09 ("the pipeline should build the
/// calibration masters itself when they are missing"): the Task 6/7
/// fixture's TWO master files deleted from disk — provenance rows and
/// their real raw source sub-frames intact, via `add_master_dark_and_flat`'s
/// real `register_master` registration (Task 8) — `start_stacking`
/// rebuilds both inside stage 0.5, BEFORE calibrate, and the run still
/// finishes `done`.
#[test]
fn deleted_masters_are_rebuilt_by_stage_masters() {
    let tmp = tempfile::tempdir().unwrap();
    let db_path = tmp.path().join("catalog.db");
    let ctx = Arc::new(ServiceContext::new_for_tests(db_path.clone()));
    let shifts = [(0.0, 0.0), (3.0, 0.0), (0.0, 3.0), (2.0, 2.0)];
    let noise = [5.0f32; 4];
    let (fixture, light_ids, _working, _output) =
        seed_star_group(&db_path, SET_NAME, &shifts, &noise);
    let (dark_set, flat_set) = test_fixtures::add_master_dark_and_flat(
        &fixture,
        &light_ids,
        STAR_FIELD_WIDTH,
        STAR_FIELD_HEIGHT,
    );

    let master_path = |set_id: i64| -> String {
        fixture
            .conn
            .query_row(
                "SELECT fi.path FROM calibration_set_frames csf
                     JOIN frames fr ON fr.id = csf.frame_id
                     JOIN files fi ON fi.id = fr.file_id
                     WHERE csf.set_id = ?1",
                [set_id],
                |r| r.get(0),
            )
            .unwrap()
    };
    let dark_path = master_path(dark_set);
    let flat_path = master_path(flat_set);
    // Before: both master files exist (the fixture just wrote them).
    assert!(
        Path::new(&dark_path).exists(),
        "fixture sanity: dark master written"
    );
    assert!(
        Path::new(&flat_path).exists(),
        "fixture sanity: flat master written"
    );
    std::fs::remove_file(&dark_path).unwrap();
    std::fs::remove_file(&flat_path).unwrap();
    assert!(!Path::new(&dark_path).exists());
    assert!(!Path::new(&flat_path).exists());

    let recorder = Arc::new(Recording::new());
    let started = start_stacking(
        ctx.clone(),
        recorder.clone(),
        &PathPolicy::AllowAll,
        "test".to_string(),
        StackSubject::FrameSet(fixture.set_id),
        None,
        None,
    )
    .expect("start should succeed");

    wait_for_run(&ctx, started.run_id);

    let run_row = crate::db::stacking::get_run(&fixture.conn, started.run_id)
        .unwrap()
        .unwrap();
    assert_eq!(run_row.status, "done", "{run_row:?}");

    // After: both masters rebuilt AT THEIR ORIGINAL LIBRARY PATHS.
    assert!(
        Path::new(&dark_path).exists(),
        "dark master must be rebuilt at its original path"
    );
    assert!(
        Path::new(&flat_path).exists(),
        "flat master must be rebuilt at its original path"
    );

    // Progress: stage `masters` reports 2/2 before `calibrate` ever ticks.
    let events = recorder.events(STACKING_PROGRESS_EVENT);
    let masters_indices: Vec<usize> = events
        .iter()
        .enumerate()
        .filter(|(_, e)| e["stage"].as_str() == Some("masters"))
        .map(|(i, _)| i)
        .collect();
    assert!(!masters_indices.is_empty(), "no masters progress events");
    let last_masters = &events[*masters_indices.last().unwrap()];
    assert_eq!(
        last_masters["current"].as_u64(),
        Some(2),
        "{last_masters:?}"
    );
    assert_eq!(last_masters["total"].as_u64(), Some(2), "{last_masters:?}");
    let first_calibrate_index = events
        .iter()
        .position(|e| e["stage"].as_str() == Some("calibrate"))
        .expect("a calibrate progress event exists");
    assert!(
        *masters_indices.last().unwrap() < first_calibrate_index,
        "masters progress must finish before calibrate starts: masters at {masters_indices:?}, calibrate first at {first_calibrate_index}"
    );

    // The summary lists two masters_built, one per rebuilt master.
    let summary: RunSummary = serde_json::from_str(
        run_row
            .summary_json
            .as_deref()
            .expect("summary_json stored"),
    )
    .unwrap();
    assert_eq!(
        summary.masters_built.len(),
        2,
        "{:?}",
        summary.masters_built
    );
    let rebuilt_master_set_ids: std::collections::HashSet<i64> = summary
        .masters_built
        .iter()
        .map(|m| m.master_set_id)
        .collect();
    assert_eq!(
        rebuilt_master_set_ids,
        std::collections::HashSet::from([dark_set, flat_set]),
        "{:?}",
        summary.masters_built
    );
    for m in &summary.masters_built {
        assert_eq!(m.kind, MasterWork::Rebuild, "{m:?}");
        assert!(Path::new(&m.path).exists(), "{m:?}");
    }
}
