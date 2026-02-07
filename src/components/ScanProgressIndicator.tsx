import { Loader2, FolderSearch, Database, Sparkles, XCircle } from 'lucide-react';
import { useScanProgressContext } from '../contexts/ScanProgressContext';

const phaseConfig: Record<string, { label: string; icon: typeof Loader2 }> = {
  verifying: { label: 'Verifying existing files...', icon: FolderSearch },
  discovery: { label: 'Discovering files...', icon: FolderSearch },
  processing: { label: 'Processing files', icon: Loader2 },
  inserting: { label: 'Saving to database', icon: Database },
  calibrating: { label: 'Creating calibration sets', icon: Sparkles },
  caching: { label: 'Building duplicate cache...', icon: Database },
};

export function ScanProgressIndicator() {
  const { activeScans, cancelScan } = useScanProgressContext();

  // Find active (non-complete) scans
  const activeScan = Array.from(activeScans.values()).find((s) => !s.isComplete);

  // Don't render if no active scan
  if (!activeScan) {
    return null;
  }

  const { rootId, rootPath, progress, isCancelling } = activeScan;
  const folderName = rootPath.split('/').pop() || rootPath;

  const phase = progress?.phase || 'discovery';
  const config = phaseConfig[phase] || phaseConfig.processing;
  const PhaseIcon = config.icon;

  const handleCancel = () => {
    cancelScan(rootId);
  };

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 backdrop-blur-sm">
      <div className="bg-surface-elevated border border-border rounded-xl p-6 w-[400px] shadow-2xl">
        {/* Header */}
        <div className="flex items-center gap-3 mb-4">
          <div className="p-2 bg-accent/20 rounded-lg">
            <PhaseIcon className="text-accent animate-pulse" size={24} />
          </div>
          <div className="flex-1">
            <h2 className="text-lg font-semibold text-white">
              {isCancelling ? 'Cancelling...' : config.label}
            </h2>
            <p className="text-sm text-content-muted truncate" title={rootPath}>
              {folderName}
            </p>
          </div>
        </div>

        {/* Progress section */}
        {progress ? (
          <>
            {/* Progress bar - indeterminate during discovery (total=0) */}
            <div className="h-2 bg-surface-hover rounded-full overflow-hidden mb-3">
              {progress.total > 0 ? (
                <div
                  className={`h-full transition-all duration-300 ${isCancelling ? 'bg-warning' : 'bg-accent'}`}
                  style={{ width: `${progress.percent}%` }}
                />
              ) : (
                <div className="h-full bg-accent animate-pulse" style={{ width: '100%' }} />
              )}
            </div>

            {/* Stats */}
            <div className="flex items-center justify-between text-sm mb-2">
              <span className="text-content-secondary">
                {progress.total > 0 ? (
                  `${progress.current.toLocaleString()} / ${progress.total.toLocaleString()} files`
                ) : (
                  `${progress.current.toLocaleString()} files found`
                )}
              </span>
              {progress.total > 0 && (
                <span className={`font-medium ${isCancelling ? 'text-warning' : 'text-accent'}`}>
                  {progress.percent.toFixed(1)}%
                </span>
              )}
            </div>

            {/* Current file */}
            {progress.current_file && (
              <div
                className="text-xs text-content-muted truncate"
                title={progress.current_file}
              >
                {progress.current_file}
              </div>
            )}
          </>
        ) : (
          <div className="flex items-center gap-2 text-content-muted">
            <Loader2 className="animate-spin" size={16} />
            <span>Starting scan...</span>
          </div>
        )}

        {/* Footer with cancel button */}
        <div className="mt-4 flex items-center justify-between">
          <p className="text-xs text-content-muted">
            {isCancelling ? 'Stopping scan...' : 'Please wait while the scan completes...'}
          </p>
          {!isCancelling && (
            <button
              onClick={handleCancel}
              className="flex items-center gap-1.5 px-3 py-1.5 text-sm text-error hover:text-error/80 hover:bg-error-muted rounded-lg transition"
            >
              <XCircle size={16} />
              Cancel
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
