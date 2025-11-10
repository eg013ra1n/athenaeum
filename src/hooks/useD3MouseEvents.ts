/**
 * Custom hook for mouse event handling
 * Provides standardized event handling for SVG drawing operations
 */

import { useCallback, useRef } from 'react';

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
    selection: SVGSVGElement | any,
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
  detachMouseHandlers: (selection: SVGSVGElement | any) => void;
}

/**
 * Get pointer coordinates relative to an SVG element
 * Mimics d3.pointer() behavior without requiring d3 dependency
 */
function getPointerCoordinates(event: MouseEvent, svgElement?: SVGSVGElement): [number, number] {
  if (!svgElement) {
    // If no element specified, use event properties
    return [event.clientX, event.clientY];
  }

  const rect = svgElement.getBoundingClientRect();
  const x = event.clientX - rect.left;
  const y = event.clientY - rect.top;
  return [x, y];
}

/**
 * Custom hook for mouse event handling
 */
export function useD3MouseEvents(): D3MouseEventAPI {
  const handlersRef = useRef<MouseEventHandlers>({});
  const svgElementRef = useRef<SVGSVGElement | null>(null);

  const attachMouseHandlers = useCallback(
    (selection: any, handlers: MouseEventHandlers) => {
      handlersRef.current = handlers;

      // Get the SVG element from d3 selection
      const svgElement = selection.node ? selection.node() : selection;
      svgElementRef.current = svgElement;

      if (handlers.onMouseDown) {
        selection.on('mousedown', function(event: MouseEvent) {
          const [x, y] = getPointerCoordinates(event, svgElement);
          handlers.onMouseDown?.(x, y, event);
        });
      }

      if (handlers.onMouseMove) {
        selection.on('mousemove', function(event: MouseEvent) {
          const [x, y] = getPointerCoordinates(event, svgElement);
          handlers.onMouseMove?.(x, y, event);
        });
      }

      if (handlers.onMouseUp) {
        selection.on('mouseup', function(event: MouseEvent) {
          const [x, y] = getPointerCoordinates(event, svgElement);
          handlers.onMouseUp?.(x, y, event);
        });
      }

      if (handlers.onDblClick) {
        selection.on('dblclick', function(event: MouseEvent) {
          const [x, y] = getPointerCoordinates(event, svgElement);
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
        return getPointerCoordinates(event, svgElement);
      } catch (err) {
        console.error('Error getting mouse coordinates:', err);
        return null;
      }
    },
    []
  );

  const detachMouseHandlers = useCallback((selection: any) => {
    selection.on('mousedown', null).on('mousemove', null).on('mouseup', null).on('dblclick', null);
  }, []);

  return {
    attachMouseHandlers,
    getMouseCoordinates,
    detachMouseHandlers
  };
}
