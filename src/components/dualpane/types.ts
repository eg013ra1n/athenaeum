// Types for the Far-Manager-style dual-pane file browser.

import type { FileWithFrame } from '../../types/models';

export type PaneId = 'left' | 'right';
export type PaneMode = 'files' | 'metadata';

export type SortDir = 'asc' | 'desc';

export interface PaneState {
  mode: PaneMode;
  cwd: string;
  selection: Set<string>;
  /** Last single-clicked path — anchor for shift-click range selection. */
  anchor: string | null;
  scrollTop: number;
  /** Free-text filter restricted to the current folder; ephemeral (clears on cwd change). */
  filterText: string;
  /** Sort direction for the file/folder list (sorts by basename). */
  sortDir: SortDir;
}

export interface DirectoryListing {
  subdirectories: string[];
  files: FileWithFrame[];
}

/** Hit returned by `search_catalog`. */
export interface CatalogSearchHit {
  fileId: number;
  path: string;
  filename: string;
  object: string | null;
  filter: string | null;
  imagetyp: string | null;
  instrume: string | null;
  telescop: string | null;
  dateObs: string | null;
}

/** Originally-scanned header values for a frame, decoded back from
 *  fits_header.header. Used by the metadata pane to show "what the file
 *  said before you edited it" and offer per-field revert. */
export interface FrameOriginalSnapshot {
  frameId: number;
  object: string | null;
  filter: string | null;
  imagetyp: string | null;
  instrume: string | null;
  telescop: string | null;
  focallen: number | null;
  gain: number | null;
  offset: number | null;
  binning: string | null;
  exptime: number | null;
  ccdTemp: number | null;
  dateObs: string | null;
  /** RA in decimal degrees, decoded from OBJCTRA (sexagesimal) or RA
   *  (decimal) in the FITS header. The metadata pane shows it next to the
   *  catalog value so the user can revert a plate-solved value back to
   *  whatever the file shipped with. */
  ra: number | null;
  /** DEC in decimal degrees, decoded from OBJCTDEC or DEC in the header. */
  dec: number | null;
  /** Image position angle in degrees (north through east), from CROTA2 or
   *  OBJCTROT in the FITS header. Plate solving overrides this. */
  rotation: number | null;
}

/** One frame_set ("Object") the selection is part of. */
export interface FrameSetMembership {
  frameSetId: number;
  name: string | null;
  selectedCount: number;
}

/** One calibration_set the selection is either a member of or consumes. */
export interface CalibrationSetMembership {
  calibrationSetId: number;
  imagetyp: string;
  /** Set when this entry is from the consumer side (cstf.calibration_type). */
  calibrationType: string | null;
  instrume: string | null;
  gain: number | null;
  binning: string | null;
  exptime: number | null;
  filter: string | null;
  ccdTemp: number | null;
  date: string;
  selectedCount: number;
}

/** When the selection includes calibration frames, the calibration sets they
 *  belong to are referenced by other frames (LIGHTs). Those LIGHTs roll up
 *  to a frame_set ("Object") — this entry says "the calibration data goes
 *  into <Object>'s <calibration_type>". */
export interface UsedInFrameSetMembership {
  frameSetId: number;
  name: string | null;
  calibrationType: string;
  usedForFrameCount: number;
}

export interface FrameMembershipsSummary {
  frameSets: FrameSetMembership[];
  calibrationSetMember: CalibrationSetMembership[];
  calibrationSetConsumer: CalibrationSetMembership[];
  usedInFrameSets: UsedInFrameSetMembership[];
  frameCountTotal: number;
  frameCountUnaffiliated: number;
}

/** Counts of relations that would be removed when bulk_update_frame_metadata
 *  is called for a set of frames — drives the unlink warning dialog. */
export interface FrameMetadataRelations {
  calibrationSetMemberCount: number;
  calibrationSetConsumerCount: number;
  sessionMemberCount: number;
  affectedFrameCount: number;
}

/** Summary returned by `list_unfinished_file_operations`. */
export interface FileOperationSummary {
  id: number;
  kind: string;        // 'move' | 'delete'
  status: string;      // 'pending' | 'running' | ...
  sourceRoot: string | null;
  destDir: string | null;
  totalFiles: number;
  totalBytes: number;
  createdAt: string;
  startedAt: string | null;
}

/** Payload of the `file-op-progress` event. */
export interface FileOpProgressPayload {
  operationId: number;
  kind: string;        // 'move' | 'delete'
  stage: string;       // 'copy' | 'verify' | 'commit_move' | ...
  current: number;
  total: number;
  message: string;
}

/** Payload of the `file-op-finished` event. */
export interface FileOpFinishedPayload {
  operation_id: number;
  outcome: 'completed' | 'cancelled' | 'failed';
  kind: string;
}

/** Split a path on / or \ separators. */
export function splitPath(p: string): string[] {
  return p.split(/[/\\]/).filter(Boolean);
}

/** Parent of a path, preserving the dominant separator. */
export function getParentPath(p: string): string {
  const sep = p.includes('\\') ? '\\' : '/';
  const parts = p.split(/[/\\]/);
  parts.pop();
  // Re-add leading slash for absolute Unix paths.
  if (sep === '/' && p.startsWith('/')) {
    return '/' + parts.filter(Boolean).join('/');
  }
  return parts.join(sep);
}

/** Last component of a path. */
export function getBasename(p: string): string {
  const parts = p.split(/[/\\]/).filter(Boolean);
  return parts[parts.length - 1] || p;
}

/** Join a directory and a child name with whichever separator the dir uses. */
export function joinPath(dir: string, child: string): string {
  if (!dir) return child;
  const sep = dir.includes('\\') ? '\\' : '/';
  if (dir.endsWith(sep)) return dir + child;
  return dir + sep + child;
}

/**
 * Return the longest scan-root path that is an ancestor (or equal) to `path`,
 * or null if `path` is not inside any scan root. Browsing is clamped at this
 * boundary — the "Up" button and breadcrumb stop here. Browsing *deeper* into
 * subdirectories is always allowed.
 */
export function findEnclosingScanRoot(path: string, scanRootPaths: string[]): string | null {
  if (!path) return null;
  // Pick the LONGEST root that's a prefix of `path` — covers nested roots
  // (uncommon, but better defined behavior).
  let best: string | null = null;
  for (const root of scanRootPaths) {
    if (!root) continue;
    if (path === root || path.startsWith(root.endsWith('/') || root.endsWith('\\') ? root : root + (root.includes('\\') ? '\\' : '/'))) {
      if (best === null || root.length > best.length) best = root;
    }
  }
  return best;
}

/** Sentinel path for the synthetic "parent folder" row at the top of each
 *  pane. The click/keyboard handler maps this back to the parent of the
 *  current cwd (clamped to the enclosing scan root). Never appears in
 *  selection state passed to the backend. */
export const PARENT_ROW = '__parent__';

/**
 * Compute the parent of `cwd` clamped to the enclosing scan root.
 * Returns null when `cwd` is at or above the root, or unrooted.
 */
export function getClampedParent(cwd: string, scanRootPaths: string[]): string | null {
  const root = findEnclosingScanRoot(cwd, scanRootPaths);
  if (!root || cwd === root) return null;
  const parent = getParentPath(cwd);
  if (!parent || parent === cwd) return null;
  const parentRoot = findEnclosingScanRoot(parent, scanRootPaths);
  if (parentRoot !== root) return null;
  return parent;
}

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}
