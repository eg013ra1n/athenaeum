//! Deleting capture files from the library browser — allowed always, honest
//! always (spec §2).
//!
//! The owner requirement this implements is blunt: the operator may delete
//! **anything at any moment**. So this module never refuses a deletion because
//! of what the file is *doing* — queued, mid-transfer, already delivered are all
//! deletable. Instead it makes the consequence explicit **before** the fact
//! ([`delete_preview`]) and reports exactly what happened **after** it
//! ([`delete_perform`]), file by file.
//!
//! # Two steps on one route
//!
//! `POST /api/library/delete { items, confirm: false }` plans and previews and
//! touches nothing; the same call with `confirm: true` performs. Both go through
//! [`plan_deletion`] over the same selection, so the dialog and the action can
//! only ever differ by what genuinely changed on disk between the two calls —
//! and that difference lands as a per-file outcome, never as a surprise.
//!
//! # The consequence matrix (spec §2), and where each row lives
//!
//! | case | behaviour | where |
//! | ---- | ---- | ---- |
//! | file is `queued` | taken out of the batcher pending set **first**, then unlinked — the next auto/scheduled flush never sees it | [`delete_perform`] step 1 |
//! | file is in an **in-flight** batch | deleted anyway; the transfer completes from the package's payload *copy*. The preview NAMES the batch so the operator knows | `in_flight_batches` |
//! | file was in a **confirmed** batch | deleted freely; that batch's re-send degrades to its eligible subset later | `confirmed_batches` |
//! | file **reappears** later | [`SeenStore::mark_deleted`] stamps the row, and [`SeenStore::should_enqueue`] treats anything found at a stamped path as new | [`delete_perform`] step 4 |
//! | delete races a flush | already safe — the batcher drops vanished files with a `warn!` | batcher |
//! | directory delete | recursive, files first by the rules above, then empty dirs deepest-first; one file's failure never strands its siblings | [`delete_perform`] |
//! | audit | one `sync_history` row per deleted file, written BEFORE the unlink | [`write_deletion_audit`] |
//!
//! # What is refused
//!
//! Exactly one thing: a path that resolves inside Perseus's own `data_dir` or
//! package staging area ([`DeleteOutcomeDto::Refused`], reason
//! [`INTERNAL_PATH_REASON`]). That is defence in depth on top of the containment
//! guard rather than a duplicate of it — the guard proves a path is inside a
//! *capture root*, which is not the same question as "is it ours", and an
//! operator who nests `data_dir` inside a capture root by misconfiguration must
//! not be able to delete the agent's own database from a file listing.
//!
//! # Symlinks: the link is the victim, never its target
//!
//! [`resolve_in_root`] canonicalizes, so it hands back a symlink's *target* —
//! perfect as a containment proof (a link out of the root is rejected there) and
//! wrong as a deletion target (unlinking the target would leave the operator's
//! selected entry behind as a dangling link and destroy a file they did not
//! pick). Every victim is therefore re-derived as *canonical parent + own name*
//! ([`victim_path`]): identical to the canonical path for an ordinary file, and
//! the link itself for a link. It is also the exact spelling the listing keys
//! on, so the pending / seen / batch joins line up.

use std::collections::{BTreeMap, BTreeSet, HashSet};
use std::path::{Path, PathBuf};

use anyhow::{bail, Context, Result};
use athenaeum_core::sync::{OutboundRow, OutboundState, SyncStore};

use crate::batch_store::BatchStore;
use crate::batcher::BatcherHandle;
use crate::config::Config;
use crate::run::write_deletion_audit;
use crate::seen::SeenStore;

use super::listing::newest_by_target;
use super::{resolve_in_root, split_rel, SendItem};

/// The one refusal reason this module produces (spec §2's `refused(internal-path)`).
pub const INTERNAL_PATH_REASON: &str = "perseus-internal path";

/// What happened to one selected path once `confirm: true` ran.
///
/// Internally tagged (`{"kind":"deleted"}`, `{"kind":"refused","reason":…}`) so
/// the page switches on one field for every arm — an externally tagged enum
/// would serialize the unit variant as a bare string and the others as nested
/// objects, which no consumer can branch on cleanly.
#[derive(serde::Serialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "camelCase")]
pub enum DeleteOutcomeDto {
    /// The file is gone from disk, its audit row is written, and its seen row is
    /// stamped.
    Deleted,
    /// Perseus declined to touch it (see [`INTERNAL_PATH_REASON`]) — the only
    /// arm that is a *decision* rather than an outcome.
    Refused { reason: String },
    /// It should have been deleted and was not: already gone (`"not found"`), an
    /// unwritable parent directory, an unpersistable audit row.
    Error { reason: String },
}

