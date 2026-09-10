//! Working/output folders for a stacking run (spec §9.6): resolving the
//! effective folders (per-set override over the global setting), validating
//! and creating them, the on-disk layout under the working folder a run's
//! stages read/write, free-space and byte-usage probes, and cleanup of the
//! working folder's own subtrees once a run is done with them.
//!
//! Nothing here writes pixels or touches the catalog beyond the artifact
//! bookkeeping `cleanup_work` clears — this module only decides *where*
//! things go and reports *how much* is there.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use serde::{Deserialize, Serialize};

use crate::api::sync::OverlapRule;
use crate::api::{ApiError, PathPolicy};
use crate::settings::{keys, SettingsManager};
use crate::stacking::groups::{ColorMode, IntegrationGroup};

/// The on-disk layout a stacking run stages its working-folder artifacts
/// into: `<working_dir>/<set_slug>/{calibrated,registered,ln,runs}/…`. One
/// `WorkingLayout` per frame set (the slug — [`crate::stacking::groups::set_slug`]
/// — is computed by the caller from the frame set's name).
#[derive(Debug, Clone)]
pub struct WorkingLayout {
    pub root: PathBuf,
}

impl WorkingLayout {
    pub fn new(working_dir: &Path, set_slug: &str) -> Self {
        WorkingLayout {
            root: working_dir.join(set_slug),
        }
    }

    /// `root/calibrated` — the parent of every group's calibrated-frame
    /// subdirectory. One accessor per subtree (this and the three below),
    /// so `work_usage`/`cleanup_work` never repeat the literal.
    pub fn calibrated_root(&self) -> PathBuf {
        self.root.join("calibrated")
    }

    /// `root/registered` — the parent of every group's registered-frame
    /// subdirectory.
    pub fn registered_root(&self) -> PathBuf {
        self.root.join("registered")
    }

    /// `root/ln` — the parent of every group's local-normalization
    /// subdirectory (M2; not written by anything in M1, but part of the
    /// layout and the usage/cleanup accounting from this task onward).
    pub fn ln_root(&self) -> PathBuf {
        self.root.join("ln")
    }

    /// `root/runs` — one JSON snapshot per run (see [`Self::run_json`]).
    pub fn runs_root(&self) -> PathBuf {
        self.root.join("runs")
    }

    /// `root/calibrated/<group_key>` — stage-1 calibrated frames for one
    /// integration group.
    pub fn calibrated_dir(&self, group_key: &str) -> PathBuf {
        self.calibrated_root().join(group_key)
    }

    /// `root/registered/<group_key>` — stage-5 registered frames for one
    /// integration group.
    pub fn registered_dir(&self, group_key: &str) -> PathBuf {
        self.registered_root().join(group_key)
    }

    /// `root/ln/<group_key>` — stage-6 local-normalization intermediates.
    pub fn ln_dir(&self, group_key: &str) -> PathBuf {
        self.ln_root().join(group_key)
    }

    /// `root/ln/<group_key>/reference.fits` — the group's chosen local-
    /// normalization reference plane, in the reference geometry every
    /// frame's `LnGrid` (M2 Task 1) is defined over.
    pub fn ln_reference_path(&self, group_key: &str) -> PathBuf {
        self.ln_dir(group_key).join("reference.fits")
    }

    /// `root/ln/<group_key>/<stem>.athln` — one frame's local-normalization
    /// sidecar (M2 Task 1's `LnFrameGrids::write`/`read`).
    pub fn ln_sidecar_path(&self, group_key: &str, stem: &str) -> PathBuf {
        self.ln_dir(group_key).join(format!("{stem}.athln"))
    }

    /// `root/rej` — the parent of every run's rejection-bitmap tree (M3
    /// Task 2, spec §6.2, ruling R-M3-8). Unlike the other three `_root`
    /// accessors above, `.rej` files are per-RUN temporaries, not cached
    /// artifacts — the layout nests one level deeper (`run-<id>`) than
    /// `calibrated`/`registered`/`ln` do, because a re-run must never read
    /// a stale run's bitmaps back.
    pub fn rej_root(&self) -> PathBuf {
        self.root.join("rej")
    }

    /// `root/rej/run-<run_id>` — one run's whole rejection-bitmap tree,
    /// removed at that run's single exit path unless the user asked to
    /// keep everything.
    pub fn rej_run_dir(&self, run_id: i64) -> PathBuf {
        self.rej_root().join(format!("run-{run_id}"))
    }

    /// `root/rej/run-<run_id>/<group_key>` — one group's per-frame `.rej`
    /// files for that run.
    pub fn rej_dir(&self, run_id: i64, group_key: &str) -> PathBuf {
        self.rej_run_dir(run_id).join(group_key)
    }

    /// `root/rej/run-<run_id>/<group_key>/<stem>.rej`.
    pub fn rej_path(&self, run_id: i64, group_key: &str, stem: &str) -> PathBuf {
        self.rej_dir(run_id, group_key).join(format!("{stem}.rej"))
    }

    /// `root/runs` — same as [`Self::runs_root`]; kept as its own name since
    /// it predates the other three `_root` accessors and is the one other
    /// tasks (e.g. [`Self::run_json`]) already call.
    pub fn runs_dir(&self) -> PathBuf {
        self.runs_root()
    }

    /// `root/runs/run-<run_id>.json`.
    pub fn run_json(&self, run_id: i64) -> PathBuf {
        self.runs_dir().join(format!("run-{run_id}.json"))
    }
}

/// The two stacking folders after precedence (spec §9.6): a non-empty,
/// trimmed per-set [`crate::stacking::config::PathsConfig`] override wins;
/// otherwise the non-empty, trimmed global setting (`stacking.working_dir` /
/// `stacking.output_dir`); otherwise `None` — the caller (run start) is the
/// one that turns an unresolved folder into a blocking error, not this
/// module.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResolvedDirs {
    pub working: Option<String>,
    pub output: Option<String>,
}

