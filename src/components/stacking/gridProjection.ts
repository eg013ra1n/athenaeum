// The canonical grid's gnomonic projection, in TypeScript, for the schematic only (wave 6c, D11). It mirrors
// `collab::grid_geometry::grid_wcs` + `WcsSolution::sky_to_pixel` (no SIP: the grid has none), 0-based pixels, and
// is pinned to Task 1b's golden file to 1e-6 px. Nothing here decides what a run does: the Rust side owns that.

import type { CanonicalGrid } from '../../types/models';

/** A position in the grid's 0-based pixel frame (the frame is BOTTOM-UP: y grows toward north at rotation 0). */
export interface GridPixel {
  x: number;
  y: number;
}

const RAD = Math.PI / 180;

/** `cos c` (c: the angle between the point and the grid centre) at or below this is "90 degrees or more away".
 *  Zero would be the rule, but at exactly 90 degrees the float result is ~6e-17, and the projection of that is a
 *  coordinate of 1e16 px; a threshold at float noise keeps such a point out the same way. */
const COS_C_MIN = 1e-9;

/** The sky position's pixel in the grid, or `null` when it is 90 degrees or more from the grid centre.
 *
 *  The TAN of the Rust side returns finite, MIRRORED values for a point behind the plane (`solvemyastro`
 *  `wcs.rs`, `sky_to_tangent` divides by a negative `cos c`), and a mirrored outline drawn as a real one is
 *  worse than no outline, so the rule lives here: `null` for `cos c <= 0`.
 *
 *  The grid's WCS (`grid_wcs`): CRPIX the 0-based centre `((W-1)/2, (H-1)/2)`, CD in the FITS CROTA2 form with
 *  `CDELT1 = -s` (east left) and `CDELT2 = +s`. */
export function skyToGridPixel(raDeg: number, decDeg: number, grid: CanonicalGrid): GridPixel | null {
  const s = grid.scaleArcsec / 3600;
  const rot = grid.rotationDeg * RAD;
  const sin = Math.sin(rot);
  const cos = Math.cos(rot);
  const cd00 = -s * cos;
  const cd01 = -s * sin;
  const cd10 = -s * sin;
  const cd11 = s * cos;

  const ra = raDeg * RAD;
  const dec = decDeg * RAD;
  const ra0 = grid.crval1 * RAD;
  const dec0 = grid.crval2 * RAD;
  const dRa = ra - ra0;
  const cosC = Math.sin(dec0) * Math.sin(dec) + Math.cos(dec0) * Math.cos(dec) * Math.cos(dRa);
  if (!(cosC > COS_C_MIN)) return null;

  const xi = ((Math.cos(dec) * Math.sin(dRa)) / cosC) / RAD;
  const eta = ((Math.cos(dec0) * Math.sin(dec) - Math.sin(dec0) * Math.cos(dec) * Math.cos(dRa)) / cosC) / RAD;
  const det = cd00 * cd11 - cd01 * cd10;
  if (!(Math.abs(det) >= 1e-30)) return null;
  const u = (cd11 * xi - cd01 * eta) / det;
  const v = (-cd10 * xi + cd00 * eta) / det;
  const x = u + (grid.widthPx - 1) / 2;
  const y = v + (grid.heightPx - 1) / 2;
  return Number.isFinite(x) && Number.isFinite(y) ? { x, y } : null;
}

/** A grid pixel on the schematic: drawn grid-up, as the master will look. SVG x is the grid x, SVG y is
 *  `heightPx - y` (the grid is BOTTOM-UP; east is left because the grid's CDELT1 is negative). */
export function toSchematic(p: GridPixel, grid: CanonicalGrid): GridPixel {
  return { x: p.x, y: grid.heightPx - p.y };
}

/** How far north the `N` arrow looks, in degrees of sky. */
const NORTH_PROBE_DEG = 0.1;

/** The direction of north on the schematic, in degrees clockwise from straight up: `0` at rotation 0, `-30` at
 *  rotation 30 (the north end leans toward east, i.e. left). Measured along the projection of a point 0.1 degrees
 *  north of the grid centre; within 0.1 degrees of the pole it looks south of the centre instead and reverses. */
export function northArrowAngleDeg(grid: CanonicalGrid): number {
  const centre = skyToGridPixel(grid.crval1, grid.crval2, grid);
  const towardNorth = grid.crval2 + NORTH_PROBE_DEG <= 90;
  const probeDec = towardNorth ? grid.crval2 + NORTH_PROBE_DEG : grid.crval2 - NORTH_PROBE_DEG;
  const probe = skyToGridPixel(grid.crval1, probeDec, grid);
  if (!centre || !probe) return 0;
  const sign = towardNorth ? 1 : -1;
  const dx = sign * (probe.x - centre.x);
  const upward = sign * (probe.y - centre.y); // grid y grows upward on the schematic
  return Math.atan2(dx, upward) / RAD;
}

// ── How a grid reads ────────────────────────────────────────────────────

/** `n` rounded to `digits` decimals, trailing zeros dropped (`2.00` reads `2`), `—` for a non-finite one. */
export function trimmed(n: number, digits: number): string {
  return Number.isFinite(n) ? String(Number(n.toFixed(digits))) : '—';
}

/** `n` with exactly `digits` decimals (`2.00` stays `2.00`), `—` for a non-finite one; a value that rounds to zero
 *  carries no sign (`-0.0000` reads `0.0000`). */
export function fixed(n: number, digits: number): string {
  if (!Number.isFinite(n)) return '—';
  const text = n.toFixed(digits);
  return /^-0(\.0+)?$/.test(text) ? text.slice(1) : text;
}

/** The grid's field of view as `<w>° × <h>°` (`widthPx · scale / 3600` and the same for the height). */
export function fieldOfView(grid: CanonicalGrid): string {
  const deg = (px: number) => trimmed((px * grid.scaleArcsec) / 3600, 2);
  return `${deg(grid.widthPx)}° × ${deg(grid.heightPx)}°`;
}

/** The grid's values on one line: scale, size and field of view, centre, rotation. The scale reads 2 decimals and the
 *  centre RA and Dec 4 (D6); the size, the field of view and the rotation drop their trailing zeros. */
export function gridValuesLine(grid: CanonicalGrid): string {
  return (
    `${fixed(grid.scaleArcsec, 2)}″/px · ${trimmed(grid.widthPx, 0)} × ${trimmed(grid.heightPx, 0)} px ` +
    `(${fieldOfView(grid)}) · centre RA ${fixed(grid.crval1, 4)}° Dec ${fixed(grid.crval2, 4)}° · ` +
    `rotation ${trimmed(grid.rotationDeg, 2)}°`
  );
}