impl DeleteOutcomeDto {
    /// The human reason behind a non-`Deleted` outcome, for the preview's
    /// `blocked` field and the retention log's error lines.
    fn reason(&self) -> Option<&str> {
        match self {
            DeleteOutcomeDto::Deleted => None,
            DeleteOutcomeDto::Refused { reason } | DeleteOutcomeDto::Error { reason } => {
                Some(reason)
            }
        }
    }
}

/// One line of the confirm dialog: a file the selection resolved to, and every
/// consequence of removing it that Perseus can know.
#[derive(serde::Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DeletePreviewItem {
    /// Wire rel-path inside `root` — the same `(root, rel)` addressing every
    /// library route uses. A directory in the selection is expanded, so every
    /// preview row is a real file.
    pub rel: String,
    pub root: usize,
    /// Sitting in the batcher's pending set: deleting it also takes it out, so
    /// the next flush never sees it.
    pub queued: bool,
    /// Names of the batches carrying this file whose transfer is still moving.
    /// Non-empty is NOT a blocker — the upload reads the package's payload copy,
    /// so it completes regardless — it is what the dialog must say out loud.
    pub in_flight_batches: Vec<String>,
    /// How many batches carrying this file are already confirmed. Deleting it
    /// degrades those batches' future re-sends to their eligible subset.
    pub confirmed_batches: usize,
    /// Why this row will NOT be deleted — [`INTERNAL_PATH_REASON`], or
    /// `"not found"` for a path that vanished under a stale listing. `None` for
    /// everything that will actually be deleted. Blocked rows are still listed:
    /// an operator who picked 12 files must be told that 11 will go, not shown a
    /// silently shorter list.
    pub blocked: Option<String>,
}

/// Per-path result of a performed pass.
#[derive(serde::Serialize, Debug, Clone)]
#[serde(rename_all = "camelCase")]
pub struct DeleteOutcomeItem {
    pub rel: String,
    /// Carried alongside `rel` because a rel-path alone is ambiguous on a
    /// multi-root node — the pair is the address, exactly as in the request.
    pub root: usize,
    pub outcome: DeleteOutcomeDto,
}

/// Everything one performed pass produced.
///
/// `items` is the wire payload; the other two fields never leave the process —
/// they carry the absolute paths the retention log records, which the library
/// contract keeps off the wire.
#[derive(Debug, Default)]
pub struct DeleteReport {
    pub items: Vec<DeleteOutcomeItem>,
    /// Absolute paths actually unlinked.
    pub deleted_paths: Vec<String>,
    /// One line per refusal or failure, `"<path>: <reason>"`.
    pub notes: Vec<String>,
}

/// Borrowed handles the preview's consequence join reads. Snapshotted once by
/// the route, exactly like the listing's [`StatusSources`](super::StatusSources).
pub struct DeleteSources<'a> {
    /// The batcher's pending accumulator as `(capture_dir, file)` pairs.
    pub pending: &'a [(PathBuf, PathBuf)],
    pub batches: &'a BatchStore,
    /// Every outbound row in the read window; the newest attempt per
    /// `(package_ref, peer)` is picked here.
    pub outbound: &'a [OutboundRow],
}

/// Everything a performed pass writes through.
pub struct DeleteContext<'a> {
    /// Audit sink. `&dyn` for the same reason retention takes one: a test can
    /// inject a store whose `append_history` fails and prove the delete is
    /// refused rather than silently unaudited.
    pub store: &'a dyn SyncStore,
    pub seen: &'a SeenStore,
    /// The running batcher, or `None` while the engine is detached — a detached
    /// node has no pending set, which is the honest answer, not an error.
    pub batcher: Option<&'a BatcherHandle>,
    /// Who is deleting, stamped into the audit row's outcome
    /// (`deleted_<actor>`); the web browser passes `run::MANUAL_WEB_ACTOR`
    /// (`"manual-web"`).
    pub actor: &'a str,
}

/// One file the pass will try to unlink.
#[derive(Debug, Clone)]
struct PlanFile {
    root: usize,
    rel: String,
    /// The CANONICAL capture root, so the `(capture_dir, file)` pair matches the
    /// spelling the watcher fed the batcher.
    root_dir: PathBuf,
    /// The victim (canonical parent + own name — see the module docs).
    path: PathBuf,
}

/// A selected path that will not be acted on, and why.
#[derive(Debug, Clone)]
struct PlanBlocked {
    root: usize,
    rel: String,
    outcome: DeleteOutcomeDto,
}

/// A resolved deletion, ready to preview or perform. Building one writes
/// nothing and removes nothing.
#[derive(Debug, Default)]
pub struct DeletePlan {
    files: Vec<PlanFile>,
    /// Directories to try to remove after the files, **deepest first**. Never
    /// includes a capture root itself: a root is configuration, not content, and
    /// removing it would silently stop the watcher that owns it.
    dirs: Vec<PathBuf>,
    blocked: Vec<PlanBlocked>,
}

