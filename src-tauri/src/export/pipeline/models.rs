//! Data models for the export pipeline
//!
//! These types represent export jobs, steps, and their states for tracking
//! pipeline execution with checkpoint and resume capability.

use serde::{Deserialize, Serialize};
use crate::export::models::{ExportConfig, CameraType};

// ============================================================================
// Job Status Types
// ============================================================================

/// Status of an export job
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum JobStatus {
    /// Job created but not started
    Pending,
    /// Job is currently executing
    Running,
    /// Job was paused by user
    Paused,
    /// Job completed successfully
    Completed,
    /// Job failed with error
    Failed,
    /// Job was cancelled by user
    Cancelled,
}

impl JobStatus {
    /// Parse from database string
    pub fn from_str(s: &str) -> Self {
        match s {
            "pending" => JobStatus::Pending,
            "running" => JobStatus::Running,
            "paused" => JobStatus::Paused,
            "completed" => JobStatus::Completed,
            "failed" => JobStatus::Failed,
            "cancelled" => JobStatus::Cancelled,
            _ => JobStatus::Pending,
        }
    }

    /// Convert to database string
    pub fn to_str(&self) -> &'static str {
        match self {
            JobStatus::Pending => "pending",
            JobStatus::Running => "running",
            JobStatus::Paused => "paused",
            JobStatus::Completed => "completed",
            JobStatus::Failed => "failed",
            JobStatus::Cancelled => "cancelled",
        }
    }
}

/// Status of a pipeline step
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepStatus {
    /// Step not yet started
    Pending,
    /// Step is currently executing
    Running,
    /// Step completed successfully
    Completed,
    /// Step failed with error
    Failed,
    /// Step was skipped (e.g., no frames to process)
    Skipped,
}

impl StepStatus {
    /// Parse from database string
    pub fn from_str(s: &str) -> Self {
        match s {
            "pending" => StepStatus::Pending,
            "running" => StepStatus::Running,
            "completed" => StepStatus::Completed,
            "failed" => StepStatus::Failed,
            "skipped" => StepStatus::Skipped,
            _ => StepStatus::Pending,
        }
    }

    /// Convert to database string
    pub fn to_str(&self) -> &'static str {
        match self {
            StepStatus::Pending => "pending",
            StepStatus::Running => "running",
            StepStatus::Completed => "completed",
            StepStatus::Failed => "failed",
            StepStatus::Skipped => "skipped",
        }
    }
}

// ============================================================================
// Step Types
// ============================================================================

