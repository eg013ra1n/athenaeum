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
  content_hash: string;
  duplicate_group_id: number | null;
  created_at: string; // ISO 8601 datetime
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
  focal_length: number | null;
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
  last_scan: string | null; // ISO 8601 datetime
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
