/**
 * Custom hook for rectangle-based region selection on sky map
 * Uses pixel-space approach for stable drawing, converts to sky coordinates only at completion
 */

import { useRef } from 'react';
import { api } from '../api';
import { SelectionResult, SelectionCandidates } from '../types/selection';
import { SVGOverlayAPI } from './useSvgOverlay';
import { CoordinateTransformAPI } from './useCoordinateTransform';
import { D3MouseEventAPI } from './useD3MouseEvents';
import { calculateSkyBounds, PixelRectangle } from '../utils/skyBoundsCalculator';
import { clampRectangleToProjection } from '../utils/projectionBounds';

// Pixel-based selection limits
const MIN_SELECTION_SIZE = 10;  // Minimum 10x10 pixels

export interface RectangleSelectionAPI {
  /**
   * Start rectangle selection mode
   * @param onComplete - Callback with selection results
   */
  startSelection: (onComplete: (result: SelectionResult) => void) => void;

  /**
   * Cancel current selection
   */
  cancelSelection: () => void;

  /**
   * Check if selection is active
   */
  isActive: () => boolean;
}

interface RectangleState {
  startPixel: [number, number] | null;
  currentPixel: [number, number] | null;
  isDrawing: boolean;
  hasShownBoundaryWarning: boolean;
}

/**
 * Custom hook for rectangle-based region selection
 * Manages drawing a rectangle and querying frames within it
 */
