/**
 * Custom hook for managing SVG overlay layer on d3-celestial
 * Handles overlay creation, coordinate transformation, and event management
 */

import { useEffect, useRef, useState } from 'react';
import * as d3 from 'd3';

export interface SVGOverlayConfig {
  containerId: string;
  pointerEvents?: 'all' | 'none';
}

export interface SVGOverlayAPI {
  getSvg: () => d3.Selection<SVGSVGElement, unknown, HTMLElement, unknown> | null;
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
  const svgRef = useRef<d3.Selection<SVGSVGElement, unknown, HTMLElement, unknown> | null>(null);
  const [enabled, setEnabled] = useState(false);

  useEffect(() => {
    // Create SVG overlay layer
    const container = document.getElementById(config.containerId);
    if (!container) {
      console.warn(`Container with id "${config.containerId}" not found`);
      return;
    }

    // Check if SVG already exists
    const existingSvg = container.querySelector('.selection-overlay');
    if (existingSvg) {
      svgRef.current = d3.select(existingSvg as SVGSVGElement);
      return;
    }

    // Create new SVG overlay
    const svg = d3
      .select(container)
      .append('svg')
      .attr('class', 'selection-overlay')
      .style('position', 'absolute')
      .style('top', '0')
      .style('left', '0')
      .style('width', '100%')
      .style('height', '100%')
      .style('pointer-events', config.pointerEvents || 'none')
      .style('z-index', '10');

    svgRef.current = svg;

    // Cleanup function
    return () => {
      // Don't remove SVG on unmount - it should persist
      // Just clear event listeners
      if (svgRef.current) {
        svgRef.current.on('mousedown', null).on('mousemove', null).on('mouseup', null).on('click', null);
      }
    };
  }, [config.containerId]);

  const getSvg = () => svgRef.current;

  const enable = () => {
    if (svgRef.current) {
      svgRef.current.style('pointer-events', 'all');
      setEnabled(true);
    }
  };

  const disable = () => {
    if (svgRef.current) {
      svgRef.current.style('pointer-events', 'none');
      setEnabled(false);
    }
  };

  const clear = () => {
    if (svgRef.current) {
      svgRef.current.selectAll('*').remove();
    }
  };

  const isEnabled = () => enabled;

  return { getSvg, enable, disable, clear, isEnabled };
}
