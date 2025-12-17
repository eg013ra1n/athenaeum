import React, { memo } from "react";
import {
  Loader2,
  CheckSquare,
  Square,
  XSquare,
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
    <div className="w-1/4 bg-gray-900 border-l border-gray-700 flex flex-col">
      {/* List header with selection controls */}
      <div className="p-2 border-b border-gray-700">
        <div className="flex items-center justify-between">
          <h3 className="text-sm font-semibold text-gray-400">
            Frames ({frames.length})
          </h3>
          <div className="flex items-center gap-1">
            <button
              onClick={onSelectAll}
              className="p-1 text-gray-400 hover:text-white hover:bg-gray-700 rounded transition-colors"
              title="Select all (Ctrl+A)"
            >
              <CheckSquare size={16} />
            </button>
            <button
              onClick={onClearSelection}
              className="p-1 text-gray-400 hover:text-white hover:bg-gray-700 rounded transition-colors"
              title="Clear selection"
            >
              <XSquare size={16} />
            </button>
          </div>
        </div>
      </div>

      {/* Scrollable frame list */}
      <div className="flex-1 overflow-y-auto p-2">
        <div className="space-y-1">
          {frames.map((frame, index) => {
            const isSelected = selectedFrames.has(index);
            const isCurrent = index === currentIndex;
            const isBlackholed = frame.file.id ? blackholedFileIds.has(frame.file.id) : false;

            // Determine row styling based on state combinations
            let rowClasses = "flex items-center gap-2 px-2 py-2 rounded text-sm cursor-pointer transition-colors";
            if (isBlackholed && isSelected) {
              // Blackholed + selected: orange/amber tint with yellow ring
              rowClasses += " bg-orange-900/40 text-orange-200 ring-2 ring-yellow-400 hover:bg-orange-900/50";
            } else if (isBlackholed) {
              // Blackholed only
              rowClasses += " bg-red-900/30 text-gray-500 hover:bg-red-900/40";
            } else if (isCurrent) {
              // Current frame
              rowClasses += " bg-blue-600 text-white";
            } else if (isSelected) {
              // Selected only
              rowClasses += " bg-yellow-600/30 text-yellow-100 hover:bg-yellow-600/40";
            } else {
              // Default
              rowClasses += " bg-gray-800 text-gray-300 hover:bg-gray-700";
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
                    className={`flex-shrink-0 p-0.5 rounded transition-colors ${isSelected ? 'text-yellow-400' : 'text-red-400 hover:text-red-300'}`}
                    title={isSelected ? "Click to unselect" : "Click to select"}
                  >
                    {isSelected ? <CheckSquare size={16} /> : <Trash2 size={16} />}
                  </button>
                ) : (
                  <button
                    onClick={(e) => onCheckboxClick(index, e)}
                    className={`flex-shrink-0 p-0.5 rounded transition-colors ${
                      isSelected
                        ? "text-yellow-400"
                        : "text-gray-500 hover:text-gray-300"
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
                  </div>
                  <div className="text-xs opacity-75 mt-0.5">
                    {frame.frame?.filter && (
                      <span>{frame.frame.filter}</span>
                    )}
                    {frame.frame?.exptime && (
                      <span className="ml-2">{frame.frame.exptime}s</span>
                    )}
                    {isBlackholed && (
                      <span className="ml-2 text-red-400">(blackholed)</span>
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
