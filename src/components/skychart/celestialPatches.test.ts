import { describe, expect, it } from 'vitest';
import lib from '../../../public/lib/celestial.min.js?raw';

// The bundled d3-celestial 0.7.35 carries local patches the Sky Chart relies
// on (docs/skychart/README.md). Replacing the file with a stock copy would
// silently break resize and pixel-ratio handling; this pins them.

describe('celestial.min.js patches', () => {
  it('is 0.7.35', () => {
    expect(lib).toContain('version:"0.7.35"');
  });

  it('P1: f() re-reads the pixel ratio on a forced resize and honours projectionRatio', () => {
    expect(lib).toContain('function f(t){t&&(Ht=window.devicePixelRatio||1);$.projectionRatio&&(Gt=$.projectionRatio);');
  });

  it('P2: resize() accepts { projectionRatio }', () => {
    expect(lib).toContain('k(t,"projectionRatio")&&($.projectionRatio=t.projectionRatio)),f(!0),$.width');
  });
});
