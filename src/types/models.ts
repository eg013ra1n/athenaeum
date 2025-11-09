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
  date_obs: string | null;
  objctra: string | null;
  objctdec: string | null;
  total_exp_time: number | null;
  project_id: number | null;
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
