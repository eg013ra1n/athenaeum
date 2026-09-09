/** View shared by the existing equatorial chart and the HiPS renderer. */
export interface HipsView {
  ra: number;
  dec: number;
  rotation: number;
  scale: number;
}

/**
 * Horizontal STG field of view in degrees for a canvas width in CSS pixels.
 * D3's bundled stereographic projection uses rho = scale * tan(theta / 2),
 * where theta is angular distance from the center. At the horizontal edge,
 * rho = width / 2 and theta = FOV / 2, hence FOV = 4 atan(width / (2 scale)).
 * Scale is the D3 projection scale in its logical pixel coordinates, not a
 * backing-store/HiDPI scale. Valid for positive finite width and scale.
 * Source: public/lib/d3.min.js, geo.stereographic (azimuthal scale function).
 */
export function stereographicFov(width: number, scale: number): number {
  if (!Number.isFinite(width) || !Number.isFinite(scale) || width <= 0 || scale <= 0) {
    throw new Error('Invalid stereographic view dimensions');
  }
  return (4 * Math.atan(width / (2 * scale)) * 180) / Math.PI;
}

export function isHipsView(value: unknown): value is HipsView {
  if (!value || typeof value !== 'object') return false;
  const view = value as HipsView;
  return (
    [view.ra, view.dec, view.rotation, view.scale].every(Number.isFinite) &&
    Math.abs(view.dec) <= 90 &&
    view.scale > 0
  );
}
