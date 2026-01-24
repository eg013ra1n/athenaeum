import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type {
  ExportJob,
  ExportConfig,
  FileValidation,
  PipelinePlan,
  PipelineProgress,
  JobStatus,
  StepStatus,
  StepType,
} from '../types/export';

// ============================================================================
// Single Job Hook
// ============================================================================

interface UseExportJobResult {
  job: ExportJob | null;
  loading: boolean;
  error: string | null;
  refresh: () => void;
}

/**
 * Hook to fetch a single export job with all its steps
 */
export function useExportJob(jobId: number | null): UseExportJobResult {
  const [job, setJob] = useState<ExportJob | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadJob = useCallback(async () => {
    if (jobId === null) {
      setJob(null);
      return;
    }

    try {
      setLoading(true);
      setError(null);
      const result = await invoke<ExportJob>('get_export_job', { jobId });
      setJob(result);
    } catch (err) {
      setError(err as string);
      setJob(null);
    } finally {
      setLoading(false);
    }
  }, [jobId]);

  useEffect(() => {
    loadJob();
  }, [loadJob]);

  // Auto-refresh while job is running
  useEffect(() => {
    if (job?.status === 'running') {
      const interval = setInterval(() => {
        loadJob();
      }, 2000); // Poll every 2 seconds
      return () => clearInterval(interval);
    }
  }, [job?.status, loadJob]);

  return { job, loading, error, refresh: loadJob };
}

// ============================================================================
// Jobs List Hook
// ============================================================================

interface UseExportJobsResult {
  jobs: ExportJob[];
  loading: boolean;
  error: string | null;
  refresh: () => void;
}

/**
 * Hook to fetch all export jobs, optionally filtered by frame set
 */
export function useExportJobs(frameSetId?: number | null): UseExportJobsResult {
  const [jobs, setJobs] = useState<ExportJob[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const loadJobs = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await invoke<ExportJob[]>('get_export_jobs', {
        frameSetId: frameSetId ?? null,
      });
      setJobs(result);
    } catch (err) {
      setError(err as string);
      setJobs([]);
    } finally {
      setLoading(false);
    }
  }, [frameSetId]);

  useEffect(() => {
    loadJobs();
  }, [loadJobs]);

  return { jobs, loading, error, refresh: loadJobs };
}

// ============================================================================
// Job History Hook
// ============================================================================

interface UseJobHistoryResult {
  jobs: ExportJob[];
  loading: boolean;
  error: string | null;
  refresh: () => void;
}

/**
 * Hook to fetch recent job history
 */
export function useJobHistory(limit?: number): UseJobHistoryResult {
  const [jobs, setJobs] = useState<ExportJob[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const loadHistory = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await invoke<ExportJob[]>('get_job_history', {
        limit: limit ?? null,
      });
      setJobs(result);
    } catch (err) {
      setError(err as string);
      setJobs([]);
    } finally {
      setLoading(false);
    }
  }, [limit]);

  useEffect(() => {
    loadHistory();
  }, [loadHistory]);

  return { jobs, loading, error, refresh: loadHistory };
}

// ============================================================================
// Job Creation Hook
// ============================================================================

interface UseExportJobCreateResult {
  create: (config: CreateJobParams) => Promise<ExportJob>;
  loading: boolean;
  error: string | null;
}

interface CreateJobParams {
  frameSetId: number;
  outputDir: string;
  config: ExportConfig;
}

/**
 * Hook to create a new export job
 */
export function useExportJobCreate(): UseExportJobCreateResult {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const create = useCallback(async (params: CreateJobParams): Promise<ExportJob> => {
    try {
      setLoading(true);
      setError(null);
      const job = await invoke<ExportJob>('create_export_job', {
        frameSetId: params.frameSetId,
        outputDir: params.outputDir,
        config: params.config,
      });
      return job;
    } catch (err) {
      const errorMessage = err as string;
      setError(errorMessage);
      throw new Error(errorMessage);
    } finally {
      setLoading(false);
    }
  }, []);

  return { create, loading, error };
}

