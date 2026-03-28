import { useState, useEffect, useMemo, useCallback, useRef } from 'react';
import { Play, Trash2, BarChart3, Download, Check, LineChart } from 'lucide-react';
import { api } from '../api';
import type {
  CalibrationHierarchyView as CalibrationHierarchyViewData,
  FrameAnalysis,
  AnalyzeFrameSetResult,
  AnalysisProgressEvent,
} from '../types/models';
import { MergedCameraFilterTree } from './calibration/MergedCameraFilterTree';
import { LightsAnalysisTable, type EnrichedLightFrame } from './calibration/LightsAnalysisTable';
import { RejectionThresholdBar, RejectionThresholds, EMPTY_THRESHOLDS } from './calibration/RejectionThresholdBar';
import { buildMergedCameraFilterTree } from './calibration/utils';
import { AnalysisChartsModal } from './analysis/AnalysisChartsModal';

interface LightsAnalysisViewProps {
  hierarchy: CalibrationHierarchyViewData;
  frameSetId: number;
  frameSetName?: string;
  onRefresh?: () => void;
  onBlink?: (frameIds: number[]) => void;
}

export function LightsAnalysisView({ hierarchy, frameSetId, frameSetName, onRefresh, onBlink }: LightsAnalysisViewProps) {
  // Tree checkbox state — which filter groups are checked (for filtering the table)
  const [checkedKeys, setCheckedKeys] = useState<Set<string>>(new Set());
  // Table row selection — which individual frames are selected (for mass actions)
  const [selectedFrameIds, setSelectedFrameIds] = useState<Set<number>>(new Set());
  const [blackholedFileIds, setBlackholedFileIds] = useState<Set<number>>(new Set());
  const [blackholing, setBlackholing] = useState(false);
  const [thresholds, setThresholds] = useState<RejectionThresholds>(EMPTY_THRESHOLDS);
  const [defaultThresholds, setDefaultThresholds] = useState<RejectionThresholds | null>(null);

  // Analysis state
  const [analysisData, setAnalysisData] = useState<Map<number, FrameAnalysis>>(new Map());
  const [analyzing, setAnalyzing] = useState(false);
  const [analysisProgress, setAnalysisProgress] = useState<AnalysisProgressEvent | null>(null);
  const [csvExportedMsg, setCsvExportedMsg] = useState<string | null>(null);
  const csvTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [chartsOpen, setChartsOpen] = useState(false);
  const [useArcsec, setUseArcsec] = useState(false);

  const { nodes, framesByKey, allFrames } = useMemo(
    () => buildMergedCameraFilterTree(hierarchy),
    [hierarchy]
  );

  // Load existing analysis data on mount
  useEffect(() => {
    loadAnalysisData();
  }, [frameSetId]);

  // Load saved rejection defaults on mount
  useEffect(() => {
    (async () => {
      try {
        const json = await api.invoke<string>('get_setting', {
          key: 'analysis.rejection_defaults',
          defaultValue: '',
        });
        if (json) {
          const saved = JSON.parse(json) as RejectionThresholds;
          const merged = { ...EMPTY_THRESHOLDS, ...saved };
          setDefaultThresholds(merged);
          setThresholds(merged);
        }
      } catch (err) {
        console.error('Failed to load rejection defaults:', err);
      }
    })();
  }, []);

  const loadAnalysisData = useCallback(async () => {
    try {
      const results = await api.invoke<FrameAnalysis[]>('get_analysis_for_frame_set', {
        frameSetId,
      });
      const map = new Map<number, FrameAnalysis>();
      for (const a of results) {
        map.set(a.frame_id, a);
      }
      setAnalysisData(map);
    } catch (err) {
      console.error('Failed to load analysis data:', err);
    }
  }, [frameSetId]);

  // Run analysis on all frames
  const handleAnalyzeAll = useCallback(async (force?: boolean) => {
    setAnalyzing(true);
    setAnalysisProgress(null);

    // Listen for progress events
    const unlisten = await api.listen<AnalysisProgressEvent>('analysis-progress', (payload) => {
      setAnalysisProgress(payload);
    });

    try {
      const result = await api.invoke<AnalyzeFrameSetResult>('analyze_frame_set', {
        frameSetId,
        force: force ?? false,
      });

      if (result.errors.length > 0) {
        console.error('Analysis errors:', result.errors);
      }

      // Reload analysis data
      await loadAnalysisData();
    } catch (err) {
      console.error('Failed to analyze frame set:', err);
    } finally {
      unlisten();
      setAnalyzing(false);
      setAnalysisProgress(null);
    }
  }, [frameSetId, loadAnalysisData]);

  const handleClearThresholds = useCallback(() => {
    setThresholds(EMPTY_THRESHOLDS);
  }, []);

  const handleLoadDefaults = useCallback(() => {
    if (defaultThresholds) setThresholds(defaultThresholds);
  }, [defaultThresholds]);

  // Frames shown in the table: filtered by checked tree items, or all if nothing checked
  const displayedFrames = useMemo(() => {
    if (checkedKeys.size === 0) return allFrames;
    const frames: EnrichedLightFrame[] = [];
    for (const key of checkedKeys) {
      const keyFrames = framesByKey.get(key);
      if (keyFrames) frames.push(...keyFrames);
    }
    return frames;
  }, [checkedKeys, allFrames, framesByKey]);

  // Plate scale from first frame with optics data (arcsec/pixel)
  const plateScale = useMemo(() => {
    for (const f of allFrames) {
      if (f.focallen && f.xpixsz) {
        return (f.xpixsz / f.focallen) * 206.265;
      }
    }
    return null;
  }, [allFrames]);

  // Count of frames in checked filter groups (for the action bar)
  const checkedFrameCount = useMemo(() => {
    if (checkedKeys.size === 0) return 0;
    let count = 0;
    for (const key of checkedKeys) {
      const keyFrames = framesByKey.get(key);
      if (keyFrames) count += keyFrames.length;
    }
    return count;
  }, [checkedKeys, framesByKey]);

  // Compute rejected frame IDs from thresholds
  const rejectedFrameIds = useMemo(() => {
    const rejected = new Set<number>();
    // When in arcsec mode, the user enters thresholds in arcsec — convert back to pixels for comparison
    const ps = (useArcsec && plateScale) ? plateScale : null;
    const rawFwhm = parseFloat(thresholds.fwhm);
    const fwhmThreshold = ps ? rawFwhm / ps : rawFwhm;
    const eccThreshold = parseFloat(thresholds.eccentricity);
    const medianSnrThreshold = parseFloat(thresholds.median_snr);
    const snrDbThreshold = parseFloat(thresholds.frame_snr);
    const psfThreshold = parseFloat(thresholds.psf_signal);
    const snrWtThreshold = parseFloat(thresholds.snr_weight);
    const rejectTrailed = thresholds.trail === 'true';
    const starsThreshold = parseFloat(thresholds.stars);
    const scoreThreshold = parseFloat(thresholds.score);

    for (const frame of displayedFrames) {
      const a = analysisData.get(frame.frame_id);
      if (!a) continue;

      // FWHM > threshold = rejected (worse)
      if (!isNaN(fwhmThreshold) && a.median_fwhm > fwhmThreshold) {
        rejected.add(frame.frame_id);
        continue;
      }
      // Eccentricity > threshold = rejected (worse)
      if (!isNaN(eccThreshold) && a.median_eccentricity > eccThreshold) {
        rejected.add(frame.frame_id);
        continue;
      }
      // SNR < threshold = rejected (worse)
      if (!isNaN(medianSnrThreshold) && a.median_snr < medianSnrThreshold) {
        rejected.add(frame.frame_id);
        continue;
      }
      // Frame SNR < threshold = rejected (worse)
      if (!isNaN(snrDbThreshold) && a.frame_snr < snrDbThreshold) {
        rejected.add(frame.frame_id);
        continue;
      }
      // PSF Signal < threshold = rejected (worse)
      if (!isNaN(psfThreshold) && a.psf_signal < psfThreshold) {
        rejected.add(frame.frame_id);
        continue;
      }
      // SNR Weight < threshold = rejected (worse)
      if (!isNaN(snrWtThreshold) && a.snr_weight < snrWtThreshold) {
        rejected.add(frame.frame_id);
        continue;
      }
      // Reject frames flagged as trailed
      if (rejectTrailed && a.possibly_trailed) {
        rejected.add(frame.frame_id);
        continue;
      }
      // Stars < threshold = rejected (worse)
      if (!isNaN(starsThreshold) && a.stars_detected < starsThreshold) {
        rejected.add(frame.frame_id);
        continue;
      }
      // Score < threshold (as percentage) = rejected (worse)
      if (!isNaN(scoreThreshold) && a.quality_score != null && a.quality_score * 100 < scoreThreshold) {
        rejected.add(frame.frame_id);
        continue;
      }
    }

    return rejected;
  }, [thresholds, displayedFrames, analysisData, useArcsec, plateScale]);

  // Auto-select rejected frames when thresholds change
  useEffect(() => {
    setSelectedFrameIds(rejectedFrameIds);
  }, [rejectedFrameIds]);

  // Clear table selection when tree filter changes
  const handleCheckedChange = useCallback((keys: Set<string>) => {
    setCheckedKeys(keys);
    setSelectedFrameIds(new Set());
  }, []);

  const handleBlackhole = useCallback((fileId: number) => {
    setBlackholedFileIds(prev => new Set([...prev, fileId]));
    setSelectedFrameIds(prev => {
      const frame = allFrames.find(f => f.file_id === fileId);
      if (frame) {
        const next = new Set(prev);
        next.delete(frame.frame_id);
        return next;
      }
      return prev;
    });
    onRefresh?.();
  }, [onRefresh, allFrames]);

  const handleBlinkSelected = useCallback(() => {
    if (selectedFrameIds.size === 0) return;
    onBlink?.([...selectedFrameIds]);
  }, [selectedFrameIds, onBlink]);

  const handleBlackholeSelected = useCallback(async () => {
    if (selectedFrameIds.size === 0) return;

    const fileIds = allFrames
      .filter(f => selectedFrameIds.has(f.frame_id) && !blackholedFileIds.has(f.file_id))
      .map(f => f.file_id);

    if (fileIds.length === 0) return;

    setBlackholing(true);
    try {
      for (const fileId of fileIds) {
        await api.invoke('move_to_black_hole', { fileId, fromWhere: 'frame_set_detail' });
      }
      setBlackholedFileIds(prev => new Set([...prev, ...fileIds]));
      setSelectedFrameIds(new Set());
      onRefresh?.();
    } catch (err) {
      console.error('Failed to blackhole selected frames:', err);
    } finally {
      setBlackholing(false);
    }
  }, [selectedFrameIds, allFrames, blackholedFileIds, onRefresh]);

  const handleExportCsv = useCallback(() => {
    const headers = ['Filename', 'Date/Time', 'Camera', 'Filter', 'Exposure', 'Stars', 'FWHM (px)', 'Eccentricity', 'SNR', 'Frame SNR (dB)', 'PSF Signal (ADU)', 'SNR Weight', 'Trail R\u00B2', 'Score'];
    const rows = displayedFrames.map(frame => {
      const a = analysisData.get(frame.frame_id);
      return [
        frame.filename,
        frame.date_obs ?? '',
        frame.camera ?? '',
        frame.filter ?? '',
        frame.exptime != null ? String(frame.exptime) : '',
        a ? String(a.stars_detected) : '',
        a ? a.median_fwhm.toFixed(2) : '',
        a ? a.median_eccentricity.toFixed(3) : '',
        a ? a.median_snr.toFixed(1) : '',
        a ? a.frame_snr.toFixed(1) : '',
        a ? a.psf_signal.toFixed(1) : '',
        a ? a.snr_weight.toFixed(1) : '',
        a ? a.trail_r_squared.toFixed(4) : '',
        a?.quality_score != null ? (a.quality_score * 100).toFixed(1) : '',
      ].map(v => `"${String(v).replace(/"/g, '""')}"`).join(',');
    });

    const csv = [headers.join(','), ...rows].join('\n');
    const blob = new Blob([csv], { type: 'text/csv;charset=utf-8;' });
    const url = URL.createObjectURL(blob);
    const now = new Date();
    const timestamp = `${now.getFullYear()}${String(now.getMonth() + 1).padStart(2, '0')}${String(now.getDate()).padStart(2, '0')}_${String(now.getHours()).padStart(2, '0')}${String(now.getMinutes()).padStart(2, '0')}${String(now.getSeconds()).padStart(2, '0')}`;
    const safeName = (frameSetName || 'analysis').replace(/[^a-zA-Z0-9_-]/g, '_');
    const a = document.createElement('a');
    a.href = url;
    a.download = `${timestamp}_${safeName}.csv`;
    a.click();
    URL.revokeObjectURL(url);

    if (csvTimerRef.current) clearTimeout(csvTimerRef.current);
    setCsvExportedMsg(`Exported ${rows.length} frames to CSV`);
    csvTimerRef.current = setTimeout(() => setCsvExportedMsg(null), 3000);
  }, [displayedFrames, analysisData]);

  const analyzedCount = analysisData.size;
  const totalLightFrames = allFrames.length;

  return (
    <div className="flex flex-col h-full">
      {/* Analysis Action Bar */}
      <div className="flex items-center gap-3 mb-3 flex-shrink-0">
        <button
          onClick={() => handleAnalyzeAll(false)}
          disabled={analyzing}
          className="inline-flex items-center gap-2 px-4 py-2 bg-accent hover:bg-accent-hover disabled:bg-surface-hover disabled:cursor-not-allowed text-white text-sm rounded-lg transition-colors"
        >
          <BarChart3 size={16} />
          {analyzing ? 'Analyzing...' : 'Analyze All'}
        </button>
        {analyzedCount > 0 && (
          <button
            onClick={() => handleAnalyzeAll(true)}
            disabled={analyzing}
            className="inline-flex items-center gap-1.5 px-3 py-2 bg-surface-hover hover:bg-surface-hover text-content-secondary text-sm rounded-lg transition-colors disabled:opacity-50"
          >
            Re-analyze
          </button>
        )}
        <div className="text-xs text-content-muted">
          {analyzedCount > 0
            ? `${analyzedCount} / ${totalLightFrames} frames analyzed`
            : 'No analysis data yet'
          }
          {rejectedFrameIds.size > 0 && (
            <span className="ml-2 text-error">
              {rejectedFrameIds.size} rejected
            </span>
          )}
        </div>
        {csvExportedMsg && (
          <span className="ml-auto inline-flex items-center gap-1 text-xs text-green-400">
            <Check size={14} />
            {csvExportedMsg} — check your Downloads folder
          </span>
        )}
        <button
          onClick={handleExportCsv}
          disabled={analysisData.size === 0}
          className={`${csvExportedMsg ? '' : 'ml-auto '}inline-flex items-center gap-1.5 px-3 py-2 bg-surface-hover hover:bg-surface-hover text-content-secondary text-sm rounded-lg transition-colors disabled:opacity-50 disabled:cursor-not-allowed`}
        >
          <Download size={16} />
          Export CSV{checkedKeys.size > 0 ? ' (filtered)' : ''}
        </button>
        <button
          onClick={() => setChartsOpen(true)}
          disabled={analysisData.size === 0}
          className="inline-flex items-center gap-1.5 px-3 py-2 bg-surface-hover hover:bg-surface-hover text-content-secondary text-sm rounded-lg transition-colors disabled:opacity-50 disabled:cursor-not-allowed"
        >
          <LineChart size={16} />
          Charts{checkedKeys.size > 0 ? ' (filtered)' : ''}
        </button>
      </div>

      {/* Progress Bar */}
      {analyzing && analysisProgress && (
        <div className="mb-3 flex-shrink-0">
          <div className="flex items-center justify-between text-xs text-content-muted mb-1">
            <span>{analysisProgress.current_file}</span>
            <span>{analysisProgress.current} / {analysisProgress.total} ({analysisProgress.percent.toFixed(0)}%)</span>
          </div>
          <div className="w-full bg-surface-hover rounded-full h-2">
            <div
              className="bg-accent h-2 rounded-full transition-all duration-300"
              style={{ width: `${analysisProgress.percent}%` }}
            />
          </div>
        </div>
      )}

      {/* Main Content — two-panel layout */}
      <div className="flex flex-1 min-h-0 gap-4">
        {/* Left panel — Navigation tree */}
        <MergedCameraFilterTree
          nodes={nodes}
          checkedKeys={checkedKeys}
          onCheckedChange={handleCheckedChange}
          className="w-80 flex-shrink-0"
        />

        {/* Right panel — Threshold bar + Table */}
        <div className="flex-1 min-w-0 flex flex-col gap-3">
          <div className="flex items-start gap-2 flex-wrap">
            {plateScale && (
              <>
                <div className="flex flex-col items-center gap-0.5 flex-shrink-0">
                  <button
                    onClick={() => setUseArcsec(prev => !prev)}
                    className={`w-10 h-7 text-xs font-medium rounded-lg border transition-colors ${
                      useArcsec
                        ? 'bg-accent/20 border-accent text-accent'
                        : 'bg-surface-hover border-border text-content-secondary hover:text-content'
                    }`}
                    title="Toggle star measurement units between pixels and arcseconds"
                  >
                    {useArcsec ? '"' : 'px'}
                  </button>
                  <span className="text-[10px] text-content-muted leading-tight">units</span>
                </div>
                <span className="text-border self-center">|</span>
              </>
            )}
            <RejectionThresholdBar
              thresholds={thresholds}
              onChange={setThresholds}
              onClear={handleClearThresholds}
              onLoadDefaults={handleLoadDefaults}
              hasDefaults={defaultThresholds !== null}
              useArcsec={useArcsec && !!plateScale}
            />
          </div>

          <div className="flex-1 min-h-0 overflow-y-auto border border-border rounded-xl">
            <LightsAnalysisTable
              frames={displayedFrames}
              blackholedFileIds={blackholedFileIds}
              selectedFrameIds={selectedFrameIds}
              onSelectionChange={setSelectedFrameIds}
              onBlackhole={handleBlackhole}
              analysisData={analysisData}
              rejectedFrameIds={rejectedFrameIds}
              plateScale={useArcsec ? plateScale : null}
            />
          </div>
        </div>
      </div>

      {/* Bottom Action Bar — visible when table rows are selected */}
      {selectedFrameIds.size > 0 && (
        <div className="mt-3 bg-surface-elevated/80 rounded-lg p-3 border border-border/50">
          <div className="flex items-center justify-between">
            {/* Left side: Action buttons */}
            <div className="flex items-center gap-2">
              <button
                onClick={handleBlinkSelected}
                className="
                  inline-flex items-center gap-1.5
                  px-3 py-1.5
                  bg-cyan-600 hover:bg-cyan-700
                  text-white text-sm
                  rounded
                  transition-colors
                  focus:outline-none focus-visible:ring-1 focus-visible:ring-cyan-500
                "
              >
                <Play size={14} aria-hidden="true" />
                Blink Selected ({selectedFrameIds.size})
              </button>
              <span className="text-content-muted">|</span>
              <button
                onClick={handleBlackholeSelected}
                disabled={blackholing}
                className="
                  inline-flex items-center gap-1.5
                  px-3 py-1.5
                  bg-error hover:brightness-90
                  disabled:opacity-50
                  text-white text-sm
                  rounded
                  transition-colors
                  focus:outline-none focus-visible:ring-1 focus-visible:ring-error
                "
              >
                <Trash2 size={14} aria-hidden="true" />
                {blackholing ? 'Moving...' : 'Blackhole Selected'}
              </button>
            </div>
            {/* Right side: Selection info and Clear */}
            <div className="flex items-center gap-3">
              <div className="text-sm text-content-secondary">
                <span className="font-medium text-content">{selectedFrameIds.size}</span>{' '}
                frame{selectedFrameIds.size !== 1 ? 's' : ''}
                {checkedKeys.size > 0 && (
                  <span className="text-content-muted ml-1">
                    (filtered to {checkedFrameCount})
                  </span>
                )}
              </div>
              <button
                onClick={() => setSelectedFrameIds(new Set())}
                className="
                  px-3 py-1.5
                  text-content-muted hover:text-content
                  text-sm
                  rounded
                  transition-colors
                  focus:outline-none focus-visible:ring-1 focus-visible:ring-border
                "
              >
                Clear
              </button>
            </div>
          </div>
        </div>
      )}

      <AnalysisChartsModal
        isOpen={chartsOpen}
        onClose={() => setChartsOpen(false)}
        displayedFrames={displayedFrames}
        analysisData={analysisData}
        frameSetName={frameSetName}
      />
    </div>
  );
}
