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
 * Returns display coordinates in CSS pixel space (no scaling needed for HiDPI)
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

      // Get the SVG element from d3 selection or use directly
      const svgElement = selection.node ? selection.node() : selection;
      svgElementRef.current = svgElement;

      console.log('Attaching handlers to element:', svgElement, 'Tag:', svgElement?.tagName);

      // Use native event listeners for SVGSVGElement
      if (svgElement && typeof svgElement.addEventListener === 'function') {
        console.log('Element supports addEventListener, attaching events');
        if (handlers.onMouseDown) {
          const mouseDownListener = (event: MouseEvent) => {
            console.log('mousedown event fired');
            const [x, y] = getPointerCoordinates(event, svgElement);
            handlers.onMouseDown?.(x, y, event);
          };
          svgElement.addEventListener('mousedown', mouseDownListener);
          console.log('mousedown listener attached');
          // Store for cleanup
          (svgElement as any).__mouseDownListener = mouseDownListener;
        }

        if (handlers.onMouseMove) {
          const mouseMoveListener = (event: MouseEvent) => {
            const [x, y] = getPointerCoordinates(event, svgElement);
            handlers.onMouseMove?.(x, y, event);
          };
          svgElement.addEventListener('mousemove', mouseMoveListener);
          (svgElement as any).__mouseMoveListener = mouseMoveListener;
        }

        if (handlers.onMouseUp) {
          const mouseUpListener = (event: MouseEvent) => {
            const [x, y] = getPointerCoordinates(event, svgElement);
            handlers.onMouseUp?.(x, y, event);
          };
          svgElement.addEventListener('mouseup', mouseUpListener);
          (svgElement as any).__mouseUpListener = mouseUpListener;
        }

        if (handlers.onDblClick) {
          const dblClickListener = (event: MouseEvent) => {
            const [x, y] = getPointerCoordinates(event, svgElement);
            handlers.onDblClick?.(x, y, event);
          };
          svgElement.addEventListener('dblclick', dblClickListener);
          (svgElement as any).__dblClickListener = dblClickListener;
        }
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
    const svgElement = selection.node ? selection.node() : selection;

    if (svgElement && typeof svgElement.removeEventListener === 'function') {
      // Remove stored listeners
      if ((svgElement as any).__mouseDownListener) {
        svgElement.removeEventListener('mousedown', (svgElement as any).__mouseDownListener);
        delete (svgElement as any).__mouseDownListener;
      }
      if ((svgElement as any).__mouseMoveListener) {
        svgElement.removeEventListener('mousemove', (svgElement as any).__mouseMoveListener);
        delete (svgElement as any).__mouseMoveListener;
      }
      if ((svgElement as any).__mouseUpListener) {
        svgElement.removeEventListener('mouseup', (svgElement as any).__mouseUpListener);
        delete (svgElement as any).__mouseUpListener;
      }
      if ((svgElement as any).__dblClickListener) {
        svgElement.removeEventListener('dblclick', (svgElement as any).__dblClickListener);
        delete (svgElement as any).__dblClickListener;
      }
    }
  }, []);

  return {
    attachMouseHandlers,
    getMouseCoordinates,
    detachMouseHandlers
  };
}
