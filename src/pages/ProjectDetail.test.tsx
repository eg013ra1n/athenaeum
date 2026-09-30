import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, act, waitFor, within } from '@testing-library/react';
import { MemoryRouter, Routes, Route, useLocation } from 'react-router-dom';
import { NotificationProvider } from '../contexts/NotificationContext';
import { SessionStateProvider } from '../contexts/SessionStateContext';
import { NavHistoryProvider } from '../contexts/NavHistoryContext';
import { ToastStack } from '../components/Toast';
import ProjectDetail from './ProjectDetail';
import { useCollabNotifications } from '../hooks/useCollabNotifications';
import { api } from '../api';
import type {
  FrameGateRow,
  GateReport,
  ProjectCard,
  ProjectDetail as Detail,
  ProjectFrameView,
  PublishResult,
} from '../types/models';

vi.mock('../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
}));

function projectCard(overrides: Partial<ProjectCard> = {}): ProjectCard {
  return {
    projectId: 'proj-1',
    slug: 'm42-mosaic',
    title: 'M42 Mosaic',
    dataRole: 'send_receive',
    coordinator: false,
    requireApproval: false,
    pendingFrames: 0,
    projectStatus: 'open',
    targetName: 'M42',
    targetRaDeg: 83.8,
    targetDecDeg: -5.4,
    targetRadiusDeg: 1.5,
    membershipVersion: 1,
    linkedSets: 1,
    candidates: 2,
    publishable: 2,
    autoReplicate: true,
    autoPublish: true,
    fetchedAt: '2026-09-24T00:00:00Z',
    publishingDevice: null,
    publishingHere: false,
    ...overrides,
  };
}

function detailFixture(card: ProjectCard = projectCard()): Detail {
  return {
    card,
    members: [],
    thresholdsVersion: null,
    thresholds: [],
    links: [],
    portalBase: 'https://hub.example',
    goals: null,
  };
}

function gateFixture(overrides: Partial<GateReport> = {}): GateReport {
  return { projectId: 'proj-1', total: 2, publishable: 2, rows: [], blockers: [], ...overrides };
}

let publishedListener: ((res: unknown) => void) | undefined;
/** Every `api.listen` registration this render made, by event name — lets a
 *  test fire `analysis-complete`/`plate-solve-complete` (or any other event)
 *  without a dedicated capture variable per event. */
const listeners: Record<string, ((payload: unknown) => void) | undefined> = {};

beforeEach(() => {
  publishedListener = undefined;
  for (const k of Object.keys(listeners)) delete listeners[k];
  localStorage.clear();
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((command: string) => {
    switch (command) {
      case 'get_collab_project_detail':
        return Promise.resolve(detailFixture());
      case 'evaluate_collab_gate':
        return Promise.resolve(gateFixture());
      case 'list_collab_frames':
        return Promise.resolve([] as ProjectFrameView[]);
      case 'list_collab_projects':
        return Promise.resolve([projectCard()]);
      case 'publish_collab_frames':
        return Promise.resolve({
          announced: 2,
          updated: 0,
          state: 'published',
          heldBack: [],
          unchanged: 0,
        } as PublishResult);
      default:
        return Promise.resolve(null);
    }
  }) as never);
  vi.mocked(api.listen).mockImplementation((<T,>(event: string, cb: (p: T) => void) => {
    listeners[event] = cb as unknown as (payload: unknown) => void;
    if (event === 'collab-published') {
      publishedListener = cb as unknown as (res: unknown) => void;
    }
    return Promise.resolve(() => {});
  }) as never);
});

/** Mounts the same live listener `Layout.tsx` mounts once at the app root
 *  (next to `ProjectDetail`, never inside it), so this test exercises the
 *  real "manual publish → one toast from the live event" path instead of
 *  asserting on an inline call ProjectDetail no longer makes. */
function CollabNotificationsHarness() {
  useCollabNotifications();
  return null;
}

/** Renders wherever `onOpenCalibration`'s `navigate` sends the page, so a
 *  test can assert the exact path + query it landed on. */