/// Type of pipeline step
///
/// Steps are aligned with actual Siril script execution:
///
/// Pre-processing (unchanged):
/// - ValidateSources: Check all source files exist
/// - OrganizeFiles: Create folder structure and generate per-step scripts
///
/// Master creation (one step per master for granular control):
/// - CreateMasterBias: Create a single master bias
/// - CreateMasterDark: Create a single master dark
/// - CreateMasterDarkFlat: Create a single master darkflat
/// - CreateMasterFlat: Create a single master flat
///
/// Light calibration (one step per branch for granular control):
/// - CalibrateBranch: Calibrate lights for a single branch
///
/// Collection (unchanged):
/// - CollectCalibrated: Collect pp_lights to unified folders (mono/osc separation)
///
/// Registration (one step per camera type):
/// - RegisterFrames: Register all frames for a camera type (mono or osc)
///
/// Stacking (one step per filter+camera combo):
/// - StackGroup: Stack a single filter+camera group
///
/// Legacy types (kept for backward compatibility with existing jobs):
/// - CreateMasters: Run 00_create_masters.ssf (all masters in one step) [LEGACY]
/// - CalibrateLights: Run 01_calibrate_lights.ssf (all lights in one step) [LEGACY]
/// - GenerateRegistration: Create 02_register_and_stack.ssf [LEGACY]
/// - RegisterAndStack: Run 02_register_and_stack.ssf [LEGACY]
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepType {
    // === Pre-processing ===
    /// Validate all source files exist
    ValidateSources,
    /// Organize files into folder structure and generate per-step scripts
    OrganizeFiles,

    // === Per-Master Steps (NEW - granular control) ===
    /// Create a single master bias (runs individual script)
    CreateMasterBias,
    /// Create a single master dark (runs individual script)
    CreateMasterDark,
    /// Create a single master darkflat (runs individual script)
    CreateMasterDarkFlat,
    /// Create a single master flat (runs individual script)
    CreateMasterFlat,

    // === Per-Branch Calibration (NEW - granular control) ===
    /// Calibrate lights for a single branch (runs individual script)
    CalibrateBranch,

    // === Per-Branch Registration (NEW - multi-group fix) ===
    /// Register a single branch with its own focal length and pixel size
    /// This is critical for multi-telescope/camera setups where each branch
    /// has different optical parameters
    RegisterBranch,

    // === Collection ===
    /// Collect registered frames using branch metadata (mono/osc separation)
    CollectRegistered,

    // === Global Registration (NEW - multi-group fix) ===
    /// Global homography registration to align frames from different optical setups
    /// Runs after per-branch registration, uses -transf=homography -2pass
    GlobalRegistration,

    // === Prepare Stacking (NEW - multi-group fix) ===
    /// Copy globally registered frames to stacking directories
    PrepareStacking,

    // === Per-Camera Registration (LEGACY - kept for backward compatibility) ===
    /// Register all frames for a camera type (mono or osc)
    RegisterFrames,

    // === Per-Group Stacking (NEW - granular control) ===
    /// Stack a single filter+camera group
    StackGroup,

    // === Collection (LEGACY - kept for backward compatibility) ===
    /// Collect calibrated frames to unified folder (mono/osc separation)
    #[serde(alias = "collect_calibrated")]
    CollectCalibrated,

    // === Legacy Types (backward compatibility) ===
    /// Create all master calibration frames (runs 00_create_masters.ssf)
    #[serde(alias = "create_masters")]
    CreateMasters,
    /// Calibrate all light frames (runs 01_calibrate_lights.ssf)
    #[serde(alias = "calibrate_lights")]
    CalibrateLights,
    /// Generate registration script based on collected frames
    #[serde(alias = "generate_registration")]
    GenerateRegistration,
    /// Register and stack all frames (runs 02_register_and_stack.ssf)
    #[serde(alias = "register_and_stack")]
    RegisterAndStack,
}

impl StepType {
    /// Parse from database string
    pub fn from_str(s: &str) -> Self {
        match s {
            // Pre-processing
            "validate_sources" => StepType::ValidateSources,
            "organize_files" => StepType::OrganizeFiles,
            // Per-master steps (new)
            "create_master_bias" => StepType::CreateMasterBias,
            "create_master_dark" => StepType::CreateMasterDark,
            "create_master_darkflat" => StepType::CreateMasterDarkFlat,
            "create_master_flat" => StepType::CreateMasterFlat,
            // Per-branch calibration (new)
            "calibrate_branch" => StepType::CalibrateBranch,
            // Per-branch registration (multi-group fix)
            "register_branch" => StepType::RegisterBranch,
            // Collection
            "collect_registered" => StepType::CollectRegistered,
            "collect_calibrated" => StepType::CollectCalibrated,
            // Global registration (multi-group fix)
            "global_registration" => StepType::GlobalRegistration,
            // Prepare stacking (multi-group fix)
            "prepare_stacking" => StepType::PrepareStacking,
            // Per-camera registration (legacy)
            "register_frames" => StepType::RegisterFrames,
            // Per-group stacking (new)
            "stack_group" => StepType::StackGroup,
            // Legacy types (backward compatibility)
            "create_masters" => StepType::CreateMasters,
            "calibrate_lights" => StepType::CalibrateLights,
            "generate_registration" => StepType::GenerateRegistration,
            "register_and_stack" => StepType::RegisterAndStack,
            _ => StepType::ValidateSources,
        }
    }

