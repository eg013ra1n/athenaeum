/**
 * Custom hook for circle-based region selection on sky map
 * Handles drawing and querying frames within a circle
 */

import { useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useSvgOverlay } from './useSvgOverlay';
import { useCoordinateTransform } from './useCoordinateTransform';
import { useD3MouseEvents } from './useD3MouseEvents';
import { angularDistance } from '../utils/coordinates';
import { SelectionResult } from '../types/selection';

export interface CircleSelectionAPI {
  /**
   * Start circle selection mode
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

interface CircleState {
  centerPixel: [number, number] | null;
  centerSky: [number, number] | null;
  radiusDegrees: number;
  isDrawing: boolean;
}

/**
 * Custom hook for circle-based region selection
 * Manages drawing a circle and querying frames within it
 */
export function useCircleSelection(): CircleSelectionAPI {
  const svgOverlay = useSvgOverlay({ containerId: 'celestial-map' });
  const coordinateTransform = useCoordinateTransform();
  const mouseEvents = useD3MouseEvents();

  const stateRef = useRef<CircleState>({
    centerPixel: null,
    centerSky: null,
    radiusDegrees: 0,
    isDrawing: false
  });

  const circleElementRef = useRef<SVGCircleElement | null>(null);
  const callbackRef = useRef<((result: SelectionResult) => void) | null>(null);

  const startSelection = (onComplete: (result: SelectionResult) => void) => {
    callbackRef.current = onComplete;

    const svg = svgOverlay.getSvg();
    if (!svg) {
      console.error('SVG overlay not available');
      return;
    }

    // Enable overlay for interaction
    svgOverlay.enable();

    // Clear any previous circle
    svgOverlay.clear();

    // Reset state
    stateRef.current = {
      centerPixel: null,
      centerSky: null,
      radiusDegrees: 0,
      isDrawing: false
    };

    // Attach mouse handlers for circle drawing
    mouseEvents.attachMouseHandlers(svg, {
      onMouseDown: (x: number, y: number) => {
        const state = stateRef.current;
        state.centerPixel = [x, y];
        state.centerSky = coordinateTransform.pixelToSky(x, y);
        state.isDrawing = true;

        // Create circle element
        const circle = document.createElementNS('http://www.w3.org/2000/svg', 'circle');
        circle.setAttribute('class', 'selection-circle-active');
        circle.setAttribute('cx', String(x));
        circle.setAttribute('cy', String(y));
        circle.setAttribute('r', '0');
        circle.style.fill = 'rgba(59, 130, 246, 0.15)';
        circle.style.stroke = '#3b82f6';
        circle.style.strokeWidth = '2';

        svg.appendChild(circle);
        circleElementRef.current = circle;
      },

      onMouseMove: (x: number, y: number) => {
        const state = stateRef.current;
        if (!state.isDrawing || !state.centerPixel || !state.centerSky) return;

        // Calculate pixel distance
        const dx = x - state.centerPixel[0];
        const dy = y - state.centerPixel[1];
        const pixelRadius = Math.sqrt(dx * dx + dy * dy);

        // Convert boundary pixel to sky coordinates
        const boundaryPixel = coordinateTransform.pixelToSky(x, y);
        if (!boundaryPixel) return;

        // Calculate angular distance (radius in degrees)
        state.radiusDegrees = angularDistance(
          state.centerSky[0],
          state.centerSky[1],
          boundaryPixel[0],
          boundaryPixel[1]
        );

        // Update circle visual
        if (circleElementRef.current) {
          circleElementRef.current.setAttribute('r', String(pixelRadius));
        }

        // Show radius info in title
        if (circleElementRef.current) {
          const radiusArcmin = state.radiusDegrees * 60;
          circleElementRef.current.setAttribute(
            'title',
            `Radius: ${radiusArcmin.toFixed(1)}' (${state.radiusDegrees.toFixed(4)}°)`
          );
        }
      },

      onMouseUp: async () => {
        const state = stateRef.current;
        if (!state.isDrawing || !state.centerSky) {
          state.isDrawing = false;
          return;
        }

        state.isDrawing = false;

        // Query backend for frames in circle
        try {
          const result = await invoke<SelectionResult>('query_frames_in_circle', {
            ra: state.centerSky[0],
            dec: state.centerSky[1],
            radius_degrees: state.radiusDegrees
          });

          // Call completion callback with results
          if (callbackRef.current) {
            callbackRef.current(result);
          }
        } catch (err) {
          console.error('Error querying frames in circle:', err);
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
      centerPixel: null,
      centerSky: null,
      radiusDegrees: 0,
      isDrawing: false
    };

    circleElementRef.current = null;
    callbackRef.current = null;
  };

  const isActive = () => stateRef.current.isDrawing;

  return {
    startSelection,
    cancelSelection,
    isActive
  };
}
