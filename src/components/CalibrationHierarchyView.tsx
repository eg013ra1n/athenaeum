import { useState, useMemo, useCallback } from 'react';
import { api } from '../api';
import { Scissors, Plus, Play, Settings } from 'lucide-react';
import type {
  CalibrationHierarchyView as CalibrationHierarchyViewData,
} from '../types/models';
import { ManualCalibrationModal } from './ManualCalibrationModal';
import { CameraFilterTree } from './calibration/CameraFilterTree';
import { CalibrationLightsTable } from './calibration/CalibrationLightsTable';
import { CalibrationCardView } from './calibration/CalibrationCardView';
import { CalibrationGroupModal } from './calibration/CalibrationGroupModal';
import type { FlatGroupData, DarkOnlyGroupData } from './calibration/CalibrationGroupCard';
import type { EnrichedLightFrame } from './calibration/LightsAnalysisTable';
import { buildCameraFilterTree } from './calibration/utils';
import { BlackholedFramesSection } from './calibration/BlackholedFramesSection';

interface CalibrationHierarchyViewProps {
  data: CalibrationHierarchyViewData;
  blackholedFileIds: Set<number>;
  useBiasForDarkOptimization?: boolean;
  onRefresh?: () => void;
  onBlink?: (frameIds: number[]) => void;
  onBlinkSelected?: (frameIds: number[]) => void;
  onSplit?: (selectedFilterKeys: Set<string>) => void;
  onCreateCustomSet?: (selectedFilterKeys: Set<string>) => void;
}

interface ModalState {
  type: 'flat' | 'dark';
  data: FlatGroupData | DarkOnlyGroupData;
}

