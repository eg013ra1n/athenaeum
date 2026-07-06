// AUTO-GENERATED from Rust by athenaeum-core/src/ts_export.rs — do not edit.
// Regenerate: TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract

export type CameraType = "osc" | "mono";

export type ExportFrame = { 
/**
 * Frame ID in database
 */
frameId: number, 
/**
 * File ID in database
 */
fileId: number, 
/**
 * Full file path
 */
filePath: string, 
/**
 * Filename only
 */
filename: string, 
/**
 * Exposure time in seconds
 */
exptime: number | null, 
/**
 * Filter name
 */
filter: string | null, 
/**
 * CCD temperature
 */
ccdTemp: number | null, 
/**
 * Gain setting
 */
gain: number | null, 
/**
 * Offset setting
 */
offset: number | null, 
/**
 * Binning (e.g., "1x1")
 */
binning: string | null, 
/**
 * Date observed
 */
dateObs: string | null, 
/**
 * Focal length in mm
 */
focallen: number | null, 
/**
 * Pixel size in micrometers (from XPIXSZ or PIXSIZE1 FITS header)
 */
xpixsz: number | null, 
/**
 * Bayer pattern for OSC detection (e.g., "RGGB")
 */
bayerpat: string | null, 
/**
 * Camera/instrument name
 */
instrume: string | null, };

export type CalibrationSetInfo = { 
/**
 * Calibration set ID
 */
setId: number, 
/**
 * Image type (FLAT, DARK, BIAS, DARKFLAT)
 */
imagetyp: string, 
/**
 * Frames in this calibration set
 */
frames: Array<ExportFrame>, 
/**
 * Frame count
 */
frameCount: number, 
/**
 * Sub-calibration: DarkFlat set (for Flats)
 */
darkFlat: CalibrationSetInfo | null, 
/**
 * Sub-calibration: Dark set (for Flats or Lights)
 */
dark: CalibrationSetInfo | null, 
/**
 * Sub-calibration: Bias set (for Flats, Darks, or Lights)
 */
bias: CalibrationSetInfo | null, 
/**
 * Match quality score (0.0 - 1.0)
 */
matchScore: number | null, 
/**
 * Warnings (date, temperature mismatch, etc.)
 */
warnings: Array<string>, };

export type CalibrationSubgroup = { 
/**
 * Unique subgroup key (hash of calibration set IDs)
 */
subgroupKey: string, 
/**
 * Display name (e.g., "Night 1 - Camera X" or auto-generated)
 */
displayName: string, 
/**
 * Light frames in this subgroup
 */
frames: Array<ExportFrame>, 
/**
 * Linked Flat calibration set (with its own sub-calibrations)
 */
flat: CalibrationSetInfo | null, 
/**
 * Linked Dark calibration set (with its own sub-calibrations)
 */
dark: CalibrationSetInfo | null, 
/**
 * Linked Bias calibration set
 */
bias: CalibrationSetInfo | null, 
/**
 * Warnings for this subgroup
 */
warnings: Array<string>, };

export type ExportGroup = { 
/**
 * Unique group key for identification (e.g., "Ha_Mono")
 */
groupKey: string, 
/**
 * Filter name (None for unfiltered/OSC luminance)
 */
filter: string | null, 
/**
 * Camera type (OSC or Mono)
 */
cameraType: CameraType, 
/**
 * Display name for UI (e.g., "Ha (Mono)", "Luminance (OSC)")
 */
displayName: string, 
/**
 * Calibration subgroups - frames grouped by their linked calibration sets
 */
subgroups: Array<CalibrationSubgroup>, 
/**
 * Total light frame count across all subgroups
 */
totalFrames: number, 
/**
 * Total exposure time across all subgroups (seconds)
 */
totalExposure: number, 
/**
 * Warnings specific to this group
 */
warnings: Array<string>, };

export type CalibrationSummary = { 
/**
 * Total flat frames available
 */
flatCount: number, 
/**
 * Total dark frames available
 */
darkCount: number, 
/**
 * Total bias frames available
 */
biasCount: number, 
/**
 * Total dark flat frames available
 */
darkFlatCount: number, 
/**
 * Whether all lights have matched flats
 */
flatsComplete: boolean, 
/**
 * Whether all lights have matched darks
 */
darksComplete: boolean, 
/**
 * Whether all lights have matched bias
 */
biasComplete: boolean, 
/**
 * Warnings about calibration matching
 */
warnings: Array<string>, };

