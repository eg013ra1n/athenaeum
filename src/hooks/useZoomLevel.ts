/**
 * Hook to monitor d3-celestial zoom level
 * Returns current zoom scale and a boolean indicating if we're zoomed in
 */

import { useState, useEffect } from 'react';

export interface ZoomState {
  /** Current zoom scale (1.0 = no zoom, higher = zoomed in) */
  scale: number;

  /** True if zoomed in beyond threshold, false if at default view */
  isZoomedIn: boolean;

  /** Threshold scale above which we consider "zoomed in" */
  zoomThreshold: number;
}

/**
 * Monitor d3-celestial zoom level
 *
 * @param zoomThreshold - Scale above which we consider "zoomed in" (default: 1.5)
 * @returns ZoomState object with current scale and isZoomedIn flag
 */
export function useZoomLevel(zoomThreshold: number = 1.5): ZoomState {
  const [scale, setScale] = useState(1.0);

  useEffect(() => {
    // Function to check current zoom level from d3-celestial
    const updateZoom = () => {
      if (window.Celestial && window.Celestial.scale) {
        const currentScale = window.Celestial.scale();
        setScale(currentScale);
      }
    };

    // Initial check
    updateZoom();

    // Listen for d3-celestial zoom events
    // d3-celestial emits 'zoomend' event on the container
    const container = document.getElementById('celestial-map');
    if (container) {
      container.addEventListener('zoomend', updateZoom);

      // Also listen for redraw events which may indicate zoom changes
      container.addEventListener('redraw', updateZoom);

      return () => {
        container.removeEventListener('zoomend', updateZoom);
        container.removeEventListener('redraw', updateZoom);
      };
    }
  }, []);

  return {
    scale,
    isZoomedIn: scale > zoomThreshold,
    zoomThreshold,
  };
}
