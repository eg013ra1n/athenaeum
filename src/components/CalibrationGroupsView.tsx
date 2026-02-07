import { AlertTriangle, ChevronDown, ChevronRight } from 'lucide-react';
import { useState } from 'react';
import type { CalibrationSetDetail, FrameSetCalibrationGroups, CalibrationWarning } from '../types/models';

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
      ? 'bg-info-muted border-info/30'
      : type === 'dark'
      ? 'bg-purple/20 border-purple/30'
      : 'bg-success-muted border-success/30';

    return (
      <div className={`border rounded p-3 ${bgColor}`}>
        <h4 className="font-medium text-sm mb-2">{title}</h4>
        <div className="grid grid-cols-2 gap-2 text-xs">
          {set.exptime !== null && (
            <div>
              <span className="text-content-muted">Exposure:</span>{' '}
              <span className="text-content">{set.exptime}s</span>
            </div>
          )}
          <div>
            <span className="text-content-muted">Temp:</span>{' '}
            <span className="text-content">
              {formatTemp(set.ccd_temp)}
              {set.temp_min !== set.temp_max && (
                <span className="text-content-muted ml-1">
                  ({formatTemp(set.temp_min)} - {formatTemp(set.temp_max)})
                </span>
              )}
            </span>
          </div>
          {set.gain !== null && (
            <div>
              <span className="text-content-muted">Gain:</span>{' '}
              <span className="text-content">{set.gain}</span>
            </div>
          )}
          {set.binning && (
            <div>
              <span className="text-content-muted">Binning:</span>{' '}
              <span className="text-content">{set.binning}</span>
            </div>
          )}
          <div className="col-span-2">
            <span className="text-content-muted">Date:</span>{' '}
            <span className="text-content">{set.date_display}</span>
          </div>
          <div>
            <span className="text-content-muted">Frames:</span>{' '}
            <span className="text-content">{set.frame_count}</span>
          </div>
        </div>

        {/* Warning display */}
        {warnings.length > 0 && (
          <div className="mt-3 pt-3 border-t border-warning/30 space-y-1">
            {warnings.map((warning, i) => (
              <div key={i} className="flex items-start gap-2 text-xs">
                <AlertTriangle size={14} className="text-warning flex-shrink-0 mt-0.5" />
                <span className="text-warning/90">{warning.message}</span>
              </div>
            ))}
          </div>
        )}
      </div>
    );
  };

  return (
    <div className="space-y-4">
      {/* Summary Stats */}
      <div className="bg-surface-elevated rounded-lg p-4">
        <div className="grid grid-cols-4 gap-4 text-center">
          <div>
            <div className="text-2xl font-bold text-accent">{data.groups.length}</div>
            <div className="text-sm text-content-muted">Calibration Groups</div>
          </div>
          <div>
            <div className="text-2xl font-bold text-success">
              {data.total_frames - data.uncalibrated_frame_count}
            </div>
            <div className="text-sm text-content-muted">Calibrated Frames</div>
          </div>
          <div>
            <div className="text-2xl font-bold text-orange">
              {data.uncalibrated_frame_count}
            </div>
            <div className="text-sm text-content-muted">Uncalibrated Frames</div>
          </div>
          <div>
            <div className="text-2xl font-bold text-content-secondary">{data.total_frames}</div>
            <div className="text-sm text-content-muted">Total Frames</div>
          </div>
        </div>
      </div>

      {/* Calibration Groups */}
      {data.groups.length > 0 && (
        <div className="space-y-3">
          <h3 className="text-lg font-semibold text-content">Calibration Groups</h3>
          {data.groups.map((group, index) => {
            const isExpanded = expandedGroups.has(index);
            const hasFlat = group.flat_set_detail !== null;
            const hasDark = group.dark_set_detail !== null;
            const hasBias = group.bias_set_detail !== null;

            return (
              <div
                key={index}
                className="bg-surface-elevated rounded-lg border border-border overflow-hidden"
              >
                {/* Group Header */}
                <button
                  onClick={() => toggleGroup(index)}
                  className="w-full px-4 py-3 flex items-center justify-between hover:bg-surface-hover transition-colors"
                >
                  <div className="flex items-center gap-3">
                    {isExpanded ? (
                      <ChevronDown className="w-5 h-5 text-content-muted" />
                    ) : (
                      <ChevronRight className="w-5 h-5 text-content-muted" />
                    )}
                    <div className="text-left">
                      <div className="font-medium text-content">
                        Group {index + 1}
                        {group.has_warnings && (
                          <AlertTriangle className="inline w-4 h-4 text-orange ml-2" />
                        )}
                      </div>
                      <div className="text-sm text-content-muted">
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
                  <div className="text-sm text-content-muted">
                    {group.frame_ids.length} frame{group.frame_ids.length !== 1 ? 's' : ''}
                  </div>
                </button>

                {/* Group Details - Expanded */}
                {isExpanded && (
                  <div className="px-4 pb-4 space-y-4 bg-surface">
                    {/* Calibration Sets Grid */}
                    <div className="grid grid-cols-1 md:grid-cols-3 gap-3">
                      {hasFlat && (
                        <CalibrationSetCard
                          title="Flat Calibration"
                          set={group.flat_set_detail}
                          type="flat"
                          warnings={group.flat_warnings}
                        />
                      )}
                      {hasDark && (
                        <CalibrationSetCard
                          title="Dark Calibration"
                          set={group.dark_set_detail}
                          type="dark"
                          warnings={group.dark_warnings}
                        />
                      )}
                      {hasBias && (
                        <CalibrationSetCard
                          title="Bias Calibration"
                          set={group.bias_set_detail}
                          type="bias"
                          warnings={group.bias_warnings}
                        />
                      )}
                    </div>

                    {/* Frame IDs */}
                    <div className="bg-surface rounded p-3">
                      <div className="text-xs text-content-muted mb-1">Frame IDs:</div>
                      <div className="text-xs text-content-secondary font-mono">
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
        <div className="bg-orange/20 border border-orange/30 rounded-lg overflow-hidden">
          <button
            onClick={() => setExpandedUncalibrated(!expandedUncalibrated)}
            className="w-full px-4 py-3 flex items-center justify-between hover:bg-orange/30 transition-colors"
          >
            <div className="flex items-center gap-3">
              {expandedUncalibrated ? (
                <ChevronDown className="w-5 h-5 text-orange" />
              ) : (
                <ChevronRight className="w-5 h-5 text-orange" />
              )}
              <div className="text-left">
                <div className="font-medium text-orange flex items-center gap-2">
                  <AlertTriangle className="w-4 h-4" />
                  Uncalibrated Frames
                </div>
                <div className="text-sm text-orange">
                  {data.uncalibrated_frame_count} frame{data.uncalibrated_frame_count !== 1 ? 's' : ''} without calibration
                </div>
              </div>
            </div>
          </button>

          {expandedUncalibrated && (
            <div className="px-4 pb-4 bg-orange/10">
              <div className="bg-surface rounded p-3">
                <div className="text-xs text-content-muted mb-1">Frame IDs:</div>
                <div className="text-xs text-content-secondary font-mono">
                  {data.uncalibrated_frame_ids.join(', ')}
                </div>
              </div>
            </div>
          )}
        </div>
      )}

      {/* Empty State */}
      {data.groups.length === 0 && data.uncalibrated_frame_count === 0 && (
        <div className="text-center py-12 text-content-muted">
          <p>No frames found in this frame set.</p>
        </div>
      )}
    </div>
  );
}
