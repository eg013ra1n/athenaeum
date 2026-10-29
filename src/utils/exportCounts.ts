import type { LinkFallbackCounts } from '../types/export';

/** Under the link toggle on Windows: what link mode does there without Developer Mode. */
export const WINDOWS_LINKS_HINT =
  'Without Developer Mode, Windows makes hard links instead (or copies, across drives)';

/** "1 linked · 2 with FILTER rewritten" — only the non-zero counts (plan P11). */
export function exportCountsText(c: {
  linked: number;
  copied: number;
  rewritten: number;
  failed: number;
}): string {
  return [
    c.linked > 0 && `${c.linked} linked`,
    c.copied > 0 && `${c.copied} copied`,
    c.rewritten > 0 && `${c.rewritten} with FILTER rewritten`,
    c.failed > 0 && `${c.failed} failed`,
  ]
    .filter(Boolean)
    .join(' · ');
}

/** The first `limit` warnings, then "and N more" / "+N more" tail text. */
export function limitWarnings(
  warnings: string[],
  limit: number,
): { shown: string[]; more: number } {
  return { shown: warnings.slice(0, limit), more: Math.max(0, warnings.length - limit) };
}

/** "From zip: 12 copied" — frames that were extracted out of zips and copied (a zip frame is never linked).
 *  `null` when the export had none. */
export function zipCopiedText(zipCopied: number | null | undefined): string | null {
  return zipCopied != null && zipCopied > 0 ? `From zip: ${zipCopied} copied` : null;
}

/** "Windows did not allow symbolic links: 3 hard links, 1 copy (…)" — link-mode files Windows would not
 *  symlink (no Developer Mode), made as hard links or copies. `null` when every link was a symbolic link. */
export function linkFallbackText(lf: LinkFallbackCounts | null | undefined): string | null {
  if (!lf || lf.hardLinks + lf.copies === 0) return null;
  const made = [
    lf.hardLinks > 0 && `${lf.hardLinks} hard link${lf.hardLinks === 1 ? '' : 's'}`,
    lf.copies > 0 && `${lf.copies} ${lf.copies === 1 ? 'copy' : 'copies'}`,
  ]
    .filter(Boolean)
    .join(', ');
  return `Windows did not allow symbolic links: ${made} (turn on Developer Mode for symbolic links)`;
}
