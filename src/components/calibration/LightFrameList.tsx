import { useState, useMemo, useCallback } from 'react';
import { ChevronDown, ChevronRight, Copy, Check } from 'lucide-react';
import type { LightFrameWithCalibration } from '../../types/models';
import { StatusIndicator } from './StatusIndicator';

type SortField = 'time' | 'telescop' | 'focallen' | 'exptime';
type SortDirection = 'asc' | 'desc';

interface LightFrameListProps {
  frames: LightFrameWithCalibration[];
  /** Initially collapsed to reduce visual clutter */
  defaultExpanded?: boolean;
  /** File IDs that are in the blackhole (soft-deleted) */
  blackholedFileIds?: Set<number>;
}

/**
 * Format date/time for display.
 */
function formatTime(dateStr: string | null): string {
  if (!dateStr) return '-';
  try {
    return new Date(dateStr).toLocaleTimeString('en-US', {
      hour: '2-digit',
      minute: '2-digit',
      second: '2-digit',
      hour12: false,
    });
  } catch {
    return '-';
  }
}

/**
 * Sortable column header component.
 */
function SortableHeader({
  field,
  label,
  currentSort,
  currentDirection,
  onSort,
  align = 'left',
}: {
  field: SortField;
  label: string;
  currentSort: SortField | null;
  currentDirection: SortDirection;
  onSort: (field: SortField) => void;
  align?: 'left' | 'center' | 'right';
}) {
  const isActive = currentSort === field;

  const alignmentClass = {
    left: '',
    center: 'w-full justify-center',
    right: 'ml-auto',
  }[align];

  return (
    <button
      onClick={() => onSort(field)}
      className={`
        flex items-center gap-1 text-sm font-semibold text-content-secondary
        hover:text-content transition-colors
        ${alignmentClass}
      `}
    >
      {label}
      {isActive && (
        <span className="text-accent">
          {currentDirection === 'asc' ? '↑' : '↓'}
        </span>
      )}
    </button>
  );
}

/**
 * Copy button with feedback.
 */
function CopyButton({ text }: { text: string }) {
  const [copied, setCopied] = useState(false);

  const handleCopy = useCallback(async (e: React.MouseEvent) => {
    e.stopPropagation(); // Prevent row toggle
    try {
      await navigator.clipboard.writeText(text);
      setCopied(true);
      setTimeout(() => setCopied(false), 2000);
    } catch (err) {
      console.error('Failed to copy:', err);
    }
  }, [text]);

  return (
    <button
      onClick={handleCopy}
      className="
        inline-flex items-center gap-1 px-2 py-1
        text-xs text-content-muted hover:text-content
        bg-surface-hover hover:bg-surface-hover
        rounded transition-colors
      "
      title={text}
    >
      {copied ? (
        <>
          <Check size={14} className="text-success" />
          <span className="text-success">Copied</span>
        </>
      ) : (
        <>
          <Copy size={14} />
          <span>Copy</span>
        </>
      )}
    </button>
  );
}

/**
 * Accessible light frame list with Sessions-style table.
 * Features:
 * - Collapsible to reduce information overload
 * - Proper table structure with headers
 * - Sortable columns
 * - Status indicators with full labels
 * - Minimum 14px text for all content
 * - Columns: Time, Telescope, Focal Length, Exposure, Binning
 * - Expanded details: File path (with copy), Frame ID
 */
