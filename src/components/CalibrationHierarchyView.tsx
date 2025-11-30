import { AlertTriangle, Calendar, Camera, ChevronDown, ChevronRight, Filter, CheckCircle, XCircle } from 'lucide-react';
import { useState } from 'react';
import type {
  CalibrationHierarchyView as CalibrationHierarchyViewData,
  CalibrationDateGroup,
  CalibrationSetDetail,
  CalibrationWarning,
  LightFrameWithCalibration,
  CalibrationCameraGroup,
  CalibrationFilterGroup,
} from '../types/models';

interface CalibrationHierarchyViewProps {
  data: CalibrationHierarchyViewData;
}

export function CalibrationHierarchyView({ data }: CalibrationHierarchyViewProps) {
  // Track expanded state for each level
  // Keys: date, date:camera, date:camera:filter, date:camera:filter:frames
  const [expandedItems, setExpandedItems] = useState<Set<string>>(new Set());

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
    warnings
  }: {
    title: string;
    set: CalibrationSetDetail | null;
    type: 'flat' | 'dark' | 'bias';
    warnings: CalibrationWarning[];
  }) => {
    if (!set) return null;

    const bgColor = type === 'flat'
      ? 'bg-blue-900/20 border-blue-700/30'
      : type === 'dark'
      ? 'bg-purple-900/20 border-purple-700/30'
      : 'bg-green-900/20 border-green-700/30';

    return (
      <div className={`border rounded p-3 ${bgColor}`}>
        <h4 className="font-medium text-sm mb-2">{title}</h4>
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
          <div className="col-span-2">
            <span className="text-gray-400">Date:</span>{' '}
            <span className="text-gray-200">{set.date_display}</span>
          </div>
          <div>
            <span className="text-gray-400">Frames:</span>{' '}
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
    const hasCalibration = filterGroup.flat_set || filterGroup.dark_set || filterGroup.bias_set;

    return (
      <div className="ml-8 border-l border-gray-700">
        {/* Filter Header */}
        <button
          onClick={() => toggleItem(key)}
          className="w-full px-4 py-2 flex items-center gap-3 hover:bg-gray-750 transition-colors text-left"
        >
          {isExpanded ? (
            <ChevronDown className="w-4 h-4 text-gray-400" />
          ) : (
            <ChevronRight className="w-4 h-4 text-gray-400" />
          )}
          <Filter size={16} className="text-cyan-400" />
          <div className="flex-1">
            <span className="text-gray-200">{filterGroup.filter_display}</span>
            {filterGroup.has_warnings && (
              <AlertTriangle className="inline w-4 h-4 text-orange-400 ml-2" />
            )}
          </div>
          <span className="text-sm text-gray-500">
            {filterGroup.frame_count} frame{filterGroup.frame_count !== 1 ? 's' : ''}
          </span>
        </button>

        {/* Filter Expanded Content */}
        {isExpanded && (
          <div className="px-4 pb-4 space-y-4 bg-gray-850 ml-4">
            {/* Calibration Set Cards */}
            {hasCalibration && (
              <div className="grid grid-cols-1 md:grid-cols-3 gap-3 mt-3">
                {filterGroup.flat_set && (
                  <CalibrationSetCard
                    title="Flat Calibration"
                    set={filterGroup.flat_set}
                    type="flat"
                    warnings={filterGroup.flat_warnings}
                  />
                )}
                {filterGroup.dark_set && (
                  <CalibrationSetCard
                    title="Dark Calibration"
                    set={filterGroup.dark_set}
                    type="dark"
                    warnings={filterGroup.dark_warnings}
                  />
                )}
                {filterGroup.bias_set && (
                  <CalibrationSetCard
                    title="Bias Calibration"
                    set={filterGroup.bias_set}
                    type="bias"
                    warnings={filterGroup.bias_warnings}
                  />
                )}
              </div>
            )}

            {!hasCalibration && (
              <div className="text-sm text-orange-400 flex items-center gap-2 mt-3">
                <AlertTriangle size={16} />
                No calibration linked for these frames
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
            {cameraGroup.has_warnings && (
              <AlertTriangle className="inline w-4 h-4 text-orange-400 ml-2" />
            )}
          </div>
          <span className="text-sm text-gray-500">
            {cameraGroup.frame_count} frame{cameraGroup.frame_count !== 1 ? 's' : ''}
          </span>
        </button>

        {/* Camera Expanded Content - Filter Groups */}
        {isExpanded && (
          <div className="pb-2">
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
            {dateGroup.has_warnings && (
              <AlertTriangle className="inline w-4 h-4 text-orange-400 ml-2" />
            )}
          </div>
          <span className="text-sm text-gray-500">
            {dateGroup.frame_count} frame{dateGroup.frame_count !== 1 ? 's' : ''}
          </span>
        </button>

        {/* Date Expanded Content - Camera Groups */}
        {isExpanded && (
          <div className="pb-2 bg-gray-850">
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
    </div>
  );
}
