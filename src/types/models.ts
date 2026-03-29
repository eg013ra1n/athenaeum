// TypeScript interfaces matching Rust models

export interface BlackholeChangedEvent {
  file_id: number;
  action: 'blackholed' | 'restored';
}

export enum FileFormat {
  FITS = "FITS",
  XISF = "XISF",
}

export enum ImageType {
  Light = "Light",
  Dark = "Dark",
  Flat = "Flat",
  Bias = "Bias",
  DarkFlat = "DarkFlat",
  // Master calibration types (already calibrated, no sub-calibration needed)
  MasterDark = "MasterDark",
  MasterFlat = "MasterFlat",
  MasterBias = "MasterBias",
  MasterDarkFlat = "MasterDarkFlat",
}

// Helper function to check if an imagetyp is a master type
export function isMasterType(imagetyp: ImageType | string): boolean {
  return [
    ImageType.MasterDark,
    ImageType.MasterFlat,
    ImageType.MasterBias,
    ImageType.MasterDarkFlat,
    "MasterDark",
    "MasterFlat",
    "MasterBias",
    "MasterDarkFlat",
  ].includes(imagetyp as ImageType);
}

export interface File {
  id: number | null;
  path: string;
  filename: string;
  size: number;
  modified_at: string; // ISO 8601 datetime
  format: FileFormat;
  created_at: string; // ISO 8601 datetime
  metadata_hash: string | null; // Quick hash for duplicate detection
}

export interface Frame {
  id: number | null;
  file_id: number;
  object: string | null;
  date_obs: string | null; // ISO 8601 datetime
  telescop: string | null;
  instrume: string | null;
  exptime: number | null;
  filter: string | null;
  imagetyp: ImageType | null;
  gain: number | null;
  offset: number | null;
  binning: string | null;
  xbinning: number | null;
  ybinning: number | null;
  ccd_temp: number | null;
  set_temp: number | null;
  focallen: number | null;
  xpixsz: number | null;
  ypixsz: number | null;
  ra: number | null;
  dec: number | null;
  sitelat: number | null;
  lat_obs: number | null;
  sitelong: number | null;
  long_obs: number | null;
  objctra: string | null;
  objctdec: string | null;
  rotation: number | null;
  override_: boolean;
}

export interface Day {
  id: number | null;
  date: string; // ISO 8601 date (YYYY-MM-DD)
  frame_count: number;
}

export interface Setup {
  id: number | null;
  telescop: string | null;
  instrume: string | null;
  filter: string | null;
  binning: string | null;
  gain: number | null;
}

export interface CalibrationSet {
  id: number | null;
  imagetyp: ImageType;
  exptime: number | null;
  filter: string | null;
  ccd_temp: number | null;
  gain: number | null;
  binning: string | null;
  instrume: string | null;
  date: string;
  frame_ids: number[];
}

export interface Tag {
  id: number | null;
  name: string;
  color: string | null;
}

export interface FrameTag {
  frame_id: number;
  tag_id: number;
}

export interface ScanRoot {
  id: number | null;
  path: string;
  enabled: boolean;
  find_duplicates: boolean;
  unique_camera: boolean;
  last_scan: string | null; // ISO 8601 datetime
  last_scan_errors: string[] | null;
}

export interface ScanRootWithAvailability extends ScanRoot {
  is_available: boolean;
}

export interface ExportTemplate {
  id: number | null;
  name: string;
  template: string;
  description: string | null;
}

export interface DuplicateGroup {
  id: number | null;
  size: number;
  content_hash: string;
  file_count: number;
  file_paths: string[];
  file_ids: number[];
}

export interface BlackHoleEntry {
  id: number | null;
  file_id: number;
  filename: string;
  original_path: string;
  from_where: string;
  moved_at: string; // ISO 8601 datetime
  file_size: number;
}

export interface FolderSimilarity {
  folder_a: string;
  folder_b: string;
  similarity_percent: number;
  shared_files: number;
  shared_size: number;
  unique_a: number;
  unique_b: number;
  shared_file_ids: number[];
}

