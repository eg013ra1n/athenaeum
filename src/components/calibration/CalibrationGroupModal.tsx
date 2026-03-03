import { useState, useMemo, useCallback, useEffect } from 'react';
import { X, Settings, AlertTriangle } from 'lucide-react';
import { api } from '../../api';
import type {
  CalibrationSetWithFrameCount,
  FileWithFrame,
} from '../../types/models';
import type { EnrichedLightFrame } from './LightsAnalysisTable';
import type { FlatGroupData, DarkOnlyGroupData } from './CalibrationGroupCard';
import { typeColors, subCalTypeColors } from './CalibrationSetsTable';
import { MatchBadges, extractLightParams } from './MatchBadges';
import { CalibrationLightsTable } from './CalibrationLightsTable';
import { SubCalibrationModal } from '../SubCalibrationModal';

interface CalibrationGroupModalProps {
  type: 'flat' | 'dark';
  data: FlatGroupData | DarkOnlyGroupData;
  allLightFrames: EnrichedLightFrame[];
  onClose: () => void;
  onRefresh?: () => void;
  onManualCalibration?: (frameIds: number[]) => void;
}

function formatDateRange(dateStart: string | null, dateEnd: string | null): string {
  if (!dateStart) return '-';
  try {
    const start = new Date(dateStart);
    const startStr = start.toLocaleDateString('en-GB', { day: 'numeric', month: 'short', year: 'numeric' });
    if (!dateEnd || dateStart === dateEnd) return startStr;
    const end = new Date(dateEnd);
    const endStr = end.toLocaleDateString('en-GB', { day: 'numeric', month: 'short', year: 'numeric' });
    return `${startStr} \u2013 ${endStr}`;
  } catch {
    return '-';
  }
}

function formatDateTime(dateStr: string | null): string {
  if (!dateStr) return '-';
  try {
    const d = new Date(dateStr);
    const date = d.toLocaleDateString('en-GB', { day: 'numeric', month: 'short' });
    const time = d.toLocaleTimeString('en-US', { hour: '2-digit', minute: '2-digit', hour12: false });
    return `${date} ${time}`;
  } catch {
    return '-';
  }
}

