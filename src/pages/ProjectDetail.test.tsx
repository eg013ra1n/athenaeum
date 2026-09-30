import { describe, expect, it, vi, beforeEach } from 'vitest';
import { useEffect } from 'react';
import { render, screen, fireEvent, act, waitFor, within } from '@testing-library/react';
import { MemoryRouter, Routes, Route, useLocation, useNavigate } from 'react-router-dom';
import { NotificationProvider } from '../contexts/NotificationContext';
import { SessionStateProvider, useSessionState } from '../contexts/SessionStateContext';
import { NavHistoryProvider } from '../contexts/NavHistoryContext';
import { CollabExchangeProvider } from '../contexts/CollabExchangeContext';
import { ToastStack } from '../components/Toast';
import ProjectDetail from './ProjectDetail';
import { useCollabNotifications } from '../hooks/useCollabNotifications';
import { api } from '../api';
import type {
  OwnFrameRow,
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
    links: [{ framesSetId: 10, name: 'M42', lightCount: 2, distanceDeg: 0.1, withinRadius: true }],
    portalBase: 'https://hub.example',
    goals: null,
  };
}

function ownRow(o: Partial<OwnFrameRow> = {}): OwnFrameRow {
  return {
    frameId: 1,
    frameUuid: null,
    fileName: 'L_0001.fits',
    setId: 10,
    setName: 'M42',
    night: '2026-09-29',
    filter: 'Ha',
    filterMapped: true,
    camera: 'ASI2600MM Pro',
    exptimeSec: 300,
    byteSize: 42_000_000,
    fwhmArcsec: 2.4,
    eccentricity: 0.4,
    starsDetected: 1200,
    medianSnr: 18,
    segment: 'ready',
    contributorState: 'published',
    contributorReason: null,
    failures: [],
    contentVersion: null,
    pubState: null,
    acceptedReason: null,
    holdersOnline: null,
    holdersTotal: null,
    localState: null,
    publishedAt: null,
    lastError: null,
    rules: [],
    path: '/data/L_0001.fits',
    accepted: null,
    ...o,
  };
}

/** Two frames ready to publish — the default own-frames answer. */
const twoReady: OwnFrameRow[] = [
  ownRow({ frameId: 1, fileName: 'L_0001.fits' }),
  ownRow({ frameId: 2, fileName: 'L_0002.fits' }),
];

function published(frameId: number, o: Partial<OwnFrameRow> = {}): OwnFrameRow {
  return ownRow({
    frameId,
    frameUuid: `u-${frameId}`,
    fileName: `P_${String(frameId).padStart(4, '0')}.fits`,
    segment: 'published',
    pubState: 'published',
    contentVersion: 1,
    localState: 'own_held',
    holdersOnline: 1,
    holdersTotal: 1,
    accepted: true,
    ...o,
  });
}

function heldRow(frameId: number, kind: string, text: string, o: Partial<OwnFrameRow> = {}): OwnFrameRow {
  return ownRow({
    frameId,
    fileName: `H_${String(frameId).padStart(4, '0')}.fits`,
    segment: 'held',
    failures: [{ kind, text }],
    ...o,
  });
}

function libraryFrame(o: Partial<ProjectFrameView> = {}): ProjectFrameView {
  return {
    frameUuid: 'lib-1',
    fileName: 'alice_001.fits',
    publisher: 'Alice',
    publisherAccountId: 'acc-a',
    own: false,
    filter: 'L',
    exptimeSec: 120,
    dateObs: null,
    state: 'published',
    accepted: true,
    acceptedReason: null,
    localState: 'held',
    onDisk: true,
    holdersOnline: 2,
    holdersTotal: 3,
    waitingForPublisher: false,
    newVersionWaiting: false,
    byteSize: 1024,
    contentVersion: 1,
    lastError: null,
    fwhmArcsec: 2.1,
    eccentricity: 0.3,
    starsDetected: 500,
    camera: null,
    telescope: null,
    night: null,
    medianSnr: null,
    contributorState: null,
    contributorReason: null,
    receivedAt: null,
    receivedFromDevice: null,
    receivedFromMember: null,
    ...o,
  };
}

const okPublish: PublishResult = {
  announced: 2,
  updated: 0,
  state: 'published',
  heldBack: [],
  unchanged: 0,
} as PublishResult;

/** Every `api.listen` registration this render made, by event name — ALL of
 *  them: the page and the app-root `useCollabNotifications` both listen to
 *  `collab-published`. `fire` delivers a payload to every one. */
const listeners: Record<string, ((payload: unknown) => void)[]> = {};

function fire(event: string, payload: unknown) {
  act(() => {
    for (const cb of listeners[event] ?? []) cb(payload);
  });
}

