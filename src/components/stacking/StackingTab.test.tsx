// Wave 6c Task 4: pins what the Stacking tab does for a frame set at the `api` boundary — the commands it sends,
// their arguments and their order — so the move to a stacking subject cannot change it. Rendered through the real
// `StackingProvider` and the real Results panel and Frames table; the board, the inspector and the groups table are
// replaced by stubs (they read a full `StackingConfig` and are not what is pinned here).
//
// Fake timers: the write lands after the tab's 500 ms debounce and the plan refresh after its 300 ms one, so the
// clock is advanced explicitly (`src/test/fakeClock`) instead of racing a real-timer `waitFor`.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen, within } from '@testing-library/react';
import { StrictMode, type ReactElement } from 'react';
import { MemoryRouter } from 'react-router-dom';
import { advance, PAST_THE_DEBOUNCE_MS } from '../../test/fakeClock';
import { StackingProvider } from '../../contexts/StackingContext';
import { StackingTab } from './StackingTab';
import type { LightFrameRef } from './FramesTable';
import { api } from '../../api';
import type { StackingConfig, StackingPlan } from '../../types/stacking';

vi.mock('../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));

vi.mock('./PipelineBoard', () => ({
  PipelineBoard: (p: {
    progress?: { runId: number };
    outcome?: { runId: number };
    onToggleDrizzle: (checked: boolean) => void;
  }) => (
    <div data-testid="board" data-run={p.progress?.runId ?? ''} data-outcome={p.outcome?.runId ?? ''}>
      <button type="button" onClick={() => p.onToggleDrizzle(true)}>edit config</button>
    </div>
  ),
}));
vi.mock('./StageInspector', () => ({ StageInspector: () => <div data-testid="inspector" /> }));
// The Groups table is a stub that shows what the tab hands it and patches the grouping when asked.
vi.mock('./GroupsTable', () => ({
  GroupsTable: (p: {
    grouping: { exposureMode: string };
    disabled?: boolean;
    toleranceInCalibrationHash: boolean;
    onPatchGrouping: (patch: { exposureMode: string }) => void;
  }) => (
    <div data-testid="groups" data-hint={String(p.toleranceInCalibrationHash)} data-mode={p.grouping.exposureMode} data-disabled={String(!!p.disabled)}>
      <button type="button" onClick={() => p.onPatchGrouping({ exposureMode: 'stops' })}>group by stops</button>
    </div>
  ),
}));

const CONFIG = {
  version: 1,
  paths: { working: null, output: null },
  grouping: { exposureToleranceSec: 2, exposureMode: 'seconds', stopsWidth: 1 },
  reference: { mode: 'manual', twoPass: false },
  drizzle: { enabled: false },
  normalization: { local: { enabled: false }, rejection: 'scaleZeroOffset' },
} as unknown as StackingConfig;

const plan = (over: Record<string, unknown> = {}): StackingPlan =>
  ({
    setId: 7, projectId: null, setName: 'M31', config: CONFIG, groups: [], blockers: [], warnings: [],
    staleStages: [], activeRunId: null, freeBytes: 5e9, estimateBytes: 1e9, workingDir: '/work', outputDir: '/out',
    readiness: { rawSetsWithoutMaster: 0, rawSetIdsWithoutMaster: [] }, frameCount: 2, includedCount: 2,
    excludedFrameIds: [], projectFrames: [], ...over,
  }) as unknown as StackingPlan;

const USAGE = {
  totalBytes: 0, calibratedBytes: 0, registeredBytes: 0, lnBytes: 0, runsBytes: 0, rejBytes: 0, previewsBytes: 0,
};

const RUN = {
  run: { id: 5, status: 'done', startedAt: '2026-11-01T10:00:00Z', finishedAt: null, error: null },
  groupCount: 0,
  masterPaths: [],
};

const FRAMES: LightFrameRef[] = [
  { frameId: 101, filename: 'a.fit' },
  { frameId: 102, filename: 'b.fit' },
];

const PROJECT_CONFIG = {
  config: CONFIG, excludedFrameUuids: ['u-ex'], referenceFrameUuid: 'u-ref', isDefault: false, updatedAt: null,
};

/** What the backend answers, by command name. A command nobody listed resolves to `undefined`. */
function answer(command: string, args: Record<string, unknown> = {}): Promise<unknown> {
  switch (command) {
    case 'get_stacking_config':
      return Promise.resolve({ config: CONFIG, excludedFrameIds: [], isDefault: true, updatedAt: null });
    case 'get_stacking_presets':
      return Promise.resolve({ default: CONFIG, fastPreview: CONFIG, maximumQuality: CONFIG });
    case 'list_stacking_presets':
      return Promise.resolve([]);
    case 'get_stacking_plan':
      return Promise.resolve(plan({ setId: args.setId, workingDir: `/work-${args.setId}` }));
    case 'get_stacking_runs':
      return Promise.resolve([RUN]);
    case 'get_stacking_run':
      return Promise.resolve({ run: RUN.run, groups: [], frames: [], summary: null });
    case 'get_stacking_work_usage':
      return Promise.resolve(USAGE);
    case 'estimate_zip_extraction':
      return Promise.resolve({ files: 0, bytes: 0 });
    case 'start_stacking':
      return Promise.resolve({ runId: 11 });
    case 'cleanup_stacking_work':
      return Promise.resolve(1234);
    case 'get_project_stacking_config':
      return Promise.resolve(PROJECT_CONFIG);
    case 'get_project_stacking_plan':
      return Promise.resolve(plan({ setId: null, projectId: args.projectId, workingDir: `/work-${args.projectId}` }));
    case 'get_project_stacking_runs':
      return Promise.resolve([RUN]);
    case 'get_project_stacking_work_usage':
      return Promise.resolve(USAGE);
    case 'set_project_stacking_config':
      return Promise.resolve({ ...PROJECT_CONFIG, config: args.config, excludedFrameUuids: args.excludedFrameUuids });
    case 'start_project_stacking':
      return Promise.resolve({ runId: 12 });
    case 'cleanup_project_stacking_work':
      return Promise.resolve(99);
    default:
      return Promise.resolve(undefined);
  }
}

const handlers: Record<string, (payload: unknown) => void> = {};

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((command: string, args?: Record<string, unknown>) =>
    answer(command, args)) as never);
  vi.mocked(api.listen).mockReset();
  vi.mocked(api.listen).mockImplementation(((event: string, cb: (payload: unknown) => void) => {
    handlers[event] = cb;
    return Promise.resolve(() => {});
  }) as never);
  notifyMock.mockClear();
  try { localStorage.clear(); } catch { /* the tab reads its prefs defensively too */ }
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

