import { useState, useEffect, useLayoutEffect, useRef, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useNavigate } from 'react-router-dom';
import { ImagingLocation } from '../types/models';
import { DrawingMode, SelectionResult } from '../types/selection';
import { useSvgOverlay } from '../hooks/useSvgOverlay';
import { useCoordinateTransform } from '../hooks/useCoordinateTransform';
import { useD3MouseEvents } from '../hooks/useD3MouseEvents';
import { useCircleSelection } from '../hooks/useCircleSelection';
import { useRectangleSelection } from '../hooks/useRectangleSelection';
import { useZoomLevel } from '../hooks/useZoomLevel';
import { useViewportBounds, isPointInBounds } from '../hooks/useViewportBounds';
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
  const circleSelection = useCircleSelection(svgOverlay, coordinateTransform, mouseEvents);
  const rectangleSelection = useRectangleSelection(svgOverlay, coordinateTransform, mouseEvents);
  const zoomLevel = useZoomLevel(2.0); // Threshold: show FOV boxes when scale > 2.0
  const viewportBounds = useViewportBounds();

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

    // Convert RA from astronomical format (0-360°) to GeoJSON format (-180 to +180°)
    const raToGeoJsonLongitude = (ra: number): number => {
      // RA > 180° becomes negative longitude
      return ra > 180 ? ra - 360 : ra;
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

    // Convert to GeoJSON features with FOV data
    const features = validLocs.map(loc => ({
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
    }));

    const imagingData = {
      type: 'FeatureCollection',
      features: features
    };

    // Add custom layer for imaging locations
    window.Celestial.add({
      type: 'raw',
      callback: function(error: any) {
        if (error) {
          console.error('Error loading imaging data:', error);
          return;
        }

        // Transform data to celestial coordinates
        const data = window.Celestial.getData(imagingData, window.Celestial.settings().transform);

        console.log('Transformed data features:', data?.features?.length);

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

          const rect = canvas.getBoundingClientRect();
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
          markersGroup.selectAll('.fov-box')
            .data(data.features)
            .enter().append('g')
            .attr('class', 'fov-box')
            .each(function(this: any, d: any) {
              const g = d3.select(this);
              const hasFov = d.properties.fov_width && d.properties.fov_height;

              if (hasFov) {
                // Draw FOV rectangle
                const fovW = d.properties.fov_width;
                const fovH = d.properties.fov_height;
                const originalRa = d.properties.original_ra;  // Use original RA (0-360°) for calculations
                const dec = d.geometry.coordinates[1];

                // Rectangle corners in RA/Dec, converted to GeoJSON format
                const corners = [
                  [raToGeoJsonLongitude(originalRa - fovW/2), dec - fovH/2],
                  [raToGeoJsonLongitude(originalRa + fovW/2), dec - fovH/2],
                  [raToGeoJsonLongitude(originalRa + fovW/2), dec + fovH/2],
                  [raToGeoJsonLongitude(originalRa - fovW/2), dec + fovH/2],
                ];

                // Project corners and create initial path
                const projectedCorners = corners.map((c: any) => {
                  const pt = window.Celestial.map.projection()(c);
                  return pt || [0, 0]; // Fallback if projection fails
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
                  g.append('path')
                    .attr('d', 'M-10,0 L10,0 M0,-10 L0,10')
                    .attr('transform', `translate(${pt[0]},${pt[1]})`)
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

          const markers = markersGroup.selectAll('.imaging-marker')
            .data(data.features)
            .enter().append('path')
            .attr('class', 'imaging-marker')
            .attr('d', 'M-8,0 L8,0 M0,-8 L0,8')
            .attr('transform', function(d: any, i: number) {
              const coords = d.geometry.coordinates;
              const pt = window.Celestial.map.projection()(coords);
              if (i === 0) console.log(`Marker ${i}: coords=[${coords[0]}, ${coords[1]}] → projected to [${pt[0]},${pt[1]}]`);
              return pt ? `translate(${pt[0]},${pt[1]})` : 'translate(0,0)';
            })
            .style('stroke', '#22c55e')
            .style('stroke-width', '2px')
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
      },
      redraw: function() {
        const map = window.Celestial.map;

        // Get our SVG overlay
        const svg = d3.select('#celestial-map').select('svg.imaging-markers-overlay');
        if (svg.empty()) return;

        const markersGroup = svg.select('g.imaging-markers-layer');
        if (markersGroup.empty()) return;

        if (isZoomedIn) {
          // Redraw FOV boxes
          markersGroup.selectAll('.fov-box').each(function(this: any, d: any) {
            const pt = map.projection()(d.geometry.coordinates);
            const corners = (this as any).__fovCorners;

            if (corners) {
              // Project corners and draw path
              const projectedCorners = corners.map((c: any) => {
                const pt = map.projection()(c);
                return pt || [0, 0];
              });
              const pathData = `M${projectedCorners[0][0]},${projectedCorners[0][1]} L${projectedCorners[1][0]},${projectedCorners[1][1]} L${projectedCorners[2][0]},${projectedCorners[2][1]} L${projectedCorners[3][0]},${projectedCorners[3][1]} Z`;

              d3.select(this).select('.fov-rect')
                .attr('d', pathData);
            } else if (pt) {
              // Update cross position
              d3.select(this).select('path')
                .attr('transform', `translate(${pt[0]},${pt[1]})`);
            }

            // Visibility check
            const isVisible = pt && window.Celestial.clip(d.geometry.coordinates);
            d3.select(this)
              .style('display', isVisible ? null : 'none');
          });
        } else {
          // Redraw simple markers
          markersGroup.selectAll('.imaging-marker').each(function(this: any, d: any) {
            const pt = map.projection()(d.geometry.coordinates);

            if (pt) {
              d3.select(this)
                .attr('transform', `translate(${pt[0]},${pt[1]})`);
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
      setTimeout(() => addImagingMarkers(locations, zoomLevel.isZoomedIn), 100);
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

  // Handle circle selection
  useEffect(() => {
    if (!mapReady || drawingMode !== 'circle') return;

    console.log('Circle selection effect triggered');

    circleSelection.startSelection((result) => {
      console.log('Circle selection completed with result:', result);
      setSelectionResult(result);
      setShowDialog(true);
      setDrawingMode('none');
    });

    return () => {
      console.log('Circle selection effect cleanup');
      circleSelection.cancelSelection();
    };
  }, [drawingMode, mapReady, circleSelection]);

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
            You can then use the circle selection tool to organize them into frame sets, or go to the Objects page to use "Auto-Generate Frame Sets".
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
        className="flex-1 w-full overflow-hidden"
        style={{ minHeight: 0 }}
      />

      {/* Selection Toolbar */}
      <SelectionToolbar
        activeMode={drawingMode}
        onModeChange={setDrawingMode}
        isDisabled={!mapReady}
      />

      {/* Selection Results Dialog */}
      <SelectionDialog
        isOpen={showDialog}
        result={selectionResult}
        selectionType={drawingMode as 'circle' | 'rectangle' | 'polygon' | null}
        onClose={() => {
          setShowDialog(false);
          setSelectionResult(null);
        }}
      />
    </div>
  );
}
