//! Trigger-only shim for the auto-publish worker (collab v3 wave 2, Task 10).
//! Plate solving itself stays where it is (`plate_solve::service` + the
//! per-host `plate_solve_batch` command — see
//! `crates/athenaeum-tauri/src/commands/plate_solve.rs` and its web mirror);
//! this module exists only so both hosts have ONE shared call after they
//! emit `plate-solve-complete`.

use crate::services::ServiceContext;

/// Called by both hosts right after `plate-solve-complete` fires. A
/// plate-solve batch is a free-form list of frame ids (not scoped to one
/// frame set), so mapping it cheaply to "which linked projects care" isn't
/// worth it here — this dirties every auto-publish-enabled project instead
/// (the worker's `auto_publish = 1` + linked-set filter still applies, and a
/// run with nothing new to generate is cheap — Task 7).
pub fn on_batch_finished(_ctx: &ServiceContext) {
    crate::api::collab_autopublish::request_auto_publish(None);
}
