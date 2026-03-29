import { useState, useEffect, useMemo, useCallback, useRef } from 'react';
import { Play, Trash2, BarChart3, Download, Check, LineChart, X, Scissors, Plus, Calendar, Camera, RotateCcw } from 'lucide-react';
import { api } from '../api';
import type {
  CalibrationHierarchyView as CalibrationHierarchyViewData,
  FrameAnalysis,
} from '../types/models';
import { CameraFilterTree } from './calibration/CameraFilterTree';
import { MergedCameraFilterTree } from './calibration/MergedCameraFilterTree';
import { LightsAnalysisTable, type EnrichedLightFrame } from './calibration/LightsAnalysisTable';
import { RejectionThresholdBar, RejectionThresholds, EMPTY_THRESHOLDS } from './calibration/RejectionThresholdBar';
import { buildCameraFilterTree, buildMergedCameraFilterTree } from './calibration/utils';
import { BlackholedFramesSection } from './calibration/BlackholedFramesSection';
import { AnalysisChartsModal } from './analysis/AnalysisChartsModal';
import { useAnalysisProgressContext } from '../contexts/AnalysisProgressContext';

interface LightsAnalysisViewProps {
  hierarchy: CalibrationHierarchyViewData;
  frameSetId: number;
  frameSetName?: string;
  blackholedFileIds: Set<number>;
  onRefresh?: () => void;
  onBlink?: (frameIds: number[]) => void;
  onSplit?: (selectedFilterKeys: Set<string>) => void;
  onCreateCustomSet?: (selectedFilterKeys: Set<string>) => void;
}

