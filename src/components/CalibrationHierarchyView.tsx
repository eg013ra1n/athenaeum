import { useState, useMemo, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Scissors, Plus } from 'lucide-react';
import type {
  CalibrationHierarchyView as CalibrationHierarchyViewData,
  CalibrationFilterGroup,
  LightFrameWithCalibration,
} from '../types/models';
import { ManualCalibrationModal } from './ManualCalibrationModal';
import {
  NavigationTree,
  DetailPanel,
  SelectedItem,
  AggregatedWarning,
} from './calibration';

interface CalibrationHierarchyViewProps {
  data: CalibrationHierarchyViewData;
  useBiasForDarkOptimization?: boolean;
  onRefresh?: () => void;
  /** Callback when Blink button is clicked with frame IDs */
  onBlink?: (frameIds: number[]) => void;
  /** Callback to open split dialog with selected filter keys */
  onSplit?: (selectedFilterKeys: Set<string>) => void;
  /** Callback to open create custom set dialog with selected filter keys */
  onCreateCustomSet?: (selectedFilterKeys: Set<string>) => void;
}

/**
 * Build a unique filter key that includes exptime to differentiate same filter with different exposures.
 */
function buildFilterKey(filterGroup: CalibrationFilterGroup): string {
  const baseFilter = filterGroup.filter ?? '__no_filter__';
  return filterGroup.exptime !== null
    ? `${baseFilter}:${filterGroup.exptime}`
    : baseFilter;
}

/**
 * Collect warnings from a filter group.
 */
function collectFilterGroupWarnings(filterGroup: CalibrationFilterGroup): AggregatedWarning[] {
  const warnings: AggregatedWarning[] = [];

  // Only Flat and Dark matter for lights - Bias is linked to Dark, not directly to lights
  const hasCalibration =
    filterGroup.flat_sets.length > 0 ||
    filterGroup.dark_sets.length > 0;

  if (!hasCalibration) {
    warnings.push({
      message: `No calibration linked (${filterGroup.frame_count} frame${filterGroup.frame_count !== 1 ? 's' : ''})`,
      type: 'missing_calibration',
      filter: filterGroup.filter ?? undefined,
    });
  }

  const addSetWarnings = (sets: typeof filterGroup.flat_sets, setType: string) => {
    for (const setWithCount of sets) {
      for (const warning of setWithCount.warnings) {
        warnings.push({
          message: `${setType}: ${warning.message}`,
          type: warning.warning_type as 'date' | 'temperature',
          filter: filterGroup.filter ?? undefined,
        });
      }
    }
  };

  addSetWarnings(filterGroup.flat_sets, 'Flat');
  addSetWarnings(filterGroup.dark_sets, 'Dark');

  return warnings;
}

/**
 * CalibrationHierarchyView with accessible master-detail layout.
 *
 * Features:
 * - Two-panel layout: navigation tree on left, detail panel on right
 * - Improved text contrast (minimum WCAG AA)
 * - Larger fonts (14px minimum for body text)
 * - 44px minimum touch targets
 * - Clear visual hierarchy
 */
