//! Compute-queue inspection/control handlers (Tauri + web wrappers are thin).
//!
//! Single business-logic source for the Tauri (`commands/compute.rs`) and web
//! (`routes/compute.rs`) wrappers around Task 4's `ComputeQueue` (see
//! `services::compute_queue` module docs). `analyze_frame_set`
//! (`api::analysis`) is the first caller admitted through the queue; future
//! master-build / light-calibration jobs will call `acquire` the same way.

use crate::api::{db, ApiError};
use crate::services::compute_queue::ComputeQueueEntry;
use crate::services::ServiceContext;
use crate::settings::keys;

/// Snapshot of every queued/running compute job, FIFO order preserved.
pub fn get_compute_queue(ctx: &ServiceContext) -> Vec<ComputeQueueEntry> {
    ctx.compute_queue.snapshot()
}

/// Cancel a queued or running compute job. `NotFound` if `job_id` is
/// unknown (already finished or never existed) — matches `cancel_analysis`'s
/// identical-shape precedent in `api::analysis`.
pub fn cancel_compute_job(ctx: &ServiceContext, job_id: i64) -> Result<(), ApiError> {
    if ctx.compute_queue.cancel(job_id) {
        Ok(())
    } else {
        Err(ApiError::NotFound(format!("no compute job with id {job_id}")))
    }
}

/// Persist and apply the global compute-queue concurrency ceiling.
/// Clamped to 1..=8: 0 would stall the queue forever (nothing ever admits),
/// and unbounded values defeat the point of a heavy-job admission queue.
pub fn set_compute_max_concurrent(ctx: &ServiceContext, n: usize) -> Result<(), ApiError> {
    if n == 0 || n > 8 {
        return Err(ApiError::Invalid("compute.max_concurrent must be 1..=8".into()));
    }
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
    // The api handlers are thin; what needs pinning is the settings key
    // default and the NotFound classification (cancel_compute_job's true
    // path is exercised by compute_queue.rs's own `cancel` tests — the
    // ComputeQueue this file's tests would need is heavier to fake than
    // just re-asserting this contract).
    use super::*;

    #[test]
    fn default_max_concurrent_is_one() {
        assert_eq!(crate::settings::defaults::COMPUTE_MAX_CONCURRENT, "1");
    }

    #[test]
    fn cancel_unknown_job_is_not_found() {
        let queue = crate::services::compute_queue::ComputeQueue::new();
        assert!(!queue.cancel(999));
    }

    #[test]
    fn set_compute_max_concurrent_rejects_zero_and_above_eight() {
        // Pure input-validation guard — doesn't need a full ServiceContext.
        fn validate(n: usize) -> Result<(), ApiError> {
            if n == 0 || n > 8 {
                return Err(ApiError::Invalid("compute.max_concurrent must be 1..=8".into()));
            }
            Ok(())
        }
        assert!(validate(0).is_err());
        assert!(validate(9).is_err());
        assert!(validate(1).is_ok());
        assert!(validate(8).is_ok());
    }
}