    /// Convert to database string
    pub fn to_str(&self) -> &'static str {
        match self {
            // Pre-processing
            StepType::ValidateSources => "validate_sources",
            StepType::OrganizeFiles => "organize_files",
            // Per-master steps (new)
            StepType::CreateMasterBias => "create_master_bias",
            StepType::CreateMasterDark => "create_master_dark",
            StepType::CreateMasterDarkFlat => "create_master_darkflat",
            StepType::CreateMasterFlat => "create_master_flat",
            // Per-branch calibration (new)
            StepType::CalibrateBranch => "calibrate_branch",
            // Per-branch registration (multi-group fix)
            StepType::RegisterBranch => "register_branch",
            // Collection
            StepType::CollectRegistered => "collect_registered",
            StepType::CollectCalibrated => "collect_calibrated",
            // Global registration (multi-group fix)
            StepType::GlobalRegistration => "global_registration",
            // Prepare stacking (multi-group fix)
            StepType::PrepareStacking => "prepare_stacking",
            // Per-camera registration (legacy)
            StepType::RegisterFrames => "register_frames",
            // Per-group stacking (new)
            StepType::StackGroup => "stack_group",
            // Legacy types
            StepType::CreateMasters => "create_masters",
            StepType::CalibrateLights => "calibrate_lights",
            StepType::GenerateRegistration => "generate_registration",
            StepType::RegisterAndStack => "register_and_stack",
        }
    }

    /// Get human-readable display name
    pub fn display_name(&self) -> &'static str {
        match self {
            // Pre-processing
            StepType::ValidateSources => "Validate Sources",
            StepType::OrganizeFiles => "Organize Files",
            // Per-master steps (new)
            StepType::CreateMasterBias => "Create Master Bias",
            StepType::CreateMasterDark => "Create Master Dark",
            StepType::CreateMasterDarkFlat => "Create Master DarkFlat",
            StepType::CreateMasterFlat => "Create Master Flat",
            // Per-branch calibration (new)
            StepType::CalibrateBranch => "Calibrate Branch",
            // Per-branch registration (multi-group fix)
            StepType::RegisterBranch => "Register Branch",
            // Collection
            StepType::CollectRegistered => "Collect Registered Frames",
            StepType::CollectCalibrated => "Collect Calibrated Frames",
            // Global registration (multi-group fix)
            StepType::GlobalRegistration => "Global Registration",
            // Prepare stacking (multi-group fix)
            StepType::PrepareStacking => "Prepare Stacking",
            // Per-camera registration (legacy)
            StepType::RegisterFrames => "Register Frames",
            // Per-group stacking (new)
            StepType::StackGroup => "Stack Group",
            // Legacy types
            StepType::CreateMasters => "Create Master Calibrations",
            StepType::CalibrateLights => "Calibrate Light Frames",
            StepType::GenerateRegistration => "Generate Registration Script",
            StepType::RegisterAndStack => "Register & Stack",
        }
    }

    /// Whether this step type can be safely resumed from
    pub fn can_resume_from(&self) -> bool {
        // All steps can be safely resumed from since they check for existing outputs
        true
    }

    /// Check if this is a legacy (monolithic) step type
    pub fn is_legacy(&self) -> bool {
        matches!(
            self,
            StepType::CreateMasters
                | StepType::CalibrateLights
                | StepType::GenerateRegistration
                | StepType::RegisterAndStack
                | StepType::RegisterFrames
                | StepType::CollectCalibrated
        )
    }

    /// Get the phase group for UI display
    pub fn phase(&self) -> StepPhase {
        match self {
            StepType::ValidateSources | StepType::OrganizeFiles => StepPhase::Preparation,
            StepType::CreateMasterBias
            | StepType::CreateMasterDark
            | StepType::CreateMasterDarkFlat
            | StepType::CreateMasterFlat
            | StepType::CreateMasters => StepPhase::Masters,
            StepType::CalibrateBranch | StepType::CalibrateLights => StepPhase::Calibration,
            StepType::RegisterBranch => StepPhase::BranchRegistration,
            StepType::CollectRegistered | StepType::CollectCalibrated | StepType::GenerateRegistration => StepPhase::Collection,
            StepType::GlobalRegistration | StepType::RegisterFrames | StepType::RegisterAndStack => StepPhase::GlobalRegistration,
            StepType::PrepareStacking => StepPhase::PrepareStacking,
            StepType::StackGroup => StepPhase::Stacking,
        }
    }
}

/// Phase grouping for UI display
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum StepPhase {
    /// Validation and file organization
    Preparation,
    /// Master calibration frame creation
    Masters,
    /// Light frame calibration
    Calibration,
    /// Per-branch registration (with branch-specific optics)
    BranchRegistration,
    /// Collecting registered frames
    Collection,
    /// Global registration (homography + 2pass)
    GlobalRegistration,
    /// Prepare stacking directories
    PrepareStacking,
    /// Stacking by filter/camera groups
    Stacking,
}

