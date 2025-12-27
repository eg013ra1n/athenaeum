import { Loader2 } from 'lucide-react';
import type { ExportableFrameSet } from '../../types/export';

interface FrameSetSelectorProps {
  frameSets: ExportableFrameSet[];
  loading: boolean;
  selectedId: number | null;
  onSelect: (id: number) => void;
}

function formatExposure(seconds: number): string {
  if (seconds < 60) {
    return `${seconds.toFixed(0)}s`;
  }
  if (seconds < 3600) {
    return `${(seconds / 60).toFixed(1)}m`;
  }
  return `${(seconds / 3600).toFixed(1)}h`;
}

export function FrameSetSelector({
  frameSets,
  loading,
  selectedId,
  onSelect,
}: FrameSetSelectorProps) {
  if (loading) {
    return (
      <div className="flex items-center justify-center py-8">
        <Loader2 className="h-6 w-6 animate-spin text-blue-500" />
        <span className="ml-2 text-gray-400">Loading frame sets...</span>
      </div>
    );
  }

  if (frameSets.length === 0) {
    return (
      <div className="text-center py-8 text-gray-400">
        No frame sets available for export. Create frame sets in the Objects view first.
      </div>
    );
  }

  return (
    <div className="space-y-2 max-h-64 overflow-y-auto">
      {frameSets.map((frameSet) => (
        <button
          key={frameSet.id}
          onClick={() => onSelect(frameSet.id)}
          className={`w-full text-left p-3 rounded-lg border transition-colors ${
            selectedId === frameSet.id
              ? 'bg-blue-600/20 border-blue-500'
              : 'bg-gray-700/50 border-gray-600 hover:bg-gray-700 hover:border-gray-500'
          }`}
        >
          <div className="flex items-center justify-between">
            <div>
              <div className="font-medium">
                {frameSet.name || frameSet.objectName || `Frame Set ${frameSet.id}`}
              </div>
              <div className="text-sm text-gray-400 mt-1">
                {frameSet.frameCount} frames • {formatExposure(frameSet.totalExposureSeconds)}
                {frameSet.filters.length > 0 && (
                  <span className="ml-2">
                    • {frameSet.filters.join(', ')}
                  </span>
                )}
              </div>
            </div>
            {selectedId === frameSet.id && (
              <div className="w-4 h-4 rounded-full bg-blue-500" />
            )}
          </div>
        </button>
      ))}
    </div>
  );
}
