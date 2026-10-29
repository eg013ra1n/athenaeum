// AUTO-GENERATED from Rust by athenaeum-core/src/ts_export.rs — do not edit.
// Regenerate: TS_RS_WRITE=1 cargo test -p athenaeum-core --test ts_contract

import type { FileWithFrame } from './models';

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

export type ZipCacheStatus = { 
/**
 * Bytes on disk under the cache directory.
 */
bytes: number, 
/**
 * Cached entries (key directories holding a file).
 */
entries: number, 
/**
 * The configured size limit.
 */
limitBytes: number, };

export type ExtractionEstimate = { files: number, bytes: number, };

export type UnpackTarget = { "kind": "frameSet", id: number, } | { "kind": "calibrationSet", id: number, };

export type UnpackOutcome = { 
/**
 * Entries extracted to the target folder.
 */
unpacked: number, 
/**
 * Frames whose identical file was already at the target; nothing was written.
 */
adopted: number, conflicts: Array<string>, };

export type ZipListItem = { path: string, size: number, 
/**
 * Frame entries the catalog holds for this zip.
 */
frameEntries: number, 
/**
 * The Move and ZIP operation that wrote this zip, when it is one of ours (the browser warns that
 * removing it removes the only copy of an archived frame set).
 */
createdByOperation: number | null, 
/**
 * The zip is in the Black Hole (owner ruling A: it stays listed and readable, but its frames are not
 * used). The browser marks the row so a second F8 is not mistaken for a new move.
 */
inBlackHole: boolean, };

export type ZipDirListing = { 
/**
 * Sub-folders of the listed folder, each as the full entry path of the folder (`/`-separated, no leading
 * or trailing `/`): pass one back as the inner folder to open it.
 */
subdirectories: Array<string>, 
/**
 * Cataloged frames at this level, with their frame metadata.
 */
files: Array<FileWithFrame>, 
/**
 * Entries at this level the catalog does not hold as frames (other files, names the scan skipped,
 * names that are unsafe to unpack). They are listed by name and size only; nothing opens them.
 */
otherEntries: Array<ZipOtherEntry>, };

export type ZipOtherEntry = { 
/**
 * The name at this level (for an unsafe name that has no level, the whole stored name, at the top).
 */
name: string, size: number, 
/**
 * Where this entry was unpacked or restored to (its release in the catalog), when the catalog knows.
 */
releasedTo: string | null, 
/**
 * `released_to` exists on disk now. `false` for an entry that was not released. When the copy is gone
 * the next scan of the zip takes its catalog row back into the zip.
 */
releasedPresent: boolean, };

