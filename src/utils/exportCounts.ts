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
