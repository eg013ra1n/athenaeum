import { useState, useCallback, useRef, useEffect, useMemo } from 'react';
import { Calendar, Camera, Aperture, ChevronDown, ChevronRight, ChevronsDownUp } from 'lucide-react';
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
  /** Whether selection mode is active (shows checkboxes) */
  selectionMode?: boolean;
  /** Callback to toggle selection mode */
  onToggleSelectionMode?: () => void;
}

/**
 * Styled checkbox component with indeterminate support.
 */
function StyledCheckbox({
  checked,
  indeterminate,
  onChange,
  className = '',
  title,
}: {
  checked: boolean;
  indeterminate?: boolean;
  onChange: (e: React.ChangeEvent<HTMLInputElement>) => void;
  className?: string;
  title?: string;
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
      className={`
        w-3.5 h-3.5 cursor-pointer rounded
        border border-gray-500
        bg-transparent
        checked:bg-blue-500 checked:border-blue-500
        focus:ring-1 focus:ring-blue-500 focus:ring-offset-0
        ${className}
      `}
      title={title}
      onClick={(e) => e.stopPropagation()}
    />
  );
}

/**
 * Navigation tree for calibration hierarchy.
 * VS Code-style compact design with:
 * - Compact rows with minimal padding
 * - Indent guide lines for hierarchy
 * - Selection mode toggle for multi-select
 * - ARIA tree roles for accessibility
 */
