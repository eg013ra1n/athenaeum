import React, { memo } from "react";
import {
  Loader2,
  CheckSquare,
  Square,
  Trash2,
} from "lucide-react";
import type { FrameListProps } from "./types";

/** Frame list sidebar for BlinkViewer with selection checkboxes */
export const FrameList: React.FC<FrameListProps> = memo(function FrameList({
  frames,
  currentIndex,
  selectedFrames,
  blackholedFileIds,
  loadingIndices,
  onFrameClick,
  onCheckboxClick,
  onSelectAll,
  onClearSelection,
}) {
  return (
    <div className="w-1/4 bg-surface border-l border-border flex flex-col">
      {/* List header with selection controls */}
      <div className="p-2 border-b border-border">
        <div className="flex items-center justify-between">
          <h3 className="text-sm font-semibold text-content-muted">
            Frames ({frames.length})
          </h3>
          <div className="flex items-center gap-1">
            <button
              onClick={onSelectAll}
              className="px-1.5 py-0.5 text-xs text-content-muted hover:text-content hover:bg-surface-hover rounded transition-colors"
              title="Select all (Ctrl+A)"
            >
              Select all
            </button>
            <button
              onClick={onClearSelection}
              className="px-1.5 py-0.5 text-xs text-content-muted hover:text-content hover:bg-surface-hover rounded transition-colors"
              title="Clear selection"
            >
              Unselect
            </button>
          </div>
        </div>
      </div>

      {/* Scrollable frame list */}
      <div className="flex-1 overflow-y-auto p-2 select-none">
        <div className="space-y-1">
          {frames.map((frame, index) => {
            const isSelected = selectedFrames.has(index);
            const isCurrent = index === currentIndex;
            const isBlackholed = frame.file.id ? blackholedFileIds.has(frame.file.id) : false;

            // Determine row styling based on state combinations
            let rowClasses = "flex items-center gap-2 px-2 py-2 rounded text-sm cursor-pointer transition-colors";
            if (isBlackholed && isSelected) {
              // Blackholed + selected: orange/amber tint with warning ring
              rowClasses += " bg-warning-muted text-warning ring-2 ring-warning hover:brightness-110";
            } else if (isBlackholed) {
              // Blackholed only
              rowClasses += " bg-error-muted text-content-muted hover:brightness-110";
            } else if (isCurrent) {
              // Current frame
              rowClasses += " bg-accent text-surface";
            } else if (isSelected) {
              // Selected only
              rowClasses += " bg-warning-muted text-warning hover:brightness-110";
            } else {
              // Default
              rowClasses += " bg-surface-elevated text-content-secondary hover:bg-surface-hover";
            }

            return (
              <div
                key={frame.file.id}
                onClick={(e) => onFrameClick(index, e)}
                className={rowClasses}
              >
                {/* Checkbox or Blackhole indicator */}
                {isBlackholed ? (
                  <button
                    onClick={(e) => onCheckboxClick(index, e)}
                    className={`flex-shrink-0 p-0.5 rounded transition-colors ${isSelected ? 'text-warning' : 'text-error hover:brightness-110'}`}
                    title={isSelected ? "Click to unselect" : "Click to select"}
                  >
                    {isSelected ? <CheckSquare size={16} /> : <Trash2 size={16} />}
                  </button>
                ) : (
                  <button
                    onClick={(e) => onCheckboxClick(index, e)}
                    className={`flex-shrink-0 p-0.5 rounded transition-colors ${
                      isSelected
                        ? "text-warning"
                        : "text-content-muted hover:text-content-secondary"
                    }`}
                  >
                    {isSelected ? (
                      <CheckSquare size={16} />
                    ) : (
                      <Square size={16} />
                    )}
                  </button>
                )}

                {/* Frame info */}
                <div className={`flex-1 min-w-0 ${isBlackholed ? 'line-through opacity-60' : ''}`}>
                  <div className="font-medium truncate">
                    {frame.file.filename}
                    {isBlackholed && (
                      <span className="ml-2 text-xs text-error">(blackholed)</span>
                    )}
                  </div>
                </div>

                {/* Loading indicator */}
                {loadingIndices.has(index) && (
                  <Loader2 className="animate-spin flex-shrink-0" size={14} />
                )}
              </div>
            );
          })}
        </div>
      </div>
    </div>
  );
});
