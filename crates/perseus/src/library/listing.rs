//! One directory of a capture root, each file joined to what Perseus knows about
//! its send fate.
//!
//! The listing is deliberately **shallow**: exactly one `read_dir` of the
//! addressed directory, never a walk. An operator browsing a night's worth of
//! subs must not pay for a recursive scan of a NAS share, and the UI navigates by
//! descending one level at a time.
//!
//! # Where a status comes from
//!
//! Perseus has no single "state" column for a capture file — the fate of a frame
//! is spread across three stores, and [`FileStatus`] is the *derived* answer:
//!
//! | source | fact it contributes |
//! | ---- | ---- |
//! | [`BatcherHandle::pending_snapshot`](crate::batcher::BatcherHandle::pending_snapshot) | accumulated, not yet flushed → [`Queued`](FileStatus::Queued) |
//! | [`BatchStore::batches_for_source`] + `sync_outbound` | which packages carried it, and how each package's newest attempt TO EACH TARGET ended |
//! | [`SeenStore::is_recorded`] | it was handed to the engine at least once and retention has not removed it |
//!
//! A package fanned out to several targets has one `sync_outbound` row per
//! target under the same `package_ref`, so the unit of a verdict is
//! `(package_ref, peer)` — see `newest_by_target`. Every target of every batch
//! carrying the file votes, and the batch arms are ANY-of.
//!
//! Precedence runs newest-fact-first, because a file can be several of these at
//! once (a re-captured frame is both `Queued` now and `Delivered` from a previous
//! batch — the operator cares about the pending send):
//!
//! 1. in the pending set → [`Queued`](FileStatus::Queued);
//! 2. else ANY target's newest attempt non-terminal → [`Sending`](FileStatus::Sending) (something is still moving);
//! 3. else ANY target's newest attempt `Confirmed` → [`Delivered`](FileStatus::Delivered) (it landed somewhere — which targets got it is the Transfers tab's job, not this chip's);
//! 4. else ANY target's newest attempt a **receiver decline** → [`Declined`](FileStatus::Declined);
//! 5. else a live seen row → [`Sent`](FileStatus::Sent);
//! 6. else [`Unsent`](FileStatus::Unsent).
//!
//! A locally-failed or operator-cancelled attempt deliberately falls through
//! arms 2–4: neither says anything about the file's fate that arm 5 does not say
//! more honestly.
//!
//! # The retention fate line is a SEPARATE derivation
//!
//! [`LibraryEntry::retention`] does not read [`FileStatus`] at all. The status
//! chip answers "where is this file in its send life"; the fate line answers
//! "what will retention do to it", and the two legitimately disagree — a
//! re-captured file sitting in the batcher reads `Queued` while its live seen
//! linkage still points at the package that confirmed last week, which is the
//! candidacy that will delete it. [`retention_fate`] is derived from the
//! evaluator (`athenaeum_core::sync::retention`), never from the chip; see its
//! docs for the resolutions that follow from that.
//!
//! Its anchor comes from the **live seen linkage**
//! ([`SeenStore::package_for_path`]), not from the batch-participation set: the
//! deleter only ever resolves candidates through `sources_for_package`
//! (`deleted_at IS NULL`), and `perseus_seen.path` is the PRIMARY KEY, so a
//! re-enqueue MOVES the linkage and only the package named by the live row can
//! ever delete the file.
//!
//! # Path spelling is the join key
//!
//! Every store keys on the path spelling the watcher recorded, which is
//! **canonicalized** (`spawn_watcher` canonicalizes the root, and each discovered
//! file, precisely so `notify` and the poll sweep agree). This module matches
//! that by resolving the browsed directory ONCE through
//! [`super::resolve_in_root`] and joining entry names onto the canonical result —
//! never by calling [`super::to_wire_rel`] per row, which would cost two
//! canonicalizations per file.
//!
//! The one spelling that still diverges is an in-root **symlink to a file**: the
//! watcher records the link's target, this listing keys on the link's own path,
//! so such an entry reads `Unsent` even after its target was sent. Listing the
//! link under its own name is the deliberate half of that trade (a listing must
//! show what the directory contains); the status is honestly conservative.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use athenaeum_core::sharing::types::NodeId;
use athenaeum_core::sync::{OutboundRow, OutboundState, RetentionPolicy};
use chrono::{DateTime, Utc};

use crate::batch_store::BatchStore;
use crate::config::RetentionConfig;
use crate::resend::is_declined;
use crate::seen::{mtime_millis, SeenStore};

use super::{resolve_in_root, split_rel};

/// Fate line for a file no policy can name yet — its live seen linkage has not
/// reached the confirmed-only chokepoint (or there is no live linkage at all).
const FATE_KEPT: &str = "kept until sent and confirmed";

/// What retention's clock says about one file, resolved through its **live**
/// seen linkage ([`SeenStore::package_for_path`]).
///
/// The last two states are kept distinct on purpose. "Never confirmed" and
/// "confirmed, but no row carried a usable instant" are different facts, and
/// collapsing them makes the fate line print [`FATE_KEPT`] over a file whose
/// package HAS confirmed — denying a confirm that happened. Only `keep_days`
/// reads the instant at all; `on_confirm` and `disk_pct` act on the confirm
/// itself, exactly as the evaluator does.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FateAnchor {
    /// No confirmed row can name this file: it has no live seen linkage (never
    /// enqueued, or deleted and re-captured at the same path), or the package it
    /// IS linked to has not confirmed to any target.
    NotConfirmed,
    /// The linked package confirmed; this is the earliest usable confirm across
    /// its targets — the instant retention's clock starts.
    ConfirmedAt(DateTime<Utc>),
    /// The linked package confirmed, but no confirmed row carried a parseable
    /// `confirmed_at`.
    ConfirmedUndated,
}

/// The derived send fate of one capture file. Serialized lowercase — the wire
/// values the Library UI switches on.
#[derive(serde::Serialize, PartialEq, Eq, Debug, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub enum FileStatus {
    /// No store knows this file: it has never been enqueued.
    Unsent,
    /// Accumulated in the batcher, awaiting the next flush.
    Queued,
    /// At least one target of one batch carrying it is in flight (that target's
    /// newest attempt is non-terminal).
    Sending,
    /// A batch carrying it was confirmed by at least one receiver.
    Delivered,
    /// Every target that reached a verdict declined it (final per
    /// `(batch, target)`).
    Declined,
    /// Handed to the sync engine at some point, with no live batch row that says
    /// more.
    Sent,
}

/// One file row of a [`LibraryListing`].
#[derive(serde::Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LibraryEntry {
    /// The entry's own filename (never a path) — join it onto the listing's
    /// `path` to address it.
    pub name: String,
    pub size: u64,
    /// Modification time in milliseconds since the epoch, `0` when the
    /// filesystem does not report one (same rendering `perseus_seen` stores).
    pub mtime_ms: i64,
    pub status: FileStatus,
    /// How many recorded send batches carried this file (original + any divert
    /// copies). `0` for a file that was never packaged.
    pub batches: usize,
    /// What retention intends to do with this file, in the operator's words
    /// ([`retention_fate`]). `None` under `keep_everything` — the node deletes
    /// nothing, so there is no fate to state.
    pub retention: Option<String>,
}

/// One directory of one capture root: its immediate subdirectory names and its
/// immediate files, each sorted by name.
#[derive(serde::Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct LibraryListing {
    /// Index of the capture root in `capture_dirs_resolved()`, echoed back so the
    /// client can build child paths without tracking it itself.
    pub root: usize,
    /// The normalized wire rel-path of this directory (`""` for the root).
    pub path: String,
    pub dirs: Vec<String>,
    pub files: Vec<LibraryEntry>,
}

