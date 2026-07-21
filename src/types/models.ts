// AUTO-GENERATED from Rust by athenaeum-core/src/ts_export.rs — do not edit.
// Regenerate: TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract

export type FileFormat = "FITS" | "XISF";

export type ImageType = "Light" | "Dark" | "Flat" | "Bias" | "DarkFlat" | "MasterLight" | "MasterDark" | "MasterFlat" | "MasterBias" | "MasterDarkFlat";

export type File = { id: number | null, path: string, filename: string, size: number, modified_at: string, format: FileFormat, created_at: string, metadata_hash: string | null, content_hash: string | null, archived_in_operation: number | null, archive_zip_path: string | null, archive_path_in_zip: string | null, uuid: string | null, updated_at: string | null, };

export type Frame = { id: number | null, file_id: number, object: string | null, date_obs: string | null, telescop: string | null, instrume: string | null, exptime: number | null, filter: string | null, imagetyp: ImageType | null, is_master: boolean, gain: number | null, offset: number | null, binning: string | null, xbinning: number | null, ybinning: number | null, ccd_temp: number | null, set_temp: number | null, focallen: number | null, xpixsz: number | null, ypixsz: number | null, naxis1: number | null, naxis2: number | null, ra: number | null, dec: number | null, sitelat: number | null, lat_obs: number | null, sitelong: number | null, long_obs: number | null, objctra: string | null, objctdec: string | null, override_: boolean, swcreate: string | null, 
/**
 * Bayer pattern for OSC cameras (e.g., "RGGB", "BGGR")
 * None indicates a mono camera
 */
bayerpat: string | null, 
/**
 * Image position angle in degrees (North through East)
 * Extracted from CROTA2 or computed from CD matrix
 */
rotation: number | null, uuid: string | null, updated_at: string | null, };

export type ScanRoot = { id: number | null, path: string, enabled: boolean, find_duplicates: boolean, unique_camera: boolean, last_scan: string | null, last_scan_errors: Array<string> | null, monitor_enabled: boolean, 
/**
 * 'normal' | 'calibration_library' | 'sync_incoming' | 'collaboration'
 */
kind: string, };

export type DuplicateFile = { fileId: number, path: string, filename: string, modifiedAt: string, 
/**
 * Longest-prefix match against `scan_roots.path`. `None` when the file
 * isn't inside any registered scan root (shouldn't happen for scanned
 * libraries, but we don't want to drop the row).
 */
scanRootId: number | null, scanRootPath: string | null, 
/**
 * IMAGETYP from the frame row (raw string — e.g. "LIGHT", "FLAT").
 * `None` if no frame row or the field was missing.
 */
imagetyp: string | null, 
/**
 * DATE-OBS from the frame row (RFC3339). `None` if not set.
 */
dateObs: string | null, };

export type DuplicateGroup = { id: number | null, size: number, content_hash: string, file_count: number, file_paths: Array<string>, file_ids: Array<number>, files: Array<DuplicateFile>, };

export type BulkMoveResult = { moved: number, 
/**
 * Per-file failures — (file_id, error message). Empty on full success.
 */
failed: Array<[number, string]>, };

export type BulkMoveProgressEvent = { current: number, total: number, percent: number, currentFile: string | null, };

export type BlackHoleEntry = { id: number | null, file_id: number, filename: string, original_path: string, from_where: string, moved_at: string, file_size: number, };

export type FolderSimilarity = { folder_a: string, folder_b: string, similarity_percent: number, shared_files: number, shared_size: number, unique_a: number, unique_b: number, shared_file_ids: Array<number>, };

export type ScanProgressEvent = { current: number, total: number, current_file: string | null, percent: number, root_id: number, phase: string, };

export type CalibratedDuplicate = { keptPath: string, duplicatePath: string, };

export type ScanCompleteEvent = { root_id: number, files_found: number, files_processed: number, files_skipped: number, errors: Array<string>, lights_count: number, darks_count: number, flats_count: number, bias_count: number, darkflats_count: number, calibration_sets_created: number, cancelled: boolean, 
/**
 * Calibrated-LIGHT artifacts (design §4.3) whose header identity matches a
 * tracked output whose file ALSO still exists — a duplicate copy the user
 * should be told about. Never registered as frames; surfaced only here.
 */
calibrated_duplicates: Array<CalibratedDuplicate>, };

export type FileWithFrame = { file: File, frame: Frame | null, };

export type FramesSet = { id: number | null, name: string | null, is_custom: boolean, is_archived: boolean, date_obs_start: string | null, date_obs_end: string | null, objctra: string | null, objctdec: string | null, total_exp_time: number | null, flat_pattern: string | null, avg_rotation: number | null, min_rotation: number | null, max_rotation: number | null, archived_at: string | null, archive_operation_id: number | null, uuid: string | null, updated_at: string | null, };

export type Setting = { key: string, value: string, updated_at: string | null, };

export type ImagingNight = { id: number | null, frames_set_id: number, start_time: string, end_time: string, created_at: string | null, };

export type Session = { id: number | null, imaging_night_id: number, instrume: string, frame_count: number, total_exp_time: number | null, created_at: string | null, uuid: string | null, updated_at: string | null, };

export type SessionWithFrames = { session: Session, frames: Array<FileWithFrame>, };

export type ImagingNightWithSessions = { imaging_night: ImagingNight, sessions: Array<SessionWithFrames>, };

export type FrameSetDetail = { frames_set: FramesSet, nights: Array<ImagingNightWithSessions>, };

export type CameraStats = { instrume: string, frame_count: number, total_hours: number, first_use: string | null, last_use: string | null, };

export type CalibrationSetDetail = { id: number | null, imagetyp: ImageType, exptime: number | null, ccd_temp: number, temp_min: number, temp_max: number, gain: number | null, offset: number | null, binning: string | null, instrume: string | null, filter: string | null, date_start: string, date_end: string, date_display: string, frame_count: number, is_master: boolean, naxis1: number | null, naxis2: number | null, bayerpat: string | null, swcreate: string | null, xpixsz: number | null, format: string | null, focallen: number | null, uuid: string | null, updated_at: string | null, 
/**
 * Set when this raw set has been superseded by a built master.
 */
superseded_by_set_id: number | null, };

export type RelinkResult = { files_matched: number, files_new: number, files_orphaned: number, orphaned_file_ids: Array<number>, };

export type OrphanedFile = { id: number, path: string, filename: string, size: number, modified_at: string, has_frame: boolean, object: string | null, date_obs: string | null, };

