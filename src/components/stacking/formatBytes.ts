// Byte-count formatter for the Stacking tab (M3 Task 6). Lifted out of
// `ResultsPanel.tsx`, which had it as a private function — the Drizzle
// panel's estimate line needs the exact same formatting, so this is now the
// one shared implementation both import instead of a second hand copy.

export function formatBytes(n: number): string {
  if (n < 1024) return `${n} B`;
  const kb = n / 1024;
  if (kb < 1024) return `${kb.toFixed(1)} KB`;
  const mb = kb / 1024;
  if (mb < 1024) return `${mb.toFixed(1)} MB`;
  const gb = mb / 1024;
  return `${gb.toFixed(2)} GB`;
}
