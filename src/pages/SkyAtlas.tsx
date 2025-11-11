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

      try {
        const config = {
          container: 'celestial-map',
          width: Math.floor(rect.width),
          projection: 'aitoff',
          transform: 'equatorial',
          center: null,
          orientationfixed: false,
          zoomlevel: null,
          zoomextend: 10,
          interactive: true,
          form: false,
          location: false,
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

      console.log(`🔍 Canvas internal: ${canvas.width}x${canvas.height}`);
      console.log(`🔍 Canvas display: ${displayRect.width.toFixed(1)}x${displayRect.height.toFixed(1)}`);
      console.log(`🔍 Projection space: ${projectionWidth}x${projectionHeight}`);
      console.log(`🔍 Device pixel ratio: ${dpr}`);
      console.log(`🔍 Scale factors: ${scaleX.toFixed(3)}x${scaleY.toFixed(3)}`);

      return { scaleX, scaleY, offsetX: displayRect.left, offsetY: displayRect.top };
    };

    // Filter out locations with invalid coordinates
    const validLocs = locs.filter(loc =>
      loc.ra !== null && loc.ra !== undefined &&
      loc.dec !== null && loc.dec !== undefined &&
      !isNaN(loc.ra) && !isNaN(loc.dec) &&
      isFinite(loc.ra) && isFinite(loc.dec)
    );

    console.log(`Adding ${validLocs.length} imaging location markers (zoomed: ${isZoomedIn}) - filtered from ${locs.length}`);

    if (validLocs.length === 0) {
      console.warn('No valid imaging locations to display');
      return;
    }

    // Add test markers for known celestial objects to verify positioning
    const testMarkers = [
      {
        type: 'Feature',
        id: 'test-m42',
        properties: {
          name: 'TEST: M42 Orion Nebula',
          object_name: 'M42',
          frame_count: 0,
          total_exposure: 0,
          filters: 'TEST',
          date_range: '',
          frame_set_id: null,
          location_type: 'test',
          fov_width: null,
          fov_height: null,
          original_ra: 83.82  // M42 RA: 5h 35m 17s
        },
        geometry: {
          type: 'Point',
          coordinates: [raToGeoJsonLongitude(83.82), -5.39]  // Dec: -5° 23' 28"
        }
      },
      {
        type: 'Feature',
        id: 'test-m31',
        properties: {
          name: 'TEST: M31 Andromeda',
          object_name: 'M31',
          frame_count: 0,
          total_exposure: 0,
          filters: 'TEST',
          date_range: '',
          frame_set_id: null,
          location_type: 'test',
          fov_width: null,
          fov_height: null,
          original_ra: 10.68  // M31 RA: 0h 42m 44s
        },
        geometry: {
          type: 'Point',
          coordinates: [raToGeoJsonLongitude(10.68), 41.27]  // Dec: +41° 16' 9"
        }
      },
      {
        type: 'Feature',
        id: 'test-m45',
        properties: {
          name: 'TEST: M45 Pleiades',
          object_name: 'M45',
          frame_count: 0,
          total_exposure: 0,
          filters: 'TEST',
          date_range: '',
          frame_set_id: null,
          location_type: 'test',
          fov_width: null,
          fov_height: null,
          original_ra: 56.75  // M45 RA: 3h 47m 0s
        },
        geometry: {
          type: 'Point',
          coordinates: [raToGeoJsonLongitude(56.75), 24.12]  // Dec: +24° 7' 0"
        }
      }
    ];

    // Convert to GeoJSON features with FOV data
    const features = [
      ...validLocs.map(loc => ({
        type: 'Feature',
        id: loc.id,
        properties: {
          name: loc.object_name || 'Unknown',
          object_name: loc.object_name,
          frame_count: loc.frame_count,
          total_exposure: loc.total_exposure,
          filters: loc.filters.join(', '),
          date_range: loc.date_range,
          frame_set_id: loc.frame_set_id,
          location_type: loc.location_type,
          fov_width: loc.fov_width,
          fov_height: loc.fov_height,
          original_ra: loc.ra  // Keep original RA for FOV calculations
        },
        geometry: {
          type: 'Point',
          coordinates: [raToGeoJsonLongitude(loc.ra), loc.dec]  // Convert RA to GeoJSON format
        }
      })),
      ...testMarkers  // Add test markers for verification
    ];

    const imagingData = {
      type: 'FeatureCollection',
      features: features
    };

    // Transform data using d3-celestial's coordinate system
    const transformedData = window.Celestial.getData(imagingData, window.Celestial.settings().transform);
    console.log('✨ Pre-transformed data features:', transformedData?.features?.length);

    // Instead of using Celestial.add() which has unreliable callback execution,
    // we'll render the markers directly
    const renderMarkers = () => {
      console.log('🎨 Rendering markers directly (bypassing Celestial.add)');
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

          console.log('✅ Created SVG overlay:', svg.node());
        } else {
          console.log('✅ Using existing SVG overlay');
        }

        // Create or get markers group
        let markersGroup = svg.select('g.imaging-markers-layer');
        if (markersGroup.empty()) {
          markersGroup = svg.append('g')
            .attr('class', 'imaging-markers-layer')
            .style('pointer-events', 'auto'); // Re-enable pointer events for markers
          console.log('✅ Created markers group');
        } else {
          console.log('✅ Using existing markers group');
        }

        // Clear old markers
        const removedMarkers = markersGroup.selectAll('.imaging-marker').size();
        const removedBoxes = markersGroup.selectAll('.fov-box').size();
        markersGroup.selectAll('.imaging-marker').remove();
        markersGroup.selectAll('.fov-box').remove();
        console.log(`Removed ${removedMarkers} markers and ${removedBoxes} boxes`);

        if (isZoomedIn) {
          // Zoomed in: Draw FOV rectangles (or crosses if no FOV data)
          // Get canvas scaling factors once for all markers
          const scaling = getCanvasScaling();

          markersGroup.selectAll('.fov-box')
            .data(data.features)
            .enter().append('g')
            .attr('class', 'fov-box')
            .each(function(this: any, d: any) {
              const g = d3.select(this);
              const hasFov = d.properties.fov_width && d.properties.fov_height;
              const fovW = d.properties.fov_width;
              const fovH = d.properties.fov_height;

              if (hasFov) {
                // Draw FOV rectangle
                const originalRa = d.properties.original_ra;  // Use original RA (0-360°) for calculations
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

                g.append('path')
                  .attr('class', 'fov-rect')
                  .attr('d', pathData)
                  .style('fill', 'rgba(34, 197, 94, 0.15)')
                  .style('stroke', '#22c55e')
                  .style('stroke-width', '2px')
                  .style('cursor', 'pointer');

                // Store corners for redraw
                (this as any).__fovCorners = corners;
              } else {
                // No FOV data: draw green cross
                const pt = window.Celestial.map.projection()(d.geometry.coordinates);
                if (pt) {
                  // Apply scaling to cross position
                  const scaledX = pt[0] * scaling.scaleX;
                  const scaledY = pt[1] * scaling.scaleY;
                  g.append('path')
                    .attr('d', 'M-10,0 L10,0 M0,-10 L0,10')
                    .attr('transform', `translate(${scaledX},${scaledY})`)
                    .style('stroke', '#22c55e')
                    .style('stroke-width', '2px')
                    .style('cursor', 'pointer');
                }
              }

              // Visibility check
              const isVisible = window.Celestial.clip(d.geometry.coordinates);
              g.style('display', isVisible ? null : 'none');

              // Click handler
              g.on('click', function() {
                const frameSetId = d.properties.frame_set_id;
                if (frameSetId) {
                  navigate(`/objects/${frameSetId}`);
                }
              });

              // Tooltip
              g.append('title')
                .text(function() {
                  const totalHours = (d.properties.total_exposure / 3600).toFixed(2);
                  const typeLabel = d.properties.location_type === 'frameset' ? '[Frame Set]' : '[Unorganized]';
                  const fovInfo = hasFov ? `\nFOV: ${fovW.toFixed(2)}° × ${fovH.toFixed(2)}°` : '';
                  return `${typeLabel} ${d.properties.name}\nFrames: ${d.properties.frame_count}\nExposure: ${totalHours}h\nFilters: ${d.properties.filters}${fovInfo}`;
                });
            });
        } else {
          // Zoomed out: Draw simple green crosses
          console.log('===== ZOOMED OUT: Creating simple markers =====');
          console.log('Appending to markersGroup:', markersGroup.node());

          // Get canvas scaling factors once for all markers
          const scaling = getCanvasScaling();

          const markers = markersGroup.selectAll('.imaging-marker')
            .data(data.features)
            .enter().append('path')
            .attr('class', 'imaging-marker')
            .attr('d', 'M-8,0 L8,0 M0,-8 L0,8')
            .attr('transform', function(d: any, i: number) {
              const coords = d.geometry.coordinates;
              const pt = window.Celestial.map.projection()(coords);

              if (!pt) return 'translate(0,0)';

              // Apply scaling to account for canvas stretching
              const scaledX = pt[0] * scaling.scaleX;
              const scaledY = pt[1] * scaling.scaleY;

              // Enhanced debug logging for coordinate verification
              if (i === 0 || d.properties.location_type === 'test') {
                console.log(`=== COORDINATE DEBUG (${d.properties.name}) ===`);
                console.log('Original RA (0-360°):', d.properties.original_ra);
                console.log('Original Dec:', coords[1]);
                console.log('Converted coords [lon, lat]:', coords);
                console.log('Projected pixel position [x, y]:', pt);
                console.log('Scaled position [x, y]:', [scaledX, scaledY]);
                console.log('Scaling factors:', scaling);
              }

              return `translate(${scaledX},${scaledY})`;
            })
            .style('stroke', function(d: any) {
              // Make test markers red for easy identification
              return d.properties.location_type === 'test' ? '#ef4444' : '#22c55e';
            })
            .style('stroke-width', function(d: any) {
              // Make test markers thicker
              return d.properties.location_type === 'test' ? '3px' : '2px';
            })
            .style('cursor', 'pointer')
            .style('display', function(d: any) {
              const pt = window.Celestial.map.projection()(d.geometry.coordinates);
              const isVisible = pt && window.Celestial.clip(d.geometry.coordinates);
              return isVisible ? null : 'none';
            })
            .on('click', function(d: any) {
              const frameSetId = d.properties.frame_set_id;
              if (frameSetId) {
                navigate(`/objects/${frameSetId}`);
              }
            })
            .append('title')
            .text(function(d: any) {
              const totalHours = (d.properties.total_exposure / 3600).toFixed(2);
              const typeLabel = d.properties.location_type === 'frameset' ? '[Frame Set]' : '[Unorganized]';
              return `${typeLabel} ${d.properties.name}\nFrames: ${d.properties.frame_count}\nExposure: ${totalHours}h`;
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
