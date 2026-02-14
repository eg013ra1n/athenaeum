import { useState, useMemo } from 'react';
import { Trash2, Pencil, Check, X, Star, ChevronUp, ChevronDown, MapPin, Clock, RotateCw } from 'lucide-react';
import type { FramesSetWithCount } from '../types/models';

type SortField = 'name' | 'coordinates' | 'frames' | 'exposure' | 'rotation' | 'date' | 'custom';
type SortDirection = 'asc' | 'desc';

interface ObjectsTableViewProps {
  frameSets: FramesSetWithCount[];
  isMergeMode: boolean;
  isDragging: boolean;
  draggedSetId: number | null;
  dropTargetId: number | null;
  editingSetId: number | null;
  editingName: string;
  onMouseDown: (e: React.MouseEvent, setId: number) => void;
  onView: (setId: number) => void;
  onDelete: (setId: number, name: string | null) => void;
  onStartEditing: (setId: number, name: string | null) => void;
  onSaveRename: (setId: number) => void;
  onCancelEditing: () => void;
  onEditingNameChange: (name: string) => void;
  onMarkAsCustom: (setId: number) => void;
}

export function ObjectsTableView({
  frameSets,
  isMergeMode,
  isDragging,
  draggedSetId,
  dropTargetId,
  editingSetId,
  editingName,
  onMouseDown,
  onView,
  onDelete,
  onStartEditing,
  onSaveRename,
  onCancelEditing,
  onEditingNameChange,
  onMarkAsCustom,
}: ObjectsTableViewProps) {
  const [sortField, setSortField] = useState<SortField>('date');
  const [sortDirection, setSortDirection] = useState<SortDirection>('desc');

  const handleSort = (field: SortField) => {
    if (sortField === field) {
      setSortDirection(prev => prev === 'asc' ? 'desc' : 'asc');
    } else {
      setSortField(field);
      setSortDirection('asc');
    }
  };

  const sortedFrameSets = useMemo(() => {
    return [...frameSets].sort((a, b) => {
      const dir = sortDirection === 'asc' ? 1 : -1;

      switch (sortField) {
        case 'name':
          return dir * (a.frames_set.name || '').localeCompare(b.frames_set.name || '');
        case 'coordinates':
          return dir * (a.frames_set.objctra || '').localeCompare(b.frames_set.objctra || '');
        case 'frames':
          return dir * (a.member_count - b.member_count);
        case 'exposure':
          return dir * ((a.frames_set.total_exp_time || 0) - (b.frames_set.total_exp_time || 0));
        case 'rotation':
          return dir * ((a.frames_set.avg_rotation ?? -999) - (b.frames_set.avg_rotation ?? -999));
        case 'date':
          return dir * (a.frames_set.date_obs_start || '').localeCompare(b.frames_set.date_obs_start || '');
        case 'custom':
          return dir * ((a.frames_set.is_custom ? 1 : 0) - (b.frames_set.is_custom ? 1 : 0));
        default:
          return 0;
      }
    });
  }, [frameSets, sortField, sortDirection]);

  const formatExposureTime = (seconds: number | null) => {
    if (!seconds) return '-';
    const hours = (seconds / 3600).toFixed(1);
    return `${hours}h`;
  };

  const formatRotation = (avg: number | null, min: number | null, max: number | null) => {
    if (avg == null) return '-';
    if (min != null && max != null && Math.abs(max - min) >= 1) {
      return `${min.toFixed(0)}° – ${max.toFixed(0)}°`;
    }
    return `${avg.toFixed(0)}°`;
  };

  const formatDateRange = (start: string | null, end: string | null) => {
    if (!start) return '-';
    const startDate = new Date(start).toLocaleDateString();
    if (!end || start === end) return startDate;
    const endDate = new Date(end).toLocaleDateString();
    return `${startDate} - ${endDate}`;
  };

  const SortIcon = ({ field }: { field: SortField }) => {
    if (sortField !== field) return null;
    return sortDirection === 'asc'
      ? <ChevronUp size={14} className="inline ml-1" />
      : <ChevronDown size={14} className="inline ml-1" />;
  };

  const headerClass = "px-4 py-3 text-left text-xs font-medium text-content-muted uppercase tracking-wider cursor-pointer hover:text-content transition-colors select-none";

  return (
    <div className={`overflow-x-auto bg-surface-elevated rounded-lg border border-border ${isDragging || isMergeMode ? 'select-none' : ''}`}>
      <table className="w-full">
        <thead className="bg-surface">
          <tr>
            <th className={headerClass} onClick={() => handleSort('name')}>
              Name <SortIcon field="name" />
            </th>
            <th className={`${headerClass} hidden md:table-cell`} onClick={() => handleSort('coordinates')}>
              <MapPin size={12} className="inline mr-1" />
              Coordinates <SortIcon field="coordinates" />
            </th>
            <th className={headerClass} onClick={() => handleSort('frames')}>
              Frames <SortIcon field="frames" />
            </th>
            <th className={`${headerClass} hidden lg:table-cell`} onClick={() => handleSort('exposure')}>
              <Clock size={12} className="inline mr-1" />
              Exposure <SortIcon field="exposure" />
            </th>
            <th className={`${headerClass} hidden lg:table-cell`} onClick={() => handleSort('rotation')}>
              <RotateCw size={12} className="inline mr-1" />
              Rotation <SortIcon field="rotation" />
            </th>
            <th className={`${headerClass} hidden md:table-cell`} onClick={() => handleSort('date')}>
              Date Range <SortIcon field="date" />
            </th>
            <th className={headerClass} onClick={() => handleSort('custom')}>
              <Star size={12} className="inline" /> <SortIcon field="custom" />
            </th>
            <th className="px-4 py-3 text-center text-xs font-medium text-content-muted uppercase tracking-wider">
              Actions
            </th>
          </tr>
        </thead>
        <tbody className="divide-y divide-border">
          {sortedFrameSets.map(({ frames_set, member_count }, index) => {
            const isBeingDragged = isDragging && draggedSetId === frames_set.id;
            const isDropTarget = dropTargetId === frames_set.id;

            return (
              <tr
                key={frames_set.id}
                data-set-id={frames_set.id}
                onMouseDown={(e) => !editingSetId && isMergeMode && onMouseDown(e, frames_set.id!)}
                className={`
                  ${index % 2 === 0 ? 'bg-surface-elevated' : 'bg-surface-elevated/50'}
                  ${isBeingDragged ? 'opacity-40 bg-info-muted' : ''}
                  ${isDropTarget ? 'bg-success-muted ring-2 ring-success' : ''}
                  ${!editingSetId && isMergeMode && !isDragging ? 'cursor-grab' : ''}
                  ${isDragging ? 'cursor-grabbing' : ''}
                  hover:bg-surface-hover/50 transition-colors
                `}
              >
                {/* Name */}
                <td className="px-4 py-3">
                  {editingSetId === frames_set.id ? (
                    <div className="flex items-center gap-2">
                      <input
                        type="text"
                        value={editingName}
                        onChange={(e) => onEditingNameChange(e.target.value)}
                        onKeyDown={(e) => {
                          if (e.key === 'Enter') onSaveRename(frames_set.id!);
                          else if (e.key === 'Escape') onCancelEditing();
                        }}
                        className="flex-1 px-2 py-1 bg-surface-hover text-content rounded border border-border focus:outline-none focus:border-accent text-sm"
                        autoFocus
                        onClick={(e) => e.stopPropagation()}
                      />
                      <button
                        onClick={(e) => { e.stopPropagation(); onSaveRename(frames_set.id!); }}
                        className="p-1 text-success hover:text-success/80"
                      >
                        <Check size={16} />
                      </button>
                      <button
                        onClick={(e) => { e.stopPropagation(); onCancelEditing(); }}
                        className="p-1 text-error hover:text-error/80"
                      >
                        <X size={16} />
                      </button>
                    </div>
                  ) : (
                    <button
                      onClick={(e) => { e.stopPropagation(); onView(frames_set.id!); }}
                      className="font-medium text-content hover:text-accent transition-colors text-left"
                    >
                      {frames_set.name || 'Untitled'}
                    </button>
                  )}
                </td>

                {/* Coordinates */}
                <td className="px-4 py-3 hidden md:table-cell">
                  {frames_set.objctra && frames_set.objctdec ? (
                    <span className="font-mono text-xs text-content-muted">
                      {frames_set.objctra} / {frames_set.objctdec}
                    </span>
                  ) : (
                    <span className="text-content-muted">-</span>
                  )}
                </td>

                {/* Frames */}
                <td className="px-4 py-3 text-content-secondary">
                  {member_count}
                </td>

                {/* Exposure */}
                <td className="px-4 py-3 hidden lg:table-cell text-content-secondary">
                  {formatExposureTime(frames_set.total_exp_time)}
                </td>

                {/* Rotation */}
                <td className="px-4 py-3 hidden lg:table-cell text-content-muted font-mono text-sm">
                  {formatRotation(frames_set.avg_rotation, frames_set.min_rotation, frames_set.max_rotation)}
                </td>

                {/* Date Range */}
                <td className="px-4 py-3 hidden md:table-cell text-content-muted text-sm">
                  {formatDateRange(frames_set.date_obs_start, frames_set.date_obs_end)}
                </td>

                {/* Custom Star */}
                <td className="px-4 py-3 text-center">
                  {frames_set.is_custom ? (
                    <Star size={16} className="text-orange fill-orange inline" />
                  ) : (
                    <button
                      onClick={(e) => { e.stopPropagation(); onMarkAsCustom(frames_set.id!); }}
                      className="text-content-muted hover:text-orange/80 transition-colors"
                      title="Mark as Custom Set"
                    >
                      <Star size={16} />
                    </button>
                  )}
                </td>

                {/* Actions */}
                <td className="px-4 py-3">
                  <div className="flex items-center justify-center gap-2">
                    <button
                      onClick={(e) => { e.stopPropagation(); onStartEditing(frames_set.id!, frames_set.name); }}
                      className="p-1.5 text-content-muted hover:text-content hover:bg-surface-hover rounded transition-colors"
                      title="Rename"
                    >
                      <Pencil size={14} />
                    </button>
                    <button
                      onClick={(e) => { e.stopPropagation(); onDelete(frames_set.id!, frames_set.name); }}
                      className="p-1.5 text-content-muted hover:text-error hover:bg-surface-hover rounded transition-colors"
                      title="Delete"
                    >
                      <Trash2 size={14} />
                    </button>
                  </div>
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
    </div>
  );
}
