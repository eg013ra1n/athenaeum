import { AlertTriangle, ChevronDown, ChevronRight } from 'lucide-react';
import { useState } from 'react';
import type { CalibrationSetDetail, FrameSetCalibrationGroups } from '../types/models';

interface CalibrationGroupsViewProps {
  data: FrameSetCalibrationGroups;
}

export function CalibrationGroupsView({ data }: CalibrationGroupsViewProps) {
  const [expandedGroups, setExpandedGroups] = useState<Set<number>>(new Set());
  const [expandedUncalibrated, setExpandedUncalibrated] = useState(false);

  const toggleGroup = (index: number) => {
    setExpandedGroups((prev) => {
      const next = new Set(prev);
      if (next.has(index)) {
        next.delete(index);
      } else {
        next.add(index);
      }
      return next;
    });
  };

  const formatTemp = (temp: number | null) => {
    if (temp === null) return 'N/A';
    return `${temp.toFixed(1)}°C`;
  };

  const CalibrationSetCard = ({
    title,
    set,
    type
  }: {
    title: string;
    set: CalibrationSetDetail | null;
    type: 'flat' | 'dark' | 'bias';
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
          <div className="col-span-2">
            <span className="text-gray-400">Date:</span>{' '}
            <span className="text-gray-200">{set.date_display}</span>
          </div>
          <div>
            <span className="text-gray-400">Frames:</span>{' '}
            <span className="text-gray-200">{set.frame_count}</span>
          </div>
        </div>
      </div>
    );
  };

  return (
    <div className="space-y-4">
      {/* Summary Stats */}
      <div className="bg-gray-800 rounded-lg p-4">
        <div className="grid grid-cols-4 gap-4 text-center">
          <div>
            <div className="text-2xl font-bold text-blue-400">{data.groups.length}</div>
            <div className="text-sm text-gray-400">Calibration Groups</div>
          </div>
          <div>
            <div className="text-2xl font-bold text-green-400">
              {data.total_frames - data.uncalibrated_frame_count}
            </div>
            <div className="text-sm text-gray-400">Calibrated Frames</div>
          </div>
          <div>
            <div className="text-2xl font-bold text-orange-400">
              {data.uncalibrated_frame_count}
            </div>
            <div className="text-sm text-gray-400">Uncalibrated Frames</div>
          </div>
          <div>
            <div className="text-2xl font-bold text-gray-300">{data.total_frames}</div>
            <div className="text-sm text-gray-400">Total Frames</div>
          </div>
        </div>
      </div>

      {/* Calibration Groups */}
      {data.groups.length > 0 && (
        <div className="space-y-3">
          <h3 className="text-lg font-semibold text-gray-200">Calibration Groups</h3>
          {data.groups.map((group, index) => {
            const isExpanded = expandedGroups.has(index);
            const hasFlat = group.flat_set_detail !== null;
            const hasDark = group.dark_set_detail !== null;
            const hasBias = group.bias_set_detail !== null;

            return (
              <div
                key={index}
                className="bg-gray-800 rounded-lg border border-gray-700 overflow-hidden"
              >
                {/* Group Header */}
                <button
                  onClick={() => toggleGroup(index)}
                  className="w-full px-4 py-3 flex items-center justify-between hover:bg-gray-750 transition-colors"
                >
                  <div className="flex items-center gap-3">
                    {isExpanded ? (
                      <ChevronDown className="w-5 h-5 text-gray-400" />
                    ) : (
                      <ChevronRight className="w-5 h-5 text-gray-400" />
                    )}
                    <div className="text-left">
                      <div className="font-medium text-gray-200">
                        Group {index + 1}
                        {group.has_warnings && (
                          <AlertTriangle className="inline w-4 h-4 text-orange-400 ml-2" />
                        )}
                      </div>
                      <div className="text-sm text-gray-400">
                        {group.frame_count} frame{group.frame_count !== 1 ? 's' : ''} •{' '}
                        {[
                          hasFlat && 'Flats',
                          hasDark && 'Darks',
                          hasBias && 'Bias',
                        ]
                          .filter(Boolean)
                          .join(' + ')}
                      </div>
                    </div>
                  </div>
                  <div className="text-sm text-gray-500">
                    {group.frame_ids.length} frame{group.frame_ids.length !== 1 ? 's' : ''}
                  </div>
                </button>

                {/* Group Details - Expanded */}
                {isExpanded && (
                  <div className="px-4 pb-4 space-y-4 bg-gray-850">
                    {/* Calibration Sets Grid */}
                    <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
                      {hasFlat && (
                        <CalibrationSetCard
                          title="Flat Calibration"
                          set={group.flat_set_detail}
                          type="flat"
                        />
                      )}
                      {hasDark && (
                        <CalibrationSetCard
                          title="Dark Calibration"
                          set={group.dark_set_detail}
                          type="dark"
                        />
                      )}
                      {hasBias && (
                        <CalibrationSetCard
                          title="Bias Calibration"
                          set={group.bias_set_detail}
                          type="bias"
                        />
                      )}
                    </div>

                    {/* Frame IDs */}
                    <div className="bg-gray-900 rounded p-3">
                      <div className="text-xs text-gray-400 mb-1">Frame IDs:</div>
                      <div className="text-xs text-gray-300 font-mono">
                        {group.frame_ids.join(', ')}
                      </div>
                    </div>
                  </div>
                )}
              </div>
            );
          })}
        </div>
      )}

      {/* Uncalibrated Frames */}
      {data.uncalibrated_frame_count > 0 && (
        <div className="bg-orange-900/20 border border-orange-700/30 rounded-lg overflow-hidden">
          <button
            onClick={() => setExpandedUncalibrated(!expandedUncalibrated)}
            className="w-full px-4 py-3 flex items-center justify-between hover:bg-orange-900/30 transition-colors"
          >
            <div className="flex items-center gap-3">
              {expandedUncalibrated ? (
                <ChevronDown className="w-5 h-5 text-orange-400" />
              ) : (
                <ChevronRight className="w-5 h-5 text-orange-400" />
              )}
              <div className="text-left">
                <div className="font-medium text-orange-300 flex items-center gap-2">
                  <AlertTriangle className="w-4 h-4" />
                  Uncalibrated Frames
                </div>
                <div className="text-sm text-orange-400">
                  {data.uncalibrated_frame_count} frame{data.uncalibrated_frame_count !== 1 ? 's' : ''} without calibration
                </div>
              </div>
            </div>
          </button>

          {expandedUncalibrated && (
            <div className="px-4 pb-4 bg-orange-900/10">
              <div className="bg-gray-900 rounded p-3">
                <div className="text-xs text-gray-400 mb-1">Frame IDs:</div>
                <div className="text-xs text-gray-300 font-mono">
                  {data.uncalibrated_frame_ids.join(', ')}
                </div>
              </div>
            </div>
          )}
        </div>
      )}

      {/* Empty State */}
      {data.groups.length === 0 && data.uncalibrated_frame_count === 0 && (
        <div className="text-center py-12 text-gray-400">
          <p>No frames found in this frame set.</p>
        </div>
      )}
    </div>
  );
}
