import { describe, expect, it } from 'vitest';
import { triangleAffine } from './hipsLayer';

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
