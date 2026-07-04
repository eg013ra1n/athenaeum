//! Archive operation data types.

use serde::{Deserialize, Serialize};

/// What to do with a calibration type's files during archiving.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum ArchiveDisposition {
    Move,
    Copy,
    Skip,
}

impl ArchiveDisposition {
    pub fn as_str(&self) -> &'static str {
        match self {
            ArchiveDisposition::Move => "move",
            ArchiveDisposition::Copy => "copy",
            ArchiveDisposition::Skip => "skip",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "move" => Some(Self::Move),
            "copy" => Some(Self::Copy),
            "skip" => Some(Self::Skip),
            _ => None,
        }
    }
}

/// Compression mode for archive zips.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum ArchiveCompression {
    Store,
    Deflate,
}

impl ArchiveCompression {
    pub fn as_str(&self) -> &'static str {
        match self {
            ArchiveCompression::Store => "store",
            ArchiveCompression::Deflate => "deflate",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "store" => Some(Self::Store),
            "deflate" => Some(Self::Deflate),
            _ => None,
        }
    }
}

/// Stages of a forward archive operation. Matches the `archive_operation_steps.stage` column.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveStage {
    Copy,
    VerifyCopy,
    ZipAdd,
    VerifyZip,
    DeleteSource,
    Finalize,
    // Rollback-only stages
    DeleteStaging,
    RestoreSource,
    // Restore-only stage: hash-verifying a file that's already on disk at
    // source_path before trusting it as "restored" (the reconcile
    // skip-if-exists check in archive::restore).
    VerifyRestore,
}

impl ArchiveStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            ArchiveStage::Copy => "copy",
            ArchiveStage::VerifyCopy => "verify_copy",
            ArchiveStage::ZipAdd => "zip_add",
            ArchiveStage::VerifyZip => "verify_zip",
            ArchiveStage::DeleteSource => "delete_source",
            ArchiveStage::Finalize => "finalize",
            ArchiveStage::DeleteStaging => "delete_staging",
            ArchiveStage::RestoreSource => "restore_source",
            ArchiveStage::VerifyRestore => "verify_restore",
        }
    }
}

/// Status values for a step row.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    InProgress,
    Done,
    Failed,
    RolledBack,
}

impl StepStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            StepStatus::Pending => "pending",
            StepStatus::InProgress => "in_progress",
            StepStatus::Done => "done",
            StepStatus::Failed => "failed",
            StepStatus::RolledBack => "rolled_back",
        }
    }
}

/// State machine for `archive_operations.status`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ArchiveStatus {
    Planning,
    Copying,
    Verifying,
    Zipping,
    ZipVerifying,
    DeletingSources,
    Finalizing,
    Completed,
    Cancelled,
    RollingBack,
    RolledBack,
    Failed,
}

impl ArchiveStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            ArchiveStatus::Planning => "planning",
            ArchiveStatus::Copying => "copying",
            ArchiveStatus::Verifying => "verifying",
            ArchiveStatus::Zipping => "zipping",
            ArchiveStatus::ZipVerifying => "zip_verifying",
            ArchiveStatus::DeletingSources => "deleting_sources",
            ArchiveStatus::Finalizing => "finalizing",
            ArchiveStatus::Completed => "completed",
            ArchiveStatus::Cancelled => "cancelled",
            ArchiveStatus::RollingBack => "rolling_back",
            ArchiveStatus::RolledBack => "rolled_back",
            ArchiveStatus::Failed => "failed",
        }
    }

    /// Is this a state where work could still be in progress (i.e. resumable)?
    pub fn is_unfinished(&self) -> bool {
        !matches!(
            self,
            ArchiveStatus::Completed
                | ArchiveStatus::Cancelled
                | ArchiveStatus::RolledBack
                | ArchiveStatus::Failed
        )
    }
}

/// The frame role determines which zip a file goes into.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
pub enum FrameRole {
    Light,
    Flat,
    Dark,
    Bias,
    Darkflat,
}

impl FrameRole {
    pub fn as_str(&self) -> &'static str {
        match self {
            FrameRole::Light => "light",
            FrameRole::Flat => "flat",
            FrameRole::Dark => "dark",
            FrameRole::Bias => "bias",
            FrameRole::Darkflat => "darkflat",
        }
    }

    /// Folder name within the zip filename (e.g. "Lights", "Flats").
    pub fn zip_suffix(&self) -> &'static str {
        match self {
            FrameRole::Light => "Lights",
            FrameRole::Flat => "Flats",
            FrameRole::Dark => "Darks",
            FrameRole::Bias => "Bias",
            FrameRole::Darkflat => "DarkFlats",
        }
    }

    /// Priority for dedup (lower = wins): light > flat > darkflat > dark > bias.
    pub fn priority(&self) -> u8 {
        match self {
            FrameRole::Light => 0,
            FrameRole::Flat => 1,
            FrameRole::Darkflat => 2,
            FrameRole::Dark => 3,
            FrameRole::Bias => 4,
        }
    }
}

/// Disposition selections for the four calibration types.
/// `None` means the type is not present in the chain (so no question was asked).
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct Dispositions {
    pub flats: Option<ArchiveDisposition>,
    pub darks: Option<ArchiveDisposition>,
    pub bias: Option<ArchiveDisposition>,
    pub darkflats: Option<ArchiveDisposition>,
}

/// One row of `archive_operations`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveOperation {
    pub id: i64,
    pub frames_set_id: i64,
    pub archive_root_path: String,
    pub flats_disposition: Option<String>,
    pub darks_disposition: Option<String>,
    pub bias_disposition: Option<String>,
    pub darkflats_disposition: Option<String>,
    pub compression: String,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub error_message: Option<String>,
}

