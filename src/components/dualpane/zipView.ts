// Pure helpers for the browser's zip view: where a pane is inside a zip, how the breadcrumb and the
// parent row read, and how a virtual path (`<zip path>/<entry path>`) maps back to a zip and an inner folder.
//
// A zip is one whole file in the browser. Opening it lists one level of its entries (`list_zip_entries`);
// nothing inside is ever renamed, moved or deleted — only the whole zip can be.

import type { ZipListItem } from '../../types/archive';
import { getBasename } from './types';

/** Where a pane is inside a zip. `innerDir` is `/`-separated with no leading or trailing `/` (`''` is the top). */
export interface ZipLocation {
  path: string;
  innerDir: string;
}

/** The status-line reason shown while the active pane is inside a zip. */
export const ZIP_READ_ONLY_NOTE = 'Inside a zip — only the whole zip can be moved or removed';

/** Shown when a Move would land in a pane that is inside a zip. */
export const ZIP_DEST_NOTE = 'The other pane is inside a zip — open a folder there to move files into it';

/** The inner folder one level up, or `null` when `innerDir` is already the top of the zip. */
export function innerParent(innerDir: string): string | null {
  if (!innerDir) return null;
  const i = innerDir.lastIndexOf('/');
  return i < 0 ? '' : innerDir.slice(0, i);
}

/** Breadcrumb entries for a zip location: the zip itself, then one per inner folder. */
export function zipCrumbs(zip: ZipLocation): { name: string; innerDir: string }[] {
  const crumbs = [{ name: getBasename(zip.path), innerDir: '' }];
  if (!zip.innerDir) return crumbs;
  const parts = zip.innerDir.split('/');
  parts.forEach((name, i) => crumbs.push({ name, innerDir: parts.slice(0, i + 1).join('/') }));
  return crumbs;
}

/** `<zip name>/<inner folder>`, the one-line form of the location. */
export function zipLocationLabel(zip: ZipLocation): string {
  return zip.innerDir ? `${getBasename(zip.path)}/${zip.innerDir}` : getBasename(zip.path);
}

/** Label of the `..` row: where one level up lands (`folder` is the disk folder holding the zip). */
export function zipUpLabel(zip: ZipLocation, folder: string): string {
  const parent = innerParent(zip.innerDir);
  if (parent === null) return folder;
  return parent === '' ? getBasename(zip.path) : `${getBasename(zip.path)}/${parent}`;
}

/** A virtual path split into the zip it lives in and the folder holding the entry. `null` when the path has
 *  no `.zip` component followed by an entry. The caller must confirm the zip really exists: a real folder
 *  named `x.zip` looks the same. */
export function findZipInPath(path: string): { zipPath: string; innerDir: string } | null {
  const m = /^(.*?\.zip)[/\\](.+)$/i.exec(path);
  if (!m) return null;
  const entry = m[2].split(/[/\\]/).filter(Boolean);
  if (entry.length === 0) return null;
  entry.pop();
  return { zipPath: m[1], innerDir: entry.join('/') };
}

/** Frame-count line of a zip row. */
export function zipFramesLabel(frameEntries: number): string {
  return `${frameEntries.toLocaleString()} frame${frameEntries === 1 ? '' : 's'}`;
}

/** What the Black Hole confirmation says about the zips it is about to move. */
export function zipRemovalSummary(zips: ZipListItem[]): { frames: number; onlyCopies: number } {
  return {
    frames: zips.reduce((sum, z) => sum + z.frameEntries, 0),
    onlyCopies: zips.filter((z) => z.createdByOperation != null).length,
  };
}
