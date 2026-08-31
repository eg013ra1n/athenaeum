import { Loader2, FolderOutput, Check, AlertCircle, X, XCircle } from 'lucide-react';
import { useExportProgressContext } from '../contexts/ExportProgressContext';

const phaseConfig: Record<string, { label: string }> = {
  collecting: { label: 'Collecting export data...' },
  copying: { label: 'Exporting files' },
  // Calibrated-lights mode: this file is being generated from its masters
  // rather than copied, which is slow enough that saying so matters.
  calibrating: { label: 'Calibrating…' },
};

export function ExportProgressIndicator() {
  const { activeExports, dismissCompletedExport, cancelExport } = useExportProgressContext();

  // Find the first active or just-completed export to display
  const activeExport = Array.from(activeExports.values()).find((e) => !e.isComplete)
    ?? Array.from(activeExports.values()).find((e) => e.isComplete);

  if (!activeExport) {
    return null;
  }

  const { frameSetId, progress, isComplete, isCancelling, result } = activeExport;

  // Completed state
  if (isComplete && result) {
    return (
      <div className="fixed bottom-4 right-4 z-50 w-[380px]">
        <div className="bg-surface-elevated border border-border rounded-xl p-4 shadow-2xl">
          {/* Header */}
          <div className="flex items-center gap-3 mb-3">
            <div className={`p-1.5 rounded-lg ${result.success ? 'bg-success/20' : 'bg-error/20'}`}>
              {result.success ? (
                <Check className="text-success" size={20} />
              ) : (
                <AlertCircle className="text-error" size={20} />
              )}
            </div>
            <div className="flex-1 min-w-0">
              <h2 className="text-sm font-semibold text-white">
                {result.success ? 'Export Complete' : 'Export Failed'}
              </h2>
            </div>
            <button
              onClick={() => dismissCompletedExport(frameSetId)}
              className="p-1 text-content-muted hover:text-content-secondary rounded transition"
            >
              <X size={16} />
            </button>
          </div>

          {/* Result details */}
          {result.success ? (
            <div className="text-xs text-content-secondary space-y-0.5">
              <div>Files organized: {result.filesOrganized}</div>
              <div className="truncate text-content-muted" title={result.outputDir}>
                {result.outputDir}
              </div>
            </div>
          ) : (
            <div className="text-xs text-error">{result.error}</div>
          )}

          {result.warnings.length > 0 && (
            <div className="mt-2 text-xs text-warning">
              {result.warnings.slice(0, 3).map((warning, i) => (
                <div key={i}>&#8226; {warning}</div>
              ))}
              {result.warnings.length > 3 && (
                <div className="text-content-muted">...and {result.warnings.length - 3} more</div>
              )}
            </div>
          )}
        </div>
      </div>
    );
  }

  // In-progress state
  const phase = progress?.phase || 'collecting';
  const config = phaseConfig[phase] || phaseConfig.collecting;

  return (
    <div className="fixed bottom-4 right-4 z-50 w-[380px]">
      <div className="bg-surface-elevated border border-border rounded-xl p-4 shadow-2xl">
        {/* Header */}
        <div className="flex items-center gap-3 mb-3">
          <div className="p-1.5 bg-accent/20 rounded-lg">
            <FolderOutput className="text-accent animate-pulse" size={20} />
          </div>
          <div className="flex-1 min-w-0">
            <h2 className="text-sm font-semibold text-white">
              {isCancelling ? 'Cancelling...' : config.label}
            </h2>
          </div>
          {!isCancelling && (
            <button
              onClick={() => cancelExport(frameSetId)}
              className="p-1 text-content-muted hover:text-error rounded transition"
              title="Cancel export"
            >
              <XCircle size={16} />
            </button>
          )}
        </div>

        {/* Progress section */}
        {progress && progress.total > 0 ? (
          <>
            {/* Progress bar */}
            <div className="h-1.5 bg-surface-hover rounded-full overflow-hidden mb-2">
              <div
                className={`h-full transition-all duration-300 ${isCancelling ? 'bg-warning' : 'bg-accent'}`}
                style={{ width: `${progress.percent}%` }}
              />
            </div>

            {/* Stats */}
            <div className="flex items-center justify-between text-xs mb-1">
              <span className="text-content-secondary">
                {progress.current.toLocaleString()} / {progress.total.toLocaleString()} files
              </span>
              <span className={`font-medium ${isCancelling ? 'text-warning' : 'text-accent'}`}>
                {progress.percent.toFixed(1)}%
              </span>
            </div>

            {/* Current file */}
            {progress.currentFile && (
              <div
                className="text-xs text-content-muted truncate"
                title={progress.currentFile}
              >
                {progress.currentFile}
              </div>
            )}
          </>
        ) : (
          <div className="flex items-center gap-2 text-xs text-content-muted">
            <Loader2 className="animate-spin" size={14} />
            <span>{phase === 'collecting' ? 'Preparing export data...' : 'Starting export...'}</span>
          </div>
        )}
      </div>
    </div>
  );
}
