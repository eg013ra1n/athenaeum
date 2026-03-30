import { useState, useCallback, useRef, useEffect, useMemo } from 'react';
import { Camera, Aperture, ChevronDown, ChevronRight, ChevronsDownUp } from 'lucide-react';
import type { MergedCameraFilterNode } from './utils';

interface MergedCameraFilterTreeProps {
  nodes: MergedCameraFilterNode[];
  /** Checked filter keys (multi-select) */
  checkedKeys: Set<string>;
  onCheckedChange: (keys: Set<string>) => void;
  className?: string;
  /** Text shown in header when items are checked (e.g. "3 groups · 42 frames") */
  checkedLabel?: string;
  /** Stacked SNR per filter key (dB), displayed next to filter label */
  filterSnrMap?: Map<string, number>;
  /** Optional footer rendered inside the tree container with a top border separator */
  footer?: React.ReactNode;
}

/** Styled checkbox matching NavigationTree's StyledCheckbox */
function StyledCheckbox({
  checked,
  indeterminate,
  onChange,
  title,
}: {
  checked: boolean;
  indeterminate?: boolean;
  onChange: (e: React.ChangeEvent<HTMLInputElement>) => void;
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
      className="
        w-3.5 h-3.5 cursor-pointer rounded
        border border-border
        bg-transparent
        checked:bg-accent checked:border-accent
        focus:ring-1 focus:ring-accent focus:ring-offset-0
      "
      title={title}
      onClick={(e) => e.stopPropagation()}
    />
  );
}