export function LightFrameList({ frames, defaultExpanded = false, blackholedFileIds = new Set() }: LightFrameListProps) {
  const [isExpanded, setIsExpanded] = useState(defaultExpanded);
  const [sortField, setSortField] = useState<SortField | null>(null);
  const [sortDirection, setSortDirection] = useState<SortDirection>('asc');
  const [expandedFrames, setExpandedFrames] = useState<Set<number>>(new Set());

  const handleSort = (field: SortField) => {
    if (sortField === field) {
      setSortDirection(prev => prev === 'asc' ? 'desc' : 'asc');
    } else {
      setSortField(field);
      setSortDirection('asc');
    }
  };

  const toggleFrameExpansion = (frameId: number) => {
    setExpandedFrames(prev => {
      const next = new Set(prev);
      if (next.has(frameId)) {
        next.delete(frameId);
      } else {
        next.add(frameId);
      }
      return next;
    });
  };

  const sortedFrames = useMemo(() => {
    if (!sortField) return frames;

    return [...frames].sort((a, b) => {
      let comparison = 0;

      switch (sortField) {
        case 'time': {
          const timeA = a.date_obs ? new Date(a.date_obs).getTime() : 0;
          const timeB = b.date_obs ? new Date(b.date_obs).getTime() : 0;
          comparison = timeA - timeB;
          break;
        }
        case 'telescop':
          comparison = (a.telescop ?? '').localeCompare(b.telescop ?? '');
          break;
        case 'focallen':
          comparison = (a.focallen ?? 0) - (b.focallen ?? 0);
          break;
        case 'exptime':
          comparison = (a.exptime ?? 0) - (b.exptime ?? 0);
          break;
      }

      return sortDirection === 'asc' ? comparison : -comparison;
    });
  }, [frames, sortField, sortDirection]);

  return (
    <div className="mt-6">
      {/* Collapsible header */}
      <button
        onClick={() => setIsExpanded(!isExpanded)}
        className="
          w-full flex items-center gap-3
          text-left py-2 px-1
          hover:bg-surface-elevated/50 rounded-lg
          focus:outline-none focus-visible:ring-2 focus-visible:ring-accent
          transition-colors
        "
        aria-expanded={isExpanded}
        aria-controls="light-frames-table"
      >
        {isExpanded ? (
          <ChevronDown size={20} className="text-content-muted" aria-hidden="true" />
        ) : (
          <ChevronRight size={20} className="text-content-muted" aria-hidden="true" />
        )}
        <h3 className="text-lg font-semibold text-content">
          Light Frames
        </h3>
        <span className="text-sm text-content-muted">
          ({frames.length} frame{frames.length !== 1 ? 's' : ''})
        </span>
      </button>

      {/* Expandable table */}
      {isExpanded && (
        <div
          id="light-frames-table"
          className="mt-3 border border-border rounded-xl overflow-hidden"
        >
          <table className="w-full" role="table">
            <thead className="bg-surface">
              <tr>
                <th scope="col" className="w-24 px-4 py-3 text-left">
                  <SortableHeader
                    field="time"
                    label="Time"
                    currentSort={sortField}
                    currentDirection={sortDirection}
                    onSort={handleSort}
                  />
                </th>
                <th scope="col" className="w-20 px-4 py-3 text-center text-sm font-semibold text-content-secondary">
                  Calibration
                </th>
                <th scope="col" className="px-4 py-3">
                  <SortableHeader
                    field="telescop"
                    label="Telescope"
                    currentSort={sortField}
                    currentDirection={sortDirection}
                    onSort={handleSort}
                    align="center"
                  />
                </th>
                <th scope="col" className="px-4 py-3">
                  <SortableHeader
                    field="focallen"
                    label="Focal Len"
                    currentSort={sortField}
                    currentDirection={sortDirection}
                    onSort={handleSort}
                    align="center"
                  />
                </th>
                <th scope="col" className="px-4 py-3">
                  <SortableHeader
                    field="exptime"
                    label="Exposure"
                    currentSort={sortField}
                    currentDirection={sortDirection}
                    onSort={handleSort}
                    align="center"
                  />
                </th>
                <th scope="col" className="px-4 py-3 text-center text-sm font-semibold text-content-secondary">
                  Temp
                </th>
                <th scope="col" className="w-20 px-2 py-3 text-center text-sm font-semibold text-content-secondary">
                  Path
                </th>
              </tr>
            </thead>
            <tbody className="divide-y divide-border">
              {sortedFrames.map((frame, idx) => {
                const isFrameExpanded = expandedFrames.has(frame.frame_id);
                const isBlackholed = blackholedFileIds.has(frame.file_id);

                return (
                  <>
                    <tr
                      key={frame.frame_id}
                      onClick={() => toggleFrameExpansion(frame.frame_id)}
                      className={`
                        ${idx % 2 === 0 ? 'bg-surface-elevated' : 'bg-surface'}
                        hover:bg-surface-hover cursor-pointer transition-colors
                        ${isBlackholed ? 'opacity-50' : ''}
                      `}
                    >
                      <td className={`w-24 px-4 py-3 text-sm text-content-secondary ${isBlackholed ? 'line-through' : ''}`}>
                        {formatTime(frame.date_obs)}
                      </td>
                      <td className="w-20 px-4 py-3 text-center">
                        <StatusIndicator status={frame.calibration_status} compact />
                      </td>
                      <td className={`px-4 py-3 text-sm text-content-secondary text-center ${isBlackholed ? 'line-through' : ''}`}>
                        {frame.telescop ?? '-'}
                      </td>
                      <td className={`px-4 py-3 text-sm text-content-secondary text-center ${isBlackholed ? 'line-through' : ''}`}>
                        {frame.focallen !== null ? `${frame.focallen}mm` : '-'}
                      </td>
                      <td className={`px-4 py-3 text-sm text-content-secondary text-center ${isBlackholed ? 'line-through' : ''}`}>
                        {frame.exptime !== null ? `${frame.exptime}s` : '-'}
                      </td>
                      <td className={`px-4 py-3 text-sm text-content-secondary text-center ${isBlackholed ? 'line-through' : ''}`}>
                        {frame.ccd_temp !== null ? `${frame.ccd_temp.toFixed(1)}°C` : '-'}
                      </td>
                      <td className="w-20 px-2 py-3 text-center">
                        <CopyButton text={frame.file_path} />
                      </td>
                    </tr>

                    {/* Expanded frame details */}
                    {isFrameExpanded && (
                      <tr key={`${frame.frame_id}-details`} className="bg-surface border-t border-border">
                        <td colSpan={7} className="px-4 py-2">
                          <div className="flex flex-wrap items-center gap-x-6 gap-y-1 text-sm">
                            <div className="flex items-center gap-2">
                              <span className="text-content-muted">Gain:</span>
                              <span className="text-content">
                                {frame.gain !== null ? frame.gain : '-'}
                              </span>
                            </div>
                            <div className="flex items-center gap-2">
                              <span className="text-content-muted">Offset:</span>
                              <span className="text-content">
                                {frame.offset !== null ? frame.offset : '-'}
                              </span>
                            </div>
                            <div className="flex items-center gap-2">
                              <span className="text-content-muted">Binning:</span>
                              <span className="text-content">{frame.binning ?? '-'}</span>
                            </div>
                            <div className="flex items-center gap-2">
                              <span className="text-content-muted">Software:</span>
                              <span className="text-content">{frame.swcreate ?? '-'}</span>
                            </div>
                            <div className="border-l border-border pl-6 flex items-center gap-4">
                              <span className="text-content-muted text-xs uppercase tracking-wide">Linked:</span>
                              <div className="flex items-center gap-2">
                                <span className="text-content-muted">Flat:</span>
                                <span className={frame.calibration_status.flat_set_id ? 'text-success' : 'text-content-muted'}>
                                  {frame.calibration_status.flat_set_id ? `#${frame.calibration_status.flat_set_id}` : 'None'}
                                </span>
                              </div>
                              <div className="flex items-center gap-2">
                                <span className="text-content-muted">Dark:</span>
                                <span className={frame.calibration_status.dark_set_id ? 'text-success' : 'text-content-muted'}>
                                  {frame.calibration_status.dark_set_id ? `#${frame.calibration_status.dark_set_id}` : 'None'}
                                </span>
                              </div>
                              {frame.calibration_status.bias_set_id && (
                                <div className="flex items-center gap-2">
                                  <span className="text-content-muted">Bias:</span>
                                  <span className="text-success">#{frame.calibration_status.bias_set_id}</span>
                                </div>
                              )}
                            </div>
                          </div>
                        </td>
                      </tr>
                    )}
                  </>
                );
              })}
            </tbody>
          </table>

          {/* Empty state */}
          {frames.length === 0 && (
            <div className="px-4 py-8 text-center text-content-muted">
              <p className="text-sm">No light frames in this group</p>
            </div>
          )}
        </div>
      )}

      {/* Summary when collapsed */}
      {!isExpanded && frames.length > 0 && (
        <div className="mt-2 px-4 py-3 bg-surface-elevated/50 rounded-lg">
          <FrameSummary frames={frames} />
        </div>
      )}
    </div>
  );
}