export type ImagingLocation = { id: number, ra: number, dec: number, objectName: string | null, frameCount: number, totalExposure: number, filters: Array<string>, dateRange: [string, string], frameSetId: number | null, fovWidth: number | null, fovHeight: number | null, locationType: string, cameras: string | null, focalLengths: string | null, isCustom: boolean, rotation: number | null, };

export type CalibrationLink = { id: number | null, source_id: number, source_type: string, calibration_set_id: number, calibration_type: string, matched_at: string, match_score: number | null, date_warning: boolean, temp_warning: boolean, is_manual_override: boolean, };

export type FrameCalibrationStatus = { frame_id: number, has_flats: boolean, has_darks: boolean, has_bias: boolean, has_darkflats: boolean, flats_warning: boolean, darks_warning: boolean, bias_warning: boolean, flat_set_id: number | null, dark_set_id: number | null, bias_set_id: number | null, darkflat_set_id: number | null, };

export type CalibrationHierarchy = { light_frame_id: number, flat_sets: Array<CalibrationSetWithLinks>, dark_sets: Array<CalibrationSetWithLinks>, missing_calibration: Array<string>, warnings: Array<CalibrationWarning>, };

export type CalibrationSetWithLinks = { set: CalibrationSetDetail, sub_calibration: Array<CalibrationLink>, };

export type CalibrationWarning = { warning_type: string, message: string, calibration_type: string, set_id: number, };

export type CalibrationStats = { total_frames: number, frames_with_flats: number, frames_with_darks: number, frames_with_bias: number, frames_complete: number, frames_partial: number, frames_none: number, total_warnings: number, };

export type CalibrationGroup = { flat_set_id: number | null, dark_set_id: number | null, bias_set_id: number | null, flat_set_detail: CalibrationSetDetail | null, dark_set_detail: CalibrationSetDetail | null, bias_set_detail: CalibrationSetDetail | null, frame_count: number, frame_ids: Array<number>, has_warnings: boolean, flat_warnings: Array<CalibrationWarning>, dark_warnings: Array<CalibrationWarning>, bias_warnings: Array<CalibrationWarning>, };

export type FrameSetCalibrationGroups = { groups: Array<CalibrationGroup>, uncalibrated_frame_count: number, uncalibrated_frame_ids: Array<number>, total_frames: number, };

export type CalibrationTolerance = { flat_date_warning_days: number, dark_date_warning_days: number, };

export type ProcessingProgress = { total_frames: number, processed_frames: number, current_frame_id: number | null, percent_complete: number, };

export type ProcessingStats = { total_frames: number, frames_with_full_calibration: number, frames_with_partial_calibration: number, frames_with_no_calibration: number, total_flat_sets_linked: number, total_dark_sets_linked: number, total_warnings: number, date_warnings: number, temp_warnings: number, missing_flats: number, missing_darks: number, missing_bias: number, frames_with_flats_only: number, frames_with_darks_only: number, };

export type FlatTiming = "Before" | "After" | "During";

export type FlatGroup = { 
/**
 * IDs of frames in this group
 */
frame_ids: Array<number>, 
/**
 * Timestamp of first frame in group
 */
start_time: string, 
/**
 * Timestamp of last frame in group
 */
end_time: string, 
/**
 * Average CCD temperature across all frames (if available)
 */
avg_temp: number | null, 
/**
 * Coldest CCD temperature observed in the group (if any frame has temp).
 * Persisted as `calibration_set.temp_min` so the matcher / UI can see the
 * real range, not the synthetic average.
 */
temp_min: number | null, 
/**
 * Warmest CCD temperature observed in the group.
 */
temp_max: number | null, 
/**
 * Number of frames in group
 */
frame_count: number, 
/**
 * Filter used (if any)
 */
filter: string | null, 
/**
 * Camera/instrument name (None if unknown)
 */
instrume: string | null, 
/**
 * Binning pattern (e.g., "1x1", "2x2") (None if unknown)
 */
binning: string | null, 
/**
 * Gain setting
 */
gain: number | null, 
/**
 * Offset setting
 */
offset: number | null, 
/**
 * Exposure time (seconds)
 */
exptime: number | null, 
/**
 * Focal length
 */
focal_length: number | null, };

export type FlatGroupMatch = { 
/**
 * The flat group
 */
group: FlatGroup, 
/**
 * Match score (0.0-1.0), higher is better
 */
match_score: number, 
/**
 * Days between light frame and flat group
 */
age_days: number, 
/**
 * Temperature difference (if available)
 */
temp_diff: number | null, 
/**
 * Relative timing to session
 */
timing: FlatTiming, };

export type CalibrationHierarchyView = { date_groups: Array<CalibrationDateGroup>, total_frames: number, calibrated_frames: number, uncalibrated_frames: number, };

export type CalibrationDateGroup = { date: string, date_display: string, camera_groups: Array<CalibrationCameraGroup>, frame_count: number, has_warnings: boolean, };

export type CalibrationCameraGroup = { instrume: string, filter_groups: Array<CalibrationFilterGroup>, frame_count: number, has_warnings: boolean, };

export type SubCalibrationDetail = { calibration_type: string, set: CalibrationSetDetail, date_warning: boolean, temp_warning: boolean, };

export type CalibrationSetConsumer = { frameSetId: number, name: string | null, dateObsStart: string | null, };

export type CalibrationSetWithFrameCount = { set: CalibrationSetDetail, frame_count: number, frame_ids: Array<number>, warnings: Array<CalibrationWarning>, sub_calibration: Array<SubCalibrationDetail>, };

export type CalibrationSetWithScore = { set: CalibrationSetDetail, match_score: number, match_details: MatchDetails, };

export type MatchDetails = { instrume_match: boolean, binning_match: boolean, gain_match: boolean, offset_match: boolean, exptime_match: boolean, filter_match: boolean, temp_diff: number | null, date_diff_days: number, };

export type LightFrameParameters = { instrume: string | null, binning: string | null, gain: number | null, offset: number | null, filter: string | null, avg_ccd_temp: number | null, avg_exptime: number | null, exptime_range: [number, number] | null, frame_count: number, date_range: [string, string] | null, current_flat_set_id: number | null, current_dark_set_id: number | null, current_bias_set_id: number | null, };

export type CalibrationSetParameters = { set_id: number, imagetyp: string, instrume: string | null, binning: string | null, gain: number | null, offset: number | null, exptime: number | null, filter: string | null, ccd_temp: number | null, date_start: string | null, date_end: string | null, frame_count: number, current_dark_set_id: number | null, current_darkflat_set_id: number | null, current_bias_set_id: number | null, };

