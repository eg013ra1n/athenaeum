/**
 * Hook to calculate visible sky region bounds
 * Determines which imaging locations are currently visible in the viewport
 */

import { useState, useEffect, useCallback } from 'react';

export interface ViewportBounds {
  /** Minimum RA in degrees (0-360) */
  raMin: number;

  /** Maximum RA in degrees (0-360) */
  raMax: number;

  /** Minimum Dec in degrees (-90 to 90) */
  decMin: number;

  /** Maximum Dec in degrees (-90 to 90) */
  decMax: number;

  /** True if bounds are valid */
  isValid: boolean;
}

/**
 * Calculate viewport bounds in celestial coordinates
 * Samples the four corners and center of the canvas to determine visible region
 *
 * @returns ViewportBounds object with RA/Dec min/max
 */
export function useViewportBounds(): ViewportBounds {
  const [bounds, setBounds] = useState<ViewportBounds>({
    raMin: 0,
    raMax: 360,
    decMin: -90,
    decMax: 90,
    isValid: false,
  });

  const updateBounds = useCallback(() => {
    const canvas = document.querySelector('#celestial-map canvas') as HTMLCanvasElement;
    if (!canvas || !window.Celestial) {
      return;
    }

    const projection = window.Celestial.projection;
    if (!projection || typeof projection.invert !== 'function') {
      return;
    }

    // Get canvas dimensions
    const rect = canvas.getBoundingClientRect();
    const width = rect.width;
    const height = rect.height;

    // Sample points: four corners and center
    const samplePoints = [
      [0, 0],                    // Top-left
      [width, 0],                // Top-right
      [0, height],               // Bottom-left
      [width, height],           // Bottom-right
      [width / 2, height / 2],   // Center
      [width / 4, height / 2],   // Left-center
      [3 * width / 4, height / 2], // Right-center
      [width / 2, height / 4],   // Top-center
      [width / 2, 3 * height / 4], // Bottom-center
    ];

    const validCoords: Array<[number, number]> = [];

    // Convert each sample point to RA/Dec
    for (const [x, y] of samplePoints) {
      try {
        const coords = projection.invert([x, y]);
        if (coords && coords.length === 2 &&
            !isNaN(coords[0]) && !isNaN(coords[1]) &&
            isFinite(coords[0]) && isFinite(coords[1])) {
          validCoords.push(coords as [number, number]);
        }
      } catch (e) {
        // Ignore invalid projections (outside visible hemisphere)
        continue;
      }
    }

    if (validCoords.length === 0) {
      setBounds({
        raMin: 0,
        raMax: 360,
        decMin: -90,
        decMax: 90,
        isValid: false,
      });
      return;
    }

    // Calculate bounds from valid coordinates
    let raMin = 360;
    let raMax = 0;
    let decMin = 90;
    let decMax = -90;

    for (const [ra, dec] of validCoords) {
      raMin = Math.min(raMin, ra);
      raMax = Math.max(raMax, ra);
      decMin = Math.min(decMin, dec);
      decMax = Math.max(decMax, dec);
    }

    // Handle RA wrap-around (0°/360° boundary)
    // If the span is > 180°, we might be crossing the boundary
    if (raMax - raMin > 180) {
      // Likely crossing 0°/360° boundary, swap min/max
      [raMin, raMax] = [raMax, raMin];
    }

    // Add small padding (5 degrees) to ensure FOV boxes near edge are included
    const raPadding = 5;
    const decPadding = 5;

    raMin = Math.max(0, raMin - raPadding);
    raMax = Math.min(360, raMax + raPadding);
    decMin = Math.max(-90, decMin - decPadding);
    decMax = Math.min(90, decMax + decPadding);

    setBounds({
      raMin,
      raMax,
      decMin,
      decMax,
      isValid: true,
    });
  }, []);

  useEffect(() => {
    // Initial bounds calculation
    updateBounds();

    // Listen for zoom/pan events
    const container = document.getElementById('celestial-map');
    if (container) {
      container.addEventListener('zoomend', updateBounds);
      container.addEventListener('redraw', updateBounds);

      return () => {
        container.removeEventListener('zoomend', updateBounds);
        container.removeEventListener('redraw', updateBounds);
      };
    }
  }, [updateBounds]);

  return bounds;
}

/**
 * Check if a point (RA, Dec) is within the viewport bounds
 *
 * @param ra - Right Ascension in degrees
 * @param dec - Declination in degrees
 * @param bounds - Viewport bounds from useViewportBounds
 * @returns True if the point is visible
 */
export function isPointInBounds(
  ra: number,
  dec: number,
  bounds: ViewportBounds
): boolean {
  if (!bounds.isValid) {
    return false;
  }

  // Check declination (simple range check)
  if (dec < bounds.decMin || dec > bounds.decMax) {
    return false;
  }

  // Check RA (handle wrap-around case)
  if (bounds.raMin <= bounds.raMax) {
    // Normal case: no wrap-around
    return ra >= bounds.raMin && ra <= bounds.raMax;
  } else {
    // Wrap-around case: raMin > raMax (crossing 0°/360°)
    return ra >= bounds.raMin || ra <= bounds.raMax;
  }
}
