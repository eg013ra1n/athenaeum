// Display helpers shared by the Star Catalog tiers table and the Quad Index
// block of the plate-solve settings (both show a byte size and an elapsed
// download timer in the same style).

// Decimal units (1 MB = 10^6 bytes), the same the download progress lines use.
export function formatSize(bytes: number): string {
  if (bytes === 0) return '—';
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(1)} GB`;
  if (bytes >= 1e6) return `${(bytes / 1e6).toFixed(0)} MB`;
  return `${Math.round(bytes / 1024)} KB`;
}

export function formatElapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${String(sec).padStart(2, '0')}s`;
  return `${sec}s`;
}
