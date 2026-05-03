import { useCallback, useEffect, useMemo, useRef, useState } from 'react';
import { Plus, X, BarChart3, ZoomOut } from 'lucide-react';
import { api } from '../../api';
import type { FrameAnalysis } from '../../types/models';
import type { EnrichedLightFrame } from '../calibration/LightsAnalysisTable';
import type { RejectionThresholds } from '../calibration/RejectionThresholdBar';
import {
  METRIC_REGISTRY,
  METRIC_KEYS,
  transformToChartData,
} from './chartDataTransform';
import { LegendShape, MetricChart } from './MetricChart';

const DEFAULT_METRICS = ['median_fwhm', 'median_eccentricity', 'median_snr', 'stars_detected'];
const SETTINGS_KEY = 'ui.analysis_chart.metrics';

interface LightsAnalysisChartViewProps {
  displayedFrames: EnrichedLightFrame[];
  analysisData: Map<number, FrameAnalysis>;
  selectedFrameIds: Set<number>;
  onSelectionChange: (ids: Set<number>) => void;
  rejectedFrameIds: Set<number>;
  plateScale: number | null;
  useArcsec: boolean;
  thresholds: RejectionThresholds;
  onAnalyze?: () => void;
  analyzing?: boolean;
}

function gridColsClass(count: number): string {
  // 1-3 charts stack vertically — wide-and-short is the best aspect for time series.
  // 4+ charts switch to 2 cols on lg+ so each row stays a reasonable height.
  if (count <= 3) return 'grid-cols-1';
  return 'grid-cols-1 lg:grid-cols-2';
}