/// Resolve the effective working/output folders. `conn` reads the global
/// settings row through `settings`'s own runtime > DB > default precedence
/// (`SettingsManager::get_with_precedence`); a settings-read failure (e.g. a
/// closed connection) is logged and treated as unset rather than propagated
/// — an unresolved folder is a normal, already-handled state (the caller
/// blocks Run on it), not a crash.
pub fn resolve_dirs(
    conn: &Connection,
    settings: &SettingsManager,
    paths: &crate::stacking::config::PathsConfig,
) -> ResolvedDirs {
    ResolvedDirs {
        working: pick_dir(
            conn,
            settings,
            paths.working_dir.as_deref(),
            keys::STACKING_WORKING_DIR,
        ),
        output: pick_dir(
            conn,
            settings,
            paths.output_dir.as_deref(),
            keys::STACKING_OUTPUT_DIR,
        ),
    }
}

fn pick_dir(
    conn: &Connection,
    settings: &SettingsManager,
    set_override: Option<&str>,
    key: &str,
) -> Option<String> {
    if let Some(raw) = set_override {
        let trimmed = raw.trim();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }
    match settings.get_with_precedence(conn, key, "") {
        Ok(value) => {
            let trimmed = value.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        }
        Err(error) => {
            tracing::warn!(key, %error, "stacking: settings read failed; treating folder as unset");
            None
        }
    }
}

/// The two folders after validation: normalized, writable, and checked
/// against each other. `warnings` carries either stacking-worded
/// scan-root-overlap sentences ([`ValidateMode::Save`], see
/// [`OverlapRule::Warn`] and [`overlap_sentence`]) or "does not exist yet"
/// sentences ([`ValidateMode::Plan`], see [`ValidateMode`]'s own doc) —
/// non-fatal for stacking folders either way.
#[derive(Debug, Clone)]
pub struct ValidatedDirs {
    pub working: PathBuf,
    pub output: PathBuf,
    pub warnings: Vec<String>,
}

/// [`validate_dirs`]'s side-effect switch (final fix wave item 5):
/// `get_stacking_plan` calls [`build_plan`](crate::stacking::plan::build_plan)
/// on every debounced Stacking-tab config edit — the plan must read the
/// folders, never write to them. `Save` keeps the original (pre-wave)
/// behaviour: it creates both folders and writes/removes the
/// `.athenaeum-write-test` probe (via [`crate::api::sync::validate_transfer_dir`]),
/// and is used by `set_stacking_paths` (an explicit user save) and
/// `start_stacking` (once, immediately before the run thread spawns —
/// creating the folders the run is about to write into). `Plan` performs NO
/// filesystem WRITE at all: it still validates the sandbox `PathPolicy` and
/// the two folder-pair rules (must differ; working may not sit inside
/// output), but a folder's existence/writability becomes a `read_dir`/
/// `metadata` probe rather than a creation — reported as a WARNING when the
/// folder does not exist yet (normal for a set that has never run: "it will
/// be created when the run starts") or the `folders` blocker when it exists
/// but this process cannot write to it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidateMode {
    Plan,
    Save,
}

/// Validate the working and output folders for a stacking run, per `mode`
/// (see [`ValidateMode`] for exactly what each mode does and does not
/// touch on disk).
pub fn validate_dirs(
    conn: &Connection,
    policy: &PathPolicy,
    working: &str,
    output: &str,
    mode: ValidateMode,
) -> Result<ValidatedDirs, ApiError> {
    match mode {
        ValidateMode::Save => validate_dirs_save(conn, policy, working, output),
        ValidateMode::Plan => validate_dirs_plan(policy, working, output),
    }
}

/// [`ValidateMode::Save`]'s body — unchanged from before this wave: each
/// folder goes through [`crate::api::sync::validate_transfer_dir`] with
/// [`OverlapRule::Warn`] — a stacking folder overlapping a monitored scan
/// root is worth flagging (the scanner would try to ingest intermediate/
/// output FITS as if they were new frames — except every file this pipeline
/// writes carries a scanner-skip card, so it never actually would) but must
/// not by itself block a run the way it blocks a transfer folder. Two
/// further rules are specific to this pair: the two folders must differ,
/// and the working folder must not sit inside the output folder (scratch
/// files would then land inside whatever the user treats as "the finished
/// stack", including anything they export or archive from there). The
/// reverse nesting — output inside working — is allowed.
///
/// A folder either call had to create (it did not already exist) is removed
/// again on a later rejection — the same leaf-only, best-effort contract
/// `validate_transfer_dir` keeps for its own later steps, extended here to
/// the two rules above it: a folder validated is a folder that must exist by
/// the time this function returns `Ok`, but a rejected save must not litter
/// the filesystem with a folder the operator will never get to use.
fn validate_dirs_save(
    conn: &Connection,
    policy: &PathPolicy,
    working: &str,
    output: &str,
) -> Result<ValidatedDirs, ApiError> {
    // Recorded before either call can create anything.
    let working_existed = Path::new(working.trim()).exists();
    let output_existed = Path::new(output.trim()).exists();

    let (working_path, working_overlap_root) = crate::api::sync::validate_transfer_dir(
        conn,
        policy,
        working,
        "Stacking working folder",
        OverlapRule::Warn,
    )?;
    let (output_path, output_overlap_root) = crate::api::sync::validate_transfer_dir(
        conn,
        policy,
        output,
        "Stacking output folder",
        OverlapRule::Warn,
    )?;

    if working_path == output_path {
        remove_if_created(&working_path, working_existed);
        remove_if_created(&output_path, output_existed);
        return Err(ApiError::Invalid(
            "working and output folders must differ".into(),
        ));
    }
    if working_path.starts_with(&output_path) {
        // Nested-child first: removing the parent while the child still
        // exists would fail (a non-empty directory), leaving both behind.
        remove_if_created(&working_path, working_existed);
        remove_if_created(&output_path, output_existed);
        return Err(ApiError::Invalid(
            "the working folder may not sit inside the output folder".into(),
        ));
    }

    let mut warnings = Vec::new();
    if let Some(root) = working_overlap_root {
        warnings.push(overlap_sentence("Stacking working folder", &root));
    }
    if let Some(root) = output_overlap_root {
        warnings.push(overlap_sentence("Stacking output folder", &root));
    }

    Ok(ValidatedDirs {
        working: working_path,
        output: output_path,
        warnings,
    })
}