/// Borrowed handles to everything the status join reads. Assembled once per
/// request by the route so the per-file join never re-snapshots the batcher or
/// re-reads `sync_outbound`.
pub struct StatusSources<'a> {
    /// The batcher's pending accumulator as `(capture_dir, file)` pairs.
    pub pending: &'a [(PathBuf, PathBuf)],
    pub batches: &'a BatchStore,
    pub seen: &'a SeenStore,
    /// Every outbound row in the read window, in any order — the newest attempt
    /// per `(package_ref, peer)` is picked here, not by the caller.
    pub outbound: &'a [OutboundRow],
    /// The live retention knobs, for the per-file fate line ([`retention_fate`]).
    /// Taken as the whole config rather than a pre-mapped policy so the listing
    /// goes through the same `to_core_policy()` seam the retention task does —
    /// the threshold in the sentence is then the threshold that will be applied.
    pub retention: &'a RetentionConfig,
}

/// Every target's newest row for each package: outer key `package_ref`, inner
/// key `peer`.
///
/// **`package_ref` alone is NOT a transfer.** A Perseus package fanned out to N
/// configured targets is enqueued once per target
/// (`run::enqueue_package_to_all`), so N
/// `sync_outbound` rows share one `package_ref` (the payload dir path) and
/// differ only by `peer`. Collapsing them into one row would let whichever
/// target happened to be enqueued last speak for the whole fan-out — a NAS still
/// transferring would read `Delivered` because the studio box confirmed.
///
/// Within one target the row IS the transfer: a resend resets it in place
/// (generation+1, batch model v2.1) rather than inserting, and a divert mints a
/// row against a DIFFERENT peer — so a second row for the same pair only appears
/// in a stale read window. Max-id-wins keeps that case defensive rather than
/// arbitrary.
pub(super) fn newest_by_target(
    outbound: &[OutboundRow],
) -> HashMap<&str, HashMap<NodeId, &OutboundRow>> {
    let mut map: HashMap<&str, HashMap<NodeId, &OutboundRow>> = HashMap::new();
    for row in outbound {
        map.entry(row.package_ref.as_str())
            .or_default()
            .entry(row.peer)
            .and_modify(|cur| {
                if row.id > cur.id {
                    *cur = row;
                }
            })
            .or_insert(row);
    }
    map
}

/// What retention intends to do with one file, in the operator's words.
///
/// # This mirrors the evaluator, not the card copy
///
/// Every arm below was derived by reading
/// [`evaluate_and_apply`](athenaeum_core::sync::evaluate_and_apply), because a
/// fate line that disagrees with the evaluator is worse than no line at all.
/// `policy` is deliberately the **core** [`RetentionPolicy`] — the carrying enum
/// `RetentionConfig::to_core_policy` produces and the retention task passes
/// straight to the evaluator — so the threshold in the sentence can never drift
/// from the threshold that will be applied.
///
/// `anchor` is resolved through the file's **live seen linkage** — see
/// [`FateAnchor`] and `confirm_anchor_by_package`.
///
/// Four deliberate resolutions:
///
/// 1. **No `status` parameter.** A re-captured file waiting in the batcher reads
///    `Queued` while its live seen linkage still points at the package that
///    confirmed last week — retention deletes it through that candidacy. Gating
///    on the status chip would print "kept" over a file about to be removed.
/// 2. **The anchor follows the LIVE linkage, not every package that ever carried
///    the file.** The deleter resolves candidates via `sources_for_package`
///    (`WHERE package_ref = ? AND deleted_at IS NULL`) and `perseus_seen.path` is
///    the PRIMARY KEY, so a re-enqueue moves the linkage and the old package can
///    never name the file again. Anchoring on it would print a date that can
///    never fire.
/// 3. **The unconfirmed arm applies to every policy, not just `keep_days`.**
///    `disk_pct` draws its candidates from the same confirmed-only chokepoint,
///    so an unconfirmed file is not a pressure candidate either (spec §8 lists
///    "unsent/unconfirmed → kept…" as its own row; this is that row).
/// 4. **`on_confirm` gets a line** even though the spec's card copy enumerates
///    only three policies: it is a real, selectable value in `config.rs`, and
///    silence about the most aggressive policy Perseus has would be the worst
///    possible omission.
///
/// Returns `None` for `keep_everything`, and for the two `keep_days` states in
/// which no honest date exists: a confirm with no usable timestamp, and a
/// threshold too large to add to a date. Both degrade the SAME way — naming no
/// date beats naming a wrong one, and beats [`FATE_KEPT`], which would deny a
/// confirm that happened.
pub fn retention_fate(
    policy: &RetentionPolicy,
    dry_run: bool,
    anchor: FateAnchor,
) -> Option<String> {
    // The evaluator short-circuits here before any candidate work; so do we.
    if matches!(policy, RetentionPolicy::KeepEverything) {
        return None;
    }
    // The single chokepoint: the live linkage has no confirm ⇒ no policy can
    // ever name it.
    if matches!(anchor, FateAnchor::NotConfirmed) {
        return Some(FATE_KEPT.to_string());
    }
    // "would be " turns every deletion sentence into its dry-run form. The
    // `kept` arm above never takes it — it is not a deletion statement.
    let prefix = if dry_run { "would be " } else { "" };
    match policy {
        RetentionPolicy::KeepEverything => None,
        // Neither policy reads `confirmed_at`: the evaluator makes every
        // confirmed row a candidate (`disk_pct` gated on the probe), dated or
        // not. So an undated confirm changes nothing here.
        RetentionPolicy::OnConfirm => Some(format!("{prefix}deleted at the next retention pass")),
        RetentionPolicy::KeepDays(days) => {
            // The only policy that needs the instant. Missing it is the
            // evaluator's warn-and-refuse state (`confirmed_age_reached`), which
            // is broken data rather than a promise to keep the file — so: no
            // line, the same degrade as the unrepresentable threshold below.
            let FateAnchor::ConfirmedAt(confirmed_at) = anchor else {
                return None;
            };
            // Mirrors `confirmed_age_reached`: eligible once
            // `now - confirmed_at >= days`. Checked all the way through —
            // `keep_days` is only validated `>= 1`, so a hand-edited absurd
            // value must degrade to silence, not panic or print a wrapped date.
            let deletable_at = chrono::Duration::try_days(i64::from(*days))
                .and_then(|d| confirmed_at.checked_add_signed(d));
            let Some(deletable_at) = deletable_at else {
                tracing::warn!(
                    keep_days = *days,
                    confirmed_at = %confirmed_at,
                    "library fate: keep_days threshold is not representable as a date; no fate line"
                );
                return None;
            };
            // A date already in the past is the honest answer, not a bug: the
            // file IS eligible and is waiting for the next pass (or for the
            // dry-run to be turned off). Inventing "any moment now" would state
            // a schedule the loop does not expose.
            Some(format!(
                "{prefix}deletable after {}",
                deletable_at.format("%Y-%m-%d")
            ))
        }
        RetentionPolicy::DiskPct { .. } => {
            Some(format!("{prefix}deleted under disk pressure, oldest first"))
        }
    }
}

