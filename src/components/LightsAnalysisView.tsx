import { useState, useEffect, useMemo, useCallback } from 'react';
import { Play, Trash2, BarChart3 } from 'lucide-react';
import { api } from '../api';
import type {
  CalibrationHierarchyView as CalibrationHierarchyViewData,
  FrameAnalysis,
  AnalyzeFrameSetResult,
  AnalysisProgressEvent,
} from '../types/models';
import { CameraFilterTree, CameraFilterNode } from './calibration/CameraFilterTree';
import { LightsAnalysisTable, EnrichedLightFrame } from './calibration/LightsAnalysisTable';
import { RejectionThresholdBar, RejectionThresholds } from './calibration/RejectionThresholdBar';

interface LightsAnalysisViewProps {
  hierarchy: CalibrationHierarchyViewData;
  frameSetId: number;
  onRefresh?: () => void;
  onBlink?: (frameIds: number[]) => void;
}

interface CameraFilterData {
  nodes: CameraFilterNode[];
  framesByKey: Map<string, EnrichedLightFrame[]>;
  allFrames: EnrichedLightFrame[];
}

function buildCameraFilterTree(hierarchy: CalibrationHierarchyViewData): CameraFilterData {
  const cameraMap = new Map<string, Map<string, { label: string; frames: EnrichedLightFrame[] }>>();
  const allFrames: EnrichedLightFrame[] = [];

  for (const dateGroup of hierarchy.date_groups) {
    for (const cameraGroup of dateGroup.camera_groups) {
      const camera = cameraGroup.instrume;

      if (!cameraMap.has(camera)) {
        cameraMap.set(camera, new Map());
      }
      const filterMap = cameraMap.get(camera)!;

      for (const filterGroup of cameraGroup.filter_groups) {
        const filterName = filterGroup.filter ?? 'No Filter';
        const exptime = filterGroup.exptime;
        const key = `${camera}::${filterName}::${exptime ?? 'any'}`;
        const label = exptime != null ? `${filterName} (${exptime}s)` : filterName;

        if (!filterMap.has(key)) {
          filterMap.set(key, { label, frames: [] });
        }

        const enriched: EnrichedLightFrame[] = filterGroup.light_frames.map(f => ({
          ...f,
          camera,
          filter: filterGroup.filter,
        }));

        filterMap.get(key)!.frames.push(...enriched);
        allFrames.push(...enriched);
      }
    }
  }

  const nodes: CameraFilterNode[] = [];
  const framesByKey = new Map<string, EnrichedLightFrame[]>();

  for (const [camera, filterMap] of cameraMap) {
    const filters: CameraFilterNode['filters'] = [];

    for (const [key, data] of filterMap) {
      filters.push({
        key,
        label: data.label,
        frameCount: data.frames.length,
      });
      framesByKey.set(key, data.frames);
    }

    filters.sort((a, b) => a.label.localeCompare(b.label));

    nodes.push({
      camera,
      totalFrameCount: filters.reduce((sum, f) => sum + f.frameCount, 0),
      filters,
    });
  }

  nodes.sort((a, b) => a.camera.localeCompare(b.camera));

  return { nodes, framesByKey, allFrames };
}

export function LightsAnalysisView({ hierarchy, frameSetId, onRefresh, onBlink }: LightsAnalysisViewProps) {
  // Tree checkbox state — which filter groups are checked (for filtering the table)
  const [checkedKeys, setCheckedKeys] = useState<Set<string>>(new Set());
  // Table row selection — which individual frames are selected (for mass actions)
  const [selectedFrameIds, setSelectedFrameIds] = useState<Set<number>>(new Set());
  const [blackholedFileIds, setBlackholedFileIds] = useState<Set<number>>(new Set());
  const [blackholing, setBlackholing] = useState(false);
  const [thresholds, setThresholds] = useState<RejectionThresholds>({
    fwhm: '',
    eccentricity: '',
    snr_weight: '',
    stars: '',
    score: '',
  });

  // Analysis state
  const [analysisData, setAnalysisData] = useState<Map<number, FrameAnalysis>>(new Map());
  const [analyzing, setAnalyzing] = useState(false);
  const [analysisProgress, setAnalysisProgress] = useState<AnalysisProgressEvent | null>(null);

  const { nodes, framesByKey, allFrames } = useMemo(
    () => buildCameraFilterTree(hierarchy),
    [hierarchy]
  );

  // Load existing analysis data on mount
  useEffect(() => {
    loadAnalysisData();
  }, [frameSetId]);

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
    const fwhmThreshold = parseFloat(thresholds.fwhm);
    const eccThreshold = parseFloat(thresholds.eccentricity);
    const snrThreshold = parseFloat(thresholds.snr_weight);
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
      // SNR Weight < threshold = rejected (worse)
      if (!isNaN(snrThreshold) && a.snr_weight < snrThreshold) {
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
  }, [thresholds, displayedFrames, analysisData]);

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
        <CameraFilterTree
          nodes={nodes}
          checkedKeys={checkedKeys}
          onCheckedChange={handleCheckedChange}
          className="w-80 flex-shrink-0"
        />

        {/* Right panel — Threshold bar + Table */}
        <div className="flex-1 min-w-0 flex flex-col gap-3">
          <RejectionThresholdBar
            thresholds={thresholds}
            onChange={setThresholds}
          />

          <div className="flex-1 min-h-0 overflow-y-auto">
            <LightsAnalysisTable
              frames={displayedFrames}
              blackholedFileIds={blackholedFileIds}
              selectedFrameIds={selectedFrameIds}
              onSelectionChange={setSelectedFrameIds}
              onBlackhole={handleBlackhole}
              analysisData={analysisData}
              rejectedFrameIds={rejectedFrameIds}
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
    </div>
  );
}