export function MergedCameraFilterTree({ nodes, checkedKeys, onCheckedChange, className = '', checkedLabel, filterSnrMap, footer }: MergedCameraFilterTreeProps) {
  const [expandedCameras, setExpandedCameras] = useState<Set<string>>(
    () => new Set(nodes.map(n => n.camera))
  );

  // Re-expand all when nodes change
  useEffect(() => {
    setExpandedCameras(new Set(nodes.map(n => n.camera)));
  }, [nodes]);

  const toggleCamera = useCallback((camera: string) => {
    setExpandedCameras(prev => {
      const next = new Set(prev);
      if (next.has(camera)) next.delete(camera);
      else next.add(camera);
      return next;
    });
  }, []);

  const allFilterKeys = useMemo(
    () => nodes.flatMap(n => n.filters.map(f => f.key)),
    [nodes]
  );

  // Expand/collapse all
  const isAllExpanded = useMemo(() => {
    if (nodes.length === 0) return false;
    return nodes.every(n => expandedCameras.has(n.camera));
  }, [nodes, expandedCameras]);

  const toggleExpandAll = useCallback(() => {
    if (isAllExpanded) {
      setExpandedCameras(new Set());
    } else {
      setExpandedCameras(new Set(nodes.map(n => n.camera)));
    }
  }, [isAllExpanded, nodes]);

  // --- Check helpers ---

  const isCameraFullyChecked = useCallback((cam: MergedCameraFilterNode): boolean => {
    return cam.filters.length > 0 && cam.filters.every(f => checkedKeys.has(f.key));
  }, [checkedKeys]);

  const isCameraPartiallyChecked = useCallback((cam: MergedCameraFilterNode): boolean => {
    const count = cam.filters.filter(f => checkedKeys.has(f.key)).length;
    return count > 0 && count < cam.filters.length;
  }, [checkedKeys]);

  const toggleCameraChecked = useCallback((cam: MergedCameraFilterNode) => {
    const keys = cam.filters.map(f => f.key);
    const allChecked = isCameraFullyChecked(cam);
    const next = new Set(checkedKeys);
    if (allChecked) {
      keys.forEach(k => next.delete(k));
    } else {
      keys.forEach(k => next.add(k));
    }
    onCheckedChange(next);
  }, [checkedKeys, onCheckedChange, isCameraFullyChecked]);

  const toggleFilterChecked = useCallback((key: string) => {
    const next = new Set(checkedKeys);
    if (next.has(key)) next.delete(key);
    else next.add(key);
    onCheckedChange(next);
  }, [checkedKeys, onCheckedChange]);

  // Select all / deselect all
  const isAllChecked = allFilterKeys.length > 0 && allFilterKeys.every(k => checkedKeys.has(k));
  const isSomeChecked = allFilterKeys.some(k => checkedKeys.has(k)) && !isAllChecked;

  const toggleAllChecked = useCallback(() => {
    if (isAllChecked) {
      onCheckedChange(new Set());
    } else {
      onCheckedChange(new Set(allFilterKeys));
    }
  }, [isAllChecked, allFilterKeys, onCheckedChange]);

  return (
    <nav
      className={`bg-surface-elevated/50 rounded-lg border border-border/50 overflow-hidden flex flex-col ${className}`}
      role="tree"
      aria-label="Camera and filter navigation"
    >
      {/* Compact Header */}
      <div className="px-2 py-1.5 border-b border-border/50 flex items-center justify-between">
        <div className="flex items-center gap-1.5">
          <StyledCheckbox
            checked={isAllChecked}
            indeterminate={isSomeChecked}
            onChange={toggleAllChecked}
            title="Select all"
          />
          <span className="text-xs text-content-muted">
            {checkedKeys.size > 0 && checkedLabel ? checkedLabel : 'Select all'}
          </span>
        </div>
        <button
          onClick={toggleExpandAll}
          className="p-1 text-content-muted hover:text-content-secondary hover:bg-surface-hover/50 rounded transition-colors"
          title={isAllExpanded ? "Collapse all" : "Expand all"}
        >
          <ChevronsDownUp size={14} className={isAllExpanded ? "rotate-180" : ""} />
        </button>
      </div>

      {/* Tree content */}
      <div className="flex-1 overflow-y-auto py-1">
        {nodes.map(cam => {
          const isCamExpanded = expandedCameras.has(cam.camera);
          const camFullyChecked = isCameraFullyChecked(cam);
          const camPartiallyChecked = isCameraPartiallyChecked(cam);

          return (
            <div key={cam.camera} role="treeitem" aria-expanded={isCamExpanded}>
              {/* Camera level */}
              <div className="w-full py-1 px-2 flex items-center gap-1.5 transition-colors hover:bg-surface-hover/40">
                <StyledCheckbox
                  checked={camFullyChecked}
                  indeterminate={camPartiallyChecked}
                  onChange={() => toggleCameraChecked(cam)}
                  title="Select all in this camera"
                />
                <button
                  onClick={() => toggleCamera(cam.camera)}
                  className="flex-1 flex items-center gap-1.5 text-left focus:outline-none focus-visible:ring-1 focus-visible:ring-accent rounded min-h-[26px]"
                >
                  {isCamExpanded ? (
                    <ChevronDown size={14} className="text-content-muted flex-shrink-0" />
                  ) : (
                    <ChevronRight size={14} className="text-content-muted flex-shrink-0" />
                  )}
                  <Camera size={14} className="text-accent flex-shrink-0" />
                  <span className="flex-1 min-w-0 text-sm text-content-secondary truncate">
                    {cam.camera}
                  </span>
                  <span className="text-xs text-content-muted flex-shrink-0 tabular-nums">
                    {cam.totalFrameCount}
                  </span>
                </button>
              </div>

              {/* Filter level (with indent guide) */}
              {isCamExpanded && (
                <div role="group" className="ml-3 pl-2 border-l border-border/50">
                  {cam.filters.map(filter => {
                    const isChecked = checkedKeys.has(filter.key);

                    return (
                      <div
                        key={filter.key}
                        className={`
                          w-full py-1 px-1
                          flex items-center gap-1.5
                          transition-colors
                          hover:bg-surface-hover/40
                          ${isChecked ? 'bg-accent/10' : ''}
                        `}
                        role="treeitem"
                      >
                        <StyledCheckbox
                          checked={isChecked}
                          onChange={() => toggleFilterChecked(filter.key)}
                          title="Select this filter group"
                        />
                        <button
                          onClick={() => toggleFilterChecked(filter.key)}
                          className="flex-1 flex items-center gap-1.5 text-left focus:outline-none focus-visible:ring-1 focus-visible:ring-accent rounded min-h-[24px]"
                        >
                          <Aperture size={14} className="text-accent flex-shrink-0" />
                          <span className="flex-1 min-w-0 text-sm text-content-secondary truncate">
                            {filter.label}
                            {filterSnrMap?.has(filter.key) && (
                              <span className="text-[10px] text-content-muted ml-1" title="Stacked SNR (optimal weighting)">
                                {filterSnrMap.get(filter.key)!.toFixed(1)} dB
                              </span>
                            )}
                          </span>
                          <span className="text-xs text-content-muted flex-shrink-0 tabular-nums">
                            {filter.frameCount}
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

        {nodes.length === 0 && (
          <div className="px-2 py-6 text-center text-content-muted">
            <p className="text-xs">No frames found</p>
          </div>
        )}
      </div>
      {footer && (
        <div className="border-t border-border/50 px-2 py-1.5">
          {footer}
        </div>
      )}
    </nav>
  );
}
