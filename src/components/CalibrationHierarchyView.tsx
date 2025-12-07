import { AlertTriangle, Calendar, Camera, ChevronDown, ChevronRight, Aperture, CheckCircle, XCircle, Settings } from 'lucide-react';
import { useState } from 'react';
import { invoke } from '@tauri-apps/api/core';
import type {
  CalibrationHierarchyView as CalibrationHierarchyViewData,
  CalibrationDateGroup,
  CalibrationSetDetail,
  CalibrationWarning,
  LightFrameWithCalibration,
  CalibrationCameraGroup,
  CalibrationFilterGroup,
} from '../types/models';
import { ManualCalibrationModal } from './ManualCalibrationModal';

interface CalibrationHierarchyViewProps {
  data: CalibrationHierarchyViewData;
  useBiasForDarkOptimization?: boolean;
  onRefresh?: () => void;
}

// Aggregated warning with context
interface AggregatedWarning {
  message: string;
  type: 'missing_calibration' | 'date' | 'temperature';
  filter?: string;
  camera?: string;
}

// Collect all warnings from a filter group
function collectFilterGroupWarnings(filterGroup: CalibrationFilterGroup, includeContext: boolean = true): AggregatedWarning[] {
  const warnings: AggregatedWarning[] = [];
  const filterDisplay = filterGroup.filter_display;

  // Check for missing calibration
  const hasCalibration = filterGroup.flat_sets.length > 0 ||
                         filterGroup.dark_sets.length > 0 ||
                         filterGroup.bias_sets.length > 0;

  if (!hasCalibration) {
    warnings.push({
      message: includeContext
        ? `No calibration linked for ${filterDisplay} (${filterGroup.frame_count} frame${filterGroup.frame_count !== 1 ? 's' : ''})`
        : `No calibration linked (${filterGroup.frame_count} frame${filterGroup.frame_count !== 1 ? 's' : ''})`,
      type: 'missing_calibration',
      filter: filterGroup.filter ?? undefined,
    });
  }

  // Collect warnings from calibration sets
  const addSetWarnings = (sets: typeof filterGroup.flat_sets, setType: string) => {
    for (const setWithCount of sets) {
      for (const warning of setWithCount.warnings) {
        warnings.push({
          message: includeContext
            ? `${filterDisplay} ${setType}: ${warning.message}`
            : `${setType}: ${warning.message}`,
          type: warning.warning_type as 'date' | 'temperature',
          filter: filterGroup.filter ?? undefined,
        });
      }
    }
  };

  addSetWarnings(filterGroup.flat_sets, 'Flat');
  addSetWarnings(filterGroup.dark_sets, 'Dark');
  addSetWarnings(filterGroup.bias_sets, 'Bias');

  return warnings;
}

// Collect all warnings from a camera group
function collectCameraGroupWarnings(cameraGroup: CalibrationCameraGroup, includeContext: boolean = true): AggregatedWarning[] {
  const warnings: AggregatedWarning[] = [];

  for (const filterGroup of cameraGroup.filter_groups) {
    const filterWarnings = collectFilterGroupWarnings(filterGroup, includeContext);
    for (const warning of filterWarnings) {
      warnings.push({
        ...warning,
        camera: cameraGroup.instrume,
      });
    }
  }

  return warnings;
}

// Collect all warnings from a date group
function collectDateGroupWarnings(dateGroup: CalibrationDateGroup): AggregatedWarning[] {
  const warnings: AggregatedWarning[] = [];

  for (const cameraGroup of dateGroup.camera_groups) {
    const cameraWarnings = collectCameraGroupWarnings(cameraGroup, true);
    warnings.push(...cameraWarnings);
  }

  return warnings;
}

