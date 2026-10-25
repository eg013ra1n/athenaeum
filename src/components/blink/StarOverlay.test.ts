import { describe, it, expect } from 'vitest';
import { starSemiAxes } from './StarOverlay';

const settings = { ellipse_scale: 1.2, min_radius: 6, max_radius: 60 };
const star = (over: Partial<{ peak: number; fwhm_x: number; fwhm_y: number; beta: number | null }> = {}) => ({
  peak: 3000, fwhm_x: 3, fwhm_y: 3, beta: 2.5 as number | null, ...over,
});

describe('starSemiAxes', () => {
  it('a faint star (R <= 1) keeps ellipse_scale * fwhm', () => {
    // peak / (3 * noise) = 0.5
    const { a, b } = starSemiAxes(star({ peak: 1.5, fwhm_x: 10, fwhm_y: 8 }), 1, settings, 1);
    expect(a).toBeCloseTo(12, 6);
    expect(b).toBeCloseTo(9.6, 6);
  });

  it('a bright star (R = 1000, beta 2.5) reaches ~3.41 FWHM', () => {
    const { a, b } = starSemiAxes(star(), 1, settings, 1);
    expect(a).toBeCloseTo(10.2, 1);
    expect(b).toBeCloseTo(10.2, 1);
    expect(a / 3).toBeCloseTo(3.41, 2);
  });

  it('works per axis with the same R and beta', () => {
    const { a, b } = starSemiAxes(star({ fwhm_x: 3, fwhm_y: 6 }), 1, settings, 1);
    expect(b / a).toBeCloseTo(2, 6);
  });

  it('a null beta behaves as 2.5', () => {
    expect(starSemiAxes(star({ beta: null }), 1, settings, 1)).toEqual(starSemiAxes(star({ beta: 2.5 }), 1, settings, 1));
  });

  it('beta is clamped to [1.5, 10]', () => {
    expect(starSemiAxes(star({ beta: 0.1 }), 1, settings, 1)).toEqual(starSemiAxes(star({ beta: 1.5 }), 1, settings, 1));
    expect(starSemiAxes(star({ beta: 99 }), 1, settings, 1)).toEqual(starSemiAxes(star({ beta: 10 }), 1, settings, 1));
  });

  it('no usable noise or peak falls back to ellipse_scale * fwhm', () => {
    for (const noise of [0, -1, null, undefined, NaN]) {
      const { a } = starSemiAxes(star(), noise, settings, 1);
      expect(a).toBe(6); // 3.6 image px, lifted by the 6 canvas-px lower clamp
    }
    const big = star({ fwhm_x: 10, fwhm_y: 10 });
    expect(starSemiAxes(big, 0, settings, 1).a).toBeCloseTo(12, 6);
    expect(starSemiAxes(star({ peak: 0, fwhm_x: 10, fwhm_y: 10 }), 1, settings, 1).a).toBeCloseTo(12, 6);
  });

  it('the upper clamp is in image px: zoom does not cut a large star', () => {
    const s = star({ peak: 0, fwhm_x: 20 / 1.2, fwhm_y: 20 / 1.2 }); // 20 image px
    expect(starSemiAxes(s, 1, settings, 1).a).toBeCloseTo(20, 6);
    // x4 zoom: 80 canvas px, past the old 60 canvas-px cap
    expect(starSemiAxes(s, 1, settings, 4).a).toBeCloseTo(80, 6);
    // a 100-px star is cut at 60 image px, i.e. 240 canvas px at x4
    const huge = star({ peak: 0, fwhm_x: 100, fwhm_y: 100 });
    expect(starSemiAxes(huge, 1, settings, 4).a).toBeCloseTo(240, 6);
    expect(starSemiAxes(huge, 1, settings, 1).a).toBeCloseTo(60, 6);
  });

  it('the lower clamp is in canvas px', () => {
    const s = star({ peak: 0, fwhm_x: 2, fwhm_y: 2 }); // 2.4 image px
    expect(starSemiAxes(s, 1, settings, 1).a).toBe(6);
    expect(starSemiAxes(s, 1, settings, 0.5).a).toBe(6);
    expect(starSemiAxes(s, 1, settings, 4).a).toBeCloseTo(9.6, 6);
  });
});