// ============================================================================
// Job Control Hook
// ============================================================================

interface UseExportJobControlResult {
  start: (overrideJobId?: number) => Promise<void>;
  pause: () => Promise<void>;
  resume: (fromStep?: number) => Promise<void>;
  cancel: () => Promise<void>;
  retryStep: (stepId: number) => Promise<void>;
  deleteJob: () => Promise<void>;
  loading: boolean;
  error: string | null;
}

/**
 * Hook to control an export job (start, pause, resume, cancel, retry)
 */
export function useExportJobControl(jobId: number | null): UseExportJobControlResult {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const start = useCallback(async (overrideJobId?: number) => {
    const targetJobId = overrideJobId ?? jobId;
    if (targetJobId === null) return;
    try {
      setLoading(true);
      setError(null);
      await invoke('start_export_job', { jobId: targetJobId });
    } catch (err) {
      setError(err as string);
      throw err;
    } finally {
      setLoading(false);
    }
  }, [jobId]);

  const pause = useCallback(async () => {
    if (jobId === null) return;
    try {
      setLoading(true);
      setError(null);
      await invoke('pause_export_job', { jobId });
    } catch (err) {
      setError(err as string);
      throw err;
    } finally {
      setLoading(false);
    }
  }, [jobId]);

  const resume = useCallback(async (fromStep?: number) => {
    if (jobId === null) return;
    try {
      setLoading(true);
      setError(null);
      await invoke('resume_export_job', { jobId, fromStep: fromStep ?? null });
    } catch (err) {
      setError(err as string);
      throw err;
    } finally {
      setLoading(false);
    }
  }, [jobId]);

  const cancel = useCallback(async () => {
    if (jobId === null) return;
    try {
      setLoading(true);
      setError(null);
      await invoke('cancel_export_job', { jobId });
    } catch (err) {
      setError(err as string);
      throw err;
    } finally {
      setLoading(false);
    }
  }, [jobId]);

  const retryStep = useCallback(async (stepId: number) => {
    if (jobId === null) return;
    try {
      setLoading(true);
      setError(null);
      await invoke('retry_failed_step', { jobId, stepId });
    } catch (err) {
      setError(err as string);
      throw err;
    } finally {
      setLoading(false);
    }
  }, [jobId]);

  const deleteJob = useCallback(async () => {
    if (jobId === null) return;
    try {
      setLoading(true);
      setError(null);
      await invoke('delete_export_job', { jobId });
    } catch (err) {
      setError(err as string);
      throw err;
    } finally {
      setLoading(false);
    }
  }, [jobId]);

  return { start, pause, resume, cancel, retryStep, deleteJob, loading, error };
}

// ============================================================================
// Pipeline Plan Hook
// ============================================================================

interface UsePipelinePlanResult {
  plan: PipelinePlan | null;
  loading: boolean;
  error: string | null;
  refresh: () => void;
}

/**
 * Hook to fetch the pipeline plan for a frame set
 */
export function usePipelinePlan(
  frameSetId: number | null,
  config: ExportConfig | null
): UsePipelinePlanResult {
  const [plan, setPlan] = useState<PipelinePlan | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadPlan = useCallback(async () => {
    if (frameSetId === null || config === null) {
      setPlan(null);
      return;
    }

    try {
      setLoading(true);
      setError(null);
      const result = await invoke<PipelinePlan>('get_pipeline_plan', {
        frameSetId,
        config,
      });
      setPlan(result);
    } catch (err) {
      setError(err as string);
      setPlan(null);
    } finally {
      setLoading(false);
    }
  }, [frameSetId, config]);

  useEffect(() => {
    loadPlan();
  }, [loadPlan]);

  return { plan, loading, error, refresh: loadPlan };
}

// ============================================================================
// Source Validation Hook
// ============================================================================

