/**
 * Custom hook for managing SVG overlay layer on d3-celestial
 * Handles overlay creation, coordinate transformation, and event management
 */

import { useEffect, useRef, useState } from 'react';

export interface SVGOverlayConfig {
  containerId: string;
  pointerEvents?: 'all' | 'none';
}

export interface SVGOverlayAPI {
  getSvg: () => SVGSVGElement | null;
  enable: () => void;
  disable: () => void;
  clear: () => void;
  isEnabled: () => boolean;
}

/**
 * Custom hook for SVG overlay management
 * Creates and manages transparent SVG layer above d3-celestial canvas
 */
export function useSvgOverlay(config: SVGOverlayConfig): SVGOverlayAPI {
  const svgRef = useRef<SVGSVGElement | null>(null);
  const [enabled, setEnabled] = useState(false);

  useEffect(() => {
    // Create SVG overlay layer
    const container = document.getElementById(config.containerId);
    if (!container) {
      console.warn(`Container with id "${config.containerId}" not found`);
      return;
    }

    // Check if SVG already exists
    let svg = container.querySelector('.selection-overlay') as SVGSVGElement;
    if (svg) {
      svgRef.current = svg;
      return;
    }

    // Ensure container has relative positioning for absolute SVG overlay
    if (getComputedStyle(container).position === 'static') {
      container.style.position = 'relative';
    }

    // Create new SVG overlay
    svg = document.createElementNS('http://www.w3.org/2000/svg', 'svg');
    svg.setAttribute('class', 'selection-overlay');
    svg.setAttribute('preserveAspectRatio', 'none');
    svg.style.position = 'absolute';
    svg.style.top = '0';
    svg.style.left = '0';
    svg.style.width = '100%';
    svg.style.height = '100%';
    svg.style.pointerEvents = config.pointerEvents || 'none';
    svg.style.zIndex = '10';

    container.appendChild(svg);
    svgRef.current = svg;

    // Cleanup function
    return () => {
      // Don't remove SVG on unmount - it should persist
      // Just clear event listeners
      if (svgRef.current) {
        svgRef.current.onmousedown = null;
        svgRef.current.onmousemove = null;
        svgRef.current.onmouseup = null;
        svgRef.current.onclick = null;
      }
    };
  }, [config.containerId]);

  const getSvg = () => svgRef.current;

  const enable = () => {
    if (svgRef.current) {
      svgRef.current.style.pointerEvents = 'all';
      setEnabled(true);
    }
  };

  const disable = () => {
    if (svgRef.current) {
      svgRef.current.style.pointerEvents = 'none';
      setEnabled(false);
    }
  };

  const clear = () => {
    if (svgRef.current) {
      while (svgRef.current.firstChild) {
        svgRef.current.removeChild(svgRef.current.firstChild);
      }
    }
  };

  const isEnabled = () => enabled;

  return { getSvg, enable, disable, clear, isEnabled };
}