export type CalibrationFilterGroup = { filter: string | null, filter_display: string, exptime: number | null, light_frames: Array<LightFrameWithCalibration>, flat_sets: Array<CalibrationSetWithFrameCount>, dark_sets: Array<CalibrationSetWithFrameCount>, bias_sets: Array<CalibrationSetWithFrameCount>, has_warnings: boolean, frame_count: number, };

export type LightFrameWithCalibration = { frame_id: number, file_id: number, filename: string, file_path: string, date_obs: string | null, exptime: number | null, telescop: string | null, focallen: number | null, xpixsz: number | null, binning: string | null, ccd_temp: number | null, swcreate: string | null, gain: number | null, offset: number | null, rotation: number | null, objctra: string | null, objctdec: string | null, ra: number | null, dec: number | null, 
/**
 * True when a `plate_solves` row exists for this frame, i.e. the WCS
 * was computed by Athenaeum's plate solver (vs. read from the FITS
 * header). Drives the Original/Athenaeum WCS badge.
 */
plate_solved: boolean, calibration_status: FrameCalibrationStatus, };

export type CalendarFrameSetSummary = { id: number, name: string | null, objectName: string | null, frameCount: number, totalExposureSeconds: number, ra: number | null, dec: number | null, filters: Array<string>, };

export type CalendarUnorganizedGroup = { id: string, objectName: string | null, frameCount: number, totalExposureSeconds: number, ra: number | null, dec: number | null, filters: Array<string>, frameIds: Array<number>, };

export type CalendarDayEvent = { date: string, frameSets: Array<CalendarFrameSetSummary>, unorganizedGroups: Array<CalendarUnorganizedGroup>, totalFrameCount: number, totalExposureSeconds: number, };

export type CalendarMonthData = { year: number, month: number, days: Array<CalendarDayEvent>, totalFrameCount: number, totalExposureSeconds: number, };

export type ExcludedFrameEntry = { file_id: number, path: string, filename: string, reason: string, excluded_at: string, };

export type ExcludedFrameRow = { file: File, frame: Frame, hasDuplicate: boolean, reason: string, excludedAt: string, };

export type FrameAnalysis = { id: number | null, frame_id: number, file_id: number, stars_detected: number, median_fwhm: number, median_eccentricity: number, median_snr: number, median_hfr: number, frame_snr: number, snr_weight: number, psf_signal: number, background: number, noise: number, detection_threshold: number, width: number, height: number, source_channels: number, trail_r_squared: number, possibly_trailed: boolean, median_beta: number | null, quality_score: number | null, config_hash: string | null, analyzed_at: string, };

export type CalibrationMetadataEdits = { ccd_temp?: number | null, gain?: number | null, offset?: number | null, binning?: string | null, exptime?: number | null, };

export type StarMetric = { id: number | null, frame_analysis_id: number, x: number, y: number, peak: number, flux: number, fwhm: number, fwhm_x: number, fwhm_y: number, eccentricity: number, snr: number, hfr: number, theta: number, beta: number | null, fit_method: string, fit_residual: number, };

export type StarMetricsResponse = { stars: Array<StarMetric>, metrics: FrameAnalysis, image_width: number, image_height: number, 
/**
 * Whether the displayed image was vertically flipped during processing.
 * When true, star Y coordinates must be flipped: y_display = image_height - 1 - y
 * and theta must be negated. Mirrors rustafits annotate.rs compute_annotations().
 */
flip_vertical: boolean, };

export type CalibrationSetOriginals = { set_id: number, ccd_temp: number | null, temp_min: number | null, temp_max: number | null, gain: number | null, offset: number | null, binning: string | null, exptime: number | null, saved_at: string, };

export type MissingMetadataRow = { file: File, frame: Frame, hasDuplicate: boolean, };

export type FrameMetadataEdits = { instrume?: string | null, 
/**
 * RFC 3339 / ISO 8601 datetime string (frontend converts from user input).
 */
dateObs?: string | null, 
/**
 * Raw IMAGETYP value — "LIGHT", "DARK", "FLAT", "BIAS", "DARKFLAT".
 * For master variants, pair with `is_master = Some(true)` and a base
 * type (e.g. `imagetyp = "DARK"`, `is_master = true` → master dark).
 */
imagetyp?: string | null, isMaster?: boolean | null, 
/**
 * Target name (FITS OBJECT). Common cleanup case for misnamed targets.
 */
object?: string | null, 
/**
 * Filter name (FITS FILTER). Common cleanup case for ASIAir mis-records.
 */
filter?: string | null, 
/**
 * Telescope name (FITS TELESCOP).
 */
telescop?: string | null, 
/**
 * Focal length in mm (FITS FOCALLEN).
 */
focallen?: number | null, 
/**
 * Pixel size in µm (FITS XPIXSZ). Required alongside FOCALLEN for the
 * plate-solve scale hint; user-editable on any frame for sparse headers
 * (some surveys ship without XPIXSZ — e.g. SkyMapper).
 */
xpixsz?: number | null, 
/**
 * Sensor gain (FITS GAIN). Typically 0-1000.
 */
gain?: number | null, 
/**
 * Sensor offset (FITS OFFSET). Typically 0+.
 */
offset?: number | null, 
/**
 * Binning string (e.g. "1x1", "2x2"). When set, xbinning/ybinning are
 * also updated to match the parsed components.
 */
binning?: string | null, 
/**
 * Exposure time in seconds (FITS EXPTIME). Must be > 0.
 */
exptime?: number | null, 
/**
 * CCD temperature in °C (FITS CCD-TEMP). Typically -50..50.
 */
ccdTemp?: number | null, 
/**
 * Right Ascension in decimal degrees [0, 360). Only set by the metadata
 * pane's RA/DEC revert path — there is no text input for these. The
 * "manual update" path for coordinates is plate-solving. When set,
 * `bulk_update_frame_metadata` also writes the parallel sexagesimal
 * `objctra` so derived consumers stay consistent with what the scanner
 * and plate solver produce.
 */
ra?: number | null, 
/**
 * Declination in decimal degrees [-90, 90]. See `ra`.
 */
dec?: number | null, 
/**
 * Image position angle in degrees (north through east). Plate solving
 * derives this from the CD matrix. The metadata pane lets the user
 * override or revert it just like any other numeric field. Range is
 * `[-360, 360]` to accept either sign convention.
 */
rotation?: number | null, };

export type SkipReason = "no_coords" | "outside_threshold" | "already_in_set";