impl DeletePlan {
    /// How many files this pass would unlink (blocked entries excluded).
    pub fn file_count(&self) -> usize {
        self.files.len()
    }
}

/// The paths that are Perseus's own, whatever a capture root claims.
///
/// `packages_dir` lives under `data_dir` today; both are listed because that
/// containment is an implementation detail and this check must survive it
/// moving. Each is canonicalized when it exists so the comparison is
/// canonical-vs-canonical like the containment guard's; a `data_dir` that has
/// not been created yet falls back to its configured spelling, which still
/// matches a path built by joining onto a canonical root in the common case.
fn internal_roots(config: &Config) -> Vec<PathBuf> {
    [config.data_dir.clone(), config.packages_dir()]
        .into_iter()
        .map(|p| std::fs::canonicalize(&p).unwrap_or(p))
        .collect()
}

fn is_internal(internal: &[PathBuf], path: &Path) -> bool {
    internal.iter().any(|root| path.starts_with(root))
}

/// The path to actually unlink for `rel`: its canonical parent joined with its
/// own final segment.
///
/// For an ordinary file this is byte-identical to `canon` (the fully
/// canonicalized path). For a **symlink** it is the link, not the target — see
/// the module docs for why that distinction is the whole point. `rel == ""` (the
/// root itself) has no final segment and yields `canon` unchanged.
fn victim_path(canon_root: &Path, rel: &str, canon: &Path) -> Result<PathBuf> {
    let segs = split_rel(rel)?;
    let Some((last, parents)) = segs.split_last() else {
        return Ok(canon.to_path_buf());
    };
    let mut parent = canon_root.to_path_buf();
    for p in parents {
        parent.push(p);
    }
    let parent = std::fs::canonicalize(&parent)
        .with_context(|| format!("not found: {}", parent.display()))?;
    Ok(parent.join(last))
}

/// The wire rel-path of `path` under `canon_root`, forward-slash joined.
///
/// Only ever called with a path produced by walking `canon_root`, so the
/// `strip_prefix` cannot fail; the fallback degrades to the bare filename rather
/// than putting an absolute path on the wire.
fn wire_rel(canon_root: &Path, path: &Path) -> String {
    match path.strip_prefix(canon_root) {
        Ok(rel) => rel
            .components()
            .map(|c| c.as_os_str().to_string_lossy().into_owned())
            .collect::<Vec<_>>()
            .join("/"),
        Err(_) => path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default(),
    }
}

