/**
 * Frame-set footprints on the Sky Chart: the four sky corners of a field and
 * the one rule that decides whether its projected box is drawn.
 */

export type LonLat = [number, number];
export type Point = [number, number];

/** Why a footprint is not drawn; null when it is drawn. */
export type FootprintHidden = 'clip' | 'span' | 'distortion' | null;

export interface ProjectedFootprint {
  /** Projected corners in CSS px (empty when a corner does not project). */
  points: Point[];
  hidden: FootprintHidden;
}

const RAD = Math.PI / 180;

/** Inverse gnomonic: tangent-plane (xi, eta) in radians → [RA, Dec] in degrees. */
export function inverseGnomonic(xiRad: number, etaRad: number, ra0Deg: number, dec0Deg: number): LonLat {
  const ra0 = ra0Deg * RAD;
  const dec0 = dec0Deg * RAD;
  const rho = Math.hypot(xiRad, etaRad);
  if (rho < 1e-10) return [ra0Deg, dec0Deg];
  const c = Math.atan(rho);
  const sinC = Math.sin(c);
  const cosC = Math.cos(c);
  const dec = Math.asin(cosC * Math.sin(dec0) + (etaRad * sinC * Math.cos(dec0)) / rho) / RAD;
  const ra = (ra0 + Math.atan2(xiRad * sinC, rho * Math.cos(dec0) * cosC - etaRad * Math.sin(dec0) * sinC)) / RAD;
  return [ra, dec];
}

/**
 * Sky corners of a fovW × fovH field (degrees) centered on (ra, dec), rotated
 * by the position angle. The half-sizes are tangent-plane offsets (tan of the
 * half angle), so the box is exact at any declination including the poles.
 */
export function footprintCorners(ra: number, dec: number, fovW: number, fovH: number, paDeg: number): LonLat[] {
  const halfW = Math.tan((fovW / 2) * RAD);
  const halfH = Math.tan((fovH / 2) * RAD);
  const pa = paDeg * RAD;
  const offsets: Point[] = [[-halfW, -halfH], [halfW, -halfH], [halfW, halfH], [-halfW, halfH]];
  return offsets.map(([dxi, deta]) => {
    const xi = dxi * Math.cos(pa) + deta * Math.sin(pa);
    const eta = -dxi * Math.sin(pa) + deta * Math.cos(pa);
    return inverseGnomonic(xi, eta, ra, dec);
  });
}

/** A projected box wider or taller than this share of the canvas is hidden. */
export const MAX_SPAN_SHARE = 0.5;
/** A box whose projected aspect drifts beyond this factor from the true one is hidden. */
export const MAX_DISTORTION = 3;

/**
 * Projects a footprint and applies the visibility rules:
 * - `clip`: every corner is beyond the visible hemisphere, or a corner does not project;
 * - `span`: the box covers more than half the canvas (zoomed in too far);
 * - `distortion`: the projected aspect ratio is off by more than 3× (near the projection edge).
 * A box with only some corners beyond the hemisphere is drawn; the globe clip path trims it.
 */
export function projectFootprint(
  corners: LonLat[],
  project: (p: LonLat) => Point | null | undefined,
  isVisible: (p: LonLat) => boolean,
  canvas: { width: number; height: number },
  fov: { width: number; height: number; paDeg: number },
): ProjectedFootprint {
  const points: Point[] = [];
  let anyVisible = false;
  for (const c of corners) {
    const p = project(c);
    if (!p || !Number.isFinite(p[0]) || !Number.isFinite(p[1])) return { points: [], hidden: 'clip' };
    points.push([p[0], p[1]]);
    if (isVisible(c)) anyVisible = true;
  }
  if (!anyVisible) return { points, hidden: 'clip' };

  const xs = points.map((p) => p[0]);
  const ys = points.map((p) => p[1]);
  const xSpan = Math.max(...xs) - Math.min(...xs);
  const ySpan = Math.max(...ys) - Math.min(...ys);
  if (xSpan > canvas.width * MAX_SPAN_SHARE || ySpan > canvas.height * MAX_SPAN_SHARE) {
    return { points, hidden: 'span' };
  }

  const pa = Math.abs(fov.paDeg * RAD);
  const tanW = Math.tan((fov.width / 2) * RAD);
  const tanH = Math.tan((fov.height / 2) * RAD);
  const bbW = tanW * Math.abs(Math.cos(pa)) + tanH * Math.abs(Math.sin(pa));
  const bbH = tanW * Math.abs(Math.sin(pa)) + tanH * Math.abs(Math.cos(pa));
  const expected = bbW / Math.max(bbH, 1e-9);
  const actual = xSpan / Math.max(ySpan, 0.001);
  const distortion = actual / expected;
  if (distortion > MAX_DISTORTION || distortion < 1 / MAX_DISTORTION) return { points, hidden: 'distortion' };

  return { points, hidden: null };
}

export function pathData(points: Point[]): string {
  return `M${points.map((p) => `${p[0]},${p[1]}`).join(' L')} Z`;
}
