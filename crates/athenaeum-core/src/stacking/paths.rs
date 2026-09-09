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

    /// `root/calibrated/<group_key>` — stage-1 calibrated frames for one
    /// integration group.
    pub fn calibrated_dir(&self, group_key: &str) -> PathBuf {
        self.root.join("calibrated").join(group_key)
    }

    /// `root/registered/<group_key>` — stage-5 registered frames for one
    /// integration group.
    pub fn registered_dir(&self, group_key: &str) -> PathBuf {
        self.root.join("registered").join(group_key)
    }

    /// `root/ln/<group_key>` — stage-6 local-normalization intermediates
    /// (M2; not written by anything in M1, but part of the layout and the
    /// usage/cleanup accounting from this task onward).
    pub fn ln_dir(&self, group_key: &str) -> PathBuf {
        self.root.join("ln").join(group_key)
    }

    /// `root/runs` — one JSON snapshot per run (see [`Self::run_json`]).
    pub fn runs_dir(&self) -> PathBuf {
        self.root.join("runs")
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

/// The two folders after validation: normalized, created, writable, and
/// checked against each other. `warnings` carries scan-root-overlap
/// sentences (see [`OverlapRule::Warn`]) — non-fatal for stacking folders,
/// unlike the transfer folders `validate_transfer_dir` was written for.
#[derive(Debug, Clone)]
pub struct ValidatedDirs {
    pub working: PathBuf,
    pub output: PathBuf,
    pub warnings: Vec<String>,
}

/// Validate (and create) the working and output folders for a stacking run.
/// Each goes through [`crate::api::sync::validate_transfer_dir`] with
/// [`OverlapRule::Warn`] — a stacking folder overlapping a monitored scan
/// root is worth flagging (the scanner would try to ingest intermediate/
/// output FITS as if they were new frames) but must not by itself block a
/// run the way it blocks a transfer folder. Two further rules are specific to
/// this pair: the two folders must differ, and the working folder must not
/// sit inside the output folder (scratch files would then land inside
/// whatever the user treats as "the finished stack", including anything they
/// export or archive from there). The reverse nesting — output inside
/// working — is allowed.
pub fn validate_dirs(
    conn: &Connection,
    policy: &PathPolicy,
    working: &str,
    output: &str,
) -> Result<ValidatedDirs, ApiError> {
    let (working_path, working_warning) = crate::api::sync::validate_transfer_dir(
        conn,
        policy,
        working,
        "Stacking working folder",
        OverlapRule::Warn,
    )?;
    let (output_path, output_warning) = crate::api::sync::validate_transfer_dir(
        conn,
        policy,
        output,
        "Stacking output folder",
        OverlapRule::Warn,
    )?;

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
    warnings.extend(working_warning);
    warnings.extend(output_warning);

    Ok(ValidatedDirs {
        working: working_path,
        output: output_path,
        warnings,
    })
}

/// Free bytes available on the volume holding `path` (`statvfs`'s
/// `f_bavail * f_frsize`, the same "available to an unprivileged process"
/// figure `sync::retention::disk_usage_pct` reads). `None` on any error or on
/// a non-unix platform — a probe failure must never look like "the disk is
/// full", so the caller treats `None` as "unknown", not zero.
#[cfg(unix)]
pub fn free_bytes(path: &Path) -> Option<u64> {
    use std::ffi::CString;
    use std::os::unix::ffi::OsStrExt;

    let cpath = CString::new(path.as_os_str().as_bytes()).ok()?;
    // SAFETY: `stat` is zero-initialised and only read after a successful
    // call; `cpath` is a valid NUL-terminated C string living for the call's
    // duration.
    let mut stat: libc::statvfs = unsafe { std::mem::zeroed() };
    let rc = unsafe { libc::statvfs(cpath.as_ptr(), &mut stat) };
    if rc != 0 {
        tracing::warn!(path = %path.display(), "statvfs failed");
        return None;
    }
    Some(stat.f_bavail as u64 * stat.f_frsize as u64)
}

#[cfg(not(unix))]
pub fn free_bytes(_path: &Path) -> Option<u64> {
    None
}

/// Inputs to [`estimate_bytes`]: the groups a run would integrate, and the
/// two output toggles that change how much gets written (whether registered
/// frames are kept on disk, whether rejection maps are written alongside the
/// master).
pub struct EstimateInputs<'a> {
    pub groups: &'a [IntegrationGroup],
    pub write_registered: bool,
    pub write_maps: bool,
}