/// Resolve a browser selection into the files to unlink, the directories to
/// clear afterwards, and the entries that will not be touched.
///
/// **Nothing is written or removed here** — the preview and the perform pass
/// both run it over the same selection, so what the operator was shown and what
/// is acted on are produced by one set of rules.
///
/// Fatal (the whole request fails, carrying T1's stable prefixes so the route
/// maps a status): hostile syntax, a path escaping its root, an unknown root
/// index, an offline root, and a kind mismatch (`"not a file"` /
/// `"not a directory"` — the client's listing is stale enough that it no longer
/// knows what it picked).
///
/// Per item (the rest of the selection proceeds): a path that has already
/// vanished → `Error("not found")`, a Perseus-internal path →
/// `Refused(INTERNAL_PATH_REASON)`. **A vanished directory is absorbed here
/// where [`expand_selection`](super::expand_selection) makes it fatal** — for a
/// send, a missing tree means the operator would ship a different selection than
/// they picked; for a delete it means the outcome they asked for already holds.
///
/// Directory expansion takes **every** entry, not just capture-eligible ones:
/// deleting a night's folder has to remove its `.DS_Store` and logs too, or the
/// folder can never be emptied. (The send path's eligibility filter exists for
/// the opposite reason — not sweeping junk into a package.)
pub fn plan_deletion(config: &Config, items: &[SendItem]) -> Result<DeletePlan> {
    let roots = config.capture_dirs_resolved();
    let internal = internal_roots(config);
    // Keyed by victim path so an overlapping selection (a directory plus a file
    // inside it) collapses to one deletion, in a deterministic order.
    let mut files: BTreeMap<PathBuf, PlanFile> = BTreeMap::new();
    let mut dirs: BTreeSet<PathBuf> = BTreeSet::new();
    let mut blocked: Vec<PlanBlocked> = Vec::new();
    let mut blocked_keys: HashSet<(usize, String)> = HashSet::new();
    let mut block = |blocked: &mut Vec<PlanBlocked>, root: usize, rel: String, outcome| {
        if blocked_keys.insert((root, rel.clone())) {
            blocked.push(PlanBlocked { root, rel, outcome });
        }
    };

    for item in items {
        let Some(root) = roots.get(item.root) else {
            bail!("not found: unknown capture root {}", item.root);
        };
        let canon_root = std::fs::canonicalize(root)
            .with_context(|| format!("canonicalize root {}", root.display()))?;

        // Guard first: nothing below may see a path that has not been
        // canonicalized and prefix-checked against its root.
        let canon = match resolve_in_root(root, &item.rel) {
            Ok(canon) => canon,
            Err(error) if error.to_string().starts_with("not found") => {
                // The bound error goes in the record, never dropped: `"not found"`
                // is only the guard's wrapper and the cause underneath may be an
                // EACCES or an ELOOP rather than a real absence.
                tracing::warn!(
                    error = %format!("{error:#}"),
                    rel = %item.rel,
                    root_index = item.root,
                    "library delete: selected path is already gone"
                );
                block(
                    &mut blocked,
                    item.root,
                    item.rel.clone(),
                    DeleteOutcomeDto::Error {
                        reason: "not found".to_string(),
                    },
                );
                continue;
            }
            Err(error) => return Err(error),
        };

        let victim = match victim_path(&canon_root, &item.rel, &canon) {
            Ok(victim) => victim,
            Err(error) => {
                // The guard canonicalized this very path a moment ago, so the
                // only way here is a race: the parent went away in between.
                // That is the vanished class, this item's own outcome — not a
                // reason to fail the operator's whole selection.
                tracing::warn!(
                    error = %format!("{error:#}"),
                    rel = %item.rel,
                    root_index = item.root,
                    "library delete: the selected path's parent vanished mid-plan"
                );
                block(
                    &mut blocked,
                    item.root,
                    item.rel.clone(),
                    DeleteOutcomeDto::Error {
                        reason: "not found".to_string(),
                    },
                );
                continue;
            }
        };
        if is_internal(&internal, &victim) {
            tracing::warn!(
                path = %victim.display(),
                rel = %item.rel,
                "library delete refused: perseus-internal path"
            );
            block(
                &mut blocked,
                item.root,
                item.rel.clone(),
                DeleteOutcomeDto::Refused {
                    reason: INTERNAL_PATH_REASON.to_string(),
                },
            );
            continue;
        }

        // A symlink is deleted as a symlink whatever the client called it: the
        // link is what the operator saw in the listing, and its target belongs
        // to whoever owns it.
        let is_link = std::fs::symlink_metadata(&victim)
            .map(|m| m.file_type().is_symlink())
            .unwrap_or(false);
        if is_link {
            files.insert(
                victim.clone(),
                PlanFile {
                    root: item.root,
                    rel: item.rel.clone(),
                    root_dir: canon_root.clone(),
                    path: victim,
                },
            );
            continue;
        }

        if !item.dir {
            if !canon.is_file() {
                bail!("not a file: {:?}", item.rel);
            }
            files.insert(
                victim.clone(),
                PlanFile {
                    root: item.root,
                    rel: item.rel.clone(),
                    root_dir: canon_root.clone(),
                    path: victim,
                },
            );
            continue;
        }

        if !canon.is_dir() {
            bail!("not a directory: {:?}", item.rel);
        }
        // Symlinks are not followed (walkdir's default), so a linked subtree
        // contributes its link and never another tree's files.
        let mut walk = walkdir::WalkDir::new(&canon)
            .sort_by_file_name()
            .into_iter();
        while let Some(entry) = walk.next() {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    // One unreadable entry must not fail the whole selection —
                    // the same eligible-subset stance the rest of the pipeline
                    // takes. It simply is not deleted, and the directory it sits
                    // in will fail to empty, which the operator sees.
                    tracing::warn!(
                        %error,
                        path = %canon.display(),
                        "library delete: skipped an unreadable entry while expanding a directory"
                    );
                    continue;
                }
            };
            let path = entry.path().to_path_buf();
            let is_dir = entry.file_type().is_dir();
            if is_internal(&internal, &path) {
                tracing::warn!(
                    path = %path.display(),
                    "library delete refused: perseus-internal path inside a selected directory"
                );
                block(
                    &mut blocked,
                    item.root,
                    wire_rel(&canon_root, &path),
                    DeleteOutcomeDto::Refused {
                        reason: INTERNAL_PATH_REASON.to_string(),
                    },
                );
                if is_dir {
                    // Never descend into our own data directory to enumerate
                    // what we are not going to delete.
                    walk.skip_current_dir();
                }
                continue;
            }
            if is_dir {
                dirs.insert(path);
            } else {
                files.insert(
                    path.clone(),
                    PlanFile {
                        root: item.root,
                        rel: wire_rel(&canon_root, &path),
                        root_dir: canon_root.clone(),
                        path,
                    },
                );
            }
        }
        // The selected directory itself is removable; the capture ROOT never is.
        dirs.remove(&canon_root);
    }

    // Deepest first, so a parent is only ever attempted after its children.
    let mut dirs: Vec<PathBuf> = dirs.into_iter().collect();
    dirs.sort_by_key(|d| std::cmp::Reverse(d.components().count()));

    Ok(DeletePlan {
        files: files.into_values().collect(),
        dirs,
        blocked,
    })
}

