import { X, CheckCircle, AlertTriangle, XCircle, Loader2 } from 'lucide-react';
import type { ProcessingStats } from '../types/models';

interface CalibrationProcessModalProps {
  frameSetName: string;
  isProcessing: boolean;
  stats: ProcessingStats | null;
  error: string | null;
  onClose: () => void;
}

export function CalibrationProcessModal({
  frameSetName,
  isProcessing,
  stats,
  error,
  onClose
}: CalibrationProcessModalProps) {
  return (
    <div className="fixed inset-0 bg-black/70 flex items-center justify-center z-50 p-4">
      <div className="bg-surface-elevated rounded-lg border border-border max-w-2xl w-full max-h-[90vh] overflow-y-auto">
        {/* Header */}
        <div className="flex items-center justify-between p-6 border-b border-border">
          <h3 className="text-xl font-semibold text-content">
            Calibration Finder - {frameSetName}
          </h3>
          <button
            onClick={onClose}
            className="p-1 text-content-muted hover:text-content transition-colors"
            disabled={isProcessing}
          >
            <X size={20} />
          </button>
        </div>

        {/* Content */}
        <div className="p-6">
          {/* Processing */}
          {isProcessing && (
            <div className="text-center py-8">
              <Loader2 size={48} className="mx-auto text-purple animate-spin mb-4" />
              <p className="text-content-secondary font-medium mb-2">Finding calibration data...</p>
              <p className="text-sm text-content-muted">
                Matching frames with calibration sets based on camera settings and capture parameters
              </p>
            </div>
          )}

          {/* Error */}
          {error && !isProcessing && (
            <div className="bg-error-muted border border-error rounded-lg p-4 flex items-start gap-3">
              <AlertTriangle className="text-error flex-shrink-0 mt-0.5" size={20} />
              <div className="flex-1">
                <p className="font-medium text-error">Error</p>
                <p className="text-sm text-error mt-1">{error}</p>
              </div>
            </div>
          )}

          {/* Results */}
          {stats && !isProcessing && !error && (
            <div className="space-y-4">
              {/* Summary */}
              <div className="bg-surface-hover/50 rounded-lg p-4">
                <h4 className="font-semibold text-content mb-3">Processing Summary</h4>
                <div className="grid grid-cols-2 gap-3 text-sm">
                  <div>
                    <span className="text-content-muted">Total frames:</span>
                    <span className="ml-2 text-content font-medium">{stats.total_frames}</span>
                  </div>
                  <div>
                    <span className="text-content-muted">Sets linked:</span>
                    <span className="ml-2 text-content font-medium">
                      {stats.total_flat_sets_linked + stats.total_dark_sets_linked}
                    </span>
                  </div>
                </div>
              </div>

              {/* Calibration Completeness */}
              <div className="bg-surface-hover/50 rounded-lg p-4">
                <h4 className="font-semibold text-content mb-3">Calibration Status</h4>
                <div className="space-y-2">
                  <div className="flex items-center justify-between">
                    <div className="flex items-center gap-2">
                      <CheckCircle size={16} className="text-success" />
                      <span className="text-sm text-content-secondary">Full calibration</span>
                    </div>
                    <span className="text-sm font-medium text-content">
                      {stats.frames_with_full_calibration}
                    </span>
                  </div>
                  <div className="flex items-center justify-between">
                    <div className="flex items-center gap-2">
                      <AlertTriangle size={16} className="text-warning" />
                      <span className="text-sm text-content-secondary">Partial calibration</span>
                    </div>
                    <span className="text-sm font-medium text-content">
                      {stats.frames_with_partial_calibration}
                    </span>
                  </div>
                  <div className="flex items-center justify-between">
                    <div className="flex items-center gap-2">
                      <XCircle size={16} className="text-error" />
                      <span className="text-sm text-content-secondary">No calibration</span>
                    </div>
                    <span className="text-sm font-medium text-content">
                      {stats.frames_with_no_calibration}
                    </span>
                  </div>
                </div>
              </div>

              {/* Sets Linked */}
              <div className="bg-surface-hover/50 rounded-lg p-4">
                <h4 className="font-semibold text-content mb-3">Calibration Sets</h4>
                <div className="space-y-2">
                  <div className="flex items-center justify-between text-sm">
                    <span className="text-content-secondary">Flat sets linked:</span>
                    <span className="font-medium text-content">{stats.total_flat_sets_linked}</span>
                  </div>
                  <div className="flex items-center justify-between text-sm">
                    <span className="text-content-secondary">Dark sets linked:</span>
                    <span className="font-medium text-content">{stats.total_dark_sets_linked}</span>
                  </div>
                </div>
              </div>

              {/* Warnings */}
              {stats.total_warnings > 0 && (
                <div className="bg-warning-muted border border-warning rounded-lg p-4">
                  <h4 className="font-semibold text-warning mb-3 flex items-center gap-2">
                    <AlertTriangle size={18} />
                    Warnings ({stats.total_warnings})
                  </h4>
                  <div className="space-y-2 text-sm">
                    {stats.date_warnings > 0 && (
                      <div className="flex items-center justify-between">
                        <span className="text-warning">Calibration age warnings:</span>
                        <span className="font-medium text-warning">{stats.date_warnings}</span>
                      </div>
                    )}
                    {stats.temp_warnings > 0 && (
                      <div className="flex items-center justify-between">
                        <span className="text-warning">Temperature warnings:</span>
                        <span className="font-medium text-warning">{stats.temp_warnings}</span>
                      </div>
                    )}
                  </div>
                  <p className="text-xs text-warning/70 mt-3">
                    Warnings indicate that calibration was found but may not be optimal.
                    Consider capturing new calibration frames if possible.
                  </p>
                </div>
              )}

              {/* Missing Calibration */}
              {(stats.missing_flats > 0 || stats.missing_darks > 0 || stats.missing_bias > 0) && (
                <div className="bg-error-muted border border-error rounded-lg p-4">
                  <h4 className="font-semibold text-error mb-3 flex items-center gap-2">
                    <XCircle size={18} />
                    Missing Calibration
                  </h4>
                  <div className="space-y-2 text-sm">
                    {stats.missing_flats > 0 && (
                      <div className="flex items-center justify-between">
                        <span className="text-error">Frames missing Flats:</span>
                        <span className="font-medium text-error">{stats.missing_flats}</span>
                      </div>
                    )}
                    {stats.missing_darks > 0 && (
                      <div className="flex items-center justify-between">
                        <span className="text-error">Frames missing Darks:</span>
                        <span className="font-medium text-error">{stats.missing_darks}</span>
                      </div>
                    )}
                    {stats.missing_bias > 0 && (
                      <div className="flex items-center justify-between">
                        <span className="text-error">Frames missing Bias:</span>
                        <span className="font-medium text-error">{stats.missing_bias}</span>
                      </div>
                    )}
                  </div>
                  <p className="text-xs text-error/70 mt-3">
                    Consider capturing calibration frames with matching camera settings, or check the Equipment tab
                    to see available calibration sets.
                  </p>
                </div>
              )}

              {/* Success message */}
              {stats.frames_with_full_calibration === stats.total_frames && stats.total_warnings === 0 && (
                <div className="bg-success-muted border border-success rounded-lg p-4 text-center">
                  <CheckCircle size={32} className="mx-auto text-success mb-2" />
                  <p className="font-medium text-success">
                    Perfect! All frames have complete calibration with no warnings.
                  </p>
                </div>
              )}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-end gap-3 p-6 border-t border-border">
          <button
            onClick={onClose}
            disabled={isProcessing}
            className="px-4 py-2 bg-surface-hover hover:bg-surface-hover disabled:bg-surface-hover disabled:cursor-not-allowed text-content rounded transition-colors"
          >
            {isProcessing ? 'Processing...' : 'Close'}
          </button>
        </div>
      </div>
    </div>
  );
}