/// Rough byte estimate for a run's working+output footprint: every group
/// contributes its calibrated frames (one float32 plane per frame, three
/// planes for OSC), the same again if registered frames are also kept, and
/// one master plus (when maps are written) two rejection maps — all at the
/// group's own `W x H`. This is a footprint estimate, not an exact
/// accounting: it ignores compression, FITS header overhead, and the ln/
/// intermediates M2 adds.
pub fn estimate_bytes(i: &EstimateInputs<'_>) -> u64 {
    let mut total = 0u64;
    for g in i.groups {
        let planes: u64 = if g.color_mode == ColorMode::Osc { 3 } else { 1 };
        let w = g.width.max(0) as u64;
        let h = g.height.max(0) as u64;
        let plane_bytes = w * h * 4;
        let per_frame_bytes = planes * plane_bytes;

        let calibrated_bytes = g.frames.len() as u64 * per_frame_bytes;
        total += calibrated_bytes;
        if i.write_registered {
            total += calibrated_bytes;
        }

        let master_multiplier: u64 = if i.write_maps { 1 + 2 } else { 1 };
        total += per_frame_bytes * master_multiplier;
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
    pub total_bytes: u64,
}

pub fn work_usage(layout: &WorkingLayout) -> WorkUsage {
    let calibrated_bytes = dir_bytes(&layout.root.join("calibrated"));
    let registered_bytes = dir_bytes(&layout.root.join("registered"));
    let ln_bytes = dir_bytes(&layout.root.join("ln"));
    let runs_bytes = dir_bytes(&layout.root.join("runs"));
    WorkUsage {
        calibrated_bytes,
        registered_bytes,
        ln_bytes,
        runs_bytes,
        total_bytes: calibrated_bytes + registered_bytes + ln_bytes + runs_bytes,
    }
}

/// Recursive byte total of every regular file under `path`. `0` when `path`
/// does not exist (or is unreadable) — a missing subtree is not an error
/// here, it is the common case (a run that never wrote registered frames, a
/// layout that was just cleaned up).
fn dir_bytes(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    let mut total = 0u64;
    for entry in entries.flatten() {
        let p = entry.path();
        if p.is_dir() {
            total += dir_bytes(&p);
        } else if let Ok(meta) = entry.metadata() {
            total += meta.len();
        }
    }
    total
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
/// (and, since it removes nothing less, [`CleanupWhat::All`]) — every kind
/// this task's working-folder subtrees (`calibrated/`, `registered/`, `ln/`)
/// can hold. `runs/` (removed only at `All`) carries no artifact rows of its
/// own — a run's `run-<id>.json` is a plain snapshot file, not tracked in
/// `stacking_artifacts`.
const INTERMEDIATE_ARTIFACT_KINDS: &[&str] =
    &["registered", "calibrated", "ln", "ln_reference", "metrics"];

/// Remove a working layout's subtrees per `what` and the matching
/// `stacking_artifacts` rows, returning the bytes freed (summed by walking
/// each subtree before it is removed). Only ever touches directories under
/// `layout.root` that this task's layout owns (`registered/`, `calibrated/`,
/// `ln/`, `runs/`) — never `layout.root` itself, and never the output
/// folder, which this function never sees a path for.
pub fn cleanup_work(
    conn: &Connection,
    frames_set_id: i64,
    layout: &WorkingLayout,
    what: CleanupWhat,
) -> anyhow::Result<u64> {
    let mut dirs = vec![layout.root.join("registered")];
    let kinds: &[&str] = match what {
        CleanupWhat::Registered => &["registered"],
        CleanupWhat::Intermediates => {
            dirs.push(layout.root.join("calibrated"));
            dirs.push(layout.root.join("ln"));
            INTERMEDIATE_ARTIFACT_KINDS
        }
        CleanupWhat::All => {
            dirs.push(layout.root.join("calibrated"));
            dirs.push(layout.root.join("ln"));
            dirs.push(layout.root.join("runs"));
            INTERMEDIATE_ARTIFACT_KINDS
        }
    };

    let mut freed = 0u64;
    for dir in &dirs {
        freed += dir_bytes(dir);
        if dir.exists() {
            std::fs::remove_dir_all(dir).map_err(|e| {
                tracing::warn!(path = %dir.display(), error = %e, "stacking cleanup: remove_dir_all failed");
                anyhow::anyhow!("failed to remove {}: {e}", dir.display())
            })?;
        }
    }

    crate::db::stacking::delete_artifacts(conn, frames_set_id, kinds)?;

    tracing::debug!(
        frame_set_id = frames_set_id,
        count = dirs.len(),
        bytes = freed,
        "stacking work cleaned up"
    );

    Ok(freed)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::schema::init_db;
    use crate::db::stacking::{delete_artifacts, NewArtifact};
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
        let err =
            validate_dirs(&c, &policy, same.to_str().unwrap(), same.to_str().unwrap()).unwrap_err();
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
        )
        .unwrap();
        assert!(validated.working.is_dir());
        assert!(validated.output.is_dir());
        assert_eq!(validated.warnings.len(), 1, "{:?}", validated.warnings);
        assert!(validated.warnings[0].contains("monitored folder"));
    }

    // ── free_bytes ───────────────────────────────────────────────────────

    #[cfg(unix)]
    #[test]
    fn free_bytes_is_some_on_unix() {
        let tmp = tempfile::tempdir().unwrap();
        assert!(free_bytes(tmp.path()).is_some());
    }

    // ── estimate_bytes ───────────────────────────────────────────────────

    fn frame(id: i64) -> GroupFrame {
        GroupFrame {
            frame_id: id,
            file_id: id,
            filename: format!("f{id}.fits"),
            path: format!("/x/f{id}.fits"),
            size: 0,
            modified_at: "2025-01-01T00:00:00Z".to_string(),
            exposure_s: Some(60.0),
            date_obs: None,
        }
    }

    fn group(color_mode: ColorMode, n_frames: i64, w: i64, h: i64) -> IntegrationGroup {
        IntegrationGroup {
            key: "g".to_string(),
            instrume: None,
            color_mode,
            filter: None,
            binning: 1,
            width: w,
            height: h,
            exposure_s: Some(60.0),
            frames: (0..n_frames).map(frame).collect(),
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
        });
        let on = estimate_bytes(&EstimateInputs {
            groups: &groups,
            write_registered: true,
            write_maps: false,
        });
        assert_eq!(
            on,
            off + 3 * 400,
            "registered adds one more calibrated-sized copy per frame"
        );
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

        // Intermediates: also removes calibrated/.
        let freed = cleanup_work(&c, set_id, &layout, CleanupWhat::Intermediates).unwrap();
        assert_eq!(freed, 300);
        assert!(!layout.registered_dir("g").exists());
        assert!(!layout.calibrated_dir("g").exists());
        assert!(
            layout.root.exists(),
            "the layout's own root is never removed"
        );

        // The output folder — a sibling directory this function never sees a
        // path for — is never touched by any level.
        let output_dir = tmp.path().join("output-should-survive");
        std::fs::create_dir_all(&output_dir).unwrap();
        let output_file = output_dir.join("master.fits");
        write_bytes(&output_file, 42);
        cleanup_work(&c, set_id, &layout, CleanupWhat::All).unwrap();
        assert!(output_file.exists(), "output folder untouched");
    }
}
