import { useState } from 'react';
import {
  AlertTriangle,
  AlertCircle,
  FileX,
  ChevronDown,
  ChevronRight,
  Check,
  X,
  Info,
  FileCheck,
} from 'lucide-react';
import type { FileValidation, PipelinePlan } from '../../types/export';

// ============================================================================
// Main Validation Summary Component
// ============================================================================

interface ValidationWarningsProps {
  plan: PipelinePlan;
  onContinue?: () => void;
  onCancel?: () => void;
  showActions?: boolean;
}

export function ValidationWarnings({
  plan,
  onContinue,
  onCancel,
  showActions = true,
}: ValidationWarningsProps) {
  const hasBlockingErrors = plan.hasBlockingErrors;
  const hasWarnings = plan.warnings.length > 0 || plan.missingCalibrationFiles > 0;

  return (
    <div className="space-y-4">
      {/* Summary Card */}
      <div
        className={`p-4 rounded-lg border ${
          hasBlockingErrors
            ? 'bg-error/10 border-error/30'
            : hasWarnings
            ? 'bg-warning/10 border-warning/30'
            : 'bg-success/10 border-success/30'
        }`}
      >
        <div className="flex items-start gap-3">
          {hasBlockingErrors ? (
            <AlertCircle size={24} className="text-error mt-0.5" />
          ) : hasWarnings ? (
            <AlertTriangle size={24} className="text-warning mt-0.5" />
          ) : (
            <Check size={24} className="text-success mt-0.5" />
          )}
          <div className="flex-1">
            <h3 className={`font-medium ${hasBlockingErrors ? 'text-error' : hasWarnings ? 'text-warning' : 'text-success'}`}>
              {hasBlockingErrors
                ? 'Validation Failed'
                : hasWarnings
                ? 'Validation Passed with Warnings'
                : 'Validation Passed'}
            </h3>
            <p className="text-sm text-content-muted mt-1">
              {hasBlockingErrors
                ? 'Some source files are missing and must be resolved before export.'
                : hasWarnings
                ? 'Export can proceed, but some issues may affect results.'
                : 'All source files and calibration links are valid.'}
            </p>
          </div>
        </div>

        {/* Quick Stats */}
        <div className="mt-4 grid grid-cols-2 md:grid-cols-4 gap-4">
          <ValidationStat
            label="Source Files"
            value={plan.totalSourceFiles}
            missing={plan.missingSourceFiles}
            isError={plan.missingSourceFiles > 0}
          />
          <ValidationStat
            label="Calibration Files"
            value={plan.totalCalibrationFiles}
            missing={plan.missingCalibrationFiles}
          />
          <ValidationStat
            label="Pipeline Steps"
            value={plan.steps.length}
          />
          <ValidationStat
            label="Warnings"
            value={plan.warnings.length}
          />
        </div>
      </div>

      {/* Missing Source Files (Blocking) */}
      {plan.missingSourceFiles > 0 && (
        <MissingFilesSection
          title="Missing Source Files"
          description="These light frame files could not be found. Export cannot proceed until they are restored."
          files={plan.sourceValidations.filter(v => !v.exists)}
          isError={true}
        />
      )}

      {/* Missing Calibration Files (Warning) */}
      {plan.missingCalibrationFiles > 0 && (
        <MissingFilesSection
          title="Missing Calibration Files"
          description="These calibration files could not be found. Export will continue without them, which may affect image quality."
          files={plan.calibrationValidations.filter(v => !v.exists)}
          isError={false}
        />
      )}

      {/* General Warnings */}
      {plan.warnings.length > 0 && (
        <WarningsList warnings={plan.warnings} />
      )}

      {/* Action Buttons */}
      {showActions && (
        <div className="flex items-center justify-end gap-3 pt-2">
          {onCancel && (
            <button
              onClick={onCancel}
              className="px-4 py-2 text-sm font-medium text-content-secondary hover:text-content bg-surface-hover rounded-lg transition-colors"
            >
              Cancel
            </button>
          )}
          {onContinue && (
            <button
              onClick={onContinue}
              disabled={hasBlockingErrors}
              className={`px-4 py-2 text-sm font-medium rounded-lg transition-colors flex items-center gap-2 ${
                hasBlockingErrors
                  ? 'bg-gray-700 text-gray-500 cursor-not-allowed'
                  : 'bg-accent text-white hover:bg-accent/90'
              }`}
            >
              {hasBlockingErrors ? (
                <>
                  <X size={16} />
                  Cannot Continue
                </>
              ) : (
                <>
                  <Check size={16} />
                  Continue with Export
                </>
              )}
            </button>
          )}
        </div>
      )}
    </div>
  );
}

// ============================================================================
// Validation Stat Component
// ============================================================================

interface ValidationStatProps {
  label: string;
  value: number;
  missing?: number;
  isError?: boolean;
}

function ValidationStat({ label, value, missing, isError }: ValidationStatProps) {
  return (
    <div className="bg-surface/50 rounded-lg p-3">
      <div className="text-xs text-content-muted uppercase tracking-wide">{label}</div>
      <div className="flex items-baseline gap-2 mt-1">
        <span className="text-xl font-semibold text-content">{value}</span>
        {missing !== undefined && missing > 0 && (
          <span className={`text-sm ${isError ? 'text-error' : 'text-warning'}`}>
            ({missing} missing)
          </span>
        )}
      </div>
    </div>
  );
}

// ============================================================================
// Missing Files Section Component
// ============================================================================

interface MissingFilesSectionProps {
  title: string;
  description: string;
  files: FileValidation[];
  isError: boolean;
}

