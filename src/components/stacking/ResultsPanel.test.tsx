// Wave 6c Task 8 (D4): a group whose run wrote a coverage map shows its thumbnail beside the master's, and the
// drizzle coverage map gets a path line. The previews are fetched one at a time (master, then the drizzle master,
// then the coverage map): each render reads a full float image into the backend.
import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { api } from '../../api';
import { routeInvoke } from '../../test/apiMock';
import { ResultsPanel } from './ResultsPanel';
import type { StackSubject } from './subject';

vi.mock('../../api');

// The web build prints a path and a copy button; the desktop build has a Reveal button instead.
const { platform, revealMock } = vi.hoisted(() => ({
  platform: { isTauri: false },
  revealMock: vi.fn().mockResolvedValue(undefined),
}));
vi.mock('../../utils/platform', () => ({ get isTauri() { return platform.isTauri; }, isMac: false }));
vi.mock('../../api/desktop', () => ({ revealItemInDir: revealMock }));
vi.mock('../../contexts/NotificationContext', () => ({ useNotifications: () => ({ notify: vi.fn() }) }));
vi.mock('../../contexts/StackingContext', () => ({ useStackingContext: () => ({ lastOutcome: new Map() }) }));

const CAPTION = 'Coverage map — frames per pixel';

const USAGE = {
  totalBytes: 0, calibratedBytes: 0, registeredBytes: 0, lnBytes: 0, runsBytes: 0, rejBytes: 0, previewsBytes: 0,
};
const RUN = { id: 5, status: 'done', startedAt: '2026-11-01T10:00:00Z', finishedAt: null, error: null };

interface GroupFiles {
  drizzlePath?: string | null;
  coveragePath?: string | null;
  drizzleCoveragePath?: string | null;
  /** The run's `summary` has not been written yet (the run is still going). */
  noSummary?: boolean;
}

const detail = (f: GroupFiles = {}) => ({
  run: RUN,
  frames: [],
  groups: [
    {
      id: 1, runId: 5, groupKey: 'g1', masterPath: '/out/g1_master.fits', drizzlePath: f.drizzlePath ?? null,
      includedCount: 9, frameCount: 10, status: 'done', error: null, statsJson: null,
    },
  ],
  summary: f.noSummary
    ? null
    : {
        reference: { filename: null, switchedFrom: null },
        config: { registration: { geometry: 'coregistered' }, drizzle: { enabled: f.drizzlePath != null } },
        groups: [
          {
            key: 'g1', lnReferencePath: null, referenceFrameId: null, drizzlePath: f.drizzlePath ?? null,
            weightMapPath: null, drizzle: null, coveragePath: f.coveragePath ?? null,
            drizzleCoveragePath: f.drizzleCoveragePath ?? null,
          },
        ],
      },
});

/** A preview request the test has not answered yet. */
interface Pending {
  kind: string;
  args: Record<string, unknown>;
  resolve: () => void;
  reject: (err: string) => void;
}
let previews: Pending[] = [];
const requested = () => previews.map((p) => p.kind);

function install(files: GroupFiles) {
  previews = [];
  const runs = () => [{ run: RUN, groupCount: 1, masterPaths: [] }];
  routeInvoke({
    get_stacking_runs: runs,
    get_project_stacking_runs: runs,
    get_stacking_run: () => detail(files),
    get_stacking_work_usage: USAGE,
    get_project_stacking_work_usage: USAGE,
    get_master_light_preview: (args: Record<string, unknown> = {}) =>
      new Promise((resolve, reject) => {
        previews.push({
          kind: String(args.kind), args, resolve: () => resolve(new Uint8Array([1, 2, 3])), reject: (e) => reject(e),
        });
      }),
  }, () => undefined);
}

const PROJECT: StackSubject = { kind: 'project', projectId: 'p1', title: 'Project' };
const FRAME_SET: StackSubject = { kind: 'frameSet', framesSetId: 7, lightFrames: [] };

const mount = (subject: StackSubject = PROJECT) =>
  render(<ResultsPanel subject={subject} running={false} onSelectedRunDetailChange={vi.fn()} />);

/** Answers the oldest unanswered preview of `kind`. */
const answer = async (kind: string) => {
  const p = previews.find((x) => x.kind === kind);
  expect(p, `a pending ${kind} preview`).toBeDefined();
  await act(async () => { p?.resolve(); });
};

/** Lets every pending effect and promise settle, so "nothing more was asked for" means something. */
const settle = () => act(async () => { await new Promise((r) => setTimeout(r, 20)); });

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
  platform.isTauri = false;
  revealMock.mockClear();
  try { localStorage.clear(); } catch { /* the panel reads its prefs defensively */ }
  URL.createObjectURL = vi.fn(() => 'blob:preview');
  URL.revokeObjectURL = vi.fn();
});

