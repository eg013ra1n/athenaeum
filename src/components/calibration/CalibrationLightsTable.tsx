import { useState, useMemo, useCallback, useRef, useEffect } from 'react';
import type { EnrichedLightFrame } from './LightsAnalysisTable';
import { StatusIndicator } from './StatusIndicator';

type SortField = 'date' | 'camera' | 'filter' | 'exptime' | 'gain' | 'offset' | 'binning' | 'temp' | 'focallen';
type SortDirection = 'asc' | 'desc';

export interface CalibrationLightsTableProps {
  frames: EnrichedLightFrame[];
  selectedFrameIds: Set<number>;
  onSelectionChange: (selectedIds: Set<number>) => void;
  blackholedFileIds?: Set<number>;
  /** Show calibration status column (F/D dots). Default: true */
  showCalibrationStatus?: boolean;
  /** Compact mode with smaller padding for modal embedding */
  compact?: boolean;
}

function formatDateTime(dateStr: string | null): string {
  if (!dateStr) return '-';
  try {
    const d = new Date(dateStr);
    const year = d.getFullYear();
    const month = String(d.getMonth() + 1).padStart(2, '0');
    const day = String(d.getDate()).padStart(2, '0');
    const hours = String(d.getHours()).padStart(2, '0');
    const minutes = String(d.getMinutes()).padStart(2, '0');
    const seconds = String(d.getSeconds()).padStart(2, '0');
    return `${year}-${month}-${day} ${hours}:${minutes}:${seconds}`;
  } catch {
    return '-';
  }
}

function SortableHeader({
  field,
  label,
  currentSort,
  currentDirection,
  onSort,
  align = 'center',
}: {
  field: SortField;
  label: string;
  currentSort: SortField | null;
  currentDirection: SortDirection;
  onSort: (field: SortField) => void;
  align?: 'left' | 'center';
}) {
  const isActive = currentSort === field;

  return (
    <button
      onClick={() => onSort(field)}
      className={`flex items-center gap-1 text-xs font-semibold text-content-secondary hover:text-content transition-colors ${align === 'left' ? '' : 'w-full justify-center'}`}
    >
      {label}
      {isActive && (
        <span className="text-accent">
          {currentDirection === 'asc' ? '\u2191' : '\u2193'}
        </span>
      )}
    </button>
  );
}

function StyledCheckbox({
  checked,
  indeterminate,
  onChange,
}: {
  checked: boolean;
  indeterminate?: boolean;
  onChange: () => void;
}) {
  const ref = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (ref.current) {
      ref.current.indeterminate = indeterminate ?? false;
    }
  }, [indeterminate]);

  return (
    <input
      ref={ref}
      type="checkbox"
      checked={checked}
      onChange={onChange}
      className="rounded border-border text-accent focus:ring-accent cursor-pointer"
    />
  );
}

