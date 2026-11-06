// Wave 6c Task 5: what the Stacking tab shows for a project and for an attested set — Prepare in place of Calibrate,
// no way to a Calibration tab a project does not have, and a reference pinned in the Frames table. The real inspector
// and panels render here; the board (pinned in `PipelineBoard.test.tsx`) and the groups table are stubs.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { fireEvent, render, screen } from '@testing-library/react';
import type { ReactElement } from 'react';
import { MemoryRouter } from 'react-router-dom';
import { advance, PAST_THE_DEBOUNCE_MS } from '../../test/fakeClock';
import { StackingProvider } from '../../contexts/StackingContext';
import { StackingTab } from './StackingTab';
import { STACKING_SELECTED_STAGE_KEY } from './stackingPrefs';
import { api } from '../../api';
import type { PlanPrepare, StackingConfig, StackingPlan } from '../../types/stacking';

vi.mock('../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

vi.mock('../../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: vi.fn() }),
}));

vi.mock('./PipelineBoard', () => ({ PipelineBoard: () => null }));
vi.mock('./GroupsTable', () => ({ GroupsTable: () => null }));

const CONFIG = {
  version: 1,
  paths: { working: null, output: null },
  calibration: { flatNorm: true, flatNormMode: 'centralThird', hotPixelCorrection: true, debayerOsc: true },
  reference: { mode: 'manual', twoPass: false },
  drizzle: { enabled: false },
  normalization: { local: { enabled: false }, rejection: 'scaleZeroOffset' },
} as unknown as StackingConfig;

const PREPARE: PlanPrepare = { inPlace: 3, debayered: 1, converted: 2, unsupported: 4, atRun: 5 };

const BLOCKERS = [
  { code: 'masters', message: 'A calibration set has no master' },
  { code: 'links', message: 'Some lights have no calibration links' },
  { code: 'masterFiles', message: 'A master file is missing' },
];

const READINESS = {
  total: 2, unlinkedLights: 0, rawSetsWithoutMaster: 0, rawSetIdsWithoutMaster: [], missingMasterFiles: 0,
  blackHoleZipMasters: 0, geometryMismatches: [], geometryWarnings: [], attested: false,
};

const plan = (over: Record<string, unknown> = {}): StackingPlan =>
  ({
    setId: null, projectId: null, setName: 'T', config: CONFIG, groups: [], blockers: BLOCKERS, warnings: [],
    staleStages: [], activeRunId: null, freeBytes: 5e9, estimateBytes: 1e9, workingDir: '/work', outputDir: '/out',
    readiness: READINESS,
    reference: { mode: 'manual', frameId: null, filename: 'ref.fits', onDisk: true },
    mastersToBuild: [], frameCount: 2, includedCount: 2, excludedFrameIds: [], prepare: null, ...over,
  }) as unknown as StackingPlan;

const USAGE = {
  totalBytes: 0, calibratedBytes: 0, registeredBytes: 0, lnBytes: 0, runsBytes: 0, rejBytes: 0, previewsBytes: 0,
};

/** The plan the backend answers for each kind of subject. */
const PLANS = {
  frameSet: plan({ setId: 7 }),
  attested: plan({ setId: 7, readiness: { ...READINESS, attested: true }, prepare: PREPARE }),
  project: plan({ projectId: 'p1', prepare: PREPARE }),
};

function answer(command: string, plans: typeof PLANS): Promise<unknown> {
  switch (command) {
    case 'get_stacking_config':
      return Promise.resolve({ config: CONFIG, excludedFrameIds: [], isDefault: false, updatedAt: null });
    case 'get_project_stacking_config':
      return Promise.resolve({ config: CONFIG, excludedFrameUuids: [], referenceFrameUuid: null, isDefault: false, updatedAt: null });
    case 'get_stacking_presets':
      return Promise.resolve({ default: CONFIG, fastPreview: CONFIG, maximumQuality: CONFIG });
    case 'list_stacking_presets':
      return Promise.resolve([]);
    case 'get_stacking_plan':
      return Promise.resolve(plans.frameSet);
    case 'get_project_stacking_plan':
      return Promise.resolve(plans.project);
    case 'get_stacking_runs':
    case 'get_project_stacking_runs':
      return Promise.resolve([]);
    case 'get_stacking_work_usage':
    case 'get_project_stacking_work_usage':
      return Promise.resolve(USAGE);
    case 'set_project_stacking_config':
      return Promise.resolve({ config: CONFIG, excludedFrameUuids: [], referenceFrameUuid: null, isDefault: false, updatedAt: null });
    default:
      return Promise.resolve(undefined);
  }
}

let plans: typeof PLANS;

