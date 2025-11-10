/**
 * Astronomical coordinate utilities for sky atlas operations
 */

/**
 * Calculate great circle distance between two sky positions using Haversine formula
 * @param ra1 Right Ascension of point 1 in degrees (0-360 or -180-180)
 * @param dec1 Declination of point 1 in degrees (-90 to +90)
 * @param ra2 Right Ascension of point 2 in degrees (0-360 or -180-180)
 * @param dec2 Declination of point 2 in degrees (-90 to +90)
 * @returns Angular distance in degrees
 */
export function angularDistance(
  ra1: number,
  dec1: number,
  ra2: number,
  dec2: number
): number {
  // Convert to radians
  const ra1Rad = (ra1 * Math.PI) / 180;
  const dec1Rad = (dec1 * Math.PI) / 180;
  const ra2Rad = (ra2 * Math.PI) / 180;
  const dec2Rad = (dec2 * Math.PI) / 180;

  // Haversine formula
  const dra = ra2Rad - ra1Rad;
  const ddec = dec2Rad - dec1Rad;

  const a =
    Math.sin(ddec / 2) ** 2 +
    Math.cos(dec1Rad) * Math.cos(dec2Rad) * Math.sin(dra / 2) ** 2;

  const c = 2 * Math.atan2(Math.sqrt(a), Math.sqrt(1 - a));

  // Convert back to degrees
  return (c * 180) / Math.PI;
}

/**
 * Test if a point is inside a polygon using ray casting algorithm
 * @param ra Right Ascension of test point in degrees
 * @param dec Declination of test point in degrees
 * @param vertices Polygon vertices as array of [RA, Dec] tuples in degrees
 * @returns true if point is inside polygon, false otherwise
 */
export function pointInPolygon(
  ra: number,
  dec: number,
  vertices: Array<[number, number]>
): boolean {
  if (vertices.length < 3) {
    return false;
  }

  let inside = false;
  const n = vertices.length;

  for (let i = 0; i < n; i++) {
    const j = (i + n - 1) % n;
    const [raI, decI] = vertices[i];
    const [raJ, decJ] = vertices[j];

    // Ray casting: check if ray from point crosses polygon edge
    if (
      (decI > dec) !== (decJ > dec) &&
      ra < ((raJ - raI) * (dec - decI)) / (decJ - decI) + raI
    ) {
      inside = !inside;
    }
  }

  return inside;
}

/**
 * Normalize RA to 0-360 range
 * @param ra Right Ascension in degrees
 * @returns RA normalized to 0-360 degrees
 */
export function normalizeRA(ra: number): number {
  let normalized = ra % 360;
  if (normalized < 0) {
    normalized += 360;
  }
  return normalized;
}

/**
 * Clamp declination to -90 to +90 range
 * @param dec Declination in degrees
 * @returns Clamped declination
 */
export function clampDec(dec: number): number {
  return Math.max(-90, Math.min(90, dec));
}
