//! Resolves the working-memory budget for banded integration.
//!
//! Until 2026-09-06 this was a compile-time `256 * 1024 * 1024`. Profiling
//! (<https://github.com/eg013ra1n/athenaeum/blob/ff25561c/docs/superpowers/research/2026-09-06-master-integration-io-profiling.md>)
//! measured that constant costing 5.3x on a 100-frame set: it yields 105-row
//! bands, so the reader crosses all 100 files forty times and gets 22 MB/s off
//! a drive that sustains 243 MB/s. The budget is a property of the machine,
//! so it is resolved from the machine.

/// The pre-2026-09-06 constant. The policy is floored here so it can never
/// make any machine slower than it already is.
pub const MIN_BUDGET_BYTES: usize = 256 * 1024 * 1024;

/// 100 frames of 26 Mpx as `u16` is 5.2 GB; 8 GiB is where a large machine
/// reaches a single band and therefore whole-file sequential reads. Above
/// that the budget buys nothing an integration can spend.
pub const MAX_BUDGET_BYTES: usize = 8 * 1024 * 1024 * 1024;

/// The budget, in bytes (1 GiB), the caller's budget policy takes when [`total_ram_bytes`] returns `None`. Measured at
/// 101.2 s against the old constant's 241.6 s on the profiled set, and safe on any machine with the 8 GB a 26 Mpx
/// pipeline already needs.
pub const FALLBACK_BUDGET_BYTES: usize = 1024 * 1024 * 1024;

/// Bounds for an explicitly configured `integration.band_budget_mb`. The
/// floor is the same number as `MIN_BUDGET_BYTES` (256 MB = 256 MiB here) on
/// purpose: `per_job_budget`'s own `.max(MIN_BUDGET_BYTES)` floors every
/// resolved budget unconditionally, so a lower configured floor would be
/// silently overridden with no explanation — an honest configured window
/// must not advertise a minimum the backend refuses to honour.
///
/// KEEP IN SYNC with the `budgetNote` clamp window duplicated in
/// Athenaeum's `src/pages/Settings.tsx` (the Calibration tab's "Integration memory
/// budget" control) — these constants are private and ts-rs has no way to
/// hand a plain `usize` across the boundary, so the frontend re-derives its
/// own copy to explain a clamp without a round trip. A change here with no
/// matching change there means the UI confidently states the wrong range.
const CONFIGURED_MIN_MB: usize = 256;
const CONFIGURED_MAX_MB: usize = 16384;

/// Physical RAM this process may actually use, in bytes. The probe lives in `crate::sys`;
/// re-exported so every caller compiles as is.
pub use crate::sys::total_ram_bytes;

/// Clamps the configured integration memory budget, in MB (1 MB = 1,048,576 bytes), to 256..=16384 MB; `0` means
/// auto and returns `None`. The result is the window the backend honours, in MB.
pub fn clamp_configured_mb(mb: usize) -> Option<usize> {
    if mb == 0 { None } else { Some(mb.clamp(CONFIGURED_MIN_MB, CONFIGURED_MAX_MB)) }
}

/// Splits the machine-wide memory budget across the builds the compute queue may admit
/// at once, never below [`MIN_BUDGET_BYTES`] (256 MiB). `total` is in bytes, `max_concurrent` a count of builds
/// (0 counts as 1), and the answer is in bytes.
pub fn per_job_budget(total: usize, max_concurrent: usize) -> usize {
    (total / max_concurrent.max(1)).max(MIN_BUDGET_BYTES)
}
