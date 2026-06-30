/**
 * Recommended target density: the smallest tier whose `min_fov_deg <= fov`
 * (deeper tiers support smaller fields). Falls back to the deepest tier when the
 * field is smaller than every tier's `min_fov_deg`.
 */
export function recommendTier(
  fov: number,
  tiers: { density: number; min_fov_deg: number }[],
): number {
  const asc = [...tiers].sort((a, b) => a.density - b.density);
  const hit = asc.find((t) => t.min_fov_deg <= fov);
  return (hit ?? asc[asc.length - 1])?.density ?? 2000;
}

/** Canonical density tiers + the smallest FOV (°) each cumulative depth supports.
 *  Mirrors catalog-builder's `min_fov_for`. This mapping is fixed policy, so the
 *  FOV recommendation and the tier list work even before the server manifest
 *  (which carries authoritative byte sizes) is reachable. */
export interface TierPolicy {
  density: number;
  min_fov_deg: number;
}

export const TIER_POLICY: TierPolicy[] = [
  { density: 500, min_fov_deg: 0.6 },
  { density: 2000, min_fov_deg: 0.3 },
  { density: 5000, min_fov_deg: 0.2 },
  { density: 8000, min_fov_deg: 0.15 },
];