/**
 * Summary view of frame calibration status.
 */
function FrameSummary({ frames }: { frames: LightFrameWithCalibration[] }) {
  const stats = frames.reduce(
    (acc, frame) => {
      const status = frame.calibration_status;
      if (status.has_flats && status.has_darks) {
        acc.complete++;
      } else if (status.has_flats || status.has_darks || status.has_bias) {
        acc.partial++;
      } else {
        acc.none++;
      }
      if (status.flats_warning || status.darks_warning || status.bias_warning) {
        acc.warnings++;
      }
      return acc;
    },
    { complete: 0, partial: 0, none: 0, warnings: 0 }
  );

  return (
    <div className="flex items-center gap-6 text-sm">
      <div className="flex items-center gap-2">
        <span className="w-3 h-3 rounded-full bg-success" aria-hidden="true" />
        <span className="text-content-secondary">
          <span className="font-medium text-content">{stats.complete}</span> complete
        </span>
      </div>
      <div className="flex items-center gap-2">
        <span className="w-3 h-3 rounded-full bg-warning" aria-hidden="true" />
        <span className="text-content-secondary">
          <span className="font-medium text-content">{stats.partial}</span> partial
        </span>
      </div>
      <div className="flex items-center gap-2">
        <span className="w-3 h-3 rounded-full bg-error" aria-hidden="true" />
        <span className="text-content-secondary">
          <span className="font-medium text-content">{stats.none}</span> none
        </span>
      </div>
      {stats.warnings > 0 && (
        <div className="flex items-center gap-2">
          <span className="w-3 h-3 rounded-full bg-warning" aria-hidden="true" />
          <span className="text-warning">
            <span className="font-medium">{stats.warnings}</span> with warnings
          </span>
        </div>
      )}
    </div>
  );
}
