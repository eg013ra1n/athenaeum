import { Loader2, FolderSearch, Database, Sparkles } from 'lucide-react';
import { useScanProgressContext } from '../contexts/ScanProgressContext';

const phaseConfig: Record<string, { label: string; icon: typeof Loader2 }> = {
  discovery: { label: 'Discovering files...', icon: FolderSearch },
  processing: { label: 'Processing files', icon: Loader2 },
  inserting: { label: 'Saving to database', icon: Database },
  calibrating: { label: 'Creating calibration sets', icon: Sparkles },
};

export function ScanProgressIndicator() {
  const { activeScans } = useScanProgressContext();

  // Find active (non-complete) scans
  const activeScan = Array.from(activeScans.values()).find((s) => !s.isComplete);

  // Don't render if no active scan
  if (!activeScan) {
    return null;
  }

  const { rootPath, progress } = activeScan;
  const folderName = rootPath.split('/').pop() || rootPath;

  const phase = progress?.phase || 'discovery';
  const config = phaseConfig[phase] || phaseConfig.processing;
  const PhaseIcon = config.icon;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center bg-black/70 backdrop-blur-sm">
      <div className="bg-gray-800 border border-gray-700 rounded-xl p-6 w-[400px] shadow-2xl">
        {/* Header */}
        <div className="flex items-center gap-3 mb-4">
          <div className="p-2 bg-blue-600/20 rounded-lg">
            <PhaseIcon className="text-blue-400 animate-pulse" size={24} />
          </div>
          <div>
            <h2 className="text-lg font-semibold text-white">{config.label}</h2>
            <p className="text-sm text-gray-400 truncate" title={rootPath}>
              {folderName}
            </p>
          </div>
        </div>

        {/* Progress section */}
        {progress ? (
          <>
            {/* Progress bar */}
            <div className="h-2 bg-gray-700 rounded-full overflow-hidden mb-3">
              <div
                className="h-full bg-blue-500 transition-all duration-300"
                style={{ width: `${progress.percent}%` }}
              />
            </div>

            {/* Stats */}
            <div className="flex items-center justify-between text-sm mb-2">
              <span className="text-gray-300">
                {progress.current.toLocaleString()} / {progress.total.toLocaleString()} files
              </span>
              <span className="text-blue-400 font-medium">
                {progress.percent.toFixed(1)}%
              </span>
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

        {/* Footer message */}
        <p className="text-xs text-gray-500 mt-4 text-center">
          Please wait while the scan completes...
        </p>
      </div>
    </div>
  );
}