export type CandidateFrame = { frame_id: number, file_id: number, filename: string, ra: number | null, dec: number | null, date_obs: string | null, filter: string | null, instrume: string | null, };

export type SkippedFrame = { frame_id: number, filename: string, reason: SkipReason, };

export type FindNewFramesResult = { candidates: Array<CandidateFrame>, scan_was_awaited: boolean, };

export type MergeReport = { frames_set_id: number, added_count: number, skipped_count: number, frames_added: Array<CandidateFrame>, frames_skipped: Array<SkippedFrame>, threshold_arcmin: number, source: string, };

export type MergeLogEntry = { id: number, frames_set_id: number, occurred_at: string, source: string, threshold_arcmin: number, frames_added: Array<CandidateFrame>, frames_skipped: Array<SkippedFrame>, added_count: number, skipped_count: number, };

export type FlatContourOpts = { 
/**
 * Resampling factor in percent. Final width/height are
 * `original * resolution_pct / 100`. PI range: 5..=100.
 */
resolutionPct: number, 
/**
 * Gaussian blur sigma in pixels (post-resample). PI range: 0..=10.
 */
sigmaPx: number, 
/**
 * Number of discrete contour bands. PI range: 4..=20 (default 15).
 */
contours: number, 
/**
 * Boundary-emphasis strength as a percentage. PI range: 0..=200,
 * default 50. Higher values darken the band boundaries more.
 */
gradientPct: number, };

export type FrameSetReference = { framesSetId: number, referenceFrameId: number, 
/**
 * ISO-8601 timestamp stored as TEXT in SQLite.
 */
setAt: string, };

export type RegistrationRecord = { id: number | null, framesSetId: number, frameId: number, referenceFrameId: number, isReference: boolean, crpix1: number | null, crpix2: number | null, crval1: number | null, crval2: number | null, cd11: number | null, cd12: number | null, cd21: number | null, cd22: number | null, affineA1: number | null, affineB1: number | null, affineC1: number | null, affineA2: number | null, affineB2: number | null, affineC2: number | null, matchedStars: number, rmsResidualPx: number, 
/**
 * RMS in arcsec: `rms_residual_px * pixel_scale_arcsec`. `None` when the
 * reference pixel scale is unavailable (rare).
 */
rmsResidualArcsec: number | null, 
/**
 * `"aligned"` | `"aligned_flipped"` | `"reference"` | `"failed"`.
 * `"aligned_flipped"` is an aligned variant (not a distinct outcome): the
 * fitted transform includes a reflection (meridian-flipped sub).
 */
status: string, 
/**
 * Error message for failed rows.
 */
error: string | null, computeTimeMs: number, registeredAt: string, };

export type StackingPrepProgressEvent = { frameId: number, current: number, total: number, status: string, matchedStars: number | null, rmsPx: number | null, error: string | null, filename: string | null, };

export type StackingPrepCompleteEvent = { referenceFrameId: number, aligned: number, failed: number, total: number, };

export type LoggingConfig = { level: string, modules: { [key in string]?: string }, };

export type LoggingConfigResponse = { config: LoggingConfig, envOverrideActive: boolean, };

export type ComputeJobKind = "analysis" | "master_build" | "light_calibration";

export type ComputeJobState = "queued" | "running";

export type ComputeQueueEntry = { jobId: number, kind: ComputeJobKind, label: string, state: ComputeJobState, queuedAt: string, };

export type Combination = "average" | "median";

export type Rejection = { "method": "none" } | { "method": "percentile_clip", low: number, high: number, } | { "method": "sigma_clip", sigma_low: number, sigma_high: number, } | { "method": "winsorized_sigma", sigma_low: number, sigma_high: number, } | { "method": "linear_fit_clip", sigma_low: number, sigma_high: number, };

export type IntegrationRecipe = { combination: Combination, rejection: Rejection, };

export type MasterRecipe = { 
/**
 * None => Auto (per-type/per-N rule from spec §2/§9). Field name kept
 * (`combine`) across the CombineMethod → IntegrationRecipe migration.
 */
combine: IntegrationRecipe | null, 
/**
 * Constant-ADU fallback for flat pre-calibration when no darkflat/dark/bias master is linked.
 */
syntheticBias: number | null, 
/**
 * Task 14: when true, chain `archive_originals` on the SOURCE
 * calibration set right after a successful `New`-target build (i.e.
 * after `register_master` supersedes it) — non-fatally: an archive
 * failure is logged but never turns a successful master build into a
 * reported failure. Has no effect on `BuildTarget::Rebuild` (that path
 * never calls `register_master`, so there's no "just superseded" moment
 * to chain from — see `rebuild_master`'s own recipe construction).
 */
archiveAfter: boolean, };

export type MasterBuildPreview = { setId: number, imagetyp: string, frameCount: number, resolvedCombine: IntegrationRecipe, 
/**
 * Human description: "master darkflat #12" | "synthetic bias 500 ADU" | null.
 */
flatPrecal: string | null, 
/**
 * Absolute, collision-resolved.
 */
targetPath: string, warnings: Array<string>, 
/**
 * Raw (non-master) sub-cal sets encountered while resolving flat
 * pre-cal (see `select_flat_precal`'s DarkFlat -> Dark -> Bias
 * fallback chain) — lets the caller offer a "build its master first"
 * shortcut instead of just surfacing the warning string. Always empty
 * for non-flat previews.
 */
rawPrecalSets: Array<RawPrecalSetDto>, };

export type RawPrecalSetDto = { setId: number, calType: string, frameCount: number, };

export type MasterProvenanceInfo = { masterSetId: number, sourceSetId: number | null, recipeJson: string, memberCount: number, memberHash: string, createdAt: string, 
/**
 * Rebuild possible?
 */
sourceFramesOnDisk: boolean, 
/**
 * Any source file has archive markers.
 */
originalsArchived: boolean, 
/**
 * `archived_in_operation` read off whichever source member file has it
 * set — every member of a single archive-of-originals op shares the
 * same op id and zip by construction (`planner::build_calibration_set_plan`
 * produces exactly one `PlannedZip` per calibration set), so "any member"
 * and "the" op id are the same thing here. `None` when `originals_archived`
 * is false.
 */
archiveOperationId: number | null, 
/**
 * `archive_zip_path` read off the same member file as
 * `archive_operation_id`. `None` when `originals_archived` is false.
 */
archiveZipPath: string | null, 
/**
 * `true` when archive markers are present but `Path::exists(archive_zip_path)`
 * is false — the zip was deleted (or moved) outside the app. Always
 * `false` when `originals_archived` is false (nothing to be missing).
 * The UI uses this to hide the restore button and show an actionable
 * warning instead of letting the user kick off a restore doomed to fail.
 */
archiveZipMissing: boolean, };