/** The default command answers, with `extra` taking precedence per command. */
function mockCommands(
  card: ProjectCard = projectCard(),
  extra: Record<string, (args?: unknown) => Promise<unknown>> = {},
) {
  vi.mocked(api.invoke).mockImplementation(((command: string, args?: unknown) => {
    if (extra[command]) return extra[command](args);
    switch (command) {
      case 'get_collab_project_detail':
        return Promise.resolve(detailFixture(card));
      case 'list_project_own_frames':
        return Promise.resolve(twoReady);
      case 'list_collab_frames':
        return Promise.resolve([] as ProjectFrameView[]);
      case 'get_collab_member_summary':
        return Promise.resolve([]);
      case 'get_collab_exchange':
        return Promise.resolve({ projects: [], names: [] });
      case 'list_collab_receive_sessions':
        return Promise.resolve([]);
      case 'list_collab_moderation':
        return Promise.resolve([]);
      case 'list_collab_attention':
        return Promise.resolve({ changed: [], awaitingChoice: [], notKept: [], otherFiles: [] });
      case 'get_collaboration_dir':
        return Promise.resolve('/collab');
      case 'list_collab_projects':
        return Promise.resolve([card]);
      case 'publish_collab_frames':
        return Promise.resolve(okPublish);
      case 'republish_collab_frames':
        return Promise.resolve({ ...okPublish, announced: 0, updated: 2 });
      default:
        return Promise.resolve(null);
    }
  }) as never);
}

