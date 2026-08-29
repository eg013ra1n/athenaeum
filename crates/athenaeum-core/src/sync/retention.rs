//! Retention: the safety-critical decision of *when a successfully synced
//! source file may be deleted from the capture node*.
//!
//! This module guards a user's only copy of a night's data. Its single job is
//! to answer, for a given [`RetentionPolicy`], which locally-held source files
//! are safe to delete — and to never, under any policy or disk pressure, name a
//! file that has not been fully received by the peer.
//!
//! # The one hard invariant (enforced in exactly one place)
//!
//! **Only a package whose transfer is [`Confirmed`](super::OutboundState::Confirmed)
//! is ever eligible for deletion.** [`evaluate_and_apply`] obtains its candidate
//! set from a single call to [`SyncStore::confirmed`](super::store::SyncStore::confirmed),
//! which returns *only* confirmed rows. No other statement in this file produces
//! a path. Therefore an untransferred / queued / announced / transferring /
//! failed package can never be handed to the `deleter` — not even when the disk
//! is full. This is the property the exhaustive unit tests pin
//! (`untransferred_never_eligible_even_on_full_disk`, and the negative arm of
//! every other test).
//!
//! `Confirmed` itself carries the strengthened A7 semantics: a package is only
//! confirmed when *every* frame receipt is `Ingested`-or-`Duplicate` (an ack
//! carrying any `Rejected` receipt does not confirm). So "confirmed" here means
//! "the peer holds every frame of this package", which is exactly the
//! precondition for deleting the local source.
//!
//! # Two controller-level semantic decisions this module honours (not re-litigated)
//!
//! 1. **Primary-wins over resurrection.** A frame the primary ingested then
//!    later deleted, whose re-send the receipt log answers with `Duplicate`,
//!    still counts toward `Confirmed`. Retention MAY therefore delete it at the
//!    source: the deletion on the primary was deliberate, and sync must not
//!    resurrect it. Retention trusts `Confirmed`; it does not second-guess why a
//!    frame was a duplicate.
//! 2. **`Duplicate`-by-uuid asserts a catalog *row*, not disk presence.** The
//!    repo convention keeps rows for files on disconnected volumes. A duplicate
//!    verdict means "the peer's catalog has this frame", which is the durable
//!    fact retention keys on — it does not require the peer's copy to be mounted
//!    right now.
//!
//! # Dry-run is the default
//!
//! Until the M-Perseus-MVP soak gate passes, retention runs with `dry_run =
//! true`: every would-delete is logged (`warn!(path, policy, "retention dry-run:
//! would delete")`) and the `deleter` is **never** called. Perseus's config
//! validation refuses `dry_run = false` today, so live deletion is impossible
//! by construction in this build; the flag is threaded through so the evaluator
//! is ready the moment the gate lifts.
//!
//! # The deleter seam
//!
//! Actual removal is abstracted behind `deleter: &mut dyn FnMut(&Path) ->
//! Result<DeleteOutcome>`. Perseus supplies a closure that maps the confirmed
//! package back to its original capture file (via its own `perseus_seen` table —
//! see the crate-external plumbing), removes it, and writes a `sync_history`
//! audit row (`outcome = "retention_deleted"`). Perseus is the ONLY caller: the
//! desktop/web app shell never deletes a sent source (owner ruling 2026-08-29 —
//! its own retention loop was removed). Core stays agnostic:
//! it decides *which* confirmed subjects are eligible and logs the decision; the
//! deleter performs the side effect and owns any audit write, because only it
//! has the frame metadata.
//!
//! **Deleter contract (binding on every implementation, not just Perseus's):**
//!
//! 1. **Audit before the destructive action.** The deleter must persist its
//!    audit trail (a `sync_history` row, or whatever the host's equivalent is)
//!    *before* removing the file, and must return `Err` — leaving the file in
//!    place — if that persistence fails. A delete is only allowed to happen once
//!    it is durably discoverable that it happened; "file gone, audit missing" is
//!    the one outcome this contract forbids.
//! 2. **Report what actually happened.** Return
//!    [`DeleteOutcome::Removed`] only when a real removal occurred;
//!    [`DeleteOutcome::SkippedNoop`] for any legitimate no-op (the source was
//!    already gone, the linkage was already superseded, a last-line guard
//!    declined). [`evaluate_and_apply`] only counts `Removed` toward
//!    [`RetentionOutcome::deleted`] and only logs `retention_deleted` at `info`
//!    for `Removed` — a `SkippedNoop` logs at `debug` and never inflates the
//!    reported deletion count.
//! 3. **Re-verify immediately before removal (TOCTOU guard).** Core resolves
//!    *which package* to delete, but never touches a filesystem itself — only
//!    the deleter can check that the subject hasn't changed since it was
//!    resolved. A concurrent re-enqueue rewriting the same path between
//!    resolution and removal must cause the deleter to skip, not delete new
//!    (unconfirmed) content. Perseus's `retention_delete_source` implements this
//!    by comparing the file's current `(size, mtime)` against what was recorded
//!    at enqueue time, immediately before `remove_file`.
//!
//! # Clock injection (signature note)
//!
//! The brief's binding shape is `(store, policy, dry_run, disk_probe, deleter)`.
//! [`KeepDays`](RetentionPolicy::KeepDays) needs a "now" to compare against each
//! row's `confirmed_at`, and the brief explicitly sanctions injecting it ("use a
//! `now: DateTime` param or clock trait, your choice, document"). We take the
//! `now: DateTime<Utc>` param: it keeps the age comparison pure and lets tests
//! assert age boundaries deterministically with no wall-clock reads.

