import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Settings, Eye } from 'lucide-react';
import type { CalibrationFilterGroup, LightFrameWithCalibration } from '../../types/models';
import type { SelectedItem } from './NavigationTree';
import { CalibrationSetCard, EmptyCalibrationCard } from './CalibrationSetCard';
import { LightFrameList } from './LightFrameList';
import { WarningPanel, AggregatedWarning } from './WarningPanel';
import { SubCalibrationModal } from '../SubCalibrationModal';

interface DetailPanelProps {
  selectedItem: SelectedItem | null;
  onManualCalibration: (filterGroup: CalibrationFilterGroup) => void;
  /** Callback to open blink viewer with frames */
  onBlink?: (frames: LightFrameWithCalibration[]) => void;
  /** Callback when sub-calibration is changed (to refresh hierarchy) */
  onRefresh?: () => void;
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
 * - Blink button for LIGHT frames
 * - Manual calibration button
 */
export function DetailPanel({
  selectedItem,
  onManualCalibration,
  onBlink,
  onRefresh,
  className = '',
}: DetailPanelProps) {
  const [blackholedFileIds, setBlackholedFileIds] = useState<Set<number>>(new Set());

  // Sub-calibration modal state
  const [subCalModalSetId, setSubCalModalSetId] = useState<number | null>(null);
  const [subCalModalType, setSubCalModalType] = useState<'flat' | 'dark'>('flat');

  const handleEditSubCalibration = (setId: number, setType: 'flat' | 'dark' | 'bias') => {
    if (setType === 'bias') return; // Bias sets don't have sub-calibration
    setSubCalModalSetId(setId);
    setSubCalModalType(setType);
  };

  const handleSubCalApply = () => {
    // Refresh the hierarchy to show updated sub-calibration
    onRefresh?.();
  };

  // Fetch blackholed file IDs when filter group changes
  useEffect(() => {
    if (!selectedItem || selectedItem.type !== 'filter') {
      setBlackholedFileIds(new Set());
      return;
    }

    const filterGroup = selectedItem.data as CalibrationFilterGroup;
    const fileIds = filterGroup.light_frames.map(f => f.file_id);

    if (fileIds.length === 0) {
      setBlackholedFileIds(new Set());
      return;
    }

    invoke<number[]>('get_blackholed_file_ids', { fileIds })
      .then(blackholed => setBlackholedFileIds(new Set(blackholed)))
      .catch(err => {
        console.error('Failed to fetch blackholed file IDs:', err);
        setBlackholedFileIds(new Set());
      });
  }, [selectedItem]);

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
    filterGroup.dark_sets.length > 0;

  // Check if we have frames to view/blink (1+ LIGHT frames)
  const canBlink = filterGroup.light_frames.length >= 1;

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
          <div className="flex items-center gap-2">
            {/* Blink button */}
            {onBlink && canBlink && (
              <button
                onClick={() => onBlink(filterGroup.light_frames)}
                className="
                  inline-flex items-center gap-2
                  min-h-[44px] px-4 py-2
                  bg-blue-600 hover:bg-blue-700
                  text-white text-sm font-medium
                  rounded-lg
                  transition-colors
                  focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500
                "
                title="Open blink viewer for LIGHT frames"
              >
                <Eye size={18} aria-hidden="true" />
                Blink
              </button>
            )}
            {/* Manual Calibration button */}
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
          </div>
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
                    onEditSubCalibration={handleEditSubCalibration}
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
                    onEditSubCalibration={handleEditSubCalibration}
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
        <LightFrameList frames={filterGroup.light_frames} blackholedFileIds={blackholedFileIds} />
      </div>

      {/* Sub-Calibration Modal */}
      {subCalModalSetId !== null && (
        <SubCalibrationModal
          isOpen={true}
          sourceSetId={subCalModalSetId}
          sourceType={subCalModalType}
          onApply={handleSubCalApply}
          onClose={() => setSubCalModalSetId(null)}
        />
      )}
    </div>
  );
}
