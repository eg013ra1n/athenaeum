//! Data types for the file_op (Move/Delete) feature.

use serde::{Deserialize, Serialize};

/// What kind of file operation. Persisted as `file_operations.kind`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum FileOpKind {
    Move,
    Delete,
}

impl FileOpKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            FileOpKind::Move => "move",
            FileOpKind::Delete => "delete",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "move" => Some(Self::Move),
            "delete" => Some(Self::Delete),
            _ => None,
        }
    }
}

/// State machine for `file_operations.status`. Mirrors `ArchiveStatus`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOpStatus {
    Pending,
    Running,
    Completed,
    Cancelled,
    Failed,
}

impl FileOpStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            FileOpStatus::Pending => "pending",
            FileOpStatus::Running => "running",
            FileOpStatus::Completed => "completed",
            FileOpStatus::Cancelled => "cancelled",
            FileOpStatus::Failed => "failed",
        }
    }

    pub fn is_unfinished(&self) -> bool {
        !matches!(
            self,
            FileOpStatus::Completed | FileOpStatus::Cancelled | FileOpStatus::Failed
        )
    }
}

/// Per-file move strategy. Persisted as `file_operation_files.strategy`.
///
/// Chosen by the planner from the source/destination filesystem device IDs.
/// `Delete` is reserved for FileOpKind::Delete (Phase 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MoveStrategy {
    /// Same-volume: a single `rename(2)` is atomic and instant.
    AtomicRename,
    /// Cross-volume: copy file, hash-verify, then delete source.
    /// Survives interruption because each step is logged.
    CopyVerifyDelete,
    /// Phase 2: Delete kind. Just remove the file.
    Delete,
}

impl MoveStrategy {
    pub fn as_str(&self) -> &'static str {
        match self {
            MoveStrategy::AtomicRename => "atomic_rename",
            MoveStrategy::CopyVerifyDelete => "copy_verify_delete",
            MoveStrategy::Delete => "delete",
        }
    }

    pub fn from_str(s: &str) -> Option<Self> {
        match s {
            "atomic_rename" => Some(Self::AtomicRename),
            "copy_verify_delete" => Some(Self::CopyVerifyDelete),
            "delete" => Some(Self::Delete),
            _ => None,
        }
    }
}

/// Disposition of one planned file row. `file_operation_files.disposition`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileDisposition {
    Planned,
    InProgress,
    Done,
    Failed,
    Skipped,
}

impl FileDisposition {
    pub fn as_str(&self) -> &'static str {
        match self {
            FileDisposition::Planned => "planned",
            FileDisposition::InProgress => "in_progress",
            FileDisposition::Done => "done",
            FileDisposition::Failed => "failed",
            FileDisposition::Skipped => "skipped",
        }
    }
}

/// Step stages for `file_operation_steps.stage`. Each per-file action records
/// a sequence of these to allow resume + rollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOpStage {
    /// Cross-volume: copy source bytes to destination.
    Copy,
    /// Cross-volume: hash destination and compare to expected.
    Verify,
    /// Hot-sync: update files.path in DB and either rename(2) (atomic) or
    /// delete-source (cross-volume) inside the same transaction.
    CommitMove,
}

impl FileOpStage {
    pub fn as_str(&self) -> &'static str {
        match self {
            FileOpStage::Copy => "copy",
            FileOpStage::Verify => "verify",
            FileOpStage::CommitMove => "commit_move",
        }
    }
}

/// Step status. Same shape as ArchiveStage's StepStatus, kept separate to
/// avoid cross-module coupling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    Pending,
    InProgress,
    Done,
    Failed,
}

impl StepStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            StepStatus::Pending => "pending",
            StepStatus::InProgress => "in_progress",
            StepStatus::Done => "done",
            StepStatus::Failed => "failed",
        }
    }
}

/// One row of `file_operations`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOperation {
    pub id: i64,
    pub kind: String,
    pub status: String,
    pub source_root: Option<String>,
    pub dest_dir: Option<String>,
    pub total_files: i64,
    pub total_bytes: i64,
    pub created_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub error_message: Option<String>,
}

/// One row of `file_operation_files`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOperationFile {
    pub id: i64,
    pub operation_id: i64,
    pub source_path: String,
    pub dest_path: Option<String>,
    pub strategy: String,
    pub catalog_file_id: Option<i64>,
    pub expected_hash: Option<String>,
    pub file_size_bytes: i64,
    pub disposition: String,
}

/// One row of `file_operation_steps`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOperationStep {
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

/// Result of `planner::build_plan`. The plan is persisted to the database
/// (creating one `file_operations` row + one `file_operation_files` row per
/// resolved file) before the executor runs.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOpPlan {
    pub operation_id: i64,
    pub kind: FileOpKind,
    pub source_root: Option<String>,
    pub dest_dir: Option<String>,
    pub files: Vec<FileOperationFile>,
    pub total_files: i64,
    pub total_bytes: i64,
}

/// Progress payload emitted on the `file-op-progress` Tauri/SSE event channel.
/// Matches the shape of archive's `ArchiveProgress` so the UI can use a unified widget.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOpProgress {
    pub operation_id: i64,
    pub kind: String,        // "move" | "delete"
    pub stage: String,       // matches FileOpStage::as_str values
    pub current: usize,
    pub total: usize,
    pub message: String,
}

/// Payload emitted on the `file-op-reconciled` Tauri/SSE event channel when
/// `file_op::reconcile::reconcile_abandoned_commit_moves` heals or skips at
/// least one abandoned cross-volume move. See `file_op::reconcile` for what
/// "healed" / "skipped" mean.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOpReconciled {
    pub healed: usize,
    pub skipped: usize,
    pub operation_ids: Vec<i64>,
}
