import { useState, useMemo } from 'react';
import {
  Check,
  X,
  Loader2,
  Clock,
  SkipForward,
  ChevronDown,
  ChevronRight,
  Play,
  Pause,
  RotateCcw,
  FileCheck,
  FolderTree,
  Layers,
  Wand2,
  FolderInput,
  Target,
  AlertCircle,
  Info,
  FileCode,
  Circle,
  Box,
} from 'lucide-react';
import type { ExportJob, ExportJobStep, StepStatus, StepType, StepPhase, PipelineProgress } from '../../types/export';
import {
  getStepTypeLabel,
  getStatusColor,
  getStatusBgColor,
  formatDuration,
  calculateElapsedTime,
  getStepPhase,
  getPhaseLabel,
} from '../../hooks/useExportPipeline';

// ============================================================================
// Main Component
// ============================================================================

interface PipelineVisualizerProps {
  job: ExportJob;
  progress?: PipelineProgress | null;
  onRetryStep?: (stepId: number) => void;
  onPause?: () => void;
  onResume?: () => void;
  onCancel?: () => void;
  showControls?: boolean;
}

export function PipelineVisualizer({
  job,
  progress,
  onRetryStep,
  onPause,
  onResume,
  onCancel,
  showControls = true,
}: PipelineVisualizerProps) {
  const isRunning = job.status === 'running';
  const isPaused = job.status === 'paused';
  const isFailed = job.status === 'failed';

  // Determine if we should use phase grouping (for granular pipelines with many steps)
  const usePhaseGrouping = job.steps.length > 10;

  // Group steps by phase
  const stepsByPhase = useMemo(() => {
    if (!usePhaseGrouping) return null;

    const phases: Record<StepPhase, ExportJobStep[]> = {
      preparation: [],
      masters: [],
      calibration: [],
      branch_registration: [],
      collection: [],
      global_registration: [],
      prepare_stacking: [],
      stacking: [],
    };

    for (const step of job.steps) {
      const phase = getStepPhase(step.stepType);
      phases[phase].push(step);
    }

    return phases;
  }, [job.steps, usePhaseGrouping]);

  return (
    <div className="bg-surface rounded-lg border border-border overflow-hidden">
      {/* Header */}
      <div className="p-4 bg-surface-elevated border-b border-border">
        <div className="flex items-center justify-between">
          <div className="flex items-center gap-3">
            <JobStatusIcon status={job.status} />
            <div>
              <h3 className="font-medium">Export Job #{job.id}</h3>
              <p className="text-sm text-content-muted">
                {job.completedSteps} of {job.totalSteps} steps completed
              </p>
            </div>
          </div>

          {showControls && (
            <div className="flex items-center gap-2">
              {isRunning && onPause && (
                <button
                  onClick={onPause}
                  className="p-2 rounded-lg bg-surface-hover hover:bg-warning/20 transition-colors"
                  title="Pause job"
                >
                  <Pause size={16} className="text-warning" />
                </button>
              )}
              {isPaused && onResume && (
                <button
                  onClick={onResume}
                  className="p-2 rounded-lg bg-surface-hover hover:bg-success/20 transition-colors"
                  title="Resume job"
                >
                  <Play size={16} className="text-success" />
                </button>
              )}
              {(isRunning || isPaused) && onCancel && (
                <button
                  onClick={onCancel}
                  className="p-2 rounded-lg bg-surface-hover hover:bg-error/20 transition-colors"
                  title="Cancel job"
                >
                  <X size={16} className="text-error" />
                </button>
              )}
            </div>
          )}
        </div>

        {/* Overall Progress Bar */}
        {(isRunning || isPaused) && (
          <div className="mt-3">
            <div className="flex items-center justify-between text-sm mb-1">
              <span className="text-content-muted">Overall Progress</span>
              <span className="text-content-secondary">
                {progress ? `${progress.overallProgress.toFixed(0)}%` : `${Math.round((job.completedSteps / job.totalSteps) * 100)}%`}
              </span>
            </div>
            <div className="w-full bg-surface-hover rounded-full h-2">
              <div
                className={`h-2 rounded-full transition-all duration-300 ${isPaused ? 'bg-warning' : 'bg-accent'}`}
                style={{ width: `${progress?.overallProgress ?? (job.completedSteps / job.totalSteps) * 100}%` }}
              />
            </div>
          </div>
        )}

        {/* Current Step Message */}
        {progress && isRunning && (
          <div className="mt-2 text-sm text-content-muted flex items-center gap-2">
            <Loader2 size={14} className="animate-spin text-accent" />
            <span>{progress.message}</span>
          </div>
        )}

        {/* Error Message */}
        {isFailed && job.errorMessage && (
          <div className="mt-3 p-3 bg-error/10 rounded-lg border border-error/20">
            <div className="flex items-start gap-2">
              <AlertCircle size={16} className="text-error mt-0.5" />
              <div className="text-sm text-error">{job.errorMessage}</div>
            </div>
          </div>
        )}
      </div>

      {/* Steps List - Phase Grouped or Flat */}
      {usePhaseGrouping && stepsByPhase ? (
        <div className="divide-y divide-border">
          {(Object.entries(stepsByPhase) as [StepPhase, ExportJobStep[]][]).map(([phase, steps]) => {
            if (steps.length === 0) return null;
            return (
              <PhaseGroup
                key={phase}
                phase={phase}
                steps={steps}
                progress={progress}
                onRetryStep={onRetryStep}
              />
            );
          })}
        </div>
      ) : (
        <div className="divide-y divide-border">
          {job.steps.map((step, index) => (
            <StepRow
              key={step.id}
              step={step}
              isCurrentStep={progress?.stepOrder === step.stepOrder}
              stepProgress={progress?.stepOrder === step.stepOrder ? progress.stepProgress : undefined}
              onRetry={onRetryStep}
              isLast={index === job.steps.length - 1}
            />
          ))}
        </div>
      )}
    </div>
  );
}

