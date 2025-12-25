import React, { memo } from "react";
import {
  X,
  Play,
  Pause,
  ChevronLeft,
  ChevronRight,
  Loader2,
  Trash2,
  RotateCcw,
} from "lucide-react";
import type { ToolBarProps } from "./types";

/** Top toolbar for BlinkViewer with playback controls, selection actions, and close button */
export const ToolBar: React.FC<ToolBarProps> = memo(function ToolBar({
  currentIndex,
  totalFrames,
  isPlaying,
  blinkSpeed,
  onPrevious,
  onNext,
  onTogglePlay,
  onSpeedChange,
  selectionCount,
  blackholedInSelectionCount,
  nonBlackholedInSelectionCount,
  onBlackhole,
  onRestore,
  isBlackholing,
  isCaching,
  cacheProgress,
  onClose,
}) {
  return (
    <div className="flex items-center justify-between px-4 py-2 bg-gray-900 border-b border-gray-700">
      {/* Left: Playback controls */}
      <div className="flex items-center gap-2">
        <button
          onClick={onPrevious}
          disabled={currentIndex === 0}
          className="p-2 rounded bg-gray-800 hover:bg-gray-700 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
        >
          <ChevronLeft className="text-white" size={20} />
        </button>

        <button
          onClick={onTogglePlay}
          className="p-2 rounded bg-blue-600 hover:bg-blue-700 transition-colors"
        >
          {isPlaying ? (
            <Pause className="text-white" size={20} />
          ) : (
            <Play className="text-white" size={20} />
          )}
        </button>

        <button
          onClick={onNext}
          disabled={currentIndex === totalFrames - 1}
          className="p-2 rounded bg-gray-800 hover:bg-gray-700 disabled:opacity-50 disabled:cursor-not-allowed transition-colors"
        >
          <ChevronRight className="text-white" size={20} />
        </button>

        {/* Speed control */}
        <div className="flex items-center gap-2 ml-4">
          <label className="text-sm text-gray-400">Speed:</label>
          <input
            type="range"
            min="0.5"
            max="25"
            step="0.5"
            value={blinkSpeed}
            onChange={(e) => onSpeedChange(parseFloat(e.target.value))}
            className="w-24"
          />
          <span className="text-sm text-white w-14">{blinkSpeed} FPS</span>
        </div>
      </div>

      {/* Center: Frame counter + selection count + caching progress */}
      <div className="flex items-center gap-4">
        <div className="text-white text-sm">
          <span className="font-semibold">{currentIndex + 1}</span>
          <span className="text-gray-400"> / {totalFrames}</span>
        </div>

        {selectionCount > 0 && (
          <div className="text-sm text-yellow-400 font-medium">
            {selectionCount} selected
          </div>
        )}

        {isCaching && (
          <div className="flex items-center gap-2 text-sm text-gray-400">
            <Loader2 className="animate-spin" size={14} />
            <span>Caching {cacheProgress.current}/{cacheProgress.total}</span>
          </div>
        )}
      </div>

      {/* Right: Selection actions + Close button */}
      <div className="flex items-center gap-2">
        {/* Selection controls */}
        {selectionCount > 0 && (
          <>
            {/* Restore button - shown when selection includes blackholed frames */}
            {blackholedInSelectionCount > 0 && (
              <button
                onClick={onRestore}
                disabled={isBlackholing}
                className="flex items-center gap-1 px-3 py-1.5 text-sm bg-green-600 hover:bg-green-700 text-white rounded transition-colors disabled:opacity-50"
                title="Restore selected blackholed frames"
              >
                <RotateCcw size={16} />
                Restore ({blackholedInSelectionCount})
              </button>
            )}
            {/* Blackhole button - shown when selection includes non-blackholed frames */}
            {nonBlackholedInSelectionCount > 0 && (
              <button
                onClick={onBlackhole}
                className="flex items-center gap-1 px-3 py-1.5 text-sm bg-red-600 hover:bg-red-700 text-white rounded transition-colors"
                title="Send selected frames to blackhole"
              >
                <Trash2 size={16} />
                Blackhole ({nonBlackholedInSelectionCount})
              </button>
            )}
          </>
        )}

        <div className="text-xs text-gray-500 mr-2">
          Space: Select | Enter: Play | ↑↓: Navigate | ←→: Speed
        </div>
        <button
          onClick={onClose}
          className="p-2 hover:bg-gray-800 rounded transition-colors"
        >
          <X className="text-white" size={20} />
        </button>
      </div>
    </div>
  );
});
