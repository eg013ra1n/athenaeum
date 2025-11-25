// TypeScript interfaces matching Rust models

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
  pixsz: number | null;
  ra: number | null;
  dec: number | null;
  sitelat: number | null;
  lat_obs: number | null;
  sitelong: number | null;
  long_obs: number | null;
  objctra: string | null;
  objctdec: string | null;
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
  last_scan: string | null; // ISO 8601 datetime
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
  date_obs_start: string | null;
  date_obs_end: string | null;
  objctra: string | null;
  objctdec: string | null;
  total_exp_time: number | null;
  flat_pattern: string | null;  // e.g., "before_session", "after_session", "manual"
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
  date_start: string;
  date_end: string;
  date_display: string;
  frame_count: number;
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
  temp_delta_celsius: number;
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
}

// Flat Calibration System
export enum FlatTiming {
  Before = "Before",
  After = "After",
  During = "During",
}

export enum FlatPattern {
  BeforeSession = "before_session",
  AfterSession = "after_session",
  BeforeFilterChange = "before_filter_change",
  LongTerm = "long_term",
  Manual = "manual",
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
