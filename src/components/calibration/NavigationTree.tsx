import { useState, useCallback, useRef, useEffect } from 'react';
import { Calendar, Camera, Aperture, ChevronDown, ChevronRight } from 'lucide-react';
import type {
  CalibrationHierarchyView,
  CalibrationDateGroup,
  CalibrationCameraGroup,
  CalibrationFilterGroup,
} from '../../types/models';
import { WarningBadge } from './WarningPanel';

/** Selection target for the detail panel */
export interface SelectedItem {
  type: 'date' | 'camera' | 'filter';
  dateKey: string;
  cameraKey?: string;
  filterKey?: string;
  data: CalibrationDateGroup | CalibrationCameraGroup | CalibrationFilterGroup;
}

interface NavigationTreeProps {
  data: CalibrationHierarchyView;
  selectedItem: SelectedItem | null;
  onSelect: (item: SelectedItem | null) => void;
  className?: string;
  /** Warning counts per filter group for badge display */
  warningCounts?: Map<string, number>;
  /** Checked filter keys for selection (format: "dateKey:cameraKey:filterKey") */
  checkedFilters?: Set<string>;
  /** Callback when filter selection changes */
  onCheckedChange?: (checkedFilters: Set<string>) => void;
}

/**
 * Indeterminate checkbox component.
 */
function IndeterminateCheckbox({
  checked,
  indeterminate,
  onChange,
  className = '',
  title,
}: {
  checked: boolean;
  indeterminate: boolean;
  onChange: (e: React.ChangeEvent<HTMLInputElement>) => void;
  className?: string;
  title?: string;
}) {
  const ref = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (ref.current) {
      ref.current.indeterminate = indeterminate;
    }
  }, [indeterminate]);

  return (
    <input
      ref={ref}
      type="checkbox"
      checked={checked}
      onChange={onChange}
      className={`w-4 h-4 cursor-pointer accent-blue-500 ${className}`}
      title={title}
      onClick={(e) => e.stopPropagation()}
    />
  );
}

/**
 * Navigation tree for calibration hierarchy.
 * Shows Night → Camera → Filter structure with accessible design:
 * - Checkboxes at all levels for selection
 * - Minimum 44px touch targets
 * - Clear focus indicators
 * - Keyboard navigation support
 * - ARIA tree roles
 */