// DTOs for Tauri commands
export interface ScanResult {
  files_found: number;
  files_processed: number;
  files_skipped: number;
  errors: string[];
  // Frame type counts
  lights_count: number;
  darks_count: number;
  flats_count: number;
  bias_count: number;
  darkflats_count: number;
  // Calibration sets created
  calibration_sets_created: number;
  // Unique camera reconciliation stats (non-zero when instrume suffix state changed during rescan)
  frames_renamed: number;
  calibration_sets_deleted: number;
  sessions_updated: number;
  // Whether scan was cancelled by user
  cancelled: boolean;
}

// Scan progress event sent from backend
export interface ScanProgressEvent {
  current: number;
  total: number;
  current_file: string | null;
  percent: number;
  root_id: number;
  phase: 'verifying' | 'discovery' | 'processing' | 'inserting' | 'calibrating' | 'caching';
}

// Scan completion event sent from backend
export interface ScanCompleteEvent {
  root_id: number;
  files_found: number;
  files_processed: number;
  files_skipped: number;
  errors: string[];
  lights_count: number;
  darks_count: number;
  flats_count: number;
  bias_count: number;
  darkflats_count: number;
  calibration_sets_created: number;
  frames_renamed?: number;
  calibration_sets_deleted?: number;
  sessions_updated?: number;
  cancelled: boolean;
}

export interface FileWithFrame {
  file: File;
  frame: Frame | null;
}

export interface DirectoryContents {
  subdirectories: string[];
  files: FileWithFrame[];
}

export interface Project {
  id: number | null;
  name: string;
}

export interface FramesSet {
  id: number | null;
  name: string | null;
  is_custom: boolean;
  is_archived: boolean;
  date_obs_start: string | null;
  date_obs_end: string | null;
  objctra: string | null;
  objctdec: string | null;
  total_exp_time: number | null;
  flat_pattern: string | null;  // e.g., "before_session", "after_session", "manual"
  avg_rotation: number | null;
  min_rotation: number | null;
  max_rotation: number | null;
}

export interface FramesSetMember {
  frames_set_id: number;
  frame_id: number;
}

export interface FitsHeader {
  id: number | null;
  file_id: number;
  header: string;
}

export interface Setting {
  key: string;
  value: string;
  updated_at: string | null;
}

// Frame Sets DTOs
export interface AutoGenerateResult {
  sets_created: number;
  frames_clustered: number;
  frames_excluded: number;
  frames_already_in_sets: number;
  exclusion_reasons: string[];
}

export interface FramesSetWithCount {
  frames_set: FramesSet;
  member_count: number;
}

// Imaging Nights and Sessions
export interface ImagingNight {
  id: number | null;
  frames_set_id: number;
  start_time: string;
  end_time: string;
  created_at: string | null;
}

export interface Session {
  id: number | null;
  imaging_night_id: number;
  instrume: string;
  frame_count: number;
  total_exp_time: number | null;
  created_at: string | null;
}

export interface SessionWithMetadata {
  id: number | null;
  imaging_night_id: number;
  instrume: string;
  frame_count: number;
  total_exp_time: number | null;
  created_at: string | null;
  start_date: string | null;
  end_date: string | null;
  avg_ra: string | null;
  avg_dec: string | null;
}

export interface SessionMember {
  session_id: number;
  frame_id: number;
}

export interface SessionWithFrames {
  session: Session;
  frames: FileWithFrame[];
}

export interface ImagingNightWithSessions {
  imaging_night: ImagingNight;
  sessions: SessionWithFrames[];
}

export interface FrameSetDetail {
  frames_set: FramesSet;
  nights: ImagingNightWithSessions[];
}

// Equipment & Dark Library
export interface CameraStats {
  instrume: string;
  frame_count: number;
  total_hours: number;
  first_use: string | null;
  last_use: string | null;
}

