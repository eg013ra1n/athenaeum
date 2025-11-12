/**
 * Utility for converting pixel-space rectangles to sky coordinate bounds
 * Uses multi-point sampling to handle projection distortions
 */

export interface PixelRectangle {
  x1: number;
  y1: number;
  x2: number;
  y2: number;
}

export interface SkyBounds {
  ra_min: number;
  ra_max: number;
  dec_min: number;
  dec_max: number;
  crosses_meridian: boolean;  // True if spans RA=0/360
  contains_pole: 'north' | 'south' | null;
}

/**
 * Convert a pixel rectangle to sky coordinate bounds
 * Samples multiple points to handle projection curvature
 */
export function calculateSkyBounds(
  rect: PixelRectangle,
  pixelToSky: (x: number, y: number) => [number, number] | null
): SkyBounds | null {
  // Sample points: 4 corners + 4 edge midpoints + 9 interior grid points
  // This captures the curved distortion of the projection
  const samplePoints: Array<[number, number]> = [
    // 4 corners
    [rect.x1, rect.y1],
    [rect.x2, rect.y1],
    [rect.x2, rect.y2],
    [rect.x1, rect.y2],

    // 4 edge midpoints
    [(rect.x1 + rect.x2) / 2, rect.y1],
    [rect.x2, (rect.y1 + rect.y2) / 2],
    [(rect.x1 + rect.x2) / 2, rect.y2],
    [rect.x1, (rect.y1 + rect.y2) / 2],

    // 9 interior grid points (3x3 grid)
    ...createInteriorGrid(rect, 3)
  ];

  // Convert all sample points to sky coordinates
  const skyPoints: Array<[number, number]> = [];
  for (const [px, py] of samplePoints) {
    const sky = pixelToSky(px, py);
    if (sky) skyPoints.push(sky);
  }

  if (skyPoints.length === 0) return null;

  // Extract RA and Dec arrays
  const ras = skyPoints.map(p => p[0]);
  const decs = skyPoints.map(p => p[1]);

  // Check for pole containment
  const maxDec = Math.max(...decs);
  const minDec = Math.min(...decs);
  const containsPole = maxDec >= 89.5 ? 'north' :
                       minDec <= -89.5 ? 'south' : null;

  // Handle RA wraparound detection
  const raSpan = Math.max(...ras) - Math.min(...ras);
  const crossesMeridian = raSpan > 180;

  let ra_min: number, ra_max: number;

  if (containsPole) {
    // If contains pole, RA is unbounded (all RA values)
    ra_min = 0;
    ra_max = 360;
  } else if (crossesMeridian) {
    // Crosses RA=0/360: find the gap in RA coverage
    // The gap represents the "outside" of the selection
    const sortedRAs = [...ras].sort((a, b) => a - b);
    const gaps: Array<{ start: number; end: number; size: number }> = [];

    for (let i = 0; i < sortedRAs.length - 1; i++) {
      gaps.push({
        start: sortedRAs[i],
        end: sortedRAs[i + 1],
        size: sortedRAs[i + 1] - sortedRAs[i]
      });
    }

    // Add wraparound gap
    gaps.push({
      start: sortedRAs[sortedRAs.length - 1],
      end: sortedRAs[0] + 360,
      size: sortedRAs[0] + 360 - sortedRAs[sortedRAs.length - 1]
    });

    const largestGap = gaps.reduce((max, g) => g.size > max.size ? g : max);
    ra_min = largestGap.end % 360;
    ra_max = largestGap.start;
  } else {
    // Normal case: simple min/max
    ra_min = Math.min(...ras);
    ra_max = Math.max(...ras);
  }

  return {
    ra_min,
    ra_max,
    dec_min: Math.min(...decs),
    dec_max: Math.max(...decs),
    crosses_meridian: crossesMeridian,
    contains_pole: containsPole
  };
}

/**
 * Create a grid of interior sample points
 */
function createInteriorGrid(
  rect: PixelRectangle,
  gridSize: number
): Array<[number, number]> {
  const points: Array<[number, number]> = [];
  for (let i = 1; i <= gridSize; i++) {
    for (let j = 1; j <= gridSize; j++) {
      const x = rect.x1 + (rect.x2 - rect.x1) * (i / (gridSize + 1));
      const y = rect.y1 + (rect.y2 - rect.y1) * (j / (gridSize + 1));
      points.push([x, y]);
    }
  }
  return points;
}