/** One Stacking tab for a frame set. */
const frameSetTab = (framesSetId: number) => (
  <StackingTab subject={{ kind: 'frameSet', framesSetId, lightFrames: FRAMES }} />
);

/** One Stacking tab for a collaboration project. */
const projectTab = (projectId: string) => (
  <StackingTab subject={{ kind: 'project', projectId, title: 'Project' }} />
);

const tree = (tab: ReactElement) => (
  <MemoryRouter>
    <StackingProvider>{tab}</StackingProvider>
  </MemoryRouter>
);

/** Renders the tab and settles its mount loads: config, presets, plan, then the Results panel's own. */
async function mount(tab: ReactElement) {
  const view = render(tree(tab));
  for (let i = 0; i < 4; i++) await advance(0);
  return view;
}

/** The arguments of every call of `command`, in order. */
const argsOf = (command: string) =>
  vi.mocked(api.invoke).mock.calls.filter(([c]) => c === command).map(([, args]) => args);

describe('StackingTab for a frame set', () => {
  it('loads the stored config, then the presets, then the plan built from that config', async () => {
    await mount(frameSetTab(7));

    const order = vi.mocked(api.invoke).mock.calls.map(([c]) => c);
    expect(order.slice(0, 4)).toEqual([
      'get_stacking_config', 'get_stacking_presets', 'list_stacking_presets', 'get_stacking_plan',
    ]);
    expect(argsOf('get_stacking_config')[0]).toEqual({ setId: 7 });
    // The plan is built from the stored config, never from `undefined`.
    expect(argsOf('get_stacking_plan')[0]).toEqual({ setId: 7, config: CONFIG });
    expect(screen.getByRole('button', { name: /Run stacking/ })).toBeEnabled();
  });

  it('toggling a frame writes the exclusion after the debounce, then re-plans', async () => {
    await mount(frameSetTab(7));
    await advance(PAST_THE_DEBOUNCE_MS); // the 300 ms plan refresh the first draft schedules
    const plansBefore = argsOf('get_stacking_plan').length;

    fireEvent.click(screen.getByRole('checkbox', { name: 'Include b.fit in the next run' }));
    await advance(400);
    expect(argsOf('set_stacking_config')).toEqual([]);

    await advance(PAST_THE_DEBOUNCE_MS);
    expect(argsOf('set_stacking_config')).toEqual([{ setId: 7, config: CONFIG, excludedFrameIds: [102] }]);
    // The stored row decides what a plan includes, so an exclusion write is followed by one more plan.
    expect(argsOf('get_stacking_plan')).toHaveLength(plansBefore + 1);
    expect(argsOf('get_stacking_plan')[plansBefore]).toEqual({ setId: 7, config: CONFIG });
  });

  it('a config edit writes the whole document with the exclusions it loaded with', async () => {
    vi.mocked(api.invoke).mockImplementation(((command: string, args?: Record<string, unknown>) =>
      command === 'get_stacking_config'
        ? Promise.resolve({ config: CONFIG, excludedFrameIds: [101], isDefault: false, updatedAt: null })
        : answer(command, args)) as never);
    await mount(frameSetTab(7));

    fireEvent.click(screen.getByText('edit config'));
    await advance(PAST_THE_DEBOUNCE_MS);

    expect(argsOf('set_stacking_config')).toEqual([
      { setId: 7, config: { ...CONFIG, drizzle: { enabled: true } }, excludedFrameIds: [101] },
    ]);
  });

  it('a grouping change in the Groups table is written whole and the plan is built again with it', async () => {
    await mount(frameSetTab(7));
    await advance(PAST_THE_DEBOUNCE_MS);
    const groups = screen.getByTestId('groups');
    // An ordinary set is calibrated by the run: its tolerance is part of the calibration hash.
    expect(groups).toHaveAttribute('data-hint', 'true');
    expect(groups).toHaveAttribute('data-mode', 'seconds');
    const plansBefore = argsOf('get_stacking_plan').length;

    fireEvent.click(screen.getByText('group by stops'));
    expect(screen.getByTestId('groups')).toHaveAttribute('data-mode', 'stops');
    await advance(PAST_THE_DEBOUNCE_MS);

    const edited = { ...CONFIG, grouping: { exposureToleranceSec: 2, exposureMode: 'stops', stopsWidth: 1 } };
    expect(argsOf('set_stacking_config')).toEqual([{ setId: 7, config: edited, excludedFrameIds: [] }]);
    const plans = argsOf('get_stacking_plan');
    expect(plans.length).toBeGreaterThan(plansBefore);
    expect(plans[plans.length - 1]).toEqual({ setId: 7, config: edited });
  });

  // An attested set's stage-1 hash is the Prepare hash, which holds no grouping: the re-calibration hint would be false.
  it('the Groups table of an attested set is told the tolerance is not in the calibration hash', async () => {
    vi.mocked(api.invoke).mockImplementation(((command: string, args?: Record<string, unknown>) =>
      command === 'get_stacking_plan'
        ? Promise.resolve(plan({
            setId: args?.setId,
            readiness: { rawSetsWithoutMaster: 0, rawSetIdsWithoutMaster: [], attested: true },
          }))
        : answer(command, args)) as never);
    await mount(frameSetTab(7));
    expect(screen.getByTestId('groups')).toHaveAttribute('data-hint', 'false');
  });

  it('the Groups table is disabled while a run is in flight', async () => {
    await mount(frameSetTab(7));
    expect(screen.getByTestId('groups')).toHaveAttribute('data-disabled', 'false');

    act(() => handlers['stacking-progress'](progressEvent(7, null)));
    expect(screen.getByTestId('groups')).toHaveAttribute('data-disabled', 'true');
  });

  it('opening a set writes nothing', async () => {
    await mount(frameSetTab(7));
    await advance(PAST_THE_DEBOUNCE_MS);
    expect(argsOf('set_stacking_config')).toEqual([]);
  });

  it('a write still waiting when the set changes goes to the set it was made for', async () => {
    const view = await mount(frameSetTab(7));
    fireEvent.click(screen.getByText('edit config'));
    await advance(100); // inside the debounce

    view.rerender(tree(frameSetTab(8)));
    await advance(PAST_THE_DEBOUNCE_MS);

    expect(argsOf('set_stacking_config')).toEqual([
      { setId: 7, config: { ...CONFIG, drizzle: { enabled: true } }, excludedFrameIds: [] },
    ]);
  });

  it('Run starts the set with the draft config', async () => {
    await mount(frameSetTab(7));

    fireEvent.click(screen.getByRole('button', { name: /Run stacking/ }));
    await advance(0);

    expect(argsOf('start_stacking')).toEqual([{ setId: 7, config: CONFIG, rerunFrom: undefined }]);
  });

  it('the Results panel asks for the set’s run history, usage and the selected run', async () => {
    await mount(frameSetTab(7));

    expect(argsOf('get_stacking_runs')).toEqual([{ setId: 7, limit: 20 }]);
    expect(argsOf('get_stacking_work_usage')).toEqual([{ setId: 7 }]);
    expect(argsOf('get_stacking_run')).toEqual([{ runId: 5 }]);
    // The remembered selection of a set is stored under the set id alone.
    expect(localStorage.getItem('athenaeum.stacking.selectedRun.7')).toBe('5');
  });

  it('Delete intermediates cleans the set’s working folder', async () => {
    await mount(frameSetTab(7));

    fireEvent.click(screen.getByRole('button', { name: 'Delete intermediates' }));
    const buttons = screen.getAllByRole('button', { name: 'Delete intermediates' });
    await act(async () => { fireEvent.click(buttons[buttons.length - 1]); });
    await advance(0);

    expect(argsOf('cleanup_stacking_work')).toEqual([{ setId: 7, what: 'intermediates' }]);
  });

  it('shows the running state of its own set only', async () => {
    await mount(frameSetTab(7));
    const event = (setId: number | null, projectId: string | null) => ({
      runId: 3, setId, projectId, stage: 'measure', groupKey: null, current: 1, total: 2, percent: 50,
      bytesDone: 0, bytesTotal: 0, frameId: null, message: null,
    });

    act(() => handlers['stacking-progress'](event(8, null)));
    expect(screen.queryByRole('button', { name: /Cancel/ })).toBeNull();
    expect(screen.getByTestId('board').dataset.run).toBe('');

    act(() => handlers['stacking-progress'](event(7, null)));
    expect(screen.getByRole('button', { name: /Cancel/ })).toBeInTheDocument();
    expect(screen.getByTestId('board').dataset.run).toBe('3');
  });
});