export interface CalibrationSetDetail {
  id: number | null;
  imagetyp: ImageType;
  exptime: number | null;
  ccd_temp: number;
  temp_min: number;
  temp_max: number;
  gain: number | null;
  offset: number | null;
  binning: string | null;
  instrume: string | null;
  filter: string | null;  // Filter (for flats)
  date_start: string;
  date_end: string;
  date_display: string;
  frame_count: number;
  is_master: boolean;  // True if this is a master calibration set
  // Extended fields from frame metadata
  naxis1: number | null;      // Image width
  naxis2: number | null;      // Image height
  bayerpat: string | null;    // Bayer pattern (e.g., "RGGB") or null for mono
  swcreate: string | null;    // Software that created the file
  xpixsz: number | null;      // Pixel size in microns
  format: string | null;      // File format (FITS, XISF)
  focallen: number | null;    // Focal length in mm
}

export interface DarkLibraryResult {
  sets_created: number;
  frames_grouped: number;
  frames_excluded: number;
}

// FITS Image Data for Blink Viewer
export interface FitsImageData {
  image_base64: string;
  width: number;
  height: number;
  is_color: boolean;
  bit_depth: number;
}

// File Relinking
export interface RelinkResult {
  files_matched: number;
  files_new: number;
  files_orphaned: number;
  orphaned_file_ids: number[];
}

export interface OrphanedFile {
  id: number;
  path: string;
  filename: string;
  size: number;
  modified_at: string;
  has_frame: boolean;
  object: string | null;
  date_obs: string | null;
}

// Missing file record with status from the database
export interface MissingFileRecord {
  id: number;
  file_id: number;
  scan_root_id: number;
  detected_at: string;
  last_checked_at: string;
  status: 'missing' | 'ignored';
  // File info
  path: string;
  filename: string;
  size: number;
  modified_at: string;
  // Frame info (if exists)
  has_frame: boolean;
  object: string | null;
  date_obs: string | null;
}

// Sky Atlas
export interface ImagingLocation {
  id: number;
  ra: number;
  dec: number;
  objectName: string | null;
  frameCount: number;
  totalExposure: number;  // in seconds
  filters: string[];
  dateRange: [string, string];  // ISO date strings
  frameSetId: number | null;
  fovWidth: number | null;   // Field of view in degrees
  fovHeight: number | null;  // Field of view in degrees
  locationType: 'frameset' | 'cluster';  // 'frameset' for organized, 'cluster' for unorganized
  cameras: string | null;  // Comma-separated list of camera/instrument names
  focalLengths: string | null;  // Comma-separated list of focal lengths in mm
  isCustom: boolean;  // true for custom frame sets, false for auto-generated or clusters
  rotation: number | null;  // Average position angle in degrees (N through E)
}

// Frame Set Refresh
export interface SetUpdateReport {
  set_id: number;
  set_name: string;
  frames_added: number;
  nights_created: number;
  nights_updated: number;
  frame_ids_added: number[];
  frame_names_added: string[];
}

export interface RefreshResult {
  frames_added: number;
  sets_updated: SetUpdateReport[];
  frames_unassigned: number;
}

// Calibration Finder
export interface CalibrationLink {
  id: number | null;
  source_id: number;
  source_type: 'frame' | 'calibration_set';
  calibration_set_id: number;
  calibration_type: 'Dark' | 'Flat' | 'Bias' | 'DarkFlat';
  matched_at: string;  // ISO 8601
  match_score: number | null;  // 0.0-1.0 confidence
  date_warning: boolean;
  temp_warning: boolean;
  is_manual_override: boolean;  // true if manually assigned by user
}

export interface FrameCalibrationStatus {
  frame_id: number;
  has_flats: boolean;
  has_darks: boolean;
  has_bias: boolean;
  has_darkflats: boolean;
  flats_warning: boolean;
  darks_warning: boolean;
  bias_warning: boolean;
  flat_set_id: number | null;
  dark_set_id: number | null;
  bias_set_id: number | null;
  darkflat_set_id: number | null;
}

export interface CalibrationHierarchy {
  light_frame_id: number;
  flat_sets: CalibrationSetWithLinks[];
  dark_sets: CalibrationSetWithLinks[];
  missing_calibration: string[];  // List of missing calibration types
  warnings: CalibrationWarning[];
}

