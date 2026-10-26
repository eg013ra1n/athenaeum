import { describe, expect, it } from 'vitest';
import {
  MAX_FOV_DEG, MIN_FOV_DEG, STEREO_BASE_SCALE, ZOOM_EXTENT, clampFov, defaultView, fovToScale,
  normalizeRa, parseSkyView, parseViewParams, scaleToFov, viewsDiffer,
} from './skyView';

describe('fov ↔ scale', () => {
  it('round-trips at any width', () => {
    for (const width of [640, 1329, 2560]) {
      for (const fov of [12, 45, 90, 170]) {
        expect(scaleToFov(width, fovToScale(width, fov))).toBeCloseTo(fov, 9);
      }
    }
  });

  it('matches the stereographic radius ρ = scale · tan(θ/2) at the canvas edge', () => {
    const width = 1600;
    const scale = 3000;
    const halfAngle = (scaleToFov(width, scale) / 2) * (Math.PI / 180);
    expect(scale * Math.tan(halfAngle / 2)).toBeCloseTo(width / 2, 9);
  });

  it('has width-independent zoom bounds (≈ 11.7° … 182.7°)', () => {
    for (const width of [800, 1600, 3200]) {
      const base = (STEREO_BASE_SCALE * width) / 1024;
      expect(scaleToFov(width, base * ZOOM_EXTENT)).toBeCloseTo(MIN_FOV_DEG, 9);
      expect(scaleToFov(width, base)).toBeCloseTo(MAX_FOV_DEG, 9);
    }
    expect(MIN_FOV_DEG).toBeCloseTo(11.72, 2);
    expect(MAX_FOV_DEG).toBeCloseTo(182.72, 2);
    expect(clampFov(1)).toBe(MIN_FOV_DEG);
    expect(clampFov(500)).toBe(MAX_FOV_DEG);
    expect(clampFov(30)).toBe(30);
  });
});

describe('defaultView', () => {
  it('fills the width of a widescreen canvas, as the first-run fill-zoom did', () => {
    // zoom = aspect over the base scale
    const v = defaultView(1600, 900);
    const base = (STEREO_BASE_SCALE * 1600) / 1024;
    expect(v.fovDeg).toBeCloseTo(scaleToFov(1600, base * (1600 / 900)), 9);
    expect(v).toMatchObject({ ra: 0, dec: 0, roll: 0 });
  });

  it('never zooms out past the base scale on a tall canvas', () => {
    expect(defaultView(600, 900).fovDeg).toBeCloseTo(MAX_FOV_DEG, 9);
  });
});

describe('parseSkyView', () => {
  it('accepts a stored view and normalizes it', () => {
    expect(parseSkyView('{"ra":-10,"dec":20,"roll":1.5,"fovDeg":5}')).toEqual({
      ra: 350, dec: 20, roll: 1.5, fovDeg: MIN_FOV_DEG,
    });
  });

  it.each([
    ['', 'empty'],
    ['not json', 'garbage'],
    ['{"ra":1,"dec":2,"roll":0}', 'missing fov'],
    ['{"ra":1,"dec":95,"roll":0,"fovDeg":20}', 'dec out of range'],
    ['{"ra":"1","dec":2,"roll":0,"fovDeg":20}', 'string number'],
    ['{"ra":1,"dec":2,"roll":0,"fovDeg":0}', 'zero fov'],
    ['null', 'null'],
  ])('rejects %j (%s)', (raw) => {
    expect(parseSkyView(raw)).toBeNull();
  });
});

describe('parseViewParams', () => {
  it('reads ra, dec and an optional fov', () => {
    expect(parseViewParams(new URLSearchParams('ra=83.82&dec=-5.39&fov=60'))).toEqual({ ra: 83.82, dec: -5.39, fovDeg: 60 });
    expect(parseViewParams(new URLSearchParams('ra=370&dec=10'))).toEqual({ ra: 10, dec: 10 });
  });

  it('ignores the old absolute zoom param and bad values', () => {
    expect(parseViewParams(new URLSearchParams('ra=1&dec=2&zoom=800'))).toEqual({ ra: 1, dec: 2 });
    expect(parseViewParams(new URLSearchParams('ra=1&dec=2&fov=abc'))).toEqual({ ra: 1, dec: 2 });
    expect(parseViewParams(new URLSearchParams('ra=1'))).toBeNull();
    expect(parseViewParams(new URLSearchParams('ra=x&dec=2'))).toBeNull();
    expect(parseViewParams(new URLSearchParams('ra=1&dec=-91'))).toBeNull();
  });
});

describe('viewsDiffer', () => {
  const v = { ra: 0.00001, dec: 10, roll: 0, fovDeg: 30 };
  it('treats RA across 0h as continuous', () => {
    expect(viewsDiffer(v, { ...v, ra: 359.99999 })).toBe(false);
    expect(viewsDiffer(v, { ...v, ra: 359.9 })).toBe(true);
  });
  it('sees a zoom change', () => {
    expect(viewsDiffer(v, { ...v, fovDeg: 30.001 })).toBe(true);
  });
  it('normalizes RA', () => {
    expect(normalizeRa(-0.5)).toBeCloseTo(359.5, 9);
    expect(normalizeRa(720.25)).toBeCloseTo(0.25, 9);
  });
});
