import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { renderHook } from '@testing-library/react';
import { useStackingSubjectApi, type StackingSubjectApi, type SubjectConfig } from './useStackingSubjectApi';
import { api } from '../../api';
import type { CleanupWhat, StackingConfig } from '../../types/stacking';
import type { StackSubject } from './subject';

vi.mock('../../api', () => ({ api: { invoke: vi.fn() } }));

const cfg = { version: 1 } as unknown as StackingConfig;
const frameSet = (framesSetId: number): StackSubject => ({ kind: 'frameSet', framesSetId, lightFrames: [] });
const project = (projectId: string): StackSubject => ({ kind: 'project', projectId, title: 'T' });

function mount(subject: StackSubject) {
  return renderHook(({ s }) => useStackingSubjectApi(s), { initialProps: { s: subject } });
}

const subjectConfig = (over: Partial<SubjectConfig> = {}): SubjectConfig => ({
  config: cfg, excluded: [], referenceFrameUuid: null, isDefault: false, updatedAt: null, ...over,
});

describe('useStackingSubjectApi', () => {
  beforeEach(() => {
    vi.mocked(api.invoke).mockReset();
  });
  afterEach(() => vi.restoreAllMocks());

  // One row per call: how each subject kind reaches the backend. Each case resolves a distinctive value so a call
  // that returns something other than the command's own result fails.
  type Row = {
    name: string;
    call: (a: StackingSubjectApi) => Promise<unknown>;
    reply: unknown;
    set: [string, Record<string, unknown>];
    project: [string, Record<string, unknown>];
  };
  const rows: Row[] = [
    {
      name: 'getPlan with a config',
      call: (a) => a.getPlan(cfg),
      reply: { setName: 'plan' },
      set: ['get_stacking_plan', { setId: 7, config: cfg }],
      project: ['get_project_stacking_plan', { projectId: 'p1', config: cfg }],
    },
    {
      name: 'getPlan without a config',
      call: (a) => a.getPlan(),
      reply: { setName: 'plan' },
      set: ['get_stacking_plan', { setId: 7, config: undefined }],
      project: ['get_project_stacking_plan', { projectId: 'p1', config: undefined }],
    },
    {
      name: 'getRuns',
      call: (a) => a.getRuns(20),
      reply: [{ groupCount: 1 }],
      set: ['get_stacking_runs', { setId: 7, limit: 20 }],
      project: ['get_project_stacking_runs', { projectId: 'p1', limit: 20 }],
    },
    {
      name: 'getWorkUsage',
      call: (a) => a.getWorkUsage(),
      reply: { totalBytes: 99 },
      set: ['get_stacking_work_usage', { setId: 7 }],
      project: ['get_project_stacking_work_usage', { projectId: 'p1' }],
    },
    {
      name: 'cleanup',
      call: (a) => a.cleanup('intermediates' satisfies CleanupWhat),
      reply: 1234,
      set: ['cleanup_stacking_work', { setId: 7, what: 'intermediates' }],
      project: ['cleanup_project_stacking_work', { projectId: 'p1', what: 'intermediates' }],
    },
  ];

  it.each(rows)('$name goes to the frame-set command with the set id', async ({ call, reply, set }) => {
    vi.mocked(api.invoke).mockResolvedValue(reply);
    const { result } = mount(frameSet(7));
    await expect(call(result.current)).resolves.toBe(reply);
    expect(api.invoke).toHaveBeenCalledTimes(1);
    expect(api.invoke).toHaveBeenCalledWith(set[0], set[1]);
  });

  it.each(rows)('$name goes to the project command with the project id', async ({ call, reply, project: want }) => {
    vi.mocked(api.invoke).mockResolvedValue(reply);
    const { result } = mount(project('p1'));
    await expect(call(result.current)).resolves.toBe(reply);
    expect(api.invoke).toHaveBeenCalledTimes(1);
    expect(api.invoke).toHaveBeenCalledWith(want[0], want[1]);
  });

  it('reads a frame set config and turns its excluded frame ids into string keys', async () => {
    const updatedAt = '2026-11-05T10:00:00Z';
    vi.mocked(api.invoke).mockResolvedValue({ config: cfg, excludedFrameIds: [3, 9], isDefault: false, updatedAt });
    const { result } = mount(frameSet(7));

    const got = await result.current.getConfig();
    expect(api.invoke).toHaveBeenCalledWith('get_stacking_config', { setId: 7 });
    expect(got).toEqual({ config: cfg, excluded: ['3', '9'], referenceFrameUuid: null, isDefault: false, updatedAt });
  });

  it('writes a frame set config with the keys back as numbers and never sends a reference frame', async () => {
    vi.mocked(api.invoke).mockResolvedValue(undefined);
    const { result } = mount(frameSet(7));

    const got = await result.current.setConfig(
      subjectConfig({ excluded: ['3', '9'], referenceFrameUuid: 'ignored-for-a-set', isDefault: true }),
    );
    expect(api.invoke).toHaveBeenCalledTimes(1);
    const [command, args] = vi.mocked(api.invoke).mock.calls[0]!;
    expect(command).toBe('set_stacking_config');
    expect(args).toEqual({ setId: 7, config: cfg, excludedFrameIds: [3, 9] });
    expect(Object.keys(args as object).sort()).toEqual(['config', 'excludedFrameIds', 'setId']);
    // The set command returns nothing: the caller gets what it submitted, now stored, with no reference frame.
    expect(got).toEqual({ config: cfg, excluded: ['3', '9'], referenceFrameUuid: null, isDefault: false, updatedAt: null });
  });

  it('refuses and logs an excluded key that is not a frame id, without calling the backend', async () => {
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    const { result } = mount(frameSet(7));

    await expect(result.current.setConfig(subjectConfig({ excluded: ['3', 'uuid-1'] }))).rejects.toThrow(/uuid-1/);
    expect(api.invoke).not.toHaveBeenCalled();
    expect(err).toHaveBeenCalledTimes(1);
    expect(err.mock.calls[0]![0]).toContain('[useStackingSubjectApi]');
  });

  it('round-trips a project config: the frame uuids and the reference frame', async () => {
    const updatedAt = '2026-11-05T11:00:00Z';
    const stored = { config: cfg, excludedFrameUuids: ['u1', 'u2'], referenceFrameUuid: 'u3', isDefault: false, updatedAt };
    vi.mocked(api.invoke).mockResolvedValue(stored);
    const { result } = mount(project('p1'));

    const read = await result.current.getConfig();
    expect(api.invoke).toHaveBeenLastCalledWith('get_project_stacking_config', { projectId: 'p1' });
    expect(read).toEqual({ config: cfg, excluded: ['u1', 'u2'], referenceFrameUuid: 'u3', isDefault: false, updatedAt });

    const written = await result.current.setConfig(read);
    expect(api.invoke).toHaveBeenLastCalledWith('set_project_stacking_config', {
      projectId: 'p1', config: cfg, excludedFrameUuids: ['u1', 'u2'], referenceFrameUuid: 'u3',
    });
    expect(written).toEqual(read);
  });

  it('sends a null reference frame explicitly so a project write clears the manual reference', async () => {
    vi.mocked(api.invoke).mockResolvedValue({
      config: cfg, excludedFrameUuids: [], referenceFrameUuid: null, isDefault: false, updatedAt: null,
    });
    const { result } = mount(project('p1'));

    await result.current.setConfig(subjectConfig());
    const args = vi.mocked(api.invoke).mock.calls[0]![1] as Record<string, unknown>;
    expect(args).toHaveProperty('referenceFrameUuid', null);
    expect(args).toHaveProperty('excludedFrameUuids', []);
  });

  it('logs a rejected call with the adapter tag and the command, and rethrows it', async () => {
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    vi.mocked(api.invoke).mockRejectedValue('boom');
    const set = mount(frameSet(7)).result.current;
    const proj = mount(project('p1')).result.current;

    await expect(set.getRuns(5)).rejects.toBe('boom');
    expect(err).toHaveBeenLastCalledWith('[useStackingSubjectApi] get_stacking_runs failed:', 'boom');
    await expect(proj.getRuns(5)).rejects.toBe('boom');
    expect(err).toHaveBeenLastCalledWith('[useStackingSubjectApi] get_project_stacking_runs failed:', 'boom');
    await expect(proj.setConfig(subjectConfig())).rejects.toBe('boom');
    expect(err).toHaveBeenLastCalledWith('[useStackingSubjectApi] set_project_stacking_config failed:', 'boom');
    await expect(set.getConfig()).rejects.toBe('boom');
    expect(err).toHaveBeenLastCalledWith('[useStackingSubjectApi] get_stacking_config failed:', 'boom');
    expect(err).toHaveBeenCalledTimes(4);
  });

  it('is the same object while the subject key holds, and a new one when it changes', async () => {
    vi.mocked(api.invoke).mockResolvedValue([]);
    const { result, rerender } = mount(frameSet(7));
    const first = result.current;

    // A frame set's light frames are a new array on every catalog refresh; that must not rebuild the adapter.
    rerender({ s: { kind: 'frameSet', framesSetId: 7, lightFrames: [] } });
    expect(result.current).toBe(first);

    rerender({ s: frameSet(8) });
    expect(result.current).not.toBe(first);
    await result.current.getRuns(1);
    expect(api.invoke).toHaveBeenLastCalledWith('get_stacking_runs', { setId: 8, limit: 1 });

    rerender({ s: project('p1') });
    await result.current.getRuns(1);
    expect(api.invoke).toHaveBeenLastCalledWith('get_project_stacking_runs', { projectId: 'p1', limit: 1 });
  });
});
