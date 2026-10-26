import { describe, expect, it } from 'vitest';
import golden from './healpix.golden.json';
import {
  angularDistance, cellCap, cellCount, children, sphCoo, tilePixelSize, visibleCells,
} from './healpix';

type Row = [number, number, number, number, number, number];

describe('sphCoo against cdshealpix', () => {
  it('matches every golden vector to 1e-12 rad', () => {
    const rows = (golden as unknown as { rows: Row[] }).rows;
    expect(rows.length).toBeGreaterThan(500);
    let worst = 0;
    for (const [order, npix, dx, dy, lon, lat] of rows) {
      const [l, b] = sphCoo(order, npix, dx, dy);
      worst = Math.max(worst, angularDistance([l, b], [lon, lat]));
    }
    expect(worst).toBeLessThan(1e-12);
  });
});

describe('cells', () => {
  it('counts and children', () => {
    expect(cellCount(0)).toBe(12);
    expect(cellCount(6)).toBe(49152);
    expect(children(7)).toEqual([28, 29, 30, 31]);
  });

  it('a cell cap contains the cell', () => {
    for (const [o, n] of [[0, 4], [3, 7], [5, 1000], [6, 49151]]) {
      const cap = cellCap(o, n);
      for (let i = 0; i <= 10; i++) {
        for (let j = 0; j <= 10; j++) {
          const p = sphCoo(o, n, Math.min(i / 10, 1 - 1e-12), Math.min(j / 10, 1 - 1e-12));
          expect(angularDistance(cap.center, p)).toBeLessThanOrEqual(cap.radius);
        }
      }
    }
  });

  it('visibleCells finds the tile holding the Pleiades and nothing far away', () => {
    const RAD = Math.PI / 180;
    const pleiades: [number, number] = [56.75 * RAD, 24.12 * RAD];
    const cells = visibleCells(3, pleiades, 1 * RAD);
    expect(cells).toContain(7); // measured on the live survey, issue #18
    for (const n of cells) {
      const cap = cellCap(3, n);
      expect(angularDistance(cap.center, pleiades)).toBeLessThanOrEqual(1 * RAD + cap.radius);
    }
    expect(visibleCells(0, pleiades, Math.PI)).toHaveLength(12);
  });

  it('tile pixel size: ~0.8″ at order 9, ~13″ at order 5 for 512 px tiles', () => {
    const arcsec = (o: number) => (tilePixelSize(o, 512) * 180 * 3600) / Math.PI;
    expect(arcsec(9)).toBeCloseTo(0.805, 2);
    expect(arcsec(5)).toBeCloseTo(12.88, 1);
  });
});
