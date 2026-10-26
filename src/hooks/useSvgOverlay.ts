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
 * Creates and manages transparent SVG layer above d3-celestial canvas.
 * The container must be in the document when the effect runs (the Sky
 * Chart attaches its persistent host in a layout effect); the overlay is
 * created once and reused, since it lives in that persistent host.
 */
export function useSvgOverlay(config: SVGOverlayConfig): SVGOverlayAPI {
  const svgRef = useRef<SVGSVGElement | null>(null);
  const [enabled, setEnabled] = useState(false);

  useEffect(() => {
    const container = document.getElementById(config.containerId);
    if (!container) {
      console.error(`[useSvgOverlay] container "${config.containerId}" not found`);
      return;
    }

    let svg = container.querySelector('.selection-overlay') as SVGSVGElement | null;
    if (!svg) {
      if (getComputedStyle(container).position === 'static') {
        container.style.position = 'relative';
      }
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
    }
    svgRef.current = svg;

    return () => {
      // The SVG persists with its container; only drop the listeners.
      const el = svgRef.current;
      if (el) {
        el.onmousedown = null;
        el.onmousemove = null;
        el.onmouseup = null;
        el.onclick = null;
      }
    };
  }, [config.containerId, config.pointerEvents]);

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