export type MasterCreationPlan = { 
/**
 * Ordered list of masters to create (respects dependencies)
 */
masters: Array<MasterInfo>, 
/**
 * Map of set_id → master file path for reference
 */
masterPaths: { [key in number]?: string }, };

export type MasterInfo = { 
/**
 * Calibration set ID
 */
setId: number, 
/**
 * Master type (Bias, Dark, DarkFlat, Flat)
 */
masterType: string, 
/**
 * Output filename (e.g., "master_bias_3.fit")
 */
outputName: string, 
/**
 * Source frames for this master
 */
sourceFrames: Array<ExportFrame>, 
/**
 * Dependencies - set IDs of masters needed before this one
 */
dependsOn: Array<number>, 
/**
 * Calibration master to apply: Bias set ID
 */
applyBias: number | null, 
/**
 * Calibration master to apply: Dark set ID (for lights and darks that need dark calibration)
 * For flats, this is only set if the dark exposure time matches the flat exposure (±30%)
 */
applyDark: number | null, 
/**
 * Calibration master to apply: DarkFlat set ID (for flats - short exposure dark matching flat exposure)
 */
applyDarkflat: number | null, 
/**
 * Source frame exposure time (for exposure-time matching in flat calibration)
 */
sourceExptime: number | null, };

export type ExportData = { 
/**
 * Frame set ID
 */
frameSetId: number, 
/**
 * Frame set name
 */
frameSetName: string, 
/**
 * Object name
 */
objectName: string | null, 
/**
 * Export groups (new structure with subgroups)
 */
groups: Array<ExportGroup>, 
/**
 * Master creation plan (ordered list of masters to create)
 */
masterPlan: MasterCreationPlan, 
/**
 * Filter groups with their calibrations (legacy - kept for compatibility)
 */
filters: Array<FilterExportGroup>, 
/**
 * Overall calibration summary
 */
calibrationSummary: CalibrationSummary, 
/**
 * Total light frame count
 */
totalLightFrames: number, 
/**
 * Total exposure time in seconds
 */
totalExposureSeconds: number, };

export type FilterExportGroup = { 
/**
 * Filter name (None for unfiltered/OSC)
 */
filter: string | null, 
/**
 * Light frames for this filter
 */
lightFrames: Array<ExportFrame>, 
/**
 * Matched flat calibration sets
 */
flatSets: Array<ExportCalibrationSet>, 
/**
 * Matched dark calibration sets
 */
darkSets: Array<ExportCalibrationSet>, 
/**
 * Matched bias calibration sets
 */
biasSets: Array<ExportCalibrationSet>, };

export type ExportCalibrationSet = { 
/**
 * Calibration set ID
 */
setId: number, 
/**
 * Image type (FLAT, DARK, BIAS, DARKFLAT)
 */
imagetyp: string, 
/**
 * Frames in this calibration set
 */
frames: Array<ExportFrame>, 
/**
 * Sub-calibrations (e.g., Flat -> Dark, Dark -> Bias)
 */
subCalibrations: Array<ExportCalibrationSet>, 
/**
 * Match quality score (0.0 - 1.0)
 */
matchScore: number | null, 
/**
 * Warnings about this calibration match
 */
warnings: Array<string>, };

export type CalibrationRoute = { 
/**
 * Export groups and their calibration trees
 */
groups: Array<CalibrationRouteGroup>, 
/**
 * Overall summary
 */
summary: CalibrationRouteSummary, };

export type CalibrationRouteGroup = { 
/**
 * Group display name (e.g., "Ha (Mono)")
 */
name: string, 
/**
 * Number of light frames
 */
lightCount: number, 
/**
 * Total exposure time (seconds)
 */
totalExposure: number, 
/**
 * Number of subgroups
 */
subgroupCount: number, 
/**
 * Calibration tree nodes
 */
calibrationTree: Array<CalibrationTreeNode>, };