export function CalibrationHierarchyView({
  data,
  blackholedFileIds,
  useBiasForDarkOptimization = false,
  onRefresh,
  onBlink: _onBlink,
  onBlinkSelected,
  onSplit,
  onCreateCustomSet,
}: CalibrationHierarchyViewProps) {
  // onBlink is aliased to _onBlink — in the new design, onBlinkSelected handles all blink actions.
  // The prop is kept for interface compatibility with FrameSetDetail which passes it.
  void _onBlink;
  const isPreCalibration = data.calibrated_frames === 0;

  // Build camera→filter tree from hierarchy data
  const { nodes, framesByKey, allFrames: allFramesRaw } = useMemo(
    () => buildCameraFilterTree(data),
    [data]
  );

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
  const [selectedFrameIds, setSelectedFrameIds] = useState<Set<number>>(new Set());

  // Post-cal: group modal state
  const [modalData, setModalData] = useState<ModalState | null>(null);

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
    return frames;
  }, [checkedKeys, allFrames, framesByKey]);

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

  // Clear table selection when tree filter changes
  const handleCheckedChange = useCallback((keys: Set<string>) => {
    setCheckedKeys(keys);
    setSelectedFrameIds(new Set());
  }, []);

  // Blink selected frames (pre-cal table selection)
  const handleBlinkSelected = useCallback(() => {
    if (!onBlinkSelected) return;

    if (selectedFrameIds.size > 0) {
      onBlinkSelected([...selectedFrameIds]);
    } else if (checkedKeys.size > 0) {
      onBlinkSelected(displayedFrames.map(f => f.frame_id));
    }
  }, [onBlinkSelected, selectedFrameIds, checkedKeys, displayedFrames]);

  // Split with checked filter keys
  const handleSplit = useCallback(() => {
    if (onSplit && checkedKeys.size > 0) {
      onSplit(checkedKeys);
    }
  }, [onSplit, checkedKeys]);

  // Create custom set with checked filter keys
  const handleCreateCustomSet = useCallback(() => {
    if (onCreateCustomSet && checkedKeys.size > 0) {
      onCreateCustomSet(checkedKeys);
    }
  }, [onCreateCustomSet, checkedKeys]);

  // Open card group detail modal
  const handleOpenGroup = useCallback((type: 'flat' | 'dark', groupData: FlatGroupData | DarkOnlyGroupData) => {
    setModalData({ type, data: groupData });
  }, []);

  // Open manual calibration modal for specific frame IDs
  const openManualCalibrationForFrameIds = useCallback((frameIds: number[]) => {
    setManualModalFrameIds(frameIds);
    setManualModalFilterDisplay('Selected frames');
    setManualModalCurrentFlat(null);
    setManualModalCurrentDark(null);
    setManualModalCurrentBias(null);
    setManualModalOpen(true);
  }, []);

  // Open manual calibration modal for pre-cal selected frames
  const handleManualCalibrationPreCal = useCallback(() => {
    if (selectedFrameIds.size === 0) return;
    openManualCalibrationForFrameIds([...selectedFrameIds]);
  }, [selectedFrameIds, openManualCalibrationForFrameIds]);

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

  // Determine action bar visibility and counts
  const showPreCalActionBar = isPreCalibration && selectedFrameIds.size > 0;
  const showFilterActionBar = !isPreCalibration && checkedKeys.size > 0 && (onBlinkSelected || onSplit || onCreateCustomSet);

  return (
    <div className="flex flex-col h-full">
      {/* Main Content */}
      {data.date_groups.length > 0 ? (
        <div className="flex flex-1 min-h-0 gap-4">
          {/* Left Panel — CameraFilterTree */}
          <CameraFilterTree
            nodes={nodes}
            checkedKeys={checkedKeys}
            onCheckedChange={handleCheckedChange}
            className="w-80 flex-shrink-0"
          />

          {/* Right Panel */}
          <div className="flex-1 min-w-0 flex flex-col">
            {isPreCalibration ? (
              /* Pre-calibration: flat sortable table */
              <div className="flex-1 min-h-0 overflow-y-auto border border-border rounded-xl">
                <CalibrationLightsTable
                  frames={displayedFrames}
                  selectedFrameIds={selectedFrameIds}
                  onSelectionChange={setSelectedFrameIds}
                />
              </div>
            ) : (
              /* Post-calibration: card view */
              <CalibrationCardView
                data={data}
                allFrames={allFrames}
                visibleFrameIds={visibleFrameIds}
                onOpenGroup={handleOpenGroup}
                onManualCalibration={openManualCalibrationForFrameIds}
              />
            )}
            <BlackholedFramesSection frames={blackholedFrames} />
          </div>
        </div>
      ) : (
        /* Empty State */
        <div className="flex-1 flex items-center justify-center">
          <div className="text-center py-16 px-8 bg-surface-elevated rounded-xl border border-border">
            <p className="text-lg text-content-muted">No frames found in this frame set.</p>
          </div>
        </div>
      )}

      {/* Pre-cal action bar: when table rows are selected */}
      {showPreCalActionBar && (
        <div className="mt-3 bg-surface-elevated/80 rounded-lg p-3 border border-border/50">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              {onBlinkSelected && (
                <>
                  <button
                    onClick={() => onBlinkSelected([...selectedFrameIds])}
                    className="inline-flex items-center gap-1.5 px-3 py-1.5 bg-cyan-600 hover:bg-cyan-700 text-white text-sm rounded transition-colors focus:outline-none focus-visible:ring-1 focus-visible:ring-cyan-500"
                  >
                    <Play size={14} aria-hidden="true" />
                    Blink ({selectedFrameIds.size})
                  </button>
                  <span className="text-content-muted">|</span>
                </>
              )}
              <button
                onClick={handleManualCalibrationPreCal}
                className="inline-flex items-center gap-1.5 px-3 py-1.5 bg-accent hover:bg-accent-hover text-white text-sm rounded transition-colors focus:outline-none focus-visible:ring-1 focus-visible:ring-accent"
              >
                <Settings size={14} aria-hidden="true" />
                Assign Calibration
              </button>
            </div>
            <div className="flex items-center gap-3">
              <div className="text-sm text-content-secondary">
                <span className="font-medium text-content">{selectedFrameIds.size}</span>{' '}
                frame{selectedFrameIds.size !== 1 ? 's' : ''}
              </div>
              <button
                onClick={() => setSelectedFrameIds(new Set())}
                className="px-3 py-1.5 text-content-muted hover:text-content text-sm rounded transition-colors focus:outline-none focus-visible:ring-1 focus-visible:ring-border"
              >
                Clear
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Post-cal action bar: when CameraFilterTree groups are checked */}
      {showFilterActionBar && (
        <div className="mt-3 bg-surface-elevated/80 rounded-lg p-3 border border-border/50">
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              {onBlinkSelected && (
                <>
                  <button
                    onClick={handleBlinkSelected}
                    className="inline-flex items-center gap-1.5 px-3 py-1.5 bg-cyan-600 hover:bg-cyan-700 text-white text-sm rounded transition-colors focus:outline-none focus-visible:ring-1 focus-visible:ring-cyan-500"
                  >
                    <Play size={14} aria-hidden="true" />
                    Blink Selected ({checkedFrameCount})
                  </button>
                  {(onSplit || onCreateCustomSet) && (
                    <span className="text-content-muted">|</span>
                  )}
                </>
              )}
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
            <div className="flex items-center gap-3">
              <div className="text-sm text-content-secondary">
                <span className="font-medium text-content">{checkedKeys.size}</span>{' '}
                group{checkedKeys.size !== 1 ? 's' : ''}
                <span className="text-content-muted ml-1">
                  ({checkedFrameCount} frame{checkedFrameCount !== 1 ? 's' : ''})
                </span>
              </div>
              <button
                onClick={() => setCheckedKeys(new Set())}
                className="px-3 py-1.5 text-content-muted hover:text-content text-sm rounded transition-colors focus:outline-none focus-visible:ring-1 focus-visible:ring-border"
              >
                Clear
              </button>
            </div>
          </div>
        </div>
      )}

      {/* Calibration Group Modal */}
      {modalData && (
        <CalibrationGroupModal
          type={modalData.type}
          data={modalData.data}
          allLightFrames={allFrames}
          onClose={() => setModalData(null)}
          onRefresh={() => {
            setModalData(null);
            onRefresh?.();
          }}
          onManualCalibration={openManualCalibrationForFrameIds}
        />
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
