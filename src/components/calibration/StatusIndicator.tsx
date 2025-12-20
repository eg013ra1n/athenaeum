import { CheckCircle, AlertTriangle, XCircle } from 'lucide-react';
import type { FrameCalibrationStatus } from '../../types/models';

interface StatusIndicatorProps {
  status: FrameCalibrationStatus;
  /** Show compact badges without text labels */
  compact?: boolean;
}

interface StatusItem {
  key: string;
  has: boolean;
  warning: boolean;
  label: string;
}

/**
 * Accessible status indicator for calibration status.
 * Uses full-word labels and 16px icons for readability.
 * Status is conveyed with both icons and text (not color alone).
 */
export function StatusIndicator({ status, compact = false }: StatusIndicatorProps) {
  // Only show Flat and Dark - Bias is not directly linked to light frames
  const items: StatusItem[] = [
    { key: 'flat', has: status.has_flats, warning: status.flats_warning, label: 'Flat' },
    { key: 'dark', has: status.has_darks, warning: status.darks_warning, label: 'Dark' },
  ];

  const getStatusStyles = (has: boolean, warning: boolean): string => {
    if (!has) {
      return 'bg-red-900/30 text-red-300 border-red-700';
    }
    if (warning) {
      return 'bg-amber-900/30 text-amber-300 border-amber-700';
    }
    return 'bg-emerald-900/30 text-emerald-300 border-emerald-700';
  };

  const getStatusIcon = (has: boolean, warning: boolean) => {
    if (!has) {
      return <XCircle size={16} aria-hidden="true" />;
    }
    if (warning) {
      return <AlertTriangle size={16} aria-hidden="true" />;
    }
    return <CheckCircle size={16} aria-hidden="true" />;
  };

  const getStatusLabel = (has: boolean, warning: boolean, label: string): string => {
    if (!has) {
      return `${label} missing`;
    }
    if (warning) {
      return `${label} found with warnings`;
    }
    return `${label} found`;
  };

  if (compact) {
    return (
      <div className="flex items-center justify-center gap-1" role="group" aria-label="Calibration status">
        {items.map(({ key, has, warning, label }) => (
          <span
            key={key}
            className={`
              inline-flex items-center justify-center
              w-6 h-6 rounded text-xs font-bold
              ${getStatusStyles(has, warning)}
            `}
            title={getStatusLabel(has, warning, label)}
            aria-label={getStatusLabel(has, warning, label)}
          >
            {label[0]}
          </span>
        ))}
      </div>
    );
  }

  return (
    <div className="flex items-center gap-2" role="group" aria-label="Calibration status">
      {items.map(({ key, has, warning, label }) => (
        <span
          key={key}
          className={`
            inline-flex items-center gap-1.5
            px-2.5 py-1.5 rounded-md border
            text-sm font-medium
            ${getStatusStyles(has, warning)}
          `}
          title={getStatusLabel(has, warning, label)}
        >
          {getStatusIcon(has, warning)}
          <span>{label}</span>
        </span>
      ))}
    </div>
  );
}

/**
 * Single status badge for use in compact layouts.
 */
export function StatusBadge({
  has,
  warning,
  label,
}: {
  has: boolean;
  warning: boolean;
  label: string;
}) {
  const getStatusStyles = (has: boolean, warning: boolean): string => {
    if (!has) {
      return 'bg-red-900/30 text-red-300 border-red-700';
    }
    if (warning) {
      return 'bg-amber-900/30 text-amber-300 border-amber-700';
    }
    return 'bg-emerald-900/30 text-emerald-300 border-emerald-700';
  };

  const getStatusIcon = (has: boolean, warning: boolean) => {
    if (!has) {
      return <XCircle size={16} aria-hidden="true" />;
    }
    if (warning) {
      return <AlertTriangle size={16} aria-hidden="true" />;
    }
    return <CheckCircle size={16} aria-hidden="true" />;
  };

  const getStatusLabel = (has: boolean, warning: boolean, label: string): string => {
    if (!has) {
      return `${label} missing`;
    }
    if (warning) {
      return `${label} found with warnings`;
    }
    return `${label} found`;
  };

  return (
    <span
      className={`
        inline-flex items-center gap-1.5
        px-2.5 py-1.5 rounded-md border
        text-sm font-medium
        ${getStatusStyles(has, warning)}
      `}
      title={getStatusLabel(has, warning, label)}
    >
      {getStatusIcon(has, warning)}
      <span>{label}</span>
    </span>
  );
}
