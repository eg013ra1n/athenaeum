/**
 * The Sky Chart's view model: center, roll and horizontal field of view.
 *
 * The field of view is stored in degrees, never as d3-celestial's absolute
 * projection scale: that scale is `500 · width / 1024 · zoom` for the
 * stereographic projection, so the same number frames a different area at a
 * different window width. Stereographic maps an angular distance θ from the
 * center to ρ = scale · tan(θ / 2), so the horizontal field of view of a
 * canvas `width` CSS px wide is 4 · atan(width / (2 · scale)).
 */

export interface SkyView {
  /** Right ascension of the view center, degrees in [0, 360). */
  ra: number;
  /** Declination of the view center, degrees in [-90, 90]. */
  dec: number;
  /** Screen roll, degrees. */
  roll: number;
  /** Horizontal field of view, degrees. */
  fovDeg: number;
}

/** d3-celestial's stereographic base scale per 1024 px of width. */
export const STEREO_BASE_SCALE = 500;
/** `zoomextend` in the chart config: the max zoom over the base scale. */
export const ZOOM_EXTENT = 20;

const DEG = 180 / Math.PI;

/** Field of view at a zoom factor over the base scale; independent of width. */
function fovAtZoom(zoom: number): number {
  return 4 * Math.atan(1024 / (2 * STEREO_BASE_SCALE * zoom)) * DEG;
}

/** Narrowest view the chart reaches (max zoom), ≈ 11.7°. */
export const MIN_FOV_DEG = fovAtZoom(ZOOM_EXTENT);
/** Widest view the chart reaches (base scale), ≈ 182.7°. */
export const MAX_FOV_DEG = fovAtZoom(1);

export function scaleToFov(widthPx: number, scale: number): number {
  return 4 * Math.atan(widthPx / (2 * scale)) * DEG;
}

export function fovToScale(widthPx: number, fovDeg: number): number {
  return widthPx / (2 * Math.tan(fovDeg / DEG / 4));
}

export function clampFov(fovDeg: number): number {
  return Math.min(MAX_FOV_DEG, Math.max(MIN_FOV_DEG, fovDeg));
}

export function normalizeRa(ra: number): number {
  return ((ra % 360) + 360) % 360;
}

/**
 * The first view for a user with nothing saved: the projection circle fills
 * the width of a widescreen canvas (zoom = width / height), centered on 0h.
 */
export function defaultView(widthPx: number, heightPx: number): SkyView {
  const aspect = heightPx > 0 ? widthPx / heightPx : 1;
  return { ra: 0, dec: 0, roll: 0, fovDeg: clampFov(fovAtZoom(Math.max(1, aspect))) };
}

/** Parses a stored view; anything malformed or out of range is null. */
export function parseSkyView(raw: string | null | undefined): SkyView | null {
  if (!raw) return null;
  let v: unknown;
  try {
    v = JSON.parse(raw);
  } catch {
    return null;
  }
  if (!v || typeof v !== 'object') return null;
  const { ra, dec, roll, fovDeg } = v as Record<string, unknown>;
  if (![ra, dec, roll, fovDeg].every((n) => typeof n === 'number' && Number.isFinite(n))) return null;
  if ((dec as number) < -90 || (dec as number) > 90 || (fovDeg as number) <= 0) return null;
  return {
    ra: normalizeRa(ra as number),
    dec: dec as number,
    roll: roll as number,
    fovDeg: clampFov(fovDeg as number),
  };
}

/**
 * A view requested by a link (`/skychart?ra=…&dec=…&fov=…`). `ra` and `dec`
 * are required; `fov` is optional and keeps the current zoom when absent.
 */
export function parseViewParams(params: URLSearchParams): { ra: number; dec: number; fovDeg?: number } | null {
  if (!params.has('ra') || !params.has('dec')) return null;
  const ra = Number(params.get('ra'));
  const dec = Number(params.get('dec'));
  if (!Number.isFinite(ra) || !Number.isFinite(dec) || dec < -90 || dec > 90) return null;
  const fovRaw = params.get('fov');
  const fov = fovRaw === null ? NaN : Number(fovRaw);
  return {
    ra: normalizeRa(ra),
    dec,
    ...(Number.isFinite(fov) && fov > 0 ? { fovDeg: clampFov(fov) } : {}),
  };
}

/** True when two views differ by more than ~0.1″ / 1e-6 of the field. */
export function viewsDiffer(a: SkyView, b: SkyView): boolean {
  const eps = 3e-5;
  const dRa = Math.abs(normalizeRa(a.ra - b.ra + 180) - 180);
  return dRa > eps || Math.abs(a.dec - b.dec) > eps || Math.abs(a.roll - b.roll) > eps
    || Math.abs(a.fovDeg - b.fovDeg) > a.fovDeg * 1e-6;
}