interface UseSourceValidationResult {
  validations: FileValidation[];
  loading: boolean;
  error: string | null;
  missingCount: number;
  totalCount: number;
  refresh: () => void;
}

/**
 * Hook to validate source files for a frame set
 */
export function useSourceValidation(frameSetId: number | null): UseSourceValidationResult {
  const [validations, setValidations] = useState<FileValidation[]>([]);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const loadValidations = useCallback(async () => {
    if (frameSetId === null) {
      setValidations([]);
      return;
    }

    try {
      setLoading(true);
      setError(null);
      const result = await invoke<FileValidation[]>('validate_export_sources', {
        frameSetId,
      });
      setValidations(result);
    } catch (err) {
      setError(err as string);
      setValidations([]);
    } finally {
      setLoading(false);
    }
  }, [frameSetId]);

  useEffect(() => {
    loadValidations();
  }, [loadValidations]);

  const missingCount = validations.filter(v => !v.exists).length;
  const totalCount = validations.length;

  return { validations, loading, error, missingCount, totalCount, refresh: loadValidations };
}

// ============================================================================
// Job Progress Hook (Event Listener)
// ============================================================================

interface UseJobProgressResult {
  /** Current progress state */
  progress: PipelineProgress | null;
  /** Current step type being executed */
  currentStepType: StepType | null;
  /** Overall completion percentage */
  overallProgress: number;
  /** Whether the job is currently running */
  isRunning: boolean;
  /** Whether the job has completed */
  isComplete: boolean;
  /** Whether the job has failed */
  isFailed: boolean;
  /** Error message if failed */
  error: string | null;
  /** Clear the progress state */
  reset: () => void;
}

/**
 * Hook to listen for pipeline progress events
 * Subscribes to `pipeline-progress` events from the backend
 */
export function useJobProgress(jobId: number | null): UseJobProgressResult {
  const [progress, setProgress] = useState<PipelineProgress | null>(null);
  const [error, setError] = useState<string | null>(null);
  const unlistenRef = useRef<UnlistenFn | null>(null);

  useEffect(() => {
    if (jobId === null) {
      setProgress(null);
      return;
    }

    const setupListener = async () => {
      // Clean up previous listener
      if (unlistenRef.current) {
        unlistenRef.current();
      }

      unlistenRef.current = await listen<PipelineProgress>('pipeline-progress', (event) => {
        // Only process events for our job
        if (event.payload.jobId === jobId) {
          setProgress(event.payload);

          // Auto-detect failure from message
          if (event.payload.message.toLowerCase().includes('failed')) {
            setError(event.payload.message);
          }
        }
      });
    };

    setupListener();

    return () => {
      if (unlistenRef.current) {
        unlistenRef.current();
        unlistenRef.current = null;
      }
    };
  }, [jobId]);

  const reset = useCallback(() => {
    setProgress(null);
    setError(null);
  }, []);

  const currentStepType = progress?.stepType ?? null;
  const overallProgress = progress?.overallProgress ?? 0;
  const isRunning = progress !== null && overallProgress < 100 && !error;
  const isComplete = overallProgress >= 100;
  const isFailed = error !== null;

  return {
    progress,
    currentStepType,
    overallProgress,
    isRunning,
    isComplete,
    isFailed,
    error,
    reset,
  };
}

// ============================================================================
// Combined Pipeline Hook
// ============================================================================

interface UseExportPipelineResult {
  // Job state
  job: ExportJob | null;
  jobLoading: boolean;
  jobError: string | null;
  refreshJob: () => void;

  // Control actions
  startJob: (overrideJobId?: number) => Promise<void>;
  pauseJob: () => Promise<void>;
  resumeJob: (fromStep?: number) => Promise<void>;
  cancelJob: () => Promise<void>;
  retryStep: (stepId: number) => Promise<void>;
  controlLoading: boolean;

