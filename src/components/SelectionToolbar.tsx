/**
 * Toolbar component for spatial selection tool
 * Provides button to activate rectangle selection mode
 */

import { Square } from 'lucide-react';
import { DrawingMode } from '../types/selection';

export interface SelectionToolbarProps {
  activeMode: DrawingMode;
  onModeChange: (mode: DrawingMode) => void;
  isDisabled?: boolean;
}

/**
 * Toolbar for selection tool activation
 */
export function SelectionToolbar({
  activeMode,
  onModeChange,
  isDisabled = false
}: SelectionToolbarProps) {
  return (
    <div className="absolute bottom-4 left-4 z-20 flex items-center gap-2">
      <span className="text-sm font-medium text-gray-300">Select</span>

      <button
        onClick={() => onModeChange(activeMode === 'rectangle' ? 'none' : 'rectangle')}
        disabled={isDisabled}
        title="Select frames in a rectangular region (Press S)"
        className={`w-10 h-10 rounded transition flex items-center justify-center ${
          activeMode === 'rectangle'
            ? 'bg-blue-500 text-white shadow-lg'
            : 'bg-gray-700 text-gray-300 hover:bg-gray-600'
        } ${isDisabled ? 'opacity-50 cursor-not-allowed' : ''}`}
      >
        <div className="relative w-[18px] h-[18px] flex items-center justify-center">
          <Square size={18} className="absolute" />
          <span className="text-[9px] font-bold relative z-10">S</span>
        </div>
      </button>
    </div>
  );
}
