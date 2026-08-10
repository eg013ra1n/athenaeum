//! Assembles the 6 frontend type files from the Rust model types.
//! Diffed against disk by tests/ts_contract.rs; regenerate with:
//!   TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract
//!
//! Registry order matches the Task 5 TS -> Rust inventory
//! (.superpowers/sdd/p1-ts-inventory.md): one line per mapped declaration, in
//! the order it appeared in the original hand-written file. Cross-checked:
//! none of the 138 mapped types reference a type that lives in a *different*
//! one of the 6 output files, so none of the preambles below need an
//! `import type { .. } from './other-file'` line.

use ts_rs::TS;

const HEADER: &str = "// AUTO-GENERATED from Rust by athenaeum-core/src/ts_export.rs — do not edit.\n\
                      // Regenerate: TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract\n\n";

/// ts-rs 12 threads a `Config` through `TS::decl` (previously argument-less).
/// Deliberately `Config::default()` and NOT `Config::from_env()`: this file is a
/// checked-in contract diffed by tests/ts_contract.rs, so a stray
/// `TS_RS_LARGE_INT` / `TS_RS_USE_V11_HASHMAP` in someone's shell must not
/// silently regenerate different bindings. Generation stays hermetic.
fn ts_config() -> ts_rs::Config {
    ts_rs::Config::default()
}

macro_rules! decls {
    ($($t:ty),* $(,)?) => {{
        let cfg = ts_config();
        let mut out = String::new();
        $( out.push_str(&format!("export {}\n\n", <$t as TS>::decl(&cfg))); )*
        out
    }};
}

/// ts-rs 12 maps `i64`/`u64` (and `i128`/`u128`) to TS `bigint`; only
/// `usize`/`isize`/`f32`/`f64` map to `number`. Since ts-rs 12 that mapping IS
/// globally configurable — `Config::with_large_int("number")`, or its
/// `TS_RS_LARGE_INT` env equivalent, which `ts_config()` above deliberately
/// does not read — alongside the per-field `#[ts(type = "..")]` override that
/// was the only lever on ts-rs 10.
///
/// Every `i64`/`u64` field in this codebase is a SQLite
/// `INTEGER PRIMARY KEY` / row count / byte size that (a) crosses the Tauri
/// IPC boundary as a plain `serde_json` number, never a JS `BigInt`, and (b)
/// never approaches `Number.MAX_SAFE_INTEGER`. The pre-Task-6 hand-written
/// files always typed these fields `number` (verified against `git show
/// HEAD:src/types/*.ts`), and the frontend already does plain numeric
/// arithmetic/comparisons on ids — switching ~150 occurrences to `bigint`
/// would both misrepresent the wire format and require rewriting arithmetic
/// across dozens of consumer files for no behavioral benefit. This
/// harness-level substitution keeps the fix in one place instead of
/// bulk-adding `#[ts(type = "number")]` to every affected field (100+ sites
/// across models.rs/export/models.rs/archive/models.rs); on ts-rs 12
/// `ts_config().with_large_int("number")` is an equivalent one-line
/// replacement for it (checked: with the substitution disabled it reproduces
/// the checked-in bindings byte-for-byte), left for its own change rather than
/// folded into a dependency refresh. Safe as a plain literal
/// replacement: `bigint` does not otherwise appear anywhere in this crate's
/// source (verified via grep), so there is no risk of clobbering a doc
/// comment or identifier that happens to contain the substring.
fn js_safe_ints(ts: String) -> String {
    ts.replace("bigint", "number")
}