export type BatchBuildReport = { startedSetIds: Array<number>, skipped: Array<BatchSkip>, };

export type BatchSkip = { setId: number, reason: string, };

export type LightFrameReadiness = { frameId: number, filename: string, 
/**
 * `db::light_calibrations::derive_status` mapped to the frontend's
 * verbatim strings: `"notCalibrated"` | `"calibrated"` | `"partial"` |
 * `"stale"`.
 */
status: string, 
/**
 * Dark-link classification: `"master"` | `"rawSet"` | `"missing"`.
 */
dark: string, 
/**
 * Flat-link classification: `"master"` | `"rawSet"` | `"missing"`.
 */
flat: string, 
/**
 * Bias-link classification: `"master"` | `"rawSet"` | `"missing"`.
 */
bias: string, 
/**
 * Distinct raw (non-master, non-superseded) calibration-set ids this
 * frame links to — the sets a preflight would have to build masters for.
 */
rawSetIds: Array<number>, };

export type LightCalReadiness = { frames: Array<LightFrameReadiness>, readyCount: number, rawSetCount: number, missingCount: number, 
/**
 * Distinct raw calibration-set ids across all frames that a preflight
 * must build masters for, in first-seen order. `raw_set_ids_to_build.len()`
 * is the number of master builds; `raw_set_count` is the number of
 * affected frames (a single raw set can serve many frames).
 */
rawSetIdsToBuild: Array<number>, };

export type ExportReadiness = { total: number, calibrated: number, stale: number, missing: number, };

export type LightCalDetails = { frameId: number, 
/**
 * CALSTAT recorded on the calibrated output (`"BDF"`, `"BF"`, `"BD"`, …).
 */
calstat: string, 
/**
 * Master-dark filename, or `None` when unlinked/unresolvable.
 */
darkMaster: string | null, 
/**
 * Master-flat filename, or `None`.
 */
flatMaster: string | null, 
/**
 * Master-bias filename, or `None`.
 */
biasMaster: string | null, flatNormApplied: boolean, 
/**
 * Normalization statistic wire string (`"centralThird"` |
 * `"pixinsightTrimmed"`); meaningful only when `flat_norm_applied`.
 */
flatNormMode: string, 
/**
 * Canonical JSON of the advanced parameters actually applied.
 */
calParams: string, engineVersion: number, 
/**
 * RFC3339 timestamp the tracking row was written.
 */
calibratedAt: string, outputPath: string, 
/**
 * `derive_status` against the caller's wanted preferences resolved to Stale
 * or Partial — the coverage view should flag this row for re-calibration.
 */
stale: boolean, };

export type LightCalScope = { onlyStale: boolean, };

export type FlatNormMode = "centralThird" | "pixinsightTrimmed";

export type LightCalParams = { 
/**
 * Per-tail discard fraction for the `pixinsightTrimmed` statistic
 * (default [`PI_TRIM_FRACTION`] = 0.05). Only meaningful when the flat is
 * normalized in `pixinsightTrimmed` mode; stamped as `ATH_CTRM` then.
 */
trimFraction: number, 
/**
 * DN added to the output AFTER the scale divide (`out += pedestal_dn /
 * OUTPUT_SCALE_DIVISOR`, default 0 = off), for consumers that clip
 * negatives. Stamped as `ATH_CPED` (the DN value) always; `CALSTAT`
 * unchanged — a pedestal is not a calibration step.
 */
pedestalDn: number, 
/**
 * What to do for a light with no dark master (default `subtractBias`).
 */
biasFallback: BiasFallback, };

export type BiasFallback = "subtractBias" | "skipFrame";

export type Direction = "sent" | "received";

export type OutboundState = "queued" | "announced" | "transferring" | "delivered" | "confirmed" | "failed" | "cancelled";

export type InboundState = "announced" | "fetching" | "ingesting" | "done" | "failed" | "cancelled";

export type HistoryRow = { frameUuid: string, filename: string, object: string | null, 
/**
 * Peer node id, hex-encoded.
 */
peerDevice: string, direction: Direction, bytes: number, startedAt: string, finishedAt: string | null, 
/**
 * Short outcome tag: `sent`, `ingested`, `duplicate`, `rejected`, `failed`
 * (or the composite `failed: rejected frame(s) <uuids>`), or `cancelled`.
 * Retention / manual-deletion audit rows reuse this column too:
 * `retention_deleted` (an automatic retention pass) and `deleted_manual`
 * (the web "Delete selected" action).
 */
outcome: string, 
/**
 * Stage-II collab provenance: the `project_id` this frame moved for, taken
 * from the manifest record's [`ProjectStamp`](crate::package::ProjectStamp).
 * `None` for a personal-sync transfer (no project dimension) — the Transfers
 * UI shows a project chip only for the collab rows.
 */
project: string | null, 
/**
 * The per-package batch key this frame-transfer belonged to (Task 14) — the
 * durable handle each side already knows: the SENDER stamps its package
 * dir's basename (== the last component of `sync_outbound.package_ref`), the
 * RECEIVER stamps the wire announce `package_id` (== `sync_inbound.package_id`).
 * [`list_transfer_files`](crate::api::sync::list_transfer_files) recovers the
 * same key from the outbound / inbound row to join a package's per-frame
 * verdicts back to its manifest. Legacy rows written before this column stay
 * `None`.
 */
packageId: string | null, 
/**
 * The human batch name this transfer belonged to (Transfers Status Model v2,
 * §D1) — mirrors [`OutboundRow::display_name`] / [`InboundRow::display_name`]
 * so the transfer log can show the named batch without re-joining the live
 * row. `None` for legacy rows and for callers that do not yet supply it
 * (later tasks fill it).
 */
batchName: string | null, };

export type TransferFileCounts = { 
/**
 * Total per-file rows for the batch (0 for a legacy pre-v2 batch with no
 * per-file rows — the summary's `file_count` is the manifest fallback).
 */
total: number, 
/**
 * Files that finished transferring and were not rejected.
 */
done: number, 
/**
 * Files that failed or were rejected by the peer.
 */
failed: number, };

