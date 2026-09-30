/** `n === 1 ? singular : plural` — the one pluralization rule shared across
 * the collab UI (defaults `plural` to `singular + 's'`). */
export function pluralize(n: number, singular: string, plural: string = `${singular}s`): string {
  return n === 1 ? singular : plural;
}

/** Human byte size, shared by the Stage-II collab exchange UI. */
export function formatBytes(n: number): string {
  if (!isFinite(n) || n < 0) return '—';
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

/** Sum of a session's elapsed seconds: `0` → `'0m'`, under a minute → `Ns`,
 * otherwise `Xh Ym` with a zero part dropped (`1h 30m`, `7h`, `45m`). */
export function formatDuration(seconds: number): string {
  if (seconds === 0) return '0m';
  if (seconds < 60) return `${Math.round(seconds)}s`;
  const totalMin = Math.round(seconds / 60);
  const h = Math.floor(totalMin / 60);
  const m = totalMin % 60;
  if (h === 0) return `${m}m`;
  if (m === 0) return `${h}h`;
  return `${h}h ${m}m`;
}

/** A transfer rate in bytes/s, human-scaled (`950 KB/s`, `31.0 MB/s`, `2.00 GB/s`). */
export function formatRate(bps: number): string {
  if (!Number.isFinite(bps) || bps < 0) return '—';
  if (bps < 1024) return `${bps} B/s`;
  if (bps < 1024 * 1024) return `${Math.round(bps / 1024)} KB/s`;
  if (bps < 1024 * 1024 * 1024) return `${(bps / (1024 * 1024)).toFixed(1)} MB/s`;
  return `${(bps / (1024 * 1024 * 1024)).toFixed(2)} GB/s`;
}

/** Coarse "how long ago" bucketing against a caller-supplied `now` (ms epoch)
 * so callers stay pure/testable — never `Date.now()` internally. */
export function formatRelative(iso: string, now: number): string {
  const t = Date.parse(iso);
  if (Number.isNaN(t)) return '—';
  const diffSec = (now - t) / 1000;
  if (diffSec < 60) return 'just now';
  if (diffSec < 3600) return `${Math.floor(diffSec / 60)} min ago`;
  if (diffSec < 86400) return `${Math.floor(diffSec / 3600)} h ago`;
  const days = Math.floor(diffSec / 86400);
  return `${days} day${days === 1 ? '' : 's'} ago`;
}