/// The consequence line for every file the plan would delete (spec §2's
/// `confirm: false` half). Reads three stores and writes none.
///
/// Blocked entries are appended after the deletable ones, each carrying its
/// reason in `blocked` — the dialog shows the whole selection, not a silently
/// filtered one.
pub fn delete_preview(
    plan: &DeletePlan,
    src: &DeleteSources<'_>,
) -> Result<Vec<DeletePreviewItem>> {
    let pending: HashSet<&Path> = src.pending.iter().map(|(_, file)| file.as_path()).collect();
    let newest = newest_by_target(src.outbound);

    let mut out = Vec::with_capacity(plan.files.len() + plan.blocked.len());
    for file in &plan.files {
        let refs = src
            .batches
            .batches_for_source(&file.path.to_string_lossy())?;
        let mut in_flight_batches = Vec::new();
        let mut confirmed_batches = 0usize;
        for package_ref in &refs {
            // A batch whose outbound rows were history-deleted says nothing
            // about this file's fate — exactly as in the listing's status join.
            let Some(targets) = newest.get(package_ref.as_str()) else {
                continue;
            };
            // A batch is counted ONCE, by its strongest live fact: a fan-out
            // with one target still moving and another already confirmed is
            // in-flight, because that is the consequence worth spelling out.
            if targets.values().any(|row| !row.state.is_terminal()) {
                let label = targets
                    .values()
                    .find_map(|row| row.display_name.clone())
                    .unwrap_or_else(|| {
                        Path::new(package_ref)
                            .file_name()
                            .map(|n| n.to_string_lossy().into_owned())
                            .unwrap_or_else(|| package_ref.clone())
                    });
                in_flight_batches.push(label);
            } else if targets
                .values()
                .any(|row| row.state == OutboundState::Confirmed)
            {
                confirmed_batches += 1;
            }
        }
        in_flight_batches.sort();
        out.push(DeletePreviewItem {
            rel: file.rel.clone(),
            root: file.root,
            queued: pending.contains(file.path.as_path()),
            in_flight_batches,
            confirmed_batches,
            blocked: None,
        });
    }

    for entry in &plan.blocked {
        out.push(DeletePreviewItem {
            rel: entry.rel.clone(),
            root: entry.root,
            queued: false,
            in_flight_batches: Vec::new(),
            confirmed_batches: 0,
            blocked: entry.outcome.reason().map(str::to_string),
        });
    }
    Ok(out)
}

