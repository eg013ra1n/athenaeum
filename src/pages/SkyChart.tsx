import { useState, useEffect, useLayoutEffect, useRef, useCallback, useMemo } from 'react';
import { api } from '../api';
import { HistoryNav } from '../components/HistoryNav';
import { useNavigate, useSearchParams } from 'react-router-dom';
import { ImagingLocation } from '../types/models';
import { DrawingMode, SelectionResult } from '../types/selection';
import { useSvgOverlay } from '../hooks/useSvgOverlay';
import { useCoordinateTransform } from '../hooks/useCoordinateTransform';
import { useD3MouseEvents } from '../hooks/useD3MouseEvents';
import { useRectangleSelection } from '../hooks/useRectangleSelection';
import { useSkyBackground } from '../hooks/useSkyBackground';
import { SelectionDialog } from '../components/SelectionDialog';
import { DateRangeFilter, DateParts, toISODateRange } from '../components/DateRangeFilter';
import {
  STAR_LIMIT_SETTING_KEY, applyView, attachChart, celestialAvailable, currentStarLimit,
  detachChart, displayChart, isChartDisplayed, loadChartPrefs, resizeChart, setStarLimit as setChartStarLimit,
} from '../components/skychart/celestialHost';
import {
  setFootprintHandlers, setFootprints, setSelectedFootprint,
} from '../components/skychart/footprintLayer';
import { FootprintHidden } from '../components/skychart/footprint';
import { defaultView, parseSkyView, parseViewParams } from '../components/skychart/skyView';
import '../styles/celestial-overrides.css';

/** Jump to target frames the footprint's larger side this many times over. */
const JUMP_FOV_FACTOR = 4;
const VIEW_PARAMS = ['ra', 'dec', 'fov'];

const HIDDEN_HINT: Record<Exclude<FootprintHidden, null>, string> = {
  span: 'FOV overlay hidden at this zoom — zoom out to see it.',
  distortion: 'FOV overlay hidden near the chart edge — pan toward it to see it.',
  clip: 'FOV overlay is on the far side of the sky — pan toward it to see it.',
};

