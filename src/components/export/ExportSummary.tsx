import { Loader2 } from 'lucide-react';
import { useExportSummary } from '../../hooks/useExportData';
import { EquipmentHeader } from './EquipmentHeader';
import { FilterGroupCard } from './FilterGroupCard';
import { FolderStructurePreview } from './FolderStructurePreview';
import type { DetailedWarning } from '../../types/export';

interface ExportSummaryProps {
  frameSetId: number;
}

/**
 * Main export summary container that orchestrates all summary components
 */
export function ExportSummary({ frameSetId }: ExportSummaryProps) {
  const { summary, loading, error } = useExportSummary(frameSetId);

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12">
        <Loader2 className="w-8 h-8 animate-spin text-accent" />
        <span className="ml-3 text-content-muted">Loading export summary...</span>
      </div>
    );
  }

  if (error) {
    return (
      <div className="p-4 bg-error/10 border border-error/30 rounded-lg">
        <h3 className="font-medium text-error mb-1">Failed to load export summary</h3>
        <p className="text-sm text-content-muted">{error}</p>
      </div>
    );
  }

  if (!summary) {
    return (
      <div className="p-4 text-center text-content-muted">
        No export data available
      </div>
    );
  }

  // Filter warnings to only include actionable ones (exclude missing calibration - already shown in UI)
  const getWarningsForFilter = (filterName: string | null): DetailedWarning[] => {
    const name = filterName || 'Unfiltered';
    return summary.warnings.filter(
      (w) =>
        w.filter === name &&
        w.warningType !== 'missing_calibration' // Already shown inline
    );
  };

  return (
    <div className="space-y-6">
      {/* Frame Set Header */}
      <div className="border-b border-border pb-4">
        <h2 className="text-xl font-semibold text-content">
          {summary.objectName || summary.frameSetName}
        </h2>
        {summary.objectName && summary.frameSetName !== summary.objectName && (
          <p className="text-sm text-content-muted mt-1">{summary.frameSetName}</p>
        )}
      </div>

      {/* Equipment Header */}
      <EquipmentHeader
        cameras={summary.cameras}
        telescopes={summary.telescopes}
        dateRange={summary.dateRange}
      />

      {/* Filter Group Cards */}
      <div>
        <h3 className="text-sm font-medium text-content-muted uppercase tracking-wide mb-3">
          Filter Groups ({summary.filterGroups.length})
        </h3>
        <div className="space-y-4">
          {summary.filterGroups.map((group, index) => (
            <FilterGroupCard
              key={index}
              group={group}
              warnings={getWarningsForFilter(group.filter)}
            />
          ))}
        </div>
      </div>

      {/* Folder Structure Preview */}
      <FolderStructurePreview
        preview={summary.folderPreview}
        estimatedSizeBytes={summary.estimatedSizeBytes}
      />

      {/* Summary Stats Footer */}
      <div className="p-4 bg-surface-elevated rounded-lg border border-border">
        <div className="flex flex-wrap gap-6 text-sm">
          <div>
            <span className="text-content-muted">Total Frames: </span>
            <span className="font-medium text-content">{summary.totalFiles}</span>
          </div>
          <div>
            <span className="text-content-muted">Estimated Size: </span>
            <span className="font-medium text-content">{formatBytes(summary.estimatedSizeBytes)}</span>
          </div>
          <div>
            <span className="text-content-muted">Filter Groups: </span>
            <span className="font-medium text-content">{summary.filterGroups.length}</span>
          </div>
          {summary.cameras.length > 0 && (
            <div>
              <span className="text-content-muted">Cameras: </span>
              <span className="font-medium text-content">{summary.cameras.length}</span>
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

/**
 * Format bytes to human-readable string
 */
function formatBytes(bytes: number): string {
  const KB = 1024;
  const MB = KB * 1024;
  const GB = MB * 1024;
  const TB = GB * 1024;

  if (bytes >= TB) {
    return `${(bytes / TB).toFixed(1)} TB`;
  } else if (bytes >= GB) {
    return `${(bytes / GB).toFixed(1)} GB`;
  } else if (bytes >= MB) {
    return `${(bytes / MB).toFixed(1)} MB`;
  } else if (bytes >= KB) {
    return `${(bytes / KB).toFixed(1)} KB`;
  } else {
    return `${bytes} B`;
  }
}