use std::path::{Path, PathBuf};

use anyhow::Result;
use chrono::{DateTime, Utc};

use super::models::OutboundRow;
use super::store::SyncStore;

/// How aggressively a capture node reclaims local space once frames are safely
/// on the peer. Every variant only ever acts on *confirmed* packages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RetentionPolicy {
    /// Never delete anything — the safe default.
    KeepEverything,
    /// Delete a source file as soon as its package is confirmed.
    OnConfirm,
    /// Delete a confirmed source once its `confirmed_at` is older than `n` days.
    KeepDays(u32),
    /// When disk usage is at or over `max_pct`, delete oldest-confirmed-first,
    /// stopping the moment usage drops back under the cap.
    DiskPct { max_pct: u8 },
}

impl RetentionPolicy {
    /// Stable snake_case label for the structured-log `policy` field.
    pub fn label(&self) -> &'static str {
        match self {
            RetentionPolicy::KeepEverything => "keep_everything",
            RetentionPolicy::OnConfirm => "on_confirm",
            RetentionPolicy::KeepDays(_) => "keep_days",
            RetentionPolicy::DiskPct { .. } => "disk_pct",
        }
    }
}

/// What one `deleter` invocation actually did to its subject.
///
/// See the deleter-contract section of the [module docs](self). Distinguishing
/// a real removal from a legitimate no-op is what lets [`evaluate_and_apply`]
/// report [`RetentionOutcome::deleted`] and log `retention_deleted` **only** for
/// a file that truly left disk.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeleteOutcome {
    /// The subject was actually removed. By contract the deleter has already
    /// persisted its audit trail before returning this.
    Removed,
    /// Nothing was removed — already handled, guard declined, or any other
    /// legitimate no-op. Never counted toward `deleted`, logged at `debug`.
    SkippedNoop,
}

