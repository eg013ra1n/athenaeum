/**
 * Stable member → colour-tone mapping (Task 13, wave 2 "Members" design).
 * The Members tab renders the tone dot; the Overview and Exchange tabs reuse
 * this module so the same member shows the same colour everywhere on the
 * project page.
 *
 * Design tokens only (global constraints — no raw hex, ever). The tone is
 * the member's rank in the FULL member list, sorted by `displayName` then
 * `accountId` for a deterministic order that never depends on API/array
 * ordering, taken modulo the palette length so it stays stable as members
 * are added (existing members keep their tone unless the sort order shifts
 * around them — acceptable for a cosmetic dot, never load-bearing).
 */
export const MEMBER_TONES = [
  'bg-accent',
  'bg-success',
  'bg-purple',
  'bg-warning',
  'bg-orange',
  'bg-error',
  'bg-info',
  'bg-accent-muted',
];

export function memberTone(
  accountId: string,
  all: { accountId: string; displayName: string }[],
): string {
  const sorted = [...all].sort(
    (a, b) => a.displayName.localeCompare(b.displayName) || a.accountId.localeCompare(b.accountId),
  );
  const idx = sorted.findIndex((m) => m.accountId === accountId);
  if (idx === -1) return MEMBER_TONES[0];
  return MEMBER_TONES[idx % MEMBER_TONES.length];
}
