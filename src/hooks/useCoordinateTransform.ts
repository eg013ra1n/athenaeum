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
 * Pass-through for display to canvas coordinates
 * d3-celestial's projection expects logical (CSS) pixel coordinates,
 * not physical pixels. Canvas.width/height include devicePixelRatio scaling,
 * but the projection works in CSS pixel space, so no conversion needed.
 */
function scaleDisplayToCanvas(x: number, y: number): [number, number] {
  return [x, y];
}

/**
 * Pass-through for canvas to display coordinates
 * d3-celestial's projection returns logical (CSS) pixel coordinates
 * that already match the display coordinate space.
 */
function scaleCanvasToDisplay(x: number, y: number): [number, number] {
  return [x, y];
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

      // Convert display coordinates to canvas space (pass-through on HiDPI displays)
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
