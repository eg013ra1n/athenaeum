import { Loader2, FolderSearch, Database, Sparkles, XCircle } from 'lucide-react';
import { useScanProgressContext } from '../contexts/ScanProgressContext';

const phaseConfig: Record<string, { label: string; icon: typeof Loader2 }> = {
  discovery: { label: 'Discovering files...', icon: FolderSearch },
  processing: { label: 'Processing files', icon: Loader2 },
  inserting: { label: 'Saving to database', icon: Database },
  calibrating: { label: 'Creating calibration sets', icon: Sparkles },
  caching: { label: 'Building duplicate cache...', icon: Database },
  verifying: { label: 'Verifying files on disk...', icon: FolderSearch },
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
      <div className="bg-gray-800 border border-gray-700 rounded-xl p-6 w-[400px] shadow-2xl">
        {/* Header */}
        <div className="flex items-center gap-3 mb-4">
          <div className="p-2 bg-blue-600/20 rounded-lg">
            <PhaseIcon className="text-blue-400 animate-pulse" size={24} />
          </div>
          <div className="flex-1">
            <h2 className="text-lg font-semibold text-white">
              {isCancelling ? 'Cancelling...' : config.label}
            </h2>
            <p className="text-sm text-gray-400 truncate" title={rootPath}>
              {folderName}
            </p>
          </div>
        </div>

        {/* Progress section */}
        {progress ? (
          <>
            {/* Progress bar - indeterminate during discovery (total=0) */}
            <div className="h-2 bg-gray-700 rounded-full overflow-hidden mb-3">
              {progress.total > 0 ? (
                <div
                  className={`h-full transition-all duration-300 ${isCancelling ? 'bg-yellow-500' : 'bg-blue-500'}`}
                  style={{ width: `${progress.percent}%` }}
                />
              ) : (
                <div className="h-full bg-blue-500 animate-pulse" style={{ width: '100%' }} />
              )}
            </div>

            {/* Stats */}
            <div className="flex items-center justify-between text-sm mb-2">
              <span className="text-gray-300">
                {progress.total > 0 ? (
                  `${progress.current.toLocaleString()} / ${progress.total.toLocaleString()} files`
                ) : (
                  `${progress.current.toLocaleString()} files found`
                )}
              </span>
              {progress.total > 0 && (
                <span className={`font-medium ${isCancelling ? 'text-yellow-400' : 'text-blue-400'}`}>
                  {progress.percent.toFixed(1)}%
                </span>
              )}
            </div>

            {/* Current file */}
            {progress.current_file && (
              <div
                className="text-xs text-gray-500 truncate"
                title={progress.current_file}
              >
                {progress.current_file}
              </div>
            )}
          </>
        ) : (
          <div className="flex items-center gap-2 text-gray-400">
            <Loader2 className="animate-spin" size={16} />
            <span>Starting scan...</span>
          </div>
        )}

        {/* Footer with cancel button */}
        <div className="mt-4 flex items-center justify-between">
          <p className="text-xs text-gray-500">
            {isCancelling ? 'Stopping scan...' : 'Please wait while the scan completes...'}
          </p>
          {!isCancelling && (
            <button
              onClick={handleCancel}
              className="flex items-center gap-1.5 px-3 py-1.5 text-sm text-red-400 hover:text-red-300 hover:bg-red-900/30 rounded-lg transition"
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
