// Mirrors crates/athenaeum-core/src/archive/models.rs

export type ArchiveDisposition = 'move' | 'copy' | 'skip';
export type ArchiveCompression = 'store' | 'deflate';
export type ConflictResolution = 'overwrite' | 'add_suffix';
export type FrameRole = 'light' | 'flat' | 'dark' | 'bias' | 'darkflat';

export interface Dispositions {
  flats: ArchiveDisposition | null;
  darks: ArchiveDisposition | null;
  bias: ArchiveDisposition | null;
  darkflats: ArchiveDisposition | null;
}

export interface ArchiveSettings {
  rootPath: string | null;
  compression: ArchiveCompression;
}

export interface ArchiveOperationFile {
  id: number;
  operation_id: number;
  file_id: number | null;
  source_path: string;
  target_zip_path: string;
  target_path_in_zip: string;
  expected_hash: string;
  disposition: string;
  frame_role: string;
  file_size_bytes: number;
}

export interface PlannedZip {
  zip_path: string;
  zip_filename: string;
  frame_role: FrameRole;
  file_count: number;
  total_size_bytes: number;
}

export interface SharedCalibrationWarning {
  frame_role: FrameRole;
  calibration_set_id: number;
  other_frames_set_ids: number[];
}

export interface ZipFilenameConflict {
  zip_path: string;
  zip_filename: string;
}

export interface ArchivePlan {
  frames_set_id: number;
  archive_root_path: string;
  dispositions: Dispositions;
  compression: ArchiveCompression;
  files: ArchiveOperationFile[];
  zips: PlannedZip[];
  shared_calibrations: SharedCalibrationWarning[];
  conflicts: ZipFilenameConflict[];
  total_size_bytes: number;
}

export interface ArchiveOperationSummary {
  id: number;
  frames_set_id: number;
  frame_set_name: string | null;
  status: string;
  started_at: string;
  finished_at: string | null;
  error_message: string | null;
}

export interface ArchivedFrameSetSummary {
  frames_set_id: number;
  name: string | null;
  archived_at: string | null;
  operation_id: number | null;
  archive_root_path: string | null;
  started_at: string | null;
  lights_count: number;
  flats_count: number;
  darks_count: number;
  bias_count: number;
  darkflats_count: number;
}

export interface ArchiveProgressEvent {
  operation_id: number;
  stage: string;
  current: number;
  total: number;
  message: string;
}

export interface ArchiveRoot {
  id: number;
  path: string;
  label: string | null;
  is_default: boolean;
}