  // Progress
  progress: PipelineProgress | null;
  overallProgress: number;
  isRunning: boolean;
  isComplete: boolean;
  isFailed: boolean;
}

/**
 * Combined hook that provides all pipeline functionality for a single job
 */
export function useExportPipeline(jobId: number | null): UseExportPipelineResult {
  const { job, loading: jobLoading, error: jobError, refresh: refreshJob } = useExportJob(jobId);
  const { start, pause, resume, cancel, retryStep, loading: controlLoading } = useExportJobControl(jobId);
  const { progress, overallProgress, isRunning, isComplete, isFailed } = useJobProgress(jobId);

  return {
    job,
    jobLoading,
    jobError,
    refreshJob,
    startJob: start,
    pauseJob: pause,
    resumeJob: resume,
    cancelJob: cancel,
    retryStep,
    controlLoading,
    progress,
    overallProgress,
    isRunning,
    isComplete,
    isFailed,
  };
}

// ============================================================================
// Helper Functions
// ============================================================================

/**
 * Get a human-readable label for a job status
 */
export function getJobStatusLabel(status: JobStatus): string {
  switch (status) {
    case 'pending':
      return 'Pending';
    case 'running':
      return 'Running';
    case 'paused':
      return 'Paused';
    case 'completed':
      return 'Completed';
    case 'failed':
      return 'Failed';
    case 'cancelled':
      return 'Cancelled';
    default:
      return status;
  }
}

/**
 * Get a human-readable label for a step status
 */
export function getStepStatusLabel(status: StepStatus): string {
  switch (status) {
    case 'pending':
      return 'Pending';
    case 'running':
      return 'Running';
    case 'completed':
      return 'Completed';
    case 'failed':
      return 'Failed';
    case 'skipped':
      return 'Skipped';
    default:
      return status;
  }
}

/**
 * Get a human-readable label for a step type
 */
export function getStepTypeLabel(stepType: StepType): string {
  switch (stepType) {
    // Pre-processing
    case 'validate_sources':
      return 'Validate Sources';
    case 'organize_files':
      return 'Organize Files';
    // Per-master steps (new)
    case 'create_master_bias':
      return 'Create Master Bias';
    case 'create_master_dark':
      return 'Create Master Dark';
    case 'create_master_darkflat':
      return 'Create Master DarkFlat';
    case 'create_master_flat':
      return 'Create Master Flat';
    // Per-branch calibration (new)
    case 'calibrate_branch':
      return 'Calibrate Branch';
    // Collection
    case 'collect_calibrated':
      return 'Collect Calibrated Frames';
    // Per-camera registration (new)
    case 'register_frames':
      return 'Register Frames';
    // Per-group stacking (new)
    case 'stack_group':
      return 'Stack Group';
    // Legacy types
    case 'create_masters':
      return 'Create Master Calibrations';
    case 'calibrate_lights':
      return 'Calibrate Light Frames';
    case 'generate_registration':
      return 'Generate Registration Script';
    case 'register_and_stack':
      return 'Register & Stack';
    default:
      return stepType;
  }
}

/**
 * Get the appropriate icon name for a step type (for lucide-react)
 */
export function getStepTypeIcon(stepType: StepType): string {
  switch (stepType) {
    // Pre-processing
    case 'validate_sources':
      return 'FileCheck';
    case 'organize_files':
      return 'FolderTree';
    // Per-master steps (new) - use Layers for all master types
    case 'create_master_bias':
    case 'create_master_dark':
    case 'create_master_darkflat':
    case 'create_master_flat':
    case 'create_masters':
      return 'Layers';
    // Per-branch calibration (new)
    case 'calibrate_branch':
    case 'calibrate_lights':
      return 'Wand2';
    // Collection
    case 'collect_calibrated':
      return 'FolderInput';
    case 'generate_registration':
      return 'FileCode';
    // Per-camera registration (new)
    case 'register_frames':
    case 'register_and_stack':
      return 'Target';
    // Per-group stacking (new)
    case 'stack_group':
      return 'Layers';
    default:
      return 'Circle';
  }
}

