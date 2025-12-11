import { useState, useMemo, useCallback } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type {
  CalibrationHierarchyView as CalibrationHierarchyViewData,
  CalibrationFilterGroup,
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
}: CalibrationHierarchyViewProps) {
  // Selection state for master-detail navigation
  const [selectedItem, setSelectedItem] = useState<SelectedItem | null>(null);

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
          const filterKey = filterGroup.filter ?? '__no_filter__';
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
      {/* Summary Stats Bar */}
      <div className="bg-gray-800 rounded-xl p-5 border border-gray-600">
        <div className="grid grid-cols-4 gap-6 text-center">
          <div>
            <div className="text-3xl font-bold text-gray-100">{data.total_frames}</div>
            <div className="text-sm text-gray-300 mt-1">Total Frames</div>
          </div>
          <div>
            <div className="text-3xl font-bold text-emerald-400">{data.calibrated_frames}</div>
            <div className="text-sm text-gray-300 mt-1">Calibrated</div>
          </div>
          <div>
            <div className="text-3xl font-bold text-amber-400">{data.uncalibrated_frames}</div>
            <div className="text-sm text-gray-300 mt-1">Uncalibrated</div>
          </div>
          <div>
            <div className="text-3xl font-bold text-blue-400">{data.date_groups.length}</div>
            <div className="text-sm text-gray-300 mt-1">Sessions</div>
          </div>
        </div>
      </div>

      {/* Main Content - Master-Detail Layout */}
      {data.date_groups.length > 0 ? (
        <div className="flex flex-1 min-h-0 mt-4 gap-4">
          {/* Navigation Tree - Left Panel */}
          <NavigationTree
            data={data}
            selectedItem={selectedItem}
            onSelect={setSelectedItem}
            warningCounts={warningCounts}
            className="w-96 flex-shrink-0"
          />

          {/* Detail Panel - Right Panel */}
          <DetailPanel
            selectedItem={selectedItem}
            onManualCalibration={openManualCalibrationModal}
            className="flex-1"
          />
        </div>
      ) : (
        /* Empty State */
        <div className="flex-1 flex items-center justify-center mt-4">
          <div className="text-center py-16 px-8 bg-gray-800 rounded-xl border border-gray-600">
            <p className="text-lg text-gray-400">No frames found in this frame set.</p>
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