export interface CalibrationSetWithLinks {
  set: CalibrationSetDetail;
  sub_calibration: CalibrationLink[];  // Links to Dark/Bias sets for this set
}

export interface CalibrationWarning {
  warning_type: 'date' | 'temperature';
  message: string;
  calibration_type: 'Dark' | 'Flat' | 'Bias' | 'DarkFlat';
  set_id: number;
}

export interface CalibrationMatchResult {
  frames_processed: number;
  frames_with_calibration: number;
  frames_partial_calibration: number;
  frames_no_calibration: number;
  sets_linked: number;
  warnings_count: number;
  processing_time_ms: number;
  frame_statuses: FrameCalibrationStatus[];
}

export interface CalibrationStats {
  total_frames: number;
  frames_with_flats: number;
  frames_with_darks: number;
  frames_with_bias: number;
  frames_complete: number;  // All required calibration found
  frames_partial: number;    // Some calibration found
  frames_none: number;       // No calibration found
  total_warnings: number;
}

export interface CalibrationGroup {
  flat_set_id: number | null;
  dark_set_id: number | null;
  bias_set_id: number | null;
  flat_set_detail: CalibrationSetDetail | null;
  dark_set_detail: CalibrationSetDetail | null;
  bias_set_detail: CalibrationSetDetail | null;
  frame_count: number;
  frame_ids: number[];
  has_warnings: boolean;
  // Per-calibration warnings with contextual messages
  flat_warnings: CalibrationWarning[];
  dark_warnings: CalibrationWarning[];
  bias_warnings: CalibrationWarning[];
}

export interface FrameSetCalibrationGroups {
  groups: CalibrationGroup[];
  uncalibrated_frame_count: number;
  uncalibrated_frame_ids: number[];
  total_frames: number;
}

export interface CalibrationTolerance {
  flat_date_warning_days: number;
  dark_date_warning_days: number;
}

export interface ProcessingProgress {
  total_frames: number;
  processed_frames: number;
  current_frame_id: number | null;
  percent_complete: number;
}

export interface ProcessingStats {
  total_frames: number;
  frames_with_full_calibration: number;
  frames_with_partial_calibration: number;
  frames_with_no_calibration: number;
  total_flat_sets_linked: number;
  total_dark_sets_linked: number;
  total_warnings: number;
  date_warnings: number;
  temp_warnings: number;
  missing_flats: number;
  missing_darks: number;
  missing_bias: number;
  frames_with_flats_only: number;
  frames_with_darks_only: number;
}

// Flat Calibration System
export enum FlatTiming {
  Before = "Before",
  After = "After",
  During = "During",
}


export interface FlatGroup {
  frame_ids: number[];
  start_time: string;  // ISO 8601 datetime
  end_time: string;    // ISO 8601 datetime
  avg_temp: number | null;
  frame_count: number;
  filter: string | null;
  instrume: string;
  binning: string;
  gain: number | null;
  offset: number | null;
  exptime: number | null;
  focal_length: number | null;
}

export interface FlatGroupMatch {
  group: FlatGroup;
  match_score: number;  // 0.0-1.0, higher is better
  age_days: number;
  temp_diff: number | null;
  timing: FlatTiming;
}

export interface FilterPeriod {
  filter: string | null;
  start_time: string;  // ISO 8601 datetime
  end_time: string;    // ISO 8601 datetime
  frame_count: number;
}

// ========== Calibration Hierarchy View ==========

/** Hierarchical calibration view organized by Date → Camera → Filter */
export interface CalibrationHierarchyView {
  date_groups: CalibrationDateGroup[];
  total_frames: number;
  calibrated_frames: number;
  uncalibrated_frames: number;
}

/** Group of frames for a single session date */
export interface CalibrationDateGroup {
  date: string;                    // e.g., "2024-01-15"
  date_display: string;            // e.g., "January 15, 2024"
  camera_groups: CalibrationCameraGroup[];
  frame_count: number;
  has_warnings: boolean;
}

