/**
 * Custom hook for D3-based mouse event handling
 * Provides standardized event handling for SVG drawing operations
 */

import { useCallback, useRef } from 'react';
import * as d3 from 'd3';

export interface MouseEventHandlers {
  onMouseDown?: (x: number, y: number, event: MouseEvent) => void;
  onMouseMove?: (x: number, y: number, event: MouseEvent) => void;
  onMouseUp?: (x: number, y: number, event: MouseEvent) => void;
  onDblClick?: (x: number, y: number, event: MouseEvent) => void;
}

export interface D3MouseEventAPI {
  /**
   * Attach mouse event handlers to an SVG element
   */
  attachMouseHandlers: (
    selection: d3.Selection<any, any, any, any>,
    handlers: MouseEventHandlers
  ) => void;

  /**
   * Get current mouse coordinates relative to SVG
   * @param svgElement - SVG element reference
   * @param event - Mouse event
   * @returns [x, y] coordinates or null
   */
  getMouseCoordinates: (svgElement: SVGSVGElement | null, event: MouseEvent) => [number, number] | null;

  /**
   * Detach all mouse event handlers
   */
  detachMouseHandlers: (selection: d3.Selection<any, any, any, any>) => void;
}

/**
 * Custom hook for D3 mouse event handling
 */
export function useD3MouseEvents(): D3MouseEventAPI {
  const handlersRef = useRef<MouseEventHandlers>({});

  const attachMouseHandlers = useCallback(
    (selection: d3.Selection<any, any, any, any>, handlers: MouseEventHandlers) => {
      handlersRef.current = handlers;

      if (handlers.onMouseDown) {
        selection.on('mousedown', function(event: MouseEvent) {
          const [x, y] = d3.pointer(event);
          handlers.onMouseDown?.(x, y, event);
        });
      }

      if (handlers.onMouseMove) {
        selection.on('mousemove', function(event: MouseEvent) {
          const [x, y] = d3.pointer(event);
          handlers.onMouseMove?.(x, y, event);
        });
      }

      if (handlers.onMouseUp) {
        selection.on('mouseup', function(event: MouseEvent) {
          const [x, y] = d3.pointer(event);
          handlers.onMouseUp?.(x, y, event);
        });
      }

      if (handlers.onDblClick) {
        selection.on('dblclick', function(event: MouseEvent) {
          const [x, y] = d3.pointer(event);
          handlers.onDblClick?.(x, y, event);
        });
      }
    },
    []
  );

  const getMouseCoordinates = useCallback(
    (svgElement: SVGSVGElement | null, event: MouseEvent): [number, number] | null => {
      if (!svgElement) return null;

      try {
        const [x, y] = d3.pointer(event, svgElement);
        return [x, y];
      } catch (err) {
        console.error('Error getting mouse coordinates:', err);
        return null;
      }
    },
    []
  );

  const detachMouseHandlers = useCallback((selection: d3.Selection<any, any, any, any>) => {
    selection.on('mousedown', null).on('mousemove', null).on('mouseup', null).on('dblclick', null);
  }, []);

  return {
    attachMouseHandlers,
    getMouseCoordinates,
    detachMouseHandlers
  };
}