export type OutboundSummary = { 
/**
 * Durable `sync_outbound` row id (stable across the package's lifecycle;
 * the sender's `sync-progress`/`sync-finished` events key on it too).
 */
id: number, 
/**
 * Short, human-readable package handle (basename of the package dir).
 */
packageShort: string, state: OutboundState, attempts: number, createdAt: string, 
/**
 * Destination peer node id (hex), shortened for display.
 */
peerShort: string, 
/**
 * The most recent failed-attempt reason (Task 9), or `None` when the package
 * has never failed / was cleared on a successful announce or confirm.
 */
lastError: string | null, 
/**
 * Wall-clock deadline (RFC3339 UTC) of the next scheduled retry (Task 2), or
 * `None` when the package is not currently waiting out a backoff window (it is
 * awaiting an ack or terminal). Drives the Transfers UI's live countdown.
 */
nextRetryAt: string | null, 
/**
 * Total payload bytes across the package's manifest (Task 14).
 */
byteSize: number, 
/**
 * Number of frames/files in the package's manifest (Task 14).
 */
fileCount: number, 
/**
 * The human batch name (`sync_outbound.display_name`, §D1), or `None` for a
 * legacy/never-named row. The Transfers UI shows this instead of the raw
 * package handle.
 */
displayName: string | null, 
/**
 * The destination peer's friendly device name, resolved from the cached
 * `SYNC_DEVICE_NAMES` hex→name map (no hub round-trip). `None` when the peer
 * is not in the cache.
 */
deviceName: string | null, 
/**
 * Backend-derived presentation state (§D5): `queued` | `preparing` |
 * `transferring` | `uploaded` | `waiting` | `confirmed` | `cancelled` |
 * `failed`. `waiting` (a live backoff window) WINS over the raw
 * [`state`](Self::state) when a retry is armed for the future; the raw
 * `state` field stays for compatibility. See
 * [`outbound_display_state`].
 */
displayState: string, 
/**
 * RFC3339 deadline of the armed retry when [`display_state`](Self::display_state)
 * is `waiting`, else `None` — the countdown target.
 */
stalledUntil: string | null, 
/**
 * Per-file rollup for the progress line ("N of M files").
 */
fileCounts: TransferFileCounts, 
/**
 * Whether a retry is armed (`next_retry_at` is set). The frontend reads this
 * instead of deriving "retrying" from `attempts`. Distinct from the
 * `waiting` display-state, which additionally requires the deadline to be in
 * the future.
 */
retrying: boolean, };

export type InboundSummary = { 
/**
 * Durable `sync_inbound` row id — the handle
 * [`list_transfer_files`](crate::api::sync::list_transfer_files) resolves the
 * received-detail read by.
 */
id: number, 
/**
 * The full wire package id (`sync_inbound.package_id`) — the exact key
 * `cancel_incoming_package` matches on (`WHERE package_id = ?1`). A row
 * surfaced only by the status poll (e.g. announced/fetching from a prior
 * session) has no other way for the caller to obtain the full id, since
 * [`package_short`](Self::package_short) is a truncated display string.
 */
packageId: string, 
/**
 * Short, human-readable package handle (leading chars of the wire package id).
 */
packageShort: string, 
/**
 * Sending peer node id (hex), shortened for display.
 */
peerShort: string, state: InboundState, frameCount: number, byteSize: number, 
/**
 * Cumulative bytes fetched so far (0 until the fetch stage reports progress).
 */
bytesDone: number, createdAt: string, 
/**
 * The human batch name (`sync_inbound.display_name`, §D1) carried in the v2
 * announce, or `None` for a v1/unnamed batch. Shown instead of the raw
 * package id.
 */
displayName: string | null, 
/**
 * The sending peer's friendly device name, resolved from the cached
 * `SYNC_DEVICE_NAMES` hex→name map (no hub round-trip). `None` when unknown.
 */
deviceName: string | null, 
/**
 * Backend-derived presentation state (§D5): `announced` | `fetching` |
 * `ingesting` | `done` | `cancelled` | `failed`. Currently mirrors the raw
 * [`state`](Self::state) (the receiver has no `waiting`/backoff concept yet).
 */
displayState: string, 
/**
 * Always `None` for inbound in v2 (no receiver-side retry backoff yet);
 * present for shape-parity with [`OutboundSummary`].
 */
stalledUntil: string | null, 
/**
 * Per-file rollup for the progress line ("N of M files").
 */
fileCounts: TransferFileCounts, };

export type SyncSenderStatus = { 
/**
 * Whether the sender engine has been lazily started this session.
 */
started: boolean, 
/**
 * Non-terminal packages not yet in the transferring window (`queued` /
 * `announced`).
 */
queued: number, 
/**
 * Packages in the in-flight transferring window (awaiting the peer ack).
 */
transferring: number, 
/**
 * Terminal `confirmed` package count (all frames ingested-or-duplicate).
 */
confirmedTotal: number, 
/**
 * Terminal `failed` package count (local-unrecoverable payload only).
 */
failedTotal: number, 
/**
 * Terminal `cancelled` package count (user-cancelled sends).
 */
cancelledTotal: number, 
/**
 * The in-flight rows for the Active tab.
 */
active: Array<OutboundSummary>, };

export type SyncReceiverStatus = { 
/**
 * Whether the receiver transport is running (a ticket has been minted).
 */
started: boolean, 
/**
 * The in-flight inbound rows for the receive-side Active tab (non-terminal
 * `sync_inbound` rows, Task 14). Empty when nothing is being received.
 */
active: Array<InboundSummary>, 
/**
 * Total frames received (history rows with `direction = received`).
 */
receivedTotal: number, };

export type TransportHealth = { 
/**
 * One of `"not_started"` (no transport bound yet), `"relay_connected"`
 * (a home relay is up — remote peers can be reached), `"direct_only"` (the
 * relay is disabled or unreachable — peers behind NAT may not reach us), or
 * `"no_relay_map"` (signed in but no relay configuration resolved or cached —
 * remote transfers will stall).
 */
status: string, 
/**
 * The home-relay URL, when one is known (currently connected, or the last
 * one the watcher saw). `None` before any relay transition / with no relay.
 */
relayUrl: string | null, 
/**
 * The most recent relay error, when the last transition was a disconnect.
 * `None` on a healthy relay or when the disconnect carried no error.
 */
lastError: string | null, };

export type SyncStatus = { 
/**
 * Whether the dev pairing flag (`sync.dev_ticket_pairing`) is enabled.
 */
devPairingEnabled: boolean, 
/**
 * Whether the receiver transport is running (a ticket has been minted).
 */
transportStarted: boolean, 
/**
 * This device's pairing ticket, once the receiver has started.
 */
pairingTicket: string | null, 
/**
 * Total frames received (history rows with `direction = received`).
 */
receivedTotal: number, 
/**
 * Send-side rollup.
 */
sender: SyncSenderStatus, 
/**
 * Receive-side rollup.
 */
receiver: SyncReceiverStatus, 
/**
 * Transport reachability health (Task 3.3): relay connected / direct-only /
 * no relay map / not started. Drives the sidebar badge's health dot.
 */
transport: TransportHealth, };