// ============================================================================
// Phase Group Component (for collapsible phases)
// ============================================================================

interface PhaseGroupProps {
  phase: StepPhase;
  steps: ExportJobStep[];
  progress?: PipelineProgress | null;
  onRetryStep?: (stepId: number) => void;
}

function PhaseGroup({ phase, steps, progress, onRetryStep }: PhaseGroupProps) {
  const [expanded, setExpanded] = useState(() => {
    // Auto-expand if there's a running or failed step
    return steps.some(s => s.status === 'running' || s.status === 'failed');
  });

  const completedCount = steps.filter(s => s.status === 'completed').length;
  const failedCount = steps.filter(s => s.status === 'failed').length;
  const runningCount = steps.filter(s => s.status === 'running').length;

  const phaseStatus: StepStatus =
    failedCount > 0 ? 'failed' :
    runningCount > 0 ? 'running' :
    completedCount === steps.length ? 'completed' :
    completedCount > 0 ? 'running' : 'pending';

  return (
    <div>
      {/* Phase Header */}
      <div
        className={`flex items-center gap-3 p-3 cursor-pointer hover:bg-surface-hover/50 ${
          runningCount > 0 ? 'bg-accent/5' : ''
        }`}
        onClick={() => setExpanded(!expanded)}
      >
        {/* Expand/Collapse Icon */}
        <div className="w-6 flex justify-center">
          {expanded ? (
            <ChevronDown size={16} className="text-content-muted" />
          ) : (
            <ChevronRight size={16} className="text-content-muted" />
          )}
        </div>

        {/* Phase Status Icon */}
        <StepStatusIcon status={phaseStatus} isCurrentStep={runningCount > 0} />

        {/* Phase Icon */}
        <PhaseIcon phase={phase} />

        {/* Phase Info */}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span className="font-medium">{getPhaseLabel(phase)}</span>
            <span className="text-xs px-1.5 py-0.5 bg-surface-hover rounded text-content-muted">
              {steps.length} step{steps.length !== 1 ? 's' : ''}
            </span>
          </div>
          <div className="text-sm text-content-muted">
            {completedCount}/{steps.length} complete
            {failedCount > 0 && <span className="text-error ml-2">({failedCount} failed)</span>}
          </div>
        </div>

        {/* Phase Progress */}
        {steps.length > 1 && (
          <div className="w-20">
            <div className="w-full bg-surface-hover rounded-full h-1.5">
              <div
                className={`h-1.5 rounded-full transition-all duration-300 ${
                  failedCount > 0 ? 'bg-error' : 'bg-success'
                }`}
                style={{ width: `${(completedCount / steps.length) * 100}%` }}
              />
            </div>
          </div>
        )}
      </div>

      {/* Expanded Steps */}
      {expanded && (
        <div className="border-t border-border/50 bg-surface-elevated/30">
          {steps.map((step, index) => (
            <StepRow
              key={step.id}
              step={step}
              isCurrentStep={progress?.stepOrder === step.stepOrder}
              stepProgress={progress?.stepOrder === step.stepOrder ? progress.stepProgress : undefined}
              onRetry={onRetryStep}
              isLast={index === steps.length - 1}
              compact
            />
          ))}
        </div>
      )}
    </div>
  );
}

