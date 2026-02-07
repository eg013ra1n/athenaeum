import { X, CheckCircle2, XCircle, Sun, Moon, Aperture, CircleDot, Eclipse, Layers, FileWarning, AlertTriangle, RefreshCw } from 'lucide-react';
import type { ScanResult } from '../types/models';

interface ScanSummaryModalProps {
  isOpen: boolean;
  onClose: () => void;
  scanResult: ScanResult;
  rootPath: string;
  missingFilesCount?: number;
}

export function ScanSummaryModal({ isOpen, onClose, scanResult, rootPath, missingFilesCount }: ScanSummaryModalProps) {
  if (!isOpen) return null;

  const { cancelled } = scanResult;
  const totalFrames = scanResult.lights_count + scanResult.darks_count +
    scanResult.flats_count + scanResult.bias_count + scanResult.darkflats_count;

  const frameTypes = [
    {
      label: 'Lights',
      count: scanResult.lights_count,
      icon: Sun,
      color: 'text-warning',
      bgColor: 'bg-warning-muted',
      borderColor: 'border-warning/50'
    },
    {
      label: 'Darks',
      count: scanResult.darks_count,
      icon: Moon,
      color: 'text-accent',
      bgColor: 'bg-info-muted',
      borderColor: 'border-info/50'
    },
    {
      label: 'Flats',
      count: scanResult.flats_count,
      icon: Aperture,
      color: 'text-purple',
      bgColor: 'bg-purple/30',
      borderColor: 'border-purple/50'
    },
    {
      label: 'Bias',
      count: scanResult.bias_count,
      icon: CircleDot,
      color: 'text-cyan-400',
      bgColor: 'bg-cyan-900/30',
      borderColor: 'border-cyan-700'
    },
    {
      label: 'DarkFlats',
      count: scanResult.darkflats_count,
      icon: Eclipse,
      color: 'text-indigo-400',
      bgColor: 'bg-indigo-900/30',
      borderColor: 'border-indigo-700'
    },
  ];

  const hasCalibrationFrames = scanResult.darks_count > 0 || scanResult.flats_count > 0 ||
    scanResult.bias_count > 0 || scanResult.darkflats_count > 0;

  return (
    <div className="fixed inset-0 z-50 flex items-center justify-center">
      {/* Backdrop */}
      <div
        className="absolute inset-0 bg-black/70 backdrop-blur-sm"
        onClick={onClose}
      />

      {/* Modal */}
      <div className="relative bg-surface-elevated rounded-xl shadow-2xl w-full max-w-lg mx-4 border border-border">
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-border">
          <div className="flex items-center gap-3">
            {cancelled ? (
              <XCircle className="text-warning" size={24} />
            ) : (
              <CheckCircle2 className="text-success" size={24} />
            )}
            <h2 className="text-xl font-semibold">
              {cancelled ? 'Scan Cancelled' : 'Scan Complete'}
            </h2>
          </div>
          <button
            onClick={onClose}
            className="p-2 hover:bg-surface-hover rounded-lg transition"
          >
            <X size={20} />
          </button>
        </div>

        {/* Content */}
        <div className="p-6 space-y-6">
          {/* Path */}
          <div className="text-sm text-content-muted overflow-hidden">
            <span className="font-mono bg-surface px-2 py-1 rounded block truncate" title={rootPath}>
              {rootPath}
            </span>
          </div>

          {/* Cancelled notice */}
          {cancelled && (
            <div className="p-3 bg-warning-muted border border-warning/50 rounded-lg">
              <p className="text-sm text-warning">
                Scan was cancelled. Results below are partial - rescan to process remaining files.
              </p>
            </div>
          )}

          {/* Missing files warning */}
          {missingFilesCount !== undefined && missingFilesCount > 0 && (
            <div className="flex items-center gap-3 p-3 bg-orange/20 border border-orange/50 rounded-lg">
              <AlertTriangle className="text-orange flex-shrink-0" size={20} />
              <p className="text-sm text-orange">
                {missingFilesCount} file{missingFilesCount !== 1 ? 's' : ''} no longer exist{missingFilesCount === 1 ? 's' : ''} on disk
              </p>
            </div>
          )}

          {/* Summary Stats */}
          <div className="grid grid-cols-3 gap-4">
            <div className="bg-surface rounded-lg p-4 text-center">
              <p className="text-2xl font-bold text-success">{scanResult.files_found}</p>
              <p className="text-xs text-content-muted mt-1">Found</p>
            </div>
            <div className="bg-surface rounded-lg p-4 text-center">
              <p className="text-2xl font-bold text-accent">{scanResult.files_processed}</p>
              <p className="text-xs text-content-muted mt-1">Processed</p>
            </div>
            <div className="bg-surface rounded-lg p-4 text-center">
              <p className="text-2xl font-bold text-content-muted">{scanResult.files_skipped}</p>
              <p className="text-xs text-content-muted mt-1">Skipped</p>
            </div>
          </div>

          {/* Frame Types Breakdown */}
          {totalFrames > 0 && (
            <div>
              <h3 className="text-sm font-semibold text-content-secondary mb-3">Frame Types</h3>
              <div className="grid grid-cols-2 gap-2">
                {frameTypes.filter(ft => ft.count > 0).map(frameType => {
                  const Icon = frameType.icon;
                  return (
                    <div
                      key={frameType.label}
                      className={`flex items-center gap-3 p-3 rounded-lg border ${frameType.bgColor} ${frameType.borderColor}`}
                    >
                      <Icon className={frameType.color} size={20} />
                      <div>
                        <p className={`font-semibold ${frameType.color}`}>{frameType.count}</p>
                        <p className="text-xs text-content-muted">{frameType.label}</p>
                      </div>
                    </div>
                  );
                })}
              </div>
            </div>
          )}

          {/* Unique Camera Reconciliation */}
          {scanResult.frames_renamed > 0 && (
            <div className="flex items-center gap-3 p-4 bg-info-muted border border-info/50 rounded-lg">
              <RefreshCw className="text-accent" size={24} />
              <div>
                <p className="font-semibold text-accent">
                  Unique Camera Reconciliation
                </p>
                <p className="text-xs text-content-muted">
                  {scanResult.frames_renamed} frame{scanResult.frames_renamed !== 1 ? 's' : ''} renamed
                  {scanResult.calibration_sets_deleted > 0 && `, ${scanResult.calibration_sets_deleted} cal set${scanResult.calibration_sets_deleted !== 1 ? 's' : ''} rebuilt`}
                  {scanResult.sessions_updated > 0 && `, ${scanResult.sessions_updated} session${scanResult.sessions_updated !== 1 ? 's' : ''} updated`}
                </p>
              </div>
            </div>
          )}

          {/* Calibration Sets Created */}
          {hasCalibrationFrames && scanResult.calibration_sets_created > 0 && (
            <div className="flex items-center gap-3 p-4 bg-success-muted border border-success/50 rounded-lg">
              <Layers className="text-success" size={24} />
              <div>
                <p className="font-semibold text-success">
                  {scanResult.calibration_sets_created} Calibration Set{scanResult.calibration_sets_created !== 1 ? 's' : ''} Created
                </p>
                <p className="text-xs text-content-muted">
                  Automatically grouped from scanned calibration frames
                </p>
              </div>
            </div>
          )}

          {/* Errors */}
          {scanResult.errors.length > 0 && (
            <div className="p-4 bg-error-muted border border-error/50 rounded-lg overflow-hidden">
              <div className="flex items-start gap-3 min-w-0">
                <FileWarning className="text-error flex-shrink-0" size={20} />
                <div className="flex-1 min-w-0 overflow-hidden">
                  <p className="font-semibold text-error mb-2">
                    {scanResult.errors.length} Error{scanResult.errors.length !== 1 ? 's' : ''}
                  </p>
                  <div className="max-h-32 overflow-y-auto overflow-x-hidden">
                    <ul className="space-y-1 text-xs text-error/80">
                      {scanResult.errors.slice(0, 10).map((error, idx) => (
                        <li key={idx} className="truncate" title={String(error)}>
                          {String(error)}
                        </li>
                      ))}
                      {scanResult.errors.length > 10 && (
                        <li className="text-content-muted">
                          ...and {scanResult.errors.length - 10} more
                        </li>
                      )}
                    </ul>
                  </div>
                </div>
              </div>
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="p-4 border-t border-border flex justify-end">
          <button
            onClick={onClose}
            className="px-6 py-2 bg-accent hover:bg-accent-hover rounded-lg transition font-medium"
          >
            Done
          </button>
        </div>
      </div>
    </div>
  );
}
