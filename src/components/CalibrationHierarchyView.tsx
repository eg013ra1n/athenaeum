import { useState, useEffect, useMemo, useCallback } from 'react';
import { api } from '../api';
import { Scissors, Plus, Calendar, Camera, X, ScanSearch } from 'lucide-react';
import type {
  CalibrationHierarchyView as CalibrationHierarchyViewData,
  FrameAnalysis,
} from '../types/models';
import { ManualCalibrationModal } from './ManualCalibrationModal';
import { CameraFilterTree } from './calibration/CameraFilterTree';
import { MergedCameraFilterTree } from './calibration/MergedCameraFilterTree';
import type { EnrichedLightFrame } from './calibration/LightsAnalysisTable';
import { CalibrationTableView } from './calibration/CalibrationTableView';
import { buildCameraFilterTree, buildMergedCameraFilterTree } from './calibration/utils';
import { CalibrationFinderButton } from './CalibrationFinderButton';
import { BlackholedFramesSection } from './calibration/BlackholedFramesSection';

interface CalibrationHierarchyViewProps {
  data: CalibrationHierarchyViewData;
  blackholedFileIds: Set<number>;
  useBiasForDarkOptimization?: boolean;
  /** Stacked SNR per filter key, passed through to the tree */
  filterSnrMap?: Map<string, number>;
  /** Analysis data keyed by frame_id, for computing metrics in table rows */
  analysisData?: Map<number, FrameAnalysis>;
  /** Frame set ID for calibration finder */
  frameSetId?: number;
  /** Frame set name for calibration finder */
  frameSetName?: string;
  /** Callback when calibration matching completes */
  onCalibrationComplete?: () => void;
  onRefresh?: () => void;
  onBlink?: (frameIds: number[]) => void;
  onSplit?: (selectedFilterKeys: Set<string>) => void;
  onCreateCustomSet?: (selectedFilterKeys: Set<string>) => void;
}

