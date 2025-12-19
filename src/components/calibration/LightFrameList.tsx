import { useState, useMemo, useCallback } from 'react';
import { ChevronDown, ChevronRight, Copy, Check } from 'lucide-react';
import type { LightFrameWithCalibration } from '../../types/models';
import { StatusIndicator } from './StatusIndicator';

type SortField = 'time' | 'telescop' | 'focallen' | 'exptime' | 'binning';
type SortDirection = 'asc' | 'desc';

interface LightFrameListProps {
  frames: LightFrameWithCalibration[];
  /** Initially collapsed to reduce visual clutter */
  defaultExpanded?: boolean;
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
  align?: 'left' | 'right';
}) {
  const isActive = currentSort === field;

  return (
    <button
      onClick={() => onSort(field)}
      className={`
        flex items-center gap-1 text-sm font-semibold text-gray-300
        hover:text-gray-100 transition-colors
        ${align === 'right' ? 'ml-auto' : ''}
      `}
    >
      {label}
      {isActive && (
        <span className="text-blue-400">
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
        text-xs text-gray-400 hover:text-gray-100
        bg-gray-700 hover:bg-gray-600
        rounded transition-colors
      "
      title="Copy to clipboard"
    >
      {copied ? (
        <>
          <Check size={14} className="text-emerald-400" />
          <span className="text-emerald-400">Copied</span>
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
export function LightFrameList({ frames, defaultExpanded = false }: LightFrameListProps) {
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
        case 'binning':
          comparison = (a.binning ?? '').localeCompare(b.binning ?? '');
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
          hover:bg-gray-800/50 rounded-lg
          focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500
          transition-colors
        "
        aria-expanded={isExpanded}
        aria-controls="light-frames-table"
      >
        {isExpanded ? (
          <ChevronDown size={20} className="text-gray-400" aria-hidden="true" />
        ) : (
          <ChevronRight size={20} className="text-gray-400" aria-hidden="true" />
        )}
        <h3 className="text-lg font-semibold text-gray-100">
          Light Frames
        </h3>
        <span className="text-sm text-gray-400">
          ({frames.length} frame{frames.length !== 1 ? 's' : ''})
        </span>
      </button>

      {/* Expandable table */}
      {isExpanded && (
        <div
          id="light-frames-table"
          className="mt-3 border border-gray-600 rounded-xl overflow-hidden"
        >
          <table className="w-full" role="table">
            <thead className="bg-gray-900">
              <tr>
                <th scope="col" className="px-4 py-3 text-left">
                  <SortableHeader
                    field="time"
                    label="Time"
                    currentSort={sortField}
                    currentDirection={sortDirection}
                    onSort={handleSort}
                  />
                </th>
                <th scope="col" className="px-4 py-3 text-left">
                  <SortableHeader
                    field="telescop"
                    label="Telescope"
                    currentSort={sortField}
                    currentDirection={sortDirection}
                    onSort={handleSort}
                  />
                </th>
                <th scope="col" className="px-4 py-3 text-right">
                  <SortableHeader
                    field="focallen"
                    label="Focal Len"
                    currentSort={sortField}
                    currentDirection={sortDirection}
                    onSort={handleSort}
                    align="right"
                  />
                </th>
                <th scope="col" className="px-4 py-3 text-right">
                  <SortableHeader
                    field="exptime"
                    label="Exposure"
                    currentSort={sortField}
                    currentDirection={sortDirection}
                    onSort={handleSort}
                    align="right"
                  />
                </th>
                <th scope="col" className="px-4 py-3 text-center">
                  <SortableHeader
                    field="binning"
                    label="Binning"
                    currentSort={sortField}
                    currentDirection={sortDirection}
                    onSort={handleSort}
                  />
                </th>
                <th scope="col" className="px-4 py-3 text-left text-sm font-semibold text-gray-300">
                  Calibration
                </th>
              </tr>
            </thead>
            <tbody className="divide-y divide-gray-700">
              {sortedFrames.map((frame, idx) => {
                const isFrameExpanded = expandedFrames.has(frame.frame_id);

                return (
                  <>
                    <tr
                      key={frame.frame_id}
                      onClick={() => toggleFrameExpansion(frame.frame_id)}
                      className={`
                        ${idx % 2 === 0 ? 'bg-gray-800' : 'bg-gray-850'}
                        hover:bg-gray-700 cursor-pointer transition-colors
                      `}
                    >
                      <td className="px-4 py-3 text-sm text-gray-300">
                        {formatTime(frame.date_obs)}
                      </td>
                      <td className="px-4 py-3 text-sm text-gray-300">
                        {frame.telescop ?? '-'}
                      </td>
                      <td className="px-4 py-3 text-sm text-gray-300 text-right">
                        {frame.focallen !== null ? `${frame.focallen}mm` : '-'}
                      </td>
                      <td className="px-4 py-3 text-sm text-gray-300 text-right">
                        {frame.exptime !== null ? `${frame.exptime}s` : '-'}
                      </td>
                      <td className="px-4 py-3 text-sm text-gray-300 text-center">
                        {frame.binning ?? '-'}
                      </td>
                      <td className="px-4 py-3">
                        <StatusIndicator status={frame.calibration_status} compact />
                      </td>
                    </tr>

                    {/* Expanded frame details */}
                    {isFrameExpanded && (
                      <tr key={`${frame.frame_id}-details`} className="bg-gray-900 border-t border-gray-700">
                        <td colSpan={6} className="px-4 py-2">
                          <div className="flex flex-wrap items-center gap-x-6 gap-y-1 text-sm">
                            <div className="flex items-center gap-2">
                              <span className="text-gray-400">Temp:</span>
                              <span className="text-gray-100">
                                {frame.ccd_temp !== null ? `${frame.ccd_temp.toFixed(1)}°C` : '-'}
                              </span>
                            </div>
                            <div className="flex items-center gap-2">
                              <span className="text-gray-400">Software:</span>
                              <span className="text-gray-100">{frame.swcreate ?? '-'}</span>
                            </div>
                            <div className="flex items-center gap-2 min-w-0 flex-1 overflow-hidden">
                              <span className="text-gray-400 flex-shrink-0">Path:</span>
                              <span className="text-gray-100 font-mono truncate" title={frame.file_path}>
                                {frame.file_path}
                              </span>
                              <span className="flex-shrink-0">
                                <CopyButton text={frame.file_path} />
                              </span>
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
            <div className="px-4 py-8 text-center text-gray-400">
              <p className="text-sm">No light frames in this group</p>
            </div>
          )}
        </div>
      )}

      {/* Summary when collapsed */}
      {!isExpanded && frames.length > 0 && (
        <div className="mt-2 px-4 py-3 bg-gray-800/50 rounded-lg">
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
        <span className="w-3 h-3 rounded-full bg-emerald-500" aria-hidden="true" />
        <span className="text-gray-300">
          <span className="font-medium text-gray-100">{stats.complete}</span> complete
        </span>
      </div>
      <div className="flex items-center gap-2">
        <span className="w-3 h-3 rounded-full bg-amber-500" aria-hidden="true" />
        <span className="text-gray-300">
          <span className="font-medium text-gray-100">{stats.partial}</span> partial
        </span>
      </div>
      <div className="flex items-center gap-2">
        <span className="w-3 h-3 rounded-full bg-red-500" aria-hidden="true" />
        <span className="text-gray-300">
          <span className="font-medium text-gray-100">{stats.none}</span> none
        </span>
      </div>
      {stats.warnings > 0 && (
        <div className="flex items-center gap-2">
          <span className="w-3 h-3 rounded-full bg-amber-400" aria-hidden="true" />
          <span className="text-amber-300">
            <span className="font-medium">{stats.warnings}</span> with warnings
          </span>
        </div>
      )}
    </div>
  );
}