/// The result of one retention pass.
///
/// `eligible` is every confirmed subject the policy deemed a deletion candidate
/// this pass; `deleted` is the subset actually removed (== `eligible` for
/// `OnConfirm`/`KeepDays`; a prefix for `DiskPct`; always empty when `dry_run`).
/// `would_warn_disk_pressure` flags that a `DiskPct` pass ended still at/over
/// the cap — nothing eligible on a full disk, or everything eligible deleted and
/// it still wasn't enough — so the host can raise a warning (Perseus logs it;
/// app-side `notify()` lands in M4).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RetentionOutcome {
    pub eligible: Vec<PathBuf>,
    pub deleted: Vec<PathBuf>,
    pub dry_run: bool,
    pub would_warn_disk_pressure: bool,
}

/// Whether a confirmed row's age has reached the `KeepDays` threshold.
///
/// Fails **safe**: a missing or unparseable `confirmed_at` is never eligible
/// (returns `false` with a `warn!`), so a corrupt timestamp can only ever *keep*
/// a file, never delete one.
fn confirmed_age_reached(row: &OutboundRow, days: u32, now: DateTime<Utc>) -> bool {
    let Some(ts) = row.confirmed_at.as_deref() else {
        tracing::warn!(
            package_ref = %row.package_ref,
            "retention: confirmed row missing confirmed_at; not eligible for keep_days"
        );
        return false;
    };
    match DateTime::parse_from_rfc3339(ts) {
        Ok(dt) => now.signed_duration_since(dt.with_timezone(&Utc)) >= chrono::Duration::days(days as i64),
        Err(error) => {
            tracing::warn!(
                package_ref = %row.package_ref,
                confirmed_at = %ts,
                %error,
                "retention: unparseable confirmed_at; not eligible for keep_days"
            );
            false
        }
    }
}

/// Current disk usage of the volume holding `path`, as a whole percent
/// (`0..=100`) — the probe a [`RetentionPolicy::DiskPct`] caller feeds to
/// [`evaluate_and_apply`]. Perseus is the only caller (it takes the MAX across
/// its capture volumes); nothing else consults it.
///
/// Fails **safe**: any error, or a platform without `statvfs`, returns `0`
/// ("empty disk") so a bad reading can never *trigger* a deletion — it can only
/// decline to.
#[cfg(unix)]
pub fn disk_usage_pct(path: &Path) -> u8 {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let Ok(cpath) = CString::new(path.as_os_str().as_bytes()) else {
        return 0;
    };
    // SAFETY: `stat` is zero-initialised and only read after a successful call;
    // `cpath` is a valid NUL-terminated C string living for the call's duration.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(cpath.as_ptr(), &mut stat) };
    if rc != 0 {
        tracing::warn!(path = %path.display(), "statvfs failed; retention disk probe returns 0%");
        return 0;
    }
    let total = stat.f_blocks as u128;
    let avail = stat.f_bavail as u128;
    if total == 0 {
        return 0;
    }
    let used = total.saturating_sub(avail);
    ((used * 100) / total).min(100) as u8
}

#[cfg(not(unix))]
pub fn disk_usage_pct(_path: &Path) -> u8 {
    // No statvfs; treat as empty so retention never deletes on disk pressure.
    0
}

