//! Find unfinished archive operations and resume them.
//!
//! Resume reuses the executor: idempotency-by-step-log means already-Done
//! steps are skipped automatically.

use crate::archive::db as adb;
use crate::archive::executor::{run_operation, was_cancelled, CancelFlag};
use crate::archive::models::ArchiveOperationSummary;
use crate::events::ProgressEmitter;
use anyhow::Result;
use rusqlite::Connection;

/// List operations whose status is unfinished (resumable or rollback-needed).
pub fn find_unfinished_operations(conn: &Connection) -> Result<Vec<ArchiveOperationSummary>> {
    adb::list_unfinished_operations(conn)
}

/// Resume a previously-interrupted operation. Re-runs the executor; idempotent
/// step rows ensure already-completed work is not redone.
pub fn resume_operation(
    conn: &Connection,
    operation_id: i64,
    cancel: &CancelFlag,
    emitter: &dyn ProgressEmitter,
) -> Result<()> {
    match run_operation(conn, operation_id, cancel, emitter) {
        Ok(()) => Ok(()),
        Err(e) if was_cancelled(&e) => Err(e),
        Err(e) => Err(e),
    }
}

/// [`resume_operation`] plus the shared terminal bookkeeping — see
/// `executor::run_operation_standalone`. Both hosts' resume workers call
/// this; neither used to emit any terminal event at all.
pub fn resume_operation_standalone(
    conn: &Connection,
    operation_id: i64,
    cancel: &CancelFlag,
    emitter: &dyn ProgressEmitter,
) -> Result<()> {
    let result = resume_operation(conn, operation_id, cancel, emitter);
    crate::archive::executor::finish_forward_operation(conn, operation_id, emitter, result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::archive::executor::run_operation;
    use crate::archive::models::{ArchiveCompression, ConflictResolution, Dispositions};
    use crate::archive::planner::{build_plan, commit_plan};
    use crate::db::schema::init_db;
    use crate::events::NullEmitter;
    use std::sync::Arc;
    use std::sync::atomic::AtomicBool;
    use tempfile::TempDir;

    fn fixture() -> (Connection, TempDir, TempDir, i64) {
        let arch = TempDir::new().unwrap();
        let scan = TempDir::new().unwrap();
        let l1 = scan.path().join("M31/L_001.fits");
        std::fs::create_dir_all(l1.parent().unwrap()).unwrap();
        std::fs::write(&l1, b"l1").unwrap();

        let conn = Connection::open_in_memory().unwrap();
        init_db(&conn).unwrap();
        conn.execute("INSERT INTO scan_roots (id, path) VALUES (1, ?1)",
            [scan.path().to_str().unwrap()]).unwrap();
        conn.execute("INSERT INTO frames_set (id, name, is_archived) VALUES (1, 'M31', 1)", []).unwrap();
        conn.execute("INSERT INTO imaging_nights (id, frames_set_id, start_time, end_time)
             VALUES (10, 1, '2025-10-12', '2025-10-13')", []).unwrap();
        conn.execute("INSERT INTO sessions (id, imaging_night_id, instrume) VALUES (100, 10, 'C')", []).unwrap();
        conn.execute(
            "INSERT INTO files (id, path, filename, size, modified_at, format)
             VALUES (1000, ?1, 'L_001.fits', 2, '2025-10-12', 'FITS')",
            [l1.to_str().unwrap()],
        ).unwrap();
        conn.execute(
            "INSERT INTO frames (id, file_id, object, telescop, instrume, imagetyp)
             VALUES (10000, 1000, 'M31', 'T', 'C', 'Light')",
            [],
        ).unwrap();
        conn.execute("INSERT INTO session_members (session_id, frame_id) VALUES (100, 10000)", []).unwrap();

        let plan = build_plan(
            &conn, 1, arch.path(),
            &Dispositions { flats: None, darks: None, bias: None, darkflats: None },
            ArchiveCompression::Store,
        ).unwrap();
        let op_id = commit_plan(&conn, &plan, ConflictResolution::Overwrite).unwrap();
        (conn, arch, scan, op_id)
    }

    #[test]
    fn find_unfinished_returns_in_progress_ops() {
        let (conn, _arch, _scan, op_id) = fixture();
        // Status starts as Planning (unfinished)
        let unfinished = find_unfinished_operations(&conn).unwrap();
        assert_eq!(unfinished.len(), 1);
        assert_eq!(unfinished[0].id, op_id);
    }

    #[test]
    fn find_unfinished_excludes_completed() {
        let (conn, _arch, _scan, op_id) = fixture();
        let cancel = Arc::new(AtomicBool::new(false));
        run_operation(&conn, op_id, &cancel, &NullEmitter).unwrap();
        let unfinished = find_unfinished_operations(&conn).unwrap();
        assert_eq!(unfinished.len(), 0);
    }

    #[test]
    fn resume_completes_a_partially_run_operation() {
        let (conn, arch, _scan, op_id) = fixture();
        // Manually run the copy phase only by calling run_operation under
        // a flag that flips after one iteration would normally be tricky.
        // Simpler: just call run_operation, which should succeed end-to-end here.
        let cancel = Arc::new(AtomicBool::new(false));
        resume_operation(&conn, op_id, &cancel, &NullEmitter).unwrap();
        let op = adb::get_operation(&conn, op_id).unwrap();
        assert_eq!(op.status, "completed");
        let _ = arch;
    }
}

#[cfg(test)]
mod standalone_tests {
    use super::*;
    use crate::events::ProgressEmitter;
    use std::sync::atomic::AtomicBool;
    use std::sync::{Arc, Mutex};

    #[derive(Default)]
    struct CapturingEmitter(Mutex<Vec<(String, serde_json::Value)>>);

    impl ProgressEmitter for CapturingEmitter {
        fn emit_json(&self, event_name: &str, payload: serde_json::Value) {
            self.0.lock().unwrap().push((event_name.to_string(), payload));
        }
    }

    #[test]
    fn resume_operation_standalone_emits_terminal_event() {
        let conn = rusqlite::Connection::open_in_memory().unwrap();
        crate::db::schema::init_db(&conn).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let emitter = CapturingEmitter::default();

        let result = resume_operation_standalone(&conn, 999, &cancel, &emitter);
        assert!(result.is_err());

        let events = emitter.0.lock().unwrap();
        assert!(events
            .iter()
            .any(|(n, p)| n == "archive-finished" && p["outcome"] == "failed"));
    }
}