export function useRectangleSelection(
  svgOverlay: SVGOverlayAPI,
  coordinateTransform: CoordinateTransformAPI,
  mouseEvents: D3MouseEventAPI
): RectangleSelectionAPI {

  const stateRef = useRef<RectangleState>({
    startPixel: null,
    currentPixel: null,
    isDrawing: false,
    hasShownBoundaryWarning: false
  });

  const rectangleElementRef = useRef<SVGRectElement | null>(null);
  const textElementRef = useRef<SVGTextElement | null>(null);
  const callbackRef = useRef<((result: SelectionResult) => void) | null>(null);

  const startSelection = (onComplete: (result: SelectionResult) => void) => {
    callbackRef.current = onComplete;

    const svg = svgOverlay.getSvg();
    if (!svg) {
      console.error('SVG overlay not available');
      return;
    }

    console.log('Rectangle selection started, SVG element:', svg);

    // Enable overlay for interaction
    svgOverlay.enable();
    console.log('SVG overlay enabled');

    // Clear any previous rectangle
    svgOverlay.clear();

    // Reset state
    stateRef.current = {
      startPixel: null,
      currentPixel: null,
      isDrawing: false,
      hasShownBoundaryWarning: false
    };

    // Attach mouse handlers for rectangle drawing
    console.log('Attaching mouse handlers to SVG');
    mouseEvents.attachMouseHandlers(svg, {
      onMouseDown: (x: number, y: number) => {
        console.log('Rectangle onMouseDown at:', x, y);
        const state = stateRef.current;
        state.startPixel = [x, y];
        state.currentPixel = [x, y];
        state.isDrawing = true;

        // Create rectangle element
        const rect = document.createElementNS('http://www.w3.org/2000/svg', 'rect');
        rect.setAttribute('class', 'selection-rectangle-active');
        rect.setAttribute('x', String(x));
        rect.setAttribute('y', String(y));
        rect.setAttribute('width', '0');
        rect.setAttribute('height', '0');
        rect.style.fill = 'rgba(16, 185, 129, 0.15)'; // Green with transparency
        rect.style.stroke = '#10b981'; // Green border
        rect.style.strokeWidth = '2';

        svg.appendChild(rect);
        rectangleElementRef.current = rect;
        console.log('Rectangle element created at:', x, y);
      },

      onMouseMove: (x: number, y: number) => {
        const state = stateRef.current;
        if (!state.isDrawing || !state.startPixel) return;

        // Clamp rectangle so ALL corners and edges stay within projection
        const [clampedX, clampedY] = clampRectangleToProjection(
          x,
          y,
          state.startPixel[0],
          state.startPixel[1],
          coordinateTransform.pixelToSky,
          5  // Sample 5 points per edge
        );

        const isClamped = (clampedX !== x || clampedY !== y);

        // Update state
        state.currentPixel = [clampedX, clampedY];

        // Calculate dimensions
        const width = Math.abs(clampedX - state.startPixel[0]);
        const height = Math.abs(clampedY - state.startPixel[1]);

        // Determine color - yellow/red if clamped to boundary
        let strokeColor: string;
        let fillColor: string;
        if (isClamped) {
          strokeColor = '#f59e0b'; // Yellow/Orange when at boundary
          fillColor = 'rgba(245, 158, 11, 0.15)';
        } else {
          strokeColor = '#10b981'; // Green
          fillColor = 'rgba(16, 185, 129, 0.15)';
        }

        // Show warning if clamped to boundary
        if (isClamped && !state.hasShownBoundaryWarning) {
          console.log('⚠️ Selection clamped to celestial map boundary');
          state.hasShownBoundaryWarning = true;
        }

        // Calculate rectangle position (top-left corner)
        const startX = Math.min(state.startPixel[0], clampedX);
        const startY = Math.min(state.startPixel[1], clampedY);

        // Update rectangle visual
        if (rectangleElementRef.current) {
          rectangleElementRef.current.setAttribute('x', String(startX));
          rectangleElementRef.current.setAttribute('y', String(startY));
          rectangleElementRef.current.setAttribute('width', String(width));
          rectangleElementRef.current.setAttribute('height', String(height));
          rectangleElementRef.current.style.stroke = strokeColor;
          rectangleElementRef.current.style.fill = fillColor;
        }
      },

      onMouseUp: async () => {
        const state = stateRef.current;
        if (!state.isDrawing || !state.startPixel || !state.currentPixel) {
          state.isDrawing = false;
          return;
        }

        state.isDrawing = false;

        // Check minimum size
        const width = Math.abs(state.currentPixel[0] - state.startPixel[0]);
        const height = Math.abs(state.currentPixel[1] - state.startPixel[1]);

        if (width < MIN_SELECTION_SIZE || height < MIN_SELECTION_SIZE) {
          console.log('⚠️ Selection too small, ignoring (minimum 10×10 pixels)');
          // Clear the rectangle visual
          svgOverlay.clear();
          rectangleElementRef.current = null;
          textElementRef.current = null;
          return;
        }

        // Define pixel rectangle
        const pixelRect: PixelRectangle = {
          x1: Math.min(state.startPixel[0], state.currentPixel[0]),
          y1: Math.min(state.startPixel[1], state.currentPixel[1]),
          x2: Math.max(state.startPixel[0], state.currentPixel[0]),
          y2: Math.max(state.startPixel[1], state.currentPixel[1])
        };

        console.log('📐 Pixel rectangle:', pixelRect);

        // Convert to sky bounds using multi-point sampling
        const skyBounds = calculateSkyBounds(
          pixelRect,
          coordinateTransform.pixelToSky
        );

        if (!skyBounds) {
          console.error('❌ Failed to convert pixel rectangle to sky bounds');
          // Clear the rectangle visual
          svgOverlay.clear();
          rectangleElementRef.current = null;
          textElementRef.current = null;
          return;
        }

        console.log('✅ Sky bounds:', skyBounds);

        // Query backend for candidate frames in the sky bounding box
        try {
          const candidates = await api.invoke<SelectionCandidates>('query_frames_in_bounds', {
            bounds: {
              ra_min: skyBounds.ra_min,
              ra_max: skyBounds.ra_max,
              dec_min: skyBounds.dec_min,
              dec_max: skyBounds.dec_max,
              crosses_meridian: skyBounds.crosses_meridian
            }
          });

          console.log(`📦 Got ${candidates.count} candidate frames from bounding box`);

          // Pixel-space verification: project each candidate's RA/Dec to screen
          // and check if it falls inside the drawn rectangle.
          const verified: { id: number; exposure: number }[] = [];
          for (const c of candidates.candidates) {
            const pixel = coordinateTransform.skyToPixel(c.ra, c.dec);
            if (!pixel) continue;
            const [px, py] = pixel;
            if (px >= pixelRect.x1 && px <= pixelRect.x2 &&
                py >= pixelRect.y1 && py <= pixelRect.y2) {
              verified.push({ id: c.id, exposure: c.exposure });
            }
          }
          console.log(`🎯 Verified ${verified.length} frames in pixel rectangle (from ${candidates.count} candidates)`);

          const totalExposure = verified.reduce((sum, f) => sum + f.exposure, 0);
          const result: SelectionResult = {
            frameIds: verified.map(f => f.id),
            count: verified.length,
            totalExposureSeconds: totalExposure,
          };

          if (callbackRef.current) {
            callbackRef.current(result);
          }
        } catch (err) {
          console.error('❌ Error querying frames in rectangle:', err);
        } finally {
          // Always clear the rectangle visual after query completes
          svgOverlay.clear();
          rectangleElementRef.current = null;
          textElementRef.current = null;
        }
      }
    });
  };

  const cancelSelection = () => {
    const svg = svgOverlay.getSvg();
    if (svg) {
      mouseEvents.detachMouseHandlers(svg);
      svgOverlay.clear();
      svgOverlay.disable();
    }

    stateRef.current = {
      startPixel: null,
      currentPixel: null,
      isDrawing: false,
      hasShownBoundaryWarning: false
    };

    rectangleElementRef.current = null;
    textElementRef.current = null;
    callbackRef.current = null;
  };

  const isActive = () => stateRef.current.isDrawing;

  return {
    startSelection,
    cancelSelection,
    isActive
  };
}