impl StepPhase {
    pub fn display_name(&self) -> &'static str {
        match self {
            StepPhase::Preparation => "Preparation",
            StepPhase::Masters => "Masters",
            StepPhase::Calibration => "Calibration",
            StepPhase::BranchRegistration => "Branch Registration",
            StepPhase::Collection => "Collection",
            StepPhase::GlobalRegistration => "Global Registration",
            StepPhase::PrepareStacking => "Prepare Stacking",
            StepPhase::Stacking => "Stacking",
        }
    }

    pub fn to_str(&self) -> &'static str {
        match self {
            StepPhase::Preparation => "preparation",
            StepPhase::Masters => "masters",
            StepPhase::Calibration => "calibration",
            StepPhase::BranchRegistration => "branch_registration",
            StepPhase::Collection => "collection",
            StepPhase::GlobalRegistration => "global_registration",
            StepPhase::PrepareStacking => "prepare_stacking",
            StepPhase::Stacking => "stacking",
        }
    }
}

// ============================================================================
// Step Data
// ============================================================================

/// Step-specific metadata
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct StepData {
    /// Calibration set ID (for master creation steps)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_id: Option<i64>,
    /// Branch ID (for calibrate_branch step)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_id: Option<String>,
    /// Branch index (0-based, for calibrate_branch step)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub branch_index: Option<i32>,
    /// Filter name (for stack_group step)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filter: Option<String>,
    /// Camera type (for register_frames and stack_group steps)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub camera_type: Option<CameraType>,
    /// Number of frames to process
    #[serde(skip_serializing_if = "Option::is_none")]
    pub frame_count: Option<i32>,
    /// Path to generated Siril script
    #[serde(skip_serializing_if = "Option::is_none")]
    pub siril_script_path: Option<String>,
    /// Master file output path
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_path: Option<String>,
    /// Calibration set name for display
    #[serde(skip_serializing_if = "Option::is_none")]
    pub set_name: Option<String>,
    /// List of input file paths
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_files: Option<Vec<String>>,
    /// Master type (Bias, Dark, DarkFlat, Flat) for per-master steps
    #[serde(skip_serializing_if = "Option::is_none")]
    pub master_type: Option<String>,
    /// Step orders this step depends on (for dependency tracking)
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub depends_on_steps: Vec<i32>,
    /// Exposure time display string (for stack_group with exptime grouping)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exptime_display: Option<String>,
    /// Stack group key (for stack_group step)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_key: Option<String>,
    /// Output folder path (for calibrate_branch - where pp_ files will be created)
    /// This ensures executor uses the same path as the script generator
    #[serde(skip_serializing_if = "Option::is_none")]
    pub output_folder_path: Option<String>,
    /// Focal length in mm (for register_branch step)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub focal_length: Option<f64>,
    /// Pixel size in micrometers (for register_branch step)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pixel_size: Option<f64>,
}

// ============================================================================
// Export Job
// ============================================================================

/// Complete export job with all steps
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportJob {
    /// Job ID in database
    pub id: i64,
    /// Frame set being exported
    pub frame_set_id: i64,
    /// Output directory
    pub output_dir: String,
    /// Export target (siril or pixinsight_wbpp)
    pub target: String,
    /// Full export configuration (serialized JSON)
    pub config: ExportConfig,
    /// Current job status
    pub status: JobStatus,
    /// When the job was created
    pub created_at: String,
    /// When the job started executing
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// When the job completed (success or failure)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    /// Error message if failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Total number of steps in this job
    pub total_steps: i32,
    /// Number of completed steps
    pub completed_steps: i32,
    /// All steps in this job (loaded separately)
    #[serde(default)]
    pub steps: Vec<ExportJobStep>,
}

/// A single step in an export job
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExportJobStep {
    /// Step ID in database
    pub id: i64,
    /// Parent job ID
    pub job_id: i64,
    /// Order of this step (1-based)
    pub step_order: i32,
    /// Type of step
    pub step_type: StepType,
    /// Human-readable step name
    pub step_name: String,
    /// Step-specific data
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_data: Option<StepData>,
    /// Current step status
    pub status: StepStatus,
    /// When the step started
    #[serde(skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// When the step completed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    /// Error message if failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Number of retry attempts
    pub retry_count: i32,
    /// Output files created by this step
    #[serde(default)]
    pub output_files: Vec<String>,
}

// ============================================================================
// File Validation
// ============================================================================

/// File type for validation tracking
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FileType {
    /// Source light frame
    Source,
    /// Calibration frame (dark, flat, bias)
    Calibration,
    /// Output file (stacked result)
    Output,
    /// Intermediate file (calibrated, registered)
    Intermediate,
}

