// Wave 6c Task 7: the gnomonic projection of the canonical grid's `grid_wcs`, equal to the Rust TAN to 1e-6 px
// (Task 1b's golden file), and the grid-up schematic orientation built on it.
import { describe, expect, it } from 'vitest';
import golden from './gridProjection.golden.json';
import { fixed, gridValuesLine, northArrowAngleDeg, skyToGridPixel, toSchematic } from './gridProjection';
import type { CanonicalGrid } from '../../types/models';

const GRID: CanonicalGrid = { scaleArcsec: 2, widthPx: 4096, heightPx: 3072, crval1: 83.8, crval2: -5.4, rotationDeg: 0 };

/** The point `deg` east/north of the centre on the sky (small offsets: the RA step is divided by cos Dec). */
const eastOf = (g: CanonicalGrid, deg: number) => ({ ra: g.crval1 + deg / Math.cos((g.crval2 * Math.PI) / 180), dec: g.crval2 });
const northOf = (g: CanonicalGrid, deg: number) => ({ ra: g.crval1, dec: g.crval2 + deg });

describe('skyToGridPixel matches the Rust TAN', () => {
  for (const [i, entry] of golden.grids.entries()) {
    it(`gives every golden point of grid ${i} (rotation ${entry.grid.rotationDeg}) to 1e-6 px`, () => {
      expect(entry.points.length).toBeGreaterThan(0);
      for (const p of entry.points) {
        const got = skyToGridPixel(p.raDeg, p.decDeg, entry.grid);
        expect(got, `point ${p.raDeg}, ${p.decDeg}`).not.toBeNull();
        expect(Math.abs(got!.x - p.x)).toBeLessThan(1e-6);
        expect(Math.abs(got!.y - p.y)).toBeLessThan(1e-6);
      }
    });
  }
});

describe('skyToGridPixel behind the tangent plane', () => {
  it('is null for a point 100 degrees from the centre', () => {
    expect(skyToGridPixel(GRID.crval1 + 100, GRID.crval2, GRID)).toBeNull();
  });
  it('is null for the antipode', () => {
    expect(skyToGridPixel(GRID.crval1 + 180, -GRID.crval2, GRID)).toBeNull();
  });
  it('is null at 90 degrees, where cos c is zero up to float noise', () => {
    expect(skyToGridPixel(0, 0, { ...GRID, crval1: 90, crval2: 0 })).toBeNull();
  });
  it('still projects a point 80 degrees away', () => {
    expect(skyToGridPixel(GRID.crval1, GRID.crval2 + 80, GRID)).not.toBeNull();
  });
});

describe('the schematic is drawn grid-up', () => {
  it('puts a point 0.5 degrees north above the centre and one 0.5 degrees east left of it, at rotation 0', () => {
    const centre = toSchematic(skyToGridPixel(GRID.crval1, GRID.crval2, GRID)!, GRID);
    const n = northOf(GRID, 0.5);
    const north = toSchematic(skyToGridPixel(n.ra, n.dec, GRID)!, GRID);
    const e = eastOf(GRID, 0.5);
    const east = toSchematic(skyToGridPixel(e.ra, e.dec, GRID)!, GRID);
    expect(north.y).toBeLessThan(centre.y);
    expect(Math.abs(north.x - centre.x)).toBeLessThan(1e-6);
    expect(east.x).toBeLessThan(centre.x);
    expect(Math.abs(east.y - centre.y)).toBeLessThan(1);
  });

  it('flips y about the grid height: SVG y is heightPx minus the grid y', () => {
    expect(toSchematic({ x: 10, y: 3 }, GRID)).toEqual({ x: 10, y: 3072 - 3 });
  });
});

describe('northArrowAngleDeg', () => {
  it('is vertical at rotation 0', () => {
    expect(northArrowAngleDeg(GRID)).toBeCloseTo(0, 6);
  });
  it('is tilted 30 degrees from vertical at rotation 30, leaning toward east (left)', () => {
    // Clockwise from up, so a lean to the left is negative; the golden grid at 30 degrees puts the point 0.5 degrees
    // north of the centre left of it.
    expect(northArrowAngleDeg({ ...GRID, rotationDeg: 30 })).toBeCloseTo(-30, 6);
  });
  it('follows the opposite rotation', () => {
    expect(northArrowAngleDeg({ ...GRID, rotationDeg: -30 })).toBeCloseTo(30, 6);
  });
  it('still answers at a declination near the pole', () => {
    expect(Number.isFinite(northArrowAngleDeg({ ...GRID, crval2: 90 }))).toBe(true);
  });
});

// D6: the scale reads 2 decimals and the centre 4, always; the size, the field of view and the rotation drop their zeros.
describe('gridValuesLine', () => {
  it('reads the scale with 2 decimals and the centre RA and Dec with 4, zeros kept', () => {
    expect(gridValuesLine(GRID)).toBe(
      '2.00\u2033/px \u00b7 4096 \u00d7 3072 px (2.28\u00b0 \u00d7 1.71\u00b0) \u00b7 centre RA 83.8000\u00b0 Dec -5.4000\u00b0 \u00b7 rotation 0\u00b0',
    );
  });
  it('rounds to those decimals and keeps the rotation trimmed', () => {
    const line = gridValuesLine({ ...GRID, scaleArcsec: 1.4999, crval1: 83.123456, crval2: -5.000049, rotationDeg: 12.5 });
    expect(line).toContain('1.50\u2033/px');
    expect(line).toContain('centre RA 83.1235\u00b0 Dec -5.0000\u00b0');
    expect(line).toContain('rotation 12.5\u00b0');
  });
  it('never signs a centre that rounds to zero, and shows a dash for a non-finite value', () => {
    expect(fixed(-0.00001, 4)).toBe('0.0000');
    expect(fixed(Number.NaN, 4)).toBe('\u2014');
  });
});