export function NavigationTree({
  data,
  selectedItem,
  onSelect,
  className = '',
  warningCounts,
  checkedFilters = new Set(),
  onCheckedChange,
}: NavigationTreeProps) {
  // Track expanded state for date and camera levels
  const [expandedDates, setExpandedDates] = useState<Set<string>>(new Set());
  const [expandedCameras, setExpandedCameras] = useState<Set<string>>(new Set());

  const toggleDate = useCallback((dateKey: string) => {
    setExpandedDates(prev => {
      const next = new Set(prev);
      if (next.has(dateKey)) {
        next.delete(dateKey);
      } else {
        next.add(dateKey);
      }
      return next;
    });
  }, []);

  const toggleCamera = useCallback((cameraKey: string) => {
    setExpandedCameras(prev => {
      const next = new Set(prev);
      if (next.has(cameraKey)) {
        next.delete(cameraKey);
      } else {
        next.add(cameraKey);
      }
      return next;
    });
  }, []);

  const isSelected = useCallback(
    (type: 'date' | 'camera' | 'filter', dateKey: string, cameraKey?: string, filterKey?: string) => {
      if (!selectedItem) return false;
      if (selectedItem.type !== type) return false;
      if (selectedItem.dateKey !== dateKey) return false;
      if (type === 'camera' && selectedItem.cameraKey !== cameraKey) return false;
      if (type === 'filter' && (selectedItem.cameraKey !== cameraKey || selectedItem.filterKey !== filterKey)) return false;
      return true;
    },
    [selectedItem]
  );

  const getWarningCount = (dateKey: string, cameraKey?: string, filterKey?: string): number => {
    if (!warningCounts) return 0;
    const key = filterKey
      ? `${dateKey}:${cameraKey}:${filterKey}`
      : cameraKey
      ? `${dateKey}:${cameraKey}`
      : dateKey;
    return warningCounts.get(key) ?? 0;
  };

  // Build a unique filter key that includes exptime to differentiate same filter with different exposures
  const buildFilterKey = useCallback((filterGroup: CalibrationFilterGroup): string => {
    const baseFilter = filterGroup.filter ?? '__no_filter__';
    // Include exptime in key to differentiate same filter with different exposure times
    return filterGroup.exptime !== null
      ? `${baseFilter}:${filterGroup.exptime}`
      : baseFilter;
  }, []);

  // Get all filter keys for a date group
  const getFilterKeysForDate = useCallback((dateGroup: CalibrationDateGroup): string[] => {
    const keys: string[] = [];
    for (const cameraGroup of dateGroup.camera_groups) {
      for (const filterGroup of cameraGroup.filter_groups) {
        const filterKey = buildFilterKey(filterGroup);
        keys.push(`${dateGroup.date}:${cameraGroup.instrume}:${filterKey}`);
      }
    }
    return keys;
  }, [buildFilterKey]);

  // Get all filter keys for a camera group
  const getFilterKeysForCamera = useCallback((dateKey: string, cameraGroup: CalibrationCameraGroup): string[] => {
    return cameraGroup.filter_groups.map(fg => {
      const filterKey = buildFilterKey(fg);
      return `${dateKey}:${cameraGroup.instrume}:${filterKey}`;
    });
  }, [buildFilterKey]);

  // Check if all filters in a date are checked
  const isDateFullyChecked = useCallback((dateGroup: CalibrationDateGroup): boolean => {
    const keys = getFilterKeysForDate(dateGroup);
    return keys.length > 0 && keys.every(k => checkedFilters.has(k));
  }, [checkedFilters, getFilterKeysForDate]);

  // Check if some (but not all) filters in a date are checked
  const isDatePartiallyChecked = useCallback((dateGroup: CalibrationDateGroup): boolean => {
    const keys = getFilterKeysForDate(dateGroup);
    const checkedCount = keys.filter(k => checkedFilters.has(k)).length;
    return checkedCount > 0 && checkedCount < keys.length;
  }, [checkedFilters, getFilterKeysForDate]);

  // Check if all filters in a camera are checked
  const isCameraFullyChecked = useCallback((dateKey: string, cameraGroup: CalibrationCameraGroup): boolean => {
    const keys = getFilterKeysForCamera(dateKey, cameraGroup);
    return keys.length > 0 && keys.every(k => checkedFilters.has(k));
  }, [checkedFilters, getFilterKeysForCamera]);

  // Check if some (but not all) filters in a camera are checked
  const isCameraPartiallyChecked = useCallback((dateKey: string, cameraGroup: CalibrationCameraGroup): boolean => {
    const keys = getFilterKeysForCamera(dateKey, cameraGroup);
    const checkedCount = keys.filter(k => checkedFilters.has(k)).length;
    return checkedCount > 0 && checkedCount < keys.length;
  }, [checkedFilters, getFilterKeysForCamera]);

  // Toggle all filters in a date
  const toggleDateChecked = useCallback((dateGroup: CalibrationDateGroup) => {
    if (!onCheckedChange) return;
    const keys = getFilterKeysForDate(dateGroup);
    const allChecked = isDateFullyChecked(dateGroup);

    const next = new Set(checkedFilters);
    if (allChecked) {
      // Uncheck all
      keys.forEach(k => next.delete(k));
    } else {
      // Check all
      keys.forEach(k => next.add(k));
    }
    onCheckedChange(next);
  }, [checkedFilters, onCheckedChange, getFilterKeysForDate, isDateFullyChecked]);

  // Toggle all filters in a camera
  const toggleCameraChecked = useCallback((dateKey: string, cameraGroup: CalibrationCameraGroup) => {
    if (!onCheckedChange) return;
    const keys = getFilterKeysForCamera(dateKey, cameraGroup);
    const allChecked = isCameraFullyChecked(dateKey, cameraGroup);

    const next = new Set(checkedFilters);
    if (allChecked) {
      // Uncheck all
      keys.forEach(k => next.delete(k));
    } else {
      // Check all
      keys.forEach(k => next.add(k));
    }
    onCheckedChange(next);
  }, [checkedFilters, onCheckedChange, getFilterKeysForCamera, isCameraFullyChecked]);

  // Toggle a single filter
  const toggleFilterChecked = useCallback((fullKey: string) => {
    if (!onCheckedChange) return;
    const next = new Set(checkedFilters);
    if (next.has(fullKey)) {
      next.delete(fullKey);
    } else {
      next.add(fullKey);
    }
    onCheckedChange(next);
  }, [checkedFilters, onCheckedChange]);

  return (
    <nav
      className={`bg-gray-800 rounded-xl border border-gray-600 overflow-hidden flex flex-col ${className}`}
      role="tree"
      aria-label="Calibration session navigation"
    >
      {/* Header */}
      <div className="px-4 py-3 border-b border-gray-600 bg-gray-850">
        <h3 className="text-base font-semibold text-gray-100">Sessions</h3>
        <p className="text-sm text-gray-400 mt-0.5">
          {data.date_groups.length} night{data.date_groups.length !== 1 ? 's' : ''} &middot;{' '}
          {data.total_frames} frame{data.total_frames !== 1 ? 's' : ''}
        </p>
      </div>

      {/* Tree content */}
      <div className="flex-1 overflow-y-auto py-2">
        {data.date_groups.map((dateGroup) => {
          const dateKey = dateGroup.date;
          const isDateExpanded = expandedDates.has(dateKey);
          const dateWarnings = getWarningCount(dateKey);
          const dateFullyChecked = isDateFullyChecked(dateGroup);
          const datePartiallyChecked = isDatePartiallyChecked(dateGroup);

          return (
            <div key={dateKey} role="treeitem" aria-expanded={isDateExpanded}>
              {/* Date/Night level header */}
              <div
                className={`
                  w-full min-h-[52px] px-4 py-3
                  flex items-center gap-3
                  transition-colors
                  hover:bg-gray-700
                  ${isSelected('date', dateKey) ? 'bg-blue-900/30' : ''}
                `}
              >
                {onCheckedChange && (
                  <IndeterminateCheckbox
                    checked={dateFullyChecked}
                    indeterminate={datePartiallyChecked}
                    onChange={() => toggleDateChecked(dateGroup)}
                    title="Select all in this night"
                  />
                )}
                <button
                  onClick={() => toggleDate(dateKey)}
                  className="flex-1 flex items-center gap-3 text-left focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 rounded"
                  aria-label={`${dateGroup.date_display}, ${dateGroup.frame_count} frames${dateWarnings > 0 ? `, ${dateWarnings} warnings` : ''}`}
                >
                  {isDateExpanded ? (
                    <ChevronDown size={20} className="text-gray-400 flex-shrink-0" aria-hidden="true" />
                  ) : (
                    <ChevronRight size={20} className="text-gray-400 flex-shrink-0" aria-hidden="true" />
                  )}
                  <Calendar size={20} className="text-violet-400 flex-shrink-0" aria-hidden="true" />
                  <div className="flex-1 min-w-0">
                    <span className="text-base font-semibold text-gray-100 truncate block">
                      {dateGroup.date_display}
                    </span>
                  </div>
                  {dateWarnings > 0 && <WarningBadge count={dateWarnings} />}
                  <span className="text-sm text-gray-400 flex-shrink-0">
                    {dateGroup.frame_count}
                  </span>
                </button>
              </div>

              {/* Camera groups (nested) */}
              {isDateExpanded && (
                <div role="group" className="ml-4">
                  {dateGroup.camera_groups.map((cameraGroup) => {
                    const cameraKey = `${dateKey}:${cameraGroup.instrume}`;
                    const isCameraExpanded = expandedCameras.has(cameraKey);
                    const cameraWarnings = getWarningCount(dateKey, cameraGroup.instrume);
                    const cameraFullyChecked = isCameraFullyChecked(dateKey, cameraGroup);
                    const cameraPartiallyChecked = isCameraPartiallyChecked(dateKey, cameraGroup);

                    return (
                      <div key={cameraKey} role="treeitem" aria-expanded={isCameraExpanded}>
                        {/* Camera level header */}
                        <div
                          className={`
                            w-full min-h-[48px] px-4 py-2.5
                            flex items-center gap-3
                            transition-colors
                            hover:bg-gray-700
                            ${isSelected('camera', dateKey, cameraGroup.instrume) ? 'bg-blue-900/30' : ''}
                          `}
                        >
                          {onCheckedChange && (
                            <IndeterminateCheckbox
                              checked={cameraFullyChecked}
                              indeterminate={cameraPartiallyChecked}
                              onChange={() => toggleCameraChecked(dateKey, cameraGroup)}
                              title="Select all in this camera"
                            />
                          )}
                          <button
                            onClick={() => toggleCamera(cameraKey)}
                            className="flex-1 flex items-center gap-3 text-left focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 rounded"
                            aria-label={`${cameraGroup.instrume}, ${cameraGroup.frame_count} frames${cameraWarnings > 0 ? `, ${cameraWarnings} warnings` : ''}`}
                          >
                            {isCameraExpanded ? (
                              <ChevronDown size={18} className="text-gray-400 flex-shrink-0" aria-hidden="true" />
                            ) : (
                              <ChevronRight size={18} className="text-gray-400 flex-shrink-0" aria-hidden="true" />
                            )}
                            <Camera size={18} className="text-blue-400 flex-shrink-0" aria-hidden="true" />
                            <div className="flex-1 min-w-0">
                              <span className="text-sm font-medium text-gray-200 truncate block">
                                {cameraGroup.instrume}
                              </span>
                            </div>
                            {cameraWarnings > 0 && <WarningBadge count={cameraWarnings} />}
                            <span className="text-sm text-gray-400 flex-shrink-0">
                              {cameraGroup.frame_count}
                            </span>
                          </button>
                        </div>

                        {/* Filter groups (leaf level - selectable) */}
                        {isCameraExpanded && (
                          <div role="group" className="ml-4">
                            {cameraGroup.filter_groups.map((filterGroup) => {
                              const filterKey = buildFilterKey(filterGroup);
                              const fullKey = `${dateKey}:${cameraGroup.instrume}:${filterKey}`;
                              const filterWarnings = getWarningCount(dateKey, cameraGroup.instrume, filterKey);
                              const isFilterSelected = isSelected('filter', dateKey, cameraGroup.instrume, filterKey);
                              const isFilterChecked = checkedFilters.has(fullKey);

                              return (
                                <div
                                  key={fullKey}
                                  className={`
                                    w-full min-h-[44px] px-4 py-2
                                    flex items-center gap-3
                                    transition-colors
                                    hover:bg-gray-700
                                    ${isFilterSelected
                                      ? 'bg-blue-600/30 ring-1 ring-blue-500/50 ring-inset'
                                      : ''
                                    }
                                  `}
                                  role="treeitem"
                                  aria-selected={isFilterSelected}
                                >
                                  {onCheckedChange && (
                                    <input
                                      type="checkbox"
                                      checked={isFilterChecked}
                                      onChange={() => toggleFilterChecked(fullKey)}
                                      className="w-4 h-4 cursor-pointer accent-blue-500"
                                      title="Select this filter group"
                                      onClick={(e) => e.stopPropagation()}
                                    />
                                  )}
                                  <button
                                    onClick={() => onSelect({
                                      type: 'filter',
                                      dateKey,
                                      cameraKey: cameraGroup.instrume,
                                      filterKey,
                                      data: filterGroup,
                                    })}
                                    className="flex-1 flex items-center gap-3 text-left focus:outline-none focus-visible:ring-2 focus-visible:ring-blue-500 rounded"
                                    aria-label={`${filterGroup.filter_display}, ${filterGroup.frame_count} frames${filterWarnings > 0 ? `, ${filterWarnings} warnings` : ''}`}
                                  >
                                    <Aperture size={16} className="text-cyan-400 flex-shrink-0" aria-hidden="true" />
                                    <div className="flex-1 min-w-0">
                                      <span className="text-sm text-gray-200 truncate block">
                                        {filterGroup.filter_display}
                                      </span>
                                    </div>
                                    {filterWarnings > 0 && <WarningBadge count={filterWarnings} />}
                                    <span className="text-sm text-gray-400 flex-shrink-0">
                                      {filterGroup.frame_count}
                                    </span>
                                  </button>
                                </div>
                              );
                            })}
                          </div>
                        )}
                      </div>
                    );
                  })}
                </div>
              )}
            </div>
          );
        })}

        {/* Empty state */}
        {data.date_groups.length === 0 && (
          <div className="px-4 py-8 text-center text-gray-400">
            <p className="text-sm">No sessions found</p>
          </div>
        )}
      </div>
    </nav>
  );
}