beforeEach(() => {
  plans = { ...PLANS };
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((command: string) => answer(command, plans)) as never);
  vi.mocked(api.listen).mockReset();
  vi.mocked(api.listen).mockImplementation((() => Promise.resolve(() => {})) as never);
  try { localStorage.clear(); } catch { /* the tab reads its prefs defensively too */ }
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
  vi.restoreAllMocks();
});

const projectTab = <StackingTab subject={{ kind: 'project', projectId: 'p1', title: 'Project' }} />;
const frameSetTab = (
  <StackingTab subject={{ kind: 'frameSet', framesSetId: 7, lightFrames: [{ frameId: 101, filename: 'a.fit' }] }} />
);

async function mount(tab: ReactElement, stage?: string) {
  if (stage) localStorage.setItem(STACKING_SELECTED_STAGE_KEY, stage);
  const view = render(
    <MemoryRouter>
      <StackingProvider>{tab}</StackingProvider>
    </MemoryRouter>,
  );
  for (let i = 0; i < 4; i++) await advance(0);
  return view;
}

const argsOf = (command: string) =>
  vi.mocked(api.invoke).mock.calls.filter(([c]) => c === command).map(([, args]) => args);

describe('StackingTab Coverage link', () => {
  it('is not rendered for a project, whose blockers still show', async () => {
    await mount(projectTab);
    for (const b of BLOCKERS) expect(screen.getByText(b.message)).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: /Coverage/ })).toBeNull();
  });

  it('is rendered beside each calibration blocker of a frame set', async () => {
    await mount(frameSetTab);
    expect(screen.getAllByRole('button', { name: /Coverage/ })).toHaveLength(3);
  });
});

describe('StackingTab reference for a manual choice', () => {
  it('sends a project to its Frames table, with no Choose in Analysis button', async () => {
    await mount(projectTab, 'reference');
    expect(
      screen.getByText('Pin a frame in the Frames table below. Only frames with a WCS can be the reference.'),
    ).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Choose in Analysis' })).toBeNull();
  });

  it('keeps Choose in Analysis for a frame set', async () => {
    await mount(frameSetTab, 'reference');
    expect(screen.getByRole('button', { name: 'Choose in Analysis' })).toBeInTheDocument();
  });

  it('keeps Choose in Analysis for an attested set', async () => {
    plans.frameSet = PLANS.attested;
    await mount(frameSetTab, 'reference');
    expect(screen.getByRole('button', { name: 'Choose in Analysis' })).toBeInTheDocument();
  });
});

describe('StackingTab Prepare', () => {
  it('opens a project on the Prepare panel, with the counts and no calibration control', async () => {
    await mount(projectTab);
    expect(screen.getByRole('heading', { name: '1 · Prepare' })).toBeInTheDocument();
    expect(screen.getByText('Checked when the run starts').closest('dt')?.nextElementSibling?.textContent).toBe('5');
    expect(screen.queryByText(/Normalize master flat/)).toBeNull();
    expect(screen.queryByText(/Masters resolved/)).toBeNull();
  });

  it('opens an attested set on the Prepare panel too', async () => {
    plans.frameSet = PLANS.attested;
    await mount(frameSetTab);
    expect(screen.getByRole('heading', { name: '1 · Prepare' })).toBeInTheDocument();
  });

  it('opens a frame set on the Calibrate panel', async () => {
    await mount(frameSetTab);
    expect(screen.getByRole('heading', { name: '1 · Calibrate' })).toBeInTheDocument();
    expect(screen.getByText('Normalize master flat (recommended)')).toBeInTheDocument();
    expect(screen.queryByText('In place')).toBeNull();
  });

  it('the debayer option writes the whole project config with it turned off', async () => {
    await mount(projectTab);
    fireEvent.click(screen.getByRole('checkbox', { name: 'Debayer OSC lights (VNG)' }));
    await advance(PAST_THE_DEBOUNCE_MS);
    expect(argsOf('set_project_stacking_config')).toEqual([
      {
        projectId: 'p1',
        config: { ...CONFIG, calibration: { ...CONFIG.calibration, debayerOsc: false } },
        excludedFrameUuids: [],
        referenceFrameUuid: null,
      },
    ]);
  });

  it('names the first stage Prepare in the Re-run menu of a project, and Calibrate for a frame set', async () => {
    const view = await mount(projectTab);
    fireEvent.click(screen.getByRole('button', { name: /Re-run from/ }));
    expect(screen.getByRole('button', { name: 'Prepare' })).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Calibrate' })).toBeNull();
    view.unmount();

    await mount(frameSetTab);
    fireEvent.click(screen.getByRole('button', { name: /Re-run from/ }));
    expect(screen.getByRole('button', { name: 'Calibrate' })).toBeInTheDocument();
  });
});