/** Group of frames for a single camera within a date */
export interface CalibrationCameraGroup {
  instrume: string;                // Camera name
  filter_groups: CalibrationFilterGroup[];
  frame_count: number;
  has_warnings: boolean;
}

/** Sub-calibration linked to a calibration set with full details */
export interface SubCalibrationDetail {
  calibration_type: 'Dark' | 'DarkFlat' | 'Bias';
  set: CalibrationSetDetail;
  date_warning: boolean;
  temp_warning: boolean;
}

/** A calibration set with the count of frames that use it */
export interface CalibrationSetWithFrameCount {
  set: CalibrationSetDetail;
  frame_count: number;        // How many frames in this group use this set
  frame_ids: number[];        // Which frames use this set
  warnings: CalibrationWarning[];
  sub_calibration: SubCalibrationDetail[];  // Linked sub-calibrations (e.g., Flat→Dark, Dark→Bias)
}

/** A calibration set with match score for manual selection */
export interface CalibrationSetWithScore {
  set: CalibrationSetDetail;
  match_score: number;        // 0.0-1.0, higher is better match
  match_details: MatchDetails;
}

/** Details about how well a calibration set matches light frame parameters */
export interface MatchDetails {
  instrume_match: boolean;    // Camera matches
  binning_match: boolean;     // Binning matches
  gain_match: boolean;        // Gain matches (or both null)
  filter_match: boolean;      // Filter matches (only relevant for flats)
  temp_diff: number | null;   // Temperature difference in Celsius
  date_diff_days: number;     // Days between calibration and light frames
}

/** Average parameters of light frames for manual selection display */
export interface LightFrameParameters {
  instrume: string | null;
  binning: string | null;
  gain: number | null;
  offset: number | null;
  filter: string | null;
  avg_ccd_temp: number | null;
  avg_exptime: number | null;
  exptime_range: [number, number] | null;  // [min, max]
  frame_count: number;
  date_range: [string, string] | null;     // [start, end]
  current_flat_set_id: number | null;
  current_dark_set_id: number | null;
  current_bias_set_id: number | null;
}

/** Parameters of a calibration set for sub-calibration selection display */
export interface CalibrationSetParameters {
  set_id: number;
  imagetyp: string;
  instrume: string | null;
  binning: string | null;
  gain: number | null;
  offset: number | null;
  exptime: number | null;
  filter: string | null;
  ccd_temp: number | null;
  date_start: string | null;
  date_end: string | null;
  frame_count: number;
  // Current sub-calibration links
  current_dark_set_id: number | null;
  current_darkflat_set_id: number | null;
  current_bias_set_id: number | null;
}

/** Group of frames for a single filter within a camera */
export interface CalibrationFilterGroup {
  filter: string | null;          // null = "No Filter"
  filter_display: string;          // "Ha", "OIII", "No Filter", or "Ha (60s)" when split by exptime
  exptime: number | null;         // When split by exposure time, this is the exptime for this sub-group
  light_frames: LightFrameWithCalibration[];
  flat_sets: CalibrationSetWithFrameCount[];   // All unique flat sets used by frames in this group
  dark_sets: CalibrationSetWithFrameCount[];   // All unique dark sets used by frames in this group
  bias_sets: CalibrationSetWithFrameCount[];   // All unique bias sets used by frames in this group
  has_warnings: boolean;
  frame_count: number;
}

/** A light frame with its calibration status */
export interface LightFrameWithCalibration {
  frame_id: number;
  file_id: number;
  filename: string;
  file_path: string;
  date_obs: string | null;
  exptime: number | null;
  telescop: string | null;
  focallen: number | null;
  xpixsz: number | null;
  binning: string | null;
  ccd_temp: number | null;
  swcreate: string | null;
  gain: number | null;
  offset: number | null;
  rotation: number | null;
  objctra: string | null;
  objctdec: string | null;
  ra: number | null;
  dec: number | null;
  calibration_status: FrameCalibrationStatus;
}

// ========== Calendar View Interfaces ==========

/** Summary of a frame set for calendar display */
export interface CalendarFrameSetSummary {
  id: number;
  name: string | null;
  objectName: string | null;
  frameCount: number;
  totalExposureSeconds: number;
  ra: number | null;
  dec: number | null;
  filters: string[];
}