export function CalibrationHierarchyView({
  data,
  blackholedFileIds,
  useBiasForDarkOptimization = false,
  filterSnrMap,
  analysisData,
  frameSetId,
  frameSetName,
  onCalibrationComplete,
  onRefresh,
  onBlink,
  onSplit,
  onCreateCustomSet,
}: CalibrationHierarchyViewProps) {
  // Re-assign mode toggle
  const [reassignMode, setReassignMode] = useState(false);

  // View mode: by-night (date→camera→filter) or by-camera (camera→filter)
  const [viewMode, setViewMode] = useState<'by-night' | 'by-camera'>('by-night');

  // Load persisted view mode on mount
  useEffect(() => {
    (async () => {
      try {
        const saved = await api.invoke<string>('get_setting', { key: 'ui.tree_view_mode', defaultValue: 'by-night' });
        if (saved === 'by-camera') setViewMode('by-camera');
      } catch { /* use default */ }
    })();
  }, []);

  const handleViewModeChange = useCallback((mode: 'by-night' | 'by-camera') => {
    setViewMode(mode);
    setCheckedKeys(new Set());
    api.invoke('set_setting', { key: 'ui.tree_view_mode', value: mode }).catch(() => {});
  }, []);

  // Build both tree structures
  const dateTree = useMemo(() => buildCameraFilterTree(data), [data]);
  const mergedTree = useMemo(() => buildMergedCameraFilterTree(data), [data]);

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

  // CameraFilterTree checkbox state (for filtering)
  const [checkedKeys, setCheckedKeys] = useState<Set<string>>(new Set());

  // Pre-cal: table row selection
  // Manual calibration modal state
  const [manualModalOpen, setManualModalOpen] = useState(false);
  const [manualModalFrameIds, setManualModalFrameIds] = useState<number[]>([]);
  const [manualModalFilterDisplay, setManualModalFilterDisplay] = useState('');
  const [manualModalCurrentFlat, setManualModalCurrentFlat] = useState<number | null>(null);
  const [manualModalCurrentDark, setManualModalCurrentDark] = useState<number | null>(null);
  const [manualModalCurrentBias, setManualModalCurrentBias] = useState<number | null>(null);

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

  // Visible frame IDs for card view filtering
  const visibleFrameIds = useMemo(() => {
    if (checkedKeys.size === 0) return undefined;
    return new Set(displayedFrames.map(f => f.frame_id));
  }, [checkedKeys, displayedFrames]);

  // Count of selected/checked frames for action bar
  const checkedFrameCount = useMemo(() => {
    if (checkedKeys.size === 0) return 0;
    return displayedFrames.length;
  }, [checkedKeys, displayedFrames]);

  // Clear tree filter
  const handleCheckedChange = useCallback((keys: Set<string>) => {
    setCheckedKeys(keys);
  }, []);

  // Split with checked filter keys
  const handleSplit = useCallback(() => {
    if (onSplit && checkedKeys.size > 0) onSplit(checkedKeys);
  }, [onSplit, checkedKeys]);

  // Create custom set with checked filter keys
  const handleCreateCustomSet = useCallback(() => {
    if (onCreateCustomSet && checkedKeys.size > 0) onCreateCustomSet(checkedKeys);
  }, [onCreateCustomSet, checkedKeys]);

  // Open manual calibration modal for specific frame IDs
  const openManualCalibrationForFrameIds = useCallback((frameIds: number[]) => {
    setManualModalFrameIds(frameIds);
    setManualModalFilterDisplay('Selected frames');
    setManualModalCurrentFlat(null);
    setManualModalCurrentDark(null);
    setManualModalCurrentBias(null);
    setManualModalOpen(true);
  }, []);

  // Handle manual calibration apply
  const handleManualCalibrationApply = useCallback(
    async (flatSetId: number | null, darkSetId: number | null, biasSetId: number | null) => {
      try {
        if (flatSetId !== null && flatSetId !== manualModalCurrentFlat) {
          await api.invoke('manual_assign_calibration', {
            frameIds: manualModalFrameIds,
            calibrationSetId: flatSetId,
            calibrationType: 'Flat',
          });
          setManualModalCurrentFlat(flatSetId);
        }
        if (darkSetId !== null && darkSetId !== manualModalCurrentDark) {
          await api.invoke('manual_assign_calibration', {
            frameIds: manualModalFrameIds,
            calibrationSetId: darkSetId,
            calibrationType: 'Dark',
          });
          setManualModalCurrentDark(darkSetId);
        }
        if (biasSetId !== null && biasSetId !== manualModalCurrentBias) {
          await api.invoke('manual_assign_calibration', {
            frameIds: manualModalFrameIds,
            calibrationSetId: biasSetId,
            calibrationType: 'Bias',
          });
          setManualModalCurrentBias(biasSetId);
        }

        setManualModalOpen(false);

        if (onRefresh) {
          onRefresh();
        }
      } catch (error) {
        console.error('Failed to apply manual calibration:', error);
      }
    },
    [manualModalFrameIds, manualModalCurrentFlat, manualModalCurrentDark, manualModalCurrentBias, onRefresh]
  );

  const showFilterActionBar = checkedKeys.size > 0 && (onSplit || onCreateCustomSet);

  return (
    <div className="flex flex-col h-full">
      {/* Main Content */}
      {data.date_groups.length > 0 ? (
        <div className="flex flex-1 min-h-0 gap-4">
          {/* Left Panel — Find Calibration + Tree */}
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
                  filterSnrMap={filterSnrMap}
                  footer={treeFooter}
                />
              ) : (
                <MergedCameraFilterTree
                  nodes={mergedTree.nodes}
                  checkedKeys={checkedKeys}
                  onCheckedChange={handleCheckedChange}
                  className="flex-1 min-h-0"
                  checkedLabel={label}
                  filterSnrMap={filterSnrMap}
                  footer={treeFooter}
                />
              );
            })()}
          </div>

          {/* Right Panel */}
          <div className="flex-1 min-w-0 flex flex-col gap-2">
            {/* Toolbar */}
            {frameSetId && frameSetName && (
              <div className="flex items-start gap-2 flex-wrap flex-shrink-0">
                <div className="flex flex-col items-center gap-0.5 flex-shrink-0">
                  <CalibrationFinderButton
                    frameSetId={frameSetId}
                    frameSetName={frameSetName}
                    onComplete={() => { onCalibrationComplete?.(); onRefresh?.(); }}
                  />
                  <span className="text-[10px] text-content-muted leading-tight">
                    {data.calibrated_frames}/{data.total_frames}
                  </span>
                </div>
                <button
                  onClick={() => setReassignMode(v => !v)}
                  className={`h-7 inline-flex items-center gap-1.5 px-3 text-xs font-medium rounded-lg transition-colors ${
                    reassignMode
                      ? 'bg-accent text-white hover:brightness-90'
                      : 'bg-surface-elevated text-content-secondary hover:text-content hover:bg-surface-hover border border-border/50'
                  }`}
                  title={reassignMode ? 'Exit Re-assign mode' : 'Enter Re-assign mode — select individual frames to reassign calibration'}
                >
                  <ScanSearch size={12} />
                  Re-assign
                </button>
              </div>
            )}

            <CalibrationTableView
              data={data}
              allFrames={allFrames}
              visibleFrameIds={visibleFrameIds}
              analysisData={analysisData}
              onManualCalibration={openManualCalibrationForFrameIds}
              onBlink={onBlink}
              reassignMode={reassignMode}
            />
            <BlackholedFramesSection frames={blackholedFrames} />

            {/* Action bar: when tree groups are checked */}
            {showFilterActionBar && (
              <div className="mt-1 bg-surface-elevated/80 rounded-lg px-2 py-1.5 border border-border/50">
                <div className="flex items-center gap-2">
                  <button
                    onClick={() => setCheckedKeys(new Set())}
                    className="inline-flex items-center gap-1.5 px-3 py-1.5 bg-surface-hover hover:bg-surface-hover text-content-secondary hover:text-content text-sm rounded transition-colors"
                  >
                    <X size={14} />
                    Clear Selection
                  </button>
                  <div className="text-sm text-content-secondary">
                    <span className="font-medium text-content">{checkedKeys.size}</span>{' '}
                    group{checkedKeys.size !== 1 ? 's' : ''}
                    <span className="text-content-muted ml-1">({checkedFrameCount} frame{checkedFrameCount !== 1 ? 's' : ''})</span>
                  </div>
                </div>
              </div>
            )}
          </div>
        </div>
      ) : (
        <div className="flex-1 flex items-center justify-center">
          <div className="text-center py-16 px-8 bg-surface-elevated rounded-xl border border-border">
            <p className="text-lg text-content-muted">No frames found in this frame set.</p>
          </div>
        </div>
      )}

      {/* Manual Calibration Modal */}
      <ManualCalibrationModal
        isOpen={manualModalOpen}
        frameIds={manualModalFrameIds}
        filterDisplay={manualModalFilterDisplay}
        currentFlatSetId={manualModalCurrentFlat}
        currentDarkSetId={manualModalCurrentDark}
        currentBiasSetId={manualModalCurrentBias}
        useBiasForDarkOptimization={useBiasForDarkOptimization}
        onApply={handleManualCalibrationApply}
        onClose={() => setManualModalOpen(false)}
      />
    </div>
  );
}
