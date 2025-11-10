/**
 * Toolbar component for spatial selection tools
 * Provides buttons to activate different selection modes
 */

import { Circle, Square, Lasso, X } from 'lucide-react';
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
  const tools: Array<{
    mode: Exclude<DrawingMode, 'none'>;
    icon: React.ReactNode;
    label: string;
    description: string;
  }> = [
    {
      mode: 'circle',
      icon: <Circle size={20} />,
      label: 'Circle',
      description: 'Select frames in a circular region'
    },
    {
      mode: 'rectangle',
      icon: <Square size={20} />,
      label: 'Rectangle',
      description: 'Select frames in a rectangular region'
    },
    {
      mode: 'polygon',
      icon: <Lasso size={20} />,
      label: 'Polygon',
      description: 'Select frames in a polygonal region'
    }
  ];

  return (
    <div className="flex-shrink-0 p-4 border-b border-gray-700 bg-gray-800">
      <div className="flex items-center gap-2">
        <span className="text-sm font-medium text-gray-300 mr-2">Selection Tool:</span>

        {tools.map((tool) => (
          <button
            key={tool.mode}
            onClick={() => onModeChange(activeMode === tool.mode ? 'none' : tool.mode)}
            disabled={isDisabled}
            title={tool.description}
            className={`p-2 rounded transition flex items-center gap-1 ${
              activeMode === tool.mode
                ? 'bg-blue-600 text-white shadow-lg'
                : 'bg-gray-700 text-gray-300 hover:bg-gray-600'
            } ${isDisabled ? 'opacity-50 cursor-not-allowed' : ''}`}
          >
            {tool.icon}
            <span className="text-sm">{tool.label}</span>
          </button>
        ))}

        {activeMode !== 'none' && (
          <button
            onClick={() => onModeChange('none')}
            disabled={isDisabled}
            title="Cancel selection"
            className="ml-2 p-2 rounded bg-red-700 hover:bg-red-600 text-white transition"
          >
            <X size={20} />
          </button>
        )}
      </div>

      {activeMode !== 'none' && (
        <div className="mt-2 text-sm text-gray-400">
          {activeMode === 'circle'
            ? 'Click to set center, drag to set radius'
            : activeMode === 'rectangle'
            ? 'Click and drag to draw rectangle'
            : 'Click to add vertices, double-click to finish'}
        </div>
      )}
    </div>
  );
}