/** Group of unorganized frames for calendar display */
export interface CalendarUnorganizedGroup {
  id: string;
  objectName: string | null;
  frameCount: number;
  totalExposureSeconds: number;
  ra: number | null;
  dec: number | null;
  filters: string[];
  frameIds: number[];
}

/** Summary of imaging activity for a single calendar day */
export interface CalendarDayEvent {
  date: string; // YYYY-MM-DD
  frameSets: CalendarFrameSetSummary[];
  unorganizedGroups: CalendarUnorganizedGroup[];
  totalFrameCount: number;
  totalExposureSeconds: number;
}

/** Full calendar data for a month */
export interface CalendarMonthData {
  year: number;
  month: number; // 1-12
  days: CalendarDayEvent[];
  totalFrameCount: number;
  totalExposureSeconds: number;
}

export interface CalendarYearData {
  year: number;
  months: CalendarMonthData[]; // Only months with data
  totalFrameCount: number;
  totalExposureSeconds: number;
}

// ========== Calibration Set Metadata Editing ==========

// Excluded frames from auto-generation
export interface ExcludedFrameEntry {
  file_id: number;
  path: string;
  filename: string;
  reason: string;
  excluded_at: string;
}

/** Result of reclassifying excluded frames */
export interface ReclassifyResult {
  frames_updated: number;
  cameras_refreshed: string[];
}

// ========== Frame Analysis ==========

/** Star detection and image quality analysis results for a single frame */
export interface FrameAnalysis {
  id: number | null;
  frame_id: number;
  file_id: number;
  stars_detected: number;
  median_fwhm: number;
  median_eccentricity: number;
  median_snr: number;
  median_hfr: number;
  frame_snr: number;
  snr_weight: number;
  psf_signal: number;
  background: number;
  noise: number;
  detection_threshold: number;
  width: number;
  height: number;
  source_channels: number;
  trail_r_squared: number;
  possibly_trailed: boolean;
  median_beta: number | null;
  quality_score: number | null;
  config_hash: string | null;
  analyzed_at: string;
}

/** Result of batch frame set analysis */
export interface AnalyzeFrameSetResult {
  analyzed: number;
  skipped: number;
  failed: number;
  errors: string[];
  cancelled: boolean;
}

/** Analysis progress event emitted during batch analysis */
export interface AnalysisProgressEvent {
  frame_set_id: number;
  current: number;
  total: number;
  current_file: string;
  percent: number;
}

/** Analysis complete event emitted when a frame set analysis finishes */
export interface AnalysisCompleteEvent {
  frame_set_id: number;
  analyzed: number;
  skipped: number;
  failed: number;
  errors: string[];
  cancelled: boolean;
}

/** Edits to apply to calibration set metadata (selective fields) */
export interface CalibrationMetadataEdits {
  ccd_temp?: number | null;
  gain?: number | null;
  offset?: number | null;
  binning?: string | null;
  exptime?: number | null;
}

/** Individual star detection result for client-side overlay rendering */
export interface StarMetric {
  id: number | null;
  frame_analysis_id: number;
  x: number;
  y: number;
  peak: number;
  flux: number;
  fwhm: number;
  fwhm_x: number;
  fwhm_y: number;
  eccentricity: number;
  snr: number;
  hfr: number;
  theta: number;
  beta: number | null;
  fit_method: string;
  fit_residual: number;
}

/** Response from get_frame_star_metrics command */
export interface StarMetricsResponse {
  stars: StarMetric[];
  metrics: FrameAnalysis;
  flip_vertical: boolean;
  image_width: number;
  image_height: number;
}

/** Original calibration set metadata values (backed up before editing) */
export interface CalibrationSetOriginals {
  set_id: number;
  ccd_temp: number | null;
  temp_min: number | null;
  temp_max: number | null;
  gain: number | null;
  offset: number | null;
  binning: string | null;
  exptime: number | null;
  saved_at: string;  // ISO 8601
}