/// Perform the plan: files first, then the directories that were emptied.
///
/// Per file, in this order (spec §2):
///
/// 1. **pending-remove** — out of the batcher's accumulator before anything
///    else, so a flush racing this delete cannot pick up a file that is about to
///    stop existing;
/// 2. **audit-write** — the `sync_history` row lands BEFORE the unlink, so a
///    deletion is never invisible. A failure here aborts *this file* and leaves
///    it on disk (retention's audit-before-delete contract, same reasoning);
/// 3. **unlink**;
/// 4. **`mark_deleted`** — stamps the seen row so a later identical
///    `(size, mtime)` re-capture at that path is treated as new (spec §2's
///    reappear row). A failure here is logged, not fatal: the file is already
///    gone and audited.
///
/// One file's failure never stops the pass — the operator asked for a set, and a
/// permission hole on one frame must not strand the other ninety-nine.
///
/// A file removed from the pending set by a pass that then fails to delete it is
/// deliberately NOT put back: it is still on disk with no seen row, so the
/// watcher's next sweep re-enqueues it. Restoring it here would risk a duplicate
/// entry racing that sweep.
pub fn delete_perform(plan: &DeletePlan, ctx: &DeleteContext<'_>) -> DeleteReport {
    let mut report = DeleteReport::default();

    for file in &plan.files {
        let path = file.path.as_path();

        // 1. Out of the pending set first (spec §2's queued row).
        if let Some(batcher) = ctx.batcher {
            let pairs = [(file.root_dir.clone(), file.path.clone())];
            if batcher.remove_pending(&pairs) > 0 {
                tracing::info!(
                    path = %path.display(),
                    "library delete: removed from the pending set before deleting"
                );
            }
        }

        // `symlink_metadata`: a symlink's own size, and a dangling link still
        // stats (it is deletable, and its audit row should say so).
        let size = match std::fs::symlink_metadata(path) {
            Ok(meta) => meta.len(),
            Err(error) => {
                tracing::warn!(
                    path = %path.display(),
                    %error,
                    "library delete: file vanished before it could be deleted"
                );
                report.push_failure(file, "not found".to_string());
                continue;
            }
        };

        // 2. Audit BEFORE the destructive action; a failure refuses this file.
        if let Err(error) = write_deletion_audit(ctx.store, path, size, ctx.actor) {
            let chain = format!("{error:#}");
            tracing::error!(
                path = %path.display(),
                error = %chain,
                "library delete: audit row could not be persisted — file left alone"
            );
            report.push_failure(file, "audit could not be written".to_string());
            continue;
        }

        // 3. The destructive action.
        if let Err(error) = std::fs::remove_file(path) {
            tracing::error!(
                path = %path.display(),
                %error,
                "library delete: unlink failed"
            );
            report.push_failure(file, error.to_string());
            continue;
        }

        // 4. Best-effort bookkeeping: the file is gone and audited either way.
        if let Err(error) = ctx.seen.mark_deleted(path) {
            tracing::error!(
                path = %path.display(),
                error = %format!("{error:#}"),
                "library delete: failed to stamp the seen row deleted after a successful delete"
            );
        }

        tracing::info!(path = %path.display(), size, actor = ctx.actor, "library delete");
        report.deleted_paths.push(path.display().to_string());
        report.items.push(DeleteOutcomeItem {
            rel: file.rel.clone(),
            root: file.root,
            outcome: DeleteOutcomeDto::Deleted,
        });
    }

    // The plan's refusals and already-gone entries are outcomes too — reported
    // after the files that were acted on, matching [`delete_preview`]'s order so
    // the dialog and the result read the same way down the page.
    for entry in &plan.blocked {
        if let Some(reason) = entry.outcome.reason() {
            report.notes.push(format!("{}: {reason}", entry.rel));
        }
        report.items.push(DeleteOutcomeItem {
            rel: entry.rel.clone(),
            root: entry.root,
            outcome: entry.outcome.clone(),
        });
    }

    // Directories last, deepest first, and only if the pass actually emptied
    // them: a directory that still holds something the operator did not select
    // (or a file that failed above) stays, which is the honest outcome. Only a
    // failure is reported — a removed directory is implied by its files' rows.
    for dir in &plan.dirs {
        match std::fs::remove_dir(dir) {
            Ok(()) => {
                tracing::info!(path = %dir.display(), "library delete: removed empty directory")
            }
            Err(error) => {
                tracing::warn!(
                    path = %dir.display(),
                    %error,
                    "library delete: directory not removed"
                );
                report.notes.push(format!("{}: {error}", dir.display()));
            }
        }
    }

    report
}