export type TransferFileEntry = { 
/**
 * The file's path relative to the batch root (forward-slash, structured per
 * §D2) — the primary key of the per-file tables (Transfers Status Model v2).
 * Drives the detail pane's collapsible directory tree. For a legacy pre-v2
 * batch with no per-file rows this falls back to the manifest `rel_path`
 * (sent) or the history `filename` (received).
 */
relPath: string, 
/**
 * The file's basename within the package (kept for compat).
 */
name: string, 
/**
 * The file's payload size in bytes (from the per-file row / manifest).
 */
bytesTotal: number, 
/**
 * Cumulative bytes transferred for this file, when known; `None` on the
 * legacy fallback path where no per-file row exists.
 */
bytesDone: number | null, 
/**
 * Per-file lifecycle state (`pending`/`sending`/`uploaded`/`done` outgoing;
 * `announced`/`fetching`/`done`/`failed` incoming), from the per-file row.
 * `None` on the legacy fallback path.
 */
state: string | null, 
/**
 * Per-frame outcome once settled: outgoing — the peer's ack verdict
 * (`ingested`/`duplicate`/`rejected`/…); incoming — this node's ingest
 * verdict. `None` while still in flight / not yet known.
 */
outcome: string | null, 
/**
 * A per-file error detail when this file's transfer failed, else `None`.
 */
error: string | null, };

export type TerminalTransfers = { 
/**
 * Terminal outbound rows (`confirmed`/`failed`/`cancelled`), newest-first.
 */
sent: Array<OutboundSummary>, 
/**
 * Terminal inbound rows (`done`/`failed`/`cancelled`), newest-first.
 */
received: Array<InboundSummary>, };

export type SyncProgressEvent = { packageId: string, 
/**
 * Which half emitted this tick (`received` = inbound, `sent` = outbound).
 */
direction: Direction, 
/**
 * Coarse stage: receiver `received`/`fetching`/`ingesting`, or sender
 * `queued`/`transferring`.
 */
stage: string, 
/**
 * The other peer's node id (hex): the sending peer for a receive tick, the
 * destination peer for a send tick.
 */
peerDevice: string, frameCount: number, 
/**
 * Collab exchange (slice 4): the project id when this package is a project
 * exchange, else `None` for personal sync. Additive — the Transfers UI reads
 * it in Task 11 to route project transfers to the project view.
 */
projectId: string | null, 
/**
 * Cumulative fetched bytes at this tick (Task 11). Present only on the
 * `fetching`-stage ticks driven by the transport's batch progress; `None` on
 * the coarse stage ticks (`received`/`ingesting`/sender stages) that carry no
 * byte figure.
 */
bytesDone: number | null, 
/**
 * Total package bytes for the fetch (the announce's `byte_size`), paired with
 * [`bytes_done`](Self::bytes_done) on `fetching` ticks; `None` elsewhere.
 */
bytesTotal: number | null, };

export type SyncFileProgressEvent = { packageId: string, 
/**
 * Sending peer's node id (hex).
 */
peerDevice: string, 
/**
 * The entry's forward-slash `rel_path` within the package.
 */
file: string, bytesDone: number, bytesTotal: number, };

export type SyncFinishedEvent = { packageId: string, 
/**
 * Which half finished (`received` = inbound, `sent` = outbound), so the UI
 * can raise the right notification ("frames arrived" vs "package delivered").
 */
direction: Direction, 
/**
 * Receiver: `ingested` (all accepted), `partial` (some rejected), `failed`
 * (all rejected), or `replayed` (re-acked from the receipt log, no ingest).
 * Sender: `confirmed`, `failed[: …]`, or `cancelled`.
 */
outcome: string, 
/**
 * The other peer's node id (hex).
 */
peerDevice: string, okCount: number, 
/**
 * Frame uuids the receiver rejected (integrity failure).
 */
failed: Array<string>, 
/**
 * Sender-only dedup outcome (Sync Phase 3): frames actually sent (`new`) vs.
 * dropped as the peer's duplicates by the pre-announce handshake. Always `0`
 * on the receiver-side emits — the receiver reports ingest/duplicate per
 * frame in [`ok_count`](Self::ok_count) / its receipts, not this split.
 */
newCount: number, duplicateCount: number, 
/**
 * Collab exchange (slice 4): the project id when this package is a project
 * exchange, else `None` for personal sync. Additive — Task 11 wires it to
 * the project-transfer UI/notification.
 */
projectId: string | null, };

export type SyncHistoryQuery = { 
/**
 * Exact `filename` filter (unfiltered when absent).
 */
filename: string | null, 
/**
 * Exact `object` filter (unfiltered when absent).
 */
object: string | null, 
/**
 * Restrict to one direction (`sent` / `received`); unfiltered when absent.
 */
direction: Direction | null, 
/**
 * Exact peer node id (hex) filter; unfiltered when absent.
 */
peer: string | null, 
/**
 * Exact `project_id` filter (Stage II collab); unfiltered when absent. The
 * Transfers UI's project-dimension passthrough (Task 11).
 */
project: string | null, 
/**
 * Newest-first cap. `0` is treated as the default cap.
 */
limit: number, };

export type IneligibleFrame = { frameId: number, reason: string, };

export type EnqueueSelectionResult = { 
/**
 * Frames actually enqueued for send (equals `eligible_count`).
 */
enqueuedCount: number, 
/**
 * Eligible frames — present on disk and resolvable in the catalog. The `N`.
 */
eligibleCount: number, 
/**
 * Total frames requested. The `M` in `(N of M)`.
 */
totalCount: number, 
/**
 * Frames that could not be sent, each with a reason. Never silently dropped.
 */
ineligible: Array<IneligibleFrame>, };

export type TransferEventEntry = { 
/**
 * RFC3339 UTC timestamp of the event.
 */
ts: string, 
/**
 * Short snake_case event kind (`announce_sent`, `dial_failed`,
 * `retry_scheduled`, `serve_started`, `uploaded`, `ack_received`,
 * `fetch_started`, `ingested`, `cancelled`, …).
 */
kind: string, 
/**
 * Optional human detail (e.g. a class-tagged reason). `None` for a bare event.
 */
detail: string | null, };

