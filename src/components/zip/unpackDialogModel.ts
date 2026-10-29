// Pure helpers behind the "Unpack from zip" dialog: the default target folder and the wording of the result.

import type { UnpackOutcome } from '../../types/archive';

/** Folder name of a zip without its extension (`/a/Night 1.zip` -> `Night 1`). */
export function zipStem(zipPath: string): string {
  const base = zipPath.split(/[/\\]/).filter(Boolean).pop() ?? '';
  return base.replace(/\.zip$/i, '');
}

/** The folder holding a zip (`/a/b/Night.zip` -> `/a/b`), or `''` for a bare name. */
export function zipParent(zipPath: string): string {
  const i = Math.max(zipPath.lastIndexOf('/'), zipPath.lastIndexOf('\\'));
  return i <= 0 ? '' : zipPath.slice(0, i);
}

/** Default target: a folder named like the zip, next to it (`/a/b/Night.zip` -> `/a/b/Night`). */
export function defaultUnpackDir(zipPath: string): string {
  const parent = zipParent(zipPath);
  const stem = zipStem(zipPath);
  if (!parent) return stem;
  const sep = parent.includes('\\') && !parent.includes('/') ? '\\' : '/';
  return `${parent}${sep}${stem}`;
}

/** The zip most of the given rows live in (ties: the first seen). Its location names the default target. */
export function mainZip(zipPaths: readonly string[]): string | null {
  const counts = new Map<string, number>();
  for (const p of zipPaths) counts.set(p, (counts.get(p) ?? 0) + 1);
  let best: string | null = null;
  let bestCount = 0;
  for (const [p, n] of counts) {
    if (n > bestCount) { best = p; bestCount = n; }
  }
  return best;
}

/** Notification title and detail for a finished unpack. */
export function unpackSummary(o: { unpacked: number; adopted: number; conflicts: number }, outcome: string): { title: string; detail?: string } {
  if (outcome === 'failed') return { title: 'Unpack failed' };
  if (outcome === 'cancelled') return { title: 'Unpack cancelled', detail: `${o.unpacked} frame${o.unpacked === 1 ? '' : 's'} were already unpacked` };
  const parts = [`${o.unpacked} unpacked`];
  if (o.adopted > 0) parts.push(`${o.adopted} already in place`);
  if (o.conflicts > 0) {
    return { title: `Unpack finished with ${o.conflicts} frame${o.conflicts === 1 ? '' : 's'} left in the zip`, detail: parts.join(', ') };
  }
  return { title: `Unpacked ${o.unpacked + o.adopted} frame${o.unpacked + o.adopted === 1 ? '' : 's'}`, detail: o.adopted > 0 ? parts.join(', ') : undefined };
}

/** One line for the dialog after the run (counts only; the conflicts are listed beside it). */
export function unpackResultLine(o: UnpackOutcome): string {
  const parts = [`${o.unpacked} unpacked`];
  if (o.adopted > 0) parts.push(`${o.adopted} already in place`);
  if (o.conflicts.length > 0) parts.push(`${o.conflicts.length} left in the zip`);
  return parts.join(', ');
}
