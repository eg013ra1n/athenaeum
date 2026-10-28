// Small pure helpers for how zip-resident frames are shown outside the file browser: the chip on a
// frame set, the "From zip" extraction line in batch dialogs, and which rows belong to a foreign zip.

import type { ExtractionEstimate } from '../types/archive';

/** Label of the zip chip on a frame set / group: `'zip'` when every member is in a zip, `'2 of 5 in zip'`
 *  when some are, `null` when none are (no chip). */
export function zipChipLabel(zipCount: number, total: number): string | null {
  if (zipCount <= 0) return null;
  if (zipCount >= total) return 'zip';
  return `${zipCount} of ${total} in zip`;
}

/** The "From zip" line of a batch dialog, or `null` when nothing would be extracted. */
export function extractionLine(estimate: Pick<ExtractionEstimate, 'files' | 'bytes'> | null | undefined): string | null {
  if (!estimate || estimate.files <= 0) return null;
  const files = `${estimate.files.toLocaleString('en-US')} file${estimate.files === 1 ? '' : 's'}`;
  const size = estimate.bytes >= 1e8
    ? `${(estimate.bytes / 1e9).toFixed(1)} GB`
    : `${Math.max(1, Math.round(estimate.bytes / 1e6))} MB`;
  return `From zip: ${files}, ${size}`;
}

/** What the export dialog adds under the symbolic-links choice: a frame in a zip has no file to link to. */
export const ZIP_COPIED_NOT_LINKED = 'Zip-resident frames are copied, not linked';

/** The row fields every zip-aware surface reads. */
export interface ZipMarkers {
  archive_zip_path?: string | null;
  archive_path_in_zip?: string | null;
  archived_in_operation?: number | null;
}

/** True when the file lives inside a zip (ours or foreign). */
export function isInZip(f: ZipMarkers): boolean {
  return f.archive_zip_path != null && f.archive_zip_path !== '';
}

/** True when the file lives inside a zip Athenaeum did not write: it can be unpacked, and Unarchive does
 *  not know it. A frame Move and ZIP archived carries `archived_in_operation`. */
export function isForeignZipFile(f: ZipMarkers): boolean {
  return isInZip(f) && f.archived_in_operation == null;
}

/** `{ zipPath, innerDir }` of the entry a row occupies, for opening the zip at the entry's folder.
 *  `null` for a disk file. `innerDir` has `/` separators and no leading or trailing `/`. */
export function zipLocationOf(f: ZipMarkers): { zipPath: string; innerDir: string } | null {
  if (!isInZip(f) || !f.archive_path_in_zip) return null;
  const parts = f.archive_path_in_zip.split(/[/\\]/).filter(Boolean);
  parts.pop();
  return { zipPath: f.archive_zip_path as string, innerDir: parts.join('/') };
}

/** The state `navigate('/files', { state: { reveal } })` takes for a row: its path, plus the zip and inner
 *  folder to open when the row lives in one. Our own zipped rows keep their ORIGINAL disk path in
 *  `files.path`, so only the zip markers say where the bytes are. */
export function revealRequest(
  path: string,
  markers: ZipMarkers,
): { path: string; token: number; zip?: { zipPath: string; innerDir: string } } {
  const zip = zipLocationOf(markers);
  return zip ? { path, zip, token: Date.now() } : { path, token: Date.now() };
}