/// Evaluate `policy` over the store's confirmed packages and (unless `dry_run`)
/// apply it through `deleter`.
///
/// See the [module docs](self) for the hard invariant, the dry-run contract, and
/// the clock-injection note. `disk_probe` returns current disk usage as a whole
/// percent (`0..=100`); it is only consulted by [`RetentionPolicy::DiskPct`] and
/// is re-invoked after each delete so the pass stops as soon as usage is back
/// under the cap.
pub fn evaluate_and_apply(
    store: &dyn SyncStore,
    policy: &RetentionPolicy,
    dry_run: bool,
    now: DateTime<Utc>,
    disk_probe: &dyn Fn() -> u8,
    deleter: &mut dyn FnMut(&Path) -> Result<DeleteOutcome>,
) -> Result<RetentionOutcome> {
    // ── THE SINGLE CHOKEPOINT ────────────────────────────────────────────────
    // The ONLY source of deletable paths in this function. `confirmed()` returns
    // exclusively `state = 'confirmed'` rows, so nothing untransferred can ever
    // flow past this line into `candidates` / `deleter`.
    let confirmed = store.confirmed()?;

    let mut outcome = RetentionOutcome {
        eligible: Vec::new(),
        deleted: Vec::new(),
        dry_run,
        would_warn_disk_pressure: false,
    };

    // KeepEverything: nothing is ever eligible — short-circuit before any probe.
    if matches!(policy, RetentionPolicy::KeepEverything) {
        tracing::debug!(
            policy = policy.label(),
            confirmed = confirmed.len(),
            "retention: keep_everything — nothing eligible"
        );
        return Ok(outcome);
    }

    // Select the eligible candidates (still ONLY from `confirmed`), already in
    // oldest-confirmed-first order (the store guarantees that ordering).
    let candidates: Vec<PathBuf> = match policy {
        RetentionPolicy::KeepEverything => unreachable!("handled above"),
        RetentionPolicy::OnConfirm => {
            confirmed.iter().map(|r| PathBuf::from(&r.package_ref)).collect()
        }
        RetentionPolicy::KeepDays(days) => confirmed
            .iter()
            .filter(|r| confirmed_age_reached(r, *days, now))
            .map(|r| PathBuf::from(&r.package_ref))
            .collect(),
        RetentionPolicy::DiskPct { max_pct } => {
            // Disk-pressure gated: candidates exist only when usage is at/over
            // the cap. Below the cap there is nothing to reclaim.
            if disk_probe() >= *max_pct {
                confirmed.iter().map(|r| PathBuf::from(&r.package_ref)).collect()
            } else {
                Vec::new()
            }
        }
    };

    outcome.eligible = candidates.clone();

    // ── Apply ────────────────────────────────────────────────────────────────
    for path in &candidates {
        // DiskPct stops the instant usage drops back under the cap. In dry-run
        // nothing is actually freed, so this never trips and we honestly report
        // every candidate we would have deleted.
        if let RetentionPolicy::DiskPct { max_pct } = policy {
            if !dry_run && disk_probe() < *max_pct {
                break;
            }
        }

        if dry_run {
            tracing::warn!(
                path = %path.display(),
                policy = policy.label(),
                "retention dry-run: would delete"
            );
            continue;
        }

        match deleter(path) {
            Ok(DeleteOutcome::Removed) => {
                tracing::info!(
                    path = %path.display(),
                    policy = policy.label(),
                    outcome = "retention_deleted",
                    "retention deleted"
                );
                outcome.deleted.push(path.clone());
            }
            Ok(DeleteOutcome::SkippedNoop) => {
                tracing::debug!(
                    path = %path.display(),
                    policy = policy.label(),
                    "retention: candidate skipped (no-op — already handled or guard declined)"
                );
            }
            Err(error) => {
                // Never swallow: a delete failure is logged and the file simply
                // survives to the next pass — erring toward keeping data.
                tracing::error!(
                    path = %path.display(),
                    policy = policy.label(),
                    %error,
                    "retention delete failed"
                );
            }
        }
    }

    // ── Disk-pressure would-warn ─────────────────────────────────────────────
    // For DiskPct: if the pass ends still at/over the cap (nothing confirmed to
    // free, or we deleted all we had and it wasn't enough), flag it so the host
    // can warn. In dry-run this is always set under pressure, which is correct:
    // "in live mode we'd still be over / we'd need attention".
    if let RetentionPolicy::DiskPct { max_pct } = policy {
        let usage = disk_probe();
        if usage >= *max_pct {
            outcome.would_warn_disk_pressure = true;
            tracing::warn!(
                policy = policy.label(),
                usage_pct = usage,
                max_pct = *max_pct,
                eligible = outcome.eligible.len(),
                deleted = outcome.deleted.len(),
                dry_run,
                "retention: disk still at/over threshold after pass"
            );
        }
    }

    Ok(outcome)
}