/** A progress / completion event of the backend, for the set or the project it names. */
const progressEvent = (setId: number | null, projectId: string | null, runId = 3) => ({
  runId, setId, projectId, stage: 'measure', groupKey: null, current: 1, total: 2, percent: 50,
  bytesDone: 0, bytesTotal: 0, frameId: null, message: null,
});
const completeEvent = (setId: number | null, projectId: string | null, runId = 3) => ({
  runId, setId, projectId, success: false, cancelled: false, error: 'boom', warnings: [], masters: [],
});

const SET_COMMANDS = [
  'get_stacking_config', 'get_stacking_plan', 'get_stacking_runs', 'get_stacking_work_usage',
  'set_stacking_config', 'start_stacking', 'cleanup_stacking_work', 'estimate_zip_extraction',
];

/** A project's plan rows: usable with a WCS, usable without one, and not on this device. */
const PROJECT_FRAMES = [
  {
    handle: 1, frameUuid: 'u1', fileName: 'a.fits', publisher: 'Alice', filterCanonical: 'L', exptimeSec: 300,
    colorMode: 'mono', hasWcs: true, onDisk: true, reason: null,
  },
  {
    handle: 2, frameUuid: 'u2', fileName: 'b.fits', publisher: 'Bob', filterCanonical: 'L', exptimeSec: 300,
    colorMode: 'mono', hasWcs: false, onDisk: true, reason: null,
  },
  {
    handle: null, frameUuid: 'u3', fileName: 'c.fits', publisher: 'Bob', filterCanonical: 'L', exptimeSec: 300,
    colorMode: 'mono', hasWcs: true, onDisk: false, reason: 'not on this device',
  },
];

/** Answers a project's stored document, plan and selected run as given: the exclusion keys and the pin it holds, the
 *  reference mode of its config, the frames of the selected run and the active run of its plan. Install it before
 *  `mount`. */