export type CalibrationTreeNode = { 
/**
 * Node type: "Light", "Flat", "Dark", "Bias", "DarkFlat"
 */
nodeType: string, 
/**
 * Display label (e.g., "Flat Set 5 (30 frames)")
 */
label: string, 
/**
 * Calibration set ID (None for Light nodes)
 */
setId: number | null, 
/**
 * Frame count
 */
count: number, 
/**
 * Child nodes (sub-calibrations)
 */
children: Array<CalibrationTreeNode>, 
/**
 * Warnings for this node
 */
warnings: Array<string>, 
/**
 * Whether this node is missing/incomplete
 */
isMissing: boolean, 
/**
 * Whether this set is shared with other subgroups/groups
 */
isShared: boolean, };

export type CalibrationRouteSummary = { 
/**
 * Total export groups
 */
groupCount: number, 
/**
 * Total light frames
 */
totalLights: number, 
/**
 * Total exposure time (seconds)
 */
totalExposure: number, 
/**
 * Number of unique calibration sets
 */
uniqueCalibrationSets: number, 
/**
 * Number of masters to create
 */
mastersToCreate: number, 
/**
 * Calibration completeness flags
 */
flatsComplete: boolean, darksComplete: boolean, biasComplete: boolean, 
/**
 * Overall warnings
 */
warnings: Array<string>, };

export type ExportResult = { 
/**
 * Whether the export was successful
 */
success: boolean, 
/**
 * Output directory path
 */
outputDir: string, 
/**
 * Number of files copied/linked
 */
filesOrganized: number, 
/**
 * Generated script paths
 */
scriptsGenerated: Array<string>, 
/**
 * Any warnings during export
 */
warnings: Array<string>, 
/**
 * Error message if failed
 */
error: string | null, };

export type ExportSummary = { 
/**
 * Frame set ID
 */
frameSetId: number, 
/**
 * Frame set name
 */
frameSetName: string, 
/**
 * Object name (target)
 */
objectName: string | null, 
/**
 * Unique cameras used
 */
cameras: Array<string>, 
/**
 * Unique telescopes used
 */
telescopes: Array<string>, 
/**
 * Date range of sessions (start, end)
 */
dateRange: [string, string] | null, 
/**
 * Filter groups with full breakdown
 */
filterGroups: Array<FilterGroupSummary>, 
/**
 * Folder structure preview
 */
folderPreview: FolderPreview, 
/**
 * Detailed warnings
 */
warnings: Array<DetailedWarning>, 
/**
 * Total file count
 */
totalFiles: number, 
/**
 * Estimated total size in bytes
 */
estimatedSizeBytes: number, };

export type FilterGroupSummary = { 
/**
 * Filter name (None for unfiltered/luminance)
 */
filter: string | null, 
/**
 * Camera type (OSC or Mono)
 */
cameraType: CameraType, 
/**
 * Camera/instrument name
 */
camera: string | null, 
/**
 * Telescope name
 */
telescope: string | null, 
/**
 * Gain setting
 */
gain: number | null, 
/**
 * Offset setting
 */
offset: number | null, 
/**
 * Binning mode (e.g., "1x1")
 */
binning: string | null, 
/**
 * Average CCD temperature
 */
avgTemp: number | null, 
/**
 * Exposure breakdown by exposure time
 */
exposureGroups: Array<ExposureGroup>, 
/**
 * Total exposure time in seconds
 */
totalExposure: number, 
/**
 * Total frame count
 */
frameCount: number, 
/**
 * Linked flat calibration info
 */
flatInfo: CalibrationDetail | null, 
/**
 * Linked dark calibration info
 */
darkInfo: CalibrationDetail | null, 
/**
 * Linked bias calibration info
 */
biasInfo: CalibrationDetail | null, 
/**
 * All frames in this group (for expandable list)
 */
frames: Array<FrameDetail>, };

export type ExposureGroup = { 
/**
 * Exposure time in seconds
 */
exptime: number, 
/**
 * Number of frames with this exposure
 */
count: number, 
/**
 * Total exposure time for this group (exptime * count)
 */
totalSeconds: number, };

export type CalibrationDetail = { 
/**
 * Calibration set ID
 */
setId: number, 
/**
 * Calibration type (Flat, Dark, Bias, DarkFlat)
 */
calibrationType: string, 
/**
 * Number of calibration frames
 */
frameCount: number, 
/**
 * Average exposure time (for darks/flats)
 */
avgExptime: number | null, 
/**
 * Average CCD temperature
 */
avgTemp: number | null, 
/**
 * Match quality score (0.0 - 1.0)
 */
matchScore: number, 
/**
 * Date range of calibration frames (start, end)
 */
dateRange: [string, string] | null, 
/**
 * Specific warnings for this calibration
 */
warnings: Array<string>, 
/**
 * Sub-calibrations (e.g., Flat -> Dark -> Bias chain)
 */
subCalibrations: Array<CalibrationDetail>, };

