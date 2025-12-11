import { Settings } from 'lucide-react';
import type { CalibrationFilterGroup } from '../../types/models';
import type { SelectedItem } from './NavigationTree';
import { CalibrationSetCard, EmptyCalibrationCard } from './CalibrationSetCard';
import { LightFrameList } from './LightFrameList';
import { WarningPanel, AggregatedWarning } from './WarningPanel';

interface DetailPanelProps {
  selectedItem: SelectedItem | null;
  onManualCalibration: (filterGroup: CalibrationFilterGroup) => void;
  className?: string;
}

/**
 * Collect warnings from a filter group for display.
 */
function collectFilterGroupWarnings(filterGroup: CalibrationFilterGroup): AggregatedWarning[] {
  const warnings: AggregatedWarning[] = [];

  // Check for missing calibration (only Flat and Dark matter for lights)
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

  // Collect warnings from calibration sets
  const addSetWarnings = (
    sets: typeof filterGroup.flat_sets,
    setType: string
  ) => {
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
 * Detail panel showing calibration information for selected filter group.
 * Features:
 * - Spacious layout with larger text
 * - Clear section hierarchy
 * - Prominent warning display
 * - Manual calibration button
 */
export function DetailPanel({
  selectedItem,
  onManualCalibration,
  className = '',
}: DetailPanelProps) {
  // Empty state when nothing selected
  if (!selectedItem || selectedItem.type !== 'filter') {
    return (
      <div
        className={`bg-gray-800 rounded-xl border border-gray-600 flex items-center justify-center ${className}`}
      >
        <div className="text-center px-8 py-16">
          <p className="text-lg text-gray-400">
            Select a filter group from the navigation tree to view calibration details
          </p>
        </div>
      </div>
    );
  }

  const filterGroup = selectedItem.data as CalibrationFilterGroup;
  const warnings = collectFilterGroupWarnings(filterGroup);
  const hasCalibration =
    filterGroup.flat_sets.length > 0 ||
    filterGroup.dark_sets.length > 0 ||
    filterGroup.bias_sets.length > 0;

  return (
    <div
      className={`bg-gray-800 rounded-xl border border-gray-600 overflow-y-auto ${className}`}
    >
      <div className="p-6">
        {/* Header */}
        <header className="flex items-start justify-between gap-4 mb-6">
          <div>
            <h2 className="text-xl font-bold text-gray-100">
              {filterGroup.filter_display}
            </h2>
            <p className="text-base text-gray-300 mt-1">
              {filterGroup.frame_count} light frame{filterGroup.frame_count !== 1 ? 's' : ''}
              {filterGroup.exptime !== null && ` at ${filterGroup.exptime}s`}
            </p>
          </div>
          <button
            onClick={() => onManualCalibration(filterGroup)}
            className="
              inline-flex items-center gap-2
              min-h-[44px] px-4 py-2
              bg-gray-700 hover:bg-gray-600
              text-gray-200 text-sm font-medium
              rounded-lg border border-gray-600
              transition-colors
              focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500
            "
          >
            <Settings size={18} aria-hidden="true" />
            Manual Calibration
          </button>
        </header>

        {/* Warnings Section */}
        {warnings.length > 0 && (
          <div className="mb-6">
            <WarningPanel aggregatedWarnings={warnings} />
          </div>
        )}

        {/* Calibration Sets Section */}
        <section>
          <h3 className="text-lg font-semibold text-gray-100 mb-4">
            Calibration Sets
          </h3>

          {hasCalibration ? (
            <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
              {/* Flat sets */}
              {filterGroup.flat_sets.length > 0 ? (
                filterGroup.flat_sets.map((flatSet, idx) => (
                  <CalibrationSetCard
                    key={`flat-${flatSet.set.id ?? idx}`}
                    type="flat"
                    data={flatSet}
                  />
                ))
              ) : (
                <EmptyCalibrationCard type="flat" />
              )}

              {/* Dark sets */}
              {filterGroup.dark_sets.length > 0 ? (
                filterGroup.dark_sets.map((darkSet, idx) => (
                  <CalibrationSetCard
                    key={`dark-${darkSet.set.id ?? idx}`}
                    type="dark"
                    data={darkSet}
                  />
                ))
              ) : (
                <EmptyCalibrationCard type="dark" />
              )}
            </div>
          ) : (
            <div className="grid grid-cols-1 lg:grid-cols-2 gap-4">
              <EmptyCalibrationCard type="flat" />
              <EmptyCalibrationCard type="dark" />
            </div>
          )}
        </section>

        {/* Light Frames Section */}
        <LightFrameList frames={filterGroup.light_frames} />
      </div>
    </div>
  );
}