export type DeviceCapability = "athenaeum" | "perseus";

export type EndpointAddrReport = { 
/**
 * The device's home relay url (`None` when it has no home relay yet).
 */
homeRelayUrl: string | null, 
/**
 * The device's direct (IP) socket addresses, as strings — used only for a
 * SAME-ACCOUNT dial (never handed to a cross-account collaborator; S1).
 */
directAddrs: Array<string>, 
/**
 * When the hub last stamped this report (RFC3339). Diagnostics only; the
 * hub owns it, so the app treats it as read-only and optional.
 */
reportedAt: string | null, };

export type AccountDevice = { id: string, name: string, 
/**
 * Base64 of the device's 32-byte ed25519 public key (== its node id).
 */
pubkey: string, 
/**
 * What this device is in the mesh (full peer vs send-only agent). Absent on
 * older hub payloads → defaults to [`DeviceCapability::Athenaeum`].
 */
capability: DeviceCapability, createdAt: string, lastSeenAt: string | null, 
/**
 * This device's self-reported dialable endpoint address (finding H1, T7).
 * Absent on older hub payloads (or a device that never reported) → `None`,
 * in which case every dial path falls back to the our-relay-map hint exactly
 * as it did before this field existed.
 */
endpointAddr: EndpointAddrReport | null, };

export type AccountStatus = { 
/**
 * Whether a device token is present locally.
 */
signedIn: boolean, 
/**
 * Signed-in email (display only); `None` when signed out.
 */
email: string | null, 
/**
 * This device's hub-assigned id; `None` when signed out.
 */
deviceId: string | null, 
/**
 * This device's capability. The app is always a full peer
 * ([`DeviceCapability::Athenaeum`]).
 */
capability: DeviceCapability, 
/**
 * The hub this device authenticates against.
 */
hubUrl: string, };

export type ProjectCard = { projectId: string, slug: string, title: string, dataRole: string, coordinator: boolean, requireApproval: boolean, pendingAnnouncements: number, projectStatus: string, targetName: string, targetRaDeg: number, targetDecDeg: number, targetRadiusDeg: number, membershipVersion: number, linkedSets: number, candidates: number, publishable: number, fetchedAt: string, };

export type ProjectDetail = { card: ProjectCard, members: Array<ProjectMemberView>, thresholdsVersion: number | null, thresholds: Array<ThresholdRuleView>, links: Array<LinkedSetView>, portalBase: string, };

export type ProjectMemberView = { displayName: string, dataRole: string, coordinator: boolean, };

export type LinkedSetView = { framesSetId: number, name: string | null, lightCount: number, distanceDeg: number | null, withinRadius: boolean, };

export type LinkSuggestion = { framesSetId: number, name: string | null, lightCount: number, 
/**
 * Angular distance (deg) from the set center to the project target; `None`
 * when the set has no parseable center.
 */
distanceDeg: number | null, withinRadius: boolean, alreadyLinked: boolean, };

export type GateReport = { projectId: string, total: number, publishable: number, rows: Array<FrameGateRow>, };

export type ProjectSetMatch = { projectId: string, projectTitle: string, projectSlug: string, };

export type ProjectSetMatchEvent = { framesSetId: number, setName: string | null, matches: Array<ProjectSetMatch>, };

export type PortalNewProjectLink = { url: string, };

export type PublishResult = { 
/**
 * The HUB package uuid (minted first, stamped into every record, announced).
 */
packageId: string, 
/**
 * The hub's announcement id (`{id}` from the announce reply).
 */
announcementId: string, 
/**
 * `pending` (require_approval) | `published`.
 */
state: string, frameCount: number, byteSize: number, 
/**
 * Announcement ids this publish superseded (Д9); also flipped `superseded=1`
 * locally.
 */
supersededAnnouncements: Array<string>, 
/**
 * Display name of the chosen first-replica member; `None` when no
 * receive-capable member is available to seed (still announced).
 */
seedTarget: string | null, };

export type ModerationFrame = { frameUuid: string, relPath: string, 
/**
 * Absolute on-disk path of the landed review copy; `None` when the
 * contribution row carries no landed path.
 */
landedPath: string | null, byteSize: number, fwhm: number | null, eccentricity: number | null, stars: number | null, snr: number | null, };

export type ModerationItem = { announcementId: string, packageId: string, publisher: string, frameCount: number, byteSize: number, createdAt: string, 
/**
 * The push-seed review copy has fully landed (`local_status == "complete"`);
 * `false` while the coordinator is still receiving it.
 */
reviewCopyComplete: boolean, frames: Array<ModerationFrame>, };

export type PackageStateChange = { projectId: string, packageId: string, 
/**
 * `newPackage` | `approved` | `rejected` | `downloadFailed` | `awaitingApproval`.
 */
kind: string, detail: string | null, };

export type ProjectPackageView = { 
/**
 * HUB package uuid (the `project_packages` row key).
 */
packageId: string, 
/**
 * Hub-mirrored decision: `pending` | `published` | `rejected`.
 */
state: string, 
/**
 * Local fetch progress: `none` | `downloading` | `complete` | `failed`.
 */
localStatus: string, 
/**
 * I published this package.
 */
own: boolean, publisher: string, byteSize: number, frameCount: number, createdAt: string, rejectReason: string | null, 
/**
 * Another announcement supersedes this one.
 */
superseded: boolean, 
/**
 * Holders the hub listed at poll time.
 */
holderCount: number, 
/**
 * Of those holders, how many the hub last saw within the online window.
 */
onlineCount: number, };

export type ContributionView = { packageId: string, frameUuid: string, publisher: string, relPath: string, byteSize: number, 
/**
 * A newer contribution for the same frame uuid supersedes this one.
 */
superseded: boolean, createdAt: string, };

export type FrameGateRow = { frameId: number, filename: string, fwhmArcsec: number | null, eccentricity: number | null, starsDetected: number | null, trailed: boolean | null, publishable: boolean, 
/**
 * Human-readable failure reasons, empty when publishable
 * (e.g. `FWHM 3.4″ > 3.0″`, `not calibrated (Stale)`, `no analysis`,
 * `unknown pixel scale`, `outside target radius (2.1° > 1.5°)`).
 */
failures: Array<string>, };

export type ThresholdRuleView = { metricKey: string, op: string, 
/**
 * The repo's ts-rs feature set has NO `serde-json-impl`, so
 * `serde_json::Value` cannot derive `TS` — override the emitted TS type
 * (the hub validates rule values to number|bool, so this is exact).
 */
value: number | boolean, };