/// [`ValidateMode::Plan`]'s body — NO filesystem write, ever (no
/// `create_dir_all`, no probe file): only a `PathPolicy` check, the two
/// folder-pair rules, and a read-only existence/writability probe per
/// folder. See [`ValidateMode`]'s own doc for the full contract.
fn validate_dirs_plan(
    policy: &PathPolicy,
    working: &str,
    output: &str,
) -> Result<ValidatedDirs, ApiError> {
    let working_path = plan_checked_path(policy, working, "Stacking working folder")?;
    let output_path = plan_checked_path(policy, output, "Stacking output folder")?;

    if working_path == output_path {
        return Err(ApiError::Invalid(
            "working and output folders must differ".into(),
        ));
    }
    if working_path.starts_with(&output_path) {
        return Err(ApiError::Invalid(
            "the working folder may not sit inside the output folder".into(),
        ));
    }

    let mut warnings = Vec::new();
    check_dir_writable_or_warn("Stacking working folder", &working_path, &mut warnings)?;
    check_dir_writable_or_warn("Stacking output folder", &output_path, &mut warnings)?;

    Ok(ValidatedDirs {
        working: working_path,
        output: output_path,
        warnings,
    })
}

/// Absolute-path + [`PathPolicy`] check ONLY — [`ValidateMode::Plan`]'s
/// lexical-only pre-check, mirroring `validate_transfer_dir`'s own
/// pre-`create_dir_all` check (a Plan-mode folder may not exist yet, so it
/// cannot be `canonicalize`d the way `Save` mode's authoritative,
/// post-creation check does).
fn plan_checked_path(policy: &PathPolicy, raw: &str, label: &str) -> Result<PathBuf, ApiError> {
    let trimmed = raw.trim();
    let candidate = Path::new(trimmed);
    if trimmed.is_empty() || !candidate.is_absolute() {
        return Err(ApiError::Invalid(format!(
            "{label}: enter an absolute path"
        )));
    }
    let normalized = crate::api::scan_roots::normalize_path(candidate);
    policy.check(&normalized)?;
    Ok(normalized)
}

/// [`ValidateMode::Plan`]'s per-folder probe: a folder that does not exist
/// yet is a WARNING, never a blocker — normal for a set that has never run.
/// An existing folder this process cannot write to (`fs::metadata` reports
/// it read-only, or `fs::read_dir` itself fails) is the `folders` BLOCKER,
/// surfaced as `Err` — `build_plan` turns any `validate_dirs` error into
/// that one blocker.
fn check_dir_writable_or_warn(
    label: &str,
    path: &Path,
    warnings: &mut Vec<String>,
) -> Result<(), ApiError> {
    if !path.exists() {
        warnings.push(format!(
            "{label} does not exist yet — it will be created when the run starts"
        ));
        return Ok(());
    }
    let readonly = std::fs::metadata(path)
        .map(|m| m.permissions().readonly())
        .unwrap_or(false);
    if readonly || std::fs::read_dir(path).is_err() {
        return Err(ApiError::Invalid(format!(
            "{label}: folder is not writable"
        )));
    }
    Ok(())
}

/// Word a stacking-specific scan-root-overlap warning from the overlapping
/// root's own path (`OverlapRule::Warn`'s payload) — deliberately not the
/// transfer-folder sentence `validate_transfer_dir` uses for
/// `OverlapRule::Reject`, since a stacking folder overlapping a monitored
/// root is allowed, not an error, and the scanner-skip-card reason is
/// specific to what this pipeline writes.
fn overlap_sentence(label: &str, overlapping_root_path: &str) -> String {
    format!(
        "{label} overlaps a monitored folder ({overlapping_root_path}) — every artifact and master carries a scanner-skip card, so this is allowed"
    )
}

/// Remove `path` if this call is the one that created it (`existed_before ==
/// false`) — best-effort and leaf-only (`remove_dir`, never
/// `remove_dir_all`): a folder that was already there, or that is not empty
/// (e.g. a rejected working-inside-output save leaves the child removed
/// before the parent, so the parent IS empty by the time this runs — see
/// [`validate_dirs`]), is left alone rather than risk removing something the
/// operator did not ask this call to touch.
fn remove_if_created(path: &Path, existed_before: bool) {
    if existed_before {
        return;
    }
    if let Err(e) = std::fs::remove_dir(path) {
        tracing::debug!(path = %path.display(), error = %e, "rejected stacking folder left in place");
    }
}

/// Free bytes available on the volume holding `path` (`statvfs`'s
/// `f_bavail * f_frsize`, the same "available to an unprivileged process"
/// figure `sync::retention::disk_usage_pct` reads). `None` on any error or on
/// a non-unix platform — a probe failure must never look like "the disk is
/// full", so the caller treats `None` as "unknown", not zero. Never silent:
/// both failure paths (an unrepresentable path, a failed `statvfs` call)
/// `warn!` before returning `None`.
#[cfg(unix)]
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let cpath = match CString::new(path.as_os_str().as_bytes()) {
        Ok(c) => c,
        Err(e) => {
            tracing::warn!(path = %path.display(), error = %e, "free space probe failed");
            return None;
        }
    };
    // SAFETY: `stat` is zero-initialised and only read after a successful
    // call; `cpath` is a valid NUL-terminated C string living for the call's
    // duration.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(cpath.as_ptr(), &mut stat) };
    if rc != 0 {
        let error = std::io::Error::last_os_error();
        tracing::warn!(path = %path.display(), %error, "free space probe failed");
        return None;
    }
    Some(stat.f_bavail as u64 * stat.f_frsize as u64)
}

#[cfg(not(unix))]
pub fn free_bytes(_path: &Path) -> Option<u64> {
    None
}

/// Inputs to [`estimate_bytes`]: the groups a run would integrate, and the
/// output toggles that change how much gets written (whether registered
/// frames are kept on disk, whether rejection maps are written alongside the
/// master, and — M3 Task 5 — whether drizzle runs at all).
pub struct EstimateInputs<'a> {
    pub groups: &'a [IntegrationGroup],
    pub write_registered: bool,
    pub write_maps: bool,
    /// `Some((scale, write_weight_map, use_rejection))` when
    /// `DrizzleConfig::enabled` — `None` when drizzle is off, adding
    /// nothing to the estimate.
    pub drizzle: Option<(u32, bool, bool)>,
}