export function CalibrationLightsTable({
  frames,
  selectedFrameIds,
  onSelectionChange,
  blackholedFileIds = new Set(),
  showCalibrationStatus = true,
  compact = false,
}: CalibrationLightsTableProps) {
  const [sortField, setSortField] = useState<SortField | null>('date');
  const [sortDirection, setSortDirection] = useState<SortDirection>('asc');

  const handleSort = useCallback((field: SortField) => {
    if (sortField === field) {
      setSortDirection(prev => prev === 'asc' ? 'desc' : 'asc');
    } else {
      setSortField(field);
      setSortDirection('asc');
    }
  }, [sortField]);

  const toggleFrame = useCallback((frameId: number) => {
    const next = new Set(selectedFrameIds);
    if (next.has(frameId)) {
      next.delete(frameId);
    } else {
      next.add(frameId);
    }
    onSelectionChange(next);
  }, [selectedFrameIds, onSelectionChange]);

  const toggleAll = useCallback(() => {
    if (selectedFrameIds.size === frames.length && frames.length > 0) {
      onSelectionChange(new Set());
    } else {
      onSelectionChange(new Set(frames.map(f => f.frame_id)));
    }
  }, [selectedFrameIds, frames, onSelectionChange]);

  const allSelected = frames.length > 0 && selectedFrameIds.size === frames.length;
  const someSelected = selectedFrameIds.size > 0 && selectedFrameIds.size < frames.length;

  const sortedFrames = useMemo(() => {
    if (!sortField) return frames;

    return [...frames].sort((a, b) => {
      let comparison = 0;

      switch (sortField) {
        case 'date': {
          const timeA = a.date_obs ? new Date(a.date_obs).getTime() : 0;
          const timeB = b.date_obs ? new Date(b.date_obs).getTime() : 0;
          comparison = timeA - timeB;
          break;
        }
        case 'camera':
          comparison = a.camera.localeCompare(b.camera);
          break;
        case 'filter':
          comparison = (a.filter ?? '').localeCompare(b.filter ?? '');
          break;
        case 'exptime':
          comparison = (a.exptime ?? 0) - (b.exptime ?? 0);
          break;
        case 'gain':
          comparison = (a.gain ?? 0) - (b.gain ?? 0);
          break;
        case 'offset':
          comparison = (a.offset ?? 0) - (b.offset ?? 0);
          break;
        case 'binning':
          comparison = (a.binning ?? '').localeCompare(b.binning ?? '');
          break;
        case 'temp':
          comparison = (a.ccd_temp ?? -999) - (b.ccd_temp ?? -999);
          break;
        case 'focallen':
          comparison = (a.focallen ?? 0) - (b.focallen ?? 0);
          break;
      }

      return sortDirection === 'asc' ? comparison : -comparison;
    });
  }, [frames, sortField, sortDirection]);

  const px = compact ? 'px-1.5' : 'px-2';
  const py = compact ? 'py-1' : 'py-1.5';
  const cellPad = `${px} ${py}`;

  return (
    <div>
      <table className="w-full" role="table">
        <thead className="bg-surface sticky top-0 z-10">
          <tr>
            <th scope="col" className={`w-10 ${cellPad} text-center`}>
              <StyledCheckbox
                checked={allSelected}
                indeterminate={someSelected}
                onChange={toggleAll}
              />
            </th>
            <th scope="col" className={`${cellPad} text-left`}>
              <SortableHeader field="date" label="Date/Time" currentSort={sortField} currentDirection={sortDirection} onSort={handleSort} align="left" />
            </th>
            <th scope="col" className={`${cellPad} text-center`}>
              <SortableHeader field="camera" label="Camera" currentSort={sortField} currentDirection={sortDirection} onSort={handleSort} />
            </th>
            <th scope="col" className={`${cellPad} text-center`}>
              <SortableHeader field="filter" label="Filter" currentSort={sortField} currentDirection={sortDirection} onSort={handleSort} />
            </th>
            <th scope="col" className={`${cellPad} text-center`}>
              <SortableHeader field="exptime" label="Exposure" currentSort={sortField} currentDirection={sortDirection} onSort={handleSort} />
            </th>
            <th scope="col" className={`${cellPad} text-center`}>
              <SortableHeader field="gain" label="Gain" currentSort={sortField} currentDirection={sortDirection} onSort={handleSort} />
            </th>
            <th scope="col" className={`${cellPad} text-center`}>
              <SortableHeader field="offset" label="Offset" currentSort={sortField} currentDirection={sortDirection} onSort={handleSort} />
            </th>
            <th scope="col" className={`${cellPad} text-center`}>
              <SortableHeader field="binning" label="Binning" currentSort={sortField} currentDirection={sortDirection} onSort={handleSort} />
            </th>
            <th scope="col" className={`${cellPad} text-center`}>
              <SortableHeader field="temp" label="Temp" currentSort={sortField} currentDirection={sortDirection} onSort={handleSort} />
            </th>
            <th scope="col" className={`${cellPad} text-center`}>
              <SortableHeader field="focallen" label="Focal Len" currentSort={sortField} currentDirection={sortDirection} onSort={handleSort} />
            </th>
            {showCalibrationStatus && (
              <th scope="col" className={`w-20 ${cellPad} text-center text-xs font-semibold text-content-secondary`}>
                Cal
              </th>
            )}
          </tr>
        </thead>
        <tbody className="divide-y divide-border">
          {sortedFrames.map((frame, idx) => {
            const isBlackholed = blackholedFileIds.has(frame.file_id);
            const isSelected = selectedFrameIds.has(frame.frame_id);
            const strikeClass = isBlackholed ? 'line-through' : '';

            return (
              <tr
                key={frame.frame_id}
                className={`
                  ${idx % 2 === 0 ? 'bg-surface-elevated' : 'bg-surface'}
                  hover:bg-surface-hover transition-colors
                  ${isBlackholed ? 'opacity-50' : ''}
                  ${isSelected ? 'ring-1 ring-inset ring-accent/30' : ''}
                `}
              >
                <td className={`w-10 ${cellPad} text-center`}>
                  <input
                    type="checkbox"
                    checked={isSelected}
                    onChange={() => toggleFrame(frame.frame_id)}
                    className="rounded border-border text-accent focus:ring-accent cursor-pointer"
                  />
                </td>
                <td className={`${cellPad} text-sm font-mono text-content-secondary ${strikeClass}`}>
                  {formatDateTime(frame.date_obs)}
                </td>
                <td className={`${cellPad} text-sm text-content-secondary text-center ${strikeClass}`}>
                  {frame.camera}
                </td>
                <td className={`${cellPad} text-sm text-content-secondary text-center ${strikeClass}`}>
                  {frame.filter ?? '-'}
                </td>
                <td className={`${cellPad} text-sm text-content-secondary text-center ${strikeClass}`}>
                  {frame.exptime !== null ? `${frame.exptime}s` : '-'}
                </td>
                <td className={`${cellPad} text-sm text-content-secondary text-center ${strikeClass}`}>
                  {frame.gain !== null ? frame.gain : '-'}
                </td>
                <td className={`${cellPad} text-sm text-content-secondary text-center ${strikeClass}`}>
                  {frame.offset !== null ? frame.offset : '-'}
                </td>
                <td className={`${cellPad} text-sm text-content-secondary text-center ${strikeClass}`}>
                  {frame.binning ?? '-'}
                </td>
                <td className={`${cellPad} text-sm text-content-secondary text-center ${strikeClass}`}>
                  {frame.ccd_temp !== null ? `${frame.ccd_temp.toFixed(1)}\u00B0C` : '-'}
                </td>
                <td className={`${cellPad} text-sm text-content-secondary text-center ${strikeClass}`}>
                  {frame.focallen !== null ? `${frame.focallen}mm` : '-'}
                </td>
                {showCalibrationStatus && (
                  <td className={`w-20 ${cellPad} text-center`}>
                    <StatusIndicator status={frame.calibration_status} compact />
                  </td>
                )}
              </tr>
            );
          })}
        </tbody>
      </table>

      {frames.length === 0 && (
        <div className="px-4 py-8 text-center text-content-muted">
          <p className="text-sm">No light frames to display</p>
        </div>
      )}
    </div>
  );
}