/// Per `package_ref`: whether it confirmed at all (the key is present), and the
/// earliest usable `confirmed_at` across its targets (the value) — the instant
/// retention's clock starts for that package.
///
/// The two facts are separate because the policies read them separately.
/// `on_confirm` and `disk_pct` act on the confirm itself and never look at a
/// timestamp; only `keep_days` needs the instant. So a package whose only
/// confirmed rows carry a missing/unparseable `confirmed_at` maps to
/// `Some(None)` — confirmed, undated — never to an absent key, which would read
/// as "never confirmed" and print [`FATE_KEPT`] over a delivered file.
///
/// **Deliberately not [`newest_by_target`]'s collapse.** That map keeps one row
/// per `(package_ref, peer)` by max id, which is the right lens for a *status*
/// chip. The evaluator instead reads
/// [`confirmed()`](athenaeum_core::sync::store::SyncStore::confirmed) — every
/// row whose state is `Confirmed`, with no per-target collapse — and applies its
/// age test per row, so a package is a candidate as soon as its EARLIEST confirm
/// ages out. Taking the min here reproduces that. In a stale read window holding
/// both a confirmed and a newer live row for one pair, this errs toward warning
/// the operator rather than toward a silent "kept".
///
/// An unusable timestamp is logged at `debug` rather than `warn`: this is a
/// read-only display derivation re-run on every UI poll, and the evaluator
/// already warns once per pass on the same rows — a second `warn!` per file per
/// poll would bury it.
fn confirm_anchor_by_package(outbound: &[OutboundRow]) -> HashMap<&str, Option<DateTime<Utc>>> {
    let mut map: HashMap<&str, Option<DateTime<Utc>>> = HashMap::new();
    for row in outbound {
        if row.state != OutboundState::Confirmed {
            continue;
        }
        // Insert the KEY for every confirmed row, dated or not: its presence is
        // the "this package confirmed" fact the timestamp-free policies act on.
        let slot = map.entry(row.package_ref.as_str()).or_insert(None);
        let Some(ts) = row.confirmed_at.as_deref() else {
            tracing::debug!(
                package_ref = %row.package_ref,
                "library fate: confirmed row without confirmed_at; contributes no retention clock"
            );
            continue;
        };
        let parsed = match DateTime::parse_from_rfc3339(ts) {
            Ok(dt) => dt.with_timezone(&Utc),
            Err(error) => {
                tracing::debug!(
                    package_ref = %row.package_ref,
                    confirmed_at = %ts,
                    %error,
                    "library fate: unparseable confirmed_at; contributes no retention clock"
                );
                continue;
            }
        };
        match *slot {
            Some(cur) if cur <= parsed => {}
            _ => *slot = Some(parsed),
        }
    }
    map
}

/// Everything the join derives for one file.
struct FileFacts {
    status: FileStatus,
    batches: usize,
    /// Retention's clock for this file, read through its LIVE seen linkage —
    /// the only package whose candidacy can reach it (`retention_fate` note 2).
    anchor: FateAnchor,
}

/// Derive one file's status, batch-participation count, and retention anchor.
/// See the module docs for the precedence; the batch lookup runs unconditionally
/// so `batches` is honest even for a file whose status came from an earlier arm.
///
/// `confirms` is `None` under `keep_everything`: no fate is stated, so the extra
/// per-file `package_for_path` lookup is skipped entirely on the default policy.
/// An *empty* map (a node where nothing has confirmed yet) skips it for the same
/// reason — no lookup can hit.
fn status_for(
    abs: &Path,
    pending: &HashSet<&Path>,
    newest: &HashMap<&str, HashMap<NodeId, &OutboundRow>>,
    confirms: Option<&HashMap<&str, Option<DateTime<Utc>>>>,
    src: &StatusSources<'_>,
) -> Result<FileFacts> {
    let refs = src.batches.batches_for_source(&abs.to_string_lossy())?;
    let batches = refs.len();
    // Resolved for every arm, including the early returns below: the retention
    // anchor is independent of the status chip (see `retention_fate`'s note 1),
    // and it goes through the seen linkage rather than `refs` (note 2) — a
    // package the file no longer links to can never delete it.
    let anchor = match confirms {
        None => FateAnchor::NotConfirmed,
        // Nothing on this node has ever confirmed, so every `confirms.get` below
        // would miss and every file would land on `NotConfirmed` anyway — skip
        // the per-file linkage query outright. This is the whole first night of
        // a fresh node, and the listing is the hot path there.
        Some(confirms) if confirms.is_empty() => FateAnchor::NotConfirmed,
        Some(confirms) => match src.seen.package_for_path(abs)? {
            None => FateAnchor::NotConfirmed,
            Some(package_ref) => match confirms.get(package_ref.as_str()) {
                None => FateAnchor::NotConfirmed,
                Some(None) => FateAnchor::ConfirmedUndated,
                Some(Some(at)) => FateAnchor::ConfirmedAt(*at),
            },
        },
    };
    let facts = |status| FileFacts {
        status,
        batches,
        anchor,
    };

    if pending.contains(abs) {
        return Ok(facts(FileStatus::Queued));
    }

    let mut confirmed = false;
    let mut declined = false;
    for package_ref in &refs {
        // A batch whose outbound rows were history-deleted contributes no
        // verdict — it must not pin the file to a status nothing backs.
        let Some(targets) = newest.get(package_ref.as_str()) else {
            continue;
        };
        // Every target of every batch votes; the arms below are ANY-of, so one
        // still-moving copy outranks a confirmed sibling and one confirmed copy
        // outranks a sibling the other receiver declined.
        for row in targets.values() {
            if !row.state.is_terminal() {
                return Ok(facts(FileStatus::Sending));
            }
            if row.state == OutboundState::Confirmed {
                confirmed = true;
            } else if is_declined(row) {
                declined = true;
            }
        }
    }
    if confirmed {
        return Ok(facts(FileStatus::Delivered));
    }
    if declined {
        return Ok(facts(FileStatus::Declined));
    }
    if src.seen.is_recorded(abs)? {
        return Ok(facts(FileStatus::Sent));
    }
    Ok(facts(FileStatus::Unsent))
}

