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
        <Loader2 className="h-6 w-6 animate-spin text-accent" />
        <span className="ml-2 text-content-muted">Loading frame sets...</span>
      </div>
    );
  }

  if (frameSets.length === 0) {
    return (
      <div className="text-center py-8 text-content-muted">
        No frame sets available for export. Create frame sets in the Objects view first.
      </div>
    );
  }

  return (
    <div className="space-y-1.5 overflow-y-auto flex-1 min-h-0">
      {frameSets.map((frameSet) => (
        <button
          key={frameSet.id}
          onClick={() => onSelect(frameSet.id)}
          className={`w-full text-left px-3 py-2 rounded-lg border transition-colors ${
            selectedId === frameSet.id
              ? 'bg-accent/20 border-accent'
              : 'bg-surface-hover/50 border-border hover:bg-surface-hover hover:border-content-muted'
          }`}
        >
          <div className="flex items-center justify-between">
            <div className="min-w-0">
              <div className="font-medium truncate">
                {frameSet.name || frameSet.objectName || `Frame Set ${frameSet.id}`}
              </div>
              <div className="text-xs text-content-muted mt-0.5">
                {frameSet.frameCount} frames • {formatExposure(frameSet.totalExposureSeconds)}
                {frameSet.filters.length > 0 && (
                  <span className="ml-1">
                    • {frameSet.filters.join(', ')}
                  </span>
                )}
              </div>
            </div>
            {selectedId === frameSet.id && (
              <div className="w-3 h-3 rounded-full bg-accent flex-shrink-0 ml-2" />
            )}
          </div>
        </button>
      ))}
    </div>
  );
}
