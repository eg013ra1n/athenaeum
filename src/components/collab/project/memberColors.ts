/**
 * Stable member → colour mapping (hex palette per spec §4.4; this account is
 * always accent). The Members, Overview and Exchange tabs share this module so
 * the same member shows the same colour everywhere on the project page.
 * Raw hex is allowed here by design: colours are applied via inline style.
 */

/** Spec §4.4 — the mockup's member colours, in order. */
export const MEMBER_PALETTE = [
  '#88c0d0', '#a3be8c', '#b48ead', '#ebcb8b', '#d08770', '#81a1c1', '#8fbcbb', '#bf616a',
] as const;

/**
 * A member's colour: this account (`selfAccountId`) is always the accent; the
 * others take the remaining palette in displayName-then-accountId order,
 * cycling. With no known self, everyone takes the palette from slot 0.
 */
export function memberColor(
  accountId: string,
  all: { accountId: string; displayName: string }[],
  selfAccountId: string | null,
): string {
  if (selfAccountId !== null && accountId === selfAccountId) return MEMBER_PALETTE[0];
  const others = [...all]
    .filter((m) => m.accountId !== selfAccountId)
    .sort((a, b) => a.displayName.localeCompare(b.displayName) || a.accountId.localeCompare(b.accountId));
  const idx = others.findIndex((m) => m.accountId === accountId);
  if (idx === -1) return MEMBER_PALETTE[0];
  const offset = selfAccountId !== null ? 1 : 0;
  const span = MEMBER_PALETTE.length - offset;
  return MEMBER_PALETTE[offset + (idx % span)];
}