export function LightsAnalysisChartView({
  displayedFrames,
  analysisData,
  selectedFrameIds,
  onSelectionChange,
  rejectedFrameIds,
  plateScale,
  useArcsec,
  thresholds,
  onAnalyze,
  analyzing,
}: LightsAnalysisChartViewProps) {
  const [selectedMetrics, setSelectedMetrics] = useState<string[]>(DEFAULT_METRICS);
  const [xDomain, setXDomain] = useState<[number, number] | null>(null);
  const [addMenuOpen, setAddMenuOpen] = useState(false);
  const addMenuRef = useRef<HTMLDivElement>(null);

  // Load persisted metric selection
  useEffect(() => {
    (async () => {
      try {
        const saved = await api.invoke<string>('get_setting', {
          key: SETTINGS_KEY,
          defaultValue: '',
        });
        if (!saved) return;
        const parsed = JSON.parse(saved);
        if (Array.isArray(parsed)) {
          const valid = parsed.filter((k): k is string => typeof k === 'string' && k in METRIC_REGISTRY);
          if (valid.length > 0) setSelectedMetrics(valid);
        }
      } catch {
        /* keep defaults */
      }
    })();
  }, []);

  const persistMetrics = useCallback((metrics: string[]) => {
    api.invoke('set_setting', {
      key: SETTINGS_KEY,
      value: JSON.stringify(metrics),
    }).catch((err) => console.error('Failed to save chart metrics:', err));
  }, []);

  const updateMetrics = useCallback((metrics: string[]) => {
    setSelectedMetrics(metrics);
    persistMetrics(metrics);
  }, [persistMetrics]);

  const removeMetric = useCallback((key: string) => {
    if (selectedMetrics.length <= 1) return; // keep at least one
    updateMetrics(selectedMetrics.filter((k) => k !== key));
  }, [selectedMetrics, updateMetrics]);

  const addMetric = useCallback((key: string) => {
    if (selectedMetrics.includes(key)) return;
    updateMetrics([...selectedMetrics, key]);
    setAddMenuOpen(false);
  }, [selectedMetrics, updateMetrics]);

  // Click-outside for the Add Metric dropdown
  useEffect(() => {
    if (!addMenuOpen) return;
    const handler = (e: MouseEvent) => {
      if (addMenuRef.current && !addMenuRef.current.contains(e.target as Node)) {
        setAddMenuOpen(false);
      }
    };
    document.addEventListener('mousedown', handler);
    return () => document.removeEventListener('mousedown', handler);
  }, [addMenuOpen]);

  // Reset zoom when displayed frames change (filter changed)
  useEffect(() => {
    setXDomain(null);
  }, [displayedFrames]);

  const chartData = useMemo(
    () => transformToChartData(displayedFrames, analysisData),
    [displayedFrames, analysisData]
  );

  const handleZoomSelect = useCallback((left: number, right: number) => {
    setXDomain([left, right]);
  }, []);

  const handleResetZoom = useCallback(() => setXDomain(null), []);

  // Smart-toggle: if every brushed point is already selected, deselect all of them; otherwise select all
  const handleShiftBrush = useCallback((frameIds: number[]) => {
    if (frameIds.length === 0) return;
    const allSelected = frameIds.every((id) => selectedFrameIds.has(id));
    const next = new Set(selectedFrameIds);
    if (allSelected) {
      for (const id of frameIds) next.delete(id);
    } else {
      for (const id of frameIds) next.add(id);
    }
    onSelectionChange(next);
  }, [selectedFrameIds, onSelectionChange]);

  const handlePointClick = useCallback((frameId: number) => {
    const next = new Set(selectedFrameIds);
    if (next.has(frameId)) next.delete(frameId);
    else next.add(frameId);
    onSelectionChange(next);
  }, [selectedFrameIds, onSelectionChange]);

  const getThresholdLine = useCallback((key: string): { value: number; label: string } | null => {
    switch (key) {
      case 'median_fwhm': {
        const v = parseFloat(thresholds.fwhm);
        if (isNaN(v)) return null;
        // Threshold input is in current display units (px or arcsec).
        // Chart Y values for median_fwhm are also scaled to display units below — they match 1:1.
        return { value: v, label: '↑' };
      }
      case 'median_eccentricity': {
        const v = parseFloat(thresholds.eccentricity);
        return isNaN(v) ? null : { value: v, label: '↑' };
      }
      case 'frame_snr': {
        const v = parseFloat(thresholds.frame_snr);
        return isNaN(v) ? null : { value: v, label: '↓' };
      }
      case 'snr_weight': {
        const v = parseFloat(thresholds.snr_weight);
        return isNaN(v) ? null : { value: v, label: '↓' };
      }
      default:
        return null;
    }
  }, [thresholds]);

  const getValueScale = useCallback((key: string): number => {
    const def = METRIC_REGISTRY[key];
    if (def?.arcsecConvertible && useArcsec && plateScale) return plateScale;
    return 1;
  }, [useArcsec, plateScale]);

  const getYAxisLabel = useCallback((key: string): string | undefined => {
    const def = METRIC_REGISTRY[key];
    if (!def) return undefined;
    if (def.arcsecConvertible && useArcsec && plateScale) return '"';
    return def.unit;
  }, [useArcsec, plateScale]);

  const availableToAdd = useMemo(() => {
    const selected = new Set(selectedMetrics);
    return METRIC_KEYS.filter((k) => !selected.has(k));
  }, [selectedMetrics]);

  const hasFrames = displayedFrames.length > 0;
  const hasAnalysis = analysisData.size > 0;
  const showAnalyzePrompt = hasFrames && !hasAnalysis;
  const isZoomed = xDomain !== null;

  return (
    <div className="flex flex-col h-full min-h-0 bg-surface rounded-xl border border-border">
      {/* Metric picker + legend strip */}
      <div className="flex items-center gap-x-3 gap-y-2 flex-wrap px-3 py-2 border-b border-border flex-shrink-0">
        {/* Persistent Reset Zoom — disabled when no chart is zoomed */}
        <button
          onClick={handleResetZoom}
          disabled={!isZoomed}
          className="inline-flex items-center gap-1 px-2 py-0.5 text-xs rounded-full border transition-colors bg-surface-hover hover:enabled:bg-surface-elevated border-border text-content-secondary hover:enabled:text-content disabled:opacity-40 disabled:cursor-not-allowed"
          title={isZoomed ? 'Reset zoom on all charts' : 'No active zoom'}
        >
          <ZoomOut size={11} />
          Reset zoom
        </button>
        <span className="text-content-muted/50">|</span>
        <span className="text-xs text-content-muted font-medium uppercase tracking-wide">Metrics</span>
        <div className="flex items-center gap-1.5 flex-wrap">
          {selectedMetrics.map((key) => {
            const def = METRIC_REGISTRY[key];
            if (!def) return null;
            const canRemove = selectedMetrics.length > 1;
            return (
              <span
                key={key}
                className="inline-flex items-center gap-1 px-2 py-0.5 text-xs rounded-full bg-accent/15 border border-accent/30 text-accent"
              >
                {def.label}
                {canRemove && (
                  <button
                    onClick={() => removeMetric(key)}
                    className="hover:text-error transition-colors"
                    title={`Remove ${def.label}`}
                  >
                    <X size={11} />
                  </button>
                )}
              </span>
            );
          })}

          {/* Add Metric */}
          <div ref={addMenuRef} className="relative">
            <button
              onClick={() => setAddMenuOpen((o) => !o)}
              className="inline-flex items-center gap-1 px-2 py-0.5 text-xs rounded-full bg-surface-hover hover:bg-surface-elevated border border-border text-content-secondary transition-colors"
              title="Add a metric to the grid"
            >
              <Plus size={11} />
              Add metric
            </button>
            {addMenuOpen && (
              <div className="absolute z-20 left-0 top-full mt-1 w-64 max-h-80 overflow-y-auto bg-surface-elevated border border-border rounded-lg shadow-xl py-1">
                {availableToAdd.length === 0 ? (
                  <div className="px-3 py-2 text-xs text-content-muted">All metrics added.</div>
                ) : (
                  availableToAdd.map((k) => {
                    const def = METRIC_REGISTRY[k];
                    if (!def) return null;
                    return (
                      <button
                        key={k}
                        onClick={() => addMetric(k)}
                        className="w-full text-left px-3 py-1.5 text-xs text-content-secondary hover:bg-surface-hover hover:text-content transition-colors flex items-center justify-between"
                      >
                        <span>{def.label}</span>
                        {def.unit && <span className="text-[10px] text-content-muted">{def.unit}</span>}
                      </button>
                    );
                  })
                )}
              </div>
            )}
          </div>

        </div>

        {/* Series legend (right side) */}
        {chartData.seriesList.length > 0 && (
          <div className="flex items-center gap-x-3 gap-y-1 flex-wrap ml-auto">
            {chartData.seriesList.map((series) => (
              <div key={series.key} className="flex items-center gap-1.5">
                <LegendShape shape={series.shape} color={series.color} size={10} />
                <span className="text-[11px] text-content-muted">{series.label}</span>
              </div>
            ))}
          </div>
        )}
      </div>

      {/* Chart body */}
      <div className="flex-1 min-h-0 overflow-y-auto p-3">
        {!hasFrames ? (
          <EmptyMessage text="No frames to display. Adjust the filter on the left." />
        ) : showAnalyzePrompt ? (
          <AnalyzePrompt onAnalyze={onAnalyze} analyzing={analyzing} />
        ) : (
          <div
            className={`grid ${gridColsClass(selectedMetrics.length)} gap-3 h-full`}
            style={{ gridAutoRows: 'minmax(200px, 1fr)' }}
          >
            {selectedMetrics.map((key) => (
              <div
                key={key}
                className="bg-surface-elevated/40 rounded-lg border border-border/60 px-3 py-2 min-h-0 flex flex-col"
              >
                <MetricChart
                  metricKey={key}
                  dataPoints={chartData.dataPoints}
                  nightBoundaries={chartData.nightBoundaries}
                  seriesList={chartData.seriesList}
                  segments={chartData.segments}
                  yAxisLabel={getYAxisLabel(key)}
                  xDomain={xDomain ?? undefined}
                  onZoomSelect={handleZoomSelect}
                  valueScale={getValueScale(key)}
                  selectedFrameIds={selectedFrameIds}
                  rejectedFrameIds={rejectedFrameIds}
                  onPointClick={handlePointClick}
                  onShiftBrush={handleShiftBrush}
                  thresholdLine={getThresholdLine(key)}
                />
              </div>
            ))}
          </div>
        )}
      </div>

      {/* Hint footer */}
      <div className="px-3 py-1.5 border-t border-border flex-shrink-0 text-[10px] text-content-muted flex items-center justify-between">
        <span>Drag = zoom X-axis · Shift+drag = box-select · Click a point = toggle</span>
        {selectedFrameIds.size > 0 && (
          <span className="text-accent">{selectedFrameIds.size} selected</span>
        )}
      </div>
    </div>
  );
}

function EmptyMessage({ text }: { text: string }) {
  return (
    <div className="h-full flex items-center justify-center">
      <p className="text-content-muted text-sm">{text}</p>
    </div>
  );
}

function AnalyzePrompt({ onAnalyze, analyzing }: { onAnalyze?: () => void; analyzing?: boolean }) {
  return (
    <div className="h-full flex flex-col items-center justify-center gap-3 text-center px-6">
      <BarChart3 size={32} className="text-content-muted" />
      <p className="text-content-secondary text-sm max-w-sm">
        Run <span className="font-semibold text-content">Analyze</span> to see frame-quality metrics on the chart.
      </p>
      {onAnalyze && (
        <button
          onClick={onAnalyze}
          disabled={analyzing}
          className="inline-flex items-center gap-1.5 px-4 py-2 text-sm font-medium bg-accent hover:bg-accent-hover disabled:bg-surface-hover disabled:cursor-not-allowed text-white rounded-lg transition-colors"
        >
          <BarChart3 size={14} />
          {analyzing ? 'Analyzing…' : 'Analyze'}
        </button>
      )}
    </div>
  );
}
