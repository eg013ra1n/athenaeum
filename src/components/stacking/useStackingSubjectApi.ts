import { useMemo } from 'react';
import { api } from '../../api';
import type {
  CleanupWhat,
  StackingConfig,
  StackingPlan,
  StackingProjectConfig,
  StackingRunSummary,
  StackingSetConfig,
  WorkUsage,
} from '../../types/stacking';
import { subjectKey, type StackSubject } from './subject';

/** A subject's stored stacking document, in one shape for both kinds. The exclusion keys are strings:
 *  `String(frameId)` for a frame set, the frame uuid for a project. `referenceFrameUuid` is always `null`
 *  for a frame set (it has no manual reference) and is never sent for one. */
export type SubjectConfig = {
  config: StackingConfig;
  excluded: string[];
  referenceFrameUuid: string | null;
  isDefault: boolean;
  updatedAt: string | null;
};

/** Every stacking command the tab calls, with the subject's id already bound. */
export interface StackingSubjectApi {
  getPlan(config?: StackingConfig): Promise<StackingPlan>;
  getConfig(): Promise<SubjectConfig>;
  setConfig(c: SubjectConfig): Promise<SubjectConfig>;
  getRuns(limit: number): Promise<StackingRunSummary[]>;
  getWorkUsage(): Promise<WorkUsage>;
  cleanup(what: CleanupWhat): Promise<number>;
}

// One place that logs a failed command, so no caller can forget the trace. The error is rethrown: each caller
// decides what its own state does with it.
async function call<T>(command: string, args: Record<string, unknown>): Promise<T> {
  try {
    return await api.invoke<T>(command, args);
  } catch (err) {
    console.error(`[useStackingSubjectApi] ${command} failed:`, err);
    throw err;
  }
}

function fromSetConfig(c: StackingSetConfig): SubjectConfig {
  return {
    config: c.config,
    excluded: c.excludedFrameIds.map(String),
    referenceFrameUuid: null,
    isDefault: c.isDefault,
    updatedAt: c.updatedAt,
  };
}

function fromProjectConfig(c: StackingProjectConfig): SubjectConfig {
  return {
    config: c.config,
    excluded: c.excludedFrameUuids,
    referenceFrameUuid: c.referenceFrameUuid,
    isDefault: c.isDefault,
    updatedAt: c.updatedAt,
  };
}

// The keys of a frame set are `String(frameId)`. Anything else would reach the backend as `null` and fail there
// with a message that names no key, so it is refused here.
function toFrameIds(keys: string[]): number[] {
  return keys.map((key) => {
    if (!/^\d+$/.test(key)) {
      const message = `an excluded frame key is not a frame id: ${key}`;
      console.error(`[useStackingSubjectApi] ${message}`);
      throw new Error(message);
    }
    return Number(key);
  });
}

function frameSetApi(setId: number): StackingSubjectApi {
  return {
    getPlan: (config) => call<StackingPlan>('get_stacking_plan', { setId, config }),
    getConfig: async () => fromSetConfig(await call<StackingSetConfig>('get_stacking_config', { setId })),
    setConfig: async (c) => {
      await call<void>('set_stacking_config', {
        setId,
        config: c.config,
        excludedFrameIds: toFrameIds(c.excluded),
      });
      // The command returns nothing (the tab submits its state and never re-reads it): hand back what was
      // stored. A row exists now, so `isDefault` is false; `updatedAt` is the submitted one, not the new stamp.
      return { ...c, referenceFrameUuid: null, isDefault: false };
    },
    getRuns: (limit) => call<StackingRunSummary[]>('get_stacking_runs', { setId, limit }),
    getWorkUsage: () => call<WorkUsage>('get_stacking_work_usage', { setId }),
    cleanup: (what) => call<number>('cleanup_stacking_work', { setId, what }),
  };
}

function projectApi(projectId: string): StackingSubjectApi {
  return {
    getPlan: (config) => call<StackingPlan>('get_project_stacking_plan', { projectId, config }),
    getConfig: async () =>
      fromProjectConfig(await call<StackingProjectConfig>('get_project_stacking_config', { projectId })),
    // `referenceFrameUuid` is always sent: the command cannot tell an absent field from `null`, and either
    // clears the manual reference.
    setConfig: async (c) =>
      fromProjectConfig(
        await call<StackingProjectConfig>('set_project_stacking_config', {
          projectId,
          config: c.config,
          excludedFrameUuids: c.excluded,
          referenceFrameUuid: c.referenceFrameUuid,
        }),
      ),
    getRuns: (limit) => call<StackingRunSummary[]>('get_project_stacking_runs', { projectId, limit }),
    getWorkUsage: () => call<WorkUsage>('get_project_stacking_work_usage', { projectId }),
    cleanup: (what) => call<number>('cleanup_project_stacking_work', { projectId, what }),
  };
}

/** The stacking commands of one subject. Stable per subject key: a frame-set subject's `lightFrames` is a new
 *  array on every catalog refresh and must not rebuild the adapter (nor re-run an effect that depends on it). */
export function useStackingSubjectApi(subject: StackSubject): StackingSubjectApi {
  const key = subjectKey(subject);
  // The key fixes the id, so the `subject` captured by the render that first saw this key stays correct.
  // eslint-disable-next-line react-hooks/exhaustive-deps
  return useMemo(() => (subject.kind === 'frameSet' ? frameSetApi(subject.framesSetId) : projectApi(subject.projectId)), [key]);
}
