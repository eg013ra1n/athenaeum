import { useState, useEffect } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { Settings, Eye, AlertTriangle, ChevronDown, ChevronRight } from 'lucide-react';
import type { CalibrationFilterGroup, LightFrameWithCalibration, FileWithFrame } from '../../types/models';
import type { SelectedItem } from './NavigationTree';
import { CalibrationSetRow, EmptyCalibrationRow } from './CalibrationSetRow';
import { LightFrameList } from './LightFrameList';
import type { AggregatedWarning } from './WarningPanel';
import { SubCalibrationModal } from '../SubCalibrationModal';
import BlinkViewer from '../BlinkViewer';

interface DetailPanelProps {
  selectedItem: SelectedItem | null;
  onManualCalibration: (filterGroup: CalibrationFilterGroup) => void;
  /** Callback to open blink viewer with light frames */
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
 *
 * Compact layout: calibration sets as collapsible rows, light frames expanded by default.
 * Supports blinking both light frames and calibration set frames.
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

  // Calibration set blink state
  const [calBlinkFrames, setCalBlinkFrames] = useState<FileWithFrame[] | null>(null);
  const [calBlinkLoading, setCalBlinkLoading] = useState<number | null>(null);

  // Calibration section collapse
  const [calSectionExpanded, setCalSectionExpanded] = useState(true);

  const handleEditSubCalibration = (setId: number, setType: 'flat' | 'dark' | 'bias') => {
    if (setType === 'bias') return;
    setSubCalModalSetId(setId);
    setSubCalModalType(setType);
  };

  const handleSubCalApply = () => {
    onRefresh?.();
  };

  const handleViewCalSetFrames = async (setId: number) => {
    try {
      setCalBlinkLoading(setId);
      const frames = await invoke<FileWithFrame[]>('get_calibration_set_frames', { setId });
      setCalBlinkFrames(frames);
    } catch (err) {
      console.error('Failed to load calibration set frames:', err);
    } finally {
      setCalBlinkLoading(null);
    }
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
        className={`bg-surface-elevated rounded-xl border border-border flex items-center justify-center ${className}`}
      >
        <div className="text-center px-8 py-16">
          <p className="text-lg text-content-muted">
            Select a filter group from the navigation tree to view calibration details
          </p>
        </div>
      </div>
    );
  }

  const filterGroup = selectedItem.data as CalibrationFilterGroup;
  const warnings = collectFilterGroupWarnings(filterGroup);

  // Count total calibration sets
  const totalCalSets =
    filterGroup.flat_sets.length +
    filterGroup.dark_sets.length +
    filterGroup.bias_sets.length;

  // Check if we have frames to view/blink (1+ LIGHT frames)
  const canBlink = filterGroup.light_frames.length >= 1;

  return (
    <div
      className={`bg-surface-elevated rounded-xl border border-border overflow-y-auto ${className}`}
    >
      <div className="p-5">
        {/* Header */}
        <header className="flex items-start justify-between gap-4 mb-4">
          <div>
            <h2 className="text-xl font-bold text-content">
              {filterGroup.filter_display}
            </h2>
            <p className="text-sm text-content-secondary mt-0.5">
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
                  bg-accent hover:bg-accent-hover
                  text-surface text-sm font-medium
                  rounded-lg
                  transition-colors
                  focus:outline-none focus-visible:ring-2 focus-visible:ring-accent
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
                bg-surface-hover hover:bg-surface-hover
                text-content text-sm font-medium
                rounded-lg border border-border
                transition-colors
                focus:outline-none focus-visible:ring-2 focus-visible:ring-accent
              "
            >
              <Settings size={18} aria-hidden="true" />
              Manual Calibration
            </button>
          </div>
        </header>

        {/* Compact inline warnings */}
        {warnings.length > 0 && (
          <div className="flex items-center gap-2 px-3 py-2 mb-4 bg-warning-muted rounded-lg border border-warning/50">
            <AlertTriangle size={16} className="text-warning flex-shrink-0" />
            <span className="text-sm text-warning/90">
              {warnings.length} warning{warnings.length !== 1 ? 's' : ''}
              {warnings.length <= 2 && (
                <span className="text-warning/70">
                  {' \u2014 '}
                  {warnings.map(w => w.message).join('; ')}
                </span>
              )}
            </span>
          </div>
        )}

        {/* Calibration Sets Section — collapsible rows */}
        <section className="mb-4">
          <button
            onClick={() => setCalSectionExpanded(!calSectionExpanded)}
            className="flex items-center gap-2 mb-2 hover:bg-surface-hover/30 rounded px-1 py-1 transition-colors"
          >
            {calSectionExpanded ? (
              <ChevronDown size={16} className="text-content-muted" />
            ) : (
              <ChevronRight size={16} className="text-content-muted" />
            )}
            <h3 className="text-sm font-semibold text-content">
              Calibration
            </h3>
            <span className="text-xs text-content-muted">
              {totalCalSets > 0
                ? `${totalCalSets} set${totalCalSets !== 1 ? 's' : ''}`
                : 'none linked'}
            </span>
          </button>

          {calSectionExpanded && (
            <div className="space-y-1.5">
              {/* Flat sets */}
              {filterGroup.flat_sets.length > 0 ? (
                filterGroup.flat_sets.map((flatSet, idx) => (
                  <CalibrationSetRow
                    key={`flat-${flatSet.set.id ?? idx}`}
                    type="flat"
                    data={flatSet}
                    onViewFrames={handleViewCalSetFrames}
                    onEditSubCalibration={handleEditSubCalibration}
                    isLoadingFrames={calBlinkLoading === flatSet.set.id}
                  />
                ))
              ) : (
                <EmptyCalibrationRow type="flat" />
              )}

              {/* Dark sets */}
              {filterGroup.dark_sets.length > 0 ? (
                filterGroup.dark_sets.map((darkSet, idx) => (
                  <CalibrationSetRow
                    key={`dark-${darkSet.set.id ?? idx}`}
                    type="dark"
                    data={darkSet}
                    onViewFrames={handleViewCalSetFrames}
                    onEditSubCalibration={handleEditSubCalibration}
                    isLoadingFrames={calBlinkLoading === darkSet.set.id}
                  />
                ))
              ) : (
                <EmptyCalibrationRow type="dark" />
              )}

              {/* Bias sets (only shown if present) */}
              {filterGroup.bias_sets.length > 0 &&
                filterGroup.bias_sets.map((biasSet, idx) => (
                  <CalibrationSetRow
                    key={`bias-${biasSet.set.id ?? idx}`}
                    type="bias"
                    data={biasSet}
                    onViewFrames={handleViewCalSetFrames}
                    isLoadingFrames={calBlinkLoading === biasSet.set.id}
                  />
                ))}
            </div>
          )}
        </section>

        {/* Light Frames Section — expanded by default */}
        <LightFrameList
          frames={filterGroup.light_frames}
          defaultExpanded={true}
          blackholedFileIds={blackholedFileIds}
        />
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

      {/* Calibration Set Blink Viewer */}
      {calBlinkFrames !== null && (
        <BlinkViewer
          frames={calBlinkFrames}
          initialIndex={0}
          onClose={() => setCalBlinkFrames(null)}
          sourceType="calibration"
        />
      )}
    </div>
  );
}
