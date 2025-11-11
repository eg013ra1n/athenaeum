/**
 * Custom hook for rectangle-based region selection on sky map
 * Handles drawing and querying frames within a rectangle
 */

import { useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { SelectionResult } from '../types/selection';
import { SVGOverlayAPI } from './useSvgOverlay';
import { CoordinateTransformAPI } from './useCoordinateTransform';
import { D3MouseEventAPI } from './useD3MouseEvents';

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
  startSky: [number, number] | null;
  currentPixel: [number, number] | null;
  currentSky: [number, number] | null;
  isDrawing: boolean;
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
    startSky: null,
    currentPixel: null,
    currentSky: null,
    isDrawing: false
  });

  const rectangleElementRef = useRef<SVGRectElement | null>(null);
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
      startSky: null,
      currentPixel: null,
      currentSky: null,
      isDrawing: false
    };

    // Attach mouse handlers for rectangle drawing
    console.log('Attaching mouse handlers to SVG');
    mouseEvents.attachMouseHandlers(svg, {
      onMouseDown: (x: number, y: number) => {
        console.log('Rectangle onMouseDown at:', x, y);
        const state = stateRef.current;
        state.startPixel = [x, y];
        state.startSky = coordinateTransform.pixelToSky(x, y);
        state.currentPixel = [x, y];
        state.currentSky = coordinateTransform.pixelToSky(x, y);
        state.isDrawing = true;

        console.log('Start sky coords:', state.startSky);

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

        // Update current position
        state.currentPixel = [x, y];
        state.currentSky = coordinateTransform.pixelToSky(x, y);

        // Calculate rectangle dimensions
        const startX = Math.min(state.startPixel[0], x);
        const startY = Math.min(state.startPixel[1], y);
        const width = Math.abs(x - state.startPixel[0]);
        const height = Math.abs(y - state.startPixel[1]);

        // Update rectangle visual
        if (rectangleElementRef.current) {
          rectangleElementRef.current.setAttribute('x', String(startX));
          rectangleElementRef.current.setAttribute('y', String(startY));
          rectangleElementRef.current.setAttribute('width', String(width));
          rectangleElementRef.current.setAttribute('height', String(height));

          // Show bounds info in title
          if (state.startSky && state.currentSky) {
            const raMin = Math.min(state.startSky[0], state.currentSky[0]);
            const raMax = Math.max(state.startSky[0], state.currentSky[0]);
            const decMin = Math.min(state.startSky[1], state.currentSky[1]);
            const decMax = Math.max(state.startSky[1], state.currentSky[1]);

            rectangleElementRef.current.setAttribute(
              'title',
              `RA: ${raMin.toFixed(2)}° - ${raMax.toFixed(2)}°\nDec: ${decMin.toFixed(2)}° - ${decMax.toFixed(2)}°`
            );
          }
        }
      },

      onMouseUp: async () => {
        const state = stateRef.current;
        if (!state.isDrawing || !state.startSky || !state.currentSky) {
          state.isDrawing = false;
          return;
        }

        state.isDrawing = false;

        // Calculate bounds
        const startRA = state.startSky[0];
        const currentRA = state.currentSky[0];

        // Calculate both direct and wrap-around angular distances
        const directDistance = Math.abs(currentRA - startRA);
        const wrapDistance = 360 - directDistance;

        // Determine if selection crosses 0°/360° boundary
        // If wrap distance is smaller, the selection crosses the boundary
        const crossesBoundary = wrapDistance < directDistance;

        let raMin: number, raMax: number;

        if (crossesBoundary) {
          // Selection crosses 0°/360° boundary
          // In this case, we want the larger values on one side and smaller on the other
          // The "min" should be the larger value, "max" should be the smaller value
          // This creates a range like [300, 360] + [0, 60] which wraps around
          raMin = Math.max(startRA, currentRA);
          raMax = Math.min(startRA, currentRA);
          console.log('🔄 RA crosses boundary:', { startRA, currentRA, raMin, raMax });
        } else {
          // Normal case: selection doesn't cross boundary
          raMin = Math.min(startRA, currentRA);
          raMax = Math.max(startRA, currentRA);
          console.log('✅ RA normal range:', { startRA, currentRA, raMin, raMax });
        }

        // Dec: simple min/max (declination is -90 to +90, no wrap-around)
        const decMin = Math.min(state.startSky[1], state.currentSky[1]);
        const decMax = Math.max(state.startSky[1], state.currentSky[1]);

        // Query backend for frames in rectangle
        try {
          const bounds = {
            ra_min: raMin,
            ra_max: raMax,
            dec_min: decMin,
            dec_max: decMax
          };
          console.log('Querying frames with bounds:', bounds);

          const result = await invoke<SelectionResult>('query_frames_in_bounds', {
            bounds: {
              ra_min: raMin,
              ra_max: raMax,
              dec_min: decMin,
              dec_max: decMax
            }
          });

          // Call completion callback with results
          if (callbackRef.current) {
            callbackRef.current(result);
          }
        } catch (err) {
          console.error('Error querying frames in rectangle:', err);
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
      startSky: null,
      currentPixel: null,
      currentSky: null,
      isDrawing: false
    };

    rectangleElementRef.current = null;
    callbackRef.current = null;
  };

  const isActive = () => stateRef.current.isDrawing;

  return {
    startSelection,
    cancelSelection,
    isActive
  };
}
