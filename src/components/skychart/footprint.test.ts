import { describe, expect, it } from 'vitest';
import { LonLat, Point, footprintCorners, inverseGnomonic, pathData, projectFootprint } from './footprint';

const DEG = Math.PI / 180;

/** Angular distance in degrees. */
function dist(a: LonLat, b: LonLat): number {
  const [l1, p1] = [a[0] * DEG, a[1] * DEG];
  const [l2, p2] = [b[0] * DEG, b[1] * DEG];
  const s = Math.sin((p2 - p1) / 2) ** 2 + Math.cos(p1) * Math.cos(p2) * Math.sin((l2 - l1) / 2) ** 2;
  return (2 * Math.asin(Math.sqrt(s))) / DEG;
}

describe('footprintCorners', () => {
  it('puts the corners of a 2° × 1° field at the right distance from its center', () => {
    const corners = footprintCorners(83.8, -5.4, 2, 1, 0);
    // tangent-plane half-diagonal: atan(hypot(tan 1°, tan 0.5°))
    const expected = Math.atan(Math.hypot(Math.tan(1 * DEG), Math.tan(0.5 * DEG))) / DEG;
    for (const c of corners) expect(dist(c, [83.8, -5.4])).toBeCloseTo(expected, 9);
  });

  it('stays exact at the pole', () => {
    const corners = footprintCorners(0, 89.9, 1, 1, 30);
    for (const c of corners) {
      expect(Number.isFinite(c[0]) && Number.isFinite(c[1])).toBe(true);
      expect(c[1]).toBeLessThanOrEqual(90);
    }
  });

  it('turns the image +Y axis to position angle −rotation (CROTA2 / atan2(−CD1_2, CD2_2))', () => {
    // Position angle (north through east) of b seen from a, degrees in [0, 360).
    const pa = (a: LonLat, b: LonLat) => {
      const [ra1, d1] = [a[0] * DEG, a[1] * DEG];
      const [ra2, d2] = [b[0] * DEG, b[1] * DEG];
      const y = Math.sin(ra2 - ra1) * Math.cos(d2);
      const x = Math.cos(d1) * Math.sin(d2) - Math.sin(d1) * Math.cos(d2) * Math.cos(ra2 - ra1);
      return ((Math.atan2(y, x) / DEG) + 360) % 360;
    };
    const center: LonLat = [10.67, 41.1];
    for (const [rotation, expected] of [[0, 0], [283.5, 76.5], [30, 330], [90, 270]]) {
      // A box 1e-6° wide: corners 2 and 3 (the +Y edge) sit on the +Y axis.
      const c = footprintCorners(center[0], center[1], 1e-6, 1, rotation);
      const top: LonLat = [(c[2][0] + c[3][0]) / 2, (c[2][1] + c[3][1]) / 2];
      const diff = ((pa(center, top) - expected + 540) % 360) - 180;
      expect(Math.abs(diff)).toBeLessThan(1e-3);
    }
    // A CD matrix whose +Y column points east, (CD1_2, CD2_2) = (+s, 0), has
    // field rotation atan2(−s, 0) = 270°; the box's +Y edge must lie east.
    const east = footprintCorners(0, 0, 2, 1, 270);
    expect((east[2][0] + east[3][0]) / 2).toBeGreaterThan(0);
  });

  it('inverse gnomonic of the origin is the center', () => {
    expect(inverseGnomonic(0, 0, 10, 20)).toEqual([10, 20]);
  });
});

describe('projectFootprint', () => {
  // A flat projection (x = -ra, y = -dec, scaled) is enough to drive the rules.
  const flat = (k: number) => (p: LonLat): Point => [-p[0] * k, -p[1] * k];
  const canvas = { width: 1000, height: 800 };
  const fov = { width: 2, height: 1, paDeg: 0 };
  const corners = footprintCorners(100, 0, 2, 1, 0);

  it('draws a box of sane size and aspect', () => {
    const res = projectFootprint(corners, flat(50), () => true, canvas, fov);
    expect(res.hidden).toBeNull();
    expect(res.points).toHaveLength(4);
  });

  it('hides a box spanning more than half the canvas (zoom out to see it)', () => {
    expect(projectFootprint(corners, flat(300), () => true, canvas, fov).hidden).toBe('span');
  });

  it('hides a box whose aspect is distorted more than 3× (pan toward it)', () => {
    const squash = (p: LonLat): Point => [-p[0] * 50, -p[1] * 5];
    expect(projectFootprint(corners, squash, () => true, canvas, fov).hidden).toBe('distortion');
  });

  it('hides a box only when every corner is beyond the visible hemisphere', () => {
    let n = 0;
    const someVisible = () => (n++ % 4) === 0;
    expect(projectFootprint(corners, flat(50), someVisible, canvas, fov).hidden).toBeNull();
    expect(projectFootprint(corners, flat(50), () => false, canvas, fov).hidden).toBe('clip');
  });

  it('hides a box with a corner that does not project', () => {
    const broken = (p: LonLat): Point | null => (p === corners[2] ? null : flat(50)(p));
    expect(projectFootprint(corners, broken, () => true, canvas, fov)).toEqual({ points: [], hidden: 'clip' });
    const nan = (p: LonLat): Point => (p === corners[1] ? [NaN, 0] : flat(50)(p));
    expect(projectFootprint(corners, nan, () => true, canvas, fov).hidden).toBe('clip');
  });

  it('accounts for the position angle in the expected aspect', () => {
    const rotated = footprintCorners(100, 0, 2, 1, 90);
    const res = projectFootprint(rotated, flat(50), () => true, canvas, { width: 2, height: 1, paDeg: 90 });
    expect(res.hidden).toBeNull();
  });
});

describe('pathData', () => {
  it('closes the polygon', () => {
    expect(pathData([[0, 0], [1, 0], [1, 1]])).toBe('M0,0 L1,0 L1,1 Z');
  });
});
