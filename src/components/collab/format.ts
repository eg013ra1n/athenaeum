/** Human byte size, shared by the Stage-II collab exchange UI. */
export function formatBytes(n: number): string {
  if (!isFinite(n) || n < 0) return '—';
  if (n < 1024) return `${n} B`;
  if (n < 1024 * 1024) return `${(n / 1024).toFixed(1)} KB`;
  if (n < 1024 * 1024 * 1024) return `${(n / 1024 / 1024).toFixed(1)} MB`;
  return `${(n / 1024 / 1024 / 1024).toFixed(2)} GB`;
}

/** Always-GB size, for the loss-guard's "N frames missing (M GB)" wording —
 *  shared by `ReceiveTab`'s inline banner and `useCollabNotifications`'s
 *  toast so the two read identically. */
export function formatGb(bytes: number): string {
  return `${(bytes / 1024 / 1024 / 1024).toFixed(2)} GB`;
}