/// Rough byte estimate for a run's working+output footprint: every group
/// contributes its calibrated frames (one float32 plane per frame, three
/// planes for OSC), the same again if registered frames are also kept, and
/// one master plus (when maps are written) two rejection maps. The master/
/// maps term uses the group's LARGEST member's native geometry (owner
/// decision 2026-09-10: a group's members can carry different native
/// geometry now that camera/geometry are not grouping keys — there is no
/// single group-wide `W x H` any more; every registered frame actually
/// lands on the ONE run-wide reference geometry, but that is not known this
/// early, before any run has even started). This is a footprint estimate,
/// not an exact accounting: it ignores compression, FITS header overhead,
/// and the ln/intermediates M2 adds.
///
/// M3 Task 5 (spec §7): when `i.drizzle` is `Some`, two more per-group terms
/// use the SAME largest-member geometry the master term above does —
/// rejection bitmaps (`use_rejection`, one `.rej` per included frame,
/// `ceil(W/64)` u64 words per row per plane, per `rej.rs`'s own layout:
/// `included * planes * ceil(W/64) * 8 * H`) and the drizzled output itself
/// (`planes * (W*scale) * (H*scale) * 4`, doubled when `write_weight_map` is
/// on — the weight map is the same geometry). "included" here is the
/// group's own frame count (`g.frames.len()`), the same approximation the
/// calibrated/master terms above already make — the real min-weight drop
/// only happens inside a run, not at plan time.
pub fn estimate_bytes(i: &EstimateInputs<'_>) -> u64 {
    let mut total = 0u64;
    for g in i.groups {
        let planes: u64 = if g.color_mode == ColorMode::Osc { 3 } else { 1 };

        let calibrated_bytes: u64 = g
            .frames
            .iter()
            .map(|f| planes * f.width.max(0) as u64 * f.height.max(0) as u64 * 4)
            .sum();
        total += calibrated_bytes;
        if i.write_registered {
            total += calibrated_bytes;
        }

        let (max_w, max_h) = g
            .frames
            .iter()
            .map(|f| (f.width.max(0) as u64, f.height.max(0) as u64))
            .max_by_key(|&(w, h)| w * h)
            .unwrap_or((0, 0));
        let master_per_frame_bytes = planes * max_w * max_h * 4;
        let master_multiplier: u64 = if i.write_maps { 1 + 2 } else { 1 };
        total += master_per_frame_bytes * master_multiplier;

        if let Some((scale, write_weight_map, use_rejection)) = i.drizzle {
            if use_rejection {
                let words_per_row = max_w.div_ceil(64);
                total += g.frames.len() as u64 * planes * words_per_row * 8 * max_h;
            }
            let s = scale as u64;
            let drizzle_output_bytes = planes * (max_w * s) * (max_h * s) * 4;
            total += drizzle_output_bytes;
            if write_weight_map {
                total += drizzle_output_bytes;
            }
        }
    }
    total
}

/// Byte usage of a working layout's four subtrees. A missing subtree (never
/// written, or already cleaned up) reads as `0`, not an error.
#[derive(Debug, Clone, Default, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub struct WorkUsage {
    pub calibrated_bytes: u64,
    pub registered_bytes: u64,
    pub ln_bytes: u64,
    pub runs_bytes: u64,
    /// M3 Task 2: bytes under `rej/` — per-run rejection-bitmap temporaries
    /// (spec §6.2, ruling R-M3-8), never a `stacking_artifacts` row.
    pub rej_bytes: u64,
    pub total_bytes: u64,
}

/// Walks each subtree via [`crate::api::sync::dir_size_bytes`] (`walkdir`,
/// which does not follow symlinks) rather than a second hand-rolled
/// recursive walker — `std::fs::read_dir` + `Path::is_dir` follows symlinks,
/// so a symlink-to-directory under e.g. `calibrated/` would recurse forever
/// on a cyclic link and double-count real files reachable both directly and
/// through the link.
pub fn work_usage(layout: &WorkingLayout) -> WorkUsage {
    let calibrated_bytes = crate::api::sync::dir_size_bytes(&layout.calibrated_root());
    let registered_bytes = crate::api::sync::dir_size_bytes(&layout.registered_root());
    let ln_bytes = crate::api::sync::dir_size_bytes(&layout.ln_root());
    let runs_bytes = crate::api::sync::dir_size_bytes(&layout.runs_root());
    let rej_bytes = crate::api::sync::dir_size_bytes(&layout.rej_root());
    WorkUsage {
        calibrated_bytes,
        registered_bytes,
        ln_bytes,
        runs_bytes,
        rej_bytes,
        total_bytes: calibrated_bytes + registered_bytes + ln_bytes + runs_bytes + rej_bytes,
    }
}

/// How much of a working layout to remove. Each level removes everything the
/// level below it removes, plus more: `Registered` only drops the
/// already-integrated registered frames; `Intermediates` also drops the
/// calibrated and (M2) locally-normalized frames — the whole reproducible
/// stage-1..6 output, keeping only the final run manifests; `All` also drops
/// those run manifests. The output folder (the master lights themselves) is
/// never touched by any level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
pub enum CleanupWhat {
    Registered,
    Intermediates,
    All,
}

/// `stacking_artifacts.kind` values covered by [`CleanupWhat::Intermediates`]
/// — every kind this task's `calibrated/`/`registered/`/`ln/` subtrees can
/// hold. NOT used by [`CleanupWhat::All`]: `kind` is free-form TEXT, not an
/// enum this crate can enumerate exhaustively, so `All` clears every row via
/// [`crate::db::stacking::delete_all_artifacts`] instead — a future kind
/// this list doesn't yet name must not survive "delete everything" and keep
/// pointing at files cleanup just removed.
const INTERMEDIATE_ARTIFACT_KINDS: &[&str] =
    &["registered", "calibrated", "ln", "ln_reference", "metrics"];

