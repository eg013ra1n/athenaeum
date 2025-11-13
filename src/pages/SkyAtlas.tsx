import { useState, useEffect, useLayoutEffect, useRef, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useNavigate } from 'react-router-dom';
import { ImagingLocation } from '../types/models';
import { DrawingMode, SelectionResult } from '../types/selection';
import { useSvgOverlay } from '../hooks/useSvgOverlay';
import { useCoordinateTransform } from '../hooks/useCoordinateTransform';
import { useD3MouseEvents } from '../hooks/useD3MouseEvents';
import { useRectangleSelection } from '../hooks/useRectangleSelection';
import { useZoomLevel } from '../hooks/useZoomLevel';
import { useMapViewState } from '../hooks/useMapViewState';
import { SelectionToolbar } from '../components/SelectionToolbar';
import { SelectionDialog } from '../components/SelectionDialog';
import '../styles/celestial-overrides.css';

// Declare global Celestial and d3 from d3-celestial
declare global {
  interface Window {
    Celestial: any;
    d3: any;
  }
  const d3: any;
}

export default function SkyAtlas() {
  const containerRef = useRef<HTMLDivElement>(null);
  const mapInitialized = useRef(false);
  const resizeTimeoutRef = useRef<number | undefined>(undefined);

  const [locations, setLocations] = useState<ImagingLocation[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [mapReady, setMapReady] = useState(false);

  // Selection state
  const [drawingMode, setDrawingMode] = useState<DrawingMode>('none');
  const [selectionResult, setSelectionResult] = useState<SelectionResult | null>(null);
  const [showDialog, setShowDialog] = useState(false);

  // Custom hooks
  const svgOverlay = useSvgOverlay({ containerId: 'celestial-map' });
  const coordinateTransform = useCoordinateTransform();
  const mouseEvents = useD3MouseEvents();
  const rectangleSelection = useRectangleSelection(svgOverlay, coordinateTransform, mouseEvents);
  const zoomLevel = useZoomLevel(2.0); // Threshold: show FOV boxes when scale > 2.0
  const { getViewState, saveViewState } = useMapViewState();

  const navigate = useNavigate();

  // Fetch imaging locations from backend
  useEffect(() => {
    async function loadLocations() {
      try {
        const data = await invoke<ImagingLocation[]>('get_imaging_locations');
        setLocations(data);
        setLoading(false);
      } catch (err) {
        console.error('Failed to load imaging locations:', err);
        setError(err as string);
        setLoading(false);
      }
    }
    loadLocations();
  }, []);

  // Initialize d3-celestial using useLayoutEffect for synchronous DOM measurement
  useLayoutEffect(() => {
    if (loading || !containerRef.current || mapInitialized.current) return;

    // Check if Celestial is available
    if (typeof window.Celestial === 'undefined') {
      setError('Sky map library not loaded. Please refresh the page.');
      return;
    }

    const container = containerRef.current;

    // Single initialization attempt after layout settles
    const rafId = requestAnimationFrame(() => {
      const rect = container.getBoundingClientRect();

      // Check dimensions once
      if (rect.width === 0 || rect.height === 0) {
        console.error(`Container has no dimensions: ${rect.width}x${rect.height}`);
        setError('Unable to initialize sky map: container has no dimensions');
        return;
      }

      console.log(`Initializing sky map with dimensions: ${rect.width}x${rect.height}`);

      // Get saved view state from URL before initializing
      const savedState = getViewState();
      if (savedState.zoom !== null || (savedState.ra !== null && savedState.dec !== null)) {
        console.log('🔄 Will initialize with saved state:', savedState);
      }

      try {
        const config = {
          container: 'celestial-map',
          width: Math.floor(rect.width),
          projection: 'aitoff',
          transform: 'equatorial',
          center: (savedState.ra !== null && savedState.dec !== null)
            ? [savedState.ra, savedState.dec, 0]
            : null,
          orientationfixed: false,
          follow: "center",  // Prevents animation when setting initial center
          zoomlevel: null,
          zoomextend: 10,
          interactive: true,
          form: false,
          controls: true,
          datapath: '/data/',
          stars: {
            show: true,
            limit: 6,
            colors: true,
            style: { fill: '#ffffff', opacity: 0.8 },
            designation: true,
            designationLimit: 2.5,
            propername: true,
            propernameLimit: 1.5,
            size: 7,
            exponent: -0.28,
            data: 'stars.6.json'
          },
          dsos: {
            show: true,
            limit: 6,
            colors: true,
            style: { fill: '#cccccc', opacity: 1 },
            names: true,
            namesLimit: 6,
            size: null,
            exponent: 1.4,
            data: 'dsos.bright.json'
          },
          constellations: {
            show: true,
            names: true,
            namesType: 'name',
            nameStyle: { fill: '#cccc99', font: '14px Helvetica, Arial, sans-serif', align: 'center', baseline: 'middle' },
            lines: true,
            lineStyle: { stroke: '#cccccc', width: 1, opacity: 0.4 },
            bounds: false,
            boundStyle: { stroke: '#cccc00', width: 0.5, opacity: 0.8, dash: [2, 4] }
          },
          mw: {
            show: true,
            style: { fill: '#ffffff', opacity: 0.15 }
          },
          lines: {
            graticule: {
              show: true,
              stroke: '#cccccc',
              width: 0.6,
              opacity: 0.3,
              lon: { pos: [''], fill: '#eee', font: '10px Helvetica, Arial, sans-serif' },
              lat: { pos: [''], fill: '#eee', font: '10px Helvetica, Arial, sans-serif' }
            },
            equatorial: { show: true, stroke: '#aaaaaa', width: 1.3, opacity: 0.7 },
            ecliptic: { show: false, stroke: '#66cc66', width: 1.3, opacity: 0.7 },
            galactic: { show: false, stroke: '#cc6666', width: 1.3, opacity: 0.7 },
            supergalactic: { show: false, stroke: '#cc66cc', width: 1.3, opacity: 0.7 }
          },
          background: {
            fill: '#000000',
            opacity: 1,
            stroke: '#000000',
            width: 1.5
          },
          horizon: {
            show: false
          }
        };

        window.Celestial.display(config);
        mapInitialized.current = true;

        // Restore zoom after display (center is handled by config with follow:"center")
        if (savedState.zoom !== null) {
          const targetZoom = savedState.zoom; // Extract for closure
          requestAnimationFrame(() => {
            const projection = window.Celestial.mapProjection;
            if (projection && projection.scale) {
              const currentZoom = projection.scale();
              const zoomFactor = targetZoom / currentZoom;
              if (window.Celestial.zoomBy) {
                window.Celestial.zoomBy(zoomFactor);
                console.log('🔍 Restored zoom:', targetZoom);
              }
            }
          });
        }

        setMapReady(true);

        console.log('Sky map initialized successfully');
      } catch (err) {
        console.error('Failed to initialize sky map:', err);
        setError(`Failed to initialize sky map: ${err}`);
      }
    });

    return () => {
      cancelAnimationFrame(rafId);
    };
  }, [loading]);

  // Restore view state from URL after map initialization
  useEffect(() => {
    if (!mapReady || typeof window.Celestial === 'undefined') return;

    const viewState = getViewState();
    const { zoom, ra, dec } = viewState;

    // Only restore if we have state to restore
    if (zoom !== null || (ra !== null && dec !== null)) {
      console.log('🔄 Restoring view state from URL:', viewState);

      // Use requestAnimationFrame to ensure map is fully ready
      requestAnimationFrame(() => {
        // Restore center position
        if (ra !== null && dec !== null && window.Celestial.rotate) {
          window.Celestial.rotate({ center: [ra, dec, 0] });
          console.log('📍 Restored center:', [ra, dec]);
        }

        // Restore zoom level
        if (zoom !== null) {
          const projection = window.Celestial.mapProjection;
          if (projection && projection.scale) {
            const currentZoom = projection.scale();
            const zoomFactor = zoom / currentZoom;
            if (window.Celestial.zoomBy) {
              window.Celestial.zoomBy(zoomFactor);
              console.log('🔍 Restored zoom:', zoom, '(factor:', zoomFactor, ')');
            }
          }
        }
      });
    }
  }, [mapReady, getViewState]);

  // Save view state to URL on zoom/pan (debounced)
  useEffect(() => {
    if (!mapReady || typeof window.Celestial === 'undefined') return;

    let saveTimeout: number;

    const handleMapChange = () => {
      // Debounce to avoid excessive URL updates during interaction
      clearTimeout(saveTimeout);
      saveTimeout = window.setTimeout(() => {
        const projection = window.Celestial.mapProjection;
        const currentZoom = projection && projection.scale ? projection.scale() : null;
        const currentCenter = window.Celestial.rotate ? window.Celestial.rotate() : null;

        if (currentZoom !== null && currentCenter !== null) {
          saveViewState({
            zoom: currentZoom,
            ra: currentCenter[0],
            dec: currentCenter[1]
          });

          console.log('💾 Auto-saved view state:', { zoom: currentZoom, center: currentCenter });
        }
      }, 1000); // Save 1 second after user stops interacting
    };

    const container = document.getElementById('celestial-map');
    if (container) {
      // Listen for map interaction events
      container.addEventListener('zoomend', handleMapChange);
      container.addEventListener('redraw', handleMapChange);

      return () => {
        clearTimeout(saveTimeout);
        container.removeEventListener('zoomend', handleMapChange);
        container.removeEventListener('redraw', handleMapChange);
      };
    }
  }, [mapReady, saveViewState]);

  // Handle window resize with debounce
  useEffect(() => {
    if (!mapReady || !containerRef.current) return;

    const handleResize = () => {
      if (resizeTimeoutRef.current) {
        clearTimeout(resizeTimeoutRef.current);
      }

      resizeTimeoutRef.current = window.setTimeout(() => {
        if (!containerRef.current || typeof window.Celestial === 'undefined') return;

        const rect = containerRef.current.getBoundingClientRect();
        if (rect.width > 0 && rect.height > 0) {
          // Aitoff projection has a fixed 2:1 aspect ratio
          const projectionRatio = 2.0;
          const containerRatio = rect.width / rect.height;

          // Calculate optimal width to maximize fill while maintaining aspect ratio
          let targetWidth: number;
          if (containerRatio > projectionRatio) {
            // Container is wider than 2:1 - size to height
            targetWidth = Math.floor(rect.height * projectionRatio);
          } else {
            // Container is taller than 2:1 - size to width
            targetWidth = Math.floor(rect.width);
          }

          window.Celestial.resize({ width: targetWidth });
        }
      }, 250);
    };

    window.addEventListener('resize', handleResize);
    return () => {
      window.removeEventListener('resize', handleResize);
      if (resizeTimeoutRef.current) {
        clearTimeout(resizeTimeoutRef.current);
      }
    };
  }, [mapReady]);

  // Add imaging location markers with FOV visualization
  const addImagingMarkers = useCallback((locs: ImagingLocation[], isZoomedIn: boolean) => {
    if (typeof window.Celestial === 'undefined') return;

    // Convert RA to format expected by d3-celestial
    // Testing: Use RA directly without conversion (Option A from guide)
    const raToGeoJsonLongitude = (ra: number): number => {
      return ra;  // Use RA directly in 0-360° range
    };

    // Custom SVG path for 4-pointed star (✴)
    const fourPointedStar = 'M0,-10 L2,-2 L10,0 L2,2 L0,10 L-2,2 L-10,0 L-2,-2 Z';

    // Custom SVG path for sparkle (✧)
    const sparkle = 'M0,-6 L1,-1 L6,0 L1,1 L0,6 L-1,1 L-6,0 L-1,-1 Z';

    // Helper function to get marker color based on type
    const getMarkerColor = (locationType: string, isCustom: boolean): string => {
      if (locationType === 'cluster') {
        return '#22c55e';  // GREEN for unorganized
      }
      // Frameset
      return isCustom ? '#ef4444' : '#3b82f6';  // RED for custom, BLUE for auto
    };

    // Helper function to get marker stroke color
    const getMarkerStroke = (locationType: string, isCustom: boolean): string => {
      if (locationType === 'cluster') {
        return '#16a34a';  // Dark green for unorganized
      }
      // Frameset
      return isCustom ? '#dc2626' : '#2563eb';  // Dark red for custom, dark blue for auto
    };

    // Helper function to get marker path
    const getMarkerPath = (locationType: string): string => {
      return locationType === 'cluster' ? sparkle : fourPointedStar;
    };

    // Calculate scaling factors to match projection space to display space
    const getCanvasScaling = () => {
      const canvas = document.querySelector('#celestial-map canvas') as HTMLCanvasElement;
      if (!canvas) return { scaleX: 1, scaleY: 1, offsetX: 0, offsetY: 0 };

      const displayRect = canvas.getBoundingClientRect();
      const dpr = window.devicePixelRatio || 1;

      // d3-celestial uses Aitoff projection with 2:1 aspect ratio
      // The projection coordinate system is based on the width we configured
      // Get the Celestial configuration to find the projection width
      const celestialConfig = (window as any).Celestial?.settings;
      const projectionWidth = celestialConfig?.width || canvas.width / dpr;
      const projectionHeight = projectionWidth / 2; // Aitoff is always 2:1

      // The canvas may be stretched to fill the container
      // We need to scale projection coords to match actual display size
      const scaleX = displayRect.width / projectionWidth;
      const scaleY = displayRect.height / projectionHeight;

      return { scaleX, scaleY, offsetX: displayRect.left, offsetY: displayRect.top };
    };

    // Filter out locations with invalid coordinates
    const validLocs = locs.filter(loc =>
      loc.ra !== null && loc.ra !== undefined &&
      loc.dec !== null && loc.dec !== undefined &&
      !isNaN(loc.ra) && !isNaN(loc.dec) &&
      isFinite(loc.ra) && isFinite(loc.dec)
    );

    if (validLocs.length === 0) {
      console.warn('No valid imaging locations to display');
      return;
    }

    // Convert to GeoJSON features with FOV data
    const features = [
      ...validLocs.map(loc => ({
        type: 'Feature',
        id: loc.id,
        properties: {
          name: loc.objectName || 'Unknown',
          objectName: loc.objectName,
          frameCount: loc.frameCount,
          totalExposure: loc.totalExposure,
          filters: loc.filters.join(', '),
          dateRange: loc.dateRange,
          frameSetId: loc.frameSetId,
          locationType: loc.locationType,
          fovWidth: loc.fovWidth,
          fovHeight: loc.fovHeight,
          originalRa: loc.ra,  // Keep original RA for FOV calculations
          cameras: loc.cameras,
          focalLengths: loc.focalLengths,
          isCustom: loc.isCustom
        },
        geometry: {
          type: 'Point',
          coordinates: [raToGeoJsonLongitude(loc.ra), loc.dec]  // Convert RA to GeoJSON format
        }
      }))
    ];

    const imagingData = {
      type: 'FeatureCollection',
      features: features
    };

    // Transform data using d3-celestial's coordinate system
    const transformedData = window.Celestial.getData(imagingData, window.Celestial.settings().transform);

    // Instead of using Celestial.add() which has unreliable callback execution,
    // we'll render the markers directly
    const renderMarkers = () => {
      const data = transformedData;

        // d3-celestial uses canvas, not SVG. We need to create our own SVG overlay
        const mapDiv = document.getElementById('celestial-map');
        if (!mapDiv) {
          console.error('celestial-map div not found');
          return;
        }

        // Find or create SVG overlay
        let svg = d3.select('#celestial-map').select('svg.imaging-markers-overlay');
        if (svg.empty()) {
          // Create SVG overlay positioned absolutely over the canvas
          const canvas = mapDiv.querySelector('canvas');
          if (!canvas) {
            console.error('Canvas not found');
            return;
          }

          // const rect = canvas.getBoundingClientRect();  // Not currently used
          svg = d3.select('#celestial-map')
            .append('svg')
            .attr('class', 'imaging-markers-overlay')
            .style('position', 'absolute')
            .style('top', '0')
            .style('left', '0')
            .style('width', '100%')
            .style('height', '100%')
            .style('pointer-events', 'none'); // Allow clicks to pass through to canvas
        }

        // Create or get markers group
        let markersGroup = svg.select('g.imaging-markers-layer');
        if (markersGroup.empty()) {
          markersGroup = svg.append('g')
            .attr('class', 'imaging-markers-layer')
            .style('pointer-events', 'auto'); // Re-enable pointer events for markers
        }

        // Clear old markers
        markersGroup.selectAll('.imaging-marker').remove();
        markersGroup.selectAll('.fov-box').remove();

        if (isZoomedIn) {
          // Zoomed in: Draw FOV rectangles (or crosses if no FOV data)
          // Get canvas scaling factors once for all markers
          const scaling = getCanvasScaling();

          markersGroup.selectAll('.fov-box')
            .data(data.features)
            .enter().append('g')
            .attr('class', 'fov-box')
            .style('pointer-events', 'all')  // Enable pointer events for click handling
            .each(function(this: any, d: any) {
              const g = d3.select(this);
              const hasFov = d.properties.fovWidth && d.properties.fovHeight;
              const fovW = d.properties.fovWidth;
              const fovH = d.properties.fovHeight;

              const markerColor = getMarkerColor(d.properties.locationType, d.properties.isCustom);
              const markerStroke = getMarkerStroke(d.properties.locationType, d.properties.isCustom);
              const markerPath = getMarkerPath(d.properties.locationType);

              if (hasFov) {
                // Draw FOV rectangle
                const originalRa = d.properties.originalRa;  // Use original RA (0-360°) for calculations
                const dec = d.geometry.coordinates[1];

                // Rectangle corners in RA/Dec, converted to GeoJSON format
                const corners = [
                  [raToGeoJsonLongitude(originalRa - fovW/2), dec - fovH/2],
                  [raToGeoJsonLongitude(originalRa + fovW/2), dec - fovH/2],
                  [raToGeoJsonLongitude(originalRa + fovW/2), dec + fovH/2],
                  [raToGeoJsonLongitude(originalRa - fovW/2), dec + fovH/2],
                ];

                // Project corners and apply scaling
                const projectedCorners = corners.map((c: any) => {
                  const pt = window.Celestial.map.projection()(c);
                  if (!pt) return [0, 0];
                  // Apply canvas scaling
                  return [pt[0] * scaling.scaleX, pt[1] * scaling.scaleY];
                });
                const pathData = `M${projectedCorners[0][0]},${projectedCorners[0][1]} L${projectedCorners[1][0]},${projectedCorners[1][1]} L${projectedCorners[2][0]},${projectedCorners[2][1]} L${projectedCorners[3][0]},${projectedCorners[3][1]} Z`;

                // Create fill color with opacity
                const fillColor = markerColor.replace('#', '');
                const r = parseInt(fillColor.substring(0, 2), 16);
                const g_rgb = parseInt(fillColor.substring(2, 4), 16);
                const b = parseInt(fillColor.substring(4, 6), 16);
                const fillStyle = `rgba(${r}, ${g_rgb}, ${b}, 0.15)`;

                g.append('path')
                  .attr('class', 'fov-rect')
                  .attr('d', pathData)
                  .style('fill', fillStyle)
                  .style('stroke', markerColor)
                  .style('stroke-width', '2px')
                  .style('cursor', d.properties.frameSetId ? 'pointer' : 'default');

                // Store corners for redraw
                (this as any).__fovCorners = corners;
              } else {
                // No FOV data: draw star/sparkle marker
                const pt = window.Celestial.map.projection()(d.geometry.coordinates);
                if (pt) {
                  // Apply scaling to marker position
                  const scaledX = pt[0] * scaling.scaleX;
                  const scaledY = pt[1] * scaling.scaleY;
                  g.append('path')
                    .attr('d', markerPath)
                    .attr('transform', `translate(${scaledX},${scaledY})`)
                    .style('fill', markerColor)
                    .style('stroke', markerStroke)
                    .style('stroke-width', '2px')
                    .style('cursor', d.properties.frameSetId ? 'pointer' : 'default');
                }
              }

              // Visibility check
              const isVisible = window.Celestial.clip(d.geometry.coordinates);
              g.style('display', isVisible ? null : 'none');

              // Click handler
              g.on('click', function(event: any) {
                if (event && event.stopPropagation) {
                  event.stopPropagation();  // Prevent event bubbling if available
                }
                const frameSetId = d.properties.frameSetId;
                console.log('🖱️ Marker clicked:', d.properties.name, 'frameSetId:', frameSetId, 'locationType:', d.properties.locationType);
                if (frameSetId) {
                  // Capture current view state before navigating
                  const projection = window.Celestial.mapProjection;
                  const currentZoom = projection && projection.scale ? projection.scale() : null;
                  const currentCenter = window.Celestial.rotate ? window.Celestial.rotate() : null;

                  console.log('📍 Saving view state:', { zoom: currentZoom, center: currentCenter });

                  if (currentZoom !== null && currentCenter !== null) {
                    saveViewState({
                      zoom: currentZoom,
                      ra: currentCenter[0],
                      dec: currentCenter[1]
                    });
                  }

                  console.log('🚀 Navigating to /objects/' + frameSetId);
                  navigate(`/objects/${frameSetId}`);
                } else {
                  console.log('⚠️ No frameSetId for this marker (unorganized cluster)');
                }
              });

              // Tooltip
              g.append('title')
                .text(function() {
                  const totalHours = (d.properties.totalExposure / 3600).toFixed(2);
                  let typeLabel = '[Unorganized]';
                  if (d.properties.locationType === 'frameset') {
                    typeLabel = d.properties.isCustom ? '[Custom Frame Set]' : '[Auto Frame Set]';
                  }
                  const cameras = d.properties.cameras ? `\nCameras: ${d.properties.cameras}` : '';
                  const focalLengths = d.properties.focalLengths ? `\nFocal Lengths: ${d.properties.focalLengths}mm` : '';
                  const dates = d.properties.dateRange && d.properties.dateRange[0] && d.properties.dateRange[1]
                    ? `\nDates: ${d.properties.dateRange[0].split('T')[0]} to ${d.properties.dateRange[1].split('T')[0]}`
                    : '';
                  const fovInfo = hasFov ? `\nFOV: ${fovW.toFixed(2)}° × ${fovH.toFixed(2)}°` : '';
                  return `${typeLabel} ${d.properties.name}\nFrames: ${d.properties.frameCount}\nExposure: ${totalHours}h\nFilters: ${d.properties.filters}${cameras}${focalLengths}${dates}${fovInfo}`;
                });
            });
        } else {
          // Zoomed out: Draw star/sparkle markers with color differentiation
          // Get canvas scaling factors once for all markers
          const scaling = getCanvasScaling();

          const markers = markersGroup.selectAll('.imaging-marker')
            .data(data.features)
            .enter().append('path')
            .attr('class', 'imaging-marker')
            .style('pointer-events', 'all')  // Enable pointer events for click handling
            .attr('d', function(d: any) {
              return getMarkerPath(d.properties.locationType);
            })
            .attr('transform', function(d: any) {
              const coords = d.geometry.coordinates;
              const pt = window.Celestial.map.projection()(coords);

              if (!pt) return 'translate(0,0)';

              // Apply scaling to account for canvas stretching
              const scaledX = pt[0] * scaling.scaleX;
              const scaledY = pt[1] * scaling.scaleY;

              return `translate(${scaledX},${scaledY})`;
            })
            .style('fill', function(d: any, i: number) {
              const color = getMarkerColor(d.properties.locationType, d.properties.isCustom);
              if (i === 0) {
                console.log('🎨 Marker fill for', d.properties.name, ':',
                  'type:', d.properties.locationType,
                  'isCustom:', d.properties.isCustom,
                  '→ color:', color);
              }
              return color;
            })
            .style('stroke', function(d: any) {
              return getMarkerStroke(d.properties.locationType, d.properties.isCustom);
            })
            .style('stroke-width', '2px')
            .style('cursor', function(d: any) {
              return d.properties.frameSetId ? 'pointer' : 'default';
            })
            .style('display', function(d: any) {
              const pt = window.Celestial.map.projection()(d.geometry.coordinates);
              const isVisible = pt && window.Celestial.clip(d.geometry.coordinates);
              return isVisible ? null : 'none';
            })
            .each(function(this: any, d: any) {
              // Use .each() to create closure for D3.js v7 compatibility
              // Event handlers in D3.js v7 don't receive 'd' as a parameter
              d3.select(this).on('click', function(event: any) {
                if (event && event.stopPropagation) {
                  event.stopPropagation();  // Prevent event bubbling if available
                }
                const frameSetId = d.properties.frameSetId;
                console.log('🖱️ Marker clicked:', d.properties.name, 'frameSetId:', frameSetId, 'locationType:', d.properties.locationType);
                if (frameSetId) {
                  // Capture current view state before navigating
                  const projection = window.Celestial.mapProjection;
                  const currentZoom = projection && projection.scale ? projection.scale() : null;
                  const currentCenter = window.Celestial.rotate ? window.Celestial.rotate() : null;

                  console.log('📍 Saving view state:', { zoom: currentZoom, center: currentCenter });

                  if (currentZoom !== null && currentCenter !== null) {
                    saveViewState({
                      zoom: currentZoom,
                      ra: currentCenter[0],
                      dec: currentCenter[1]
                    });
                  }

                  console.log('🚀 Navigating to /objects/' + frameSetId);
                  navigate(`/objects/${frameSetId}`);
                } else {
                  console.log('⚠️ No frameSetId for this marker (unorganized cluster)');
                }
              });
            });

          // Add tooltips after setting up click handlers
          markers.append('title')
            .text(function(d: any) {
              const totalHours = (d.properties.totalExposure / 3600).toFixed(2);
              let typeLabel = '[Unorganized]';
              if (d.properties.locationType === 'frameset') {
                typeLabel = d.properties.isCustom ? '[Custom Frame Set]' : '[Auto Frame Set]';
              }
              const cameras = d.properties.cameras ? `\nCameras: ${d.properties.cameras}` : '';
              const focalLengths = d.properties.focalLengths ? `\nFocal Lengths: ${d.properties.focalLengths}mm` : '';
              const dates = d.properties.dateRange && d.properties.dateRange[0] && d.properties.dateRange[1]
                ? `\nDates: ${d.properties.dateRange[0].split('T')[0]} to ${d.properties.dateRange[1].split('T')[0]}`
                : '';
              return `${typeLabel} ${d.properties.name}\nFrames: ${d.properties.frameCount}\nExposure: ${totalHours}h\nFilters: ${d.properties.filters}${cameras}${focalLengths}${dates}`;
            });

          console.log('✅ Created', markers.size(), 'marker elements');
          console.log('✅ SVG overlay in DOM:', document.querySelector('#celestial-map svg.imaging-markers-overlay'));
          console.log('✅ Markers in SVG:', markersGroup.selectAll('.imaging-marker').size());
        }
      };

    // Call the render function immediately
    renderMarkers();

    // Register with Celestial for pan/zoom updates
    window.Celestial.add({
      type: 'raw',
      callback: renderMarkers,
      redraw: function() {
        const map = window.Celestial.map;

        // Get our SVG overlay
        const svg = d3.select('#celestial-map').select('svg.imaging-markers-overlay');
        if (svg.empty()) return;

        const markersGroup = svg.select('g.imaging-markers-layer');
        if (markersGroup.empty()) return;

        // Get canvas scaling factors for redraw
        const scaling = getCanvasScaling();

        if (isZoomedIn) {
          // Redraw FOV boxes
          markersGroup.selectAll('.fov-box').each(function(this: any, d: any) {
            const pt = map.projection()(d.geometry.coordinates);
            const corners = (this as any).__fovCorners;

            if (corners) {
              // Project corners and apply scaling
              const projectedCorners = corners.map((c: any) => {
                const pt = map.projection()(c);
                if (!pt) return [0, 0];
                return [pt[0] * scaling.scaleX, pt[1] * scaling.scaleY];
              });
              const pathData = `M${projectedCorners[0][0]},${projectedCorners[0][1]} L${projectedCorners[1][0]},${projectedCorners[1][1]} L${projectedCorners[2][0]},${projectedCorners[2][1]} L${projectedCorners[3][0]},${projectedCorners[3][1]} Z`;

              d3.select(this).select('.fov-rect')
                .attr('d', pathData);
            } else if (pt) {
              // Update cross position with scaling
              const scaledX = pt[0] * scaling.scaleX;
              const scaledY = pt[1] * scaling.scaleY;
              d3.select(this).select('path')
                .attr('transform', `translate(${scaledX},${scaledY})`);
            }

            // Visibility check
            const isVisible = pt && window.Celestial.clip(d.geometry.coordinates);
            d3.select(this)
              .style('display', isVisible ? null : 'none');
          });
        } else {
          // Redraw simple markers with scaling
          markersGroup.selectAll('.imaging-marker').each(function(this: any, d: any) {
            const pt = map.projection()(d.geometry.coordinates);

            if (pt) {
              const scaledX = pt[0] * scaling.scaleX;
              const scaledY = pt[1] * scaling.scaleY;
              d3.select(this)
                .attr('transform', `translate(${scaledX},${scaledY})`);
            }

            const isVisible = pt && window.Celestial.clip(d.geometry.coordinates);
            d3.select(this)
              .style('display', isVisible ? null : 'none');
          });
        }
      }
    });
  }, [navigate]);

  // Add markers when locations are loaded or zoom level changes
  useEffect(() => {
    if (mapReady && locations.length > 0 && !loading) {
      console.log('🎯 Marker useEffect triggered, calling addImagingMarkers');
      addImagingMarkers(locations, zoomLevel.isZoomedIn);
    }
  }, [locations, mapReady, loading, zoomLevel.isZoomedIn, addImagingMarkers]);

  // Manage SVG overlay visibility based on drawing mode
  useEffect(() => {
    if (!mapReady) return;

    const overlay = svgOverlay.getSvg();
    if (!overlay) return;

    if (drawingMode !== 'none') {
      svgOverlay.enable();
    } else {
      svgOverlay.disable();
    }
  }, [drawingMode, mapReady, svgOverlay]);

  // Handle rectangle selection
  useEffect(() => {
    if (!mapReady || drawingMode !== 'rectangle') return;

    console.log('Rectangle selection effect triggered');

    rectangleSelection.startSelection((result) => {
      console.log('Rectangle selection completed with result:', result);
      setSelectionResult(result);
      setShowDialog(true);
      setDrawingMode('none');
    });

    return () => {
      console.log('Rectangle selection effect cleanup');
      rectangleSelection.cancelSelection();
    };
  }, [drawingMode, mapReady, rectangleSelection]);

  // Keyboard shortcuts
  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      // Ignore if user is typing in an input field
      if (event.target instanceof HTMLInputElement || event.target instanceof HTMLTextAreaElement) {
        return;
      }

      // 'S' key - toggle rectangle selection
      if (event.key === 's' || event.key === 'S') {
        event.preventDefault();
        setDrawingMode(prev => prev === 'rectangle' ? 'none' : 'rectangle');
      }

      // 'Escape' key - cancel selection
      if (event.key === 'Escape' && drawingMode !== 'none') {
        event.preventDefault();
        setDrawingMode('none');
      }
    };

    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [drawingMode]);

  if (loading) {
    return (
      <div className="h-screen flex items-center justify-center bg-gray-900">
        <div className="text-center">
          <div className="animate-spin rounded-full h-12 w-12 border-b-2 border-blue-500 mx-auto mb-4"></div>
          <p className="text-gray-400">Loading sky atlas...</p>
        </div>
      </div>
    );
  }

  if (error) {
    return (
      <div className="h-screen flex items-center justify-center bg-gray-900">
        <div className="text-center max-w-md p-6">
          <p className="text-red-400 mb-2 font-semibold">Error loading sky atlas</p>
          <p className="text-gray-400 text-sm mb-4">{error}</p>
          <button
            onClick={() => window.location.reload()}
            className="px-4 py-2 bg-blue-600 hover:bg-blue-700 text-white rounded-lg transition"
          >
            Retry
          </button>
        </div>
      </div>
    );
  }

  if (locations.length === 0) {
    return (
      <div className="h-screen flex items-center justify-center bg-gray-900">
        <div className="text-center max-w-md p-6">
          <h3 className="text-xl font-bold text-gray-100 mb-2">No Imaging Locations Found</h3>
          <p className="text-gray-400 text-sm mb-4">
            You don't have any LIGHT frames with RA/Dec coordinates yet.
          </p>
          <p className="text-gray-500 text-sm">
            Once you import FITS/XISF files with coordinate data, they will appear here.
            You can then use the rectangle selection tool (press S) to organize them into frame sets, or go to the Objects page to use "Auto-Generate Frame Sets".
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="h-screen w-full flex flex-col bg-gray-900">
      {/* Header */}
      <div className="flex-shrink-0 p-4 border-b border-gray-700 bg-gray-800">
        <h2 className="text-2xl font-bold text-gray-100">Sky Atlas</h2>
        <p className="text-sm text-gray-400 mt-1">
          Offline interactive sky map • {locations.length} imaging locations
        </p>
      </div>

      {/* Sky Map - direct flex child, no wrapper */}
      <div
        id="celestial-map"
        ref={containerRef}
        className="flex-1 w-full overflow-hidden relative"
        style={{ minHeight: 0 }}
      >
        {/* Selection Toolbar - floating overlay */}
        <SelectionToolbar
          activeMode={drawingMode}
          onModeChange={setDrawingMode}
          isDisabled={!mapReady}
        />
      </div>

      {/* Selection Results Dialog */}
      <SelectionDialog
        isOpen={showDialog}
        result={selectionResult}
        selectionType={drawingMode === 'rectangle' ? 'rectangle' : null}
        onClose={() => {
          setShowDialog(false);
          setSelectionResult(null);
        }}
      />
    </div>
  );
}