function LocationDisplay() {
  const location = useLocation();
  return <div data-testid="location">{location.pathname}{location.search}</div>;
}

function renderProjectDetail() {
  return render(
    <MemoryRouter initialEntries={['/projects/proj-1']}>
      <NavHistoryProvider>
        <SessionStateProvider>
          <NotificationProvider>
            <CollabNotificationsHarness />
            <Routes>
              <Route path="/projects/:id" element={<ProjectDetail />} />
              <Route path="/objects/:id" element={<LocationDisplay />} />
            </Routes>
            <ToastStack />
          </NotificationProvider>
        </SessionStateProvider>
      </NavHistoryProvider>
    </MemoryRouter>,
  );
}

describe('ProjectDetail manual publish', () => {
  it('raises no inline toast of its own; the live collab-published event produces exactly one', async () => {
    renderProjectDetail();

    const publishButton = await screen.findByRole('button', { name: /Publish 2 passing frames/ });
    fireEvent.click(publishButton);

    const confirmButton = await screen.findByRole('button', { name: 'Publish' });
    fireEvent.click(confirmButton);

    // doPublish resolves, closes the confirm dialog, then reloads frames + detail.
    await waitFor(() =>
      expect(screen.queryByRole('button', { name: 'Publish' })).not.toBeInTheDocument(),
    );

    expect(publishedListener).toBeDefined();
    // The invoke resolved successfully — ProjectDetail itself raises no toast.
    expect(screen.queryAllByRole('status')).toHaveLength(0);

    // The backend always emits `collab-published` on a successful publish;
    // the live listener (`useCollabNotifications`) is the one place that
    // turns it into a toast.
    act(() => {
      publishedListener?.({ projectId: 'proj-1', announced: 2, updated: 0, heldBack: 0 });
    });

    const toasts = await screen.findAllByRole('status');
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toHaveTextContent('Published 2 frames in M42 Mosaic');
  });

  it('a failed publish shows an inline error and a toast whose dedupeKey cannot collide with the live one', async () => {
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      switch (command) {
        case 'get_collab_project_detail':
          return Promise.resolve(detailFixture());
        case 'evaluate_collab_gate':
          return Promise.resolve(gateFixture());
        case 'list_collab_frames':
          return Promise.resolve([] as ProjectFrameView[]);
        case 'list_collab_projects':
          return Promise.resolve([projectCard()]);
        case 'publish_collab_frames':
          return Promise.reject(new Error('hub unreachable'));
        default:
          return Promise.resolve(null);
      }
    }) as never);

    renderProjectDetail();

    const publishButton = await screen.findByRole('button', { name: /Publish 2 passing frames/ });
    fireEvent.click(publishButton);
    const confirmButton = await screen.findByRole('button', { name: 'Publish' });
    fireEvent.click(confirmButton);

    expect(await screen.findByText('hub unreachable')).toBeInTheDocument();

    const toasts = await screen.findAllByRole('status');
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toHaveTextContent('Publish failed');
  });

  it('a publish refused because another run is in progress reads as "already running", not a failure', async () => {
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      switch (command) {
        case 'get_collab_project_detail':
          return Promise.resolve(detailFixture());
        case 'evaluate_collab_gate':
          return Promise.resolve(gateFixture());
        case 'list_collab_frames':
          return Promise.resolve([] as ProjectFrameView[]);
        case 'list_collab_projects':
          return Promise.resolve([projectCard()]);
        case 'publish_collab_frames':
          // Both hosts reject with the backend's message as a plain string.
          return Promise.reject('publication of this project is already running');
        default:
          return Promise.resolve(null);
      }
    }) as never);

    renderProjectDetail();

    const publishButton = await screen.findByRole('button', { name: /Publish 2 passing frames/ });
    fireEvent.click(publishButton);
    const confirmButton = await screen.findByRole('button', { name: 'Publish' });
    fireEvent.click(confirmButton);

    expect(
      await screen.findByText(
        'Publication of this project is already running — wait for it to finish, then try again.',
      ),
    ).toBeInTheDocument();
    const toasts = await screen.findAllByRole('status');
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toHaveTextContent('Publication already running');
    expect(toasts[0]).not.toHaveTextContent('Publish failed');
  });
});

