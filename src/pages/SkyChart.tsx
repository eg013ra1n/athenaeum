import { useState, useEffect, useLayoutEffect, useRef, useCallback, useMemo } from 'react';
import { api } from '../api';
import { useNavigate, useNavigationType } from 'react-router-dom';
import { ImagingLocation } from '../types/models';
import { DrawingMode, SelectionResult } from '../types/selection';
import { useSvgOverlay } from '../hooks/useSvgOverlay';
import { useCoordinateTransform } from '../hooks/useCoordinateTransform';
import { useD3MouseEvents } from '../hooks/useD3MouseEvents';
import { useRectangleSelection } from '../hooks/useRectangleSelection';
import { useMapViewState } from '../hooks/useMapViewState';
import { SelectionDialog } from '../components/SelectionDialog';
import { DateRangeFilter, DateParts, toISODateRange } from '../components/DateRangeFilter';
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
  // T1-3 — keep heavy callback deps in refs so addImagingMarkers identity
  // stays stable across URL changes (saveViewState recreates on every
  // searchParams write). Without this, the marker effect re-runs on every
  // pan/zoom and re-registers a new Celestial callback each time, never
  // cleaned up — progressively slowing the chart.
  const navigateRef = useRef<((path: string) => void) | null>(null);
  const getCanvasScalingRef = useRef<(() => any) | null>(null);
  const updateGlobeClipRef = useRef<((svg: any, clipId: string, scaling: any) => void) | null>(null);
  const markersRegisteredRef = useRef(false);
  // Latest renderMarkers fn — the one-shot Celestial registration delegates
  // to this ref so every fresh `addImagingMarkers` call (e.g. when the date
  // filter changes) provides the new closure without registering again.
  const renderMarkersRef = useRef<(() => void) | null>(null);
  // T1-6 — track current selected target inside the redraw callback so we
  // can know when its FOV gets hidden by zoom/distortion thresholds.
  const selectedTargetRef = useRef<string>('');
  const [locations, setLocations] = useState<ImagingLocation[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [mapReady, setMapReady] = useState(false);
  const [selectedTarget, setSelectedTarget] = useState<string>('');

  // Selection state
  const [drawingMode, setDrawingMode] = useState<DrawingMode>('none');
  const [selectionResult, setSelectionResult] = useState<SelectionResult | null>(null);
  const [showDialog, setShowDialog] = useState(false);

  // Star density setting
  const [starLimit, setStarLimit] = useState(6);
  const [starLimitLoaded, setStarLimitLoaded] = useState(false);

  // Date filter state
  const [dateFrom, setDateFrom] = useState<DateParts | null>(null);
  const [dateTo, setDateTo] = useState<DateParts | null>(null);

  // T1-6 — when the user has explicitly chosen a target via the dropdown
  // and its FOV becomes hidden by the distortion / span thresholds, surface
  // a small inline hint so the user understands why nothing is highlighted.
  const [fovHiddenForTarget, setFovHiddenForTarget] = useState(false);

  // T2-18 — true while the rectangle-selection backend query is in flight.
  // Drives a small spinner overlay so the user knows their drag was
  // received and is being processed.
  const [isQueryingSelection, setIsQueryingSelection] = useState(false);

  // Brief inline banner shown when a rectangle selection returns zero
  // frames — instead of opening the dialog with "0 frames found", which
  // is noise. Auto-dismisses; the toast pattern matches T1-6 / T2-18.
  const [emptySelectionHint, setEmptySelectionHint] = useState(false);
  const emptySelectionTimerRef = useRef<number | undefined>(undefined);

  // Custom hooks
  const svgOverlay = useSvgOverlay({ containerId: 'celestial-map' });
  const coordinateTransform = useCoordinateTransform();
  const mouseEvents = useD3MouseEvents();
  const rectangleSelection = useRectangleSelection(svgOverlay, coordinateTransform, mouseEvents);
  const { getViewState, saveViewState } = useMapViewState('skychart_view_state');

  const navigate = useNavigate();
  const navigationType = useNavigationType();

  const starDataFile = (limit: number) => limit > 8 ? 'stars.14.json' : 'stars.8.json';

  // Load persisted star density setting before map init
  useEffect(() => {
    api.invoke<string>('get_setting', {
      key: 'skychart.star_limit',
      defaultValue: '6'
    }).then(val => {
      const parsed = parseFloat(val);
      if ([4, 6, 8, 14].includes(parsed)) setStarLimit(parsed);
    }).catch(console.error)
      .finally(() => setStarLimitLoaded(true));
  }, []);

  // Filter locations by date range. Uses the partial-aware helper so the
  // user can enter just a year (→ whole year), or a year + month (→ whole
  // month, leap-year-aware), as well as the existing full DD/MM/YYYY.
  const filteredLocations = useMemo(() => {
    const fromISO = toISODateRange(dateFrom, 'start');
    const toISO = toISODateRange(dateTo, 'end');

    return locations.filter(loc => {
      const locStartDate = loc.dateRange[0]?.split('T')[0];
      const locEndDate = loc.dateRange[1]?.split('T')[0];

      if (fromISO && locEndDate && locEndDate < fromISO) return false;
      if (toISO && locStartDate && locStartDate > toISO) return false;

      return true;
    });
  }, [locations, dateFrom, dateTo]);

  // Fetch imaging locations from backend.
  // T1-7 — guard setState against unmount (page can be left within ms of
  // mount; the in-flight fetch would otherwise warn / leak).
  // T2-19 — wrap raw error string in user-friendly prefix; raw goes to
  // console for support.
  useEffect(() => {
    let mounted = true;
    async function loadLocations() {
      try {
        const data = await api.invoke<ImagingLocation[]>('get_imaging_locations');
        if (!mounted) return;
        setLocations(data);
        setLoading(false);
      } catch (err) {
        console.error('Failed to load imaging locations:', err);
        if (!mounted) return;
        const msg = err instanceof Error ? err.message : String(err);
        setError(`Couldn't load chart data: ${msg}`);
        setLoading(false);
      }
    }
    loadLocations();
    return () => { mounted = false; };
  }, []);

  // T1-5 — clear selectedTarget if its id no longer exists in the loaded
  // locations (e.g. user deleted a frame set in another tab).
  useEffect(() => {
    if (!selectedTarget) return;
    if (!locations.some(l => String(l.id) === selectedTarget)) {
      setSelectedTarget('');
    }
  }, [locations, selectedTarget]);

  // T1-6 — clear the FOV-hidden hint when the user clears their target.
  useEffect(() => {
    if (!selectedTarget) setFovHiddenForTarget(false);
  }, [selectedTarget]);

  // Keep callback refs up to date — these are read by the marker render
  // pipeline (which must NOT re-register on every identity change).
  useEffect(() => { saveViewStateRef.current = saveViewState; }, [saveViewState]);
  useEffect(() => { navigateRef.current = navigate; }, [navigate]);
  useEffect(() => { selectedTargetRef.current = selectedTarget; }, [selectedTarget]);

  // Defensive unmount cleanup: null out every callback ref the registered
  // window.Celestial.add() callbacks read through. d3-celestial's internal
  // fade-in / pan / zoom animations keep firing for a moment after this
  // component unmounts (we have no way to unregister those callbacks),
  // and they were previously calling stale React Router setSearchParams /
  // navigate from this dead hook instance — which in v7 re-navigated the
  // browser back to /skychart. Nulling the refs makes those leftover fires
  // become no-ops.
  useEffect(() => {
    return () => {
      saveViewStateRef.current = null;
      navigateRef.current = null;
      getCanvasScalingRef.current = null;
      updateGlobeClipRef.current = null;
      renderMarkersRef.current = null;
      if (autoSaveTimeoutRef.current !== undefined) {
        window.clearTimeout(autoSaveTimeoutRef.current);
        autoSaveTimeoutRef.current = undefined;
      }
      if (resizeTimeoutRef.current !== undefined) {
        window.clearTimeout(resizeTimeoutRef.current);
        resizeTimeoutRef.current = undefined;
      }
      if (emptySelectionTimerRef.current !== undefined) {
        window.clearTimeout(emptySelectionTimerRef.current);
        emptySelectionTimerRef.current = undefined;
      }
    };
  }, []);

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

  // Keep getCanvasScaling ref current (callback identity is stable since
  // useCallback deps are []; the assignment is a no-op after first run).
  useEffect(() => { getCanvasScalingRef.current = getCanvasScaling; }, [getCanvasScaling]);

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
    if (!starLimitLoaded || !containerRef.current || mapInitialized.current) return;

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

      // Native widescreen: tell the library to create a canvas matching
      // the container dimensions (projectionRatio = width/height).
      const containerAspect = rect.width / rect.height;
      const projectionRatio = containerAspect;
      const initialWidth = Math.floor(rect.width);

      try {
        const config = {
          container: 'celestial-map',
          width: initialWidth,
          projectionRatio: projectionRatio,
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
            limit: starLimit,
            colors: true,
            style: { fill: '#ffffff', opacity: 0.8 },
            designation: true,
            designationLimit: 2.5,
            propername: true,
            propernameLimit: 1.5,
            size: 7,
            exponent: -0.28,
            data: starDataFile(starLimit)
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

        // Fill-zoom: the projection circle fits the height by default.
        // Zoom in by the container aspect ratio so it fills the width,
        // giving a horizontal band through the projection (no distortion).
        // For returning users, the saved absolute zoom already includes the
        // fill-zoom from their previous session, so we restore that directly.
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
        } else {
          // First-time user (no saved state): apply fill-zoom so the
          // projection circle fills the width of the widescreen canvas
          if (containerAspect > 1 && window.Celestial.zoomBy) {
            window.Celestial.zoomBy(containerAspect);
          }
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
  }, [starLimitLoaded]);

  // Handle container resize via ResizeObserver (catches window resize,
  // sidebar toggle, layout changes — anything that changes the container).
  useEffect(() => {
    if (!mapReady || !containerRef.current) return;
    const container = containerRef.current;

    // Track previous dimensions so we can scale zoom proportionally
    let prevRect = container.getBoundingClientRect();

    const observer = new ResizeObserver(() => {
      if (resizeTimeoutRef.current) clearTimeout(resizeTimeoutRef.current);

      resizeTimeoutRef.current = window.setTimeout(() => {
        if (typeof window.Celestial === 'undefined') return;

        const rect = container.getBoundingClientRect();
        const newWidth = Math.floor(rect.width);
        const newHeight = Math.floor(rect.height);

        if (newWidth <= 0 || newHeight <= 0) return;

        // Skip if dimensions haven't meaningfully changed
        if (Math.abs(newWidth - Math.floor(prevRect.width)) < 2 &&
            Math.abs(newHeight - Math.floor(prevRect.height)) < 2) return;

        const oldWidth = prevRect.width;
        const newRatio = rect.width / rect.height;

        // Save current zoom level before resize resets it
        const projection = window.Celestial.mapProjection;
        const currentScale = projection?.scale?.() ?? null;

        prevRect = rect;

        // resize() with patched projectionRatio support: updates the
        // canvas dimensions and projection ratio in a single call.
        // The library's f() re-reads Gt from $.projectionRatio, so the
        // canvas height = width / ratio is computed correctly.
        window.Celestial.resize({ width: newWidth, projectionRatio: newRatio });

        // Restore zoom: scale proportionally to the width change so the
        // same field of view is shown in the new container size.
        if (currentScale !== null && oldWidth > 0 && window.Celestial.zoomBy) {
          const freshScale = window.Celestial.mapProjection?.scale?.() ?? 1;
          const targetScale = currentScale * (newWidth / oldWidth);
          const factor = targetScale / freshScale;
          if (Math.abs(factor - 1) > 0.001) {
            window.Celestial.zoomBy(factor);
          }
        }
      }, 250);
    });

    observer.observe(container);
    return () => {
      observer.disconnect();
      if (resizeTimeoutRef.current) clearTimeout(resizeTimeoutRef.current);
    };
  }, [mapReady]);

  // T1-2 — react to URL changes ONLY when the user used browser back/forward.
  // Our own autosave writes URL params on every pan/zoom (with `replace`),
  // and reading those back to re-apply Celestial.rotate/zoomBy mid-input
  // fights the user's interaction (the live map pos and the saved-via-
  // toFixed(6) URL pos differ slightly, so each replace triggers a tiny
  // correction). `useNavigationType()` is 'POP' only for back/forward and
  // initial mount — exactly the cases we want to restore from.
  const lastAppliedUrlRef = useRef<string>('');
  useEffect(() => {
    if (!mapReady || typeof window.Celestial === 'undefined') return;
    if (navigationType !== 'POP') return;
    const state = getViewState();
    if (state.zoom == null && state.ra == null && state.dec == null) return;

    // Skip re-applying the same URL twice (handles React StrictMode and
    // initial mount where the layout effect already restored the view).
    const sig = `${state.zoom ?? ''}|${state.ra ?? ''}|${state.dec ?? ''}|${state.rotation ?? ''}`;
    if (lastAppliedUrlRef.current === sig) return;
    lastAppliedUrlRef.current = sig;

    const projection = window.Celestial.mapProjection;
    const currentZoom = projection?.scale?.() ?? null;

    if (state.ra != null && state.dec != null) {
      window.Celestial.rotate({ center: [state.ra, state.dec, state.rotation ?? 0] });
    }
    if (state.zoom != null && currentZoom != null && window.Celestial.zoomBy) {
      const factor = state.zoom / currentZoom;
      if (Math.abs(factor - 1) > 0.001) window.Celestial.zoomBy(factor);
    }
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [getViewState, mapReady, navigationType]);

  // Apply star density changes at runtime.
  // Celestial.settings() returns the defaults template (wt), NOT the active
  // rendering config ($). Mutating wt and calling redraw() has no effect.
  // The only way to update $ is Celestial.reload(config) which does
  // Object.assign($, wt.set(config)). This also re-triggers Celestial.add()
  // callbacks so imaging markers are automatically re-created.
  const starLimitInitialized = useRef(false);
  useEffect(() => {
    if (!mapReady || typeof window.Celestial === 'undefined') return;
    // Skip the first run — the initial config already has the correct values
    if (!starLimitInitialized.current) {
      starLimitInitialized.current = true;
      return;
    }
    const rect = containerRef.current?.getBoundingClientRect();
    const reloadRatio = rect && rect.height > 0 ? rect.width / rect.height : 1;
    // reload() updates star data and re-triggers Celestial.add() callbacks.
    // It preserves the current zoom level (r() doesn't reset the projection),
    // so no zoomBy() is needed afterward.
    window.Celestial.reload({
      stars: { limit: starLimit, data: starDataFile(starLimit) },
      projectionRatio: reloadRatio
    });
  }, [starLimit, mapReady]);

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

  useEffect(() => { updateGlobeClipRef.current = updateGlobeClip; }, [updateGlobeClip]);

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

      const scaling = getCanvasScalingRef.current
        ? getCanvasScalingRef.current()
        : { scaleX: 1, scaleY: 1, offsetX: 0, offsetY: 0 };

      // Clip markers to globe boundary
      updateGlobeClipRef.current?.(svg, 'markers-globe-clip', scaling);
      markersGroup.attr('clip-path', 'url(#markers-globe-clip)');

      markersGroup.selectAll('.fov-box')
        .data(data.features)
        .enter().append('g')
        .attr('class', 'fov-box')
        // Group itself is transparent to pointer events so wheel/zoom passes
        // through to the canvas underneath; only the inner painted shapes
        // (the FOV rectangle and the marker icon) reactivate clicks/hover
        // for tooltip + navigation. Without this, hovering over an FOV box
        // froze map zoom anywhere a marker was drawn.
        .style('pointer-events', 'none')
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
            // T2-15 — bumped from 0.15 → 0.28; was nearly invisible on dark sky.
            const fillStyle = `rgba(${r}, ${g_rgb}, ${b}, 0.28)`;

            // Whole-box hit area — transparent fill + thick transparent
            // stroke. pointer-events: all means clicks/dblclicks anywhere
            // inside or near the border register; wheel events get
            // explicitly forwarded to the canvas (see g.on('wheel', …)
            // below) so zoom-over-FOV still works.
            if (d.properties.frameSetId) {
              g.append('path')
                .attr('class', 'fov-hit')
                .attr('d', pathData)
                .style('fill', 'transparent')
                .style('stroke', 'transparent')
                .style('stroke-width', '14px')
                .style('pointer-events', 'all')
                .style('cursor', 'pointer');
            }

            g.append('path')
              .attr('class', 'fov-rect')
              .attr('d', pathData)
              .style('fill', fillStyle)
              .style('stroke', markerColor)
              .style('stroke-width', '2px')
              // Visible rect doesn't need its own hit testing — the
              // sibling .fov-hit captures clicks/dblclicks for us.
              .style('pointer-events', 'none');

            // Object-name label — hidden by default, positioned + shown by
            // the placement pass in the redraw callback. The pill bg + text
            // are kept in a sibling group so we can move both together with
            // a single transform attribute. The whole label is also a
            // click target when the box has a frame set, so users can
            // click the readable name instead of hunting for the border.
            const labelG = g.append('g')
              .attr('class', 'fov-label')
              .style('pointer-events', d.properties.frameSetId ? 'visiblePainted' : 'none')
              .style('cursor', d.properties.frameSetId ? 'pointer' : 'default')
              .style('display', 'none');
            labelG.append('rect')
              .attr('class', 'fov-label-bg')
              .attr('rx', 3)
              .attr('ry', 3)
              .style('fill', 'rgba(0,0,0,0.7)')
              .style('stroke', markerColor)
              .style('stroke-width', '1px');
            labelG.append('text')
              .attr('class', 'fov-label-text')
              .attr('text-anchor', 'middle')
              .attr('dominant-baseline', 'middle')
              .style('font-size', '11px')
              .style('font-family', 'Helvetica, Arial, sans-serif')
              .style('fill', '#ffffff')
              .style('user-select', 'none');

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
                .style('pointer-events', 'visiblePainted')
                .style('cursor', d.properties.frameSetId ? 'pointer' : 'default');
            }
          }

          const isVisible = window.Celestial.clip(d.geometry.coordinates);
          g.style('display', isVisible ? null : 'none');

          // Double-click handler navigates to the object detail page.
          // (Single click is intentionally unused — avoids accidental
          // navigation when the user is just panning / zooming over a
          // dense cluster of FOV boxes.) Uses refs so the callback keeps
          // working across the lifetime of the map without forcing this
          // useCallback to re-run on every navigate identity change.
          //
          // We intentionally do NOT call saveViewState here. The autosave
          // (300 ms debounce on every pan/zoom) has already persisted the
          // current view by the time a dblclick fires, and queuing a
          // setSearchParams() in the same tick as a navigate() causes
          // React Router to land the URL update AFTER the navigation,
          // effectively replacing /objects/X with /skychart?ra=…&dec=…
          // — which presented as "jumps to the frameset and immediately
          // back".
          g.on('dblclick', function() {
            if (d3 && d3.event) {
              d3.event.stopPropagation();
              d3.event.preventDefault();
            }
            const frameSetId = d.properties.frameSetId;
            if (!frameSetId) return;
            navigateRef.current?.(`/objects/${frameSetId}`);
          });

          // Wheel passthrough — the FOV interior now captures all pointer
          // events so dblclick can hit anywhere, but wheel events should
          // still zoom the chart. Re-dispatch on the underlying canvas
          // where d3-celestial's zoom handler is bound. d3 v3 passes the
          // bound datum to .on() callbacks (NOT the native event), so the
          // real WheelEvent comes from `d3.event`.
          g.on('wheel', function() {
            const evt = d3 && d3.event as WheelEvent | undefined;
            const canvas = document.querySelector('#celestial-map canvas') as HTMLCanvasElement | null;
            if (!canvas || !evt) return;
            evt.preventDefault();
            evt.stopPropagation();
            canvas.dispatchEvent(new WheelEvent('wheel', {
              bubbles: true,
              cancelable: true,
              deltaX: evt.deltaX,
              deltaY: evt.deltaY,
              deltaZ: evt.deltaZ,
              deltaMode: evt.deltaMode,
              clientX: evt.clientX,
              clientY: evt.clientY,
              ctrlKey: evt.ctrlKey,
              shiftKey: evt.shiftKey,
              altKey: evt.altKey,
              metaKey: evt.metaKey,
            }));
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
              const displayName = d.properties.name && d.properties.name !== 'Unknown'
                ? d.properties.name
                : '[No Name]';
              return `${typeLabel} ${displayName}\nFrames: ${d.properties.frameCount}\nExposure: ${totalHours}h\nFilters: ${d.properties.filters}${cameras}${focalLengths}${dates}${fovInfo}`;
            });
        });
    };

    // Latest render fn lives in a ref so the registered Celestial callback
    // (set up exactly once below) always invokes the freshest closure.
    renderMarkersRef.current = renderMarkers;
    renderMarkers();

    // Register with Celestial for pan/zoom updates ONCE per page lifetime.
    // Without this guard, every saveViewState identity churn would re-run
    // the marker effect, re-call addImagingMarkers, and append another
    // callback to Celestial's list. That registration is never cleaned up,
    // so callbacks accumulated and the chart got progressively slower.
    if (markersRegisteredRef.current) return;
    markersRegisteredRef.current = true;

    window.Celestial.add({
      type: 'raw',
      callback: () => { renderMarkersRef.current?.(); },
      redraw: function() {
        const map = window.Celestial.map;

        const svg = d3.select('#celestial-map').select('svg.imaging-markers-overlay');
        if (svg.empty()) return;

        const markersGroup = svg.select('g.imaging-markers-layer');
        if (markersGroup.empty()) return;

        const scaling = getCanvasScalingRef.current
          ? getCanvasScalingRef.current()
          : { scaleX: 1, scaleY: 1, offsetX: 0, offsetY: 0 };

        // Update globe clip on every redraw
        updateGlobeClipRef.current?.(svg, 'markers-globe-clip', scaling);

        const canvas = document.querySelector('#celestial-map canvas') as HTMLCanvasElement;
        const canvasWidth = canvas ? canvas.getBoundingClientRect().width : 1000;
        const canvasHeight = canvas ? canvas.getBoundingClientRect().height : 1000;

        // T1-6 — tracks whether the user-selected target's FOV ends up
        // hidden in this redraw pass.
        const targetIdNum = selectedTargetRef.current ? Number(selectedTargetRef.current) : null;
        let selectedTargetHidden = false;
        let selectedTargetSeen = false;

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

            // Update both the visible stroke AND the invisible wide
            // hit-stroke that gives the rectangle a generous click target.
            d3.select(this).selectAll('.fov-rect, .fov-hit')
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

          // T1-6 — note whether this is the user-selected target and
          // whether it ended up hidden after this redraw pass.
          if (targetIdNum != null && d.id === targetIdNum) {
            selectedTargetSeen = true;
            const display = d3.select(this).style('display');
            if (display === 'none') selectedTargetHidden = true;
          }
        });

        // T1-6 — sync the React state outside the hot per-marker loop.
        // useState's setter is a no-op when the value matches, so this
        // doesn't trigger a re-render unless the visibility actually
        // changed.
        const newHidden = selectedTargetSeen && selectedTargetHidden;
        setFovHiddenForTarget(prev => prev === newHidden ? prev : newHidden);

        // ── Object-name label placement pass ──────────────────────────
        //
        // 1. Collect every visible FOV box, regardless of how small the
        //    box itself is on screen — labels remain visible even when
        //    zoomed out.
        // 2. Dedupe by object name — multiple frame sets imaging the same
        //    target collapse to a single label on the largest box.
        // 3. Place labels greedily, sorted by box area descending; skip
        //    any label that would overlap an already-placed one. This is
        //    the actual clutter guard at low zoom: instead of all labels
        //    fighting for the same patch of sky, the larger-area target
        //    wins and the rest stay hidden until the user zooms in.
        // 4. Hide labels that didn't make the cut.
        const LABEL_FONT_SIZE = 11;
        const LABEL_CHAR_W = 6.2;        // approx average char width at 11px Helvetica
        const LABEL_PAD_X = 6;
        const LABEL_PAD_Y = 3;
        const LABEL_OFFSET_Y = 4;        // gap above the FOV box

        type LabelCandidate = {
          el: SVGGElement;
          name: string;
          area: number;
          cx: number;
          topY: number;
          rectWidth: number;
          rectHeight: number;
        };
        const candidates: LabelCandidate[] = [];

        markersGroup.selectAll('.fov-box').each(function(this: any, d: any) {
          const labelG = d3.select(this).select('g.fov-label');
          if (labelG.empty()) return;

          const display = d3.select(this).style('display');
          if (display === 'none') {
            labelG.style('display', 'none');
            return;
          }

          const name = d.properties?.objectName;
          if (!name) {
            labelG.style('display', 'none');
            return;
          }

          // Recompute the projected centre + span from the same corner data
          // used to draw the rectangle (already validated above).
          const corners = (this as any).__fovCorners as [number, number][] | undefined;
          if (!corners) {
            labelG.style('display', 'none');
            return;
          }
          const projected: [number, number][] = [];
          for (const c of corners) {
            const projPt = window.Celestial.map.projection()(c);
            if (!projPt || !isFinite(projPt[0]) || !isFinite(projPt[1])) return;
            projected.push([projPt[0] * scaling.scaleX, projPt[1] * scaling.scaleY]);
          }
          const xs = projected.map(p => p[0]);
          const ys = projected.map(p => p[1]);
          const xSpan = Math.max(...xs) - Math.min(...xs);
          const ySpan = Math.max(...ys) - Math.min(...ys);

          const cx = (Math.min(...xs) + Math.max(...xs)) / 2;
          const topY = Math.min(...ys);
          const rectWidth = name.length * LABEL_CHAR_W + LABEL_PAD_X * 2;
          const rectHeight = LABEL_FONT_SIZE + LABEL_PAD_Y * 2;
          candidates.push({
            el: this as SVGGElement,
            name,
            area: xSpan * ySpan,
            cx,
            topY,
            rectWidth,
            rectHeight,
          });
        });

        // Dedupe by object name — keep the candidate with the largest
        // box. So 50 NGC 7000 frame-sets stacked together render one
        // "NGC 7000" label, not 50 overlapping copies.
        const byName = new Map<string, LabelCandidate>();
        for (const c of candidates) {
          const existing = byName.get(c.name);
          if (!existing || c.area > existing.area) byName.set(c.name, c);
        }

        // Sort largest first so important targets win the collision pass.
        const sorted = [...byName.values()].sort((a, b) => b.area - a.area);
        const placedRects: Array<[number, number, number, number]> = []; // x1, y1, x2, y2

        const intersects = (a: [number, number, number, number], b: [number, number, number, number]) =>
          !(a[2] < b[0] || b[2] < a[0] || a[3] < b[1] || b[3] < a[1]);

        // Hide all label groups first; we'll re-show the placed ones.
        markersGroup.selectAll('.fov-box g.fov-label').style('display', 'none');

        for (const c of sorted) {
          const x1 = c.cx - c.rectWidth / 2;
          const y1 = c.topY - LABEL_OFFSET_Y - c.rectHeight;
          const x2 = x1 + c.rectWidth;
          const y2 = y1 + c.rectHeight;
          const rect: [number, number, number, number] = [x1, y1, x2, y2];

          if (placedRects.some(p => intersects(p, rect))) continue;
          placedRects.push(rect);

          const labelG = d3.select(c.el).select('g.fov-label');
          labelG.style('display', null);
          labelG.select('rect.fov-label-bg')
            .attr('x', x1)
            .attr('y', y1)
            .attr('width', c.rectWidth)
            .attr('height', c.rectHeight);
          labelG.select('text.fov-label-text')
            .attr('x', c.cx)
            .attr('y', y1 + c.rectHeight / 2)
            .text(c.name);
        }
      }
    });
    // Empty deps — all dynamic dependencies (saveViewState, navigate, the
    // callbacks above) are accessed through refs that we keep in sync via
    // separate effects. This is the central fix for T1-3.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

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

  // Stabilize svgOverlay + rectangleSelection refs. Both hooks return brand-
  // new function objects every render, and `useSvgOverlay` calls
  // `setEnabled(...)` from inside `enable()` / `disable()` — which triggers
  // a re-render that churns the api object identity, which cycles the
  // effects below if they depend on the api directly. Reading through refs
  // keeps the effects keyed on the actual semantic deps (drawingMode,
  // mapReady) and breaks the loop that caused the "map restarts when
  // initiating drawing" regression.
  const svgOverlayRef = useRef(svgOverlay);
  useEffect(() => { svgOverlayRef.current = svgOverlay; }, [svgOverlay]);
  const rectangleSelectionRef = useRef(rectangleSelection);
  useEffect(() => { rectangleSelectionRef.current = rectangleSelection; }, [rectangleSelection]);

  // Manage SVG overlay visibility based on drawing mode
  useEffect(() => {
    if (!mapReady) return;
    const overlay = svgOverlayRef.current.getSvg();
    if (!overlay) return;
    if (drawingMode !== 'none') {
      svgOverlayRef.current.enable();
    } else {
      svgOverlayRef.current.disable();
    }
  }, [drawingMode, mapReady]);

  // Handle rectangle selection
  useEffect(() => {
    if (!mapReady || drawingMode !== 'rectangle') return;

    rectangleSelectionRef.current.startSelection(
      (result) => {
        setDrawingMode('none');
        // Skip the dialog entirely when the selection caught no frames.
        // Show a brief "no frames in selection" toast instead so the
        // user gets feedback that their drag was processed.
        if (result.count === 0) {
          if (emptySelectionTimerRef.current !== undefined) {
            window.clearTimeout(emptySelectionTimerRef.current);
          }
          setEmptySelectionHint(true);
          emptySelectionTimerRef.current = window.setTimeout(() => {
            setEmptySelectionHint(false);
            emptySelectionTimerRef.current = undefined;
          }, 2500);
          return;
        }
        setSelectionResult(result);
        setShowDialog(true);
      },
      // T2-18 — flip the spinner overlay on/off around the backend query.
      (querying) => setIsQueryingSelection(querying),
      // T2-19 — surface user-friendly errors via the existing error state.
      (message) => setError(message),
    );

    return () => {
      rectangleSelectionRef.current.cancelSelection();
    };
  }, [drawingMode, mapReady]);

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

  return (
    <div className="h-screen w-full flex flex-col bg-surface overflow-hidden">
      {/* Header */}
      <div className="flex-shrink-0 py-[14px] px-4 border-b border-border bg-surface-elevated flex items-center justify-between">
        {/* Left: Title */}
        <h2 className="text-2xl font-bold text-content">Sky Chart</h2>

        {/* Right: Controls */}
        <div className="flex items-center gap-4">
          {/* T2-10/T2-13/T2-16 — visible kbd hint, distinct active label,
              focus ring for keyboard nav. */}
          <button
            onClick={() => setDrawingMode(prev => prev === 'rectangle' ? 'none' : 'rectangle')}
            disabled={!mapReady}
            title={
              drawingMode === 'rectangle'
                ? 'Drawing… click and drag on the map. Press Esc or S to cancel.'
                : 'Select frames in a rectangular region (S)'
            }
            className={`px-3 py-1.5 rounded text-sm font-medium transition inline-flex items-center gap-2 focus:outline-none focus:ring-2 focus:ring-accent ${
              drawingMode === 'rectangle'
                ? 'bg-accent text-white shadow-lg'
                : 'bg-surface-hover text-content-secondary hover:bg-surface-hover'
            } ${!mapReady ? 'opacity-50 cursor-not-allowed' : ''}`}
          >
            {drawingMode === 'rectangle' ? (
              <>
                <span className="w-2 h-2 rounded-full bg-white animate-pulse" aria-hidden />
                Drawing…
                <kbd className="ml-0.5 inline-flex items-center justify-center h-[18px] min-w-[26px] px-1.5 rounded-md bg-white/20 text-white text-[10px] font-sans font-semibold leading-none tracking-wide">
                  Esc
                </kbd>
              </>
            ) : (
              <>
                Select
                <kbd className="ml-0.5 inline-flex items-center justify-center h-[18px] min-w-[18px] px-1 rounded-md bg-content/10 text-content-muted text-[10px] font-sans font-semibold leading-none tracking-wide">
                  S
                </kbd>
              </>
            )}
          </button>

          <div className="w-px h-6 bg-border" />

          {/* T2-12/T2-16/T2-17 — visible label, focus ring, no-name
              fallback in the option list. */}
          <label htmlFor="skychart-jump-to" className="text-xs text-content-muted">Jump to:</label>
          <select
            id="skychart-jump-to"
            value={selectedTarget}
            onChange={(e) => handleTargetCenter(e.target.value)}
            disabled={!mapReady || loading}
            className="px-2 py-1.5 rounded text-sm bg-surface-hover text-content-secondary border border-border focus:outline-none focus:ring-2 focus:ring-accent focus:border-accent"
          >
            <option value="">Select a target…</option>
            {locations
              .filter(loc => loc.objectName)
              .sort((a, b) => (a.objectName || '').localeCompare(b.objectName || ''))
              .filter((loc, i, arr) => arr.findIndex(l => l.objectName === loc.objectName) === i)
              .map(loc => (
                <option key={loc.id} value={String(loc.id)}>
                  {loc.objectName || '[No Name]'}
                </option>
              ))
            }
          </select>

          <div className="w-px h-6 bg-border" />

          {/* T2-11/T2-16 — magnitude in tooltips per option, focus ring. */}
          <select
            value={starLimit}
            onChange={(e) => {
              const val = parseFloat(e.target.value);
              setStarLimit(val);
              api.invoke('set_setting', {
                key: 'skychart.star_limit',
                value: String(val)
              }).catch(console.error);
            }}
            disabled={!mapReady}
            title="Limiting magnitude for stars on the chart"
            className="px-2 py-1.5 rounded text-sm bg-surface-hover text-content-secondary border border-border focus:outline-none focus:ring-2 focus:ring-accent focus:border-accent"
          >
            <option value="4" title="Stars to magnitude ~4">Stars: Few (mag 4)</option>
            <option value="6" title="Stars to magnitude ~6">Stars: Normal (mag 6)</option>
            <option value="8" title="Stars to magnitude ~8">Stars: Many (mag 8)</option>
            <option value="14" title="Extended catalog ~14k stars">Stars: Dense</option>
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

      {/* Sky Map — always rendered so it can initialize immediately */}
      <div
        id="celestial-map"
        ref={containerRef}
        className="flex-1 w-full overflow-hidden relative"
        style={{ minHeight: 0 }}
      >
        {/* Overlays for loading / error / empty states */}
        {error && (
          <div className="absolute inset-0 z-10 flex items-center justify-center bg-surface bg-opacity-90">
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
        )}
        {/* T1-6 — explain why the selected target's FOV box has vanished
            (zoom/distortion thresholds in the projection). */}
        {fovHiddenForTarget && (
          <div className="absolute top-2 left-1/2 -translate-x-1/2 z-10 bg-warning-muted border border-warning/50 text-warning text-xs px-3 py-1.5 rounded shadow">
            FOV overlay hidden at this zoom — zoom out to see it.
          </div>
        )}

        {/* T2-18 — spinner while the backend query for a rectangle
            selection is in flight. */}
        {isQueryingSelection && (
          <div className="absolute top-2 left-1/2 -translate-x-1/2 z-10 bg-surface-elevated border border-border text-content-secondary text-xs px-3 py-1.5 rounded shadow flex items-center gap-2">
            <div className="animate-spin rounded-full h-3 w-3 border-b-2 border-accent" />
            Querying selection…
          </div>
        )}

        {/* Brief feedback when a selection didn't catch any frames —
            the dialog is suppressed in that case to avoid noise, but
            the user still gets confirmation that the drag was
            processed. */}
        {emptySelectionHint && (
          <div className="absolute top-2 left-1/2 -translate-x-1/2 z-10 bg-surface-elevated border border-border text-content-secondary text-xs px-3 py-1.5 rounded shadow">
            No frames in this region.
          </div>
        )}
        {!loading && !error && locations.length === 0 && (
          <div className="absolute inset-0 z-10 flex items-center justify-center bg-surface bg-opacity-90">
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
        )}
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