/// One row of `archive_operation_files`.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct ArchiveOperationFile {
    pub id: i64,
    pub operation_id: i64,
    pub file_id: Option<i64>,
    pub source_path: String,
    pub target_zip_path: String,
    pub target_path_in_zip: String,
    pub expected_hash: String,
    pub disposition: String,        // "move" | "copy"
    pub frame_role: String,         // "light" | "flat" | ...
    pub file_size_bytes: i64,
}

/// One row of `archive_operation_steps`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArchiveOperationStep {
    pub id: i64,
    pub operation_id: i64,
    pub operation_file_id: Option<i64>,
    pub stage: String,
    pub status: String,
    pub actual_hash: Option<String>,
    pub error_message: Option<String>,
    pub started_at: Option<String>,
    pub completed_at: Option<String>,
}

/// One zip the operation will produce.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct PlannedZip {
    pub zip_path: String,            // absolute
    pub zip_filename: String,
    pub frame_role: FrameRole,
    pub file_count: usize,
    pub total_size_bytes: u64,
}

/// Warning emitted by the planner when the user chose Move on a calibration set
/// that's also linked to other (non-archived) frame sets. UI uses this to
/// disable the Move radio for that calibration type.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct SharedCalibrationWarning {
    pub frame_role: FrameRole,
    pub calibration_set_id: i64,
    pub other_frames_set_ids: Vec<i64>,
}

/// Conflict emitted by the planner when a target zip filename already exists.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct ZipFilenameConflict {
    pub zip_path: String,
    pub zip_filename: String,
}

/// The complete plan for an archive operation. Returned by `plan_archive_operation`
/// for the disposition dialog preview, and (after `commit_plan`) used to drive the executor.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct ArchivePlan {
    pub frames_set_id: i64,
    pub archive_root_path: String,
    pub dispositions: Dispositions,
    pub compression: ArchiveCompression,
    pub files: Vec<ArchiveOperationFile>,        // id=0 until commit_plan persists
    pub zips: Vec<PlannedZip>,
    pub shared_calibrations: Vec<SharedCalibrationWarning>,
    pub conflicts: Vec<ZipFilenameConflict>,
    pub total_size_bytes: u64,
}

/// How to resolve filename conflicts. Provided by the user via the conflict dialog.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "snake_case")]
pub enum ConflictResolution {
    Overwrite,
    AddSuffix,
}

/// Summary used by the resume banner + Archive page.
#[derive(Debug, Clone, Serialize, Deserialize, ts_rs::TS)]
pub struct ArchiveOperationSummary {
    pub id: i64,
    pub frames_set_id: i64,
    pub frame_set_name: Option<String>,
    pub status: String,
    pub started_at: String,
    pub finished_at: Option<String>,
    pub error_message: Option<String>,
}

/// A file whose on-disk contents at `source_path` did not match the
/// archived hash during a restore reconcile (the "already on disk, skip"
/// check). The file is left exactly as found — never overwritten from the
/// zip, never have its archive markers cleared — so a re-run can still
/// process it once the user has resolved the conflict (rename/remove the
/// impostor). `actual_hash` is `None` when the file couldn't even be hashed
/// (e.g. permission error, or it vanished mid-check); that case is treated
/// the same as a mismatch, never as a pass.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RestoreConflict {
    pub file_id: Option<i64>,
    pub source_path: String,
    pub expected_hash: String,
    pub actual_hash: Option<String>,
}

/// Outcome of a `run_restore` call. A restore never aborts the whole
/// operation over a single conflicted file — every other file still gets
/// reconciled — so the caller inspects `conflicts` after a successful call
/// to know whether the operation was a clean restore or "completed with
/// conflicts".
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct RestoreOutcome {
    pub conflicts: Vec<RestoreConflict>,
}

impl RestoreOutcome {
    pub fn has_conflicts(&self) -> bool {
        !self.conflicts.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disposition_roundtrip() {
        for d in [ArchiveDisposition::Move, ArchiveDisposition::Copy, ArchiveDisposition::Skip] {
            assert_eq!(ArchiveDisposition::from_str(d.as_str()), Some(d));
        }
    }

    #[test]
    fn compression_roundtrip() {
        for c in [ArchiveCompression::Store, ArchiveCompression::Deflate] {
            assert_eq!(ArchiveCompression::from_str(c.as_str()), Some(c));
        }
    }

    #[test]
    fn status_unfinished() {
        assert!(ArchiveStatus::Copying.is_unfinished());
        assert!(ArchiveStatus::Finalizing.is_unfinished());
        assert!(!ArchiveStatus::Completed.is_unfinished());
        assert!(!ArchiveStatus::Cancelled.is_unfinished());
        assert!(!ArchiveStatus::RolledBack.is_unfinished());
        assert!(!ArchiveStatus::Failed.is_unfinished());
    }

    #[test]
    fn restore_outcome_has_conflicts() {
        let clean = RestoreOutcome::default();
        assert!(!clean.has_conflicts());

        let dirty = RestoreOutcome {
            conflicts: vec![RestoreConflict {
                file_id: Some(1),
                source_path: "/a.fits".into(),
                expected_hash: "aaaa".into(),
                actual_hash: Some("bbbb".into()),
            }],
        };
        assert!(dirty.has_conflicts());
    }

    #[test]
    fn frame_role_priority_order() {
        // Light wins over everything; bias loses to everything.
        assert!(FrameRole::Light.priority() < FrameRole::Flat.priority());
        assert!(FrameRole::Flat.priority() < FrameRole::Darkflat.priority());
        assert!(FrameRole::Darkflat.priority() < FrameRole::Dark.priority());
        assert!(FrameRole::Dark.priority() < FrameRole::Bias.priority());
    }
}
