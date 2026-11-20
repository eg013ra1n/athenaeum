//! The Black Hole bulk move with its side effects, in one place (MCP plan, Task 11).
//!
//! The Duplicates page's batch delete, and the assistant's applied Black Hole plan, both run this: the catalog move,
//! then the collab auto-publish note (render+solver builds), then the one `blackhole-changed` event the other views refresh on. The two
//! host wrappers (`commands/duplicates.rs`, `routes/duplicates.rs`) are one call each.

use crate::api::{db, ApiError};
use crate::events::ProgressEmitter;
use crate::models::BulkMoveResult;
use crate::services::ServiceContext;

/// Move `file_ids` to the Black Hole in one transaction, then note the change for auto-publish and emit
/// `blackhole-changed` `{"action":"bulk-blackholed","count": moved}` on `events`.
///
/// `progress` receives the move's per-file progress events (they drive an open Duplicates page's bar); the
/// assistant passes `None`, so only `blackhole-changed` reaches the pages. A file that cannot be moved is listed in
/// `BulkMoveResult::failed`; it never aborts the batch.
///
/// The emit cannot fail the move: a `ProgressEmitter` reports nothing back. Only the desktop emitter logs a failed
/// send; the web's SSE emitter ignores a send with no browser connected, on purpose.
pub fn bulk_move_to_black_hole(
    ctx: &ServiceContext,
    file_ids: &[i64],
    from_where: &str,
    progress: Option<&dyn ProgressEmitter>,
    events: &dyn ProgressEmitter,
) -> Result<BulkMoveResult, ApiError> {
    let result = {
        let database = db(ctx)?;
        let conn = database.conn();
        crate::db::bulk_move_to_black_hole(&conn, file_ids, from_where, progress).map_err(|e| {
            tracing::error!(error = %e, count = file_ids.len(), "bulk black hole move failed");
            ApiError::Internal(e.to_string())
        })?
        // `conn` drops here: the note below checks a connection out again (the pool has 8).
    };

    // No auto-publish to note in a headless build (no render+solver).
    #[cfg(all(feature = "render", feature = "solver"))]
    super::collab_autopublish::note_black_hole_change(ctx, file_ids);

    // One event, so the Black Hole tab, the file manager and missing-metadata views refresh. The payload has no
    // file id on purpose: consumers just reload.
    events.emit_json(
        "blackhole-changed",
        serde_json::json!({ "action": "bulk-blackholed", "count": result.moved }),
    );

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[cfg(all(feature = "render", feature = "solver"))]
    use crate::api::collab_autopublish::{is_set_dirty_for_test, reset_dirty_state_for_test, test_lock};
    use std::sync::Mutex;

    #[derive(Default)]
    struct Capture(Mutex<Vec<(String, serde_json::Value)>>);

    impl ProgressEmitter for Capture {
        fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
            self.0.lock().unwrap().push((event_name.to_string(), payload));
        }
    }

    impl Capture {
        fn events(&self) -> Vec<(String, serde_json::Value)> {
            self.0.lock().unwrap().clone()
        }
    }

    /// A catalog with one frame set holding two light frames; returns the set id and the two file ids.
    fn two_lights(ctx: &ServiceContext) -> (i64, Vec<i64>) {
        let database = db(ctx).unwrap();
        let conn = database.conn();
        conn.execute("INSERT INTO frames_set (name) VALUES ('S')", []).unwrap();
        let set_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO imaging_nights (frames_set_id, start_time, end_time) \
             VALUES (?1, '2026-07-01T20:00:00Z', '2026-07-02T03:00:00Z')",
            [set_id],
        )
        .unwrap();
        let night = conn.last_insert_rowid();
        conn.execute("INSERT INTO sessions (imaging_night_id, instrume) VALUES (?1, 'X')", [night]).unwrap();
        let session = conn.last_insert_rowid();
        let mut files = Vec::new();
        for n in 0..2 {
            conn.execute(
                "INSERT INTO files (path, filename, size, modified_at, format) \
                 VALUES (?1, ?2, 1, '2026-07-01T21:00:00Z', 'FITS')",
                rusqlite::params![format!("/d/L{n}.fits"), format!("L{n}.fits")],
            )
            .unwrap();
            let file_id = conn.last_insert_rowid();
            conn.execute(
                "INSERT INTO frames (file_id, imagetyp, uuid) VALUES (?1, 'Light', ?2)",
                rusqlite::params![file_id, format!("u-bh-{n}")],
            )
            .unwrap();
            let frame = conn.last_insert_rowid();
            conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (?1, ?2)", [session, frame])
                .unwrap();
            files.push(file_id);
        }
        (set_id, files)
    }

    #[test]
    fn bulk_move_notes_and_emits_like_the_ui() {
        #[cfg(all(feature = "render", feature = "solver"))]
        let _guard = test_lock();
        #[cfg(all(feature = "render", feature = "solver"))]
        reset_dirty_state_for_test();
        let (_tmp, base) = crate::test_support::canonical_tempdir();
        let ctx = ServiceContext::new_for_tests(base.join("catalog.db"));
        let (set_id, files) = two_lights(&ctx);
        #[cfg(not(all(feature = "render", feature = "solver")))]
        let _ = set_id;

        let capture = Capture::default();
        let result = bulk_move_to_black_hole(&ctx, &files, "duplicates", Some(&capture), &capture).unwrap();
        assert_eq!(result.moved, 2);
        assert!(result.failed.is_empty());

        let events = capture.events();
        let changed: Vec<_> = events.iter().filter(|(n, _)| n == "blackhole-changed").collect();
        assert_eq!(changed.len(), 1, "exactly one blackhole-changed: {events:?}");
        assert_eq!(changed[0].1, serde_json::json!({"action": "bulk-blackholed", "count": 2}));
        assert_eq!(events.last().map(|(n, _)| n.as_str()), Some("blackhole-changed"), "it comes last");
        assert!(
            events.iter().any(|(n, _)| n == "bulk-move-to-black-hole-progress"),
            "the UI's progress reaches its emitter: {events:?}"
        );
        #[cfg(all(feature = "render", feature = "solver"))]
        {
            assert!(is_set_dirty_for_test(set_id), "the frame set is dirty for auto-publish");
            reset_dirty_state_for_test();
        }
        let database = db(&ctx).unwrap();
        let rows: i64 = database.conn().query_row("SELECT COUNT(*) FROM black_hole", [], |r| r.get(0)).unwrap();
        assert_eq!(rows, 2);
        drop(ctx);
    }

    #[test]
    fn assistant_move_emits_no_progress_events() {
        #[cfg(all(feature = "render", feature = "solver"))]
        let _guard = test_lock();
        #[cfg(all(feature = "render", feature = "solver"))]
        reset_dirty_state_for_test();
        let (_tmp, base) = crate::test_support::canonical_tempdir();
        let ctx = ServiceContext::new_for_tests(base.join("catalog.db"));
        let (_set, files) = two_lights(&ctx);

        let capture = Capture::default();
        let result = bulk_move_to_black_hole(&ctx, &files, "assistant", None, &capture).unwrap();
        assert_eq!(result.moved, 2);

        assert_eq!(
            capture.events(),
            vec![("blackhole-changed".to_string(), serde_json::json!({"action": "bulk-blackholed", "count": 2}))]
        );
        let database = db(&ctx).unwrap();
        let from: String = database
            .conn()
            .query_row("SELECT from_where FROM black_hole LIMIT 1", [], |r| r.get(0))
            .unwrap();
        assert_eq!(from, "assistant");
        #[cfg(all(feature = "render", feature = "solver"))]
        reset_dirty_state_for_test();
        drop(ctx);
    }
}