/** The default command answers, with `extra` taking precedence per command. */
function mockCommands(
  card: ProjectCard,
  extra: Record<string, (args?: unknown) => Promise<unknown>> = {},
) {
  vi.mocked(api.invoke).mockImplementation(((command: string, args?: unknown) => {
    if (extra[command]) return extra[command](args);
    switch (command) {
      case 'get_collab_project_detail':
        return Promise.resolve(detailFixture(card));
      case 'evaluate_collab_gate':
        return Promise.resolve(gateFixture());
      case 'list_collab_frames':
        return Promise.resolve([] as ProjectFrameView[]);
      case 'list_collab_projects':
        return Promise.resolve([card]);
      default:
        return Promise.resolve(null);
    }
  }) as never);
}

const obsPc = { deviceId: 'dev-obs', name: 'Obs PC' };
const boundElsewhere = projectCard({ publishingDevice: obsPc, publishingHere: false });
const boundHere = projectCard({ publishingDevice: { deviceId: 'dev-me', name: 'Laptop' }, publishingHere: true });

async function publishViaConfirm() {
  fireEvent.click(await screen.findByRole('button', { name: /Publish 2 passing frames/ }));
  fireEvent.click(await screen.findByRole('button', { name: 'Publish' }));
}

describe('ProjectDetail publishing device (A6)', () => {
  it('names this device when it is the publishing device, with no switch offered', async () => {
    mockCommands(boundHere);
    renderProjectDetail();
    expect(await screen.findByText('Publishing from this device')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Publish from this device' })).not.toBeInTheDocument();
  });

  it('names the other device and offers "Publish from this device"', async () => {
    mockCommands(boundElsewhere);
    renderProjectDetail();
    expect(await screen.findByText('Publishing from Obs PC')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Publish from this device' })).toBeInTheDocument();
  });

  it('an unnamed bound device reads as another device of this account', async () => {
    mockCommands(projectCard({ publishingDevice: { deviceId: 'dev-x', name: null }, publishingHere: false }));
    renderProjectDetail();
    expect(await screen.findByText('Publishing from another device of this account')).toBeInTheDocument();
  });

  it('an unbound project says nobody is publishing yet — never "this device", no switch', async () => {
    mockCommands(projectCard({ publishingDevice: null, publishingHere: false }));
    renderProjectDetail();
    expect(await screen.findByText('Nobody is publishing to this project yet')).toBeInTheDocument();
    expect(screen.queryByText('Publishing from this device')).not.toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Publish from this device' })).not.toBeInTheDocument();
  });

  it('the switch is confirmed, sends the project id and updates the card from the answer', async () => {
    const switched = projectCard({ publishingDevice: { deviceId: 'dev-me', name: 'Laptop' }, publishingHere: true });
    const setDevice = vi.fn(() => Promise.resolve(switched));
    mockCommands(boundElsewhere, { set_collab_publishing_device: setDevice });
    renderProjectDetail();

    fireEvent.click(await screen.findByRole('button', { name: 'Publish from this device' }));
    expect(
      await screen.findByText(
        'Obs PC will stop publishing new frames to this project; it can still update the frames it already published.',
      ),
    ).toBeInTheDocument();
    expect(setDevice).not.toHaveBeenCalled();

    fireEvent.click(screen.getByRole('button', { name: 'Switch' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('set_collab_publishing_device', { projectId: 'proj-1' }),
    );
    expect(await screen.findByText('Publishing from this device')).toBeInTheDocument();
    expect(screen.queryByRole('button', { name: 'Publish from this device' })).not.toBeInTheDocument();
    // No reload was needed: the card came from the command's answer.
    expect(vi.mocked(api.invoke).mock.calls.filter(([c]) => c === 'get_collab_project_detail')).toHaveLength(1);
    expect(screen.queryAllByRole('status')).toHaveLength(0);
  });

  it('cancelling the confirm switches nothing', async () => {
    const setDevice = vi.fn(() => Promise.resolve(boundHere));
    mockCommands(boundElsewhere, { set_collab_publishing_device: setDevice });
    renderProjectDetail();
    fireEvent.click(await screen.findByRole('button', { name: 'Publish from this device' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Cancel' }));
    expect(screen.queryByText(/will stop publishing new frames/)).not.toBeInTheDocument();
    expect(setDevice).not.toHaveBeenCalled();
    expect(screen.getByText('Publishing from Obs PC')).toBeInTheDocument();
  });

  it('a failed switch is one warning with the reason, and the card is unchanged', async () => {
    mockCommands(boundElsewhere, {
      set_collab_publishing_device: () => Promise.reject("The account's role may not perform this action."),
    });
    renderProjectDetail();
    fireEvent.click(await screen.findByRole('button', { name: 'Publish from this device' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Switch' }));
    const toasts = await screen.findAllByRole('status');
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toHaveTextContent('Could not publish from this device');
    // The toast shows the title; the reason is the entry's detail.
    await waitFor(() =>
      expect(localStorage.getItem('athenaeum.notifications.v1') ?? '').toContain(
        "The account's role may not perform this action.",
      ),
    );
    expect(screen.getByText('Publishing from Obs PC')).toBeInTheDocument();
  });

  it('a publish refused by the publishing device names it and offers the switch — not a generic failure', async () => {
    mockCommands(boundElsewhere, {
      publish_collab_frames: () => Promise.reject('collab_publishing_device:Obs PC'),
    });
    renderProjectDetail();
    await publishViaConfirm();

    const refusal = await screen.findByTestId('publishing-refusal');
    expect(refusal).toHaveTextContent('Obs PC publishes new frames to this project');
    expect(within(refusal).getByRole('button', { name: 'Publish from this device' })).toBeInTheDocument();
    expect(screen.queryByText('collab_publishing_device:Obs PC')).not.toBeInTheDocument();
    // The publish confirm closed; the one toast names the device, not "Publish failed".
    expect(screen.queryByRole('button', { name: 'Publish' })).not.toBeInTheDocument();
    const toasts = await screen.findAllByRole('status');
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toHaveTextContent('Obs PC publishes new frames to this project');
    expect(toasts[0]).not.toHaveTextContent('Publish failed');

    // The refusal's action opens the same confirm.
    fireEvent.click(within(refusal).getByRole('button', { name: 'Publish from this device' }));
    expect(await screen.findByText(/Obs PC will stop publishing new frames to this project/)).toBeInTheDocument();
  });

  it('an unnamed device in the refusal reads as another device of this account', async () => {
    mockCommands(boundElsewhere, {
      publish_collab_frames: () => Promise.reject('collab_publishing_device:another device of this account'),
    });
    renderProjectDetail();
    await publishViaConfirm();
    expect(await screen.findByTestId('publishing-refusal')).toHaveTextContent(
      'Another device of this account publishes new frames to this project',
    );
  });

  it('a republish refused the same way shows the same message', async () => {
    mockCommands(boundElsewhere, {
      list_collab_frames: () => Promise.resolve([ownFrame()]),
      republish_collab_frames: () => Promise.reject('collab_publishing_device:Obs PC'),
    });
    renderProjectDetail();
    fireEvent.click(await screen.findByRole('button', { name: /Recalibrate and republish all/ }));
    fireEvent.click(await screen.findByRole('button', { name: 'Republish' }));
    expect(await screen.findByTestId('publishing-refusal')).toHaveTextContent(
      'Obs PC publishes new frames to this project',
    );
    expect(screen.queryByText('Republish failed')).not.toBeInTheDocument();
  });

  it('a run that succeeds with new frames held back for the other device shows the same message', async () => {
    mockCommands(boundElsewhere, {
      publish_collab_frames: () =>
        Promise.resolve({
          announced: 0,
          updated: 1,
          state: null,
          heldBack: [
            {
              frameId: 7,
              filename: 'L_0007.fits',
              reasons: ['Obs PC publishes new frames to this project — use "Publish from this device" to switch'],
              publishingDevice: 'Obs PC',
            },
          ],
          unchanged: 0,
        } as PublishResult),
    });
    renderProjectDetail();
    await publishViaConfirm();

    const refusal = await screen.findByTestId('publishing-refusal');
    expect(refusal).toHaveTextContent('Obs PC publishes new frames to this project');
    expect(within(refusal).getByRole('button', { name: 'Publish from this device' })).toBeInTheDocument();
    // A successful run raises no toast of its own (the live event does).
    expect(screen.queryAllByRole('status')).toHaveLength(0);
  });

  it('a successful run with unrelated held-back reasons shows no refusal', async () => {
    mockCommands(boundHere, {
      publish_collab_frames: () =>
        Promise.resolve({
          announced: 1,
          updated: 0,
          state: 'published',
          heldBack: [
            {
              frameId: 8,
              filename: 'L_0008.fits',
              reasons: ['calibration failed: no master dark'],
              publishingDevice: null,
            },
          ],
          unchanged: 0,
        } as PublishResult),
    });
    renderProjectDetail();
    await publishViaConfirm();
    await waitFor(() =>
      expect(screen.queryByRole('button', { name: 'Publish' })).not.toBeInTheDocument(),
    );
    expect(screen.queryByTestId('publishing-refusal')).not.toBeInTheDocument();
  });

  it('the refusal box names the device from the held-back field, not from the reason text', async () => {
    mockCommands(boundElsewhere, {
      publish_collab_frames: () =>
        Promise.resolve({
          announced: 0,
          updated: 1,
          state: null,
          heldBack: [
            { frameId: 7, filename: 'L_0007.fits', reasons: ['held back'], publishingDevice: 'Rig 2' },
          ],
          unchanged: 0,
        } as PublishResult),
    });
    renderProjectDetail();
    await publishViaConfirm();
    expect(await screen.findByTestId('publishing-refusal')).toHaveTextContent(
      'Rig 2 publishes new frames to this project',
    );
  });

  it('a held-back reason that merely contains the phrase, with no publishingDevice, shows no refusal', async () => {
    mockCommands(boundElsewhere, {
      publish_collab_frames: () =>
        Promise.resolve({
          announced: 1,
          updated: 0,
          state: 'published',
          heldBack: [
            {
              frameId: 9,
              filename: 'L_0009.fits',
              reasons: ['Obs PC publishes new frames to this project — use "Publish from this device" to switch'],
              publishingDevice: null,
            },
          ],
          unchanged: 0,
        } as PublishResult),
    });
    renderProjectDetail();
    await publishViaConfirm();
    await waitFor(() =>
      expect(screen.queryByRole('button', { name: 'Publish' })).not.toBeInTheDocument(),
    );
    expect(screen.queryByTestId('publishing-refusal')).not.toBeInTheDocument();
  });

  it('switching after a refusal clears it', async () => {
    const switched = projectCard({ publishingDevice: { deviceId: 'dev-me', name: 'Laptop' }, publishingHere: true });
    mockCommands(boundElsewhere, {
      publish_collab_frames: () => Promise.reject('collab_publishing_device:Obs PC'),
      set_collab_publishing_device: () => Promise.resolve(switched),
    });
    renderProjectDetail();
    await publishViaConfirm();
    const refusal = await screen.findByTestId('publishing-refusal');
    fireEvent.click(within(refusal).getByRole('button', { name: 'Publish from this device' }));
    fireEvent.click(await screen.findByRole('button', { name: 'Switch' }));
    expect(await screen.findByText('Publishing from this device')).toBeInTheDocument();
    expect(screen.queryByTestId('publishing-refusal')).not.toBeInTheDocument();
  });
});

describe('ProjectDetail contribute header (F7, dead decision-C hint removed)', () => {
  it('auto_publish_switch_visible_without_receive', async () => {
    mockCommands(projectCard({ dataRole: 'send' }));
    renderProjectDetail();

    const toggle = await screen.findByRole('checkbox', { name: /Auto-publish my frames/ });
    // The fixture's `autoPublish` defaults to true; the click toggles it off.
    fireEvent.click(toggle);
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('set_project_auto_publish', {
        projectId: 'proj-1',
        enabled: false,
      }),
    );
  });

  it('never shows the dead "not available in this version" hint, even when every candidate fails only on calibration', async () => {
    mockCommands(projectCard(), {
      evaluate_collab_gate: () =>
        Promise.resolve(
          gateFixture({
            publishable: 0,
            rows: [
              {
                frameId: 1,
                filename: 'a.fits',
                fwhmArcsec: null,
                eccentricity: null,
                starsDetected: null,
                trailed: null,
                publishable: false,
                failures: ['not calibrated — 3 lights have no calibration links'],
              } as FrameGateRow,
            ],
          }),
        ),
    });
    renderProjectDetail();

    await screen.findByRole('button', { name: /Publish 0 passing frames/ });
    expect(screen.queryByText(/not available in this version/)).not.toBeInTheDocument();
  });
});

describe('ProjectDetail gate blockers (GateBlockers wiring)', () => {
  it('Analyze sends the blocked set\'s id as analyze_frame_set { frameSetId }', async () => {
    mockCommands(projectCard(), {
      evaluate_collab_gate: () =>
        Promise.resolve(
          gateFixture({ blockers: [{ kind: 'analyze', frames: 2, sets: [42], names: [] }] }),
        ),
    });
    renderProjectDetail();

    const btn = await screen.findByRole('button', { name: 'Analyze' });
    fireEvent.click(btn);
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('analyze_frame_set', { frameSetId: 42 }),
    );
  });

  it('Solve sends the failing rows\' ids as plate_solve_batch { frameIds }', async () => {
    mockCommands(projectCard(), {
      evaluate_collab_gate: () =>
        Promise.resolve(
          gateFixture({
            blockers: [{ kind: 'solve', frames: 1, sets: [], names: [] }],
            rows: [
              {
                frameId: 7,
                filename: 'a.fits',
                fwhmArcsec: null,
                eccentricity: null,
                starsDetected: null,
                trailed: null,
                publishable: false,
                failures: ['unknown pixel scale'],
              } as FrameGateRow,
            ],
          }),
        ),
    });
    renderProjectDetail();

    const btn = await screen.findByRole('button', { name: /Solve 1 frames?/ });
    fireEvent.click(btn);
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('plate_solve_batch', { frameIds: [7] }),
    );
  });

  it('"Open calibration" navigates to the set\'s /objects/<id>?tab=calibration', async () => {
    mockCommands(projectCard(), {
      evaluate_collab_gate: () =>
        Promise.resolve(
          gateFixture({ blockers: [{ kind: 'linkCalibration', frames: 3, sets: [99], names: [] }] }),
        ),
    });
    renderProjectDetail();

    const btn = await screen.findByRole('button', { name: 'Open calibration' });
    fireEvent.click(btn);
    const loc = await screen.findByTestId('location');
    expect(loc.textContent).toBe('/objects/99?tab=calibration');
  });

  it('re-fetches the gate when analysis-complete fires', async () => {
    mockCommands(projectCard());
    renderProjectDetail();
    await screen.findByRole('heading', { name: /M42 Mosaic/ });
    const before = vi.mocked(api.invoke).mock.calls.filter((c) => c[0] === 'evaluate_collab_gate').length;
    act(() => {
      listeners['analysis-complete']?.({
        frame_set_id: 42,
        analyzed: 2,
        skipped: 0,
        failed: 0,
        errors: [],
        cancelled: false,
      });
    });
    await waitFor(() => {
      const after = vi.mocked(api.invoke).mock.calls.filter((c) => c[0] === 'evaluate_collab_gate').length;
      expect(after).toBeGreaterThan(before);
    });
  });

  it('re-fetches the gate when plate-solve-complete fires, after this page started a solve', async () => {
    mockCommands(projectCard(), {
      evaluate_collab_gate: () =>
        Promise.resolve(
          gateFixture({
            blockers: [{ kind: 'solve', frames: 1, sets: [], names: [] }],
            rows: [
              {
                frameId: 7,
                filename: 'a.fits',
                fwhmArcsec: null,
                eccentricity: null,
                starsDetected: null,
                trailed: null,
                publishable: false,
                failures: ['unknown pixel scale'],
              } as FrameGateRow,
            ],
          }),
        ),
    });
    renderProjectDetail();
    const btn = await screen.findByRole('button', { name: /Solve 1 frames?/ });
    fireEvent.click(btn);
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('plate_solve_batch', { frameIds: [7] }),
    );
    const before = vi.mocked(api.invoke).mock.calls.filter((c) => c[0] === 'evaluate_collab_gate').length;
    act(() => {
      listeners['plate-solve-complete']?.({});
    });
    await waitFor(() => {
      const after = vi.mocked(api.invoke).mock.calls.filter((c) => c[0] === 'evaluate_collab_gate').length;
      expect(after).toBeGreaterThan(before);
    });
  });

  it('final-review minor: plate-solve-complete is a global event — it does not re-fetch this page\'s gate when this page never started a solve', async () => {
    mockCommands(projectCard());
    renderProjectDetail();
    await screen.findByRole('heading', { name: /M42 Mosaic/ });
    const before = vi.mocked(api.invoke).mock.calls.filter((c) => c[0] === 'evaluate_collab_gate').length;
    act(() => {
      listeners['plate-solve-complete']?.({});
    });
    // No `await waitFor` for a positive assertion here — give any (wrongly)
    // scheduled re-fetch a tick to land, then assert it did not.
    await new Promise((r) => setTimeout(r, 0));
    const after = vi.mocked(api.invoke).mock.calls.filter((c) => c[0] === 'evaluate_collab_gate').length;
    expect(after).toBe(before);
  });

  it('final-review minor: a failed Solve invoke notifies', async () => {
    mockCommands(projectCard(), {
      evaluate_collab_gate: () =>
        Promise.resolve(
          gateFixture({
            blockers: [{ kind: 'solve', frames: 1, sets: [], names: [] }],
            rows: [
              {
                frameId: 7,
                filename: 'a.fits',
                fwhmArcsec: null,
                eccentricity: null,
                starsDetected: null,
                trailed: null,
                publishable: false,
                failures: ['unknown pixel scale'],
              } as FrameGateRow,
            ],
          }),
        ),
      plate_solve_batch: () => Promise.reject(new Error('solver unavailable')),
    });
    renderProjectDetail();
    const btn = await screen.findByRole('button', { name: /Solve 1 frames?/ });
    fireEvent.click(btn);
    const toasts = await screen.findAllByRole('status');
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toHaveTextContent('Could not start the solve');
  });

  it('final-review minor: a failed Analyze invoke notifies', async () => {
    mockCommands(projectCard(), {
      evaluate_collab_gate: () =>
        Promise.resolve(
          gateFixture({ blockers: [{ kind: 'analyze', frames: 2, sets: [42], names: [] }] }),
        ),
      analyze_frame_set: () => Promise.reject(new Error('analysis unavailable')),
    });
    renderProjectDetail();
    const btn = await screen.findByRole('button', { name: 'Analyze' });
    fireEvent.click(btn);
    const toasts = await screen.findAllByRole('status');
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toHaveTextContent('Could not start the analysis');
  });
});

function ownFrame(): ProjectFrameView {
  return {
    frameUuid: 'f-1',
    fileName: 'c_L_0001.fits',
    own: true,
    state: 'published',
    contentVersion: 1,
    byteSize: 1024,
    holdersOnline: 1,
    holdersTotal: 1,
    localState: 'own_held',
    lastError: null,
    acceptedReason: null,
    contributorState: null,
    contributorReason: null,
    receivedAt: null,
    receivedFromDevice: null,
    receivedFromMember: null,
  } as ProjectFrameView;
}