afterEach(() => {
  vi.restoreAllMocks();
});

describe.each([['project', PROJECT], ['frame set', FRAME_SET]] as const)('ResultsPanel coverage map of a %s', (_name, subject) => {
  it('asks for the coverage preview only after the master preview resolved', async () => {
    install({ coveragePath: '/out/g1_coverage.fits' });
    mount(subject);
    await waitFor(() => expect(requested()).toEqual(['master']));
    await settle();
    expect(requested()).toEqual(['master']);

    await answer('master');
    await waitFor(() => expect(requested()).toEqual(['master', 'coverage']));
    expect(previews[1]?.args).toEqual({ runId: 5, groupKey: 'g1', kind: 'coverage', maxPx: 512 });
  });

  it('asks for it after the drizzle preview too when the group has a drizzled master', async () => {
    install({
      drizzlePath: '/out/g1_drizzle.fits', coveragePath: '/out/g1_coverage.fits',
      drizzleCoveragePath: '/out/g1_drizzle_coverage.fits',
    });
    mount(subject);
    await waitFor(() => expect(requested()).toEqual(['master']));

    await answer('master');
    await waitFor(() => expect(requested()).toEqual(['master', 'drizzle']));
    await settle();
    // The drizzle render is still running: the coverage map waits for it.
    expect(requested()).toEqual(['master', 'drizzle']);

    await answer('drizzle');
    await waitFor(() => expect(requested()).toEqual(['master', 'drizzle', 'coverage']));
    // The drizzle coverage map is a path line only: it never gets a render of its own.
    await settle();
    expect(requested()).not.toContain('drizzleCoverage');
  });
});

describe('ResultsPanel coverage map', () => {
  it('goes on to the coverage preview when the master preview failed', async () => {
    vi.spyOn(console, 'error').mockImplementation(() => {});
    install({ coveragePath: '/out/g1_coverage.fits' });
    mount();
    await waitFor(() => expect(requested()).toEqual(['master']));

    await act(async () => { previews[0]?.reject('output not found'); });
    await waitFor(() => expect(requested()).toEqual(['master', 'coverage']));
  });

  it('shows the thumbnail beside the master’s, labelled, with its path line under it', async () => {
    install({ coveragePath: '/out/g1_coverage.fits' });
    mount();
    await waitFor(() => expect(requested()).toEqual(['master']));
    await answer('master');
    await waitFor(() => expect(requested()).toEqual(['master', 'coverage']));
    await answer('coverage');

    const beside = await screen.findByTestId('master-previews');
    expect(within(beside).getByAltText('Preview of g1_master.fits')).toBeInTheDocument();
    const coverage = within(beside).getByAltText('Preview of g1_coverage.fits');
    expect(coverage).toBeInTheDocument();
    // The caption and the path line sit with the coverage thumbnail, in that order.
    const figure = coverage.closest('figure') as HTMLElement;
    expect(figure).not.toBeNull();
    expect(within(figure).getByText(CAPTION)).toBeInTheDocument();
    expect(within(figure).getByText('/out/g1_coverage.fits')).toBeInTheDocument();
    const text = figure.textContent ?? '';
    expect(text.indexOf(CAPTION)).toBeLessThan(text.indexOf('/out/g1_coverage.fits'));
  });

  it('reveals the coverage map\u2019s own file from the line under its thumbnail on the desktop', async () => {
    platform.isTauri = true;
    install({ coveragePath: '/out/g1_coverage.fits' });
    mount();
    await waitFor(() => expect(requested()).toEqual(['master']));
    await answer('master');
    await waitFor(() => expect(requested()).toEqual(['master', 'coverage']));
    await answer('coverage');

    const figure = (await screen.findByAltText('Preview of g1_coverage.fits')).closest('figure') as HTMLElement;
    fireEvent.click(within(figure).getByRole('button', { name: /Reveal/ }));
    expect(revealMock).toHaveBeenCalledWith('/out/g1_coverage.fits');
  });

  it('shows the drizzle coverage map as a path line only', async () => {
    install({
      drizzlePath: '/out/g1_drizzle.fits', coveragePath: '/out/g1_coverage.fits',
      drizzleCoveragePath: '/out/g1_drizzle_coverage.fits',
    });
    mount();
    await waitFor(() => expect(requested()).toEqual(['master']));

    const line = (await screen.findByText('Drizzle coverage map:')).parentElement as HTMLElement;
    expect(within(line).getByText('/out/g1_drizzle_coverage.fits')).toBeInTheDocument();
    // Its path is not an image: no thumbnail carries it.
    expect(screen.queryByAltText('Preview of g1_drizzle_coverage.fits')).toBeNull();
  });

  it('requests nothing and shows nothing when the paths are null', async () => {
    install({ drizzlePath: '/out/g1_drizzle.fits' });
    mount();
    await waitFor(() => expect(requested()).toEqual(['master']));
    await answer('master');
    await waitFor(() => expect(requested()).toEqual(['master', 'drizzle']));
    await answer('drizzle');
    await settle();

    expect(requested()).toEqual(['master', 'drizzle']);
    expect(screen.queryByText(CAPTION)).toBeNull();
    expect(screen.queryByText(/Drizzle coverage map/)).toBeNull();
    expect(screen.queryByTestId('master-previews')).toBeNull();
  });

  it('asks for nothing before the run’s summary has been written', async () => {
    install({ noSummary: true });
    mount();
    await waitFor(() => expect(requested()).toEqual(['master']));
    await answer('master');
    await settle();

    expect(requested()).toEqual(['master']);
    expect(screen.queryByText(CAPTION)).toBeNull();
  });
});

