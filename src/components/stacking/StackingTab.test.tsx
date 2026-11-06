// Wave 6c Task 4: pins what the Stacking tab does for a frame set at the `api` boundary — the commands it sends,
// their arguments and their order — so the move to a stacking subject cannot change it. Rendered through the real
// `StackingProvider` and the real Results panel and Frames table; the board, the inspector and the groups table are
// replaced by stubs (they read a full `StackingConfig` and are not what is pinned here).
//
// Fake timers: the write lands after the tab's 500 ms debounce and the plan refresh after its 300 ms one, so the
// clock is advanced explicitly (`src/test/fakeClock`) instead of racing a real-timer `waitFor`.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen } from '@testing-library/react';
import type { ReactElement } from 'react';
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
  PipelineBoard: (p: { progress?: { runId: number }; onToggleDrizzle: (checked: boolean) => void }) => (
    <div data-testid="board" data-run={p.progress?.runId ?? ''}>
      <button type="button" onClick={() => p.onToggleDrizzle(true)}>edit config</button>
    </div>
  ),
}));
vi.mock('./StageInspector', () => ({ StageInspector: () => null }));
vi.mock('./GroupsTable', () => ({ GroupsTable: () => null }));

const CONFIG = {
  version: 1,
  paths: { working: null, output: null },
  drizzle: { enabled: false },
  normalization: { local: { enabled: false }, rejection: 'scaleZeroOffset' },
} as unknown as StackingConfig;

const plan = (over: Record<string, unknown> = {}): StackingPlan =>
  ({
    setId: 7, projectId: null, setName: 'M31', config: CONFIG, groups: [], blockers: [], warnings: [],
    staleStages: [], activeRunId: null, freeBytes: 5e9, estimateBytes: 1e9, workingDir: '/work', outputDir: '/out',
    readiness: { rawSetsWithoutMaster: 0, rawSetIdsWithoutMaster: [] }, frameCount: 2, includedCount: 2,
    excludedFrameIds: [], ...over,
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

/** What the backend answers, by command name. A command nobody listed resolves to `undefined`. */
function answer(command: string): Promise<unknown> {
  switch (command) {
    case 'get_stacking_config':
      return Promise.resolve({ config: CONFIG, excludedFrameIds: [], isDefault: true, updatedAt: null });
    case 'get_stacking_presets':
      return Promise.resolve({ default: CONFIG, fastPreview: CONFIG, maximumQuality: CONFIG });
    case 'list_stacking_presets':
      return Promise.resolve([]);
    case 'get_stacking_plan':
      return Promise.resolve(plan());
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
    default:
      return Promise.resolve(undefined);
  }
}

const handlers: Record<string, (payload: unknown) => void> = {};

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((command: string) => answer(command)) as never);
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
const frameSetTab = (framesSetId: number) => <StackingTab framesSetId={framesSetId} lightFrames={FRAMES} />;

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
    vi.mocked(api.invoke).mockImplementation(((command: string) =>
      command === 'get_stacking_config'
        ? Promise.resolve({ config: CONFIG, excludedFrameIds: [101], isDefault: false, updatedAt: null })
        : answer(command)) as never);
    await mount(frameSetTab(7));

    fireEvent.click(screen.getByText('edit config'));
    await advance(PAST_THE_DEBOUNCE_MS);

    expect(argsOf('set_stacking_config')).toEqual([
      { setId: 7, config: { ...CONFIG, drizzle: { enabled: true } }, excludedFrameIds: [101] },
    ]);
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
