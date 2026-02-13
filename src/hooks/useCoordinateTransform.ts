/**
 * Custom hook for coordinate transformation between screen and sky coordinates
 * Interfaces with d3-celestial's projection system
 */

import { useCallback } from 'react';

declare global {
  interface Window {
    Celestial: any;
  }
}

export interface CoordinateTransformAPI {
  /**
   * Convert screen coordinates (pixels) to sky coordinates (RA/Dec)
   * @param x - Screen X coordinate in pixels
   * @param y - Screen Y coordinate in pixels
   * @returns [ra, dec] in degrees, or null if transformation fails
   */
  pixelToSky: (x: number, y: number) => [number, number] | null;

  /**
   * Convert sky coordinates (RA/Dec) to screen coordinates (pixels)
   * @param ra - Right Ascension in degrees
   * @param dec - Declination in degrees
   * @returns [x, y] in pixels, or null if transformation fails
   */
  skyToPixel: (ra: number, dec: number) => [number, number] | null;

  /**
   * Check if a sky coordinate is currently visible in the projection
   * @param ra - Right Ascension in degrees
   * @param dec - Declination in degrees
   * @returns true if coordinate is visible in current view
   */
  isVisible: (ra: number, dec: number) => boolean;

  /**
   * Get current map projection object from d3-celestial
   * @returns D3 projection object or null
   */
  getProjection: () => any;
}

/**
 * Get projection scaling factors to account for CSS stretching of the canvas.
 * d3-celestial sets canvas.width and canvas.height (buffer dimensions) to
 * projectionWidth * dpr and projectionHeight * dpr respectively.
 * Dividing by dpr gives the logical projection dimensions, which we compare
 * against the CSS display dimensions to get the scaling factors.
 * This works for any projection type without needing to know the aspect ratio.
 */
function getCanvasScaling(): { scaleX: number; scaleY: number } {
  const canvas = document.querySelector('#celestial-map canvas') as HTMLCanvasElement;
  if (!canvas) {
    return { scaleX: 1, scaleY: 1 };
  }

  const displayRect = canvas.getBoundingClientRect();
  const dpr = window.devicePixelRatio || 1;

  // Read projection dimensions directly from the canvas buffer
  // d3-celestial sets canvas.width = projWidth * dpr, canvas.height = projHeight * dpr
  const projectionWidth = canvas.width / dpr;
  const projectionHeight = canvas.height / dpr;

  return {
    scaleX: displayRect.width / projectionWidth,
    scaleY: displayRect.height / projectionHeight
  };
}

/**
 * Convert display coordinates to projection coordinates
 * Divides by scaling factors to account for aspect ratio differences
 */
function scaleDisplayToCanvas(x: number, y: number): [number, number] {
  const scaling = getCanvasScaling();
  return [x / scaling.scaleX, y / scaling.scaleY];
}

/**
 * Convert projection coordinates to display coordinates
 * Multiplies by scaling factors to account for aspect ratio differences
 */
function scaleCanvasToDisplay(x: number, y: number): [number, number] {
  const scaling = getCanvasScaling();
  return [x * scaling.scaleX, y * scaling.scaleY];
}

/**
 * Custom hook for coordinate transformation
 * Provides utilities for converting between pixel and sky coordinates
 */
export function useCoordinateTransform(): CoordinateTransformAPI {
  const pixelToSky = useCallback((x: number, y: number): [number, number] | null => {
    try {
      if (typeof window.Celestial === 'undefined') {
        console.warn('Celestial not available for coordinate transformation');
        return null;
      }

      // Try mapProjection first, fall back to map().projection()
      let projection = window.Celestial.mapProjection;

      if (!projection || !projection.invert) {
        // Try alternative way to get projection
        const map = window.Celestial.map;
        if (map && typeof map === 'function') {
          const mapObj = map();
          projection = mapObj?.projection?.();
        }
      }

      if (!projection || typeof projection.invert !== 'function') {
        console.warn('Projection not available or invert not a function', projection);
        return null;
      }

      // Convert display coordinates to projection space, accounting for aspect ratio
      const [canvasX, canvasY] = scaleDisplayToCanvas(x, y);

      // Convert pixel coordinates to sky coordinates
      const result = projection.invert([canvasX, canvasY]);
      if (!result || !Array.isArray(result) || result.length < 2) {
        console.warn('Invalid projection result:', result);
        return null;
      }
      const [ra, dec] = result;

      // Normalize RA to 0-360 range
      let normalizedRa = ra % 360;
      if (normalizedRa < 0) {
        normalizedRa += 360;
      }

      // Clamp Dec to -90 to +90 range
      const clampedDec = Math.max(-90, Math.min(90, dec));

      return [normalizedRa, clampedDec];
    } catch (err) {
      console.error('Error converting pixel to sky coordinates:', err);
      return null;
    }
  }, []);

  const skyToPixel = useCallback((ra: number, dec: number): [number, number] | null => {
    try {
      if (typeof window.Celestial === 'undefined') {
        console.warn('Celestial not available for coordinate transformation');
        return null;
      }

      // Try mapProjection first, fall back to map().projection()
      let projection = window.Celestial.mapProjection;

      if (!projection || typeof projection !== 'function') {
        // Try alternative way to get projection
        const map = window.Celestial.map;
        if (map && typeof map === 'function') {
          const mapObj = map();
          projection = mapObj?.projection?.();
        }
      }

      if (!projection || typeof projection !== 'function') {
        console.warn('Projection not available or not callable', projection);
        return null;
      }

      // Convert sky coordinates to pixel coordinates
      const result = projection([ra, dec]);
      if (!result || !Array.isArray(result) || result.length < 2) {
        console.warn('Invalid projection result:', result);
        return null;
      }
      const [canvasX, canvasY] = result;

      // Convert to display coordinates (pass-through on HiDPI displays)
      const [x, y] = scaleCanvasToDisplay(canvasX, canvasY);

      return [x, y];
    } catch (err) {
      console.error('Error converting sky to pixel coordinates:', err);
      return null;
    }
  }, []);

  const isVisible = useCallback((ra: number, dec: number): boolean => {
    try {
      if (typeof window.Celestial === 'undefined') {
        return false;
      }

      // Use d3-celestial's clip function to check visibility
      return window.Celestial.clip([ra, dec]);
    } catch (err) {
      console.error('Error checking visibility:', err);
      return false;
    }
  }, []);

  const getProjection = useCallback(() => {
    try {
      if (typeof window.Celestial === 'undefined') {
        return null;
      }
      return window.Celestial.mapProjection;
    } catch (err) {
      console.error('Error getting projection:', err);
      return null;
    }
  }, []);

  return {
    pixelToSky,
    skyToPixel,
    isVisible,
    getProjection
  };
}
