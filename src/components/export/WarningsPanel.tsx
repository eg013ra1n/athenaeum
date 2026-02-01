import {
  AlertTriangle,
  AlertCircle,
  Info,
  Thermometer,
  Calendar,
  FileX,
  Settings,
} from 'lucide-react';
import type { DetailedWarning, WarningType, WarningSeverity } from '../../types/export';

interface WarningsPanelProps {
  warnings: DetailedWarning[];
}

/**
 * Panel displaying detailed warnings with full context
 */
export function WarningsPanel({ warnings }: WarningsPanelProps) {
  if (warnings.length === 0) {
    return null;
  }

  // Group by severity
  const errors = warnings.filter((w) => w.severity === 'error');
  const warns = warnings.filter((w) => w.severity === 'warning');
  const infos = warnings.filter((w) => w.severity === 'info');

  return (
    <div className="border border-border rounded-lg overflow-hidden bg-surface">
      {/* Header */}
      <div className="p-3 bg-warning/10 border-b border-border flex items-center gap-2">
        <AlertTriangle size={16} className="text-warning" />
        <h3 className="text-sm font-medium">Attention Required</h3>
        <span className="text-xs text-content-muted">
          ({warnings.length} {warnings.length === 1 ? 'issue' : 'issues'})
        </span>
      </div>

      {/* Warnings list */}
      <div className="p-4 space-y-4">
        {errors.length > 0 && (
          <WarningGroup warnings={errors} />
        )}
        {warns.length > 0 && (
          <WarningGroup warnings={warns} />
        )}
        {infos.length > 0 && (
          <WarningGroup warnings={infos} />
        )}
      </div>
    </div>
  );
}

interface WarningGroupProps {
  warnings: DetailedWarning[];
}

function WarningGroup({ warnings }: WarningGroupProps) {
  return (
    <div className="space-y-3">
      {warnings.map((warning, index) => (
        <WarningCard key={index} warning={warning} />
      ))}
    </div>
  );
}

interface WarningCardProps {
  warning: DetailedWarning;
}

function WarningCard({ warning }: WarningCardProps) {
  const SeverityIcon = getSeverityIcon(warning.severity);
  const TypeIcon = getTypeIcon(warning.warningType);
  const severityColor = getSeverityColor(warning.severity);

  return (
    <div className={`p-3 rounded-lg border ${severityColor.bg} ${severityColor.border}`}>
      <div className="flex items-start gap-3">
        {/* Icon */}
        <div className={`mt-0.5 ${severityColor.icon}`}>
          <SeverityIcon size={16} />
        </div>

        {/* Content */}
        <div className="flex-1 min-w-0">
          {/* Title with type icon */}
          <div className="flex items-center gap-2 mb-1">
            <TypeIcon size={14} className="text-content-muted" />
            <span className="font-medium text-content">{warning.title}</span>
            {warning.filter && (
              <span className="text-xs px-1.5 py-0.5 bg-surface-elevated rounded text-content-muted">
                {warning.filter}
              </span>
            )}
          </div>

          {/* Description */}
          <p className="text-sm text-content-secondary mb-2">{warning.description}</p>

          {/* Comparison values */}
          {(warning.actualValue || warning.expectedValue) && (
            <div className="flex flex-wrap gap-4 text-sm mb-2">
              {warning.actualValue && (
                <div>
                  <span className="text-content-muted">Actual: </span>
                  <span className="text-content">{warning.actualValue}</span>
                </div>
              )}
              {warning.expectedValue && (
                <div>
                  <span className="text-content-muted">Expected: </span>
                  <span className="text-content">{warning.expectedValue}</span>
                </div>
              )}
              {warning.delta && (
                <div>
                  <span className="text-content-muted">Delta: </span>
                  <span className={severityColor.text}>{warning.delta}</span>
                </div>
              )}
            </div>
          )}

          {/* Recommendation */}
          {warning.recommendation && (
            <div className="text-sm text-content-muted italic border-t border-border/50 pt-2 mt-2">
              {warning.recommendation}
            </div>
          )}
        </div>
      </div>
    </div>
  );
}

function getSeverityIcon(severity: WarningSeverity) {
  switch (severity) {
    case 'error':
      return AlertCircle;
    case 'warning':
      return AlertTriangle;
    case 'info':
      return Info;
    default:
      return Info;
  }
}

function getTypeIcon(type: WarningType) {
  switch (type) {
    case 'temperature_mismatch':
      return Thermometer;
    case 'calibration_age':
      return Calendar;
    case 'missing_calibration':
      return FileX;
    case 'gain_offset_mismatch':
    case 'binning_mismatch':
    case 'exposure_mismatch':
      return Settings;
    default:
      return AlertTriangle;
  }
}

function getSeverityColor(severity: WarningSeverity): {
  bg: string;
  border: string;
  icon: string;
  text: string;
} {
  switch (severity) {
    case 'error':
      return {
        bg: 'bg-error/10',
        border: 'border-error/30',
        icon: 'text-error',
        text: 'text-error',
      };
    case 'warning':
      return {
        bg: 'bg-warning/10',
        border: 'border-warning/30',
        icon: 'text-warning',
        text: 'text-warning',
      };
    case 'info':
      return {
        bg: 'bg-info-muted',
        border: 'border-accent/30',
        icon: 'text-accent',
        text: 'text-accent',
      };
    default:
      return {
        bg: 'bg-surface-elevated',
        border: 'border-border',
        icon: 'text-content-muted',
        text: 'text-content-muted',
      };
  }
}
