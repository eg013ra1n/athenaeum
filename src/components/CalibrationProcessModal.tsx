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
      <div className="bg-gray-800 rounded-lg border border-gray-700 max-w-2xl w-full max-h-[90vh] overflow-y-auto">
        {/* Header */}
        <div className="flex items-center justify-between p-6 border-b border-gray-700">
          <h3 className="text-xl font-semibold text-gray-100">
            Calibration Finder - {frameSetName}
          </h3>
          <button
            onClick={onClose}
            className="p-1 text-gray-400 hover:text-gray-200 transition-colors"
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
              <Loader2 size={48} className="mx-auto text-purple-500 animate-spin mb-4" />
              <p className="text-gray-300 font-medium mb-2">Finding calibration data...</p>
              <p className="text-sm text-gray-400">
                Matching frames with calibration sets based on camera settings and capture parameters
              </p>
            </div>
          )}

          {/* Error */}
          {error && !isProcessing && (
            <div className="bg-red-900/20 border border-red-800 rounded-lg p-4 flex items-start gap-3">
              <AlertTriangle className="text-red-500 flex-shrink-0 mt-0.5" size={20} />
              <div className="flex-1">
                <p className="font-medium text-red-400">Error</p>
                <p className="text-sm text-red-300 mt-1">{error}</p>
              </div>
            </div>
          )}

          {/* Results */}
          {stats && !isProcessing && !error && (
            <div className="space-y-4">
              {/* Summary */}
              <div className="bg-gray-700/50 rounded-lg p-4">
                <h4 className="font-semibold text-gray-200 mb-3">Processing Summary</h4>
                <div className="grid grid-cols-2 gap-3 text-sm">
                  <div>
                    <span className="text-gray-400">Total frames:</span>
                    <span className="ml-2 text-gray-100 font-medium">{stats.total_frames}</span>
                  </div>
                  <div>
                    <span className="text-gray-400">Sets linked:</span>
                    <span className="ml-2 text-gray-100 font-medium">
                      {stats.total_flat_sets_linked + stats.total_dark_sets_linked}
                    </span>
                  </div>
                </div>
              </div>

              {/* Calibration Completeness */}
              <div className="bg-gray-700/50 rounded-lg p-4">
                <h4 className="font-semibold text-gray-200 mb-3">Calibration Status</h4>
                <div className="space-y-2">
                  <div className="flex items-center justify-between">
                    <div className="flex items-center gap-2">
                      <CheckCircle size={16} className="text-green-500" />
                      <span className="text-sm text-gray-300">Full calibration</span>
                    </div>
                    <span className="text-sm font-medium text-gray-100">
                      {stats.frames_with_full_calibration}
                    </span>
                  </div>
                  <div className="flex items-center justify-between">
                    <div className="flex items-center gap-2">
                      <AlertTriangle size={16} className="text-yellow-500" />
                      <span className="text-sm text-gray-300">Partial calibration</span>
                    </div>
                    <span className="text-sm font-medium text-gray-100">
                      {stats.frames_with_partial_calibration}
                    </span>
                  </div>
                  <div className="flex items-center justify-between">
                    <div className="flex items-center gap-2">
                      <XCircle size={16} className="text-red-500" />
                      <span className="text-sm text-gray-300">No calibration</span>
                    </div>
                    <span className="text-sm font-medium text-gray-100">
                      {stats.frames_with_no_calibration}
                    </span>
                  </div>
                </div>
              </div>

              {/* Sets Linked */}
              <div className="bg-gray-700/50 rounded-lg p-4">
                <h4 className="font-semibold text-gray-200 mb-3">Calibration Sets</h4>
                <div className="space-y-2">
                  <div className="flex items-center justify-between text-sm">
                    <span className="text-gray-300">Flat sets linked:</span>
                    <span className="font-medium text-gray-100">{stats.total_flat_sets_linked}</span>
                  </div>
                  <div className="flex items-center justify-between text-sm">
                    <span className="text-gray-300">Dark sets linked:</span>
                    <span className="font-medium text-gray-100">{stats.total_dark_sets_linked}</span>
                  </div>
                </div>
              </div>

              {/* Warnings */}
              {stats.total_warnings > 0 && (
                <div className="bg-yellow-900/20 border border-yellow-800 rounded-lg p-4">
                  <h4 className="font-semibold text-yellow-400 mb-3 flex items-center gap-2">
                    <AlertTriangle size={18} />
                    Warnings ({stats.total_warnings})
                  </h4>
                  <div className="space-y-2 text-sm">
                    {stats.date_warnings > 0 && (
                      <div className="flex items-center justify-between">
                        <span className="text-yellow-300">Calibration age warnings:</span>
                        <span className="font-medium text-yellow-200">{stats.date_warnings}</span>
                      </div>
                    )}
                    {stats.temp_warnings > 0 && (
                      <div className="flex items-center justify-between">
                        <span className="text-yellow-300">Temperature warnings:</span>
                        <span className="font-medium text-yellow-200">{stats.temp_warnings}</span>
                      </div>
                    )}
                  </div>
                  <p className="text-xs text-yellow-300/70 mt-3">
                    Warnings indicate that calibration was found but may not be optimal.
                    Consider capturing new calibration frames if possible.
                  </p>
                </div>
              )}

              {/* Missing Calibration */}
              {(stats.missing_flats > 0 || stats.missing_darks > 0 || stats.missing_bias > 0) && (
                <div className="bg-red-900/20 border border-red-800 rounded-lg p-4">
                  <h4 className="font-semibold text-red-400 mb-3 flex items-center gap-2">
                    <XCircle size={18} />
                    Missing Calibration
                  </h4>
                  <div className="space-y-2 text-sm">
                    {stats.missing_flats > 0 && (
                      <div className="flex items-center justify-between">
                        <span className="text-red-300">Frames missing Flats:</span>
                        <span className="font-medium text-red-200">{stats.missing_flats}</span>
                      </div>
                    )}
                    {stats.missing_darks > 0 && (
                      <div className="flex items-center justify-between">
                        <span className="text-red-300">Frames missing Darks:</span>
                        <span className="font-medium text-red-200">{stats.missing_darks}</span>
                      </div>
                    )}
                    {stats.missing_bias > 0 && (
                      <div className="flex items-center justify-between">
                        <span className="text-red-300">Frames missing Bias:</span>
                        <span className="font-medium text-red-200">{stats.missing_bias}</span>
                      </div>
                    )}
                  </div>
                  <p className="text-xs text-red-300/70 mt-3">
                    Consider capturing calibration frames with matching camera settings, or check the Equipment tab
                    to see available calibration sets.
                  </p>
                </div>
              )}

              {/* Success message */}
              {stats.frames_with_full_calibration === stats.total_frames && stats.total_warnings === 0 && (
                <div className="bg-green-900/20 border border-green-800 rounded-lg p-4 text-center">
                  <CheckCircle size={32} className="mx-auto text-green-500 mb-2" />
                  <p className="font-medium text-green-400">
                    Perfect! All frames have complete calibration with no warnings.
                  </p>
                </div>
              )}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex items-center justify-end gap-3 p-6 border-t border-gray-700">
          <button
            onClick={onClose}
            disabled={isProcessing}
            className="px-4 py-2 bg-gray-700 hover:bg-gray-600 disabled:bg-gray-700 disabled:cursor-not-allowed text-gray-200 rounded transition-colors"
          >
            {isProcessing ? 'Processing...' : 'Close'}
          </button>
        </div>
      </div>
    </div>
  );
}