function projectWith(o: {
  excluded?: string[];
  reference?: string | null;
  mode?: 'auto' | 'manual';
  runFrames?: unknown[];
  activeRunId?: number | null;
  /** `PlanGrid` of the plan (absent: the plan carries none). */
  grid?: unknown;
  /** The plan's blockers (absent: none). */
  blockers?: { code: string; message: string }[];
  /** The plan's `projectFrames` (absent: `PROJECT_FRAMES`). */
  projectFrames?: unknown[];
}) {
  vi.mocked(api.invoke).mockImplementation(((command: string, args?: Record<string, unknown>) => {
    switch (command) {
      case 'get_project_stacking_config':
        return Promise.resolve({
          ...PROJECT_CONFIG,
          config: { ...CONFIG, reference: { mode: o.mode ?? 'manual', twoPass: false } },
          excludedFrameUuids: o.excluded ?? [],
          referenceFrameUuid: o.reference ?? null,
        });
      case 'get_project_stacking_plan':
        return Promise.resolve(
          plan({
            setId: null, projectId: args?.projectId, projectFrames: o.projectFrames ?? PROJECT_FRAMES,
            blockers: o.blockers ?? [], activeRunId: o.activeRunId ?? null,
            ...(o.grid === undefined ? {} : { grid: o.grid }),
          }),
        );
      case 'get_stacking_run':
        return Promise.resolve({ run: RUN.run, groups: [], summary: null, frames: o.runFrames ?? [] });
      default:
        return answer(command, args);
    }
  }) as never);
}