function MissingFilesSection({ title, description, files, isError }: MissingFilesSectionProps) {
  const [expanded, setExpanded] = useState(files.length <= 5);
  const displayFiles = expanded ? files : files.slice(0, 3);

  return (
    <div className={`rounded-lg border ${isError ? 'border-error/30' : 'border-warning/30'}`}>
      <button
        onClick={() => setExpanded(!expanded)}
        className={`w-full flex items-center gap-3 p-3 ${
          isError ? 'bg-error/5' : 'bg-warning/5'
        } hover:bg-surface-hover/50 transition-colors`}
      >
        {expanded ? (
          <ChevronDown size={16} className="text-content-muted" />
        ) : (
          <ChevronRight size={16} className="text-content-muted" />
        )}
        <FileX size={16} className={isError ? 'text-error' : 'text-warning'} />
        <div className="flex-1 text-left">
          <div className={`font-medium ${isError ? 'text-error' : 'text-warning'}`}>
            {title} ({files.length})
          </div>
          <div className="text-sm text-content-muted">{description}</div>
        </div>
      </button>

      {expanded && (
        <div className="p-3 space-y-1 max-h-64 overflow-y-auto">
          {displayFiles.map((file, index) => (
            <div
              key={index}
              className="flex items-center gap-2 p-2 bg-surface-elevated rounded text-sm"
            >
              <FileX size={14} className={isError ? 'text-error' : 'text-warning'} />
              <span className="font-mono text-content-secondary truncate flex-1">
                {file.path}
              </span>
              {file.error && (
                <span className="text-xs text-content-muted">{file.error}</span>
              )}
            </div>
          ))}
          {!expanded && files.length > 3 && (
            <div className="text-sm text-content-muted text-center py-2">
              And {files.length - 3} more files...
            </div>
          )}
        </div>
      )}
    </div>
  );
}

// ============================================================================
// Warnings List Component
// ============================================================================

interface WarningsListProps {
  warnings: string[];
}

function WarningsList({ warnings }: WarningsListProps) {
  const [expanded, setExpanded] = useState(warnings.length <= 3);

  return (
    <div className="rounded-lg border border-warning/30">
      <button
        onClick={() => setExpanded(!expanded)}
        className="w-full flex items-center gap-3 p-3 bg-warning/5 hover:bg-surface-hover/50 transition-colors"
      >
        {expanded ? (
          <ChevronDown size={16} className="text-content-muted" />
        ) : (
          <ChevronRight size={16} className="text-content-muted" />
        )}
        <AlertTriangle size={16} className="text-warning" />
        <div className="flex-1 text-left">
          <div className="font-medium text-warning">Warnings ({warnings.length})</div>
        </div>
      </button>

      {expanded && (
        <div className="p-3 space-y-2">
          {warnings.map((warning, index) => (
            <div
              key={index}
              className="flex items-start gap-2 p-2 bg-surface-elevated rounded text-sm"
            >
              <Info size={14} className="text-warning mt-0.5" />
              <span className="text-content-secondary">{warning}</span>
            </div>
          ))}
        </div>
      )}
    </div>
  );
}

// ============================================================================
// Inline Validation Badge (for compact display)
// ============================================================================

interface ValidationBadgeProps {
  plan: PipelinePlan;
  onClick?: () => void;
}

export function ValidationBadge({ plan, onClick }: ValidationBadgeProps) {
  const hasBlockingErrors = plan.hasBlockingErrors;
  const hasWarnings = plan.warnings.length > 0 || plan.missingCalibrationFiles > 0;

  if (hasBlockingErrors) {
    return (
      <button
        onClick={onClick}
        className="flex items-center gap-1.5 px-2 py-1 bg-error/10 text-error rounded text-sm hover:bg-error/20 transition-colors"
      >
        <AlertCircle size={14} />
        <span>{plan.missingSourceFiles} missing files</span>
      </button>
    );
  }

  if (hasWarnings) {
    return (
      <button
        onClick={onClick}
        className="flex items-center gap-1.5 px-2 py-1 bg-warning/10 text-warning rounded text-sm hover:bg-warning/20 transition-colors"
      >
        <AlertTriangle size={14} />
        <span>{plan.warnings.length + (plan.missingCalibrationFiles > 0 ? 1 : 0)} warnings</span>
      </button>
    );
  }

  return (
    <div className="flex items-center gap-1.5 px-2 py-1 bg-success/10 text-success rounded text-sm">
      <FileCheck size={14} />
      <span>All files valid</span>
    </div>
  );
}

// ============================================================================
// Quick Validation Status (for headers/toolbars)
// ============================================================================

interface QuickValidationStatusProps {
  sourceCount: number;
  calibrationCount: number;
  missingSourceCount: number;
  missingCalibrationCount: number;
}

export function QuickValidationStatus({
  sourceCount,
  calibrationCount,
  missingSourceCount,
  missingCalibrationCount,
}: QuickValidationStatusProps) {
  const hasSourceErrors = missingSourceCount > 0;
  const hasCalibrationWarnings = missingCalibrationCount > 0;

  return (
    <div className="flex items-center gap-4 text-sm">
      <div className={`flex items-center gap-1.5 ${hasSourceErrors ? 'text-error' : 'text-success'}`}>
        {hasSourceErrors ? <FileX size={14} /> : <FileCheck size={14} />}
        <span>{sourceCount - missingSourceCount}/{sourceCount} sources</span>
      </div>
      <div className={`flex items-center gap-1.5 ${hasCalibrationWarnings ? 'text-warning' : 'text-success'}`}>
        {hasCalibrationWarnings ? <AlertTriangle size={14} /> : <FileCheck size={14} />}
        <span>{calibrationCount - missingCalibrationCount}/{calibrationCount} calibrations</span>
      </div>
    </div>
  );
}
