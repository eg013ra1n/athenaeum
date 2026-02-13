import { useState, useEffect, useLayoutEffect, useRef, useCallback, useMemo } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { useNavigate } from 'react-router-dom';
import { ImagingLocation } from '../types/models';
import { DrawingMode, SelectionResult } from '../types/selection';
import { useSvgOverlay } from '../hooks/useSvgOverlay';
import { useCoordinateTransform } from '../hooks/useCoordinateTransform';
import { useD3MouseEvents } from '../hooks/useD3MouseEvents';
import { useRectangleSelection } from '../hooks/useRectangleSelection';
import { useMapViewState } from '../hooks/useMapViewState';
import { SelectionDialog } from '../components/SelectionDialog';
import { DateRangeFilter, DateParts, toISODate } from '../components/DateRangeFilter';
import '../styles/celestial-overrides.css';

// Declare global Celestial and d3 from d3-celestial
declare global {
  interface Window {
    Celestial: any;
    d3: any;
  }
  const d3: any;
}

/** Inverse gnomonic: tangent-plane (xi, eta) in radians → [RA, Dec] in degrees */
function inverseGnomonic(
  xiRad: number, etaRad: number,
  ra0Deg: number, dec0Deg: number
): [number, number] {
  const ra0Rad = (ra0Deg * Math.PI) / 180;
  const dec0Rad = (dec0Deg * Math.PI) / 180;
  const sinDec0 = Math.sin(dec0Rad);
  const cosDec0 = Math.cos(dec0Rad);
  const rho = Math.sqrt(xiRad * xiRad + etaRad * etaRad);
  if (rho < 1e-10) return [ra0Deg, dec0Deg];
  const c = Math.atan(rho);
  const sinC = Math.sin(c);
  const cosC = Math.cos(c);
  const dec = Math.asin(cosC * sinDec0 + etaRad * sinC * cosDec0 / rho) * (180 / Math.PI);
  const ra = (ra0Rad + Math.atan2(xiRad * sinC, rho * cosDec0 * cosC - etaRad * sinDec0 * sinC)) * (180 / Math.PI);
  return [ra, dec];
}