beforeEach(() => {
  for (const k of Object.keys(listeners)) delete listeners[k];
  localStorage.clear();
  vi.mocked(api.invoke).mockReset();
  mockCommands();
  vi.mocked(api.listen).mockImplementation((<T,>(event: string, cb: (p: T) => void) => {
    (listeners[event] ??= []).push(cb as unknown as (payload: unknown) => void);
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

/** Renders the current location, so a test can assert the exact path +
 *  query a navigation landed on (or that `?tab=` was stripped). */
function LocationDisplay() {
  const location = useLocation();
  return <div data-testid="location">{location.pathname}{location.search}</div>;
}

/** Stores a legacy tab value in the session, then opens the project — the
 *  shape a session that last visited the old four-tab page leaves behind. */
function SeedLegacyTab({ value }: { value: string }) {
  const [, setTab] = useSessionState<string>('projectDetail.tab', 'overview');
  const navigate = useNavigate();
  useEffect(() => {
    setTab(value);
    navigate('/projects/proj-1');
  }, [navigate, setTab, value]);
  return null;
}

function renderProjectDetail(entry = '/projects/proj-1', seed?: string) {
  return render(
    <MemoryRouter initialEntries={[entry]}>
      <NavHistoryProvider>
        <SessionStateProvider>
          <NotificationProvider>
            <CollabExchangeProvider>
              <CollabNotificationsHarness />
              <Routes>
                <Route
                  path="/projects/:id"
                  element={
                    <>
                      <ProjectDetail />
                      <LocationDisplay />
                    </>
                  }
                />
                <Route path="/objects/:id" element={<LocationDisplay />} />
                <Route path="/seed" element={<SeedLegacyTab value={seed ?? 'overview'} />} />
              </Routes>
              <ToastStack />
            </CollabExchangeProvider>
          </NotificationProvider>
        </SessionStateProvider>
      </NavHistoryProvider>
    </MemoryRouter>,
  );
}

async function openTab(name: RegExp) {
  fireEvent.click(await screen.findByRole('tab', { name }));
}

/** My frames → "Publish all 2" → the confirm's Publish. */
async function publishViaConfirm() {
  await openTab(/^My frames/);
  fireEvent.click(await screen.findByRole('button', { name: 'Publish all 2' }));
  fireEvent.click(await screen.findByRole('button', { name: 'Publish' }));
}

function invokeCount(command: string): number {
  return vi.mocked(api.invoke).mock.calls.filter(([c]) => c === command).length;
}

describe('ProjectDetail manual publish', () => {
  it('raises no inline toast of its own; the live collab-published event produces exactly one', async () => {
    renderProjectDetail();
    await publishViaConfirm();

    // publish resolves, closes the confirm dialog, then reloads own frames + detail.
    await waitFor(() =>
      expect(screen.queryByRole('button', { name: 'Publish' })).not.toBeInTheDocument(),
    );
    expect(api.invoke).toHaveBeenCalledWith('publish_collab_frames', { projectId: 'proj-1', frameIds: [1, 2] });
    await waitFor(() => expect(invokeCount('list_project_own_frames')).toBeGreaterThanOrEqual(2));

    expect(listeners['collab-published']?.length ?? 0).toBeGreaterThan(0);
    // The invoke resolved successfully — ProjectDetail itself raises no toast.
    expect(screen.queryAllByRole('status')).toHaveLength(0);

    // The backend always emits `collab-published` on a successful publish;
    // the live listener (`useCollabNotifications`) is the one place that
    // turns it into a toast.
    fire('collab-published', { projectId: 'proj-1', announced: 2, updated: 0, heldBack: 0 });

    const toasts = await screen.findAllByRole('status');
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toHaveTextContent('Published 2 frames in M42 Mosaic');
  });

  it('the confirm counts the requested frames and estimates their size', async () => {
    renderProjectDetail();
    await openTab(/^My frames/);
    fireEvent.click(await screen.findByRole('button', { name: 'Publish all 2' }));
    expect(await screen.findByText('Publish to M42 Mosaic')).toBeInTheDocument();
    expect(
      screen.getByText('2 passing frames will be calibrated and announced to the project.'),
    ).toBeInTheDocument();
    expect(screen.getByText(/Estimated size ≈ 90\.0 MB/)).toBeInTheDocument();
  });

  it('a failed publish shows an inline error and a toast whose dedupeKey cannot collide with the live one', async () => {
    mockCommands(projectCard(), {
      publish_collab_frames: () => Promise.reject(new Error('hub unreachable')),
    });
    renderProjectDetail();
    await publishViaConfirm();

    expect(await screen.findByText('hub unreachable')).toBeInTheDocument();

    const toasts = await screen.findAllByRole('status');
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toHaveTextContent('Publish failed');
  });

  it('a publish refused because another run is in progress reads as "already running", not a failure', async () => {
    mockCommands(projectCard(), {
      // Both hosts reject with the backend's message as a plain string.
      publish_collab_frames: () => Promise.reject('publication of this project is already running'),
    });
    renderProjectDetail();
    await publishViaConfirm();

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

const obsPc = { deviceId: 'dev-obs', name: 'Obs PC' };
const boundElsewhere = projectCard({ publishingDevice: obsPc, publishingHere: false });
const boundHere = projectCard({ publishingDevice: { deviceId: 'dev-me', name: 'Laptop' }, publishingHere: true });

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
    expect(invokeCount('get_collab_project_detail')).toBe(1);
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
      list_project_own_frames: () => Promise.resolve([published(3)]),
      republish_collab_frames: () => Promise.reject('collab_publishing_device:Obs PC'),
    });
    renderProjectDetail();
    await openTab(/^My frames/);
    fireEvent.click(await screen.findByRole('button', { name: /Recalibrate and republish all/ }));
    fireEvent.change(screen.getByLabelText('Type 1 to confirm'), { target: { value: '1' } });
    fireEvent.click(screen.getByRole('button', { name: 'Republish' }));
    expect(await screen.findByTestId('publishing-refusal')).toHaveTextContent(
      'Obs PC publishes new frames to this project',
    );
    expect(screen.queryByText('Republish failed')).not.toBeInTheDocument();
    // The guard closed on the refusal.
    expect(screen.queryByRole('button', { name: 'Republish' })).not.toBeInTheDocument();
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

describe('ProjectDetail republish guard', () => {
  const fivePublished = [published(11), published(12), published(13), published(14), published(15)];

  it('"Recalibrate and republish all" needs the typed count, then sends the published ids explicitly (never null)', async () => {
    mockCommands(projectCard(), { list_project_own_frames: () => Promise.resolve(fivePublished) });
    renderProjectDetail();
    await openTab(/^My frames/);
    fireEvent.click(await screen.findByRole('button', { name: /Recalibrate and republish all/ }));

    expect(screen.getByRole('heading', { name: 'Recalibrate and republish all' })).toBeInTheDocument();
    expect(screen.getByText(/^5 frames · 200\.3 MB of source frames will be recalibrated\./)).toBeInTheDocument();
    const confirm = screen.getByRole('button', { name: 'Republish' });
    expect(confirm).toBeDisabled();

    const input = screen.getByLabelText('Type 5 to confirm');
    fireEvent.change(input, { target: { value: '4' } });
    expect(confirm).toBeDisabled();
    fireEvent.change(input, { target: { value: ' 5 ' } });
    expect(confirm).toBeEnabled();

    fireEvent.click(confirm);
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('republish_collab_frames', {
        projectId: 'proj-1',
        frameIds: [11, 12, 13, 14, 15],
      }),
    );
    await waitFor(() => expect(screen.queryByRole('button', { name: 'Republish' })).not.toBeInTheDocument());
  });

  it('"all" counts only the published frames that are not excluded', async () => {
    mockCommands(projectCard(), {
      list_project_own_frames: () =>
        Promise.resolve([published(21), published(22), published(23, { accepted: false, acceptedReason: 'trailed' })]),
    });
    renderProjectDetail();
    await openTab(/^My frames/);
    fireEvent.click(await screen.findByRole('button', { name: /Recalibrate and republish all/ }));
    fireEvent.change(screen.getByLabelText('Type 2 to confirm'), { target: { value: '2' } });
    fireEvent.click(screen.getByRole('button', { name: 'Republish' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('republish_collab_frames', { projectId: 'proj-1', frameIds: [21, 22] }),
    );
  });

  it('"all" never announces Ready frames: with 2 published and 1 ready it sends only the 2 published ids', async () => {
    mockCommands(projectCard(), {
      list_project_own_frames: () =>
        Promise.resolve([published(61), published(62), ownRow({ frameId: 63, fileName: 'L_0063.fits' })]),
    });
    renderProjectDetail();
    await openTab(/^My frames/);
    fireEvent.click(await screen.findByRole('button', { name: /Recalibrate and republish all/ }));
    expect(screen.getByRole('heading', { name: 'Recalibrate and republish all' })).toBeInTheDocument();
    fireEvent.change(screen.getByLabelText('Type 2 to confirm'), { target: { value: '2' } });
    fireEvent.click(screen.getByRole('button', { name: 'Republish' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('republish_collab_frames', { projectId: 'proj-1', frameIds: [61, 62] }),
    );
    expect(
      vi.mocked(api.invoke).mock.calls.filter(
        ([c, a]) => c === 'republish_collab_frames' && (a as { frameIds: unknown }).frameIds === null,
      ),
    ).toHaveLength(0);
  });

  it('a 2-frame selection confirms without typing and sends exactly those ids', async () => {
    mockCommands(projectCard(), {
      list_project_own_frames: () => Promise.resolve([published(31), published(32)]),
    });
    renderProjectDetail();
    await openTab(/^My frames/);
    fireEvent.click(await screen.findByRole('button', { name: /^Published 2/ }));
    fireEvent.click(await screen.findByRole('checkbox', { name: 'Select all shown' }));
    fireEvent.click(screen.getByRole('button', { name: 'Republish 2' }));

    expect(screen.getByRole('heading', { name: 'Republish 2 frames' })).toBeInTheDocument();
    expect(screen.queryByLabelText(/to confirm/)).not.toBeInTheDocument();
    const confirm = screen.getByRole('button', { name: 'Republish' });
    expect(confirm).toBeEnabled();
    fireEvent.click(confirm);
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('republish_collab_frames', { projectId: 'proj-1', frameIds: [31, 32] }),
    );
  });

  it('a republish refused because another run is in progress reads as "already running" inside the guard', async () => {
    mockCommands(projectCard(), {
      list_project_own_frames: () => Promise.resolve([published(41)]),
      republish_collab_frames: () => Promise.reject('publication of this project is already running'),
    });
    renderProjectDetail();
    await openTab(/^My frames/);
    fireEvent.click(await screen.findByRole('button', { name: /Recalibrate and republish all/ }));
    fireEvent.change(screen.getByLabelText('Type 1 to confirm'), { target: { value: '1' } });
    fireEvent.click(screen.getByRole('button', { name: 'Republish' }));

    expect(
      await screen.findByText(
        'Publication of this project is already running — wait for it to finish, then try again.',
      ),
    ).toBeInTheDocument();
    // Still inside the guard (it stays open on a busy refusal).
    expect(screen.getByRole('button', { name: 'Republish' })).toBeInTheDocument();
    const toasts = await screen.findAllByRole('status');
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toHaveTextContent('Publication already running');
    expect(toasts[0]).not.toHaveTextContent('Republish failed');
  });

  it('a failed republish shows the error inside the guard and one "Republish failed" toast', async () => {
    mockCommands(projectCard(), {
      list_project_own_frames: () => Promise.resolve([published(51)]),
      republish_collab_frames: () => Promise.reject(new Error('hub unreachable')),
    });
    renderProjectDetail();
    await openTab(/^My frames/);
    fireEvent.click(await screen.findByRole('button', { name: /Recalibrate and republish all/ }));
    fireEvent.change(screen.getByLabelText('Type 1 to confirm'), { target: { value: '1' } });
    fireEvent.click(screen.getByRole('button', { name: 'Republish' }));
    expect(await screen.findByText('hub unreachable')).toBeInTheDocument();
    const toasts = await screen.findAllByRole('status');
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toHaveTextContent('Republish failed');
  });
});

describe('ProjectDetail My frames header (F7, dead decision-C hint removed)', () => {
  it('auto_publish_switch_visible_without_receive', async () => {
    mockCommands(projectCard({ dataRole: 'send' }));
    renderProjectDetail();
    await openTab(/^My frames/);

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

  it('never shows the dead "not available in this version" hint, even when every frame is held back only on calibration', async () => {
    mockCommands(projectCard(), {
      list_project_own_frames: () =>
        Promise.resolve([heldRow(1, 'linkCalibration', 'not calibrated — 3 lights have no calibration links')]),
    });
    renderProjectDetail();
    await openTab(/^My frames/);
    fireEvent.click(await screen.findByRole('button', { name: /^Held back 1/ }));
    await screen.findByRole('button', { name: 'Open calibration' });
    expect(screen.queryByText(/not available in this version/)).not.toBeInTheDocument();
  });
});

describe('ProjectDetail Held back fixes (My frames wiring)', () => {
  async function openHeld(count: number) {
    await openTab(/^My frames/);
    fireEvent.click(await screen.findByRole('button', { name: new RegExp(`^Held back ${count}`) }));
  }

  it("Analyze sends the held set's id as analyze_frame_set { frameSetId }", async () => {
    mockCommands(projectCard(), {
      list_project_own_frames: () => Promise.resolve([heldRow(5, 'analyze', 'no analysis', { setId: 42 })]),
    });
    renderProjectDetail();
    await openHeld(1);

    fireEvent.click(await screen.findByRole('button', { name: 'Analyze' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('analyze_frame_set', { frameSetId: 42 }),
    );
  });

  it("Solve sends the held rows' ids as plate_solve_batch { frameIds }", async () => {
    mockCommands(projectCard(), {
      list_project_own_frames: () => Promise.resolve([heldRow(7, 'solve', 'unknown pixel scale')]),
    });
    renderProjectDetail();
    await openHeld(1);

    fireEvent.click(await screen.findByRole('button', { name: 'Solve 1' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('plate_solve_batch', { frameIds: [7] }),
    );
  });

  it('"Open calibration" navigates to the set\'s /objects/<id>?tab=calibration', async () => {
    mockCommands(projectCard(), {
      list_project_own_frames: () =>
        Promise.resolve([heldRow(8, 'linkCalibration', 'no calibration linked', { setId: 99 })]),
    });
    renderProjectDetail();
    await openHeld(1);

    fireEvent.click(await screen.findByRole('button', { name: 'Open calibration' }));
    await waitFor(() => expect(screen.getByTestId('location').textContent).toBe('/objects/99?tab=calibration'));
  });

  it('re-fetches own frames when analysis-complete fires', async () => {
    renderProjectDetail();
    await openTab(/^My frames/);
    await screen.findByRole('button', { name: 'Publish all 2' });
    const before = invokeCount('list_project_own_frames');
    fire('analysis-complete', {
      frame_set_id: 42,
      analyzed: 2,
      skipped: 0,
      failed: 0,
      errors: [],
      cancelled: false,
    });
    await waitFor(() => expect(invokeCount('list_project_own_frames')).toBeGreaterThan(before));
  });

  it('re-fetches own frames when plate-solve-complete fires, after this page started a solve', async () => {
    mockCommands(projectCard(), {
      list_project_own_frames: () => Promise.resolve([heldRow(7, 'solve', 'unknown pixel scale')]),
    });
    renderProjectDetail();
    await openHeld(1);
    fireEvent.click(await screen.findByRole('button', { name: 'Solve 1' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('plate_solve_batch', { frameIds: [7] }),
    );
    const before = invokeCount('list_project_own_frames');
    fire('plate-solve-complete', {});
    await waitFor(() => expect(invokeCount('list_project_own_frames')).toBeGreaterThan(before));
  });

  it("final-review minor: plate-solve-complete is a global event — it does not re-fetch this page's frames when this page never started a solve", async () => {
    renderProjectDetail();
    await openTab(/^My frames/);
    await screen.findByRole('button', { name: 'Publish all 2' });
    const before = invokeCount('list_project_own_frames');
    fire('plate-solve-complete', {});
    // No `await waitFor` for a positive assertion here — give any (wrongly)
    // scheduled re-fetch a tick to land, then assert it did not.
    await new Promise((r) => setTimeout(r, 0));
    expect(invokeCount('list_project_own_frames')).toBe(before);
  });

  it('final-review minor: a failed Solve invoke notifies', async () => {
    mockCommands(projectCard(), {
      list_project_own_frames: () => Promise.resolve([heldRow(7, 'solve', 'unknown pixel scale')]),
      plate_solve_batch: () => Promise.reject(new Error('solver unavailable')),
    });
    renderProjectDetail();
    await openHeld(1);
    fireEvent.click(await screen.findByRole('button', { name: 'Solve 1' }));
    const toasts = await screen.findAllByRole('status');
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toHaveTextContent('Could not start the solve');
  });

  it('final-review minor: a failed Analyze invoke notifies', async () => {
    mockCommands(projectCard(), {
      list_project_own_frames: () => Promise.resolve([heldRow(5, 'analyze', 'no analysis', { setId: 42 })]),
      analyze_frame_set: () => Promise.reject(new Error('analysis unavailable')),
    });
    renderProjectDetail();
    await openHeld(1);
    fireEvent.click(await screen.findByRole('button', { name: 'Analyze' }));
    const toasts = await screen.findAllByRole('status');
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toHaveTextContent('Could not start the analysis');
  });
});

describe('ProjectDetail tabs', () => {
  function tabNames(): string[] {
    return screen.getAllByRole('tab').map((t) => t.textContent ?? '');
  }

  it('opens on Overview by default, with the six tabs in order for a coordinator of an approval project', async () => {
    mockCommands(projectCard({ coordinator: true, requireApproval: true }));
    renderProjectDetail();
    const overview = await screen.findByRole('tab', { name: 'Overview' });
    expect(overview).toHaveAttribute('aria-selected', 'true');
    expect(tabNames()).toEqual(['Overview', 'My frames 2', 'Library', 'Members', 'Exchange', 'Moderation']);
  });

  it('a contributor (send only, not coordinator) sees no Library tab', async () => {
    mockCommands(projectCard({ dataRole: 'send', coordinator: false }));
    renderProjectDetail();
    await screen.findByRole('tab', { name: 'Overview' });
    expect(screen.queryByRole('tab', { name: /^Library/ })).not.toBeInTheDocument();
    expect(screen.getByRole('tab', { name: /^My frames/ })).toBeInTheDocument();
  });

  it('Moderation shows only for a coordinator of a project that requires approval, with the pending badge', async () => {
    mockCommands(projectCard({ coordinator: true, requireApproval: true, pendingFrames: 3 }));
    const first = renderProjectDetail();
    expect(await screen.findByRole('tab', { name: 'Moderation 3' })).toBeInTheDocument();
    first.unmount();

    mockCommands(projectCard({ coordinator: true, requireApproval: false }));
    const second = renderProjectDetail();
    await screen.findByRole('tab', { name: 'Overview' });
    expect(screen.queryByRole('tab', { name: /^Moderation/ })).not.toBeInTheDocument();
    second.unmount();

    mockCommands(projectCard({ coordinator: false, requireApproval: true }));
    renderProjectDetail();
    await screen.findByRole('tab', { name: 'Overview' });
    expect(screen.queryByRole('tab', { name: /^Moderation/ })).not.toBeInTheDocument();
  });

  it('the My frames badge shows the ready count', async () => {
    mockCommands(projectCard(), {
      list_project_own_frames: () =>
        Promise.resolve([...twoReady, ownRow({ frameId: 3, fileName: 'L_0003.fits' }), published(4)]),
    });
    renderProjectDetail();
    expect(await screen.findByRole('tab', { name: 'My frames 3' })).toBeInTheDocument();
  });

  it('the Library badge counts the library frames still to come on this device', async () => {
    mockCommands(projectCard(), {
      list_collab_frames: () =>
        Promise.resolve([
          libraryFrame({ frameUuid: 'a', localState: 'wanted' }),
          libraryFrame({ frameUuid: 'b', localState: 'held' }),
          libraryFrame({ frameUuid: 'c', localState: 'wanted', own: true }),
        ]),
    });
    renderProjectDetail();
    expect(await screen.findByRole('tab', { name: 'Library 1' })).toBeInTheDocument();
  });

  it('?tab=receive opens Library and is removed from the URL', async () => {
    renderProjectDetail('/projects/proj-1?tab=receive');
    const library = await screen.findByRole('tab', { name: /^Library/ });
    await waitFor(() => expect(library).toHaveAttribute('aria-selected', 'true'));
    expect(await screen.findByText('Received frames')).toBeInTheDocument();
    await waitFor(() => expect(screen.getByTestId('location').textContent).toBe('/projects/proj-1'));
  });

  it('?tab=contribute opens My frames', async () => {
    renderProjectDetail('/projects/proj-1?tab=contribute');
    const mine = await screen.findByRole('tab', { name: /^My frames/ });
    await waitFor(() => expect(mine).toHaveAttribute('aria-selected', 'true'));
    expect(await screen.findByRole('button', { name: 'Publish all 2' })).toBeInTheDocument();
  });

  it('?tab=members and ?tab=exchange open their tabs', async () => {
    const first = renderProjectDetail('/projects/proj-1?tab=members');
    await waitFor(() =>
      expect(screen.getByRole('tab', { name: 'Members' })).toHaveAttribute('aria-selected', 'true'),
    );
    first.unmount();
    renderProjectDetail('/projects/proj-1?tab=exchange');
    await waitFor(() =>
      expect(screen.getByRole('tab', { name: 'Exchange' })).toHaveAttribute('aria-selected', 'true'),
    );
  });

  it('a deep link to a tab this member cannot see falls back to Overview', async () => {
    mockCommands(projectCard({ dataRole: 'send', coordinator: false }));
    renderProjectDetail('/projects/proj-1?tab=receive');
    const overview = await screen.findByRole('tab', { name: 'Overview' });
    expect(overview).toHaveAttribute('aria-selected', 'true');
  });

  it('a legacy stored tab ("contribute") from the old page maps to My frames', async () => {
    renderProjectDetail('/seed', 'contribute');
    const mine = await screen.findByRole('tab', { name: /^My frames/ });
    await waitFor(() => expect(mine).toHaveAttribute('aria-selected', 'true'));
  });

  it('an Overview segment button opens My frames on that segment', async () => {
    mockCommands(projectCard(), {
      list_project_own_frames: () => Promise.resolve([heldRow(7, 'solve', 'unknown pixel scale')]),
    });
    renderProjectDetail();
    fireEvent.click(await screen.findByRole('button', { name: 'Held back 1' }));
    await waitFor(() =>
      expect(screen.getByRole('tab', { name: /^My frames/ })).toHaveAttribute('aria-selected', 'true'),
    );
    expect(await screen.findByRole('button', { name: 'Solve 1' })).toBeInTheDocument();
  });

  it('a project missing from the local list says so', async () => {
    mockCommands(projectCard(), {
      get_collab_project_detail: () => Promise.reject(new Error('not found')),
    });
    renderProjectDetail();
    expect(
      await screen.findByText('This project is not in your local list — refresh the Projects page.'),
    ).toBeInTheDocument();
  });

  it('the page no longer evaluates the project gate', async () => {
    renderProjectDetail();
    await openTab(/^My frames/);
    await screen.findByRole('button', { name: 'Publish all 2' });
    expect(invokeCount('evaluate_collab_gate')).toBe(0);
    expect(api.invoke).toHaveBeenCalledWith('list_project_own_frames', { projectId: 'proj-1' });
    expect(api.invoke).toHaveBeenCalledWith('list_collab_frames', { projectId: 'proj-1' });
  });
});

describe('ProjectDetail fix round 1', () => {
  it('collab-published for this project (e.g. an auto-publish) re-reads own frames, the library and the detail', async () => {
    renderProjectDetail();
    await openTab(/^My frames/);
    await screen.findByRole('button', { name: 'Publish all 2' });
    await waitFor(() => expect(listeners['collab-published']?.length ?? 0).toBeGreaterThanOrEqual(2));
    const own = invokeCount('list_project_own_frames');
    const lib = invokeCount('list_collab_frames');
    const det = invokeCount('get_collab_project_detail');

    fire('collab-published', { projectId: 'proj-1', announced: 3, updated: 0, heldBack: 0 });
    await waitFor(() => {
      expect(invokeCount('list_project_own_frames')).toBeGreaterThan(own);
      expect(invokeCount('list_collab_frames')).toBeGreaterThan(lib);
      expect(invokeCount('get_collab_project_detail')).toBeGreaterThan(det);
    });
  });

  it('collab-published for another project re-reads nothing', async () => {
    renderProjectDetail();
    await openTab(/^My frames/);
    await screen.findByRole('button', { name: 'Publish all 2' });
    await waitFor(() => expect(listeners['collab-published']?.length ?? 0).toBeGreaterThanOrEqual(2));
    const own = invokeCount('list_project_own_frames');
    const lib = invokeCount('list_collab_frames');
    const det = invokeCount('get_collab_project_detail');

    fire('collab-published', { projectId: 'proj-other', announced: 3, updated: 0, heldBack: 0 });
    await new Promise((r) => setTimeout(r, 0));
    expect(invokeCount('list_project_own_frames')).toBe(own);
    expect(invokeCount('list_collab_frames')).toBe(lib);
    expect(invokeCount('get_collab_project_detail')).toBe(det);
  });

  it('an own-frames failure on Overview shows the error, and My frames / Needs attention stop saying Loading…', async () => {
    mockCommands(projectCard(), {
      list_project_own_frames: () => Promise.reject(new Error('catalog locked')),
    });
    renderProjectDetail();
    expect(await screen.findByText('Could not load your frames — see console.')).toBeInTheDocument();
    const myFrames = screen.getByRole('heading', { name: 'My frames' }).parentElement!;
    expect(within(myFrames).queryByText('Loading…')).not.toBeInTheDocument();
    const attention = screen.getByRole('heading', { name: 'Needs attention' }).parentElement!;
    expect(within(attention).queryByText('Loading…')).not.toBeInTheDocument();
    expect(within(attention).queryByText('Nothing needs attention.')).not.toBeInTheDocument();
  });
});

function GoToProject2() {
  const navigate = useNavigate();
  return (
    <button type="button" onClick={() => navigate('/projects/proj-2')}>
      go to project 2
    </button>
  );
}

describe('ProjectDetail project switch', () => {
  it("navigating to another project never carries the previous project's frames, drawer or confirm over", async () => {
    mockCommands(projectCard(), {
      get_collab_project_detail: (args) =>
        Promise.resolve(
          detailFixture(
            (args as { projectId: string }).projectId === 'proj-2'
              ? projectCard({ projectId: 'proj-2', title: 'M31 Deep' })
              : projectCard(),
          ),
        ),
      list_project_own_frames: (args) =>
        Promise.resolve(
          (args as { projectId: string }).projectId === 'proj-2'
            ? [ownRow({ frameId: 9, fileName: 'M31_0009.fits' })]
            : twoReady,
        ),
    });
    render(
      <MemoryRouter initialEntries={['/projects/proj-1']}>
        <NavHistoryProvider>
          <SessionStateProvider>
            <NotificationProvider>
              <CollabExchangeProvider>
                <Routes>
                  <Route
                    path="/projects/:id"
                    element={
                      <>
                        <GoToProject2 />
                        <ProjectDetail />
                      </>
                    }
                  />
                </Routes>
              </CollabExchangeProvider>
            </NotificationProvider>
          </SessionStateProvider>
        </NavHistoryProvider>
      </MemoryRouter>,
    );
    await openTab(/^My frames/);
    fireEvent.click(await screen.findByText('L_0001.fits'));
    expect(await screen.findByRole('complementary')).toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'go to project 2' }));
    expect(await screen.findByRole('heading', { name: 'M31 Deep' })).toBeInTheDocument();
    expect(await screen.findByRole('button', { name: 'Publish all 1' })).toBeInTheDocument();
    expect(screen.queryByRole('complementary')).not.toBeInTheDocument();
    expect(screen.queryByText('L_0001.fits')).not.toBeInTheDocument();
  });
});

describe('ProjectDetail frame drawer', () => {
  it('clicking a row opens the drawer and Escape closes it', async () => {
    renderProjectDetail();
    await openTab(/^My frames/);
    fireEvent.click(await screen.findByText('L_0001.fits'));

    const drawer = await screen.findByRole('complementary');
    expect(within(drawer).getByRole('heading', { name: 'L_0001.fits' })).toBeInTheDocument();

    fireEvent.keyDown(document, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByRole('complementary')).not.toBeInTheDocument());
  });

  it("a coordinator's own Restore refreshes the still-open drawer — no stale Excluded box, offers Exclude…", async () => {
    let calls = 0;
    mockCommands(projectCard({ coordinator: true }), {
      list_project_own_frames: () => {
        calls += 1;
        return Promise.resolve([
          published(
            23,
            calls === 1 ? { accepted: false, acceptedReason: 'trailed' } : { accepted: true, acceptedReason: null },
          ),
        ]);
      },
      get_collab_frame_holders: () => Promise.resolve([]),
      restore_collab_frame: () => Promise.resolve(undefined),
    });
    renderProjectDetail();
    await openTab(/^My frames/);
    fireEvent.click(await screen.findByRole('button', { name: /^Published 1/ }));
    fireEvent.click(await screen.findByText('P_0023.fits'));

    const drawer = await screen.findByRole('complementary');
    expect(within(drawer).getByText(/Excluded — trailed/)).toBeInTheDocument();
    expect(within(drawer).queryByRole('button', { name: 'Exclude…' })).not.toBeInTheDocument();

    fireEvent.click(within(drawer).getByRole('button', { name: 'Restore' }));

    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('restore_collab_frame', { projectId: 'proj-1', frameUuid: 'u-23' }),
    );
    // The reload answers `accepted: true` — the drawer, kept open on the same
    // frame (same key, no remount), must reflect it: the Excluded box is
    // gone and the coordinator now sees Exclude… instead.
    await waitFor(() => expect(within(drawer).queryByText(/Excluded — trailed/)).not.toBeInTheDocument());
    expect(within(drawer).getByRole('button', { name: 'Exclude…' })).toBeInTheDocument();
    expect(calls).toBe(2);
  });
});