pub fn generated_files() -> Vec<(&'static str, String)> {
    vec![
        ("models.ts", js_safe_ints(format!("{HEADER}{}", decls![
            crate::models::FileFormat,
            crate::models::ImageType,
            crate::models::File,
            crate::models::Frame,
            crate::models::ScanRoot,
            crate::api::scan_roots::FolderCandidateVerdict,
            crate::api::scan_roots::ScanRootOverview,
            crate::api::scan_roots::ArchiveRootOverview,
            crate::api::scan_roots::FolderOverview,
            crate::models::DuplicateFile,
            crate::models::DuplicateGroup,
            crate::models::BulkMoveResult,
            crate::db::BulkMoveProgressEvent,
            crate::models::BlackHoleEntry,
            crate::models::FolderSimilarity,
            crate::scanner::ScanProgressEvent,
            crate::scanner::CalibratedDuplicate,
            crate::scanner::ScanCompleteEvent,
            crate::models::FileWithFrame,
            crate::models::FramesSet,
            crate::models::Setting,
            crate::models::ImagingNight,
            crate::models::Session,
            crate::models::SessionWithFrames,
            crate::models::ImagingNightWithSessions,
            crate::models::FrameSetDetail,
            crate::models::CameraStats,
            crate::models::CalibrationSetDetail,
            crate::models::RelinkResult,
            crate::models::OrphanedFile,
            crate::models::ImagingLocation,
            crate::models::CalibrationLink,
            crate::models::FrameCalibrationStatus,
            crate::models::CalibrationHierarchy,
            crate::models::CalibrationSetWithLinks,
            crate::models::CalibrationWarning,
            crate::models::CalibrationStats,
            crate::models::CalibrationGroup,
            crate::models::FrameSetCalibrationGroups,
            crate::models::CalibrationTolerance,
            crate::calibration::processor::ProcessingProgress,
            crate::calibration::processor::ProcessingStats,
            crate::calibration::flat_matcher::FlatTiming,
            crate::calibration::flat_groups::FlatGroup,
            crate::calibration::flat_matcher::FlatGroupMatch,
            crate::models::CalibrationHierarchyView,
            crate::models::CalibrationDateGroup,
            crate::models::CalibrationCameraGroup,
            crate::models::SubCalibrationDetail,
            crate::models::CalibrationSetConsumer,
            crate::models::CalibrationSetWithFrameCount,
            crate::models::CalibrationSetWithScore,
            crate::models::MatchDetails,
            crate::models::LightFrameParameters,
            crate::models::CalibrationSetParameters,
            crate::models::CalibrationFilterGroup,
            crate::models::LightFrameWithCalibration,
            crate::models::CalendarFrameSetSummary,
            crate::models::CalendarUnorganizedGroup,
            crate::models::CalendarDayEvent,
            crate::models::CalendarMonthData,
            crate::models::ExcludedFrameEntry,
            crate::models::ExcludedFrameRow,
            crate::models::FrameAnalysis,
            crate::models::CalibrationMetadataEdits,
            crate::models::StarMetric,
            crate::models::StarMetricsResponse,
            crate::models::CalibrationSetOriginals,
            crate::models::MissingMetadataRow,
            crate::models::FrameMetadataEdits,
            crate::models::SkipReason,
            crate::models::CandidateFrame,
            crate::models::SkippedFrame,
            crate::models::FindNewFramesResult,
            crate::models::MergeReport,
            crate::models::MergeLogEntry,
            crate::flat_analysis::FlatContourOpts,
            crate::registration::db::FrameSetReference,
            crate::registration::db::RegistrationRecord,
            crate::registration::service::StackingPrepProgressEvent,
            crate::registration::service::StackingPrepCompleteEvent,
            crate::logging::config::LoggingConfig,
            crate::logging::config::LoggingConfigResponse,
            crate::services::compute_queue::ComputeJobKind,
            crate::services::compute_queue::ComputeJobState,
            crate::services::compute_queue::ComputeQueueEntry,
            crate::duplicates::backfill::ContentIndexProgress,
            crate::duplicates::backfill::ContentIndexFinished,
            crate::api::content_index::ContentIndexStatus,
            crate::integration::combine::Combination,
            crate::integration::combine::Rejection,
            crate::integration::combine::IntegrationRecipe,
            crate::api::masters::MasterRecipe,
            crate::api::masters::MasterBuildPreview,
            crate::api::masters::RawPrecalSetDto,
            crate::api::masters::MasterProvenanceInfo,
            crate::api::masters::BatchBuildReport,
            crate::api::masters::BatchSkip,
            crate::api::masters::DeleteMasterResult,
            crate::api::lights::LightFrameReadiness,
            crate::api::lights::LightCalReadiness,
            crate::api::lights::ExportReadiness,
            crate::api::lights::LightCalDetails,
            crate::api::lights::LightCalScope,
            crate::api::lights::FlatNormMode,
            crate::api::lights::LightCalParams,
            crate::api::lights::BiasFallback,
            crate::sync::models::Direction,
            crate::sync::models::OutboundState,
            crate::sync::models::InboundState,
            crate::sync::models::HistoryRow,
            crate::sync::status::TransferFileCounts,
            crate::sync::status::OutboundSummary,
            crate::sync::status::InboundSummary,
            crate::sync::status::QueuedInboundSummary,
            crate::sync::status::SyncSenderStatus,
            crate::sync::status::SyncReceiverStatus,
            crate::sync::status::TransportHealth,
            crate::sync::status::SyncStatus,
            crate::sync::status::TransferFileEntry,
            crate::api::sync::TerminalTransfers,
            crate::sync::receiver::SyncProgressEvent,
            crate::sync::receiver::SyncFileProgressEvent,
            crate::sync::receiver::SyncFinishedEvent,
            crate::api::sync::SyncHistoryQuery,
            crate::api::sync::IneligibleFrame,
            crate::api::sync::EnqueueSelectionResult,
            crate::api::sync::TransferEventEntry,
            crate::api::sync::DeletedTransferRecord,
            crate::api::sync::TransferStorage,
            crate::api::sync::TransferCleanup,
            crate::account::DeviceCapability,
            crate::account::EndpointAddrReport,
            crate::account::AccountDevice,
            crate::account::AccountStatus,
            crate::api::collab::ProjectCard,
            crate::api::collab::ProjectDetail,
            crate::api::collab::ProjectMemberView,
            crate::api::collab::LinkedSetView,
            crate::api::collab::LinkSuggestion,
            crate::api::collab::GateReport,
            crate::api::collab::ProjectSetMatch,
            crate::api::collab::ProjectSetMatchEvent,
            crate::api::collab::PortalNewProjectLink,
            crate::api::collab::PublishResult,
            crate::api::collab::ModerationFrame,
            crate::api::collab::ModerationItem,
            crate::api::collab_exchange::PackageStateChange,
            crate::api::collab_exchange::ProjectPackageView,
            crate::api::collab_exchange::ProjectDownloadProgress,
            crate::api::collab_exchange::ContributionView,
            crate::collab::gate::FrameGateRow,
            crate::collab::gate::ThresholdRuleView,
        ]))),
        ("archive.ts", js_safe_ints(format!("{HEADER}{}", decls![
            crate::archive::models::ArchiveDisposition,
            crate::archive::models::ArchiveCompression,
            crate::archive::models::ConflictResolution,
            crate::archive::models::FrameRole,
            crate::archive::models::Dispositions,
            crate::archive::models::ArchiveOperationFile,
            crate::archive::models::PlannedZip,
            crate::archive::models::SharedCalibrationWarning,
            crate::archive::models::ZipFilenameConflict,
            crate::archive::models::ArchivePlan,
            crate::archive::models::ArchiveOperationSummary,
        ]))),
        ("export.ts", js_safe_ints(format!("{HEADER}{}", decls![
            crate::export::models::CameraType,
            crate::export::models::ExportFrame,
            crate::export::models::CalibrationSetInfo,
            crate::export::models::CalibrationSubgroup,
            crate::export::models::ExportGroup,
            crate::export::models::CalibrationSummary,
            crate::export::models::MasterCreationPlan,
            crate::export::models::MasterInfo,
            crate::export::models::ExportData,
            crate::export::models::FilterExportGroup,
            crate::export::models::ExportCalibrationSet,
            crate::export::models::CalibrationRoute,
            crate::export::models::CalibrationRouteGroup,
            crate::export::models::CalibrationTreeNode,
            crate::export::models::CalibrationRouteSummary,
            crate::export::models::ExportResult,
            crate::export::models::ExportSummary,
            crate::export::models::FilterGroupSummary,
            crate::export::models::ExposureGroup,
            crate::export::models::CalibrationDetail,
            crate::export::models::FrameDetail,
            crate::export::models::FolderPreview,
            crate::export::models::FolderNode,
            crate::export::models::FolderNodeType,
            crate::export::models::DetailedWarning,
            crate::export::models::WarningType,
            crate::export::models::WarningSeverity,
            crate::export::models::ExportProgressEvent,
            crate::export::models::ExportCompleteEvent,
            crate::export::models::ExportMode,
            crate::export::models::WbppExportConfig,
            crate::export::models::WbppSetupInstructions,
            crate::export::models::WbppKeywordInstruction,
        ]))),
        ("calibration-config.ts", js_safe_ints(format!("{HEADER}{}", decls![
            crate::calibration::config::MatchMode,
            crate::calibration::config::ParameterConfig,
            crate::calibration::config::CalibrationTypeConfig,
            crate::calibration::config::SourceTypeConfig,
            crate::calibration::config::BehavioralOptions,
            crate::calibration::config::MasterPreference,
            crate::calibration::config::ClusteringConfig,
            crate::calibration::config::ScoringConfig,
            crate::calibration::config::WarningConfig,
            crate::calibration::config::CalibrationMatchingConfig,
        ]))),
        ("plate-solve.ts", js_safe_ints(format!("{HEADER}{}", decls![
            crate::plate_solve::config::PlateSolveConfig,
            crate::plate_solve::storage::PlateSolveRecord,
            crate::plate_solve::hints::FovSummary,
            crate::plate_solve::object_fill::AutofindStatus,
        ]))),
        ("analysis-config.ts", js_safe_ints(format!("{HEADER}{}", decls![
            crate::analysis::config::AnalysisConfig,
            crate::rustafits_processor::AnnotationSettings,
        ]))),
    ]
}
