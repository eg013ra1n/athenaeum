import { useState, useEffect, useLayoutEffect, useRef, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useNavigate } from 'react-router-dom';
import { ImagingLocation } from '../types/models';
import { DrawingMode } from '../types/selection';
import { useSvgOverlay } from '../hooks/useSvgOverlay';
import { useCoordinateTransform } from '../hooks/useCoordinateTransform';
import { useD3MouseEvents } from '../hooks/useD3MouseEvents';

// Declare global Celestial from d3-celestial
declare global {
  interface Window {
    Celestial: any;
  }
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
  const [drawingMode] = useState<DrawingMode>('none');

  // Custom hooks
  const svgOverlay = useSvgOverlay({ containerId: 'celestial-map' });
  useCoordinateTransform();
  useD3MouseEvents();

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
          window.Celestial.resize({ width: Math.floor(rect.width) });
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

  // Add imaging location markers
  const addImagingMarkers = useCallback((locs: ImagingLocation[]) => {
    if (typeof window.Celestial === 'undefined') return;

    console.log(`Adding ${locs.length} imaging location markers`);

    // Convert to GeoJSON features
    const features = locs.map(loc => ({
      type: 'Feature',
      id: loc.id,
      properties: {
        name: loc.object_name || 'Unknown',
        object_name: loc.object_name,
        frame_count: loc.frame_count,
        total_exposure: loc.total_exposure,
        filters: loc.filters.join(', '),
        date_range: loc.date_range,
        frame_set_id: loc.frame_set_id
      },
      geometry: {
        type: 'Point',
        coordinates: [loc.ra, loc.dec]
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
        const container = window.Celestial.container;
        const data = window.Celestial.getData(imagingData, window.Celestial.settings().transform);

        // Draw markers
        container.selectAll('.imaging-marker').remove();

        container.selectAll('.imaging-marker')
          .data(data.features)
          .enter().append('path')
          .attr('class', 'imaging-marker')
          .attr('d', window.Celestial.symbol().type(window.Celestial.symbolType('square')).size(100))
          .style('fill', '#3b82f6')
          .style('stroke', '#60a5fa')
          .style('stroke-width', '1.5px')
          .style('cursor', 'pointer')
          .on('click', function(d: any) {
            const frameSetId = d.properties.frame_set_id;
            if (frameSetId) {
              navigate(`/objects/${frameSetId}`);
            }
          })
          .append('title')
          .text(function(d: any) {
            const totalHours = (d.properties.total_exposure / 3600).toFixed(2);
            return `${d.properties.name}\nFrames: ${d.properties.frame_count}\nExposure: ${totalHours}h\nFilters: ${d.properties.filters}`;
          });
      },
      redraw: function() {
        // Redraw markers on zoom/pan
        const container = window.Celestial.container;
        const map = window.Celestial.map;

        container.selectAll('.imaging-marker').each(function(this: any, d: any) {
          // Get projected coordinates
          const pt = map.projection()(d.geometry.coordinates);

          // Update position
          window.Celestial.select(this)
            .attr('transform', `translate(${pt[0]},${pt[1]})`);

          // Check if point is visible
          const isVisible = window.Celestial.clip(d.geometry.coordinates);
          window.Celestial.select(this)
            .style('display', isVisible ? null : 'none');
        });
      }
    });
  }, [navigate]);

  // Add markers when locations are loaded
  useEffect(() => {
    if (mapReady && locations.length > 0 && !loading) {
      setTimeout(() => addImagingMarkers(locations), 100);
    }
  }, [locations, mapReady, loading, addImagingMarkers]);

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
            You don't have any frame sets with coordinate data yet.
          </p>
          <p className="text-gray-500 text-sm">
            Go to the Objects page and use "Auto-Generate Frame Sets" to create frame sets from your LIGHT frames with RA/Dec coordinates.
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
    </div>
  );
}
