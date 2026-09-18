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
 *  Mirrors catalog-builder's `min_fov_for`. This mapping is fixed policy — used
 *  ONLY as a fallback when the live catalog status (`get_catalog_status`) has
 *  nothing to report (manifest unreachable AND nothing installed on disk yet),
 *  so the FOV recommendation and the tier list still work before the server
 *  manifest is reachable. Whenever live status IS available, prefer it
 *  (`tierPolicyFrom`/`buildTierRows` below) — the published manifest may not
 *  contain every density this list names (e.g. only 500/2000 today), and a
 *  recommendation for a tier the manifest doesn't have can never be satisfied. */
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

/** Minimal shape `tierPolicyFrom`/`buildTierRows` need from a catalog-status
 *  entry (`CatalogStatusInfo`, `src/types/helpers.ts`) — kept structural so
 *  this module doesn't have to import the generated type. */
export interface CatalogStatusLike {
  density: number;
  installed: boolean;
  star_count_approx: number;
  size_bytes: number;
  min_fov_deg: number;
}

/** The tiers to feed `recommendTier`/build the settings-panel table from,
 *  preferring the live manifest (via `get_catalog_status`) and falling back
 *  to the fixed `TIER_POLICY` only when the status list is empty. Both the
 *  settings panel and the "catalog missing" modal must call this — never
 *  hard-code a density or compute a recommendation against `TIER_POLICY`
 *  while live status is available, or the two surfaces can disagree (the bug
 *  this fixes) and a recommendation can name a tier the manifest doesn't
 *  actually publish. */
export function tierPolicyFrom(catalogs: CatalogStatusLike[]): TierPolicy[] {
  if (catalogs.length === 0) return TIER_POLICY;
  return catalogs.map((c) => ({ density: c.density, min_fov_deg: c.min_fov_deg }));
}

export interface TierRow {
  density: number;
  min_fov_deg: number;
  installed: boolean;
  star_count_approx: number;
  size_bytes?: number;
}

/** Per-tier rows for the settings-panel table: one row per tier the live
 *  catalog status reports (manifest tiers merged with on-disk install state —
 *  see `tier_status` in `gaia_prebuilt.rs`), falling back to `TIER_POLICY`
 *  (all "not installed", sizes unknown) only when the status list is empty. */
export function buildTierRows(catalogs: CatalogStatusLike[]): TierRow[] {
  if (catalogs.length === 0) {
    return TIER_POLICY.map((p) => ({
      density: p.density,
      min_fov_deg: p.min_fov_deg,
      installed: false,
      star_count_approx: 0,
    }));
  }
  return [...catalogs]
    .sort((a, b) => a.density - b.density)
    .map((c) => ({
      density: c.density,
      min_fov_deg: c.min_fov_deg,
      installed: c.installed,
      star_count_approx: c.star_count_approx,
      size_bytes: c.size_bytes,
    }));
}

export interface TierRecommendation {
  density: number;
  /** False when there is no FOV data to base the pick on — `density` is just
   *  the smallest known tier ("the base tier"), not a real recommendation.
   *  Callers must not say "recommended" when this is false. */
  isRecommendation: boolean;
}

/** Single source of truth for "which tier should we suggest downloading":
 *  used by both the settings panel (`recommended`/`needsDownload`) and the
 *  "catalog missing" modal's one-click download, so a click always requests
 *  exactly the tier the UI is telling the user about. With a known FOV,
 *  delegates to `recommendTier` over the live tier policy; with none (no
 *  scanned frames with usable optics yet), falls back to the smallest known
 *  tier and flags it as not a real recommendation. */
export function recommendFromFov(
  fovDeg: number | null | undefined,
  catalogs: CatalogStatusLike[],
): TierRecommendation {
  const policy = tierPolicyFrom(catalogs);
  if (fovDeg != null) {
    return { density: recommendTier(fovDeg, policy), isRecommendation: true };
  }
  const asc = [...policy].sort((a, b) => a.density - b.density);
  const base = asc[0]?.density ?? TIER_POLICY[0].density;
  return { density: base, isRecommendation: false };
}