impl DeleteReport {
    fn push_failure(&mut self, file: &PlanFile, reason: String) {
        self.notes
            .push(format!("{}: {reason}", file.path.display()));
        self.items.push(DeleteOutcomeItem {
            rel: file.rel.clone(),
            root: file.root,
            outcome: DeleteOutcomeDto::Error { reason },
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use athenaeum_core::sync::store::StandaloneSyncStore;

    /// A config over one capture root, with the data dir **outside** it (the
    /// normal deployment) unless `data_inside` asks for the misconfiguration the
    /// internal-path guard exists for.
    fn test_config(data_inside: bool) -> (tempfile::TempDir, Config, PathBuf) {
        let tmp = tempfile::tempdir().unwrap();
        let cap = tmp.path().join("cap");
        let data = if data_inside {
            cap.join(".perseus")
        } else {
            tmp.path().join("data")
        };
        std::fs::create_dir_all(&cap).unwrap();
        std::fs::create_dir_all(&data).unwrap();
        let toml = format!(
            "capture_dir=\"{}\"\ndata_dir=\"{}\"\npairing_ticket=\"t\"\nmode=\"manual\"\n\
             [retention]\npolicy=\"keep_everything\"\ndry_run=true\n",
            cap.display(),
            data.display()
        );
        let config = Config::from_toml_str(&toml).unwrap();
        (tmp, config, cap)
    }

    fn item(rel: &str, dir: bool) -> SendItem {
        SendItem {
            root: 0,
            rel: rel.to_string(),
            dir,
        }
    }

    fn write(root: &Path, rel: &str, bytes: &[u8]) -> PathBuf {
        let p = root.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(&p, bytes).unwrap();
        std::fs::canonicalize(&p).unwrap()
    }

    /// The two stores a performed pass writes through, on one temp `perseus.db`.
    struct Stores {
        store: StandaloneSyncStore,
        seen: SeenStore,
    }

    fn stores(config: &Config) -> Stores {
        Stores {
            store: StandaloneSyncStore::open(config.db_path()).unwrap(),
            seen: SeenStore::open(config.db_path()).unwrap(),
        }
    }

    impl Stores {
        fn perform(&self, plan: &DeletePlan) -> DeleteReport {
            delete_perform(
                plan,
                &DeleteContext {
                    store: &self.store,
                    seen: &self.seen,
                    // No batcher: a detached node deletes just as well, it
                    // simply has no pending set to clear.
                    batcher: None,
                    actor: "test",
                },
            )
        }
    }

    /// Deleting a symlink removes the **link**, never what it points at.
    ///
    /// The containment guard hands back the canonicalized *target*, which is
    /// exactly right as a proof of containment and exactly wrong as a deletion
    /// target: unlinking it would destroy a file the operator did not pick and
    /// leave the one they did pick behind as a dangling link.
    #[cfg(unix)]
    #[test]
    fn deleting_a_symlink_removes_the_link_not_its_target() {
        let (_tmp, config, cap) = test_config(false);
        let real = write(&cap, "real.fits", b"x");
        let link = cap.join("link.fits");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        let plan = plan_deletion(&config, &[item("link.fits", false)]).unwrap();
        assert_eq!(
            plan.files
                .iter()
                .map(|f| f.path.clone())
                .collect::<Vec<_>>(),
            // Canonical PARENT + the entry's own name: the link itself, in the
            // spelling every store keys on (`/private/var/…` on macOS, where the
            // temp root is itself a symlink).
            vec![std::fs::canonicalize(&cap).unwrap().join("link.fits")],
            "the victim is the link's own path, not its canonical target"
        );

        let report = stores(&config).perform(&plan);
        assert_eq!(report.items[0].outcome, DeleteOutcomeDto::Deleted);
        assert!(
            std::fs::symlink_metadata(&link).is_err(),
            "the link is gone"
        );
        assert!(real.exists(), "its target is untouched");
    }

    /// A symlink pointing OUT of the root never gets that far: the guard rejects
    /// it, and rejecting the whole request is right — an escape is not "one file
    /// went missing", it is a selection that means something else entirely.
    #[cfg(unix)]
    #[test]
    fn a_symlink_escaping_the_root_is_refused_by_the_guard() {
        let (tmp, config, cap) = test_config(false);
        let outside = tmp.path().join("secret");
        std::fs::create_dir_all(&outside).unwrap();
        let victim = outside.join("f.fits");
        std::fs::write(&victim, b"x").unwrap();
        std::os::unix::fs::symlink(&victim, cap.join("link.fits")).unwrap();

        let err = plan_deletion(&config, &[item("link.fits", false)])
            .expect_err("an escape is fatal")
            .to_string();
        assert!(err.starts_with("path escapes root"), "got {err:?}");
        assert!(victim.exists(), "nothing outside the root was touched");
    }

    /// A directory selection takes **every** entry — not just capture-eligible
    /// ones, or the folder could never be emptied — collapses overlapping items,
    /// and orders directories deepest-first so a parent is only attempted after
    /// its children. The capture ROOT is never in that list: it is
    /// configuration, and removing it would silently stop its watcher.
    #[test]
    fn a_directory_plan_takes_everything_and_orders_dirs_deepest_first() {
        let (_tmp, config, cap) = test_config(false);
        let a = write(&cap, "M31/a.fits", b"x");
        let notes = write(&cap, "M31/notes.txt", b"x");
        let deep = write(&cap, "M31/sub/deeper/c.fits", b"x");

        let plan = plan_deletion(
            &config,
            // Overlapping on purpose: the directory and a file inside it.
            &[item("M31", true), item("M31/a.fits", false)],
        )
        .unwrap();
        assert_eq!(
            plan.files
                .iter()
                .map(|f| f.path.clone())
                .collect::<Vec<_>>(),
            vec![a, notes, deep],
            "every entry, each exactly once"
        );
        assert_eq!(
            plan.files.iter().map(|f| f.rel.clone()).collect::<Vec<_>>(),
            vec!["M31/a.fits", "M31/notes.txt", "M31/sub/deeper/c.fits"],
            "wire rel-paths are forward-slash and root-relative"
        );

        let canon_cap = std::fs::canonicalize(&cap).unwrap();
        assert_eq!(
            plan.dirs,
            vec![
                canon_cap.join("M31/sub/deeper"),
                canon_cap.join("M31/sub"),
                canon_cap.join("M31"),
            ],
            "deepest first, and never the capture root itself"
        );

        stores(&config).perform(&plan);
        assert!(
            !cap.join("M31").exists(),
            "an emptied tree is removed whole"
        );
        assert!(cap.exists(), "the capture root survives");
    }

    /// Perseus's own data directory is refused even when it sits inside a
    /// capture root — the containment guard proves a path is in a root, which is
    /// a different question from "is it ours". The refusal is recorded ONCE and
    /// the walk never descends into it.
    #[test]
    fn an_internal_directory_is_refused_once_and_never_walked() {
        let (_tmp, config, cap) = test_config(true);
        let db = write(&cap, ".perseus/perseus.db", b"x");
        write(&cap, ".perseus/logs/agent.log", b"x");
        let own = write(&cap, "a.fits", b"x");

        let plan = plan_deletion(&config, &[item("", true)]).unwrap();
        assert_eq!(
            plan.files
                .iter()
                .map(|f| f.path.clone())
                .collect::<Vec<_>>(),
            vec![own.clone()],
            "only the operator's own file is deletable"
        );
        assert_eq!(
            plan.blocked.len(),
            1,
            "one refusal for the whole internal subtree, not one per file inside it"
        );
        assert_eq!(
            plan.blocked[0].outcome,
            DeleteOutcomeDto::Refused {
                reason: INTERNAL_PATH_REASON.to_string()
            }
        );

        stores(&config).perform(&plan);
        assert!(!own.exists());
        assert!(db.exists(), "the agent's own database survives");
    }

    /// The fatal classes, with the stable prefixes the route maps to statuses.
    /// A vanished path is deliberately absent — that is the one absorbed class
    /// (see [`plan_deletion`]'s docs), pinned by the route's own matrix test.
    #[test]
    fn hostile_paths_and_kind_mismatches_are_fatal() {
        let (_tmp, config, cap) = test_config(false);
        write(&cap, "M31/a.fits", b"x");
        for (it, prefix) in [
            (item("..", false), "invalid path segment"),
            (item("a\\b.fits", false), "invalid path segment"),
            (item("M31", false), "not a file"),
            (item("M31/a.fits", true), "not a directory"),
        ] {
            let err = plan_deletion(&config, &[it.clone()])
                .expect_err(&format!("{it:?} must fail"))
                .to_string();
            assert!(err.starts_with(prefix), "{it:?} → {err:?}");
        }
        let mut unknown_root = item("a.fits", false);
        unknown_root.root = 7;
        let err = plan_deletion(&config, &[unknown_root])
            .unwrap_err()
            .to_string();
        assert!(err.starts_with("not found"), "got {err:?}");
    }

    /// The audit row is written BEFORE the unlink, so a delete is never
    /// invisible — and the proof is the failure case: when the unlink cannot
    /// happen, the audit row is already there.
    #[cfg(unix)]
    #[test]
    fn the_audit_row_is_written_before_the_unlink() {
        use athenaeum_core::sync::{HistoryQuery, SyncStore};
        use std::os::unix::fs::PermissionsExt;

        let (_tmp, config, cap) = test_config(false);
        let locked = cap.join("locked");
        let victim = write(&cap, "locked/a.fits", b"0123456789");
        let plan = plan_deletion(&config, &[item("locked/a.fits", false)]).unwrap();

        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o500)).unwrap();
        let s = stores(&config);
        let report = s.perform(&plan);
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o700)).unwrap();