export function LightsAnalysisView({ hierarchy, frameSetId, frameSetName, blackholedFileIds, onRefresh: _onRefresh, onBlink, onSplit, onCreateCustomSet }: LightsAnalysisViewProps) {
  // View mode: by-night (date→camera→filter) or by-camera (camera→filter)
  const [viewMode, setViewMode] = useState<'by-night' | 'by-camera'>('by-camera');

  // Load persisted view mode on mount
  useEffect(() => {
    (async () => {
      try {
        const saved = await api.invoke<string>('get_setting', { key: 'ui.tree_view_mode', defaultValue: 'by-camera' });
        if (saved === 'by-night') setViewMode('by-night');
        else setViewMode('by-camera');
      } catch { /* use default */ }
    })();
  }, []);

  const handleViewModeChange = useCallback((mode: 'by-night' | 'by-camera') => {
    setViewMode(mode);
    setCheckedKeys(new Set());
    api.invoke('set_setting', { key: 'ui.tree_view_mode', value: mode }).catch(() => {});
  }, []);

  // Tree checkbox state — which filter groups are checked (for filtering the table)
  const [checkedKeys, setCheckedKeys] = useState<Set<string>>(new Set());
  // Table row selection — which individual frames are selected (for mass actions)
  const [selectedFrameIds, setSelectedFrameIds] = useState<Set<number>>(new Set());
  const [blackholing, setBlackholing] = useState(false);
  const [thresholds, setThresholds] = useState<RejectionThresholds>(EMPTY_THRESHOLDS);
  const [defaultThresholds, setDefaultThresholds] = useState<RejectionThresholds | null>(null);

  // Analysis state — uses global context for queue/progress
  const { enqueueAnalysis, isAnalyzing, cancelAnalysis, activeAnalyses } = useAnalysisProgressContext();
  const analyzing = isAnalyzing(frameSetId);
  const currentAnalysis = activeAnalyses.get(frameSetId);
  const analysisProgress = currentAnalysis?.progress ?? null;

  const [analysisData, setAnalysisData] = useState<Map<number, FrameAnalysis>>(new Map());
  const [csvExportedMsg, setCsvExportedMsg] = useState<string | null>(null);
  const csvTimerRef = useRef<ReturnType<typeof setTimeout> | null>(null);
  const [chartsOpen, setChartsOpen] = useState(false);
  const [useArcsec, setUseArcsec] = useState(false);

  // Build both tree structures
  const dateTree = useMemo(() => buildCameraFilterTree(hierarchy), [hierarchy]);
  const mergedTree = useMemo(() => buildMergedCameraFilterTree(hierarchy), [hierarchy]);

  // Select based on viewMode
  const framesByKey = viewMode === 'by-night' ? dateTree.framesByKey : mergedTree.framesByKey;
  const allFramesRaw = viewMode === 'by-night' ? dateTree.allFrames : mergedTree.allFrames;

  // Split into active and blackholed frames
  const allFrames = useMemo(
    () => allFramesRaw.filter(f => !blackholedFileIds.has(f.file_id)),
    [allFramesRaw, blackholedFileIds]
  );
  const blackholedFrames = useMemo(
    () => allFramesRaw.filter(f => blackholedFileIds.has(f.file_id)),
    [allFramesRaw, blackholedFileIds]
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

  // Enqueue analysis via global queue
  const handleAnalyzeAll = useCallback((force?: boolean) => {
    enqueueAnalysis(frameSetId, frameSetName, force);
  }, [frameSetId, frameSetName, enqueueAnalysis]);

  // Reload analysis data when analysis completes
  useEffect(() => {
    if (currentAnalysis?.isComplete) {
      loadAnalysisData();
    }
  }, [currentAnalysis?.isComplete, loadAnalysisData]);

  const handleClearThresholds = useCallback(() => {
    setThresholds(EMPTY_THRESHOLDS);
  }, []);

  const handleLoadDefaults = useCallback(() => {
    if (defaultThresholds) setThresholds(defaultThresholds);
  }, [defaultThresholds]);

  const handleSplit = useCallback(() => {
    if (onSplit && checkedKeys.size > 0) onSplit(checkedKeys);
  }, [onSplit, checkedKeys]);

  const handleCreateCustomSet = useCallback(() => {
    if (onCreateCustomSet && checkedKeys.size > 0) onCreateCustomSet(checkedKeys);
  }, [onCreateCustomSet, checkedKeys]);

  // Frames shown in the table: filtered by checked tree items, or all if nothing checked
  const displayedFrames = useMemo(() => {
    if (checkedKeys.size === 0) return allFrames;
    const frames: EnrichedLightFrame[] = [];
    for (const key of checkedKeys) {
      const keyFrames = framesByKey.get(key);
      if (keyFrames) frames.push(...keyFrames);
    }
    return frames.filter(f => !blackholedFileIds.has(f.file_id));
  }, [checkedKeys, allFrames, framesByKey, blackholedFileIds]);

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
    const snrDbThreshold = parseFloat(thresholds.frame_snr);
    const snrWtThreshold = parseFloat(thresholds.snr_weight);
    const rejectTrailed = thresholds.trail === 'true';

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
      // Frame SNR < threshold = rejected (worse)
      if (!isNaN(snrDbThreshold) && a.frame_snr < snrDbThreshold) {
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
    // Deselect the blackholed frame; state update happens via event
    setSelectedFrameIds(prev => {
      const frame = allFrames.find(f => f.file_id === fileId);
      if (frame) {
        const next = new Set(prev);
        next.delete(frame.frame_id);
        return next;
      }
      return prev;
    });
  }, [allFrames]);

  const handleBlinkSelected = useCallback(() => {
    if (selectedFrameIds.size === 0) return;
    onBlink?.([...selectedFrameIds]);
  }, [selectedFrameIds, onBlink]);

  const handleBlackholeSelected = useCallback(async () => {
    if (selectedFrameIds.size === 0) return;

    const fileIds = allFrames
      .filter(f => selectedFrameIds.has(f.frame_id))
      .map(f => f.file_id);

    if (fileIds.length === 0) return;

    setBlackholing(true);
    try {
      for (const fileId of fileIds) {
        await api.invoke('move_to_black_hole', { fileId, fromWhere: 'frame_set_detail' });
      }
      setSelectedFrameIds(new Set());
    } catch (err) {
      console.error('Failed to blackhole selected frames:', err);
    } finally {
      setBlackholing(false);
    }
  }, [selectedFrameIds, allFrames]);

  const handleExportCsv = useCallback(() => {
    const headers = ['Filename', 'Date/Time', 'Camera', 'Filter', 'Exposure', 'Stars', 'FWHM (px)', 'Eccentricity', 'SNR', 'Frame SNR (dB)', 'PSF Signal (ADU)', 'SNR Weight', 'Trail R\u00B2'];
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
  const allAnalyzed = analyzedCount >= totalLightFrames;
  const hasSelection = selectedFrameIds.size > 0;

  return (
    <div className="flex flex-col h-full">
      {/* Progress Bar */}
      {analyzing && analysisProgress && (
        <div className="mb-2 flex-shrink-0">
          <div className="flex items-center justify-between text-xs text-content-muted mb-1">
            <span>{analysisProgress.current_file}</span>
            <div className="flex items-center gap-2">
              <span>{analysisProgress.current} / {analysisProgress.total} ({analysisProgress.percent.toFixed(0)}%)</span>
              <button
                onClick={() => cancelAnalysis(frameSetId)}
                className="text-content-muted hover:text-error transition-colors"
                title="Cancel analysis"
              >
                <X size={14} />
              </button>
            </div>
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
        {/* Left panel — Tree with view toggle */}
        <div className="w-80 flex-shrink-0 flex flex-col">
          {/* View Mode Toggle */}
          <div className="flex mb-1 rounded-lg border border-border/50 overflow-hidden">
            <button
              onClick={() => handleViewModeChange('by-night')}
              className={`flex-1 flex items-center justify-center gap-1.5 px-2 py-1 text-xs font-medium transition-colors ${
                viewMode === 'by-night'
                  ? 'bg-accent/20 text-accent'
                  : 'bg-surface-elevated/50 text-content-muted hover:text-content-secondary'
              }`}
            >
              <Calendar size={12} />
              By Night
            </button>
            <button
              onClick={() => handleViewModeChange('by-camera')}
              className={`flex-1 flex items-center justify-center gap-1.5 px-2 py-1 text-xs font-medium transition-colors ${
                viewMode === 'by-camera'
                  ? 'bg-accent/20 text-accent'
                  : 'bg-surface-elevated/50 text-content-muted hover:text-content-secondary'
              }`}
            >
              <Camera size={12} />
              By Camera
            </button>
          </div>

          {/* Tree */}
          {(() => {
            const treeFooter = checkedKeys.size > 0 && (onSplit || onCreateCustomSet) ? (
              <>
                <div className="flex items-center gap-2 flex-wrap">
                  {onSplit && (
                    <button
                      onClick={handleSplit}
                      className="inline-flex items-center gap-1.5 px-3 py-1.5 bg-accent hover:bg-accent-hover text-white text-sm rounded transition-colors focus:outline-none focus-visible:ring-1 focus-visible:ring-accent"
                    >
                      <Scissors size={14} aria-hidden="true" />
                      Split
                    </button>
                  )}
                  {onCreateCustomSet && (
                    <button
                      onClick={handleCreateCustomSet}
                      className="inline-flex items-center gap-1.5 px-3 py-1.5 bg-success hover:brightness-90 text-white text-sm rounded transition-colors focus:outline-none focus-visible:ring-1 focus-visible:ring-success"
                    >
                      <Plus size={14} aria-hidden="true" />
                      Create Set
                    </button>
                  )}
                </div>
              </>
            ) : undefined;

            const label = checkedKeys.size > 0
              ? `${checkedKeys.size} group${checkedKeys.size !== 1 ? 's' : ''} · ${checkedFrameCount} frame${checkedFrameCount !== 1 ? 's' : ''}`
              : undefined;

            return viewMode === 'by-night' ? (
              <CameraFilterTree
                nodes={dateTree.nodes}
                checkedKeys={checkedKeys}
                onCheckedChange={handleCheckedChange}
                className="flex-1 min-h-0"
                checkedLabel={label}
                footer={treeFooter}
              />
            ) : (
              <MergedCameraFilterTree
                nodes={mergedTree.nodes}
                checkedKeys={checkedKeys}
                onCheckedChange={handleCheckedChange}
                className="flex-1 min-h-0"
                checkedLabel={label}
                footer={treeFooter}
              />
            );
          })()}
        </div>

        {/* Right panel */}
        <div className="flex-1 min-w-0 flex flex-col gap-2">
          {/* Unified toolbar */}
          <div className="flex items-start gap-2 flex-wrap flex-shrink-0">
            {/* Analyze button */}
            {!allAnalyzed ? (
              <div className="flex flex-col items-center gap-0.5 flex-shrink-0">
                <button
                  onClick={() => handleAnalyzeAll(false)}
                  disabled={analyzing}
                  className="h-7 inline-flex items-center gap-1.5 px-3 text-xs font-medium bg-accent hover:bg-accent-hover disabled:bg-surface-hover disabled:cursor-not-allowed text-white rounded-lg transition-colors"
                >
                  <BarChart3 size={12} />
                  {analyzing ? 'Analyzing...' : 'Analyze'}
                </button>
                <span className="text-[10px] text-content-muted leading-tight">
                  {analyzedCount > 0 ? `${analyzedCount}/${totalLightFrames}` : 'analyze'}
                </span>
              </div>
            ) : (
              <div className="flex flex-col items-center gap-0.5 flex-shrink-0">
                <button
                  onClick={() => handleAnalyzeAll(true)}
                  disabled={analyzing}
                  className="w-10 h-7 inline-flex items-center justify-center text-xs font-medium bg-success/20 hover:bg-success/30 text-success rounded-lg border border-success/30 transition-colors disabled:opacity-30 disabled:cursor-default"
                  title="Re-analyze all frames"
                >
                  <BarChart3 size={12} />
                </button>
                <span className="text-[10px] text-content-muted leading-tight">re-analyze</span>
              </div>
            )}

            <span className="text-border h-7 flex items-center">|</span>

            {/* Units toggle */}
            {plateScale && (
              <div className="flex flex-col items-center gap-0.5 flex-shrink-0">
                <button
                  onClick={() => setUseArcsec(prev => !prev)}
                  className={`w-10 h-7 text-xs font-medium rounded-lg border transition-colors ${
                    useArcsec
                      ? 'bg-accent/20 border-accent text-accent'
                      : 'bg-surface-hover border-border text-content-secondary hover:text-content'
                  }`}
                  title="Toggle units between pixels and arcseconds"
                >
                  {useArcsec ? '"' : 'px'}
                </button>
                <span className="text-[10px] text-content-muted leading-tight">units</span>
              </div>
            )}

            {/* Selection actions — always visible, disabled when nothing selected */}
            <div className="flex flex-col items-center gap-0.5 flex-shrink-0">
              <div className="h-7 inline-flex items-center gap-1">
                <span className={`text-xs font-medium ${hasSelection ? 'text-content' : 'text-content-muted/30'}`}>
                  {hasSelection ? selectedFrameIds.size : 0}
                </span>
                <button
                  onClick={() => setSelectedFrameIds(new Set())}
                  disabled={!hasSelection}
                  className="inline-flex items-center p-1 text-content-muted hover:text-content rounded transition-colors disabled:opacity-30 disabled:cursor-default"
                  title="Clear selection"
                >
                  <RotateCcw size={10} />
                </button>
              </div>
              <span className="text-[10px] text-content-muted leading-tight">selected</span>
            </div>
            <div className="flex flex-col items-center gap-0.5 flex-shrink-0">
              <button
                onClick={handleBlinkSelected}
                disabled={!hasSelection}
                className="w-10 h-7 inline-flex items-center justify-center text-xs font-medium bg-cyan-600 hover:bg-cyan-700 text-white rounded-lg transition-colors disabled:opacity-30 disabled:cursor-default"
                title="Blink selected frames"
              >
                <Play size={12} />
              </button>
              <span className="text-[10px] text-content-muted leading-tight">blink</span>
            </div>
            <div className="flex flex-col items-center gap-0.5 flex-shrink-0">
              <button
                onClick={handleBlackholeSelected}
                disabled={!hasSelection || blackholing}
                className="w-10 h-7 inline-flex items-center justify-center text-xs font-medium bg-error hover:brightness-90 text-white rounded-lg transition-colors disabled:opacity-30 disabled:cursor-default"
                title="Blackhole selected frames"
              >
                <Trash2 size={12} />
              </button>
              <span className="text-[10px] text-content-muted leading-tight">blackhole</span>
            </div>

            <span className="text-border h-7 flex items-center">|</span>

            {/* Rejection thresholds — inline */}
            <RejectionThresholdBar
              thresholds={thresholds}
              onChange={setThresholds}
              onClear={handleClearThresholds}
              onLoadDefaults={handleLoadDefaults}
              hasDefaults={defaultThresholds !== null}
              useArcsec={useArcsec && !!plateScale}
            />

            <span className="text-border h-7 flex items-center">|</span>

            {/* Export / Charts */}
            <div className="flex flex-col items-center gap-0.5 flex-shrink-0">
              <button
                onClick={handleExportCsv}
                disabled={analysisData.size === 0}
                className="w-10 h-7 inline-flex items-center justify-center text-xs font-medium bg-surface-hover hover:bg-surface-elevated text-content-secondary rounded-lg border border-border transition-colors disabled:opacity-30 disabled:cursor-default"
                title="Export CSV"
              >
                <Download size={12} />
              </button>
              <span className="text-[10px] text-content-muted leading-tight">csv</span>
            </div>
            <div className="flex flex-col items-center gap-0.5 flex-shrink-0">
              <button
                onClick={() => setChartsOpen(true)}
                disabled={analysisData.size === 0}
                className="w-10 h-7 inline-flex items-center justify-center text-xs font-medium bg-surface-hover hover:bg-surface-elevated text-content-secondary rounded-lg border border-border transition-colors disabled:opacity-30 disabled:cursor-default"
                title="Charts"
              >
                <LineChart size={12} />
              </button>
              <span className="text-[10px] text-content-muted leading-tight">charts</span>
            </div>
          </div>

          {csvExportedMsg && (
            <div className="inline-flex items-center gap-1 text-xs text-green-400 flex-shrink-0">
              <Check size={14} />
              {csvExportedMsg}
            </div>
          )}

          <div className="flex-1 min-h-0 overflow-y-auto border border-border rounded-xl">
            <LightsAnalysisTable
              frames={displayedFrames}
              selectedFrameIds={selectedFrameIds}
              onSelectionChange={setSelectedFrameIds}
              onBlackhole={handleBlackhole}
              analysisData={analysisData}
              rejectedFrameIds={rejectedFrameIds}
              plateScale={useArcsec ? plateScale : null}
            />
          </div>

          <BlackholedFramesSection frames={blackholedFrames} analysisData={analysisData} onBlink={onBlink} />
        </div>
      </div>

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