// Stacked warnings display component
function WarningsList({ warnings, className = '' }: { warnings: AggregatedWarning[]; className?: string }) {
  if (warnings.length === 0) return null;

  return (
    <div className={`space-y-1 ${className}`}>
      {warnings.map((warning, i) => (
        <div key={i} className="flex items-start gap-2 text-xs">
          <AlertTriangle size={14} className="text-orange-400 flex-shrink-0 mt-0.5" />
          <span className="text-orange-200">{warning.message}</span>
        </div>
      ))}
    </div>
  );
}

export function CalibrationHierarchyView({ data, useBiasForDarkOptimization = false, onRefresh }: CalibrationHierarchyViewProps) {
  // Track expanded state for each level
  // Keys: date, date:camera, date:camera:filter, date:camera:filter:frames
  const [expandedItems, setExpandedItems] = useState<Set<string>>(new Set());

  // Manual calibration modal state
  const [manualModalOpen, setManualModalOpen] = useState(false);
  const [manualModalFrameIds, setManualModalFrameIds] = useState<number[]>([]);
  const [manualModalFilterDisplay, setManualModalFilterDisplay] = useState('');
  const [manualModalCurrentFlat, setManualModalCurrentFlat] = useState<number | null>(null);
  const [manualModalCurrentDark, setManualModalCurrentDark] = useState<number | null>(null);
  const [manualModalCurrentBias, setManualModalCurrentBias] = useState<number | null>(null);

  // Open manual calibration modal for a filter group
  const openManualCalibrationModal = (filterGroup: CalibrationFilterGroup) => {
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
  };

  // Handle manual calibration apply
  const handleManualCalibrationApply = async (
    flatSetId: number | null,
    darkSetId: number | null,
    biasSetId: number | null
  ) => {
    try {
      // Apply each calibration type that was selected
      if (flatSetId !== null && flatSetId !== manualModalCurrentFlat) {
        await invoke('manual_assign_calibration', {
          frameIds: manualModalFrameIds,
          calibrationSetId: flatSetId,
          calibrationType: 'Flat',
        });
        // Update local state to reflect the new assignment
        setManualModalCurrentFlat(flatSetId);
      }
      if (darkSetId !== null && darkSetId !== manualModalCurrentDark) {
        await invoke('manual_assign_calibration', {
          frameIds: manualModalFrameIds,
          calibrationSetId: darkSetId,
          calibrationType: 'Dark',
        });
        // Update local state to reflect the new assignment
        setManualModalCurrentDark(darkSetId);
      }
      if (biasSetId !== null && biasSetId !== manualModalCurrentBias) {
        await invoke('manual_assign_calibration', {
          frameIds: manualModalFrameIds,
          calibrationSetId: biasSetId,
          calibrationType: 'Bias',
        });
        // Update local state to reflect the new assignment
        setManualModalCurrentBias(biasSetId);
      }

      setManualModalOpen(false);

      // Trigger refresh of parent data
      if (onRefresh) {
        onRefresh();
      }
    } catch (error) {
      console.error('Failed to apply manual calibration:', error);
    }
  };

  const toggleItem = (key: string) => {
    setExpandedItems((prev) => {
      const next = new Set(prev);
      if (next.has(key)) {
        next.delete(key);
      } else {
        next.add(key);
      }
      return next;
    });
  };

  const formatTemp = (temp: number | null) => {
    if (temp === null) return 'N/A';
    return `${temp.toFixed(1)}°C`;
  };

  // Calibration Set Card component (reused from CalibrationGroupsView)
  const CalibrationSetCard = ({
    title,
    set,
    type,
    warnings,
    linkedFrameCount
  }: {
    title: string;
    set: CalibrationSetDetail;
    type: 'flat' | 'dark' | 'bias';
    warnings: CalibrationWarning[];
    linkedFrameCount: number;  // Number of light frames using this calibration set
  }) => {
    const bgColor = type === 'flat'
      ? 'bg-blue-900/20 border-blue-700/30'
      : type === 'dark'
      ? 'bg-purple-900/20 border-purple-700/30'
      : 'bg-green-900/20 border-green-700/30';

    return (
      <div className={`border rounded p-3 ${bgColor}`}>
        <h4 className="font-medium text-sm mb-2">
          {title}
          <span className="text-gray-400 font-normal ml-2">
            ({linkedFrameCount} light{linkedFrameCount !== 1 ? 's' : ''})
          </span>
        </h4>
        <div className="grid grid-cols-2 gap-2 text-xs">
          {set.exptime !== null && (
            <div>
              <span className="text-gray-400">Exposure:</span>{' '}
              <span className="text-gray-200">{set.exptime}s</span>
            </div>
          )}
          <div>
            <span className="text-gray-400">Temp:</span>{' '}
            <span className="text-gray-200">
              {formatTemp(set.ccd_temp)}
              {set.temp_min !== set.temp_max && (
                <span className="text-gray-400 ml-1">
                  ({formatTemp(set.temp_min)} - {formatTemp(set.temp_max)})
                </span>
              )}
            </span>
          </div>
          {set.gain !== null && (
            <div>
              <span className="text-gray-400">Gain:</span>{' '}
              <span className="text-gray-200">{set.gain}</span>
            </div>
          )}
          {set.binning && (
            <div>
              <span className="text-gray-400">Binning:</span>{' '}
              <span className="text-gray-200">{set.binning}</span>
            </div>
          )}
          {type === 'flat' && set.filter && (
            <div>
              <span className="text-gray-400">Filter:</span>{' '}
              <span className="text-gray-200">{set.filter}</span>
            </div>
          )}
          <div>
            <span className="text-gray-400">Date:</span>{' '}
            <span className="text-gray-200">
              {set.date_start ? new Date(set.date_start).toLocaleDateString() : set.date_display}
            </span>
          </div>
          <div>
            <span className="text-gray-400">Time:</span>{' '}
            <span className="text-gray-200">
              {set.date_start ? new Date(set.date_start).toLocaleTimeString('en-GB', { hour: '2-digit', minute: '2-digit', second: '2-digit' }) : 'N/A'}
            </span>
          </div>
          <div>
            <span className="text-gray-400">Set Frames:</span>{' '}
            <span className="text-gray-200">{set.frame_count}</span>
          </div>
        </div>

        {/* Warning display */}
        {warnings.length > 0 && (
          <div className="mt-3 pt-3 border-t border-yellow-700/30 space-y-1">
            {warnings.map((warning, i) => (
              <div key={i} className="flex items-start gap-2 text-xs">
                <AlertTriangle size={14} className="text-yellow-400 flex-shrink-0 mt-0.5" />
                <span className="text-yellow-200">{warning.message}</span>
              </div>
            ))}
          </div>
        )}
      </div>
    );
  };

  // Calibration status icon for a single frame
  const CalibrationIcon = ({ has, warning }: { has: boolean; warning: boolean }) => {
    if (!has) {
      return <XCircle size={14} className="text-red-400" />;
    }
    if (warning) {
      return <AlertTriangle size={14} className="text-yellow-400" />;
    }
    return <CheckCircle size={14} className="text-green-400" />;
  };

  // Light frame row with calibration status
  const LightFrameRow = ({ frame, index }: { frame: LightFrameWithCalibration; index: number }) => {
    const status = frame.calibration_status;

    return (
      <div
        className={`flex items-center gap-3 px-3 py-2 text-sm ${
          index % 2 === 0 ? 'bg-gray-800' : 'bg-gray-850'
        }`}
      >
        <span className="font-mono text-gray-300 truncate flex-1 min-w-0">
          {frame.filename}
        </span>
        {frame.exptime !== null && (
          <span className="text-gray-400 text-xs w-16 text-right">
            {frame.exptime}s
          </span>
        )}
        <div className="flex items-center gap-1 text-xs">
          <span title="Flat" className="flex items-center gap-0.5">
            <CalibrationIcon has={status.has_flats} warning={status.flats_warning} />
            <span className="text-gray-500">F</span>
          </span>
          <span title="Dark" className="flex items-center gap-0.5">
            <CalibrationIcon has={status.has_darks} warning={status.darks_warning} />
            <span className="text-gray-500">D</span>
          </span>
          <span title="Bias" className="flex items-center gap-0.5">
            <CalibrationIcon has={status.has_bias} warning={status.bias_warning} />
            <span className="text-gray-500">B</span>
          </span>
        </div>
      </div>
    );
  };

  // Filter group content
  const FilterGroupContent = ({
    filterGroup,
    dateKey,
    cameraKey
  }: {
    filterGroup: CalibrationFilterGroup;
    dateKey: string;
    cameraKey: string;
  }) => {
    const filterKey = filterGroup.filter ?? '__no_filter__';
    const key = `${dateKey}:${cameraKey}:${filterKey}`;
    const framesKey = `${key}:frames`;
    const isExpanded = expandedItems.has(key);
    const isFramesExpanded = expandedItems.has(framesKey);
    const hasCalibration = filterGroup.flat_sets.length > 0 || filterGroup.dark_sets.length > 0 || filterGroup.bias_sets.length > 0;
    const filterWarnings = collectFilterGroupWarnings(filterGroup, false); // false = no filter context (we're at filter level)

    return (
      <div className="ml-8 border-l border-gray-700">
        {/* Filter Header */}
        <div className="flex items-center">
          <button
            onClick={() => toggleItem(key)}
            className="flex-1 px-4 py-2 flex items-center gap-3 hover:bg-gray-750 transition-colors text-left"
          >
            {isExpanded ? (
              <ChevronDown className="w-4 h-4 text-gray-400" />
            ) : (
              <ChevronRight className="w-4 h-4 text-gray-400" />
            )}
            <Aperture size={16} className="text-cyan-400" />
            <div className="flex-1">
              <span className="text-gray-200">{filterGroup.filter_display}</span>
              {filterWarnings.length > 0 && (
                <span className="inline-flex items-center gap-1 ml-2">
                  <AlertTriangle className="w-4 h-4 text-orange-400" />
                  <span className="text-xs text-orange-400">({filterWarnings.length})</span>
                </span>
              )}
            </div>
            <span className="text-sm text-gray-500">
              {filterGroup.frame_count} frame{filterGroup.frame_count !== 1 ? 's' : ''}
            </span>
          </button>
          {/* Manual Calibration Button */}
          <button
            onClick={(e) => {
              e.stopPropagation();
              openManualCalibrationModal(filterGroup);
            }}
            className="px-3 py-1.5 mr-2 text-xs bg-gray-700 hover:bg-gray-600 text-gray-300 rounded flex items-center gap-1.5 transition-colors"
            title="Manual Calibration Selection"
          >
            <Settings className="w-3 h-3" />
            Manual
          </button>
        </div>

        {/* Filter Expanded Content */}
        {isExpanded && (
          <div className="px-4 pb-4 space-y-4 bg-gray-850 ml-4">
            {/* Stacked Warnings at Filter Level */}
            {filterWarnings.length > 0 && (
              <div className="py-2 px-3 rounded bg-orange-900/10 border border-orange-700/30 mt-3">
                <WarningsList warnings={filterWarnings} />
              </div>
            )}

            {/* Calibration Set Cards */}
            {hasCalibration && (
              <div className="grid grid-cols-1 md:grid-cols-3 gap-3 mt-3">
                {/* Flat calibration sets */}
                {filterGroup.flat_sets.map((flatWithCount, idx) => (
                  <CalibrationSetCard
                    key={`flat-${flatWithCount.set.id ?? idx}`}
                    title={`Flat${flatWithCount.set.filter ? ` (${flatWithCount.set.filter})` : ''}`}
                    set={flatWithCount.set}
                    type="flat"
                    warnings={[]} // Warnings now shown at filter level
                    linkedFrameCount={flatWithCount.frame_count}
                  />
                ))}
                {/* Dark calibration sets */}
                {filterGroup.dark_sets.map((darkWithCount, idx) => (
                  <CalibrationSetCard
                    key={`dark-${darkWithCount.set.id ?? idx}`}
                    title={`Dark${darkWithCount.set.exptime !== null ? ` (${darkWithCount.set.exptime}s)` : ''}`}
                    set={darkWithCount.set}
                    type="dark"
                    warnings={[]} // Warnings now shown at filter level
                    linkedFrameCount={darkWithCount.frame_count}
                  />
                ))}
                {/* Bias calibration sets */}
                {filterGroup.bias_sets.map((biasWithCount, idx) => (
                  <CalibrationSetCard
                    key={`bias-${biasWithCount.set.id ?? idx}`}
                    title="Bias"
                    set={biasWithCount.set}
                    type="bias"
                    warnings={[]} // Warnings now shown at filter level
                    linkedFrameCount={biasWithCount.frame_count}
                  />
                ))}
              </div>
            )}

            {/* Light Frames List Toggle */}
            <div className="mt-4">
              <button
                onClick={() => toggleItem(framesKey)}
                className="flex items-center gap-2 text-sm text-gray-400 hover:text-gray-200 transition-colors"
              >
                {isFramesExpanded ? (
                  <ChevronDown className="w-4 h-4" />
                ) : (
                  <ChevronRight className="w-4 h-4" />
                )}
                Light Frames ({filterGroup.frame_count})
              </button>

              {isFramesExpanded && (
                <div className="mt-2 rounded overflow-hidden border border-gray-700">
                  {filterGroup.light_frames.map((frame, idx) => (
                    <LightFrameRow key={frame.frame_id} frame={frame} index={idx} />
                  ))}
                </div>
              )}
            </div>
          </div>
        )}
      </div>
    );
  };

  // Camera group content
  const CameraGroupContent = ({
    cameraGroup,
    dateKey
  }: {
    cameraGroup: CalibrationCameraGroup;
    dateKey: string;
  }) => {
    const cameraKey = cameraGroup.instrume;
    const key = `${dateKey}:${cameraKey}`;
    const isExpanded = expandedItems.has(key);
    const cameraWarnings = collectCameraGroupWarnings(cameraGroup, true);

    return (
      <div className="ml-4 border-l border-gray-700">
        {/* Camera Header */}
        <button
          onClick={() => toggleItem(key)}
          className="w-full px-4 py-2 flex items-center gap-3 hover:bg-gray-750 transition-colors text-left"
        >
          {isExpanded ? (
            <ChevronDown className="w-4 h-4 text-gray-400" />
          ) : (
            <ChevronRight className="w-4 h-4 text-gray-400" />
          )}
          <Camera size={16} className="text-blue-400" />
          <div className="flex-1">
            <span className="text-gray-200 font-medium">{cameraGroup.instrume}</span>
            {cameraWarnings.length > 0 && (
              <span className="inline-flex items-center gap-1 ml-2">
                <AlertTriangle className="w-4 h-4 text-orange-400" />
                <span className="text-xs text-orange-400">({cameraWarnings.length})</span>
              </span>
            )}
          </div>
          <span className="text-sm text-gray-500">
            {cameraGroup.frame_count} frame{cameraGroup.frame_count !== 1 ? 's' : ''}
          </span>
        </button>

        {/* Camera Expanded Content */}
        {isExpanded && (
          <div className="pb-2">
            {/* Stacked Warnings at Camera Level */}
            {cameraWarnings.length > 0 && (
              <div className="ml-8 px-4 py-2 border-l border-gray-700 bg-orange-900/10">
                <WarningsList warnings={cameraWarnings} />
              </div>
            )}

            {/* Filter Groups */}
            {cameraGroup.filter_groups.map((filterGroup) => (
              <FilterGroupContent
                key={filterGroup.filter ?? '__no_filter__'}
                filterGroup={filterGroup}
                dateKey={dateKey}
                cameraKey={cameraKey}
              />
            ))}
          </div>
        )}
      </div>
    );
  };

  // Date group content
  const DateGroupContent = ({ dateGroup }: { dateGroup: CalibrationDateGroup }) => {
    const dateKey = dateGroup.date;
    const isExpanded = expandedItems.has(dateKey);
    const dateWarnings = collectDateGroupWarnings(dateGroup);

    return (
      <div className="bg-gray-800 rounded-lg border border-gray-700 overflow-hidden mb-3">
        {/* Date Header */}
        <button
          onClick={() => toggleItem(dateKey)}
          className="w-full px-4 py-3 flex items-center gap-3 hover:bg-gray-750 transition-colors text-left"
        >
          {isExpanded ? (
            <ChevronDown className="w-5 h-5 text-gray-400" />
          ) : (
            <ChevronRight className="w-5 h-5 text-gray-400" />
          )}
          <Calendar size={18} className="text-purple-400" />
          <div className="flex-1">
            <span className="text-gray-100 font-semibold">{dateGroup.date_display}</span>
            {dateWarnings.length > 0 && (
              <span className="inline-flex items-center gap-1 ml-2">
                <AlertTriangle className="w-4 h-4 text-orange-400" />
                <span className="text-xs text-orange-400">({dateWarnings.length})</span>
              </span>
            )}
          </div>
          <span className="text-sm text-gray-500">
            {dateGroup.frame_count} frame{dateGroup.frame_count !== 1 ? 's' : ''}
          </span>
        </button>

        {/* Date Expanded Content */}
        {isExpanded && (
          <div className="pb-2 bg-gray-850">
            {/* Stacked Warnings at Date Level */}
            {dateWarnings.length > 0 && (
              <div className="px-4 py-3 border-b border-gray-700 bg-orange-900/10">
                <WarningsList warnings={dateWarnings} />
              </div>
            )}

            {/* Camera Groups */}
            {dateGroup.camera_groups.map((cameraGroup) => (
              <CameraGroupContent
                key={cameraGroup.instrume}
                cameraGroup={cameraGroup}
                dateKey={dateKey}
              />
            ))}
          </div>
        )}
      </div>
    );
  };

  return (
    <div className="space-y-4">
      {/* Summary Stats */}
      <div className="bg-gray-800 rounded-lg p-4">
        <div className="grid grid-cols-4 gap-4 text-center">
          <div>
            <div className="text-2xl font-bold text-gray-300">{data.total_frames}</div>
            <div className="text-sm text-gray-400">Total Frames</div>
          </div>
          <div>
            <div className="text-2xl font-bold text-green-400">{data.calibrated_frames}</div>
            <div className="text-sm text-gray-400">Calibrated</div>
          </div>
          <div>
            <div className="text-2xl font-bold text-orange-400">{data.uncalibrated_frames}</div>
            <div className="text-sm text-gray-400">Uncalibrated</div>
          </div>
          <div>
            <div className="text-2xl font-bold text-blue-400">{data.date_groups.length}</div>
            <div className="text-sm text-gray-400">Sessions</div>
          </div>
        </div>
      </div>

      {/* Date Groups */}
      {data.date_groups.length > 0 && (
        <div>
          <h3 className="text-lg font-semibold text-gray-200 mb-3">Calibration by Session</h3>
          {data.date_groups.map((dateGroup) => (
            <DateGroupContent key={dateGroup.date} dateGroup={dateGroup} />
          ))}
        </div>
      )}

      {/* Empty State */}
      {data.date_groups.length === 0 && data.total_frames === 0 && (
        <div className="text-center py-12 text-gray-400">
          <p>No frames found in this frame set.</p>
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