        assert!(
            matches!(report.items[0].outcome, DeleteOutcomeDto::Error { .. }),
            "the unlink really failed: {:?}",
            report.items[0].outcome
        );
        assert!(victim.exists());
        let rows = s
            .store
            .search_history(HistoryQuery {
                filename: None,
                object: None,
                direction: None,
                peer: None,
                project: None,
                package_id: None,
                limit: 100,
            })
            .unwrap();
        assert_eq!(rows.len(), 1, "the audit landed first");
        assert_eq!(rows[0].outcome, "deleted_test");
        assert_eq!(rows[0].bytes, 10);
    }

    /// The wire shapes T10 switches on. `kind` is one field for every arm.
    #[test]
    fn outcome_and_preview_serialize_in_the_documented_shape() {
        assert_eq!(
            serde_json::to_string(&DeleteOutcomeDto::Deleted).unwrap(),
            r#"{"kind":"deleted"}"#
        );
        assert_eq!(
            serde_json::to_string(&DeleteOutcomeDto::Refused {
                reason: INTERNAL_PATH_REASON.to_string()
            })
            .unwrap(),
            r#"{"kind":"refused","reason":"perseus-internal path"}"#
        );
        assert_eq!(
            serde_json::to_string(&DeleteOutcomeDto::Error {
                reason: "not found".to_string()
            })
            .unwrap(),
            r#"{"kind":"error","reason":"not found"}"#
        );
        let json = serde_json::to_string(&DeletePreviewItem {
            rel: "M31/a.fits".to_string(),
            root: 0,
            queued: true,
            in_flight_batches: vec!["pkg-1".to_string()],
            confirmed_batches: 2,
            blocked: None,
        })
        .unwrap();
        assert_eq!(
            json,
            r#"{"rel":"M31/a.fits","root":0,"queued":true,"inFlightBatches":["pkg-1"],"confirmedBatches":2,"blocked":null}"#
        );
    }
}
