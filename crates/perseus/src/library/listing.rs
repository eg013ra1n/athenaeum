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
//! | [`BatchStore::batches_for_source`] + `sync_outbound` | which packages carried it, and how each package's NEWEST attempt ended |
//! | [`SeenStore::is_recorded`] | it was handed to the engine at least once and retention has not removed it |
//!
//! Precedence runs newest-fact-first, because a file can be several of these at
//! once (a re-captured frame is both `Queued` now and `Delivered` from a previous
//! batch — the operator cares about the pending send):
//!
//! 1. in the pending set → [`Queued`](FileStatus::Queued);
//! 2. else any batch whose newest attempt is non-terminal → [`Sending`](FileStatus::Sending);
//! 3. else any batch whose newest attempt is `Confirmed` → [`Delivered`](FileStatus::Delivered);
//! 4. else any batch whose newest attempt is a **receiver decline** → [`Declined`](FileStatus::Declined);
//! 5. else a live seen row → [`Sent`](FileStatus::Sent);
//! 6. else [`Unsent`](FileStatus::Unsent).
//!
//! A locally-failed or operator-cancelled batch deliberately falls through arms
//! 2–4: neither says anything about the file's fate that arm 5 does not say more
//! honestly.
//!
//! # Path spelling is the join key
//!
//! Every store keys on the path spelling the watcher recorded, which is
//! **canonicalized** (`spawn_watcher` canonicalizes the root, and each discovered
//! file, precisely so `notify` and the poll sweep agree). This module matches
//! that by resolving the browsed directory ONCE through
//! [`resolve_in_root`](super::resolve_in_root) and joining entry names onto the
//! canonical result — never by calling [`to_wire_rel`](super::to_wire_rel) per
//! row, which would cost two canonicalizations per file.
//!
//! The one spelling that still diverges is an in-root **symlink to a file**: the
//! watcher records the link's target, this listing keys on the link's own path,
//! so such an entry reads `Unsent` even after its target was sent. Listing the
//! link under its own name is the deliberate half of that trade (a listing must
//! show what the directory contains); the status is honestly conservative.

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use athenaeum_core::sync::{OutboundRow, OutboundState};

use crate::batch_store::BatchStore;
use crate::resend::is_declined;
use crate::seen::{mtime_millis, SeenStore};

use super::{resolve_in_root, split_rel};

/// The derived send fate of one capture file. Serialized lowercase — the wire
/// values the Library UI switches on.
#[derive(serde::Serialize, PartialEq, Eq, Debug, Clone, Copy)]
#[serde(rename_all = "camelCase")]
pub enum FileStatus {
    /// No store knows this file: it has never been enqueued.
    Unsent,
    /// Accumulated in the batcher, awaiting the next flush.
    Queued,
    /// At least one batch carrying it is in flight (newest attempt non-terminal).
    Sending,
    /// A batch carrying it was confirmed by the receiver.
    Delivered,
    /// A batch carrying it was declined by the receiver (final per batch).
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
    /// What retention intends to do with this file. Always `None` here — the
    /// field ships now so the wire shape is stable, and T15 fills it.
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
    /// per `package_ref` is picked here, not by the caller.
    pub outbound: &'a [OutboundRow],
}

/// The newest attempt of each package: highest `sync_outbound.id` wins, since a
/// resend/divert always mints a higher id than the attempt it supersedes.
fn newest_by_package(outbound: &[OutboundRow]) -> HashMap<&str, &OutboundRow> {
    let mut map: HashMap<&str, &OutboundRow> = HashMap::new();
    for row in outbound {
        map.entry(row.package_ref.as_str())
            .and_modify(|cur| {
                if row.id > cur.id {
                    *cur = row;
                }
            })
            .or_insert(row);
    }
    map
}

/// Derive one file's status and batch-participation count. See the module docs
/// for the precedence; the batch lookup runs unconditionally so `batches` is
/// honest even for a file whose status came from an earlier arm.
fn status_for(
    abs: &Path,
    pending: &HashSet<&Path>,
    newest: &HashMap<&str, &OutboundRow>,
    src: &StatusSources<'_>,
) -> Result<(FileStatus, usize)> {
    let refs = src.batches.batches_for_source(&abs.to_string_lossy())?;
    let batches = refs.len();

    if pending.contains(abs) {
        return Ok((FileStatus::Queued, batches));
    }

    let mut confirmed = false;
    let mut declined = false;
    for package_ref in &refs {
        // A batch whose outbound rows were history-deleted contributes no
        // verdict — it must not pin the file to a status nothing backs.
        let Some(row) = newest.get(package_ref.as_str()) else {
            continue;
        };
        if !row.state.is_terminal() {
            return Ok((FileStatus::Sending, batches));
        }
        if row.state == OutboundState::Confirmed {
            confirmed = true;
        } else if is_declined(row) {
            declined = true;
        }
    }
    if confirmed {
        return Ok((FileStatus::Delivered, batches));
    }
    if declined {
        return Ok((FileStatus::Declined, batches));
    }
    if src.seen.is_recorded(abs)? {
        return Ok((FileStatus::Sent, batches));
    }
    Ok((FileStatus::Unsent, batches))
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
    let newest = newest_by_package(src.outbound);

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
        let (status, batches) = status_for(&abs, &pending, &newest, src)?;
        files.push(LibraryEntry {
            name,
            size: meta.len(),
            mtime_ms: mtime_millis(meta.modified().ok()),
            status,
            batches,
            // T15 fills this; the field ships now so the wire shape is stable.
            retention: None,
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
    }

    /// A bare outbound row: only the fields the status join reads (`id`,
    /// `package_ref`, `state`, `last_error`) matter; the rest are inert.
    fn row(
        id: i64,
        package_ref: &str,
        state: OutboundState,
        last_error: Option<&str>,
    ) -> OutboundRow {
        OutboundRow {
            id,
            package_ref: package_ref.to_string(),
            peer: PEER,
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

    /// Browse-everything: the listing is a file manager, not a capture filter.
    /// Only send/preview care about the extension.
    #[test]
    fn non_fits_files_are_listed_too() {
        let f = fixture();
        f.touch("notes.txt", b"hello");
        f.touch("light.fits", b"x");
        let listing = f.list("", &[], &[]);
        let names: Vec<_> = listing.files.iter().map(|e| e.name.clone()).collect();
        assert_eq!(names, vec!["light.fits", "notes.txt"]);
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
        };
        let err = list_directory(0, &f.root, "a.fits", &src)
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("not a directory"), "got {err:?}");
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