export function NavigationTree({
  data,
  selectedItem,
  onSelect,
  className = '',
  warningCounts,
  checkedFilters = new Set(),
  onCheckedChange,
  selectionMode = false,
  onToggleSelectionMode,
}: NavigationTreeProps) {
  // Track expanded state for date and camera levels - expanded by default
  const [expandedDates, setExpandedDates] = useState<Set<string>>(() => {
    return new Set(data.date_groups.map(dg => dg.date));
  });
  const [expandedCameras, setExpandedCameras] = useState<Set<string>>(() => {
    const keys = new Set<string>();
    data.date_groups.forEach(dg => {
      dg.camera_groups.forEach(cg => {
        keys.add(`${dg.date}:${cg.instrume}`);
      });
    });
    return keys;
  });

  // Expand all when data changes (e.g., navigating to different frame set)
  useEffect(() => {
    setExpandedDates(new Set(data.date_groups.map(dg => dg.date)));
    const cameraKeys = new Set<string>();
    data.date_groups.forEach(dg => {
      dg.camera_groups.forEach(cg => {
        cameraKeys.add(`${dg.date}:${cg.instrume}`);
      });
    });
    setExpandedCameras(cameraKeys);
  }, [data]);

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

  const isAllExpanded = useMemo(() => {
    if (data.date_groups.length === 0) return false;
    const allDatesExpanded = data.date_groups.every(dg => expandedDates.has(dg.date));
    if (!allDatesExpanded) return false;
    for (const dg of data.date_groups) {
      for (const cg of dg.camera_groups) {
        if (!expandedCameras.has(`${dg.date}:${cg.instrume}`)) return false;
      }
    }
    return true;
  }, [data.date_groups, expandedDates, expandedCameras]);

  const toggleExpandAll = useCallback(() => {
    if (isAllExpanded) {
      setExpandedDates(new Set());
      setExpandedCameras(new Set());
    } else {
      const allDateKeys = new Set(data.date_groups.map(dg => dg.date));
      setExpandedDates(allDateKeys);
      const allCameraKeys = new Set<string>();
      data.date_groups.forEach(dg => {
        dg.camera_groups.forEach(cg => {
          allCameraKeys.add(`${dg.date}:${cg.instrume}`);
        });
      });
      setExpandedCameras(allCameraKeys);
    }
  }, [isAllExpanded, data.date_groups]);

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
      className={`bg-gray-800/50 rounded-lg border border-gray-700/50 overflow-hidden flex flex-col ${className}`}
      role="tree"
      aria-label="Calibration session navigation"
    >
      {/* Compact Header with Selection Toggle */}
      <div className="px-2 py-1.5 border-b border-gray-700/50 flex items-center justify-between">
        <span className="text-xs text-gray-400">
          {data.date_groups.length} night{data.date_groups.length !== 1 ? 's' : ''} · {data.total_frames} frames
        </span>
        <div className="flex items-center gap-1">
          <button
            onClick={toggleExpandAll}
            className="p-1 text-gray-500 hover:text-gray-300 hover:bg-gray-700/50 rounded transition-colors"
            title={isAllExpanded ? "Collapse all" : "Expand all"}
          >
            <ChevronsDownUp size={14} className={isAllExpanded ? "rotate-180" : ""} />
          </button>
          {onToggleSelectionMode && (
            <button
              onClick={onToggleSelectionMode}
              className={`text-xs px-2 py-0.5 rounded transition-colors ${
                selectionMode
                  ? 'bg-blue-500/20 text-blue-400'
                  : 'text-gray-500 hover:text-gray-300 hover:bg-gray-700/50'
              }`}
            >
              {selectionMode ? 'Done' : 'Select'}
            </button>
          )}
        </div>
      </div>

      {/* Tree content */}
      <div className="flex-1 overflow-y-auto py-1">
        {data.date_groups.map((dateGroup) => {
          const dateKey = dateGroup.date;
          const isDateExpanded = expandedDates.has(dateKey);
          const dateWarnings = getWarningCount(dateKey);
          const dateFullyChecked = isDateFullyChecked(dateGroup);
          const datePartiallyChecked = isDatePartiallyChecked(dateGroup);

          return (
            <div key={dateKey} role="treeitem" aria-expanded={isDateExpanded}>
              {/* Date/Night level - compact row */}
              <div
                className={`
                  w-full py-1 px-2
                  flex items-center gap-1.5
                  transition-colors
                  hover:bg-gray-700/40
                  ${isSelected('date', dateKey) ? 'bg-blue-500/15' : ''}
                `}
              >
                {selectionMode && onCheckedChange && (
                  <StyledCheckbox
                    checked={dateFullyChecked}
                    indeterminate={datePartiallyChecked}
                    onChange={() => toggleDateChecked(dateGroup)}
                    title="Select all in this night"
                  />
                )}
                <button
                  onClick={() => toggleDate(dateKey)}
                  className="flex-1 flex items-center gap-1.5 text-left focus:outline-none focus-visible:ring-1 focus-visible:ring-blue-500 rounded min-h-[26px]"
                  aria-label={`${dateGroup.date_display}, ${dateGroup.frame_count} frames${dateWarnings > 0 ? `, ${dateWarnings} warnings` : ''}`}
                >
                  {isDateExpanded ? (
                    <ChevronDown size={14} className="text-gray-500 flex-shrink-0" aria-hidden="true" />
                  ) : (
                    <ChevronRight size={14} className="text-gray-500 flex-shrink-0" aria-hidden="true" />
                  )}
                  <Calendar size={14} className="text-violet-400 flex-shrink-0" aria-hidden="true" />
                  <span className="flex-1 min-w-0 text-sm text-gray-200 truncate">
                    {dateGroup.date_display}
                  </span>
                  {dateWarnings > 0 && <WarningBadge count={dateWarnings} />}
                  <span className="text-xs text-gray-500 flex-shrink-0 tabular-nums">
                    {dateGroup.frame_count}
                  </span>
                </button>
              </div>

              {/* Camera groups (nested with indent guide) */}
              {isDateExpanded && (
                <div role="group" className="ml-3 pl-2 border-l border-gray-700/50">
                  {dateGroup.camera_groups.map((cameraGroup) => {
                    const cameraKey = `${dateKey}:${cameraGroup.instrume}`;
                    const isCameraExpanded = expandedCameras.has(cameraKey);
                    const cameraWarnings = getWarningCount(dateKey, cameraGroup.instrume);
                    const cameraFullyChecked = isCameraFullyChecked(dateKey, cameraGroup);
                    const cameraPartiallyChecked = isCameraPartiallyChecked(dateKey, cameraGroup);

                    return (
                      <div key={cameraKey} role="treeitem" aria-expanded={isCameraExpanded}>
                        {/* Camera level - compact row */}
                        <div
                          className={`
                            w-full py-1 px-1
                            flex items-center gap-1.5
                            transition-colors
                            hover:bg-gray-700/40
                            ${isSelected('camera', dateKey, cameraGroup.instrume) ? 'bg-blue-500/15' : ''}
                          `}
                        >
                          {selectionMode && onCheckedChange && (
                            <StyledCheckbox
                              checked={cameraFullyChecked}
                              indeterminate={cameraPartiallyChecked}
                              onChange={() => toggleCameraChecked(dateKey, cameraGroup)}
                              title="Select all in this camera"
                            />
                          )}
                          <button
                            onClick={() => toggleCamera(cameraKey)}
                            className="flex-1 flex items-center gap-1.5 text-left focus:outline-none focus-visible:ring-1 focus-visible:ring-blue-500 rounded min-h-[24px]"
                            aria-label={`${cameraGroup.instrume}, ${cameraGroup.frame_count} frames${cameraWarnings > 0 ? `, ${cameraWarnings} warnings` : ''}`}
                          >
                            {isCameraExpanded ? (
                              <ChevronDown size={14} className="text-gray-500 flex-shrink-0" aria-hidden="true" />
                            ) : (
                              <ChevronRight size={14} className="text-gray-500 flex-shrink-0" aria-hidden="true" />
                            )}
                            <Camera size={14} className="text-blue-400 flex-shrink-0" aria-hidden="true" />
                            <span className="flex-1 min-w-0 text-sm text-gray-300 truncate">
                              {cameraGroup.instrume}
                            </span>
                            {cameraWarnings > 0 && <WarningBadge count={cameraWarnings} />}
                            <span className="text-xs text-gray-500 flex-shrink-0 tabular-nums">
                              {cameraGroup.frame_count}
                            </span>
                          </button>
                        </div>

                        {/* Filter groups (leaf level with indent guide) */}
                        {isCameraExpanded && (
                          <div role="group" className="ml-3 pl-2 border-l border-gray-700/50">
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
                                    w-full py-1 px-1
                                    flex items-center gap-1.5
                                    transition-colors
                                    hover:bg-gray-700/40
                                    ${isFilterSelected ? 'bg-blue-500/20' : ''}
                                  `}
                                  role="treeitem"
                                  aria-selected={isFilterSelected}
                                >
                                  {selectionMode && onCheckedChange && (
                                    <StyledCheckbox
                                      checked={isFilterChecked}
                                      onChange={() => toggleFilterChecked(fullKey)}
                                      title="Select this filter group"
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
                                    className="flex-1 flex items-center gap-1.5 text-left focus:outline-none focus-visible:ring-1 focus-visible:ring-blue-500 rounded min-h-[24px]"
                                    aria-label={`${filterGroup.filter_display}, ${filterGroup.frame_count} frames${filterWarnings > 0 ? `, ${filterWarnings} warnings` : ''}`}
                                  >
                                    <Aperture size={14} className="text-cyan-400 flex-shrink-0" aria-hidden="true" />
                                    <span className="flex-1 min-w-0 text-sm text-gray-300 truncate">
                                      {filterGroup.filter_display}
                                    </span>
                                    {filterWarnings > 0 && <WarningBadge count={filterWarnings} />}
                                    <span className="text-xs text-gray-500 flex-shrink-0 tabular-nums">
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
          <div className="px-2 py-6 text-center text-gray-500">
            <p className="text-xs">No sessions found</p>
          </div>
        )}
      </div>
    </nav>
  );
}
