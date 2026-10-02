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
