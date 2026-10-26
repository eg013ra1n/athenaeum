import { describe, expect, it } from 'vitest';
import { MeshPoint, createGlRenderer, meshVertices } from './hipsGl';

describe('meshVertices', () => {
  // A 2×2 mesh: point (a, b) at x = 100·b, y = 100·a.
  const grid = (n: number): MeshPoint[] => {
    const pts: MeshPoint[] = [];
    for (let a = 0; a <= n; a++) for (let b = 0; b <= n; b++) pts.push([100 * b, 100 * a]);
    return pts;
  };

  it('gives every cell two triangles split along the anti-diagonal, texture u = column, v = row', () => {
    const v = meshVertices(2, grid(2));
    expect(v.length).toBe(2 * 2 * 6 * 4);
    // First cell: (0,0) (1,0) (0,1), then (1,1) (0,1) (1,0).
    const first = Array.from(v.subarray(0, 24));
    expect(first).toEqual([
      0, 0, 0, 0, 0, 100, 0, 0.5, 100, 0, 0.5, 0,
      100, 100, 0.5, 0.5, 100, 0, 0.5, 0, 0, 100, 0, 0.5,
    ]);
    // Every vertex's texture point is its screen point / 200 on this grid.
    for (let i = 0; i < v.length; i += 4) {
      expect(v[i + 2]).toBeCloseTo(v[i] / 200, 9);
      expect(v[i + 3]).toBeCloseTo(v[i + 1] / 200, 9);
    }
  });

  it('leaves out each triangle with a point that does not project', () => {
    const pts = grid(1);
    pts[3] = null; // (1, 1)
    const v = meshVertices(1, pts);
    expect(v.length).toBe(3 * 4);
    expect(meshVertices(1, [null, null, null, null]).length).toBe(0);
  });
});

describe('createGlRenderer', () => {
  it('returns null without WebGL, leaving the canvas free for 2D', () => {
    const canvas = document.createElement('canvas');
    expect(createGlRenderer(canvas, () => {})).toBeNull();
  });
});