// ============================================================================
// Step Row Component
// ============================================================================

interface StepRowProps {
  step: ExportJobStep;
  isCurrentStep: boolean;
  stepProgress?: number;
  onRetry?: (stepId: number) => void;
  isLast: boolean;
  compact?: boolean;
}

function StepRow({ step, isCurrentStep, stepProgress, onRetry, isLast, compact = false }: StepRowProps) {
  const [expanded, setExpanded] = useState(false);
  const hasDetails = step.stepData || step.errorMessage || step.outputFiles.length > 0;
  const elapsedTime = calculateElapsedTime(step.startedAt, step.completedAt);

  return (
    <div className={`${isCurrentStep ? 'bg-accent/5' : ''} ${compact ? 'pl-8' : ''}`}>
      {/* Main Row */}
      <div
        className={`flex items-center gap-3 p-3 ${compact ? 'py-2' : ''} ${hasDetails ? 'cursor-pointer hover:bg-surface-hover/50' : ''}`}
        onClick={() => hasDetails && setExpanded(!expanded)}
      >
        {/* Step Number & Connector */}
        <div className={`flex flex-col items-center ${compact ? 'w-6' : 'w-8'}`}>
          <StepStatusIcon status={step.status} isCurrentStep={isCurrentStep} />
          {!isLast && !compact && (
            <div className={`w-0.5 h-4 mt-1 ${step.status === 'completed' ? 'bg-success/30' : 'bg-border'}`} />
          )}
        </div>

        {/* Step Icon */}
        <StepTypeIcon stepType={step.stepType} status={step.status} />

        {/* Step Info */}
        <div className="flex-1 min-w-0">
          <div className="flex items-center gap-2">
            <span className={`font-medium ${step.status === 'failed' ? 'text-error' : 'text-content'}`}>
              {step.stepName}
            </span>
            {step.stepData?.filter && (
              <span className="text-xs px-1.5 py-0.5 bg-accent/10 text-accent rounded">
                {step.stepData.filter}
              </span>
            )}
          </div>
          <div className="text-sm text-content-muted">
            {getStepTypeLabel(step.stepType)}
            {step.stepData?.frameCount && ` • ${step.stepData.frameCount} frames`}
          </div>
        </div>

        {/* Step Progress (for current step) */}
        {isCurrentStep && stepProgress !== undefined && (
          <div className="w-24">
            <div className="w-full bg-surface-hover rounded-full h-1.5">
              <div
                className="bg-accent h-1.5 rounded-full transition-all duration-300"
                style={{ width: `${stepProgress}%` }}
              />
            </div>
            <div className="text-xs text-content-muted text-right mt-0.5">
              {stepProgress.toFixed(0)}%
            </div>
          </div>
        )}

        {/* Duration */}
        {elapsedTime !== null && step.status !== 'pending' && (
          <div className="text-sm text-content-muted w-16 text-right">
            {formatDuration(elapsedTime)}
          </div>
        )}

        {/* Actions */}
        <div className="flex items-center gap-2">
          {step.status === 'failed' && onRetry && (
            <button
              onClick={(e) => {
                e.stopPropagation();
                onRetry(step.id);
              }}
              className="p-1.5 rounded bg-surface-hover hover:bg-warning/20 transition-colors"
              title="Retry this step"
            >
              <RotateCcw size={14} className="text-warning" />
            </button>
          )}
          {hasDetails && (
            expanded ? (
              <ChevronDown size={16} className="text-content-muted" />
            ) : (
              <ChevronRight size={16} className="text-content-muted" />
            )
          )}
        </div>
      </div>

      {/* Expanded Details */}
      {expanded && (
        <div className="px-3 pb-3 ml-11 space-y-2">
          {/* Step Data */}
          {step.stepData && (
            <div className="p-2 bg-surface-elevated rounded text-sm">
              <div className="text-xs text-content-muted uppercase tracking-wide mb-1">Step Details</div>
              <div className="grid grid-cols-2 gap-2">
                {step.stepData.setId && (
                  <div>
                    <span className="text-content-muted">Set ID: </span>
                    <span className="text-content">{step.stepData.setId}</span>
                  </div>
                )}
                {step.stepData.branchId && (
                  <div>
                    <span className="text-content-muted">Branch: </span>
                    <span className="text-content">{step.stepData.branchId}</span>
                  </div>
                )}
                {step.stepData.filter && (
                  <div>
                    <span className="text-content-muted">Filter: </span>
                    <span className="text-content">{step.stepData.filter}</span>
                  </div>
                )}
                {step.stepData.cameraType && (
                  <div>
                    <span className="text-content-muted">Camera: </span>
                    <span className="text-content">{step.stepData.cameraType.toUpperCase()}</span>
                  </div>
                )}
                {step.stepData.frameCount && (
                  <div>
                    <span className="text-content-muted">Frames: </span>
                    <span className="text-content">{step.stepData.frameCount}</span>
                  </div>
                )}
                {step.stepData.outputPath && (
                  <div className="col-span-2">
                    <span className="text-content-muted">Output: </span>
                    <span className="text-content font-mono text-xs">{step.stepData.outputPath}</span>
                  </div>
                )}
              </div>
            </div>
          )}

          {/* Error Message */}
          {step.errorMessage && (
            <div className="p-2 bg-error/10 rounded border border-error/20">
              <div className="flex items-start gap-2">
                <AlertCircle size={14} className="text-error mt-0.5" />
                <div className="text-sm text-error">{step.errorMessage}</div>
              </div>
              {step.retryCount > 0 && (
                <div className="text-xs text-error/70 mt-1 ml-6">
                  Retry attempts: {step.retryCount}
                </div>
              )}
            </div>
          )}

          {/* Output Files */}
          {step.outputFiles.length > 0 && (
            <div className="p-2 bg-surface-elevated rounded">
              <div className="text-xs text-content-muted uppercase tracking-wide mb-1">Output Files</div>
              <div className="space-y-1">
                {step.outputFiles.map((file, idx) => (
                  <div key={idx} className="text-sm font-mono text-content-secondary truncate">
                    {file}
                  </div>
                ))}
              </div>
            </div>
          )}

          {/* Timestamps */}
          <div className="flex items-center gap-4 text-xs text-content-muted">
            {step.startedAt && (
              <span>Started: {new Date(step.startedAt).toLocaleTimeString()}</span>
            )}
            {step.completedAt && (
              <span>Completed: {new Date(step.completedAt).toLocaleTimeString()}</span>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

// ============================================================================
// Status Icon Components
// ============================================================================

function JobStatusIcon({ status }: { status: ExportJob['status'] }) {
  switch (status) {
    case 'pending':
      return <Clock size={24} className="text-gray-400" />;
    case 'running':
      return <Loader2 size={24} className="animate-spin text-accent" />;
    case 'paused':
      return <Pause size={24} className="text-warning" />;
    case 'completed':
      return <Check size={24} className="text-success" />;
    case 'failed':
      return <X size={24} className="text-error" />;
    case 'cancelled':
      return <X size={24} className="text-gray-500" />;
    default:
      return <Info size={24} className="text-gray-400" />;
  }
}

interface StepStatusIconProps {
  status: StepStatus;
  isCurrentStep: boolean;
}

function StepStatusIcon({ status }: StepStatusIconProps) {
  const baseClasses = 'w-6 h-6 rounded-full flex items-center justify-center';

  switch (status) {
    case 'pending':
      return (
        <div className={`${baseClasses} bg-gray-700`}>
          <Clock size={12} className="text-gray-400" />
        </div>
      );
    case 'running':
      return (
        <div className={`${baseClasses} bg-accent/20`}>
          <Loader2 size={12} className="animate-spin text-accent" />
        </div>
      );
    case 'completed':
      return (
        <div className={`${baseClasses} bg-success/20`}>
          <Check size={12} className="text-success" />
        </div>
      );
    case 'failed':
      return (
        <div className={`${baseClasses} bg-error/20`}>
          <X size={12} className="text-error" />
        </div>
      );
    case 'skipped':
      return (
        <div className={`${baseClasses} bg-gray-700`}>
          <SkipForward size={12} className="text-gray-400" />
        </div>
      );
    default:
      return (
        <div className={`${baseClasses} bg-gray-700`}>
          <Clock size={12} className="text-gray-400" />
        </div>
      );
  }
}

function StepTypeIcon({ stepType, status }: { stepType: StepType; status: StepStatus }) {
  const iconColor = status === 'failed' ? 'text-error' : status === 'completed' ? 'text-success' : 'text-content-muted';
  const iconSize = 16;

  switch (stepType) {
    // Pre-processing
    case 'validate_sources':
      return <FileCheck size={iconSize} className={iconColor} />;
    case 'organize_files':
      return <FolderTree size={iconSize} className={iconColor} />;
    // Per-master steps (new) - use Layers for all master types
    case 'create_master_bias':
    case 'create_master_dark':
    case 'create_master_darkflat':
    case 'create_master_flat':
    case 'create_masters':
      return <Layers size={iconSize} className={iconColor} />;
    // Per-branch calibration (new)
    case 'calibrate_branch':
    case 'calibrate_lights':
      return <Wand2 size={iconSize} className={iconColor} />;
    // Collection
    case 'collect_calibrated':
      return <FolderInput size={iconSize} className={iconColor} />;
    case 'generate_registration':
      return <FileCode size={iconSize} className={iconColor} />;
    // Per-camera registration (new)
    case 'register_frames':
    case 'register_and_stack':
      return <Target size={iconSize} className={iconColor} />;
    // Per-group stacking (new)
    case 'stack_group':
      return <Box size={iconSize} className={iconColor} />;
    default:
      return <Info size={iconSize} className={iconColor} />;
  }
}

// Helper to get phase icon
function PhaseIcon({ phase }: { phase: StepPhase }) {
  const iconColor = 'text-content-muted';
  const iconSize = 16;

  switch (phase) {
    case 'preparation':
      return <FileCheck size={iconSize} className={iconColor} />;
    case 'masters':
      return <Layers size={iconSize} className={iconColor} />;
    case 'calibration':
      return <Wand2 size={iconSize} className={iconColor} />;
    case 'branch_registration':
      return <Target size={iconSize} className={iconColor} />;
    case 'collection':
      return <FolderInput size={iconSize} className={iconColor} />;
    case 'global_registration':
      return <Target size={iconSize} className={iconColor} />;
    case 'prepare_stacking':
      return <FolderInput size={iconSize} className={iconColor} />;
    case 'stacking':
      return <Box size={iconSize} className={iconColor} />;
    default:
      return <Circle size={iconSize} className={iconColor} />;
  }
}

// ============================================================================
// Compact Pipeline View (for sidebars, summaries)
// ============================================================================

interface PipelineCompactViewProps {
  job: ExportJob;
  onClick?: () => void;
}

export function PipelineCompactView({ job, onClick }: PipelineCompactViewProps) {
  const completedSteps = job.steps.filter(s => s.status === 'completed').length;
  const failedSteps = job.steps.filter(s => s.status === 'failed').length;
  const progress = (completedSteps / job.totalSteps) * 100;

  return (
    <div
      className={`p-3 bg-surface rounded-lg border border-border ${onClick ? 'cursor-pointer hover:bg-surface-hover' : ''}`}
      onClick={onClick}
    >
      <div className="flex items-center justify-between mb-2">
        <div className="flex items-center gap-2">
          <JobStatusIcon status={job.status} />
          <span className="font-medium">Job #{job.id}</span>
        </div>
        <span className={`text-sm px-2 py-0.5 rounded ${getStatusBgColor(job.status)} ${getStatusColor(job.status)}`}>
          {job.status}
        </span>
      </div>

      <div className="w-full bg-surface-hover rounded-full h-1.5 mb-2">
        <div
          className={`h-1.5 rounded-full ${
            job.status === 'failed' ? 'bg-error' : job.status === 'completed' ? 'bg-success' : 'bg-accent'
          }`}
          style={{ width: `${progress}%` }}
        />
      </div>

      <div className="flex items-center justify-between text-xs text-content-muted">
        <span>{completedSteps} of {job.totalSteps} steps</span>
        {failedSteps > 0 && <span className="text-error">{failedSteps} failed</span>}
      </div>
    </div>
  );
}

// ============================================================================
// Pipeline Steps Summary (for pre-execution preview)
// ============================================================================

interface PipelineStepsSummaryProps {
  steps: Array<{
    stepType: StepType;
    stepName: string;
    dependsOn: number[];
  }>;
}

export function PipelineStepsSummary({ steps }: PipelineStepsSummaryProps) {
  return (
    <div className="bg-surface rounded-lg border border-border">
      <div className="p-3 bg-surface-elevated border-b border-border">
        <h4 className="font-medium">Pipeline Steps ({steps.length})</h4>
        <p className="text-sm text-content-muted">Steps will be executed in order</p>
      </div>
      <div className="divide-y divide-border max-h-64 overflow-y-auto">
        {steps.map((step, index) => (
          <div key={index} className="flex items-center gap-3 p-2 px-3">
            <span className="w-6 h-6 rounded-full bg-surface-elevated flex items-center justify-center text-xs text-content-muted">
              {index + 1}
            </span>
            <StepTypeIcon stepType={step.stepType} status="pending" />
            <div className="flex-1 min-w-0">
              <div className="text-sm text-content truncate">{step.stepName}</div>
            </div>
          </div>
        ))}
      </div>
    </div>
  );
}