/// Remove a working layout's subtrees per `what` and the matching
/// `stacking_artifacts` rows, returning the bytes freed (summed by walking
/// each subtree, symlink-safe via [`crate::api::sync::dir_size_bytes`],
/// before it is removed). Only ever touches directories under `layout.root`
/// that this task's layout owns (`registered/`, `calibrated/`, `ln/`,
/// `runs/`, and — M3 Task 2 — `rej/`) — never `layout.root` itself, and
/// never the output folder, which this function never sees a path for.
/// `rej/` holds no `stacking_artifacts` rows (bitmaps are per-run
/// temporaries, not cached artifacts — see `stacking::rej`'s own doc), so it
/// is added to the removed DIRECTORIES on the same levels as `calibrated`/
/// `ln` without touching `INTERMEDIATE_ARTIFACT_KINDS` or the DB delete
/// calls below.
pub fn cleanup_work(
    conn: &Connection,
    frames_set_id: i64,
    layout: &WorkingLayout,
    what: CleanupWhat,
) -> anyhow::Result<u64> {
    let mut dirs = vec![layout.registered_root()];
    match what {
        CleanupWhat::Registered => {}
        CleanupWhat::Intermediates => {
            dirs.push(layout.calibrated_root());
            dirs.push(layout.ln_root());
            dirs.push(layout.rej_root());
        }
        CleanupWhat::All => {
            dirs.push(layout.calibrated_root());
            dirs.push(layout.ln_root());
            dirs.push(layout.rej_root());
            dirs.push(layout.runs_root());
        }
    }

    let mut freed = 0u64;
    for dir in &dirs {
        freed += crate::api::sync::dir_size_bytes(dir);
        if dir.exists() {
            std::fs::remove_dir_all(dir).map_err(|e| {
                tracing::warn!(path = %dir.display(), error = %e, "stacking cleanup: remove_dir_all failed");
                anyhow::anyhow!("failed to remove {}: {e}", dir.display())
            })?;
        }
    }

    match what {
        CleanupWhat::Registered => {
            crate::db::stacking::delete_artifacts(conn, frames_set_id, &["registered"])?;
        }
        CleanupWhat::Intermediates => {
            crate::db::stacking::delete_artifacts(
                conn,
                frames_set_id,
                INTERMEDIATE_ARTIFACT_KINDS,
            )?;
        }
        CleanupWhat::All => {
            crate::db::stacking::delete_all_artifacts(conn, frames_set_id)?;
        }
    }

    tracing::debug!(
        frame_set_id = frames_set_id,
        count = dirs.len(),
        freed_bytes = freed,
        "stacking work cleaned up"
    );

    Ok(freed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;
    use crate::db::stacking::{delete_artifacts, list_artifacts, NewArtifact};
    use crate::stacking::config::PathsConfig;
    use crate::stacking::groups::GroupFrame;

    fn conn() -> Connection {
        let c = Connection::open_in_memory().unwrap();
        init_db(&c).unwrap();
        c
    }

    // ── layout ───────────────────────────────────────────────────────────

    #[test]
    fn layout_paths() {
        let layout = WorkingLayout::new(Path::new("/work"), "LDN_1272");
        assert_eq!(layout.root, PathBuf::from("/work/LDN_1272"));
        assert_eq!(
            layout.calibrated_dir("cam__mono__Ha__bin1__100x100"),
            PathBuf::from("/work/LDN_1272/calibrated/cam__mono__Ha__bin1__100x100")
        );
        assert_eq!(
            layout.registered_dir("g"),
            PathBuf::from("/work/LDN_1272/registered/g")
        );
        assert_eq!(layout.ln_dir("g"), PathBuf::from("/work/LDN_1272/ln/g"));
        assert_eq!(layout.runs_dir(), PathBuf::from("/work/LDN_1272/runs"));
        assert_eq!(
            layout.run_json(42),
            PathBuf::from("/work/LDN_1272/runs/run-42.json")
        );
    }

    #[test]
    fn ln_reference_path_layout() {
        let layout = WorkingLayout::new(Path::new("/work"), "LDN_1272");
        assert_eq!(
            layout.ln_reference_path("cam__mono__Ha__bin1__100x100"),
            PathBuf::from("/work/LDN_1272/ln/cam__mono__Ha__bin1__100x100/reference.fits")
        );
    }

    #[test]
    fn ln_sidecar_path_layout() {
        let layout = WorkingLayout::new(Path::new("/work"), "LDN_1272");
        assert_eq!(
            layout.ln_sidecar_path("g", "f1"),
            PathBuf::from("/work/LDN_1272/ln/g/f1.athln")
        );
    }

    // ── resolve_dirs ─────────────────────────────────────────────────────

    #[test]
    fn resolve_dirs_precedence() {
        let c = conn();
        let settings = SettingsManager::new();

        // Nothing set anywhere: both None.
        let empty = PathsConfig::default();
        let resolved = resolve_dirs(&c, &settings, &empty);
        assert_eq!(resolved.working, None);
        assert_eq!(resolved.output, None);

        // Global setting only.
        crate::db::set_setting(&c, keys::STACKING_WORKING_DIR, "/global/work").unwrap();
        crate::db::set_setting(&c, keys::STACKING_OUTPUT_DIR, "/global/out").unwrap();
        let resolved = resolve_dirs(&c, &settings, &empty);
        assert_eq!(resolved.working.as_deref(), Some("/global/work"));
        assert_eq!(resolved.output.as_deref(), Some("/global/out"));

        // A per-set override wins over the global setting.
        let overridden = PathsConfig {
            working_dir: Some("/set/work".to_string()),
            output_dir: Some("  ".to_string()), // blank after trim: falls through
        };
        let resolved = resolve_dirs(&c, &settings, &overridden);
        assert_eq!(resolved.working.as_deref(), Some("/set/work"));
        assert_eq!(
            resolved.output.as_deref(),
            Some("/global/out"),
            "blank override falls through to the global setting"
        );

        // A runtime override (via SettingsManager) beats the DB value too.
        settings.set_runtime_override(
            keys::STACKING_OUTPUT_DIR.to_string(),
            "/runtime/out".to_string(),
        );
        let resolved = resolve_dirs(&c, &settings, &empty);
        assert_eq!(resolved.output.as_deref(), Some("/runtime/out"));
    }

    // ── validate_dirs ────────────────────────────────────────────────────

    #[test]
    fn validate_dirs_rules() {
        let c = conn();
        let tmp = tempfile::tempdir().unwrap();
        let policy = PathPolicy::AllowAll;

        // Equal folders: rejected.
        let same = tmp.path().join("same");
        let err = validate_dirs(
            &c,
            &policy,
            same.to_str().unwrap(),
            same.to_str().unwrap(),
            ValidateMode::Save,
        )
        .unwrap_err();
        assert!(
            matches!(err, ApiError::Invalid(ref m) if m.contains("must differ")),
            "{err:?}"
        );

        // Working inside output: rejected.
        let output = tmp.path().join("output");
        let working_inside = output.join("scratch");
        let err = validate_dirs(
            &c,
            &policy,
            working_inside.to_str().unwrap(),
            output.to_str().unwrap(),
            ValidateMode::Save,
        )
        .unwrap_err();
        assert!(
            matches!(err, ApiError::Invalid(ref m) if m.contains("may not sit inside")),
            "{err:?}"
        );

        // A scan-root overlap warns rather than blocks.
        let overlapping_root = tmp.path().join("lights");
        std::fs::create_dir_all(&overlapping_root).unwrap();
        crate::db::upsert_scan_root(&c, overlapping_root.to_str().unwrap(), "normal").unwrap();
        let working = overlapping_root.join("stacking-work");
        let output = tmp.path().join("stacking-out");
        let validated = validate_dirs(
            &c,
            &policy,
            working.to_str().unwrap(),
            output.to_str().unwrap(),
            ValidateMode::Save,
        )
        .unwrap();
        assert!(validated.working.is_dir());
        assert!(validated.output.is_dir());
        assert_eq!(validated.warnings.len(), 1, "{:?}", validated.warnings);
        assert!(validated.warnings[0].contains("monitored folder"));
        assert!(
            validated.warnings[0].contains(overlapping_root.to_str().unwrap()),
            "the warning names the overlapping SCAN ROOT's path, not the stacking folder's: {}",
            validated.warnings[0]
        );
        assert!(
            !validated.warnings[0].contains("ingest transfer copies"),
            "the reused transfer-folder sentence must not leak into a stacking warning: {}",
            validated.warnings[0]
        );
    }

    #[test]
    fn validate_dirs_removes_stray_folders_on_rejection() {
        let c = conn();
        let tmp = tempfile::tempdir().unwrap();
        let policy = PathPolicy::AllowAll;

        // Two fresh, non-existent, nested paths (working inside output):
        // both get created by validation, then the nesting check rejects —
        // neither may be left behind.
        let output = tmp.path().join("output-new");
        let working = output.join("scratch-new");
        assert!(!output.exists());
        assert!(!working.exists());

        let err = validate_dirs(
            &c,
            &policy,
            working.to_str().unwrap(),
            output.to_str().unwrap(),
            ValidateMode::Save,
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_)), "{err:?}");
        assert!(!working.exists(), "the folder this call created is removed");
        assert!(!output.exists(), "the folder this call created is removed");
    }

    /// Final fix wave item 5: `Plan` mode creates nothing on disk and
    /// reports each non-existent folder as a warning, never a blocker;
    /// `Save` mode (unchanged) creates both.
    #[test]
    fn validate_dirs_plan_creates_nothing_and_warns() {
        let c = conn();
        let tmp = tempfile::tempdir().unwrap();
        let policy = PathPolicy::AllowAll;

        let working = tmp.path().join("plan-working");
        let output = tmp.path().join("plan-output");
        assert!(!working.exists());
        assert!(!output.exists());

        let validated = validate_dirs(
            &c,
            &policy,
            working.to_str().unwrap(),
            output.to_str().unwrap(),
            ValidateMode::Plan,
        )
        .unwrap();
        assert!(!working.exists(), "Plan mode must create nothing");
        assert!(!output.exists(), "Plan mode must create nothing");
        assert_eq!(validated.warnings.len(), 2, "{:?}", validated.warnings);
        assert!(
            validated.warnings[0].contains("does not exist yet")
                && validated.warnings[0].contains("Stacking working folder"),
            "{:?}",
            validated.warnings
        );
        assert!(
            validated.warnings[1].contains("does not exist yet")
                && validated.warnings[1].contains("Stacking output folder"),
            "{:?}",
            validated.warnings
        );

        // `Save` mode, same two paths: both get created, no "does not exist
        // yet" warnings.
        let validated = validate_dirs(
            &c,
            &policy,
            working.to_str().unwrap(),
            output.to_str().unwrap(),
            ValidateMode::Save,
        )
        .unwrap();
        assert!(working.is_dir());
        assert!(output.is_dir());
        assert!(validated.warnings.is_empty(), "{:?}", validated.warnings);
    }

    /// Final fix wave item 5: `Plan` mode still enforces the sandbox policy
    /// and the two folder-pair rules, without ever touching disk.
    #[test]
    fn validate_dirs_plan_enforces_policy_and_pair_rules() {
        let c = conn();
        let tmp = tempfile::tempdir().unwrap();

        // Outside the sandbox: rejected, nothing created.
        let allowed = tmp.path().join("allowed");
        std::fs::create_dir_all(&allowed).unwrap();
        let policy = PathPolicy::AllowedRoots(vec![allowed.clone()]);
        let outside_working = tmp.path().join("outside-working");
        let outside_output = tmp.path().join("outside-output");
        let err = validate_dirs(
            &c,
            &policy,
            outside_working.to_str().unwrap(),
            outside_output.to_str().unwrap(),
            ValidateMode::Plan,
        )
        .unwrap_err();
        assert!(matches!(err, ApiError::Invalid(_) | ApiError::Forbidden(_)));
        assert!(!outside_working.exists());
        assert!(!outside_output.exists());

        // Equal folders: rejected.
        let policy = PathPolicy::AllowAll;
        let same = tmp.path().join("plan-same");
        let err = validate_dirs(
            &c,
            &policy,
            same.to_str().unwrap(),
            same.to_str().unwrap(),
            ValidateMode::Plan,
        )
        .unwrap_err();
        assert!(
            matches!(err, ApiError::Invalid(ref m) if m.contains("must differ")),
            "{err:?}"
        );

        // Working inside output: rejected.
        let output = tmp.path().join("plan-output-2");
        let working_inside = output.join("scratch");
        let err = validate_dirs(
            &c,
            &policy,
            working_inside.to_str().unwrap(),
            output.to_str().unwrap(),
            ValidateMode::Plan,
        )
        .unwrap_err();
        assert!(
            matches!(err, ApiError::Invalid(ref m) if m.contains("may not sit inside")),
            "{err:?}"
        );
    }

    /// Final fix wave item 5: `Plan` mode reports an EXISTING, unwritable
    /// folder as the `folders` blocker (an `Err`, same as every other
    /// `validate_dirs` failure) rather than a warning.
    #[cfg(unix)]
    #[test]
    fn validate_dirs_plan_blocks_on_an_unwritable_existing_folder() {
        use std::os::unix::fs::PermissionsExt;

        let c = conn();
        let tmp = tempfile::tempdir().unwrap();
        let policy = PathPolicy::AllowAll;

        let working = tmp.path().join("readonly-working");
        std::fs::create_dir_all(&working).unwrap();
        std::fs::set_permissions(&working, std::fs::Permissions::from_mode(0o500)).unwrap();
        let output = tmp.path().join("plan-output-3");

        let result = validate_dirs(
            &c,
            &policy,
            working.to_str().unwrap(),
            output.to_str().unwrap(),
            ValidateMode::Plan,
        );

        // Always restore permissions before asserting/unwinding, so the
        // tempdir can still be cleaned up even if an assertion below panics.
        std::fs::set_permissions(&working, std::fs::Permissions::from_mode(0o700)).unwrap();

        let err = result.unwrap_err();
        assert!(
            matches!(err, ApiError::Invalid(ref m) if m.contains("not writable")),
            "{err:?}"
        );
    }

    // ── free_bytes ───────────────────────────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn free_bytes_is_some_on_unix() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(free_bytes(tmp.path()).is_some());
    }

    // ── estimate_bytes ───────────────────────────────────────────────────

    fn frame(id: i64, w: i64, h: i64) -> GroupFrame {
        GroupFrame {
            frame_id: id,
            file_id: id,
            filename: format!("f{id}.fits"),
            path: format!("/x/f{id}.fits"),
            size: 0,
            modified_at: "2025-01-01T00:00:00Z".to_string(),
            exposure_s: Some(60.0),
            date_obs: None,
            width: w,
            height: h,
        }
    }

    fn group(color_mode: ColorMode, n_frames: i64, w: i64, h: i64) -> IntegrationGroup {
        IntegrationGroup {
            key: "g".to_string(),
            instrume: None,
            color_mode,
            filter: None,
            binning: 1,
            cameras: vec![],
            exposure_s: Some(60.0),
            frames: (0..n_frames).map(|id| frame(id, w, h)).collect(),
            total_exposure_s: n_frames as f64 * 60.0,
        }
    }

    #[test]
    fn estimate_counts_planes_and_maps() {
        let groups = vec![
            group(ColorMode::Mono, 2, 10, 10),
            group(ColorMode::Osc, 1, 10, 10),
        ];
        let inputs = EstimateInputs {
            groups: &groups,
            write_registered: false,
            write_maps: true,
            drizzle: None,
        };
        let expected = 2 * 400 + 1 * 1200 + (400 * 3) + (1200 * 3);
        assert_eq!(estimate_bytes(&inputs), expected);
    }

    #[test]
    fn estimate_adds_registered_copy_when_enabled() {
        let groups = vec![group(ColorMode::Mono, 3, 10, 10)];
        let off = estimate_bytes(&EstimateInputs {
            groups: &groups,
            write_registered: false,
            write_maps: false,
            drizzle: None,
        });
        let on = estimate_bytes(&EstimateInputs {
            groups: &groups,
            write_registered: true,
            write_maps: false,
            drizzle: None,
        });
        assert_eq!(
            on,
            off + 3 * 400,
            "registered adds one more calibrated-sized copy per frame"
        );
    }

    /// M3 Task 5, brief test (g): the estimate grows by the bitmap +
    /// drizzled-output terms once `drizzle` is `Some`. A 10x10 mono group of
    /// 3 frames at `scale = 2`, `write_weight_map = true`,
    /// `use_rejection = true`: bitmaps = `3 * 1 * ceil(10/64) * 8 * 10` =
    /// `3 * 1 * 1 * 8 * 10` = `240`; drizzled output = `1 * 20 * 20 * 4` =
    /// `1600`; weight map doubles it to `3200`. Total added: `240 + 3200`.
    #[test]
    fn estimate_grows_by_the_drizzle_bitmap_and_output_terms() {
        let groups = vec![group(ColorMode::Mono, 3, 10, 10)];
        let without_drizzle = estimate_bytes(&EstimateInputs {
            groups: &groups,
            write_registered: false,
            write_maps: false,
            drizzle: None,
        });
        let with_drizzle = estimate_bytes(&EstimateInputs {
            groups: &groups,
            write_registered: false,
            write_maps: false,
            drizzle: Some((2, true, true)),
        });
        assert_eq!(
            with_drizzle,
            without_drizzle + 240 + 3200,
            "without = {without_drizzle}, with = {with_drizzle}"
        );

        // No weight map: the output term is added only once.
        let with_drizzle_no_weight_map = estimate_bytes(&EstimateInputs {
            groups: &groups,
            write_registered: false,
            write_maps: false,
            drizzle: Some((2, false, true)),
        });
        assert_eq!(with_drizzle_no_weight_map, without_drizzle + 240 + 1600);

        // No rejection: the bitmap term drops out entirely.
        let with_drizzle_no_rejection = estimate_bytes(&EstimateInputs {
            groups: &groups,
            write_registered: false,
            write_maps: false,
            drizzle: Some((2, true, false)),
        });
        assert_eq!(with_drizzle_no_rejection, without_drizzle + 3200);
    }

    // ── usage + cleanup ──────────────────────────────────────────────────

    fn write_bytes(path: &Path, n: usize) {
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(path, vec![0u8; n]).unwrap();
    }

    fn seed_artifact(c: &Connection, frames_set_id: i64, kind: &str, path: &str, size: i64) {
        crate::db::stacking::upsert_artifact(
            c,
            &NewArtifact {
                frames_set_id,
                frame_id: None,
                group_key: "g",
                kind,
                path: Some(path),
                config_hash: "h",
                size: Some(size),
                modified_at: None,
                payload_json: None,
            },
        )
        .unwrap();
    }

    #[test]
    fn usage_and_cleanup() {
        let c = conn();
        c.execute("INSERT INTO frames_set (id, name) VALUES (1, 'S')", [])
            .unwrap();
        let set_id = 1;

        let tmp = tempfile::tempdir().unwrap();
        let layout = WorkingLayout::new(tmp.path(), "set");

        let calibrated_file = layout.calibrated_dir("g").join("f1.fits");
        write_bytes(&calibrated_file, 100);
        let registered_file = layout.registered_dir("g").join("f1.fits");
        write_bytes(&registered_file, 200);

        seed_artifact(
            &c,
            set_id,
            "calibrated",
            calibrated_file.to_str().unwrap(),
            100,
        );
        seed_artifact(
            &c,
            set_id,
            "registered",
            registered_file.to_str().unwrap(),
            200,
        );

        let usage = work_usage(&layout);
        assert_eq!(usage.calibrated_bytes, 100);
        assert_eq!(usage.registered_bytes, 200);
        assert_eq!(usage.ln_bytes, 0, "never written = 0, not an error");
        assert_eq!(usage.runs_bytes, 0);
        assert_eq!(usage.total_bytes, 300);

        // Registered: removes only registered/ and its row.
        let freed = cleanup_work(&c, set_id, &layout, CleanupWhat::Registered).unwrap();
        assert_eq!(freed, 200);
        assert!(!layout.registered_dir("g").exists());
        assert!(calibrated_file.exists(), "calibrated/ untouched");
        assert_eq!(
            delete_artifacts(&c, set_id, &["registered"]).unwrap(),
            0,
            "the registered row is already gone"
        );
        assert_eq!(
            delete_artifacts(&c, set_id, &["calibrated"]).unwrap(),
            1,
            "the calibrated row is still there"
        );
        // Re-seed the calibrated row `delete_artifacts` just consumed, and put
        // the registered file back for the next level.
        seed_artifact(
            &c,
            set_id,
            "calibrated",
            calibrated_file.to_str().unwrap(),
            100,
        );
        write_bytes(&registered_file, 200);
        seed_artifact(
            &c,
            set_id,
            "registered",
            registered_file.to_str().unwrap(),
            200,
        );

        // A future artifact kind this crate doesn't yet name — `kind` is
        // free-form TEXT, so nothing stops one existing before the code that
        // understands it does. `Intermediates` (a fixed kind list) must
        // leave it alone; only `All` may sweep it.
        seed_artifact(&c, set_id, "future", "/nowhere", 0);

        // Intermediates: also removes calibrated/, but not the unknown kind.
        let freed = cleanup_work(&c, set_id, &layout, CleanupWhat::Intermediates).unwrap();
        assert_eq!(freed, 300);
        assert!(!layout.registered_dir("g").exists());
        assert!(!layout.calibrated_dir("g").exists());
        assert!(
            layout.root.exists(),
            "the layout's own root is never removed"
        );
        let remaining = list_artifacts(&c, set_id, None).unwrap();
        assert_eq!(
            remaining.len(),
            1,
            "Intermediates does not touch a kind it doesn't name: {remaining:?}"
        );
        assert_eq!(remaining[0].kind, "future");

        // The output folder — a sibling directory this function never sees a
        // path for — is never touched by any level.
        let output_dir = tmp.path().join("output-should-survive");
        std::fs::create_dir_all(&output_dir).unwrap();
        let output_file = output_dir.join("master.fits");
        write_bytes(&output_file, 42);
        cleanup_work(&c, set_id, &layout, CleanupWhat::All).unwrap();
        assert!(output_file.exists(), "output folder untouched");
        assert!(
            list_artifacts(&c, set_id, None).unwrap().is_empty(),
            "All drops every artifact row, including a kind it doesn't name"
        );
    }

    #[cfg(unix)]
    #[test]
    fn work_usage_and_cleanup_do_not_follow_symlinks() {
        let c = conn();
        c.execute("INSERT INTO frames_set (id, name) VALUES (2, 'Sym')", [])
            .unwrap();
        let set_id = 2;

        let tmp = tempfile::tempdir().unwrap();
        let layout = WorkingLayout::new(tmp.path(), "set");

        let group_dir = layout.calibrated_dir("g");
        let real_dir = group_dir.join("real");
        std::fs::create_dir_all(&real_dir).unwrap();
        write_bytes(&real_dir.join("f.fits"), 1024);
        // A symlink back to the group directory itself — a cycle that would
        // recurse forever (and inflate the byte total) if the walker
        // followed it. `remove_dir_all` does not follow it either, so the
        // symlink itself is simply removed along with everything else.
        std::os::unix::fs::symlink(&group_dir, group_dir.join("loop")).unwrap();

        let usage = work_usage(&layout);
        assert_eq!(
            usage.calibrated_bytes, 1024,
            "the symlink is neither followed nor double-counted"
        );

        let freed = cleanup_work(&c, set_id, &layout, CleanupWhat::Intermediates).unwrap();
        assert_eq!(freed, 1024);
        assert!(!layout.calibrated_dir("g").exists());
    }

    /// M3 Task 2, brief test (j): `rej/` bytes are counted by `work_usage`
    /// and removed by `Intermediates`/`All`, but survive `Registered` — the
    /// same level `calibrated/`/`ln/` sit at, not the narrower level
    /// `registered/` alone sits at.
    #[test]
    fn rej_bytes_are_counted_and_only_intermediates_or_all_remove_them() {
        let c = conn();
        c.execute("INSERT INTO frames_set (id, name) VALUES (3, 'Rej')", [])
            .unwrap();
        let set_id = 3;

        let tmp = tempfile::tempdir().unwrap();
        let layout = WorkingLayout::new(tmp.path(), "set");

        assert_eq!(
            layout.rej_path(7, "g", "f1"),
            layout.root.join("rej").join("run-7").join("g").join("f1.rej")
        );

        let rej_file = layout.rej_dir(7, "g").join("f1.rej");
        write_bytes(&rej_file, 64);

        let usage = work_usage(&layout);
        assert_eq!(usage.rej_bytes, 64);
        assert_eq!(usage.total_bytes, 64, "the only bytes on disk are under rej/");

        // Registered: rej/ survives — it sits at the same level as
        // calibrated/ and ln/, not registered/'s narrower level.
        let freed = cleanup_work(&c, set_id, &layout, CleanupWhat::Registered).unwrap();
        assert_eq!(freed, 0);
        assert!(rej_file.exists(), "Registered must not remove rej/");

        // Intermediates: rej/ is removed alongside calibrated/ and ln/.
        let freed = cleanup_work(&c, set_id, &layout, CleanupWhat::Intermediates).unwrap();
        assert_eq!(freed, 64);
        assert!(!layout.rej_root().exists());
        assert_eq!(work_usage(&layout).rej_bytes, 0);

        // Re-seed for the All level too.
        write_bytes(&rej_file, 128);
        let freed = cleanup_work(&c, set_id, &layout, CleanupWhat::All).unwrap();
        assert_eq!(freed, 128);
        assert!(!layout.rej_root().exists());
    }
}