/// List the directory at `(root, rel)` with each file's derived status.
///
/// `root_idx` is echoed into the payload only. `rel` goes through the containment
/// guard, so the error prefixes are the route's status contract:
/// `"canonicalize root"` (the root itself is offline → `502`), `"not found"`
/// (`404`), `"invalid path segment"` / `"path escapes root"` / `"not a directory"`
/// (`400`). Anything else is a genuine `500`.
///
/// An entry whose metadata cannot be read (a broken symlink, a file removed
/// mid-listing, a permission hole) is skipped with a `warn!` rather than failing
/// the whole directory — one bad entry must not blank the operator's view.
pub fn list_directory(
    root_idx: usize,
    root: &Path,
    rel: &str,
    src: &StatusSources<'_>,
) -> Result<LibraryListing> {
    let segs = split_rel(rel)?;
    let dir = resolve_in_root(root, rel)?;
    if !dir.is_dir() {
        bail!("not a directory: {rel:?}");
    }
    let entries = std::fs::read_dir(&dir).with_context(|| format!("read dir {}", dir.display()))?;

    let pending: HashSet<&Path> = src.pending.iter().map(|(_, file)| file.as_path()).collect();
    let newest = newest_by_target(src.outbound);
    // Resolved through the same seam the retention task uses, so the sentence
    // and the evaluator can never disagree about the threshold.
    let policy = src.retention.to_core_policy();
    // `keep_everything` names nothing, so skip the whole confirm scan AND the
    // per-file linkage lookup it feeds — the evaluator short-circuits at exactly
    // the same point, and this is the default policy (i.e. the common case) on
    // every node.
    let confirms = if matches!(policy, RetentionPolicy::KeepEverything) {
        None
    } else {
        Some(confirm_anchor_by_package(src.outbound))
    };

    let mut dirs: Vec<String> = Vec::new();
    let mut files: Vec<LibraryEntry> = Vec::new();
    for entry in entries {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error) => {
                tracing::warn!(path = %dir.display(), error = %error, "library listing: unreadable dir entry");
                continue;
            }
        };
        // Join the OsString, not the lossy name: a non-UTF-8 filename must still
        // stat correctly (its wire name is lossy, and acting on it fails at the
        // containment guard — the trade `to_wire_rel` already documents).
        let abs = dir.join(entry.file_name());
        // `metadata` FOLLOWS symlinks, so a link to a directory browses as a
        // directory and a link to a file lists as a file — what an operator
        // expects. Containment of any later action on it is still enforced by
        // `resolve_in_root`, which canonicalizes and rejects an escape.
        let meta = match std::fs::metadata(&abs) {
            Ok(meta) => meta,
            Err(error) => {
                tracing::warn!(path = %abs.display(), error = %error, "library listing: skipping unreadable entry");
                continue;
            }
        };
        let name = entry.file_name().to_string_lossy().into_owned();
        if meta.is_dir() {
            dirs.push(name);
            continue;
        }
        if !meta.is_file() {
            tracing::debug!(path = %abs.display(), "library listing: skipping non-regular entry");
            continue;
        }
        // Only capture payloads are listed — the SAME extension gate the watcher
        // enqueues by and the send-selection expands by (`watcher::is_eligible`:
        // fits/fit/fts/xisf, case-insensitive), so the Library never shows a file
        // Perseus would never send (logs, sidecars, temp files).
        if !crate::watcher::is_eligible(&abs) {
            continue;
        }
        let f = status_for(&abs, &pending, &newest, confirms.as_ref(), src)?;
        files.push(LibraryEntry {
            name,
            size: meta.len(),
            mtime_ms: mtime_millis(meta.modified().ok()),
            status: f.status,
            batches: f.batches,
            retention: retention_fate(&policy, src.retention.dry_run, f.anchor),
        });
    }
    dirs.sort();
    files.sort_by(|a, b| a.name.cmp(&b.name));

    Ok(LibraryListing {
        root: root_idx,
        path: segs.join("/"),
        dirs,
        files,
    })
}

#[cfg(test)]
mod tests {
    use crate::library::*;

    use std::path::PathBuf;

    use athenaeum_core::sync::{OutboundRow, OutboundState, CANCELLED_BY_RECEIVER_DETAIL};

    use crate::batch_store::BatchStore;
    use crate::seen::SeenStore;

    const PEER: [u8; 32] = [7u8; 32];

    /// A capture root on disk plus the two Perseus stores the status join reads,
    /// all inside one throwaway tempdir (the guard is held by the fixture).
    struct Fixture {
        _tmp: tempfile::TempDir,
        root: PathBuf,
        batches: BatchStore,
        seen: SeenStore,
        /// The retention knobs the fate line is derived from. Defaults to
        /// `keep_everything`, so every status test above sees `retention: None`
        /// exactly as it did before T15; the fate tests set it explicitly.
        retention: crate::config::RetentionConfig,
    }

