// Static example rigs for the FOV helper. Users can override every field.
export interface CameraPreset {
  label: string;
  pixelUm: number;   // sensor pixel pitch, micrometers
  widthPx: number;
  heightPx: number;
}

export const CAMERA_PRESETS: CameraPreset[] = [
  { label: 'ASI2600 (IMX571, 3.76µm 6248×4176)', pixelUm: 3.76, widthPx: 6248, heightPx: 4176 },
  { label: 'ASI1600 (4.63µm 4656×3520)',          pixelUm: 4.63, widthPx: 4656, heightPx: 3520 },
  { label: 'ASI294 (4.63µm 4144×2822)',           pixelUm: 4.63, widthPx: 4144, heightPx: 2822 },
  { label: 'DSLR APS-C (3.9µm 6000×4000)',        pixelUm: 3.9,  widthPx: 6000, heightPx: 4000 },
];

/** Pixel scale in arcsec/px: 206.265 · pixelUm · binning / focalMm. */
export function pixelScaleArcsec(pixelUm: number, focalMm: number, binning: number): number {
  if (focalMm <= 0) return 0;
  return (206.265 * pixelUm * binning) / focalMm;
}

/** Field of view (long axis) in degrees. */
export function fovDeg(pixelScaleArcsec: number, widthPx: number, heightPx: number): number {
  return (pixelScaleArcsec * Math.max(widthPx, heightPx)) / 3600;
}

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
