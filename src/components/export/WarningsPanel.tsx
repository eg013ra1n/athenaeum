import {
  AlertTriangle,
  AlertCircle,
  Info,
  Thermometer,
  Calendar,
  FileX,
  Settings,
  ExternalLink,
} from 'lucide-react';
import type { DetailedWarning, WarningType, WarningSeverity } from '../../types/export';

interface WarningsPanelProps {
  warnings: DetailedWarning[];
  /** When provided, the calibration-set ID badge on each warning becomes a
   *  clickable link that jumps the caller wherever it wants (typically the
   *  Calibration Coverage tab with the set highlighted). */
  onSetClick?: (setId: number) => void;
}

/**
 * Panel displaying detailed warnings with full context
 */
export function WarningsPanel({ warnings, onSetClick }: WarningsPanelProps) {
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
          <WarningGroup warnings={errors} onSetClick={onSetClick} />
        )}
        {warns.length > 0 && (
          <WarningGroup warnings={warns} onSetClick={onSetClick} />
        )}
        {infos.length > 0 && (
          <WarningGroup warnings={infos} onSetClick={onSetClick} />
        )}
      </div>
    </div>
  );
}

interface WarningGroupProps {
  warnings: DetailedWarning[];
  onSetClick?: (setId: number) => void;
}

function WarningGroup({ warnings, onSetClick }: WarningGroupProps) {
  return (
    <div className="space-y-3">
      {warnings.map((warning, index) => (
        <WarningCard key={index} warning={warning} onSetClick={onSetClick} />
      ))}
    </div>
  );
}

interface WarningCardProps {
  warning: DetailedWarning;
  onSetClick?: (setId: number) => void;
}

function WarningCard({ warning, onSetClick }: WarningCardProps) {
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
          {/* Title with type icon. If the title text contains the warning's
              own `#<setId>` token, render that token as an inline clickable
              link instead of duplicating it as a trailing chip. Falls back
              to a trailing chip when the setId isn't surfaced in the
              title (e.g. generic warnings). */}
          <div className="flex items-center gap-2 mb-1 flex-wrap">
            <TypeIcon size={14} className="text-content-muted" />
            <span className="font-medium text-content">
              {renderTitleWithInlineSetLink(warning.title, warning.setId, onSetClick)}
            </span>
            {warning.filter && (
              <span className="text-xs px-1.5 py-0.5 bg-surface-elevated rounded text-content-muted">
                {warning.filter}
              </span>
            )}
            {warning.setId != null
              && !titleMentionsSetId(warning.title, warning.setId)
              && (
                onSetClick ? (
                  <button
                    type="button"
                    onClick={() => onSetClick(warning.setId!)}
                    title={`Open calibration set #${warning.setId} in Calibration Coverage`}
                    className="inline-flex items-center gap-0.5 text-xs font-mono px-1.5 py-0.5 rounded bg-accent/10 text-accent border border-accent/30 hover:bg-accent/20 hover:underline transition-colors cursor-pointer"
                  >
                    #{warning.setId}
                    <ExternalLink size={10} className="opacity-70" />
                  </button>
                ) : (
                  <span
                    className="text-xs font-mono px-1.5 py-0.5 rounded bg-surface-elevated text-content-muted"
                    title={`Calibration set #${warning.setId}`}
                  >
                    #{warning.setId}
                  </span>
                )
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

/** True when the warning title contains the literal `#<setId>` token. We use
 *  word-boundary matching so `#11` doesn't match `#119` etc. */
function titleMentionsSetId(title: string, setId: number): boolean {
  const re = new RegExp(`#${setId}\\b`);
  return re.test(title);
}

/** If the title has a `#<setId>` token, replace that token with an inline
 *  clickable button that jumps to the Calibration Coverage tab. Everything
 *  else in the title renders verbatim. */
function renderTitleWithInlineSetLink(
  title: string,
  setId: number | null,
  onSetClick: ((setId: number) => void) | undefined,
): React.ReactNode {
  if (setId == null) return title;
  const re = new RegExp(`#${setId}\\b`);
  const match = re.exec(title);
  if (!match) return title;

  const before = title.slice(0, match.index);
  const tokenLen = match[0].length;
  const after = title.slice(match.index + tokenLen);

  if (!onSetClick) {
    // No handler — render the token as a non-interactive monospace badge so
    // it still stands out as a reference, just not clickable.
    return (
      <>
        {before}
        <span className="font-mono text-accent" title={`Calibration set #${setId}`}>
          #{setId}
        </span>
        {after}
      </>
    );
  }

  return (
    <>
      {before}
      <button
        type="button"
        onClick={() => onSetClick(setId)}
        title={`Open calibration set #${setId} in Calibration Coverage`}
        className="inline-flex items-center gap-0.5 align-baseline font-mono text-accent hover:underline cursor-pointer"
      >
        #{setId}
        <ExternalLink size={10} className="opacity-70" />
      </button>
      {after}
    </>
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
