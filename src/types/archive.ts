// AUTO-GENERATED from Rust by athenaeum-core/src/ts_export.rs — do not edit.
// Regenerate: TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract

export type ArchiveDisposition = "move" | "copy" | "skip";

export type ArchiveCompression = "store" | "deflate";

export type ConflictResolution = "overwrite" | "add_suffix";

export type FrameRole = "light" | "flat" | "dark" | "bias" | "darkflat";

export type Dispositions = { flats: ArchiveDisposition | null, darks: ArchiveDisposition | null, bias: ArchiveDisposition | null, darkflats: ArchiveDisposition | null, };

export type ArchiveOperationFile = { id: number, operation_id: number, file_id: number | null, source_path: string, target_zip_path: string, target_path_in_zip: string, expected_hash: string, disposition: string, frame_role: string, file_size_bytes: number, };

export type PlannedZip = { zip_path: string, zip_filename: string, frame_role: FrameRole, file_count: number, total_size_bytes: number, };

export type SharedCalibrationWarning = { frame_role: FrameRole, calibration_set_id: number, other_frames_set_ids: Array<number>, };

export type ZipFilenameConflict = { zip_path: string, zip_filename: string, };

export type ArchivePlan = { frames_set_id: number, 
/**
 * `Some(id)` for a calibration-set archive-of-originals plan; `None` for
 * the original frame-set plan shape. Mutually exclusive with a real
 * (non-zero) `frames_set_id` — see the struct doc comment.
 */
calibration_set_id: number | null, archive_root_path: string, dispositions: Dispositions, compression: ArchiveCompression, files: Array<ArchiveOperationFile>, zips: Array<PlannedZip>, shared_calibrations: Array<SharedCalibrationWarning>, conflicts: Array<ZipFilenameConflict>, total_size_bytes: number, };

export type ArchiveOperationSummary = { id: number, frames_set_id: number | null, frame_set_name: string | null, status: string, started_at: string, finished_at: string | null, error_message: string | null, };