impl FileType {
    pub fn to_str(&self) -> &'static str {
        match self {
            FileType::Source => "source",
            FileType::Calibration => "calibration",
            FileType::Output => "output",
            FileType::Intermediate => "intermediate",
        }
    }

    pub fn from_str(s: &str) -> Self {
        match s {
            "source" => FileType::Source,
            "calibration" => FileType::Calibration,
            "output" => FileType::Output,
            "intermediate" => FileType::Intermediate,
            _ => FileType::Source,
        }
    }
}

/// Validation result for a single file
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileValidation {
    /// File path
    pub path: String,
    /// Whether the file exists
    pub exists: bool,
    /// File size in bytes (if exists)
    #[serde(skip_serializing_if = "Option::is_none")]
    pub size: Option<i64>,
    /// Error message if validation failed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
    /// File type
    pub file_type: FileType,
}

// ============================================================================
// Pipeline Planning
// ============================================================================

/// A planned step before job creation
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlannedStep {
    /// Type of step
    pub step_type: StepType,
    /// Human-readable step name
    pub step_name: String,
    /// Step-specific data
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_data: Option<StepData>,
    /// Step orders this step depends on (must complete first)
    #[serde(default)]
    pub depends_on: Vec<i32>,
    /// Whether this step can be resumed from
    pub can_resume_from: bool,
}

/// Complete pipeline plan with all steps and validations
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelinePlan {
    /// Ordered list of steps to execute
    pub steps: Vec<PlannedStep>,
    /// Source file validations
    pub source_validations: Vec<FileValidation>,
    /// Calibration file validations
    pub calibration_validations: Vec<FileValidation>,
    /// Total number of source files
    pub total_source_files: i32,
    /// Total number of calibration files
    pub total_calibration_files: i32,
    /// Number of missing source files
    pub missing_source_files: i32,
    /// Number of missing calibration files
    pub missing_calibration_files: i32,
    /// Whether the plan has validation errors that block execution
    pub has_blocking_errors: bool,
    /// Warning messages
    #[serde(default)]
    pub warnings: Vec<String>,
}

impl Default for PipelinePlan {
    fn default() -> Self {
        Self {
            steps: Vec::new(),
            source_validations: Vec::new(),
            calibration_validations: Vec::new(),
            total_source_files: 0,
            total_calibration_files: 0,
            missing_source_files: 0,
            missing_calibration_files: 0,
            has_blocking_errors: false,
            warnings: Vec::new(),
        }
    }
}

// ============================================================================
// Detected Work (for resume)
// ============================================================================

/// Work detected in output directory (for resume logic)
#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct DetectedWork {
    /// Master bias files found
    pub master_bias_files: Vec<String>,
    /// Master dark files found
    pub master_dark_files: Vec<String>,
    /// Master flat files found
    pub master_flat_files: Vec<String>,
    /// Master darkflat files found
    pub master_darkflat_files: Vec<String>,
    /// Calibrated light files found (pp_*)
    pub calibrated_light_files: Vec<String>,
    /// Registered light files found (r_pp_*)
    pub registered_light_files: Vec<String>,
    /// Stacked output files found
    pub stacked_files: Vec<String>,
}

impl DetectedWork {
    /// Check if any work has been done
    pub fn has_any_work(&self) -> bool {
        !self.master_bias_files.is_empty()
            || !self.master_dark_files.is_empty()
            || !self.master_flat_files.is_empty()
            || !self.master_darkflat_files.is_empty()
            || !self.calibrated_light_files.is_empty()
            || !self.registered_light_files.is_empty()
            || !self.stacked_files.is_empty()
    }
}

// ============================================================================
// Progress Events
// ============================================================================

/// Enhanced export progress for pipeline execution
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PipelineProgress {
    /// Job ID
    pub job_id: i64,
    /// Current step ID
    #[serde(skip_serializing_if = "Option::is_none")]
    pub step_id: Option<i64>,
    /// Current step order (1-based)
    pub step_order: i32,
    /// Total steps in job
    pub total_steps: i32,
    /// Step type
    pub step_type: StepType,
    /// Progress within current step (0.0 - 1.0)
    pub step_progress: f64,
    /// Overall job progress (0.0 - 1.0)
    pub overall_progress: f64,
    /// Human-readable message
    pub message: String,
    /// Current file being processed
    #[serde(skip_serializing_if = "Option::is_none")]
    pub current_file: Option<String>,
    /// Whether the current step can be resumed from
    pub is_resumable: bool,
    /// Whether the current step can be retried
    pub can_retry: bool,
}
