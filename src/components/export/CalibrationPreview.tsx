import { Check, AlertTriangle, X } from 'lucide-react';
import type { CalibrationSummary } from '../../types/export';

interface CalibrationPreviewProps {
  summary: CalibrationSummary;
}

export function CalibrationPreview({ summary }: CalibrationPreviewProps) {
  const items = [
    {
      label: 'Flats',
      count: summary.flatCount,
      complete: summary.flatsComplete,
    },
    {
      label: 'Darks',
      count: summary.darkCount,
      complete: summary.darksComplete,
    },
    {
      label: 'Bias',
      count: summary.biasCount,
      complete: summary.biasComplete,
    },
  ];

  if (summary.darkFlatCount > 0) {
    items.push({
      label: 'Dark Flats',
      count: summary.darkFlatCount,
      complete: true,
    });
  }

  return (
    <div className="space-y-3">
      <div className="space-y-2">
        {items.map(({ label, count, complete }) => (
          <div
            key={label}
            className="flex items-center justify-between p-2 bg-gray-700/50 rounded-lg"
          >
            <div className="flex items-center gap-2">
              {count > 0 ? (
                complete ? (
                  <Check size={16} className="text-green-500" />
                ) : (
                  <AlertTriangle size={16} className="text-yellow-500" />
                )
              ) : (
                <X size={16} className="text-red-500" />
              )}
              <span>{label}</span>
            </div>
            <span className={count > 0 ? 'text-gray-200' : 'text-gray-500'}>
              {count} frames
            </span>
          </div>
        ))}
      </div>

      {summary.warnings.length > 0 && (
        <div className="mt-3 p-3 bg-yellow-900/20 border border-yellow-600/30 rounded-lg">
          <div className="flex items-center gap-2 text-yellow-500 mb-2">
            <AlertTriangle size={16} />
            <span className="font-medium">Warnings</span>
          </div>
          <ul className="text-sm text-yellow-400/80 space-y-1">
            {summary.warnings.map((warning, index) => (
              <li key={index}>• {warning}</li>
            ))}
          </ul>
        </div>
      )}
    </div>
  );
}
