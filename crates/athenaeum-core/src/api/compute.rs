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
// The banded-integration memory budget lives in `integration::band_budget`,
// which reads raw pixels via astroimage and is gated on `render` — this
// module itself stays ungated so headless consumers keep compiling, so only
// the budget-specific items below carry the cfg.
#[cfg(feature = "render")]
use crate::integration::band_budget;

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
        Err(ApiError::NotFound(format!(
            "no compute job with id {job_id}"
        )))
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
        return Err(ApiError::Invalid(
            "compute.max_concurrent must be 1..=8".into(),
        ));
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

/// What the Settings control needs to show the operator both what they chose
/// and what the machine actually resolved.
#[cfg(feature = "render")]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct IntegrationBudgetInfo {
    /// The stored `integration.band_budget_mb`. `0` means auto.
    pub configured_mb: usize,
    /// What one integration job gets right now, after the auto formula and
    /// the division by `compute.max_concurrent`.
    pub effective_mb: usize,
    /// What auto alone would resolve to on this machine.
    pub auto_mb: usize,
    /// Physical RAM the probe found; `0` when the platform probe failed.
    pub total_ram_mb: usize,
}

/// Pure assembly, split out so it is testable without a `ServiceContext` —
/// the convention this module's other tests already follow.
#[cfg(feature = "render")]
pub(crate) fn budget_info_from(
    configured_mb: usize,
    effective_bytes: usize,
    auto_bytes: usize,
    total_ram_bytes: u64,
) -> IntegrationBudgetInfo {
    const MB: usize = 1024 * 1024;
    IntegrationBudgetInfo {
        configured_mb,
        effective_mb: effective_bytes / MB,
        auto_mb: auto_bytes / MB,
        total_ram_mb: (total_ram_bytes / MB as u64) as usize,
    }
}

/// Read the resolved banded-integration memory budget, for the Settings
/// control: what the operator configured, what it resolves to right now,
/// and the auto/RAM figures it takes to explain a gap between the two.
#[cfg(feature = "render")]
pub fn get_integration_band_budget(ctx: &ServiceContext) -> Result<IntegrationBudgetInfo, ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    let configured_mb = ctx
        .settings
        .get_integration_band_budget_mb(&conn)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let effective_bytes = band_budget::resolve_budget_bytes(&conn, &ctx.settings)
        .map_err(|e| ApiError::Internal(e.to_string()))?;
    let auto_bytes = band_budget::auto_budget_bytes();
    let total_ram_bytes = band_budget::total_ram_bytes().unwrap_or(0);
    Ok(budget_info_from(configured_mb, effective_bytes, auto_bytes, total_ram_bytes))
}

/// Persist the budget. `0` restores auto; anything else is clamped to
/// 256..=16384 MB by the resolver, so a wild value degrades instead of
/// OOM-ing the next build.
#[cfg(feature = "render")]
pub fn set_integration_band_budget(ctx: &ServiceContext, mb: usize) -> Result<(), ApiError> {
    let db = db(ctx)?;
    let conn = db.conn();
    ctx.settings
        .persist_setting(&conn, keys::INTEGRATION_BAND_BUDGET_MB, &mb.to_string())
        .map_err(|e| ApiError::Internal(e.to_string()))?;
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
        assert!(
            flag.load(std::sync::atomic::Ordering::SeqCst),
            "cancel flag flipped"
        );
    }

    #[test]
    fn validate_max_concurrent_rejects_zero_and_above_eight() {
        assert!(matches!(
            validate_max_concurrent(0),
            Err(ApiError::Invalid(_))
        ));
        assert!(matches!(
            validate_max_concurrent(9),
            Err(ApiError::Invalid(_))
        ));
        assert!(validate_max_concurrent(1).is_ok());
        assert!(validate_max_concurrent(8).is_ok());
    }

    #[test]
    fn default_band_budget_is_auto() {
        assert_eq!(crate::settings::defaults::INTEGRATION_BAND_BUDGET_MB, "0");
    }

    #[cfg(feature = "render")]
    #[test]
    fn budget_info_reports_configured_effective_and_auto() {
        // 700 MB configured, one admitted job, 16 GB machine.
        let info = budget_info_from(700, 700 * 1024 * 1024, 4096 * 1024 * 1024, 16384 * 1024 * 1024);
        assert_eq!(info.configured_mb, 700);
        assert_eq!(info.effective_mb, 700);
        assert_eq!(info.auto_mb, 4096, "auto is reported even when overridden, so the UI can name it");
        assert_eq!(info.total_ram_mb, 16384);

        // Auto, but two admitted jobs halve it.
        let info = budget_info_from(0, 2048 * 1024 * 1024, 4096 * 1024 * 1024, 16384 * 1024 * 1024);
        assert_eq!(info.configured_mb, 0, "0 is the auto sentinel and is reported as-is");
        assert_eq!(info.effective_mb, 2048);
        assert_eq!(info.auto_mb, 4096, "the UI must be able to say why effective < auto");

        // Failed RAM probe.
        assert_eq!(budget_info_from(0, 1024 * 1024 * 1024, 1024 * 1024 * 1024, 0).total_ram_mb, 0);
    }
}