    fn fixture() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().join("cap");
        std::fs::create_dir_all(&root).unwrap();
        let db = tmp.path().join("perseus.db");
        let batches = BatchStore::open(&db).unwrap();
        let seen = SeenStore::open(&db).unwrap();
        Fixture {
            _tmp: tmp,
            root,
            batches,
            seen,
            retention: crate::config::RetentionConfig::default(),
        }
    }

    impl Fixture {
        /// Create `rel` under the root (parents included) and return its
        /// CANONICAL absolute path — the spelling the watcher records, and so the
        /// spelling every store key uses.
        fn touch(&self, rel: &str, bytes: &[u8]) -> PathBuf {
            let p = self.root.join(rel);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent).unwrap();
            }
            std::fs::write(&p, bytes).unwrap();
            std::fs::canonicalize(&p).unwrap()
        }

        fn list(
            &self,
            rel: &str,
            pending: &[(PathBuf, PathBuf)],
            outbound: &[OutboundRow],
        ) -> LibraryListing {
            let src = StatusSources {
                pending,
                batches: &self.batches,
                seen: &self.seen,
                outbound,
                retention: &self.retention,
            };
            list_directory(0, &self.root, rel, &src).unwrap()
        }

        fn status_of(&self, listing: &LibraryListing, name: &str) -> FileStatus {
            listing
                .files
                .iter()
                .find(|e| e.name == name)
                .unwrap_or_else(|| panic!("{name} not listed"))
                .status
        }

        fn fate_of(&self, listing: &LibraryListing, name: &str) -> Option<String> {
            listing
                .files
                .iter()
                .find(|e| e.name == name)
                .unwrap_or_else(|| panic!("{name} not listed"))
                .retention
                .clone()
        }
    }

    const PEER_B: [u8; 32] = [9u8; 32];

    /// A bare outbound row: only the fields the status join reads (`id`,
    /// `package_ref`, `peer`, `state`, `last_error`) matter; the rest are inert.
    fn row(
        id: i64,
        package_ref: &str,
        state: OutboundState,
        last_error: Option<&str>,
    ) -> OutboundRow {
        row_to(id, package_ref, PEER, state, last_error)
    }

    /// Same, with an explicit target — a fan-out package has one row per peer
    /// under the SAME `package_ref`.
    fn row_to(
        id: i64,
        package_ref: &str,
        peer: [u8; 32],
        state: OutboundState,
        last_error: Option<&str>,
    ) -> OutboundRow {
        OutboundRow {
            id,
            package_ref: package_ref.to_string(),
            peer,
            state,
            attempts: 0,
            created_at: "2026-07-26T10:00:00.000Z".into(),
            confirmed_at: None,
            last_error: last_error.map(str::to_string),
            next_retry_at: None,
            wire_package_id: None,
            display_name: None,
            project_id: None,
            generation: 1,
            layout: athenaeum_core::sharing::types::PackageLayout::Batch,
        }
    }

    // ── status arms ──────────────────────────────────────────────────────────

    #[test]
    fn unsent_when_no_store_knows_the_file() {
        let f = fixture();
        f.touch("a.fits", b"x");
        let listing = f.list("", &[], &[]);
        assert_eq!(f.status_of(&listing, "a.fits"), FileStatus::Unsent);
        assert_eq!(listing.files[0].batches, 0);
        assert_eq!(listing.files[0].retention, None, "T15 fills this, not T4");
    }

    #[test]
    fn queued_when_in_the_batcher_pending_set() {
        let f = fixture();
        let abs = f.touch("a.fits", b"x");
        let pending = vec![(f.root.clone(), abs)];
        let listing = f.list("", &pending, &[]);
        assert_eq!(f.status_of(&listing, "a.fits"), FileStatus::Queued);
    }

    #[test]
    fn sending_when_the_newest_outbound_row_is_live() {
        let f = fixture();
        let abs = f.touch("a.fits", b"x");
        f.batches
            .record_files("/pkg/u1", &[("a.fits".into(), abs)])
            .unwrap();
        let outbound = vec![row(1, "/pkg/u1", OutboundState::Announced, None)];
        let listing = f.list("", &[], &outbound);
        assert_eq!(f.status_of(&listing, "a.fits"), FileStatus::Sending);
    }

    #[test]
    fn delivered_when_the_newest_row_is_confirmed() {
        let f = fixture();
        let abs = f.touch("a.fits", b"x");
        f.batches
            .record_files("/pkg/u1", &[("a.fits".into(), abs)])
            .unwrap();
        let outbound = vec![row(1, "/pkg/u1", OutboundState::Confirmed, None)];
        let listing = f.list("", &[], &outbound);
        assert_eq!(f.status_of(&listing, "a.fits"), FileStatus::Delivered);
    }

    #[test]
    fn declined_when_the_newest_row_is_a_receiver_decline() {
        let f = fixture();
        let abs = f.touch("a.fits", b"x");
        f.batches
            .record_files("/pkg/u1", &[("a.fits".into(), abs)])
            .unwrap();
        let outbound = vec![row(
            1,
            "/pkg/u1",
            OutboundState::Cancelled,
            Some(CANCELLED_BY_RECEIVER_DETAIL),
        )];
        let listing = f.list("", &[], &outbound);
        assert_eq!(f.status_of(&listing, "a.fits"), FileStatus::Declined);
    }

    #[test]
    fn sent_when_only_the_seen_store_recorded_it() {
        let f = fixture();
        let abs = f.touch("a.fits", b"x");
        f.seen.mark_enqueued(&abs, 1, 1, "/pkg/u1").unwrap();
        let listing = f.list("", &[], &[]);
        assert_eq!(
            f.status_of(&listing, "a.fits"),
            FileStatus::Sent,
            "recorded as handed to the engine, but no live batch row explains more"
        );
    }

    /// A user-cancelled (NOT receiver-declined) batch falls through the batch
    /// arms; the seen linkage is what still says "this left the node once".
    #[test]
    fn locally_cancelled_batch_falls_through_to_sent() {
        let f = fixture();
        let abs = f.touch("a.fits", b"x");
        f.batches
            .record_files("/pkg/u1", &[("a.fits".into(), abs.clone())])
            .unwrap();
        f.seen.mark_enqueued(&abs, 1, 1, "/pkg/u1").unwrap();
        let outbound = vec![row(
            1,
            "/pkg/u1",
            OutboundState::Cancelled,
            Some("operator cancelled"),
        )];
        let listing = f.list("", &[], &outbound);
        assert_eq!(f.status_of(&listing, "a.fits"), FileStatus::Sent);
    }

    // ── precedence ───────────────────────────────────────────────────────────

    #[test]
    fn pending_beats_every_batch_and_seen_fact() {
        let f = fixture();
        let abs = f.touch("a.fits", b"x");
        f.batches
            .record_files("/pkg/u1", &[("a.fits".into(), abs.clone())])
            .unwrap();
        f.seen.mark_enqueued(&abs, 1, 1, "/pkg/u1").unwrap();
        let outbound = vec![row(1, "/pkg/u1", OutboundState::Confirmed, None)];
        let pending = vec![(f.root.clone(), abs)];
        let listing = f.list("", &pending, &outbound);
        assert_eq!(
            f.status_of(&listing, "a.fits"),
            FileStatus::Queued,
            "a re-captured file waiting for the next flush is Queued, not Delivered"
        );
    }

    #[test]
    fn a_live_batch_beats_a_confirmed_sibling_batch() {
        let f = fixture();
        let abs = f.touch("a.fits", b"x");
        f.batches
            .record_files("/pkg/old", &[("a.fits".into(), abs.clone())])
            .unwrap();
        f.batches
            .record_files("/pkg/new", &[("a.fits".into(), abs)])
            .unwrap();
        let outbound = vec![
            row(1, "/pkg/old", OutboundState::Confirmed, None),
            row(2, "/pkg/new", OutboundState::Transferring, None),
        ];
        let listing = f.list("", &[], &outbound);
        assert_eq!(f.status_of(&listing, "a.fits"), FileStatus::Sending);
        assert_eq!(listing.files[0].batches, 2, "both participations counted");
    }

    /// Attempt N of ONE package is the highest-id row for that `package_ref`;
    /// an older attempt's terminal must not decide the status.
    #[test]
    fn newest_row_of_a_package_wins_over_the_older_attempt() {
        let f = fixture();
        let abs = f.touch("a.fits", b"x");
        f.batches
            .record_files("/pkg/u1", &[("a.fits".into(), abs)])
            .unwrap();

        let stale_confirm = vec![
            row(9, "/pkg/u1", OutboundState::Announced, None),
            row(4, "/pkg/u1", OutboundState::Confirmed, None),
        ];
        assert_eq!(
            f.status_of(&f.list("", &[], &stale_confirm), "a.fits"),
            FileStatus::Sending,
            "the newest attempt is in flight"
        );

        let fresh_confirm = vec![
            row(4, "/pkg/u1", OutboundState::Announced, None),
            row(9, "/pkg/u1", OutboundState::Confirmed, None),
        ];
        assert_eq!(
            f.status_of(&f.list("", &[], &fresh_confirm), "a.fits"),
            FileStatus::Delivered,
            "the newest attempt confirmed"
        );
    }

    // ── fan-out: one package_ref, one row per target ─────────────────────────

    /// A package sent to two targets has two rows sharing one `package_ref`.
    /// Collapsing them by id alone would let the last-enqueued target speak for
    /// the whole fan-out — here the NAS copy is still moving and must win.
    #[test]
    fn a_live_target_beats_a_confirmed_sibling_target() {
        let f = fixture();
        let abs = f.touch("a.fits", b"x");
        f.batches
            .record_files("/pkg/u1", &[("a.fits".into(), abs)])
            .unwrap();
        let outbound = vec![
            row_to(1, "/pkg/u1", PEER, OutboundState::Transferring, None),
            row_to(2, "/pkg/u1", PEER_B, OutboundState::Confirmed, None),
        ];
        assert_eq!(
            f.status_of(&f.list("", &[], &outbound), "a.fits"),
            FileStatus::Sending,
            "one target confirming does not finish the fan-out"
        );
    }

    /// Landing on ANY target is delivery for the library chip: the file exists
    /// somewhere off this node. Per-target detail is the Transfers tab's job.
    #[test]
    fn a_confirmed_target_beats_a_declined_sibling_target() {
        let f = fixture();
        let abs = f.touch("a.fits", b"x");
        f.batches
            .record_files("/pkg/u1", &[("a.fits".into(), abs)])
            .unwrap();
        let outbound = vec![
            row_to(1, "/pkg/u1", PEER, OutboundState::Confirmed, None),
            row_to(
                2,
                "/pkg/u1",
                PEER_B,
                OutboundState::Cancelled,
                Some(CANCELLED_BY_RECEIVER_DETAIL),
            ),
        ];
        assert_eq!(
            f.status_of(&f.list("", &[], &outbound), "a.fits"),
            FileStatus::Delivered,
            "it WAS delivered — one receiver declining does not unsend it"
        );
    }

    /// Only when every target declined is the file honestly `Declined`.
    #[test]
    fn every_target_declining_is_declined() {
        let f = fixture();
        let abs = f.touch("a.fits", b"x");
        f.batches
            .record_files("/pkg/u1", &[("a.fits".into(), abs)])
            .unwrap();
        let outbound = vec![
            row_to(
                1,
                "/pkg/u1",
                PEER,
                OutboundState::Cancelled,
                Some(CANCELLED_BY_RECEIVER_DETAIL),
            ),
            row_to(
                2,
                "/pkg/u1",
                PEER_B,
                OutboundState::Cancelled,
                Some(CANCELLED_BY_RECEIVER_DETAIL),
            ),
        ];
        assert_eq!(
            f.status_of(&f.list("", &[], &outbound), "a.fits"),
            FileStatus::Declined
        );
    }

    /// The per-attempt collapse still applies WITHIN a target: a resend resets
    /// the same row, but a stale read window can hold both spellings.
    #[test]
    fn newest_attempt_wins_per_target_not_across_targets() {
        let f = fixture();
        let abs = f.touch("a.fits", b"x");
        f.batches
            .record_files("/pkg/u1", &[("a.fits".into(), abs)])
            .unwrap();
        let outbound = vec![
            row_to(1, "/pkg/u1", PEER, OutboundState::Confirmed, None),
            // Same target, older attempt — must not resurrect a live status.
            row_to(9, "/pkg/u1", PEER_B, OutboundState::Confirmed, None),
            row_to(3, "/pkg/u1", PEER_B, OutboundState::Announced, None),
        ];
        assert_eq!(
            f.status_of(&f.list("", &[], &outbound), "a.fits"),
            FileStatus::Delivered,
            "both targets' newest attempts confirmed"
        );
    }

    /// A batch whose outbound rows were history-deleted contributes no verdict —
    /// it must not pin the file to a stale status.
    #[test]
    fn a_batch_with_no_outbound_rows_contributes_nothing() {
        let f = fixture();
        let abs = f.touch("a.fits", b"x");
        f.batches
            .record_files("/pkg/gone", &[("a.fits".into(), abs)])
            .unwrap();
        let listing = f.list("", &[], &[]);
        assert_eq!(f.status_of(&listing, "a.fits"), FileStatus::Unsent);
        assert_eq!(
            listing.files[0].batches, 1,
            "the participation is still real"
        );
    }

    #[test]
    fn a_retention_deleted_seen_row_is_not_recorded_anymore() {
        let f = fixture();
        let abs = f.touch("a.fits", b"x");
        f.seen.mark_enqueued(&abs, 1, 1, "/pkg/u1").unwrap();
        f.seen.mark_deleted(&abs).unwrap();
        assert_eq!(
            f.status_of(&f.list("", &[], &[]), "a.fits"),
            FileStatus::Unsent
        );
    }

    // ── listing shape ────────────────────────────────────────────────────────

    /// Only capture payloads list — the same `watcher::is_eligible` gate the
    /// enqueue and send-selection paths use (fits/fit/fts/xisf, any case).
    /// Logs, sidecars and temp files in the capture dir stay invisible;
    /// directories always list (they are navigation, not payloads).
    #[test]
    fn listing_shows_only_capture_extensions() {
        let f = fixture();
        f.touch("a.fits", b"x");
        f.touch("b.FIT", b"x");
        f.touch("c.xisf", b"x");
        f.touch("d.fts", b"x");
        f.touch("session.log", b"x");
        f.touch("notes.txt", b"x");
        f.touch("frame.fits.tmp", b"x");
        f.touch("Lights/keep.me", b"x");
        let listing = f.list("", &[], &[]);
        let names: Vec<_> = listing.files.iter().map(|e| e.name.clone()).collect();
        assert_eq!(
            names,
            vec![
                "a.fits".to_string(),
                "b.FIT".to_string(),
                "c.xisf".to_string(),
                "d.fts".to_string()
            ],
            "non-capture files never list"
        );
        assert_eq!(
            listing.dirs,
            vec!["Lights".to_string()],
            "directories list regardless of their contents"
        );
    }

    #[test]
    fn listing_is_a_single_directory_never_a_walk() {
        let f = fixture();
        f.touch("top.fits", b"x");
        f.touch("M31/nested.fits", b"x");
        let listing = f.list("", &[], &[]);
        assert_eq!(listing.root, 0);
        assert_eq!(listing.path, "");
        assert_eq!(listing.dirs, vec!["M31".to_string()]);
        let names: Vec<_> = listing.files.iter().map(|e| e.name.clone()).collect();
        assert_eq!(
            names,
            vec!["top.fits".to_string()],
            "no nested file at root level"
        );

        let sub = f.list("M31", &[], &[]);
        assert_eq!(sub.path, "M31");
        assert!(sub.dirs.is_empty());
        assert_eq!(sub.files.len(), 1);
        assert_eq!(sub.files[0].name, "nested.fits");
    }

    #[test]
    fn dirs_and_files_are_sorted_by_name() {
        let f = fixture();
        for name in ["c.fits", "a.fits", "b.fits"] {
            f.touch(name, b"x");
        }
        for dir in ["zeta", "alpha", "mid"] {
            std::fs::create_dir_all(f.root.join(dir)).unwrap();
        }
        let listing = f.list("", &[], &[]);
        assert_eq!(listing.dirs, vec!["alpha", "mid", "zeta"]);
        let names: Vec<_> = listing.files.iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, vec!["a.fits", "b.fits", "c.fits"]);
    }

    /// REVERSED 2026-07-28 (owner call): the listing is a CAPTURE browser, not a
    /// file manager — it shows exactly what Perseus would send. The v1
    /// browse-everything stance (this test's original assertion) put logs and
    /// sidecars in front of the operator with statuses that could only ever
    /// read "unsent".
    #[test]
    fn non_fits_files_are_hidden() {
        let f = fixture();
        f.touch("notes.txt", b"hello");
        f.touch("light.fits", b"x");
        let listing = f.list("", &[], &[]);
        let names: Vec<_> = listing.files.iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, vec!["light.fits"]);
    }

    #[test]
    fn entries_report_size_and_mtime() {
        let f = fixture();
        f.touch("a.fits", b"0123456789");
        let listing = f.list("", &[], &[]);
        assert_eq!(listing.files[0].size, 10);
        assert!(listing.files[0].mtime_ms > 0, "a real mtime is reported");
    }

    // ── error contract ───────────────────────────────────────────────────────

    /// An offline capture root (a share that did not mount) is the T3
    /// offline-at-boot case: the error keeps the `"canonicalize root"` prefix the
    /// route maps to `502 root unavailable`.
    #[test]
    fn an_offline_root_keeps_the_canonicalize_root_prefix() {
        let f = fixture();
        let gone = f.root.join("never-existed");
        let src = StatusSources {
            pending: &[],
            batches: &f.batches,
            seen: &f.seen,
            outbound: &[],
            retention: &f.retention,
        };
        let err = list_directory(0, &gone, "", &src).unwrap_err().to_string();
        assert!(err.starts_with("canonicalize root"), "got {err:?}");
    }

    #[test]
    fn a_missing_subdirectory_keeps_the_not_found_prefix() {
        let f = fixture();
        let src = StatusSources {
            pending: &[],
            batches: &f.batches,
            seen: &f.seen,
            outbound: &[],
            retention: &f.retention,
        };
        let err = list_directory(0, &f.root, "nope", &src)
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("not found"), "got {err:?}");
    }

    #[test]
    fn a_hostile_rel_path_keeps_the_invalid_segment_prefix() {
        let f = fixture();
        let src = StatusSources {
            pending: &[],
            batches: &f.batches,
            seen: &f.seen,
            outbound: &[],
            retention: &f.retention,
        };
        let err = list_directory(0, &f.root, "..", &src)
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("invalid path segment"), "got {err:?}");
    }

    /// Listing a FILE is a client mistake, not a 500: its own stable prefix.
    #[test]
    fn listing_a_file_keeps_the_not_a_directory_prefix() {
        let f = fixture();
        f.touch("a.fits", b"x");
        let src = StatusSources {
            pending: &[],
            batches: &f.batches,
            seen: &f.seen,
            outbound: &[],
            retention: &f.retention,
        };
        let err = list_directory(0, &f.root, "a.fits", &src)
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("not a directory"), "got {err:?}");
    }

    // ── retention fate (T15, spec §8) ────────────────────────────────────────
    //
    // The contract these pin is "the fate line never lies": every arm below was
    // derived by reading `sync/retention.rs::evaluate_and_apply`, not the card
    // copy. Where the spec's prose and the evaluator disagree, the evaluator
    // wins and the test says so.

    use athenaeum_core::sync::RetentionPolicy as CorePolicy;
    use chrono::{DateTime, Utc};

    fn at(ts: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(ts)
            .unwrap()
            .with_timezone(&Utc)
    }

    /// A dated confirm anchor — what a file whose LIVE linkage confirmed at `ts`
    /// resolves to.
    fn dated(ts: &str) -> FateAnchor {
        FateAnchor::ConfirmedAt(at(ts))
    }

    /// A confirmed row with an explicit `confirmed_at` — the only timestamp the
    /// evaluator's `keep_days` arm reads.
    fn confirmed_row(
        id: i64,
        package_ref: &str,
        peer: [u8; 32],
        confirmed_at: &str,
    ) -> OutboundRow {
        OutboundRow {
            confirmed_at: Some(confirmed_at.to_string()),
            ..row_to(id, package_ref, peer, OutboundState::Confirmed, None)
        }
    }

    /// `keep_everything` short-circuits before any candidate work — no line at
    /// all, in either mode, confirmed or not.
    #[test]
    fn keep_everything_never_shows_a_fate_line() {
        for dry_run in [true, false] {
            for anchor in [
                dated("2026-07-01T00:00:00Z"),
                FateAnchor::NotConfirmed,
                FateAnchor::ConfirmedUndated,
            ] {
                assert_eq!(
                    retention_fate(&CorePolicy::KeepEverything, dry_run, anchor),
                    None
                );
            }
        }
    }

    #[test]
    fn keep_days_names_the_date_n_days_after_the_confirm() {
        assert_eq!(
            retention_fate(
                &CorePolicy::KeepDays(21),
                false,
                dated("2026-07-12T22:30:00Z")
            )
            .unwrap(),
            "deletable after 2026-08-02",
            "the clock starts at confirmed_at, never at capture time"
        );
    }

    #[test]
    fn keep_days_dry_run_prefixes_would_be() {
        assert_eq!(
            retention_fate(
                &CorePolicy::KeepDays(21),
                true,
                dated("2026-07-12T22:30:00Z")
            )
            .unwrap(),
            "would be deletable after 2026-08-02"
        );
    }

    #[test]
    fn keep_days_without_a_confirm_is_kept_until_sent_and_confirmed() {
        for dry_run in [true, false] {
            assert_eq!(
                retention_fate(&CorePolicy::KeepDays(21), dry_run, FateAnchor::NotConfirmed)
                    .unwrap(),
                "kept until sent and confirmed",
                "no confirmed row ⇒ the single chokepoint never names it"
            );
        }
    }

    /// A confirm that aged out before the pass could run renders a PAST date.
    /// That is the honest statement — the file is eligible and waiting for the
    /// next pass — and inventing "any moment now" would promise a schedule the
    /// retention loop does not expose.
    #[test]
    fn a_long_past_confirm_renders_a_past_date_not_a_softened_phrase() {
        assert_eq!(
            retention_fate(
                &CorePolicy::KeepDays(7),
                false,
                dated("2020-01-01T00:00:00Z")
            )
            .unwrap(),
            "deletable after 2020-01-08"
        );
        assert_eq!(
            retention_fate(
                &CorePolicy::KeepDays(7),
                true,
                dated("2020-01-01T00:00:00Z")
            )
            .unwrap(),
            "would be deletable after 2020-01-08"
        );
    }

    #[test]
    fn disk_pct_says_deleted_under_pressure_oldest_first() {
        assert_eq!(
            retention_fate(
                &CorePolicy::DiskPct { max_pct: 80 },
                false,
                dated("2026-07-01T00:00:00Z")
            )
            .unwrap(),
            "deleted under disk pressure, oldest first"
        );
    }

    #[test]
    fn disk_pct_dry_run_prefixes_would_be() {
        assert_eq!(
            retention_fate(
                &CorePolicy::DiskPct { max_pct: 80 },
                true,
                dated("2026-07-01T00:00:00Z")
            )
            .unwrap(),
            "would be deleted under disk pressure, oldest first"
        );
    }

    /// `disk_pct` still draws its candidates from the confirmed-only chokepoint,
    /// so an unconfirmed file is NOT a pressure candidate — saying otherwise
    /// would be the exact lie this feature exists to prevent.
    #[test]
    fn disk_pct_without_a_confirm_is_kept_until_sent_and_confirmed() {
        assert_eq!(
            retention_fate(
                &CorePolicy::DiskPct { max_pct: 80 },
                false,
                FateAnchor::NotConfirmed
            )
            .unwrap(),
            "kept until sent and confirmed"
        );
    }

    /// `on_confirm` is a real, configurable policy (`config.rs`) the spec's card
    /// copy does not enumerate. Omitting a line for it would hide the most
    /// aggressive policy Perseus has.
    #[test]
    fn on_confirm_says_the_next_pass() {
        assert_eq!(
            retention_fate(&CorePolicy::OnConfirm, false, dated("2026-07-01T00:00:00Z")).unwrap(),
            "deleted at the next retention pass"
        );
        assert_eq!(
            retention_fate(&CorePolicy::OnConfirm, true, dated("2026-07-01T00:00:00Z")).unwrap(),
            "would be deleted at the next retention pass"
        );
        assert_eq!(
            retention_fate(&CorePolicy::OnConfirm, false, FateAnchor::NotConfirmed).unwrap(),
            "kept until sent and confirmed"
        );
    }

    /// The timestamp-free policies do not read `confirmed_at` at all — the
    /// evaluator makes every confirmed row a candidate, dated or not — so an
    /// undated confirm must still state the deletion, never "kept".
    #[test]
    fn an_undated_confirm_still_names_the_timestamp_free_policies() {
        assert_eq!(
            retention_fate(&CorePolicy::OnConfirm, false, FateAnchor::ConfirmedUndated).unwrap(),
            "deleted at the next retention pass"
        );
        assert_eq!(
            retention_fate(
                &CorePolicy::DiskPct { max_pct: 80 },
                true,
                FateAnchor::ConfirmedUndated
            )
            .unwrap(),
            "would be deleted under disk pressure, oldest first"
        );
    }

    /// The two `keep_days` states with no honest date degrade IDENTICALLY: a
    /// confirm whose timestamp is unusable, and a threshold too large to add to
    /// a date. Neither may print a wrong date, and neither may claim "kept until
    /// sent and confirmed" — that would deny a confirm that happened. No line.
    #[test]
    fn keep_days_degrades_to_no_line_when_no_honest_date_exists() {
        assert_eq!(
            retention_fate(
                &CorePolicy::KeepDays(7),
                false,
                FateAnchor::ConfirmedUndated
            ),
            None,
            "confirmed, but no usable instant"
        );
        assert_eq!(
            retention_fate(
                &CorePolicy::KeepDays(u32::MAX),
                false,
                dated("2026-07-01T00:00:00Z")
            ),
            None,
            "a hand-edited threshold that cannot be added to a date"
        );
    }

    // ── the join: which confirm anchors the clock ────────────────────────────

    /// A fan-out package has one row per target. `store.confirmed()` returns
    /// EVERY confirmed row, and `confirmed_age_reached` is applied per row — so
    /// the package becomes a candidate the moment the EARLIEST confirm ages out,
    /// not when the last target catches up.
    #[test]
    fn the_clock_starts_at_the_earliest_confirm_across_targets() {
        let mut f = fixture();
        f.retention.policy = crate::config::RetentionPolicy::KeepDays;
        f.retention.keep_days = 10;
        f.retention.dry_run = false;
        let abs = f.touch("a.fits", b"x");
        f.batches
            .record_files("/pkg/u1", &[("a.fits".into(), abs.clone())])
            .unwrap();
        f.seen.mark_enqueued(&abs, 1, 1, "/pkg/u1").unwrap();
        let outbound = vec![
            confirmed_row(1, "/pkg/u1", PEER, "2026-07-05T00:00:00Z"),
            confirmed_row(2, "/pkg/u1", PEER_B, "2026-07-01T00:00:00Z"),
        ];
        assert_eq!(
            f.fate_of(&f.list("", &[], &outbound), "a.fits").unwrap(),
            "deletable after 2026-07-11",
            "the earliest confirm ages out first — the later target does not delay it"
        );
    }

    /// The anchor follows the LIVE seen linkage, and a re-send moves it.
    ///
    /// `perseus_seen.path` is the PRIMARY KEY and `mark_enqueued` overwrites
    /// `package_ref`, while the deleter only ever resolves candidates through
    /// `sources_for_package` (`deleted_at IS NULL`). So once `/pkg/new` carries
    /// the file, `/pkg/old`'s ancient confirm can NEVER delete it — printing its
    /// date would name a deadline that can never fire. The honest line while the
    /// new package is unconfirmed is the kept-until arm.
    #[test]
    fn a_resend_moves_the_fate_onto_the_new_live_package() {
        let mut f = fixture();
        f.retention.policy = crate::config::RetentionPolicy::KeepDays;
        f.retention.keep_days = 7;
        let abs = f.touch("a.fits", b"x");
        f.batches
            .record_files("/pkg/old", &[("a.fits".into(), abs.clone())])
            .unwrap();
        f.seen.mark_enqueued(&abs, 1, 1, "/pkg/old").unwrap();
        let outbound = vec![
            confirmed_row(1, "/pkg/old", PEER, "2026-07-01T00:00:00Z"),
            row(2, "/pkg/new", OutboundState::Transferring, None),
        ];

        // Before the re-send the old package IS the live linkage.
        assert_eq!(
            f.fate_of(&f.list("", &[], &outbound), "a.fits").unwrap(),
            "would be deletable after 2026-07-08"
        );

        // The re-send: a second package carries the file, and the seen row moves
        // with it — exactly what the batcher does on the next enqueue.
        f.batches
            .record_files("/pkg/new", &[("a.fits".into(), abs.clone())])
            .unwrap();
        f.seen.mark_enqueued(&abs, 1, 1, "/pkg/new").unwrap();

        let listing = f.list("", &[], &outbound);
        assert_eq!(f.status_of(&listing, "a.fits"), FileStatus::Sending);
        assert_eq!(listing.files[0].batches, 2, "both participations counted");
        assert_eq!(
            f.fate_of(&listing, "a.fits").unwrap(),
            "kept until sent and confirmed",
            "the old package lost the linkage; its confirm can never delete this file"
        );
    }

    /// A file the operator (or retention) deleted and the camera media then
    /// re-copied has a `deleted_at`-stamped seen row: NO live linkage, so no
    /// package can name it, however long ago the old one confirmed. It is a new
    /// capture waiting to be sent.
    #[test]
    fn a_reappeared_file_has_no_live_linkage_and_reads_kept() {
        let mut f = fixture();
        f.retention.policy = crate::config::RetentionPolicy::KeepDays;
        f.retention.keep_days = 7;
        let abs = f.touch("a.fits", b"x");
        f.batches
            .record_files("/pkg/u1", &[("a.fits".into(), abs.clone())])
            .unwrap();
        f.seen.mark_enqueued(&abs, 1, 1, "/pkg/u1").unwrap();
        f.seen.mark_deleted(&abs).unwrap();
        let outbound = vec![confirmed_row(1, "/pkg/u1", PEER, "2026-07-01T00:00:00Z")];

        let listing = f.list("", &[], &outbound);
        assert_eq!(
            f.status_of(&listing, "a.fits"),
            FileStatus::Delivered,
            "the batch history is still real"
        );
        assert_eq!(
            f.fate_of(&listing, "a.fits").unwrap(),
            "kept until sent and confirmed",
            "a corpse row is not a linkage — this file is unsent again"
        );
    }

    /// The evaluator fails SAFE on a missing / unparseable `confirmed_at` (it
    /// warns and refuses eligibility). The fate line mirrors that with NO line —
    /// not the kept-until arm, which would deny the confirm that happened.
    #[test]
    fn a_confirmed_row_without_a_usable_timestamp_names_no_date() {
        let mut f = fixture();
        f.retention.policy = crate::config::RetentionPolicy::KeepDays;
        f.retention.keep_days = 7;
        let abs = f.touch("a.fits", b"x");
        f.batches
            .record_files("/pkg/u1", &[("a.fits".into(), abs.clone())])
            .unwrap();
        f.seen.mark_enqueued(&abs, 1, 1, "/pkg/u1").unwrap();

        let missing = vec![row(1, "/pkg/u1", OutboundState::Confirmed, None)];
        assert_eq!(f.fate_of(&f.list("", &[], &missing), "a.fits"), None);

        let garbage = vec![confirmed_row(1, "/pkg/u1", PEER, "not-a-timestamp")];
        assert_eq!(f.fate_of(&f.list("", &[], &garbage), "a.fits"), None);
    }

    /// The fate line is NOT gated on the status chip: a re-captured file waiting
    /// in the batcher reads `queued`, while its live linkage — the package that
    /// carried the previous copy — confirmed long enough ago to name a date.
    /// Printing "kept" here would cover a file that is about to be removed.
    #[test]
    fn a_queued_recapture_still_shows_its_live_packages_deadline() {
        let mut f = fixture();
        f.retention.policy = crate::config::RetentionPolicy::KeepDays;
        f.retention.keep_days = 7;
        let abs = f.touch("a.fits", b"x");
        f.batches
            .record_files("/pkg/u1", &[("a.fits".into(), abs.clone())])
            .unwrap();
        f.seen.mark_enqueued(&abs, 1, 1, "/pkg/u1").unwrap();
        let outbound = vec![confirmed_row(1, "/pkg/u1", PEER, "2026-07-01T00:00:00Z")];
        let pending = vec![(f.root.clone(), abs)];

        let listing = f.list("", &pending, &outbound);
        assert_eq!(f.status_of(&listing, "a.fits"), FileStatus::Queued);
        assert_eq!(
            f.fate_of(&listing, "a.fits").unwrap(),
            "would be deletable after 2026-07-08",
            "the evaluator does not care that a re-capture is waiting to flush"
        );
    }

    /// The default policy leaves every row's `retention` null — the whole field
    /// is absent from the UI on a node that never deletes.
    #[test]
    fn keep_everything_leaves_every_entry_without_a_fate_line() {
        let f = fixture(); // RetentionConfig::default() == keep_everything
        let abs = f.touch("a.fits", b"x");
        f.batches
            .record_files("/pkg/u1", &[("a.fits".into(), abs)])
            .unwrap();
        let outbound = vec![confirmed_row(1, "/pkg/u1", PEER, "2026-07-01T00:00:00Z")];
        let listing = f.list("", &[], &outbound);
        assert_eq!(f.status_of(&listing, "a.fits"), FileStatus::Delivered);
        assert_eq!(f.fate_of(&listing, "a.fits"), None);
    }

    /// The wire enum is camelCase-stable — the UI (T5) switches on these strings.
    #[test]
    fn file_status_serializes_lowercase() {
        let json = serde_json::to_string(&[
            FileStatus::Unsent,
            FileStatus::Queued,
            FileStatus::Sending,
            FileStatus::Delivered,
            FileStatus::Declined,
            FileStatus::Sent,
        ])
        .unwrap();
        assert_eq!(
            json,
            r#"["unsent","queued","sending","delivered","declined","sent"]"#
        );
    }
}