export function CalibrationGroupModal({
  type,
  data,
  allLightFrames,
  onClose,
  onRefresh,
  onManualCalibration,
}: CalibrationGroupModalProps) {
  const primarySet: CalibrationSetWithFrameCount = type === 'flat'
    ? (data as FlatGroupData).flatSet
    : (data as DarkOnlyGroupData).darkSet;

  const lights: EnrichedLightFrame[] = type === 'flat'
    ? (data as FlatGroupData).lights
    : (data as DarkOnlyGroupData).lights;

  const colors = typeColors[type];
  const lightParams = extractLightParams(allLightFrames);

  // State
  const [calFrames, setCalFrames] = useState<FileWithFrame[] | null>(null);
  const [calFramesLoading, setCalFramesLoading] = useState(false);
  const [selectedFrameIds, setSelectedFrameIds] = useState<Set<number>>(new Set());
  const [subCalModalSetId, setSubCalModalSetId] = useState<number | null>(null);
  const [subCalModalType, setSubCalModalType] = useState<'flat' | 'dark'>('flat');

  // Load calibration set frames
  useEffect(() => {
    if (primarySet.set.id === null) return;
    setCalFramesLoading(true);
    api.invoke<FileWithFrame[]>('get_calibration_set_frames', { setId: primarySet.set.id })
      .then(frames => setCalFrames(frames))
      .catch(err => {
        console.error('Failed to load calibration set frames:', err);
        setCalFrames(null);
      })
      .finally(() => setCalFramesLoading(false));
  }, [primarySet.set.id]);

  // Close on Escape
  useEffect(() => {
    const handleKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') onClose();
    };
    window.addEventListener('keydown', handleKeyDown);
    return () => window.removeEventListener('keydown', handleKeyDown);
  }, [onClose]);

  // Group lights by dark_set_id for the right panel
  const darkSubGroups = useMemo(() => {
    const groups = new Map<number | null, EnrichedLightFrame[]>();
    for (const frame of lights) {
      const darkId = frame.calibration_status.dark_set_id;
      if (!groups.has(darkId)) {
        groups.set(darkId, []);
      }
      groups.get(darkId)!.push(frame);
    }
    // Sort: non-null dark IDs first, then null
    const entries = [...groups.entries()].sort((a, b) => {
      if (a[0] === null && b[0] !== null) return 1;
      if (a[0] !== null && b[0] === null) return -1;
      return (a[0] ?? 0) - (b[0] ?? 0);
    });
    return entries;
  }, [lights]);

  // Find dark set details from the hierarchy data for sub-group headers
  const getDarkSetInfo = useCallback((darkSetId: number | null) => {
    if (darkSetId === null) return null;
    for (const frame of lights) {
      if (frame.calibration_status.dark_set_id === darkSetId) {
        // Try to find it in the primary set's sub_calibration
        const subCal = primarySet.sub_calibration?.find(
          sc => sc.calibration_type === 'Dark' && sc.set.id === darkSetId
        );
        if (subCal) return subCal.set;
      }
    }
    return null;
  }, [lights, primarySet]);

  const handleExcludeFromGroup = useCallback(async () => {
    if (selectedFrameIds.size === 0) return;
    try {
      await api.invoke('manual_assign_calibration', {
        frameIds: [...selectedFrameIds],
        calibrationSetId: null,
        calibrationType: type === 'flat' ? 'Flat' : 'Dark',
      });
      setSelectedFrameIds(new Set());
      onRefresh?.();
      onClose();
    } catch (err) {
      console.error('Failed to exclude frames from group:', err);
    }
  }, [selectedFrameIds, type, onRefresh, onClose]);

  const handleReassign = useCallback(() => {
    if (selectedFrameIds.size === 0 || !onManualCalibration) return;
    onManualCalibration([...selectedFrameIds]);
  }, [selectedFrameIds, onManualCalibration]);

  const handleSubCalApply = useCallback(() => {
    onRefresh?.();
  }, [onRefresh]);

  return (
    <>
      {/* Backdrop */}
      <div className="fixed inset-0 bg-black/60 z-50" onClick={onClose} />

      {/* Modal */}
      <div className="fixed inset-0 z-50 flex items-center justify-center p-4">
        <div
          className="w-full max-w-6xl max-h-[90vh] bg-surface rounded-xl border border-border shadow-2xl flex flex-col"
          onClick={e => e.stopPropagation()}
        >
          {/* Header */}
          <div className={`flex items-center gap-3 px-5 py-3 border-b border-border border-l-[3px] ${colors.border} rounded-t-xl`}>
            <span className={`px-2 py-0.5 rounded text-xs font-semibold ${colors.badge}`}>
              {type.charAt(0).toUpperCase() + type.slice(1)}
            </span>
            {primarySet.set.id !== null && (
              <span className="text-sm text-content-muted">#{primarySet.set.id}</span>
            )}
            {primarySet.set.is_master && (
              <span className="px-1 py-0.5 text-[10px] font-medium bg-amber-500/20 text-amber-400 border border-amber-500/40 rounded">
                Master
              </span>
            )}
            {type === 'dark' && (
              <span className="px-1 py-0.5 text-[10px] font-medium bg-warning-muted text-warning border border-warning/50 rounded">
                No Flat
              </span>
            )}
            <span className="text-sm text-content">
              {lights.length} light{lights.length !== 1 ? 's' : ''} covered
            </span>
            <button
              onClick={onClose}
              className="ml-auto p-1.5 text-content-muted hover:text-content hover:bg-surface-hover rounded transition-colors"
            >
              <X size={18} />
            </button>
          </div>

          {/* Body: Two panels */}
          <div className="flex flex-1 min-h-0">
            {/* Left Panel */}
            <div className="w-80 flex-shrink-0 border-r border-border overflow-y-auto p-4 space-y-4">
              {/* Set Summary */}
              <section>
                <h4 className="text-xs font-semibold text-content-muted uppercase tracking-wide mb-2">Set Details</h4>
                <div className="space-y-1 text-sm">
                  <div className="flex justify-between">
                    <span className="text-content-muted">Date</span>
                    <span className="text-content">{formatDateRange(primarySet.set.date_start, primarySet.set.date_end)}</span>
                  </div>
                  <div className="flex justify-between">
                    <span className="text-content-muted">Frames</span>
                    <span className="text-content">{primarySet.set.frame_count}</span>
                  </div>
                  {primarySet.set.exptime !== null && (
                    <div className="flex justify-between">
                      <span className="text-content-muted">Exposure</span>
                      <span className="text-content">{primarySet.set.exptime}s</span>
                    </div>
                  )}
                  {primarySet.set.ccd_temp !== null && (
                    <div className="flex justify-between">
                      <span className="text-content-muted">Temp</span>
                      <span className="text-content">{primarySet.set.ccd_temp.toFixed(1)}{'\u00B0'}C</span>
                    </div>
                  )}
                  {primarySet.set.gain !== null && (
                    <div className="flex justify-between">
                      <span className="text-content-muted">Gain</span>
                      <span className="text-content">{primarySet.set.gain}</span>
                    </div>
                  )}
                  {primarySet.set.binning && (
                    <div className="flex justify-between">
                      <span className="text-content-muted">Binning</span>
                      <span className="text-content">{primarySet.set.binning}</span>
                    </div>
                  )}
                </div>
                <div className="mt-2">
                  <MatchBadges set={primarySet.set} warnings={primarySet.warnings} lightParams={lightParams} />
                </div>
              </section>

              {/* Calibration Frames */}
              <section>
                <h4 className="text-xs font-semibold text-content-muted uppercase tracking-wide mb-2">
                  Calibration Frames
                </h4>
                {calFramesLoading ? (
                  <p className="text-xs text-content-muted">Loading...</p>
                ) : calFrames && calFrames.length > 0 ? (
                  <div className="space-y-0.5 max-h-40 overflow-y-auto">
                    {calFrames.map(f => (
                      <div key={f.file.id} className="flex items-center gap-2 text-xs text-content-secondary py-0.5">
                        <span className="text-content-muted">{formatDateTime(f.frame?.date_obs ?? null)}</span>
                        <span className="truncate flex-1 text-content-secondary" title={f.file.filename}>{f.file.filename}</span>
                      </div>
                    ))}
                  </div>
                ) : (
                  <p className="text-xs text-content-muted">No frames loaded</p>
                )}
              </section>

              {/* Sub-calibration */}
              {primarySet.sub_calibration && primarySet.sub_calibration.length > 0 && (
                <section>
                  <h4 className="text-xs font-semibold text-content-muted uppercase tracking-wide mb-2">
                    Sub-Calibration
                  </h4>
                  <div className="space-y-2">
                    {primarySet.sub_calibration.map((sub, i) => {
                      const subColors = subCalTypeColors[sub.calibration_type] ?? { dot: 'bg-content-muted', text: 'text-content' };
                      return (
                        <div key={`${sub.calibration_type}-${sub.set.id ?? i}`} className="flex items-center gap-2">
                          <span className={`w-2 h-2 rounded-full flex-shrink-0 ${subColors.dot}`} />
                          <span className={`text-sm ${subColors.text}`}>{sub.calibration_type}</span>
                          {sub.set.id !== null && (
                            <span className="text-xs text-content-muted">#{sub.set.id}</span>
                          )}
                          <span className="text-xs text-content-muted">{sub.set.frame_count}f</span>
                          {primarySet.set.id !== null && (
                            <button
                              onClick={() => {
                                setSubCalModalSetId(primarySet.set.id!);
                                setSubCalModalType(type);
                              }}
                              className="ml-auto p-1 text-content-muted hover:text-content hover:bg-surface-hover rounded transition-colors"
                              title="Change sub-calibration"
                            >
                              <Settings size={12} />
                            </button>
                          )}
                        </div>
                      );
                    })}
                  </div>
                </section>
              )}

              {/* Warnings */}
              {primarySet.warnings.length > 0 && (
                <section>
                  <h4 className="text-xs font-semibold text-content-muted uppercase tracking-wide mb-2">
                    Warnings
                  </h4>
                  <div className="space-y-1">
                    {primarySet.warnings.map((w, i) => (
                      <div key={i} className="flex items-start gap-1.5 text-xs text-warning">
                        <AlertTriangle size={12} className="flex-shrink-0 mt-0.5" />
                        <span>{w.message}</span>
                      </div>
                    ))}
                  </div>
                </section>
              )}
            </div>

            {/* Right Panel */}
            <div className="flex-1 min-w-0 flex flex-col">
              {/* Lights grouped by dark set */}
              <div className="flex-1 overflow-y-auto p-4 space-y-4">
                {darkSubGroups.map(([darkId, groupFrames]) => {
                  const darkInfo = darkId !== null ? getDarkSetInfo(darkId) : null;

                  return (
                    <div key={darkId ?? 'no-dark'}>
                      {/* Sub-group header */}
                      <div className="bg-surface rounded-lg px-3 py-2 mb-1 flex items-center gap-2">
                        {darkId !== null ? (
                          <>
                            <span className="w-2 h-2 rounded-full bg-purple flex-shrink-0" />
                            <span className="text-sm text-purple font-medium">
                              Dark Set #{darkId}
                            </span>
                            {darkInfo && (
                              <span className="text-xs text-content-muted">
                                {darkInfo.frame_count} frame{darkInfo.frame_count !== 1 ? 's' : ''}
                                {darkInfo.exptime !== null && `, ${darkInfo.exptime}s`}
                                {darkInfo.ccd_temp !== null && `, ${darkInfo.ccd_temp.toFixed(1)}\u00B0C`}
                              </span>
                            )}
                          </>
                        ) : (
                          <>
                            <span className="w-2 h-2 rounded-full bg-warning flex-shrink-0" />
                            <span className="text-sm text-warning font-medium">No Dark</span>
                          </>
                        )}
                        <span className="ml-auto text-xs text-content-muted">
                          {groupFrames.length} light{groupFrames.length !== 1 ? 's' : ''}
                        </span>
                      </div>

                      {/* Lights table for this sub-group */}
                      <div className="border border-border rounded-lg overflow-hidden">
                        <CalibrationLightsTable
                          frames={groupFrames}
                          selectedFrameIds={selectedFrameIds}
                          onSelectionChange={setSelectedFrameIds}
                          showCalibrationStatus={false}
                          compact
                        />
                      </div>
                    </div>
                  );
                })}
              </div>

              {/* Bottom action bar when frames selected */}
              {selectedFrameIds.size > 0 && (
                <div className="border-t border-border px-4 py-2 bg-surface-elevated flex items-center gap-2">
                  <span className="text-sm text-content-secondary">
                    {selectedFrameIds.size} frame{selectedFrameIds.size !== 1 ? 's' : ''} selected
                  </span>
                  <div className="ml-auto flex items-center gap-2">
                    <button
                      onClick={handleExcludeFromGroup}
                      className="text-xs px-3 py-1.5 bg-error/20 text-error hover:bg-error/30 rounded transition-colors"
                    >
                      Exclude from {type} group
                    </button>
                    {onManualCalibration && (
                      <button
                        onClick={handleReassign}
                        className="text-xs px-3 py-1.5 bg-accent hover:bg-accent-hover text-white rounded transition-colors"
                      >
                        Reassign
                      </button>
                    )}
                    <button
                      onClick={() => setSelectedFrameIds(new Set())}
                      className="text-xs px-2 py-1.5 text-content-muted hover:text-content transition-colors"
                    >
                      Clear
                    </button>
                  </div>
                </div>
              )}
            </div>
          </div>
        </div>
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
    </>
  );
}
