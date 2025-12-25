import { AlertTriangle } from 'lucide-react';
import type { CalibrationWarning } from '../../types/models';

/** Warning with additional context for display */
export interface AggregatedWarning {
  message: string;
  type: 'missing_calibration' | 'date' | 'temperature';
  filter?: string;
  camera?: string;
}

interface WarningPanelProps {
  /** CalibrationWarning array from the data model */
  warnings?: CalibrationWarning[];
  /** Or use aggregated warnings with additional context */
  aggregatedWarnings?: AggregatedWarning[];
  /** Optional title override */
  title?: string;
  /** Compact mode for inline display */
  compact?: boolean;
}

/**
 * Consolidated warning panel with accessible design.
 * Uses larger icons (18px) and readable text (14px minimum).
 */
export function WarningPanel({
  warnings,
  aggregatedWarnings,
  title = 'Warnings',
  compact = false,
}: WarningPanelProps) {
  // Convert CalibrationWarning to display format if needed
  const displayWarnings: AggregatedWarning[] = aggregatedWarnings ??
    (warnings?.map(w => ({
      message: w.message,
      type: w.warning_type as 'date' | 'temperature',
    })) ?? []);

  if (displayWarnings.length === 0) {
    return null;
  }

  if (compact) {
    return (
      <div className="flex items-center gap-2 px-3 py-2 bg-amber-900/20 rounded-lg border border-amber-700/50">
        <AlertTriangle size={18} className="text-amber-400 flex-shrink-0" aria-hidden="true" />
        <span className="text-sm text-amber-200">
          {displayWarnings.length} warning{displayWarnings.length !== 1 ? 's' : ''}
        </span>
      </div>
    );
  }

  return (
    <div
      className="p-4 bg-amber-900/20 rounded-xl border border-amber-700/50"
      role="alert"
      aria-label={`${displayWarnings.length} ${title.toLowerCase()}`}
    >
      <h4 className="text-base font-semibold text-amber-200 mb-3 flex items-center gap-2">
        <AlertTriangle size={20} className="text-amber-400" aria-hidden="true" />
        {title} ({displayWarnings.length})
      </h4>
      <ul className="space-y-2">
        {displayWarnings.map((warning, index) => (
          <li
            key={index}
            className="flex items-start gap-3 text-sm text-amber-100"
          >
            <span
              className="flex-shrink-0 w-1.5 h-1.5 mt-2 rounded-full bg-amber-400"
              aria-hidden="true"
            />
            <span>{warning.message}</span>
          </li>
        ))}
      </ul>
    </div>
  );
}

/**
 * Inline warning badge for headers and summary displays.
 */
export function WarningBadge({ count }: { count: number }) {
  if (count === 0) {
    return null;
  }

  return (
    <span
      className="inline-flex items-center gap-1 px-2 py-0.5 rounded-full bg-amber-900/40 text-amber-300 text-sm"
      title={`${count} warning${count !== 1 ? 's' : ''}`}
    >
      <AlertTriangle size={14} aria-hidden="true" />
      <span>{count}</span>
    </span>
  );
}
