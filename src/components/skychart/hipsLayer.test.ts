import { describe, expect, it } from 'vitest';
import { angularDistance, cellCap, cellCount, children, sphCoo } from './healpix';
import { meshSize, triangleAffine } from './hipsLayer';

const apply = (m: number[], [u, v]: [number, number]): [number, number] => [m[0] * u + m[2] * v + m[4], m[1] * u + m[3] * v + m[5]];

describe('triangleAffine', () => {
  it('maps each texture corner onto its screen corner', () => {
    const t: [number, number][] = [[0, 0], [512, 0], [512, 512]];
    const p: [number, number][] = [[100, 200], [140, 190], [150, 250]];
    const m = triangleAffine(p, t)!;
    t.forEach((pt, i) => {
      const [x, y] = apply(m, pt);
      expect(x).toBeCloseTo(p[i][0], 9);
      expect(y).toBeCloseTo(p[i][1], 9);
    });
  });

  it('handles a mirrored mapping (the chart flips east-west)', () => {
    const t: [number, number][] = [[0, 0], [0, 64], [64, 64]];
    const p: [number, number][] = [[10, 10], [-30, 12], [-28, 50]];
    const m = triangleAffine(p, t)!;
    t.forEach((pt, i) => {
      const [x, y] = apply(m, pt);
      expect(x).toBeCloseTo(p[i][0], 9);
      expect(y).toBeCloseTo(p[i][1], 9);
    });
  });

  it('refuses a degenerate texture triangle', () => {
    expect(triangleAffine([[0, 0], [1, 1], [2, 2]], [[0, 0], [1, 1], [2, 2]])).toBeNull();
  });
});

describe('meshSize', () => {
  const RAD = Math.PI / 180;
  // The chart at M 31, 11.7° wide at DPR 1: d3's stereographic, ρ = scale·tan(c/2), east to the left.
  const [lon0, lat0] = [10.6847 * RAD, 41.269 * RAD];
  const scale = 12978;
  const project = (lon: number, lat: number): [number, number] => {
    const dl = lon - lon0;
    const k = scale / (1 + Math.sin(lat0) * Math.sin(lat) + Math.cos(lat0) * Math.cos(lat) * Math.cos(dl));
    return [-k * Math.cos(lat) * Math.sin(dl), -k * (Math.cos(lat0) * Math.sin(lat) - Math.sin(lat0) * Math.cos(lat) * Math.cos(dl))];
  };
  /** Largest distance, device px, between the drawn n×n triangle mesh and the true projection. */
  const meshError = (order: number, npix: number, n: number): number => {
    const at = (dx: number, dy: number) => {
      const [lon, lat] = sphCoo(order, npix, Math.min(dx, 1 - 1e-12), Math.min(dy, 1 - 1e-12));
      return project(lon, lat);
    };
    let worst = 0;
    for (let a = 0; a < n; a++) {
      for (let b = 0; b < n; b++) {
        const p00 = at(a / n, b / n);
        const p10 = at((a + 1) / n, b / n);
        const p01 = at(a / n, (b + 1) / n);
        const p11 = at((a + 1) / n, (b + 1) / n);
        for (let i = 0; i <= 4; i++) {
          for (let j = 0; j <= 4; j++) {
            const [s, t] = [i / 4, j / 4];
            // Triangle (p00, p10, p01) when s + t ≤ 1, else (p11, p01, p10), as drawTile draws them.
            const drawn = s + t <= 1
              ? [0, 1].map((k) => p00[k] + s * (p10[k] - p00[k]) + t * (p01[k] - p00[k]))
              : [0, 1].map((k) => p11[k] + (1 - s) * (p01[k] - p11[k]) + (1 - t) * (p10[k] - p11[k]));
            const truth = at((a + s) / n, (b + t) / n);
            worst = Math.max(worst, Math.hypot(drawn[0] - truth[0], drawn[1] - truth[1]));
          }
        }
      }
    }
    return worst;
  };
  /** A tile of `order` near (lon, lat): the nearest-centered cell, descending from order 0. */
  const tileAt = (order: number, lonDeg: number, latDeg: number): number => {
    const target: [number, number] = [lonDeg * RAD, latDeg * RAD];
    const nearest = (o: number, cells: number[]) => cells.reduce((a, b) =>
      (angularDistance(cellCap(o, a).center, target) <= angularDistance(cellCap(o, b).center, target) ? a : b));
    let n = nearest(0, Array.from({ length: cellCount(0) }, (_, i) => i));
    for (let o = 1; o <= order; o++) n = nearest(o, children(n));
    return n;
  };

  it('keeps the drawn tile within half a pixel of the sky across the HEALPix polar/equatorial kink (M 110)', () => {
    const m110 = tileAt(4, 10.092, 41.685);
    // One cell per tile is a few px off; the kink at z = 2/3 runs through
    // this tile, so a mesh split along the other diagonal would stay ~100 px
    // off at n = 1 and shrink only linearly.
    expect(meshError(4, m110, 1)).toBeLessThan(5);
    expect(meshError(4, m110, 4)).toBeLessThan(meshError(4, m110, 2) / 3);
    const n = meshSize(4, m110, project);
    expect(n).toBeGreaterThanOrEqual(4);
    expect(meshError(4, m110, n)).toBeLessThan(0.5);
  });

  it('stays within a pixel for every tile around the view, and small tiles stay cheap', () => {
    for (const npix of [tileAt(4, 10.68, 41.27), tileAt(4, 17.43, 35.62), tileAt(4, 4, 46)]) {
      expect(meshError(4, npix, meshSize(4, npix, project))).toBeLessThan(0.5);
    }
    // An order-9 tile is ≈ 13 px here: one cell.
    expect(meshSize(9, tileAt(9, 10.68, 41.27), project)).toBe(1);
  });

  it('gives a tile with a corner that does not project a fixed mesh', () => {
    expect(meshSize(4, 0, () => null)).toBe(16);
  });
});