export type FrameDetail = { 
/**
 * Frame ID
 */
frameId: number, 
/**
 * Filename
 */
filename: string, 
/**
 * Full file path
 */
filePath: string, 
/**
 * Date observed
 */
dateObs: string | null, 
/**
 * Exposure time in seconds
 */
exptime: number | null, 
/**
 * CCD temperature
 */
temp: number | null, 
/**
 * Gain setting
 */
gain: number | null, 
/**
 * Offset setting
 */
offset: number | null, 
/**
 * Calibration chain description (e.g., "Flat #12 → Dark #8 → Bias #3")
 */
calibrationChain: string, 
/**
 * File size in bytes (if known)
 */
fileSize: number | null, };

export type FolderPreview = { 
/**
 * Root folder name
 */
rootName: string, 
/**
 * Folder structure tree
 */
structure: Array<FolderNode>, 
/**
 * Total file count
 */
totalFiles: number, 
/**
 * Estimated total size (human readable)
 */
estimatedSize: string, };

export type FolderNode = { 
/**
 * Node name (folder or file name)
 */
name: string, 
/**
 * Node type
 */
nodeType: FolderNodeType, 
/**
 * File count (for folders)
 */
fileCount: number | null, 
/**
 * Description (e.g., "← 50 darks, 100 bias")
 */
description: string | null, 
/**
 * Child nodes
 */
children: Array<FolderNode>, };

export type FolderNodeType = "folder" | "file" | "ellipsis";

export type DetailedWarning = { 
/**
 * Warning type
 */
warningType: WarningType, 
/**
 * Warning severity
 */
severity: WarningSeverity, 
/**
 * Short title
 */
title: string, 
/**
 * Detailed description
 */
description: string, 
/**
 * Related calibration set ID (if applicable)
 */
setId: number | null, 
/**
 * Related filter (if applicable)
 */
filter: string | null, 
/**
 * Actual value (for mismatches)
 */
actualValue: string | null, 
/**
 * Expected value (for mismatches)
 */
expectedValue: string | null, 
/**
 * Delta/difference (for numeric comparisons)
 */
delta: string | null, 
/**
 * Recommendation text
 */
recommendation: string | null, };

export type WarningType = "temperature_mismatch" | "calibration_age" | "missing_calibration" | "gain_offset_mismatch" | "binning_mismatch" | "exposure_mismatch" | "general";

export type WarningSeverity = "info" | "warning" | "error";

export type ExportProgressEvent = { frameSetId: number, 
/**
 * Files copied so far
 */
current: number, 
/**
 * Total files to copy
 */
total: number, percent: number, currentFile: string | null, 
/**
 * "collecting" | "copying" | "complete"
 */
phase: string, };

export type ExportCompleteEvent = { frameSetId: number, success: boolean, filesOrganized: number, warnings: Array<string>, error: string | null, outputDir: string, };

export type ExportMode = "calibratedLights" | "rawWithMasters" | "rawWithCalibrationSets";

export type WbppExportConfig = { 
/**
 * Keyword nesting order (outermost first).
 * Default: ["CAMERA", "BIAS", "DARKS", "FLAT"]
 */
keywordOrder: Array<string>, 
/**
 * What the export writes for lights + calibration (spec §12.2). Defaults
 * to [`ExportMode::RawWithCalibrationSets`] (today's behavior) so a config
 * persisted before this field existed loads unchanged.
 */
exportMode: ExportMode, };

export type WbppSetupInstructions = { 
/**
 * Ordered list of keywords to configure in WBPP
 */
keywords: Array<WbppKeywordInstruction>, 
/**
 * Example folder structure matching the current config
 */
exampleStructure: string, };

export type WbppKeywordInstruction = { 
/**
 * The WBPP grouping keyword name
 */
keyword: string, 
/**
 * Whether "Pre" should be checked for this keyword
 */
preChecked: boolean, 
/**
 * Description of what this keyword controls
 */
description: string, };

