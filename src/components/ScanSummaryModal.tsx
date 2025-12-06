import { X, CheckCircle2, Sun, Moon, Aperture, CircleDot, Eclipse, Layers, FileWarning } from 'lucide-react';
import type { ScanResult } from '../types/models';

interface ScanSummaryModalProps {
  isOpen: boolean;
  onClose: () => void;
  scanResult: ScanResult;
  rootPath: string;
}

export function ScanSummaryModal({ isOpen, onClose, scanResult, rootPath }: ScanSummaryModalProps) {
  if (!isOpen) return null;

  const totalFrames = scanResult.lights_count + scanResult.darks_count +
    scanResult.flats_count + scanResult.bias_count + scanResult.darkflats_count;

  const frameTypes = [
    {
      label: 'Lights',
      count: scanResult.lights_count,
      icon: Sun,
      color: 'text-yellow-400',
      bgColor: 'bg-yellow-900/30',
      borderColor: 'border-yellow-700'
    },
    {
      label: 'Darks',
      count: scanResult.darks_count,
      icon: Moon,
      color: 'text-blue-400',
      bgColor: 'bg-blue-900/30',
      borderColor: 'border-blue-700'
    },
    {
      label: 'Flats',
      count: scanResult.flats_count,
      icon: Aperture,
      color: 'text-purple-400',
      bgColor: 'bg-purple-900/30',
      borderColor: 'border-purple-700'
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
      <div className="relative bg-gray-800 rounded-xl shadow-2xl w-full max-w-lg mx-4 border border-gray-700">
        {/* Header */}
        <div className="flex items-center justify-between p-4 border-b border-gray-700">
          <div className="flex items-center gap-3">
            <CheckCircle2 className="text-green-400" size={24} />
            <h2 className="text-xl font-semibold">Scan Complete</h2>
          </div>
          <button
            onClick={onClose}
            className="p-2 hover:bg-gray-700 rounded-lg transition"
          >
            <X size={20} />
          </button>
        </div>

        {/* Content */}
        <div className="p-6 space-y-6">
          {/* Path */}
          <div className="text-sm text-gray-400">
            <span className="font-mono bg-gray-900 px-2 py-1 rounded">{rootPath}</span>
          </div>

          {/* Summary Stats */}
          <div className="grid grid-cols-3 gap-4">
            <div className="bg-gray-900 rounded-lg p-4 text-center">
              <p className="text-2xl font-bold text-green-400">{scanResult.files_found}</p>
              <p className="text-xs text-gray-400 mt-1">Found</p>
            </div>
            <div className="bg-gray-900 rounded-lg p-4 text-center">
              <p className="text-2xl font-bold text-blue-400">{scanResult.files_processed}</p>
              <p className="text-xs text-gray-400 mt-1">Processed</p>
            </div>
            <div className="bg-gray-900 rounded-lg p-4 text-center">
              <p className="text-2xl font-bold text-gray-400">{scanResult.files_skipped}</p>
              <p className="text-xs text-gray-400 mt-1">Skipped</p>
            </div>
          </div>

          {/* Frame Types Breakdown */}
          {totalFrames > 0 && (
            <div>
              <h3 className="text-sm font-semibold text-gray-300 mb-3">Frame Types</h3>
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
                        <p className="text-xs text-gray-400">{frameType.label}</p>
                      </div>
                    </div>
                  );
                })}
              </div>
            </div>
          )}

          {/* Calibration Sets Created */}
          {hasCalibrationFrames && scanResult.calibration_sets_created > 0 && (
            <div className="flex items-center gap-3 p-4 bg-green-900/20 border border-green-700 rounded-lg">
              <Layers className="text-green-400" size={24} />
              <div>
                <p className="font-semibold text-green-400">
                  {scanResult.calibration_sets_created} Calibration Set{scanResult.calibration_sets_created !== 1 ? 's' : ''} Created
                </p>
                <p className="text-xs text-gray-400">
                  Automatically grouped from scanned calibration frames
                </p>
              </div>
            </div>
          )}

          {/* Errors */}
          {scanResult.errors.length > 0 && (
            <div className="p-4 bg-red-900/20 border border-red-700 rounded-lg">
              <div className="flex items-start gap-3">
                <FileWarning className="text-red-400 flex-shrink-0" size={20} />
                <div className="flex-1">
                  <p className="font-semibold text-red-400 mb-2">
                    {scanResult.errors.length} Error{scanResult.errors.length !== 1 ? 's' : ''}
                  </p>
                  <div className="max-h-32 overflow-y-auto">
                    <ul className="space-y-1 text-xs text-red-300">
                      {scanResult.errors.slice(0, 10).map((error, idx) => (
                        <li key={idx} className="truncate" title={String(error)}>
                          {String(error)}
                        </li>
                      ))}
                      {scanResult.errors.length > 10 && (
                        <li className="text-gray-400">
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
        <div className="p-4 border-t border-gray-700 flex justify-end">
          <button
            onClick={onClose}
            className="px-6 py-2 bg-blue-600 hover:bg-blue-700 rounded-lg transition font-medium"
          >
            Done
          </button>
        </div>
      </div>
    </div>
  );
}
