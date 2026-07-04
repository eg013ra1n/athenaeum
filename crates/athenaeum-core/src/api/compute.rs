//! Compute-queue inspection/control handlers (Tauri + web wrappers are thin).
//!
//! Single business-logic source for the Tauri (`commands/compute.rs`) and web
//! (`routes/compute.rs`) wrappers around Task 4's `ComputeQueue` (see
//! `services::compute_queue` module docs). `analyze_frame_set`
//! (`api::analysis`) is the first caller admitted through the queue; future
//! master-build / light-calibration jobs will call `acquire` the same way.

use crate::api::{db, ApiError};
use crate::services::compute_queue::{ComputeQueue, ComputeQueueEntry};
use crate::services::ServiceContext;
use crate::settings::keys;

/// Snapshot of every queued/running compute job, FIFO order preserved.
pub fn get_compute_queue(ctx: &ServiceContext) -> Vec<ComputeQueueEntry> {
    ctx.compute_queue.snapshot()
}

/// The cancel + NotFound mapping, on a bare queue. Split out of
/// `cancel_compute_job` so the mapping is testable without faking a full
/// `ServiceContext` (this IS the handler's logic, not a parallel copy).
pub(crate) fn cancel_on(queue: &ComputeQueue, job_id: i64) -> Result<(), ApiError> {
    if queue.cancel(job_id) {
        Ok(())
    } else {
        Err(ApiError::NotFound(format!("no compute job with id {job_id}")))
    }
}

/// Cancel a queued or running compute job. `NotFound` if `job_id` is
/// unknown (already finished or never existed) — matches `cancel_analysis`'s
/// identical-shape precedent in `api::analysis`.
pub fn cancel_compute_job(ctx: &ServiceContext, job_id: i64) -> Result<(), ApiError> {
    cancel_on(&ctx.compute_queue, job_id)
}

/// Bounds check for `set_compute_max_concurrent`, split out so the real
/// guard (not a test-local copy) is what tests pin. 0 would stall the queue
/// forever (nothing ever admits), and unbounded values defeat the point of
/// a heavy-job admission queue.
pub(crate) fn validate_max_concurrent(n: usize) -> Result<(), ApiError> {
    if n == 0 || n > 8 {
        return Err(ApiError::Invalid("compute.max_concurrent must be 1..=8".into()));
    }
    Ok(())
}

/// Persist and apply the global compute-queue concurrency ceiling
/// (clamped to 1..=8 — see `validate_max_concurrent`).
pub fn set_compute_max_concurrent(ctx: &ServiceContext, n: usize) -> Result<(), ApiError> {
    validate_max_concurrent(n)?;
    let db = db(ctx)?;
    let conn = db.conn();
    ctx.settings
        .persist_setting(&conn, keys::COMPUTE_MAX_CONCURRENT, &n.to_string())
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    ctx.compute_queue.set_max_concurrent(n);
    Ok(())
}

#[cfg(test)]
mod tests {
    // The api handlers are thin; what needs pinning is the settings-key
    // default, the cancel→NotFound mapping, and the max_concurrent bounds
    // check. The latter two are tested through the REAL functions
    // (`cancel_on`, `validate_max_concurrent`) — the ctx-taking wrappers
    // only forward to them, so nothing handler-side is left untested
    // besides trivial field plumbing.
    use super::*;
    use crate::services::compute_queue::ComputeJobKind;
    use std::sync::atomic::AtomicBool;
    use std::sync::Arc;

    #[test]
    fn default_max_concurrent_is_one() {
        assert_eq!(crate::settings::defaults::COMPUTE_MAX_CONCURRENT, "1");
    }

    #[test]
    fn cancel_on_unknown_job_is_not_found() {
        let queue = ComputeQueue::new();
        assert!(matches!(cancel_on(&queue, 999), Err(ApiError::NotFound(_))));
    }

    #[test]
    fn cancel_on_known_job_is_ok() {
        let queue = ComputeQueue::new();
        // Empty queue + free slot: acquire admits immediately on this
        // thread; hold the permit so the job stays in the registry.
        let flag = Arc::new(AtomicBool::new(false));
        let (_permit, job_id) = queue
            .acquire(ComputeJobKind::Analysis, "held", flag.clone())
            .unwrap();
        assert!(cancel_on(&queue, job_id).is_ok());
        assert!(flag.load(std::sync::atomic::Ordering::SeqCst), "cancel flag flipped");
    }

    #[test]
    fn validate_max_concurrent_rejects_zero_and_above_eight() {
        assert!(matches!(validate_max_concurrent(0), Err(ApiError::Invalid(_))));
        assert!(matches!(validate_max_concurrent(9), Err(ApiError::Invalid(_))));
        assert!(validate_max_concurrent(1).is_ok());
        assert!(validate_max_concurrent(8).is_ok());
    }
}
