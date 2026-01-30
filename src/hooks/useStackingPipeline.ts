import { useState, useEffect, useCallback, useRef } from 'react';
import { invoke } from '@tauri-apps/api/core';
import { listen, type UnlistenFn } from '@tauri-apps/api/event';
import type {
  StackingJob,
  StackingConfig,
  StackingProgress,
} from '../types/stacking';

// ============================================================================
// Availability Check Hook
// ============================================================================

interface UseStackingAvailableResult {
  available: boolean;
  loading: boolean;
  error: string | null;
  refresh: () => void;
}

/**
 * Hook to check if stacking is available
 */
export function useStackingAvailable(): UseStackingAvailableResult {
  const [available, setAvailable] = useState(false);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const checkAvailability = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await invoke<boolean>('check_stacking_available');
      setAvailable(result);
    } catch (err) {
      setError(err as string);
      setAvailable(false);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    checkAvailability();
  }, [checkAvailability]);

  return { available, loading, error, refresh: checkAvailability };
}

// ============================================================================
// Default Config Hook
// ============================================================================

interface UseDefaultStackingConfigResult {
  config: StackingConfig | null;
  loading: boolean;
  error: string | null;
  refresh: () => void;
}

/**
 * Hook to get the default stacking configuration
 */
export function useDefaultStackingConfig(): UseDefaultStackingConfigResult {
  const [config, setConfig] = useState<StackingConfig | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const loadConfig = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await invoke<StackingConfig>('get_default_stacking_config');
      setConfig(result);
    } catch (err) {
      setError(err as string);
      setConfig(null);
    } finally {
      setLoading(false);
    }
  }, []);

  useEffect(() => {
    loadConfig();
  }, [loadConfig]);

  return { config, loading, error, refresh: loadConfig };
}

// ============================================================================
// Single Job Hook
// ============================================================================

interface UseStackingJobResult {
  job: StackingJob | null;
  loading: boolean;
  error: string | null;
  refresh: () => void;
}

/**
 * Hook to fetch a single stacking job with all its steps
 */
export function useStackingJob(jobId: number | null): UseStackingJobResult {
  const [job, setJob] = useState<StackingJob | null>(null);
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
      const result = await invoke<StackingJob>('get_stacking_job', { jobId });
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

interface UseStackingJobsResult {
  jobs: StackingJob[];
  loading: boolean;
  error: string | null;
  refresh: () => void;
}

/**
 * Hook to fetch all stacking jobs, optionally filtered by frame set
 */
export function useStackingJobs(frameSetId?: number | null): UseStackingJobsResult {
  const [jobs, setJobs] = useState<StackingJob[]>([]);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);

  const loadJobs = useCallback(async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await invoke<StackingJob[]>('get_stacking_jobs', {
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
// Job Creation Hook
// ============================================================================

interface UseStackingJobCreateResult {
  create: (params: CreateStackingJobParams) => Promise<StackingJob>;
  loading: boolean;
  error: string | null;
}

interface CreateStackingJobParams {
  frameSetId: number;
  outputDir: string;
  config: StackingConfig;
}

/**
 * Hook to create a new stacking job
 */
export function useStackingJobCreate(): UseStackingJobCreateResult {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const create = useCallback(async (params: CreateStackingJobParams): Promise<StackingJob> => {
    try {
      setLoading(true);
      setError(null);
      const job = await invoke<StackingJob>('create_stacking_job', {
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

interface UseStackingJobControlResult {
  start: (overrideJobId?: number) => Promise<void>;
  cancel: () => Promise<void>;
  deleteJob: () => Promise<void>;
  loading: boolean;
  error: string | null;
}

/**
 * Hook to control a stacking job (start, cancel, delete)
 */
export function useStackingJobControl(jobId: number | null): UseStackingJobControlResult {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const start = useCallback(async (overrideJobId?: number) => {
    const targetJobId = overrideJobId ?? jobId;
    if (targetJobId === null) return;
    try {
      setLoading(true);
      setError(null);
      await invoke('start_stacking_job', { jobId: targetJobId });
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
      await invoke('cancel_stacking_job', { jobId });
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
      await invoke('delete_stacking_job', { jobId });
    } catch (err) {
      setError(err as string);
      throw err;
    } finally {
      setLoading(false);
    }
  }, [jobId]);

  return { start, cancel, deleteJob, loading, error };
}

// ============================================================================
// Job Progress Hook (Event Listener)
// ============================================================================

interface UseStackingProgressResult {
  /** Current progress state */
  progress: StackingProgress | null;
  /** Current step type being executed */
  currentStepType: string | null;
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
 * Hook to listen for stacking progress events
 * Subscribes to `stacking-progress` events from the backend
 */
export function useStackingProgress(jobId: number | null): UseStackingProgressResult {
  const [progress, setProgress] = useState<StackingProgress | null>(null);
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

      unlistenRef.current = await listen<StackingProgress>('stacking-progress', (event) => {
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

export interface UseStackingPipelineResult {
  // Job state
  job: StackingJob | null;
  jobLoading: boolean;
  jobError: string | null;
  refreshJob: () => void;

  // Control actions
  startJob: (overrideJobId?: number) => Promise<void>;
  cancelJob: () => Promise<void>;
  deleteJob: () => Promise<void>;
  controlLoading: boolean;

  // Progress
  progress: StackingProgress | null;
  overallProgress: number;
  isRunning: boolean;
  isComplete: boolean;
  isFailed: boolean;
}

/**
 * Combined hook that provides all stacking functionality for a single job
 */
export function useStackingPipeline(jobId: number | null): UseStackingPipelineResult {
  const { job, loading: jobLoading, error: jobError, refresh: refreshJob } = useStackingJob(jobId);
  const { start, cancel, deleteJob, loading: controlLoading } = useStackingJobControl(jobId);
  const { progress, overallProgress, isRunning, isComplete, isFailed } = useStackingProgress(jobId);

  return {
    job,
    jobLoading,
    jobError,
    refreshJob,
    startJob: start,
    cancelJob: cancel,
    deleteJob,
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
 * Format duration in milliseconds to human-readable string
 */
export function formatDurationMs(ms: number): string {
  const seconds = Math.floor(ms / 1000);
  if (seconds < 60) {
    return `${seconds}s`;
  }
  const minutes = Math.floor(seconds / 60);
  const remainingSeconds = seconds % 60;
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
  return end - start;
}