export default function SkyChart() {
  // The slot the persistent chart host is attached to; the chart itself
  // (canvas, data, view) lives in components/skychart/celestialHost.
  const slotRef = useRef<HTMLDivElement>(null);
  const [mapReady, setMapReady] = useState(isChartDisplayed);
  const [locations, setLocations] = useState<ImagingLocation[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [reloadKey, setReloadKey] = useState(0);
  const [selectedTarget, setSelectedTarget] = useState<string>('');
  const [selectedHidden, setSelectedHidden] = useState<FootprintHidden>(null);

  // Selection state
  const [drawingMode, setDrawingMode] = useState<DrawingMode>('none');
  const [selectionResult, setSelectionResult] = useState<SelectionResult | null>(null);
  const [showDialog, setShowDialog] = useState(false);

  const [starLimit, setStarLimit] = useState(currentStarLimit);

  // Date filter state
  const [dateFrom, setDateFrom] = useState<DateParts | null>(null);
  const [dateTo, setDateTo] = useState<DateParts | null>(null);

  // True while the rectangle-selection backend query is in flight.
  const [isQueryingSelection, setIsQueryingSelection] = useState(false);

  // Brief inline banner shown when a rectangle selection returns zero frames.
  const [emptySelectionHint, setEmptySelectionHint] = useState(false);
  const emptySelectionTimerRef = useRef<number | undefined>(undefined);

  const navigate = useNavigate();
  const sky = useSkyBackground(mapReady);
  const [searchParams, setSearchParams] = useSearchParams();

  // Attach the chart host before any other effect runs: useSvgOverlay looks
  // the host up by id, and the canvas must be in the page on first paint.
  useLayoutEffect(() => {
    const slot = slotRef.current;
    if (!slot) return;
    attachChart(slot);
    return () => detachChart(slot);
  }, []);

  const svgOverlay = useSvgOverlay({ containerId: 'celestial-map' });
  const coordinateTransform = useCoordinateTransform();
  const mouseEvents = useD3MouseEvents();
  const rectangleSelection = useRectangleSelection(svgOverlay, coordinateTransform, mouseEvents);

  // Display the chart once per app session, then keep it sized to the slot.
  // Resizes are coalesced to one per frame and keep center and field of view.
  useEffect(() => {
    const slot = slotRef.current;
    if (!slot) return;
    if (!celestialAvailable()) {
      setError('Sky map library not loaded. Please restart the app.');
      return;
    }
    let cancelled = false;
    let frame = 0;
    const sync = () => {
      frame = 0;
      if (cancelled) return;
      const rect = slot.getBoundingClientRect();
      const width = Math.floor(rect.width);
      const height = Math.floor(rect.height);
      if (width <= 0 || height <= 0) return;
      if (isChartDisplayed()) {
        resizeChart(width, height);
        return;
      }
      loadChartPrefs(parseSkyView).then((prefs) => {
        if (cancelled || isChartDisplayed()) return;
        const linked = parseViewParams(new URLSearchParams(window.location.search));
        const view = { ...(prefs.view ?? defaultView(width, height)), ...(linked ?? {}) };
        try {
          displayChart(width, height, prefs.starLimit, view);
          setStarLimit(prefs.starLimit);
          setMapReady(true);
        } catch (err) {
          console.error('[SkyChart] display failed:', err);
          setError(`Failed to initialize sky chart: ${err}`);
        }
      });
    };
    const observer = new ResizeObserver(() => {
      if (!frame) frame = requestAnimationFrame(sync);
    });
    observer.observe(slot);
    sync();
    return () => {
      cancelled = true;
      observer.disconnect();
      if (frame) cancelAnimationFrame(frame);
    };
  }, []);

  // A link to `/skychart?ra=…&dec=…[&fov=…]` moves the chart once; the
  // params are then dropped so the URL never drifts from the live view.
  useEffect(() => {
    if (!mapReady) return;
    if (!VIEW_PARAMS.some((k) => searchParams.has(k))) return;
    const linked = parseViewParams(searchParams);
    if (linked) applyView({ ...linked, roll: 0 });
    const rest = new URLSearchParams(searchParams);
    for (const k of [...VIEW_PARAMS, 'zoom', 'rotation']) rest.delete(k);
    setSearchParams(rest, { replace: true });
  }, [mapReady, searchParams, setSearchParams]);

  // Footprint callbacks; cleared on leave so a late event never navigates.
  useEffect(() => {
    setFootprintHandlers({
      openFrameSet: (id) => navigate(`/objects/${id}`),
      onSelectedHidden: setSelectedHidden,
    });
    return () => {
      setFootprintHandlers(null);
      setSelectedFootprint(null);
    };
  }, [navigate]);

  useEffect(() => () => window.clearTimeout(emptySelectionTimerRef.current), []);

  // Filter locations by date range. The partial-aware helper lets the user
  // enter just a year (→ whole year), a year + month, or a full date.
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

  // Fetch imaging locations from the backend on every visit (the catalog may
  // have changed); the chart keeps the previous footprints until they land.
  useEffect(() => {
    let mounted = true;
    setLoading(true);
    api.invoke<ImagingLocation[]>('get_imaging_locations')
      .then((data) => {
        if (!mounted) return;
        setLocations(data);
        setLoading(false);
      })
      .catch((err) => {
        console.error('Failed to load imaging locations:', err);
        if (!mounted) return;
        const msg = err instanceof Error ? err.message : String(err);
        setError(`Couldn't load chart data: ${msg}`);
        setLoading(false);
      });
    return () => { mounted = false; };
  }, [reloadKey]);

  useEffect(() => {
    if (mapReady && !loading) setFootprints(filteredLocations);
  }, [filteredLocations, mapReady, loading]);

  // Clear selectedTarget if its id no longer exists in the loaded locations
  // (e.g. the frame set was deleted elsewhere).
  useEffect(() => {
    if (!selectedTarget) return;
    if (!locations.some(l => String(l.id) === selectedTarget)) {
      setSelectedTarget('');
    }
  }, [locations, selectedTarget]);

  useEffect(() => {
    setSelectedHidden(null);
    if (mapReady) setSelectedFootprint(selectedTarget ? Number(selectedTarget) : null);
  }, [selectedTarget, mapReady]);

  // Center on a target; with a field of view, frame it the same way at any window width.
  const handleTargetCenter = useCallback((targetId: string) => {
    setSelectedTarget(targetId);
    if (!targetId) return;
    const loc = locations.find(l => String(l.id) === targetId);
    if (!loc || !Number.isFinite(loc.ra) || !Number.isFinite(loc.dec)) return;
    const fov = loc.fovWidth && loc.fovHeight
      ? { fovDeg: Math.max(loc.fovWidth, loc.fovHeight) * JUMP_FOV_FACTOR }
      : {};
    applyView({ ra: loc.ra, dec: loc.dec, roll: 0, ...fov });
  }, [locations]);

  const handleStarLimit = useCallback((limit: number) => {
    setStarLimit(limit);
    setChartStarLimit(limit);
    api.invoke('set_setting', { key: STAR_LIMIT_SETTING_KEY, value: String(limit) })
      .catch((err) => console.error('[SkyChart] saving the star limit failed:', err));
  }, []);

  // svgOverlay / rectangleSelection return new objects every render;
  // reading them through refs keeps the effects below keyed on the
  // semantic deps (drawingMode, mapReady) only.
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
        // No frames caught: a brief hint instead of a "0 frames" dialog.
        if (result.count === 0) {
          window.clearTimeout(emptySelectionTimerRef.current);
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
      (querying) => setIsQueryingSelection(querying),
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

  const retry = () => {
    if (!celestialAvailable()) {
      window.location.reload();
      return;
    }
    setError(null);
    setReloadKey((k) => k + 1);
  };

  return (
    <div className="h-screen w-full flex flex-col bg-surface overflow-hidden">
      {/* Header */}
      <div className="flex-shrink-0 py-[14px] px-4 border-b border-border bg-surface-elevated flex flex-wrap items-center justify-between gap-x-4 gap-y-2">
        {/* Left: Title */}
        <div className="flex items-center gap-2 shrink-0">
          <HistoryNav />
          <h2 className="text-2xl font-bold text-content whitespace-nowrap">Sky Chart</h2>
        </div>

        {/* Right: Controls */}
        <div className="flex flex-wrap items-center justify-end gap-x-4 gap-y-2 min-w-0">
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

          <label htmlFor="skychart-jump-to" className="text-xs text-content-muted whitespace-nowrap">Jump to:</label>
          <select
            id="skychart-jump-to"
            value={selectedTarget}
            onChange={(e) => handleTargetCenter(e.target.value)}
            disabled={!mapReady || loading}
            className="px-2 py-1.5 rounded text-sm bg-surface-hover text-content-secondary border border-border focus:outline-none focus:ring-2 focus:ring-accent focus:border-accent w-56 max-w-full"
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

          <select
            value={starLimit}
            onChange={(e) => handleStarLimit(parseFloat(e.target.value))}
            disabled={!mapReady}
            title="Limiting magnitude for stars on the chart"
            className="px-2 py-1.5 rounded text-sm bg-surface-hover text-content-secondary border border-border focus:outline-none focus:ring-2 focus:ring-accent focus:border-accent"
          >
            <option value="4" title="Stars to magnitude ~4">Stars: Few (mag 4)</option>
            <option value="6" title="Stars to magnitude ~6">Stars: Normal (mag 6)</option>
            <option value="8" title="Stars to magnitude ~8">Stars: Many (mag 8)</option>
            <option value="14" title="Extended catalog: ~118k stars to magnitude ~14">Stars: Dense</option>
          </select>

          <div className="w-px h-6 bg-border" />

          <select
            value={sky.background}
            onChange={(e) => sky.chooseBackground(e.target.value)}
            disabled={!mapReady || sky.surveys.length === 0}
            title="Sky survey image drawn under the chart"
            aria-label="Sky background"
            className="px-2 py-1.5 rounded text-sm bg-surface-hover text-content-secondary border border-border focus:outline-none focus:ring-2 focus:ring-accent focus:border-accent"
          >
            <option value="none">Background: None</option>
            {sky.surveys.map((s) => (
              <option key={s.id} value={s.id}>Background: {s.label}</option>
            ))}
          </select>
          {sky.survey && (
            <input
              type="range"
              min={0.1}
              max={1}
              step={0.05}
              value={sky.opacity}
              onChange={(e) => sky.chooseOpacity(parseFloat(e.target.value))}
              title={`Background opacity ${Math.round(sky.opacity * 100)} %`}
              aria-label="Background opacity"
              className="w-24 accent-accent"
            />
          )}

          <div className="w-px h-6 bg-border" />

          <DateRangeFilter
            dateFrom={dateFrom}
            dateTo={dateTo}
            onFromChange={setDateFrom}
            onToChange={setDateTo}
          />
        </div>
      </div>

      {/* Sky Map: the persistent chart host is attached into the slot. */}
      <div className="flex-1 w-full overflow-hidden relative" style={{ minHeight: 0 }}>
        <div ref={slotRef} className="absolute inset-0" data-testid="skychart-slot" />

        {/* Overlays for loading / error / empty states */}
        {error && (
          <div className="absolute inset-0 z-20 flex items-center justify-center bg-surface bg-opacity-90">
            <div className="text-center max-w-md p-6">
              <p className="text-error mb-2 font-semibold">Error loading sky chart</p>
              <p className="text-content-muted text-sm mb-4">{error}</p>
              <button
                onClick={retry}
                className="px-4 py-2 bg-accent hover:bg-accent-hover text-white rounded-lg transition"
              >
                Retry
              </button>
            </div>
          </div>
        )}
        {/* Why the selected target's FOV box is not drawn. */}
        {selectedTarget && selectedHidden && (
          <div className="absolute top-2 left-1/2 -translate-x-1/2 z-20 bg-warning-muted border border-warning/50 text-warning text-xs px-3 py-1.5 rounded shadow">
            {HIDDEN_HINT[selectedHidden]}
          </div>
        )}

        {/* Spinner while the backend query for a rectangle selection is in flight. */}
        {isQueryingSelection && (
          <div className="absolute top-2 left-1/2 -translate-x-1/2 z-20 bg-surface-elevated border border-border text-content-secondary text-xs px-3 py-1.5 rounded shadow flex items-center gap-2">
            <div className="animate-spin rounded-full h-3 w-3 border-b-2 border-accent" />
            Querying selection…
          </div>
        )}

        {/* Feedback when a selection didn't catch any frames. */}
        {emptySelectionHint && (
          <div className="absolute top-2 left-1/2 -translate-x-1/2 z-20 bg-surface-elevated border border-border text-content-secondary text-xs px-3 py-1.5 rounded shadow">
            No frames in this region.
          </div>
        )}
        {/* Survey credit (always shown while a survey is drawn) and tile status. */}
        {sky.survey && (
          <div className="absolute bottom-1 left-2 right-2 z-10 flex items-end justify-between gap-4 pointer-events-none text-[10px] leading-tight text-content-muted">
            <span className="bg-surface/70 px-1.5 py-0.5 rounded pointer-events-auto">
              {sky.survey.label}: {sky.survey.attribution} · {sky.survey.license}
            </span>
            <span className="bg-surface/70 px-1.5 py-0.5 rounded">
              {!sky.survey.online
                ? 'Offline: cached tiles only'
                : sky.status && sky.status.loading > 0
                  ? `Loading sky tiles… ${sky.status.drawn}/${sky.status.wanted}`
                  : sky.status && sky.status.failed > 0
                    ? `${sky.status.failed} sky tiles unavailable`
                    : ''}
            </span>
          </div>
        )}
        {!loading && !error && locations.length === 0 && (
          <div className="absolute inset-0 z-20 flex items-center justify-center bg-surface bg-opacity-90">
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