describe('StackingTab for a project', () => {
  it('a project subject reads and writes through the project commands', async () => {
    await mount(projectTab('p1'));

    expect(argsOf('get_project_stacking_config')).toEqual([{ projectId: 'p1' }]);
    expect(argsOf('get_project_stacking_plan')[0]).toEqual({ projectId: 'p1', config: CONFIG });
    expect(argsOf('get_project_stacking_runs')).toEqual([{ projectId: 'p1', limit: 20 }]);
    expect(argsOf('get_project_stacking_work_usage')).toEqual([{ projectId: 'p1' }]);
    // The remembered run of a project is stored under its own key, apart from every set's.
    expect(argsOf('get_stacking_run')).toEqual([{ runId: 5 }]);
    expect(localStorage.getItem('athenaeum.stacking.selectedRun.project:p1')).toBe('5');

    // A config edit is written whole, with the exclusions and the pinned reference the project was loaded with.
    const edited = { ...CONFIG, drizzle: { enabled: true } };
    fireEvent.click(screen.getByText('edit config'));
    await advance(PAST_THE_DEBOUNCE_MS);
    expect(argsOf('set_project_stacking_config')).toEqual([
      { projectId: 'p1', config: edited, excludedFrameUuids: ['u-ex'], referenceFrameUuid: 'u-ref' },
    ]);

    fireEvent.click(screen.getByRole('button', { name: /Run stacking/ }));
    await advance(0);
    expect(argsOf('start_project_stacking')).toEqual([{ projectId: 'p1', config: edited, rerunFrom: undefined }]);

    fireEvent.click(screen.getByRole('button', { name: 'Delete intermediates' }));
    expect(screen.getByText(/^Removes this project's registered/)).toBeInTheDocument();
    const buttons = screen.getAllByRole('button', { name: 'Delete intermediates' });
    await act(async () => { fireEvent.click(buttons[buttons.length - 1]); });
    await advance(0);
    expect(argsOf('cleanup_project_stacking_work')).toEqual([{ projectId: 'p1', what: 'intermediates' }]);

    // No frame-set command was sent, and no zip estimate (a project has no light-frame list to ask about).
    for (const command of SET_COMMANDS) expect(argsOf(command), command).toEqual([]);
  });

  it('progress of another subject is not shown', async () => {
    await mount(projectTab('p1'));
    expect(screen.getByRole('button', { name: /Run stacking/ })).toBeEnabled();

    act(() => handlers['stacking-progress'](progressEvent(null, 'p2')));
    act(() => handlers['stacking-progress'](progressEvent(7, null, 4)));
    act(() => handlers['stacking-complete'](completeEvent(null, 'p2')));
    act(() => handlers['stacking-complete'](completeEvent(7, null, 4)));
    expect(screen.queryByRole('button', { name: /Cancel/ })).toBeNull();
    expect(screen.getByRole('button', { name: /Run stacking/ })).toBeEnabled();
    expect(screen.getByTestId('board').dataset.run).toBe('');
    expect(screen.getByTestId('board').dataset.outcome).toBe('');

    // The same events for the project on screen do show (the check above is not vacuous).
    act(() => handlers['stacking-progress'](progressEvent(null, 'p1', 5)));
    expect(screen.getByRole('button', { name: /Cancel/ })).toBeInTheDocument();
    expect(screen.getByTestId('board').dataset.run).toBe('5');
    act(() => handlers['stacking-complete'](completeEvent(null, 'p1', 5)));
    expect(screen.getByTestId('board').dataset.outcome).toBe('5');
  });

  it('a frame set tab shows no progress of a project', async () => {
    await mount(frameSetTab(7));

    act(() => handlers['stacking-progress'](progressEvent(null, 'p1')));
    expect(screen.queryByRole('button', { name: /Cancel/ })).toBeNull();
    expect(screen.getByTestId('board').dataset.run).toBe('');
  });

  it('a pending write goes to the subject it was made for', async () => {
    const view = await mount(projectTab('p1'));
    fireEvent.click(screen.getByText('edit config'));
    await advance(100); // inside the debounce

    view.rerender(tree(frameSetTab(8)));
    await advance(PAST_THE_DEBOUNCE_MS);

    expect(argsOf('set_project_stacking_config')).toEqual([
      {
        projectId: 'p1', config: { ...CONFIG, drizzle: { enabled: true } },
        excludedFrameUuids: ['u-ex'], referenceFrameUuid: 'u-ref',
      },
    ]);
    expect(argsOf('set_stacking_config')).toEqual([]);
    // The set that took the tab over loaded as itself and was not written.
    expect(argsOf('get_stacking_config')).toEqual([{ setId: 8 }]);
  });

  it('a pending write of a set is not sent to the project that takes the tab over', async () => {
    const view = await mount(frameSetTab(7));
    fireEvent.click(screen.getByRole('checkbox', { name: 'Include a.fit in the next run' }));
    await advance(100);

    view.rerender(tree(projectTab('p1')));
    await advance(PAST_THE_DEBOUNCE_MS);

    expect(argsOf('set_stacking_config')).toEqual([{ setId: 7, config: CONFIG, excludedFrameIds: [101] }]);
    expect(argsOf('set_project_stacking_config')).toEqual([]);
  });

  it('a slow write of the old subject does not stop the new subject\u2019s plan from landing', async () => {
    let releaseWrite = () => {};
    let releasePlan = () => {};
    vi.mocked(api.invoke).mockImplementation(((command: string, args?: Record<string, unknown>) => {
      if (command === 'set_stacking_config') {
        return new Promise((resolve) => { releaseWrite = () => resolve(undefined); });
      }
      if (command === 'get_project_stacking_plan') {
        return new Promise((resolve) => {
          releasePlan = () => resolve(plan({ setId: null, projectId: 'p1', workingDir: '/work-p1' }));
        });
      }
      return answer(command, args);
    }) as never);
    const view = await mount(frameSetTab(7));
    await advance(PAST_THE_DEBOUNCE_MS); // the 300 ms plan refresh the first draft schedules
    fireEvent.click(screen.getByRole('checkbox', { name: 'Include a.fit in the next run' }));
    await advance(100);

    view.rerender(tree(projectTab('p1')));
    await advance(0);
    await advance(0);
    const plansForSet = argsOf('get_stacking_plan').length;
    // The set's exclusion write finishes while the project's own plan is still on its way: it must not re-plan
    // the set (that would supersede the project's plan fetch and leave the tab without a plan).
    releaseWrite();
    await advance(0);
    releasePlan();
    await advance(0);
    await advance(0);

    expect(screen.getByText('/work-p1')).toBeInTheDocument();
    expect(argsOf('get_stacking_plan')).toHaveLength(plansForSet);
  });

  it('the Groups table of a project patches the project\u2019s own config', async () => {
    await mount(projectTab('p1'));
    await advance(PAST_THE_DEBOUNCE_MS);
    expect(screen.getByTestId('groups')).toHaveAttribute('data-hint', 'false');

    fireEvent.click(screen.getByText('group by stops'));
    await advance(PAST_THE_DEBOUNCE_MS);

    const edited = { ...CONFIG, grouping: { exposureToleranceSec: 2, exposureMode: 'stops', stopsWidth: 1 } };
    expect(argsOf('set_project_stacking_config')).toEqual([
      { projectId: 'p1', config: edited, excludedFrameUuids: ['u-ex'], referenceFrameUuid: 'u-ref' },
    ]);
    expect(argsOf('set_stacking_config')).toEqual([]);
  });

  it('a project run row whose frame left the plan renders from the run, with no toggle and no pin', async () => {
    projectWith({
      runFrames: [{
        id: 1, runId: 5, groupId: 1, frameId: 41, included: true, exclusionReason: null, weight: 1,
        regStatus: null, regModel: null, regRmsPx: null, regInliers: null, frameUuid: 'u41',
      }],
    });
    await mount(projectTab('p1'));

    // The plan's frames have both controls; the run's own row has neither, and is named by its handle.
    const orphan = screen.getByText('frame 41').closest('tr');
    expect(orphan).not.toBeNull();
    expect(within(orphan as HTMLElement).queryByRole('checkbox')).toBeNull();
    expect(within(orphan as HTMLElement).queryByRole('button')).toBeNull();
    expect(screen.getByRole('checkbox', { name: 'Include a.fits in the next run' })).toBeEnabled();
    await advance(PAST_THE_DEBOUNCE_MS);
    expect(argsOf('set_project_stacking_config')).toEqual([]);
  });

  describe('the Frames table', () => {
    const MANUAL = { ...CONFIG, reference: { mode: 'manual', twoPass: false } };
    const AUTO = { ...CONFIG, reference: { mode: 'auto', twoPass: false } };

    it('lists the plan\u2019s frames by publisher, with the exclusion and the pin the project loaded with', async () => {
      projectWith({ excluded: ['u2'], reference: 'u1' });
      await mount(projectTab('p1'));

      expect(screen.getByRole('button', { name: /^Frames \(3\)/ })).toBeInTheDocument();
      expect(screen.getByText('Alice')).toBeInTheDocument();
      expect(screen.getByRole('checkbox', { name: 'Include a.fits in the next run' })).toBeChecked();
      expect(screen.getByRole('checkbox', { name: 'Include b.fits in the next run' })).not.toBeChecked();
      expect(screen.getByRole('button', { name: 'Clear the reference frame a.fits' })).toHaveAttribute('aria-pressed', 'true');
      expect(screen.getByRole('button', { name: 'Use b.fits as the reference frame' })).toBeEnabled();
      expect(screen.getByRole('button', { name: 'Use c.fits as the reference frame' })).toBeDisabled();
    });

    it('unchecking a frame writes its uuid with the pin kept, then re-plans', async () => {
      projectWith({ excluded: ['u2'], reference: 'u1' });
      await mount(projectTab('p1'));
      await advance(PAST_THE_DEBOUNCE_MS);
      const plansBefore = argsOf('get_project_stacking_plan').length;

      fireEvent.click(screen.getByRole('checkbox', { name: 'Include a.fits in the next run' }));
      await advance(PAST_THE_DEBOUNCE_MS);

      expect(argsOf('set_project_stacking_config')).toEqual([
        { projectId: 'p1', config: MANUAL, excludedFrameUuids: ['u2', 'u1'], referenceFrameUuid: 'u1' },
      ]);
      expect(argsOf('get_project_stacking_plan')).toHaveLength(plansBefore + 1);
      // A frame set's key is its frame id; none of that reaches a project.
      expect(argsOf('set_stacking_config')).toEqual([]);
    });

    it('pinning a frame writes Manual and its uuid, then re-plans so the plan reads the stored pin', async () => {
      projectWith({ mode: 'auto' });
      await mount(projectTab('p1'));
      await advance(PAST_THE_DEBOUNCE_MS);

      fireEvent.click(screen.getByRole('button', { name: 'Use b.fits as the reference frame' }));
      await advance(PAST_THE_DEBOUNCE_MS);

      expect(argsOf('set_project_stacking_config')).toEqual([
        { projectId: 'p1', config: MANUAL, excludedFrameUuids: [], referenceFrameUuid: 'u2' },
      ]);
      const commands = vi.mocked(api.invoke).mock.calls.map(([c]) => c);
      expect(commands.lastIndexOf('get_project_stacking_plan')).toBeGreaterThan(
        commands.indexOf('set_project_stacking_config'),
      );
      expect(screen.getByRole('button', { name: 'Clear the reference frame b.fits' })).toHaveAttribute('aria-pressed', 'true');
    });

    it('pinning the pinned frame clears the stored pin and returns to Auto', async () => {
      projectWith({ reference: 'u1' });
      await mount(projectTab('p1'));

      fireEvent.click(screen.getByRole('button', { name: 'Clear the reference frame a.fits' }));
      await advance(PAST_THE_DEBOUNCE_MS);

      expect(argsOf('set_project_stacking_config')).toEqual([
        { projectId: 'p1', config: AUTO, excludedFrameUuids: [], referenceFrameUuid: null },
      ]);
      expect(screen.getByRole('button', { name: 'Use a.fits as the reference frame' })).toHaveAttribute('aria-pressed', 'false');
    });

    it('moves the pin to another frame in one write', async () => {
      projectWith({ reference: 'u1' });
      await mount(projectTab('p1'));

      fireEvent.click(screen.getByRole('button', { name: 'Use b.fits as the reference frame' }));
      await advance(PAST_THE_DEBOUNCE_MS);

      expect(argsOf('set_project_stacking_config')).toEqual([
        { projectId: 'p1', config: MANUAL, excludedFrameUuids: [], referenceFrameUuid: 'u2' },
      ]);
    });

    it('does not show a uuid left stored under Auto as pinned, and the click pins it', async () => {
      projectWith({ mode: 'auto', reference: 'u1' });
      await mount(projectTab('p1'));
      expect(screen.getByRole('button', { name: 'Use a.fits as the reference frame' })).toHaveAttribute('aria-pressed', 'false');

      fireEvent.click(screen.getByRole('button', { name: 'Use a.fits as the reference frame' }));
      await advance(PAST_THE_DEBOUNCE_MS);

      expect(argsOf('set_project_stacking_config')).toEqual([
        { projectId: 'p1', config: MANUAL, excludedFrameUuids: [], referenceFrameUuid: 'u1' },
      ]);
    });

    it('disables the checkboxes and the pins while a run is active', async () => {
      projectWith({ activeRunId: 9 });
      await mount(projectTab('p1'));
      expect(screen.getByRole('checkbox', { name: 'Include a.fits in the next run' })).toBeDisabled();
      expect(screen.getByRole('button', { name: 'Use a.fits as the reference frame' })).toBeDisabled();
    });

    it('a frame set\u2019s table has no publisher and no pin', async () => {
      await mount(frameSetTab(7));
      expect(screen.queryByRole('button', { name: 'Publisher' })).toBeNull();
      expect(screen.queryByRole('columnheader', { name: 'Reference' })).toBeNull();
    });
  });

  describe('the grid card', () => {
    const GRID_PLAN = {
      state: 'proposed', current: null, framesInside: 12, canEdit: true,
      proposal: { scaleArcsec: 1.5, widthPx: 3200, heightPx: 2560, crval1: 83.82, crval2: -5.39, rotationDeg: 0 },
      target: { raDeg: 83.82, decDeg: -5.39, radiusDeg: 1 }, footprints: [], framesNotDrawn: 0,
    };

    it('sits above the board for a project', async () => {
      projectWith({ grid: GRID_PLAN });
      await mount(projectTab('p1'));
      const heading = screen.getByRole('heading', { name: 'Canonical grid \u2014 not confirmed' });
      const board = screen.getByTestId('board');
      expect(heading.compareDocumentPosition(board) & Node.DOCUMENT_POSITION_FOLLOWING).toBeTruthy();
    });

    it('is absent when the plan carries no grid, and for a frame set', async () => {
      projectWith({ grid: null });
      const view = await mount(projectTab('p1'));
      expect(screen.queryByText(/Canonical grid/)).toBeNull();
      view.unmount();
      await mount(frameSetTab(7));
      expect(screen.queryByText(/Canonical grid/)).toBeNull();
    });

    it('Use proposal saves the proposal for this project, then re-plans', async () => {
      projectWith({ grid: GRID_PLAN });
      await mount(projectTab('p1'));
      await advance(PAST_THE_DEBOUNCE_MS);
      const plansBefore = argsOf('get_project_stacking_plan').length;

      fireEvent.click(screen.getByRole('button', { name: 'Use proposal' }));
      await advance(0);
      await advance(0);

      expect(argsOf('set_collab_project_grid')).toEqual([{ projectId: 'p1', grid: GRID_PLAN.proposal }]);
      expect(argsOf('get_project_stacking_plan')).toHaveLength(plansBefore + 1);
    });
  });

  // Wave 6c Task 9: a project with nothing usable included on this device shows why, instead of a board of rows that
  // cannot run. It replaces the board only: the toolbar, the blocker line, the grid card, the Groups card, the
  // inspector and the Frames table keep their places.
  describe('the empty state', () => {
    const NO_FRAMES = { code: 'frames', message: 'No frame of this project is on this device yet' };
    const TOO_FEW = { code: 'frames', message: 'At least 3 included frames in one group' };
    /** The same three frames, none of them stackable here. */
    const ELSEWHERE = PROJECT_FRAMES.map((f) => ({ ...f, handle: null, onDisk: false, reason: 'not on this device' }));
    /** On this device, but the reader refuses them. */
    const UNSUPPORTED = PROJECT_FRAMES.map((f) => ({
      ...f, handle: null, onDisk: true, reason: 'unsupported file: tile-compressed FITS',
    }));

    it.each([
      ['the plan lists no frame at all', [] as unknown[]],
      ['no listed frame is on this device', ELSEWHERE],
      ['every frame on this device is unsupported', UNSUPPORTED],
    ])('says no frame is usable here when %s', async (_case, projectFrames) => {
      projectWith({ blockers: [NO_FRAMES], projectFrames });
      await mount(projectTab('p1'));

      expect(screen.getByText('Nothing to stack on this device')).toHaveClass('text-content');
      expect(screen.getByText('No frame of this project is usable on this device.')).toBeInTheDocument();
      expect(screen.queryByText('Every usable frame is excluded.')).toBeNull();
      // The board gives way to it; the blocker line stays, and Run stays refused.
      expect(screen.queryByTestId('board')).toBeNull();
      expect(screen.getByText(NO_FRAMES.message)).toBeInTheDocument();
      expect(screen.getByRole('button', { name: /Run stacking/ })).toBeDisabled();
    });

    it('replaces the board only: the Groups card and the inspector keep their places', async () => {
      projectWith({ blockers: [NO_FRAMES], projectFrames: ELSEWHERE });
      await mount(projectTab('p1'));

      expect(screen.getByText('Nothing to stack on this device')).toBeInTheDocument();
      expect(screen.queryByTestId('board')).toBeNull();
      expect(screen.getByRole('heading', { name: /^Groups \(/ })).toBeInTheDocument();
      expect(screen.getByTestId('groups')).toBeInTheDocument();
      expect(screen.getByTestId('inspector')).toBeInTheDocument();
    });

    it('is not shown while usable frames are included, however few: the board and the Groups card stay', async () => {
      // Two usable, included frames: the `frames` blocker stands (no group of three), the reason is the red line.
      projectWith({ blockers: [TOO_FEW] });
      await mount(projectTab('p1'));

      expect(screen.queryByText('Nothing to stack on this device')).toBeNull();
      expect(screen.queryByText(/excluded\./)).toBeNull();
      expect(screen.queryByText('No frame of this project is usable on this device.')).toBeNull();
      expect(screen.getByText(TOO_FEW.message)).toBeInTheDocument();
      expect(screen.getByTestId('board')).toBeInTheDocument();
      expect(screen.getByTestId('groups')).toBeInTheDocument();
      expect(screen.getByTestId('inspector')).toBeInTheDocument();
      expect(screen.getByRole('button', { name: /Run stacking/ })).toBeDisabled();
    });

    it('is not shown while one usable frame is still included', async () => {
      projectWith({ blockers: [TOO_FEW], excluded: ['u1'] });
      await mount(projectTab('p1'));

      expect(screen.queryByText('Nothing to stack on this device')).toBeNull();
      expect(screen.getByTestId('board')).toBeInTheDocument();
      expect(screen.getByTestId('groups')).toBeInTheDocument();
    });

    it('keeps the Frames table under it when the plan lists frames', async () => {
      projectWith({ blockers: [NO_FRAMES], projectFrames: ELSEWHERE });
      await mount(projectTab('p1'));

      expect(screen.getByRole('button', { name: /^Frames \(3\)/ })).toBeInTheDocument();
      expect(screen.getByText('a.fits')).toBeInTheDocument();
      expect(screen.getAllByText('not on this device')).toHaveLength(3);
    });

    it('says every usable frame is excluded when frames are on this device, and an exclusion can be undone', async () => {
      projectWith({ blockers: [TOO_FEW], excluded: ['u1', 'u2'], reference: 'u2' });
      await mount(projectTab('p1'));
      await advance(PAST_THE_DEBOUNCE_MS);

      expect(screen.getByText('Nothing to stack on this device')).toBeInTheDocument();
      expect(screen.getByText('Every usable frame is excluded.')).toBeInTheDocument();
      expect(screen.queryByText('No frame of this project is usable on this device.')).toBeNull();
      expect(screen.queryByTestId('board')).toBeNull();
      expect(screen.getByTestId('groups')).toBeInTheDocument();

      const include = screen.getByRole('checkbox', { name: 'Include a.fits in the next run' });
      expect(include).not.toBeChecked();
      fireEvent.click(include);
      await advance(PAST_THE_DEBOUNCE_MS);
      expect(argsOf('set_project_stacking_config')).toEqual([
        { projectId: 'p1', config: { ...CONFIG, reference: { mode: 'manual', twoPass: false } }, excludedFrameUuids: ['u2'], referenceFrameUuid: 'u2' },
      ]);
    });

    it('keeps the grid card above it', async () => {
      projectWith({
        blockers: [NO_FRAMES], projectFrames: ELSEWHERE,
        grid: {
          state: 'proposed', current: null, framesInside: 12, canEdit: true,
          proposal: { scaleArcsec: 1.5, widthPx: 3200, heightPx: 2560, crval1: 83.82, crval2: -5.39, rotationDeg: 0 },
          target: { raDeg: 83.82, decDeg: -5.39, radiusDeg: 1 }, footprints: [], framesNotDrawn: 0,
        },
      });
      await mount(projectTab('p1'));
      expect(screen.getByRole('heading', { name: 'Canonical grid \u2014 not confirmed' })).toBeInTheDocument();
      expect(screen.getByText('Nothing to stack on this device')).toBeInTheDocument();
    });

    it('is not shown while the plan has no frames blocker', async () => {
      projectWith({ blockers: [{ code: 'grid', message: 'The project has no confirmed grid' }] });
      await mount(projectTab('p1'));

      expect(screen.queryByText('Nothing to stack on this device')).toBeNull();
      expect(screen.getByTestId('board')).toBeInTheDocument();
      expect(screen.getByTestId('groups')).toBeInTheDocument();
    });

    it('is never shown for a frame set: its board stays whatever its blockers', async () => {
      vi.mocked(api.invoke).mockImplementation(((command: string, args?: Record<string, unknown>) =>
        command === 'get_stacking_plan'
          ? Promise.resolve(plan({ setId: args?.setId, blockers: [TOO_FEW] }))
          : answer(command, args)) as never);
      await mount(frameSetTab(7));

      expect(screen.queryByText('Nothing to stack on this device')).toBeNull();
      expect(screen.getByTestId('board')).toBeInTheDocument();
      expect(screen.getByText(TOO_FEW.message)).toBeInTheDocument();
    });
  });

  // D7: a project's plan follows the live feed: `collab-grid-changed` and `collab-frames-changed` for this project,
  // coalesced to one refetch per second.
  describe('the live feed', () => {
    /** Replaces the `listen` mock with one that knows which registrations are still subscribed. */
    function trackListeners() {
      const live = new Map<string, Set<(payload: unknown) => void>>();
      const registered: string[] = [];
      const released: string[] = [];
      vi.mocked(api.listen).mockImplementation(((event: string, cb: (payload: unknown) => void) => {
        handlers[event] = cb;
        if (!live.has(event)) live.set(event, new Set());
        live.get(event)!.add(cb);
        registered.push(event);
        return Promise.resolve(() => {
          live.get(event)!.delete(cb);
          released.push(event);
        });
      }) as never);
      const count = (event: string, list: string[]) => list.filter((e) => e === event).length;
      return {
        subscribed: (event: string) => live.get(event)?.size ?? 0,
        registered: (event: string) => count(event, registered),
        released: (event: string) => count(event, released),
        /** Delivers the payload to every callback still subscribed to the event. */
        fire: (event: string, payload: unknown) => act(() => {
          for (const cb of [...(live.get(event) ?? [])]) cb(payload);
        }),
      };
    }
    const plans = () => argsOf('get_project_stacking_plan').length;

    it('a collab-grid-changed for this project refetches the plan once, with the draft config', async () => {
      const feed = trackListeners();
      await mount(projectTab('p1'));
      // One draft field is edited and its own re-plan has landed: the event's refetch must carry the edit, not the
      // config the tab loaded with.
      const edited = { ...CONFIG, drizzle: { enabled: true } };
      fireEvent.click(screen.getByText('edit config'));
      await advance(PAST_THE_DEBOUNCE_MS);
      const before = plans();
      expect(argsOf('get_project_stacking_plan')[before - 1]).toEqual({ projectId: 'p1', config: edited });

      feed.fire('collab-grid-changed', { projectId: 'p1' });
      await advance(900);
      expect(plans()).toBe(before); // coalescing: nothing yet, the timer is trailing
      await advance(200);
      expect(plans()).toBe(before + 1);
      expect(argsOf('get_project_stacking_plan')[before]).toEqual({ projectId: 'p1', config: edited });

      // The next one refetches again: the first did not leave the tab waiting for ever.
      feed.fire('collab-grid-changed', { projectId: 'p1' });
      await advance(1100);
      expect(plans()).toBe(before + 2);
    });

    it('a collab-frames-changed for another project does not refetch', async () => {
      const feed = trackListeners();
      await mount(projectTab('p1'));
      await advance(PAST_THE_DEBOUNCE_MS);
      const before = plans();

      feed.fire('collab-frames-changed', { projectId: 'p2', kind: 'newFrames', count: 3 });
      feed.fire('collab-grid-changed', { projectId: 'p2' });
      await advance(PAST_THE_DEBOUNCE_MS);
      expect(plans()).toBe(before);

      // The same event for this project does (the check above is not vacuous).
      feed.fire('collab-frames-changed', { projectId: 'p1', kind: 'newFrames', count: 3 });
      await advance(1100);
      expect(plans()).toBe(before + 1);
    });

    it('three events for this project inside one second cause one plan call', async () => {
      const feed = trackListeners();
      await mount(projectTab('p1'));
      await advance(PAST_THE_DEBOUNCE_MS);
      const before = plans();

      feed.fire('collab-frames-changed', { projectId: 'p1', kind: 'newFrames', count: 1 });
      await advance(300);
      feed.fire('collab-grid-changed', { projectId: 'p1' });
      await advance(300);
      feed.fire('collab-frames-changed', { projectId: 'p1', kind: 'remapped', count: 2 });
      await advance(PAST_THE_DEBOUNCE_MS);

      expect(plans()).toBe(before + 1);
    });

    it('an event still waiting when the tab unmounts refetches nothing', async () => {
      const feed = trackListeners();
      const view = await mount(projectTab('p1'));
      await advance(PAST_THE_DEBOUNCE_MS);
      const before = plans();

      feed.fire('collab-grid-changed', { projectId: 'p1' });
      await advance(300);
      view.unmount();
      await advance(PAST_THE_DEBOUNCE_MS);

      expect(plans()).toBe(before);
    });

    it('a frame set tab listens to neither event', async () => {
      const feed = trackListeners();
      await mount(frameSetTab(7));
      expect(feed.registered('collab-grid-changed')).toBe(0);
      expect(feed.registered('collab-frames-changed')).toBe(0);
    });

    // Review Focus 5: StrictMode mounts the tab twice; exactly one listener per event remains, and unmounting
    // releases every registration.
    it('after a StrictMode double mount one listener per event remains, and unmount releases them all', async () => {
      const feed = trackListeners();
      const view = render(<StrictMode>{tree(projectTab('p1'))}</StrictMode>);
      for (let i = 0; i < 4; i++) await advance(0);

      for (const event of ['collab-grid-changed', 'collab-frames-changed']) {
        expect(feed.registered(event), event).toBe(2);
        expect(feed.subscribed(event), event).toBe(1);
      }

      view.unmount();
      await advance(0);
      for (const event of ['collab-grid-changed', 'collab-frames-changed']) {
        expect(feed.subscribed(event), event).toBe(0);
        expect(feed.released(event), event).toBe(feed.registered(event));
      }
    });
  });
});