export default function SkyChart() {
  const containerRef = useRef<HTMLDivElement>(null);
  const mapInitialized = useRef(false);
  const resizeTimeoutRef = useRef<number | undefined>(undefined);
  const autoSaveTimeoutRef = useRef<number | undefined>(undefined);
  const saveViewStateRef = useRef<((state: any, replace?: boolean) => void) | null>(null);
  const autoSaveRegistered = useRef(false);
  const [locations, setLocations] = useState<ImagingLocation[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [mapReady, setMapReady] = useState(false);
  const [selectedTarget, setSelectedTarget] = useState<string>('');

  // Selection state
  const [drawingMode, setDrawingMode] = useState<DrawingMode>('none');
  const [selectionResult, setSelectionResult] = useState<SelectionResult | null>(null);
  const [showDialog, setShowDialog] = useState(false);

  // Date filter state
  const [dateFrom, setDateFrom] = useState<DateParts | null>(null);
  const [dateTo, setDateTo] = useState<DateParts | null>(null);

  // Custom hooks
  const svgOverlay = useSvgOverlay({ containerId: 'celestial-map' });
  const coordinateTransform = useCoordinateTransform();
  const mouseEvents = useD3MouseEvents();
  const rectangleSelection = useRectangleSelection(svgOverlay, coordinateTransform, mouseEvents);
  const { getViewState, saveViewState } = useMapViewState('skychart_view_state');

  const navigate = useNavigate();

  // Filter locations by date range
  const filteredLocations = useMemo(() => {
    const fromISO = toISODate(dateFrom);
    const toISO = toISODate(dateTo);

    return locations.filter(loc => {
      const locStartDate = loc.dateRange[0]?.split('T')[0];
      const locEndDate = loc.dateRange[1]?.split('T')[0];

      if (fromISO && locEndDate && locEndDate < fromISO) return false;
      if (toISO && locStartDate && locStartDate > toISO) return false;

      return true;
    });
  }, [locations, dateFrom, dateTo]);

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

  // Keep saveViewState ref up to date
  useEffect(() => {
    saveViewStateRef.current = saveViewState;
  }, [saveViewState]);

  // Projection scaling helper — reads canvas buffer dimensions directly
  const getCanvasScaling = useCallback(() => {
    const canvas = document.querySelector('#celestial-map canvas') as HTMLCanvasElement;
    if (!canvas) return { scaleX: 1, scaleY: 1, offsetX: 0, offsetY: 0 };

    const displayRect = canvas.getBoundingClientRect();
    const dpr = window.devicePixelRatio || 1;

    const projectionWidth = canvas.width / dpr;
    const projectionHeight = canvas.height / dpr;

    const scaleX = displayRect.width / projectionWidth;
    const scaleY = displayRect.height / projectionHeight;

    return { scaleX, scaleY, offsetX: displayRect.left, offsetY: displayRect.top };
  }, []);

  // Auto-save map state on pan/zoom
  useEffect(() => {
    if (!mapReady || typeof window.Celestial === 'undefined') return;
    if (autoSaveRegistered.current) return;

    window.Celestial.add({
      type: 'raw',
      callback: () => {},
      redraw: function() {
        clearTimeout(autoSaveTimeoutRef.current);
        autoSaveTimeoutRef.current = window.setTimeout(() => {
          const projection = window.Celestial.mapProjection;
          const currentZoom = projection && projection.scale ? projection.scale() : null;
          const currentCenter = window.Celestial.rotate ? window.Celestial.rotate() : null;

          if (currentZoom !== null && currentCenter !== null && saveViewStateRef.current) {
            saveViewStateRef.current({
              zoom: currentZoom,
              ra: currentCenter[0],
              dec: currentCenter[1],
              rotation: currentCenter[2] ?? 0
            });
          }
        }, 300);
      }
    });

    autoSaveRegistered.current = true;
  }, [mapReady]);

  // Initialize d3-celestial with stereographic projection
  useLayoutEffect(() => {
    if (loading || !containerRef.current || mapInitialized.current) return;

    if (typeof window.Celestial === 'undefined') {
      setError('Sky map library not loaded. Please refresh the page.');
      return;
    }

    const container = containerRef.current;

    const rafId = requestAnimationFrame(() => {
      const rect = container.getBoundingClientRect();

      if (rect.width === 0 || rect.height === 0) {
        console.error(`Container has no dimensions: ${rect.width}x${rect.height}`);
        setError('Unable to initialize sky chart: container has no dimensions');
        return;
      }

      const savedState = getViewState();
      const restoringState = savedState.ra !== null || savedState.zoom !== null;

      // Stereographic is 1:1 — fit within container's smaller dimension
      // to minimize CSS stretch distortion (same logic as resize handler)
      const projectionRatio = 1.0;
      const containerRatio = rect.width / rect.height;
      const initialWidth = containerRatio > projectionRatio
        ? Math.floor(rect.height * projectionRatio)  // fit to height
        : Math.floor(rect.width);                     // fit to width

      try {
        const config = {
          container: 'celestial-map',
          width: initialWidth,
          projection: 'stereographic',
          transform: 'equatorial',
          center: (savedState.ra !== null && savedState.dec !== null)
            ? [savedState.ra, savedState.dec, savedState.rotation ?? 0]
            : null,
          // Suppress all d3 transition animations during initialization so
          // any internal rotate()/zoomBy() calls inside display() use instant
          // code paths. Re-enabled in the restoration rAF below.
          disableAnimations: restoringState,
          orientationfixed: false,
          follow: "center",
          zoomlevel: null,
          zoomextend: 20,
          adaptable: true,
          interactive: true,
          form: false,
          controls: true,
          datapath: '/data/',
          stars: {
            show: true,
            limit: 8,
            colors: true,
            style: { fill: '#ffffff', opacity: 0.8 },
            designation: true,
            designationLimit: 2.5,
            propername: true,
            propernameLimit: 1.5,
            size: 7,
            exponent: -0.28,
            data: 'stars.8.json'
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

        // Restore zoom and re-enable animations in a single rAF.
        // disableAnimations was set true in the config above, so zoomBy()
        // takes the instant path that syncs both projection.scale AND the
        // internal d3.geo.zoom behavior state (K.scale).
        if (restoringState) {
          requestAnimationFrame(() => {
            if (savedState.zoom !== null) {
              const projection = window.Celestial.mapProjection;
              if (projection && projection.scale && window.Celestial.zoomBy) {
                const currentZoom = projection.scale();
                const zoomFactor = savedState.zoom / currentZoom;
                if (Math.abs(zoomFactor - 1) > 0.001) {
                  window.Celestial.zoomBy(zoomFactor);
                }
              }
            }
            // Re-enable animations now that restoration is complete
            const settings = window.Celestial.settings();
            settings.disableAnimations = false;
          });
        }

        setMapReady(true);
      } catch (err) {
        console.error('Failed to initialize sky chart:', err);
        setError(`Failed to initialize sky chart: ${err}`);
      }
    });

    return () => {
      cancelAnimationFrame(rafId);
    };
  }, [loading]);

  // Handle window resize
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
          // Stereographic projection has 1:1 aspect ratio
          const projectionRatio = 1.0;
          const containerRatio = rect.width / rect.height;

          let targetWidth: number;
          if (containerRatio > projectionRatio) {
            targetWidth = Math.floor(rect.height * projectionRatio);
          } else {
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

  // Update the globe clip path on an SVG overlay so content outside the
  // projected sphere is hidden. Uses d3.geo.path({type:"Sphere"}) for the
  // exact projection boundary, scaled to display space.
  const updateGlobeClip = useCallback((svg: any, clipId: string, scaling: { scaleX: number; scaleY: number }) => {
    const projection = window.Celestial.map?.projection?.();
    if (!projection) return;

    let defs = svg.select('defs');
    if (defs.empty()) defs = svg.append('defs');

    let clipPath = defs.select(`#${clipId}`);
    if (clipPath.empty()) {
      clipPath = defs.append('clipPath').attr('id', clipId);
      clipPath.append('path');
    }

    const pathGen = d3.geo.path().projection(projection);
    const sphereD = pathGen({ type: 'Sphere' });

    clipPath.select('path')
      .attr('d', sphereD)
      .attr('transform', `scale(${scaling.scaleX},${scaling.scaleY})`);
  }, []);

  // Add imaging location markers with FOV visualization
  const addImagingMarkers = useCallback((locs: ImagingLocation[]) => {
    if (typeof window.Celestial === 'undefined') return;

    const raToGeoJsonLongitude = (ra: number): number => ra;

    const fourPointedStar = 'M0,-10 L2,-2 L10,0 L2,2 L0,10 L-2,2 L-10,0 L-2,-2 Z';
    const sparkle = 'M0,-6 L1,-1 L6,0 L1,1 L0,6 L-1,1 L-6,0 L-1,-1 Z';

    const getMarkerColor = (locationType: string, isCustom: boolean): string => {
      if (locationType === 'cluster') return '#22c55e';
      return isCustom ? '#ef4444' : '#3b82f6';
    };

    const getMarkerStroke = (locationType: string, isCustom: boolean): string => {
      if (locationType === 'cluster') return '#16a34a';
      return isCustom ? '#dc2626' : '#2563eb';
    };

    const getMarkerPath = (locationType: string): string => {
      return locationType === 'cluster' ? sparkle : fourPointedStar;
    };

    const validLocs = locs.filter(loc =>
      loc.ra !== null && loc.ra !== undefined &&
      loc.dec !== null && loc.dec !== undefined &&
      !isNaN(loc.ra) && !isNaN(loc.dec) &&
      isFinite(loc.ra) && isFinite(loc.dec)
    );

    if (validLocs.length === 0) return;

    const features = validLocs.map(loc => ({
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
        originalRa: loc.ra,
        cameras: loc.cameras,
        focalLengths: loc.focalLengths,
        isCustom: loc.isCustom,
        rotation: loc.rotation
      },
      geometry: {
        type: 'Point',
        coordinates: [raToGeoJsonLongitude(loc.ra), loc.dec]
      }
    }));

    const imagingData = { type: 'FeatureCollection', features };
    const transformedData = window.Celestial.getData(imagingData, window.Celestial.settings().transform);

    const renderMarkers = () => {
      const data = transformedData;
      const mapDiv = document.getElementById('celestial-map');
      if (!mapDiv) return;

      let svg = d3.select('#celestial-map').select('svg.imaging-markers-overlay');
      if (svg.empty()) {
        svg = d3.select('#celestial-map')
          .append('svg')
          .attr('class', 'imaging-markers-overlay')
          .style('position', 'absolute')
          .style('top', '0')
          .style('left', '0')
          .style('width', '100%')
          .style('height', '100%')
          .style('pointer-events', 'none');
      }

      let markersGroup = svg.select('g.imaging-markers-layer');
      if (markersGroup.empty()) {
        markersGroup = svg.append('g')
          .attr('class', 'imaging-markers-layer')
          .style('pointer-events', 'auto');
      }

      markersGroup.selectAll('.imaging-marker').remove();
      markersGroup.selectAll('.fov-box').remove();

      const scaling = getCanvasScaling();

      // Clip markers to globe boundary
      updateGlobeClip(svg, 'markers-globe-clip', scaling);
      markersGroup.attr('clip-path', 'url(#markers-globe-clip)');

      markersGroup.selectAll('.fov-box')
        .data(data.features)
        .enter().append('g')
        .attr('class', 'fov-box')
        .style('pointer-events', 'all')
        .each(function(this: any, d: any) {
          const g = d3.select(this);
          const hasFov = d.properties.fovWidth && d.properties.fovHeight;
          const fovW = d.properties.fovWidth;
          const fovH = d.properties.fovHeight;

          const markerColor = getMarkerColor(d.properties.locationType, d.properties.isCustom);
          const markerStroke = getMarkerStroke(d.properties.locationType, d.properties.isCustom);
          const markerPath = getMarkerPath(d.properties.locationType);

          if (hasFov) {
            const originalRa = d.properties.originalRa;
            const dec = d.geometry.coordinates[1];
            const pa = d.properties.rotation ?? 0;
            const paRad = (pa * Math.PI) / 180;

            // Convert half-FOV angle to gnomonic tangent-plane coordinate.
            // tan(angle) is the correct offset on the tangent plane; for small angles tan(x) ≈ x.
            const halfW = Math.tan((fovW / 2) * Math.PI / 180) * (180 / Math.PI);
            const halfH = Math.tan((fovH / 2) * Math.PI / 180) * (180 / Math.PI);
            const offsets: [number, number][] = [
              [-halfW, -halfH],
              [+halfW, -halfH],
              [+halfW, +halfH],
              [-halfW, +halfH],
            ];

            // Inverse gnomonic projection: tangent-plane offsets → sphere
            // Handles poles correctly (flat-sky dRA=xi/cosDec breaks at Dec>89°)
            const corners = offsets.map(([dxi, deta]) => {
              const xi  =  dxi * Math.cos(paRad) + deta * Math.sin(paRad);
              const eta = -dxi * Math.sin(paRad) + deta * Math.cos(paRad);
              const xiRad = (xi * Math.PI) / 180;
              const etaRad = (eta * Math.PI) / 180;
              const [cornerRA, cornerDec] = inverseGnomonic(xiRad, etaRad, originalRa, dec);
              return [raToGeoJsonLongitude(cornerRA), cornerDec] as [number, number];
            });

            const projectedCorners: [number, number][] = [];
            let hasInvalidCorner = false;
            for (const c of corners) {
              const pt = window.Celestial.map.projection()(c);
              if (!pt || !isFinite(pt[0]) || !isFinite(pt[1])) {
                hasInvalidCorner = true;
                break;
              }
              if (!window.Celestial.clip(c)) {
                hasInvalidCorner = true;
                break;
              }
              projectedCorners.push([pt[0] * scaling.scaleX, pt[1] * scaling.scaleY]);
            }

            if (hasInvalidCorner || projectedCorners.length !== 4) {
              g.style('display', 'none');
              return;
            }

            const xCoords = projectedCorners.map(p => p[0]);
            const yCoords = projectedCorners.map(p => p[1]);
            const xSpan = Math.max(...xCoords) - Math.min(...xCoords);
            const ySpan = Math.max(...yCoords) - Math.min(...yCoords);

            const canvas = document.querySelector('#celestial-map canvas') as HTMLCanvasElement;
            const canvasWidth = canvas ? canvas.getBoundingClientRect().width : 1000;
            const canvasHeight = canvas ? canvas.getBoundingClientRect().height : 1000;

            if (xSpan > canvasWidth * 0.5 || ySpan > canvasHeight * 0.5) {
              g.style('display', 'none');
              return;
            }

            const absPARad = Math.abs(paRad);
            const tanW = Math.tan((fovW / 2) * Math.PI / 180) * 2 * (180 / Math.PI);
            const tanH = Math.tan((fovH / 2) * Math.PI / 180) * 2 * (180 / Math.PI);
            const expectedBBWidth = tanW * Math.abs(Math.cos(absPARad)) + tanH * Math.abs(Math.sin(absPARad));
            const expectedBBHeight = tanW * Math.abs(Math.sin(absPARad)) + tanH * Math.abs(Math.cos(absPARad));
            const originalAspectRatio = expectedBBWidth / Math.max(expectedBBHeight, 0.001);
            const projectedAspectRatio = xSpan / Math.max(ySpan, 0.001);
            const distortionRatio = projectedAspectRatio / originalAspectRatio;

            if (distortionRatio > 3 || distortionRatio < 0.33) {
              g.style('display', 'none');
              return;
            }

            const pathData = `M${projectedCorners[0][0]},${projectedCorners[0][1]} L${projectedCorners[1][0]},${projectedCorners[1][1]} L${projectedCorners[2][0]},${projectedCorners[2][1]} L${projectedCorners[3][0]},${projectedCorners[3][1]} Z`;

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

            (this as any).__fovCorners = corners;
            (this as any).__fovWidth = fovW;
            (this as any).__fovHeight = fovH;
            (this as any).__rotation = pa;
          } else {
            const pt = window.Celestial.map.projection()(d.geometry.coordinates);
            if (pt) {
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

          const isVisible = window.Celestial.clip(d.geometry.coordinates);
          g.style('display', isVisible ? null : 'none');

          // Click handler
          g.on('click', function(event: any) {
            if (event && event.stopPropagation) {
              event.stopPropagation();
            }
            const frameSetId = d.properties.frameSetId;
            if (frameSetId) {
              const projection = window.Celestial.mapProjection;
              const currentZoom = projection && projection.scale ? projection.scale() : null;
              const currentCenter = window.Celestial.rotate ? window.Celestial.rotate() : null;

              if (currentZoom !== null && currentCenter !== null) {
                saveViewState({
                  zoom: currentZoom,
                  ra: currentCenter[0],
                  dec: currentCenter[1],
                  rotation: currentCenter[2] ?? 0
                });
              }

              navigate(`/objects/${frameSetId}`);
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
    };

    renderMarkers();

    // Register with Celestial for pan/zoom updates
    window.Celestial.add({
      type: 'raw',
      callback: renderMarkers,
      redraw: function() {
        const map = window.Celestial.map;

        const svg = d3.select('#celestial-map').select('svg.imaging-markers-overlay');
        if (svg.empty()) return;

        const markersGroup = svg.select('g.imaging-markers-layer');
        if (markersGroup.empty()) return;

        const scaling = getCanvasScaling();

        // Update globe clip on every redraw
        updateGlobeClip(svg, 'markers-globe-clip', scaling);

        const canvas = document.querySelector('#celestial-map canvas') as HTMLCanvasElement;
        const canvasWidth = canvas ? canvas.getBoundingClientRect().width : 1000;
        const canvasHeight = canvas ? canvas.getBoundingClientRect().height : 1000;

        markersGroup.selectAll('.fov-box').each(function(this: any, d: any) {
          const pt = map.projection()(d.geometry.coordinates);
          const corners = (this as any).__fovCorners;

          if (corners) {
            const projectedCorners: [number, number][] = [];
            let hasInvalidCorner = false;
            for (const c of corners) {
              const projPt = map.projection()(c);
              if (!projPt || !isFinite(projPt[0]) || !isFinite(projPt[1])) {
                hasInvalidCorner = true;
                break;
              }
              projectedCorners.push([projPt[0] * scaling.scaleX, projPt[1] * scaling.scaleY]);
            }

            if (hasInvalidCorner || projectedCorners.length !== 4) {
              d3.select(this).style('display', 'none');
              return;
            }

            const xCoords = projectedCorners.map(p => p[0]);
            const yCoords = projectedCorners.map(p => p[1]);
            const xSpan = Math.max(...xCoords) - Math.min(...xCoords);
            const ySpan = Math.max(...yCoords) - Math.min(...yCoords);

            if (xSpan > canvasWidth * 0.5 || ySpan > canvasHeight * 0.5) {
              d3.select(this).style('display', 'none');
              return;
            }

            const storedFovW = (this as any).__fovWidth;
            const storedFovH = (this as any).__fovHeight;
            if (storedFovW && storedFovH) {
              const storedRotation = (this as any).__rotation ?? 0;
              const absPARad = Math.abs(storedRotation * Math.PI / 180);
              const tanW = Math.tan((storedFovW / 2) * Math.PI / 180) * 2 * (180 / Math.PI);
              const tanH = Math.tan((storedFovH / 2) * Math.PI / 180) * 2 * (180 / Math.PI);
              const expectedBBWidth = tanW * Math.abs(Math.cos(absPARad)) + tanH * Math.abs(Math.sin(absPARad));
              const expectedBBHeight = tanW * Math.abs(Math.sin(absPARad)) + tanH * Math.abs(Math.cos(absPARad));
              const originalAspectRatio = expectedBBWidth / Math.max(expectedBBHeight, 0.001);
              const projectedAspectRatio = xSpan / Math.max(ySpan, 0.001);
              const distortionRatio = projectedAspectRatio / originalAspectRatio;

              if (distortionRatio > 3 || distortionRatio < 0.33) {
                d3.select(this).style('display', 'none');
                return;
              }
            }

            const pathData = `M${projectedCorners[0][0]},${projectedCorners[0][1]} L${projectedCorners[1][0]},${projectedCorners[1][1]} L${projectedCorners[2][0]},${projectedCorners[2][1]} L${projectedCorners[3][0]},${projectedCorners[3][1]} Z`;

            d3.select(this).select('.fov-rect')
              .attr('d', pathData);

            d3.select(this).style('display', null);
          } else if (pt) {
            const scaledX = pt[0] * scaling.scaleX;
            const scaledY = pt[1] * scaling.scaleY;
            d3.select(this).select('path')
              .attr('transform', `translate(${scaledX},${scaledY})`);

            const isVisible = pt && window.Celestial.clip(d.geometry.coordinates);
            d3.select(this).style('display', isVisible ? null : 'none');
          }
        });
      }
    });
  }, [navigate, getCanvasScaling, saveViewState, updateGlobeClip]);

  // Add markers when locations are loaded
  useEffect(() => {
    if (mapReady && filteredLocations.length > 0 && !loading) {
      addImagingMarkers(filteredLocations);
    }
  }, [filteredLocations, mapReady, loading, addImagingMarkers]);

  // Target centering handler
  const handleTargetCenter = useCallback((targetId: string) => {
    setSelectedTarget(targetId);
    if (!targetId || typeof window.Celestial === 'undefined') return;

    const loc = locations.find(l => String(l.id) === targetId);
    if (!loc || loc.ra === null || loc.dec === null) return;

    // Center on target
    window.Celestial.rotate({ center: [loc.ra, loc.dec, 0] });

    // Zoom to show ~2x the FOV for context
    if (loc.fovWidth && loc.fovHeight) {
      const maxFovDeg = Math.max(loc.fovWidth, loc.fovHeight);
      // Approximate: at default scale, the visible field is ~180 deg.
      // We want ~2x the FOV visible, so target field = maxFovDeg * 2
      // Scale factor ~ 180 / (targetField)
      const targetField = maxFovDeg * 4; // show some context
      const projection = window.Celestial.mapProjection;
      if (projection && projection.scale) {
        const currentScale = projection.scale();
        // d3-celestial default scale is ~200 for ~180deg field
        // We want targetField degrees visible, so scale = defaultScale * (180 / targetField)
        const defaultScale = 200; // approximate default
        const targetScale = defaultScale * (180 / targetField);
        const zoomFactor = targetScale / currentScale;
        if (window.Celestial.zoomBy && zoomFactor > 0) {
          window.Celestial.zoomBy(zoomFactor);
        }
      }
    }
  }, [locations]);

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

    rectangleSelection.startSelection((result) => {
      setSelectionResult(result);
      setShowDialog(true);
      setDrawingMode('none');
    });

    return () => {
      rectangleSelection.cancelSelection();
    };
  }, [drawingMode, mapReady, rectangleSelection]);

  // Keyboard shortcuts
  useEffect(() => {
    const handleKeyDown = (event: KeyboardEvent) => {
      if (event.target instanceof HTMLInputElement || event.target instanceof HTMLTextAreaElement) {
        return;
      }

      if (event.key === 's' || event.key === 'S') {
        event.preventDefault();
        setDrawingMode(prev => prev === 'rectangle' ? 'none' : 'rectangle');
      }

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
      <div className="h-screen flex items-center justify-center bg-surface">
        <div className="text-center">
          <div className="animate-spin rounded-full h-12 w-12 border-b-2 border-accent mx-auto mb-4"></div>
          <p className="text-content-muted">Loading sky chart...</p>
        </div>
      </div>
    );
  }

  if (error) {
    return (
      <div className="h-screen flex items-center justify-center bg-surface">
        <div className="text-center max-w-md p-6">
          <p className="text-error mb-2 font-semibold">Error loading sky chart</p>
          <p className="text-content-muted text-sm mb-4">{error}</p>
          <button
            onClick={() => window.location.reload()}
            className="px-4 py-2 bg-accent hover:bg-accent-hover text-white rounded-lg transition"
          >
            Retry
          </button>
        </div>
      </div>
    );
  }

  if (locations.length === 0) {
    return (
      <div className="h-screen flex items-center justify-center bg-surface">
        <div className="text-center max-w-md p-6">
          <h3 className="text-xl font-bold text-content mb-2">No Imaging Locations Found</h3>
          <p className="text-content-muted text-sm mb-4">
            You don't have any LIGHT frames with RA/Dec coordinates yet.
          </p>
          <p className="text-content-muted text-sm">
            Once you import FITS/XISF files with coordinate data, they will appear here.
          </p>
        </div>
      </div>
    );
  }

  return (
    <div className="h-screen w-full flex flex-col bg-surface overflow-hidden">
      {/* Header */}
      <div className="flex-shrink-0 py-[14px] px-4 border-b border-border bg-surface-elevated flex items-center justify-between">
        {/* Left: Title + Subtitle */}
        <div>
          <h2 className="text-2xl font-bold text-content">Sky Chart</h2>
          <p className="text-sm text-content-muted">
            Stereographic projection • {filteredLocations.length} imaging location{filteredLocations.length !== 1 ? 's' : ''}
            {filteredLocations.length !== locations.length && ` (${locations.length} total)`}
          </p>
        </div>

        {/* Right: Controls */}
        <div className="flex items-center gap-4">
          <button
            onClick={() => setDrawingMode(prev => prev === 'rectangle' ? 'none' : 'rectangle')}
            disabled={!mapReady}
            title="Select frames in a rectangular region (Press S)"
            className={`px-3 py-1.5 rounded text-sm font-medium transition ${
              drawingMode === 'rectangle'
                ? 'bg-accent text-white shadow-lg'
                : 'bg-surface-hover text-content-secondary hover:bg-surface-hover'
            } ${!mapReady ? 'opacity-50 cursor-not-allowed' : ''}`}
          >
            Select
          </button>

          <div className="w-px h-6 bg-border" />

          {/* Target centering dropdown */}
          <select
            value={selectedTarget}
            onChange={(e) => handleTargetCenter(e.target.value)}
            disabled={!mapReady}
            className="px-2 py-1.5 rounded text-sm bg-surface-hover text-content-secondary border border-border focus:outline-none focus:border-accent"
          >
            <option value="">Center on target...</option>
            {locations
              .filter(loc => loc.objectName)
              .sort((a, b) => (a.objectName || '').localeCompare(b.objectName || ''))
              .map(loc => (
                <option key={loc.id} value={String(loc.id)}>
                  {loc.objectName}
                </option>
              ))
            }
          </select>

          <div className="w-px h-6 bg-border" />

          <DateRangeFilter
            dateFrom={dateFrom}
            dateTo={dateTo}
            onFromChange={setDateFrom}
            onToChange={setDateTo}
          />
        </div>
      </div>

      {/* Sky Map */}
      <div
        id="celestial-map"
        ref={containerRef}
        className="flex-1 w-full overflow-hidden relative"
        style={{ minHeight: 0 }}
      >
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