/**
 * Get the phase for a step type
 */
export function getStepPhase(stepType: StepType): import('../types/export').StepPhase {
  switch (stepType) {
    case 'validate_sources':
    case 'organize_files':
      return 'preparation';
    case 'create_master_bias':
    case 'create_master_dark':
    case 'create_master_darkflat':
    case 'create_master_flat':
    case 'create_masters':
      return 'masters';
    case 'calibrate_branch':
    case 'calibrate_lights':
      return 'calibration';
    case 'register_branch':
      return 'branch_registration';
    case 'collect_calibrated':
    case 'collect_registered':
    case 'generate_registration':
      return 'collection';
    case 'global_registration':
    case 'register_frames':
    case 'register_and_stack':
      return 'global_registration';
    case 'prepare_stacking':
      return 'prepare_stacking';
    case 'stack_group':
      return 'stacking';
    default:
      return 'preparation';
  }
}

/**
 * Get human-readable label for a step phase
 */
export function getPhaseLabel(phase: import('../types/export').StepPhase): string {
  switch (phase) {
    case 'preparation':
      return 'Preparation';
    case 'masters':
      return 'Masters';
    case 'calibration':
      return 'Calibration';
    case 'branch_registration':
      return 'Branch Registration';
    case 'collection':
      return 'Collection';
    case 'global_registration':
      return 'Global Registration';
    case 'prepare_stacking':
      return 'Prepare Stacking';
    case 'stacking':
      return 'Stacking';
    default:
      return phase;
  }
}

/**
 * Check if a step type is a legacy (monolithic) type
 */
export function isLegacyStepType(stepType: StepType): boolean {
  return [
    'create_masters',
    'calibrate_lights',
    'generate_registration',
    'register_and_stack',
  ].includes(stepType);
}

/**
 * Get the appropriate color class for a job/step status
 */
export function getStatusColor(status: JobStatus | StepStatus): string {
  switch (status) {
    case 'pending':
      return 'text-gray-400';
    case 'running':
      return 'text-blue-400';
    case 'completed':
      return 'text-green-400';
    case 'failed':
      return 'text-red-400';
    case 'paused':
      return 'text-yellow-400';
    case 'cancelled':
      return 'text-gray-500';
    case 'skipped':
      return 'text-gray-400';
    default:
      return 'text-gray-400';
  }
}

/**
 * Get the appropriate background color class for a status badge
 */
export function getStatusBgColor(status: JobStatus | StepStatus): string {
  switch (status) {
    case 'pending':
      return 'bg-gray-700';
    case 'running':
      return 'bg-blue-900/50';
    case 'completed':
      return 'bg-green-900/50';
    case 'failed':
      return 'bg-red-900/50';
    case 'paused':
      return 'bg-yellow-900/50';
    case 'cancelled':
      return 'bg-gray-800';
    case 'skipped':
      return 'bg-gray-700';
    default:
      return 'bg-gray-700';
  }
}

/**
 * Format duration in seconds to human-readable string
 */
export function formatDuration(seconds: number): string {
  if (seconds < 60) {
    return `${Math.round(seconds)}s`;
  }
  const minutes = Math.floor(seconds / 60);
  const remainingSeconds = Math.round(seconds % 60);
  if (minutes < 60) {
    return `${minutes}m ${remainingSeconds}s`;
  }
  const hours = Math.floor(minutes / 60);
  const remainingMinutes = minutes % 60;
  return `${hours}h ${remainingMinutes}m`;
}

/**
 * Calculate elapsed time between two ISO date strings
 */
export function calculateElapsedTime(startedAt: string | null, completedAt: string | null): number | null {
  if (!startedAt) return null;
  const start = new Date(startedAt).getTime();
  const end = completedAt ? new Date(completedAt).getTime() : Date.now();
  return (end - start) / 1000;
}