export function CalibrationHierarchyView({
  data,
  useBiasForDarkOptimization = false,
  onRefresh,
  onBlink,
  onSplit,
  onCreateCustomSet,
}: CalibrationHierarchyViewProps) {
  // Selection state for master-detail navigation
  const [selectedItem, setSelectedItem] = useState<SelectedItem | null>(null);

  // Selection mode state (controls checkbox visibility)
  const [selectionMode, setSelectionMode] = useState(false);

  // Checkbox selection state for filter groups (format: "dateKey:cameraKey:filterKey")
  const [checkedFilters, setCheckedFilters] = useState<Set<string>>(new Set());

  // Toggle selection mode and clear selection when exiting
  const toggleSelectionMode = useCallback(() => {
    if (selectionMode) {
      setCheckedFilters(new Set());
    }
    setSelectionMode(prev => !prev);
  }, [selectionMode]);

  // Manual calibration modal state
  const [manualModalOpen, setManualModalOpen] = useState(false);
  const [manualModalFrameIds, setManualModalFrameIds] = useState<number[]>([]);
  const [manualModalFilterDisplay, setManualModalFilterDisplay] = useState('');
  const [manualModalCurrentFlat, setManualModalCurrentFlat] = useState<number | null>(null);
  const [manualModalCurrentDark, setManualModalCurrentDark] = useState<number | null>(null);
  const [manualModalCurrentBias, setManualModalCurrentBias] = useState<number | null>(null);

  // Build warning counts map for navigation tree badges
  const warningCounts = useMemo(() => {
    const counts = new Map<string, number>();

    for (const dateGroup of data.date_groups) {
      const dateKey = dateGroup.date;
      let dateWarnings = 0;

      for (const cameraGroup of dateGroup.camera_groups) {
        const cameraKey = `${dateKey}:${cameraGroup.instrume}`;
        let cameraWarnings = 0;

        for (const filterGroup of cameraGroup.filter_groups) {
          const filterKey = buildFilterKey(filterGroup);
          const fullKey = `${dateKey}:${cameraGroup.instrume}:${filterKey}`;
          const filterWarnings = collectFilterGroupWarnings(filterGroup).length;

          counts.set(fullKey, filterWarnings);
          cameraWarnings += filterWarnings;
        }

        counts.set(cameraKey, cameraWarnings);
        dateWarnings += cameraWarnings;
      }

      counts.set(dateKey, dateWarnings);
    }

    return counts;
  }, [data]);

  // Count total selected frames across all checked filter groups
  const selectedFrameCount = useMemo(() => {
    let count = 0;
    for (const dateGroup of data.date_groups) {
      for (const cameraGroup of dateGroup.camera_groups) {
        for (const filterGroup of cameraGroup.filter_groups) {
          const filterKey = buildFilterKey(filterGroup);
          const fullKey = `${dateGroup.date}:${cameraGroup.instrume}:${filterKey}`;
          if (checkedFilters.has(fullKey)) {
            count += filterGroup.light_frames.length;
          }
        }
      }
    }
    return count;
  }, [data, checkedFilters]);

  // Handle blink from DetailPanel - passes frame IDs to parent
  const handleBlink = useCallback((frames: LightFrameWithCalibration[]) => {
    if (onBlink) {
      const frameIds = frames.map(f => f.frame_id);
      onBlink(frameIds);
    }
  }, [onBlink]);

  // Handle split button click
  const handleSplit = useCallback(() => {
    if (onSplit && checkedFilters.size > 0) {
      onSplit(checkedFilters);
    }
  }, [onSplit, checkedFilters]);

  // Handle create custom set button click
  const handleCreateCustomSet = useCallback(() => {
    if (onCreateCustomSet && checkedFilters.size > 0) {
      onCreateCustomSet(checkedFilters);
    }
  }, [onCreateCustomSet, checkedFilters]);

  // Open manual calibration modal for a filter group
  const openManualCalibrationModal = useCallback((filterGroup: CalibrationFilterGroup) => {
    const frameIds = filterGroup.light_frames.map((f) => f.frame_id);
    const currentFlat = filterGroup.flat_sets.length > 0 ? filterGroup.flat_sets[0].set.id ?? null : null;
    const currentDark = filterGroup.dark_sets.length > 0 ? filterGroup.dark_sets[0].set.id ?? null : null;
    const currentBias = filterGroup.bias_sets.length > 0 ? filterGroup.bias_sets[0].set.id ?? null : null;

    setManualModalFrameIds(frameIds);
    setManualModalFilterDisplay(filterGroup.filter_display);
    setManualModalCurrentFlat(currentFlat);
    setManualModalCurrentDark(currentDark);
    setManualModalCurrentBias(currentBias);
    setManualModalOpen(true);
  }, []);

  // Handle manual calibration apply
  const handleManualCalibrationApply = useCallback(
    async (flatSetId: number | null, darkSetId: number | null, biasSetId: number | null) => {
      try {
        if (flatSetId !== null && flatSetId !== manualModalCurrentFlat) {
          await invoke('manual_assign_calibration', {
            frameIds: manualModalFrameIds,
            calibrationSetId: flatSetId,
            calibrationType: 'Flat',
          });
          setManualModalCurrentFlat(flatSetId);
        }
        if (darkSetId !== null && darkSetId !== manualModalCurrentDark) {
          await invoke('manual_assign_calibration', {
            frameIds: manualModalFrameIds,
            calibrationSetId: darkSetId,
            calibrationType: 'Dark',
          });
          setManualModalCurrentDark(darkSetId);
        }
        if (biasSetId !== null && biasSetId !== manualModalCurrentBias) {
          await invoke('manual_assign_calibration', {
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

  return (
    <div className="flex flex-col h-full">
      {/* Main Content - Master-Detail Layout */}
      {data.date_groups.length > 0 ? (
        <div className="flex flex-1 min-h-0 gap-4">
          {/* Navigation Tree - Left Panel */}
          <NavigationTree
            data={data}
            selectedItem={selectedItem}
            onSelect={setSelectedItem}
            warningCounts={warningCounts}
            className="w-80 flex-shrink-0"
            checkedFilters={checkedFilters}
            onCheckedChange={setCheckedFilters}
            selectionMode={selectionMode}
            onToggleSelectionMode={toggleSelectionMode}
          />

          {/* Detail Panel - Right Panel */}
          <DetailPanel
            selectedItem={selectedItem}
            onManualCalibration={openManualCalibrationModal}
            onBlink={onBlink ? handleBlink : undefined}
            className="flex-1"
          />
        </div>
      ) : (
        /* Empty State */
        <div className="flex-1 flex items-center justify-center">
          <div className="text-center py-16 px-8 bg-gray-800 rounded-xl border border-gray-600">
            <p className="text-lg text-gray-400">No frames found in this frame set.</p>
          </div>
        </div>
      )}

      {/* Bottom Action Bar - visible when in selection mode with items selected */}
      {selectionMode && checkedFilters.size > 0 && (onSplit || onCreateCustomSet) && (
        <div className="mt-3 bg-gray-800/80 rounded-lg p-3 border border-gray-700/50">
          <div className="flex items-center justify-between">
            <div className="text-sm text-gray-300">
              <span className="font-medium text-gray-100">{checkedFilters.size}</span>{' '}
              filter group{checkedFilters.size !== 1 ? 's' : ''} selected
              <span className="text-gray-500 ml-2">
                ({selectedFrameCount} frame{selectedFrameCount !== 1 ? 's' : ''})
              </span>
            </div>
            <div className="flex items-center gap-2">
              <button
                onClick={toggleSelectionMode}
                className="
                  px-3 py-1.5
                  text-gray-400 hover:text-gray-200
                  text-sm
                  rounded
                  transition-colors
                  focus:outline-none focus-visible:ring-1 focus-visible:ring-gray-500
                "
              >
                Cancel
              </button>
              {onSplit && (
                <button
                  onClick={handleSplit}
                  className="
                    inline-flex items-center gap-1.5
                    px-3 py-1.5
                    bg-blue-600 hover:bg-blue-700
                    text-white text-sm
                    rounded
                    transition-colors
                    focus:outline-none focus-visible:ring-1 focus-visible:ring-blue-500
                  "
                >
                  <Scissors size={14} aria-hidden="true" />
                  Split
                </button>
              )}
              {onCreateCustomSet && (
                <button
                  onClick={handleCreateCustomSet}
                  className="
                    inline-flex items-center gap-1.5
                    px-3 py-1.5
                    bg-green-600 hover:bg-green-700
                    text-white text-sm
                    rounded
                    transition-colors
                    focus:outline-none focus-visible:ring-1 focus-visible:ring-green-500
                  "
                >
                  <Plus size={14} aria-hidden="true" />
                  Create Set
                </button>
              )}
            </div>
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