// Dark guard: a skipped group says why on its card, the header counts them, and a finished run that lost groups says
// so in the run dropdown.
describe('ResultsPanel skipped groups', () => {
  const skippedGroup = (id: number, key: string, error: string | null) => ({
    id, runId: 5, groupKey: key, masterPath: null, drizzlePath: null,
    includedCount: 0, frameCount: 14, status: 'skipped', error, statsJson: null,
  });
  const doneGroup = {
    id: 1, runId: 5, groupKey: 'R', masterPath: '/out/R_master.fits', drizzlePath: null,
    includedCount: 9, frameCount: 10, status: 'done', error: null, statsJson: null,
  };

  function installGroups(groups: unknown[], runSummary: { groupCount: number; masterPaths: string[]; status?: string }) {
    previews = [];
    const run = { ...RUN, status: runSummary.status ?? 'done' };
    const runs = () => [{ run, groupCount: runSummary.groupCount, masterPaths: runSummary.masterPaths }];
    routeInvoke({
      get_stacking_runs: runs,
      get_project_stacking_runs: runs,
      get_stacking_run: () => ({ run, frames: [], groups, summary: null }),
      get_stacking_work_usage: USAGE,
      get_project_stacking_work_usage: USAGE,
      get_master_light_preview: () => new Promise(() => undefined),
    }, () => undefined);
  }

  const header = (start: string) => {
    const p = Array.from(document.querySelectorAll('p')).find((e) => e.textContent?.startsWith(start));
    if (!p) throw new Error(`no header line starting ${start}`);
    return p;
  };

  it('a skipped card shows the reason in the warning colour', async () => {
    installGroups(
      [doneGroup, skippedGroup(2, 'L', '2 of 14 frames registered; at least 3 are needed')],
      { groupCount: 2, masterPaths: ['/out/R_master.fits'] },
    );
    mount();
    const line = await screen.findByText('Skipped: 2 of 14 frames registered; at least 3 are needed');
    expect(line.className).toContain('text-warning');
  });

  it('a skipped card from an older run has no reason and says only Skipped', async () => {
    installGroups([skippedGroup(2, 'L', null), skippedGroup(3, 'G', '')], { groupCount: 2, masterPaths: [] });
    mount();
    const lines = await screen.findAllByText('Skipped');
    expect(lines).toHaveLength(2);
    expect(lines[0].className).toContain('text-warning');
  });

  it('the header counts the skipped groups, and only when there are some', async () => {
    installGroups(
      [doneGroup, skippedGroup(2, 'L', 'fewer than 3 included frames'), skippedGroup(3, 'G', null)],
      { groupCount: 3, masterPaths: ['/out/R_master.fits'] },
    );
    const first = mount();
    const count = await screen.findByText('· 2 skipped');
    expect(count.className).toContain('text-warning');
    expect(header('3 groups')).toBeDefined();
    first.unmount();

    installGroups([doneGroup], { groupCount: 1, masterPaths: ['/out/R_master.fits'] });
    mount();
    await waitFor(() => expect(header('1 group')).toBeDefined());
    expect(screen.queryByText(/skipped/)).toBeNull();
  });

  it('the run dropdown says how many groups of a finished run have no master', async () => {
    installGroups([doneGroup], { groupCount: 3, masterPaths: ['/out/R_master.fits'] });
    mount();
    const option = await screen.findByRole('option', { name: /#5 · .* · done · 2 of 3 groups without a master/ });
    expect(option).toBeDefined();
  });

  it('the run dropdown stays plain when every group has a master or the run is not done', async () => {
    installGroups([doneGroup], { groupCount: 1, masterPaths: ['/out/R_master.fits'] });
    const first = mount();
    expect((await screen.findByRole('option', { name: /#5 · .* · done$/ }))).toBeDefined();
    first.unmount();

    installGroups([doneGroup], { groupCount: 2, masterPaths: [], status: 'failed' });
    mount();
    const option = await screen.findByRole('option', { name: /#5 · .* · failed$/ });
    expect(option.textContent).not.toContain('without a master');
  });
});
