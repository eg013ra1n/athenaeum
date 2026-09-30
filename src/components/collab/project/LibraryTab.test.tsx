import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { CollabExchangeProvider } from '../../../contexts/CollabExchangeContext';
import { NotificationProvider } from '../../../contexts/NotificationContext';
import { SessionStateProvider } from '../../../contexts/SessionStateContext';
import { api } from '../../../api';
import LibraryTab, { libraryToCome } from './LibraryTab';
import type { ExchangeSnapshot, FlowView, ProjectFlows, ProjectFrameView } from '../../../types/models';
import type { FrameVM } from './frames';

vi.mock('../../../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
}));

afterEach(cleanup);

function frame(overrides: Partial<ProjectFrameView> = {}): ProjectFrameView {
  return {
    frameUuid: 'u-1',
    fileName: 'light_001.fits',
    publisher: 'Alice',
    publisherAccountId: 'acc',
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
    ...overrides,
  };
}

function flow(overrides: Partial<FlowView> = {}): FlowView {
  return {
    projectId: 'proj-1',
    device: 'devA',
    direction: 'recv',
    bytesSession: 0,
    rateBps: 0,
    etaSecs: null,
    moving: true,
    completed: 0,
    inFlight: [],
    ...overrides,
  };
}

function projectFlows(overrides: Partial<ProjectFlows> = {}): ProjectFlows {
  return {
    projectId: 'proj-1',
    recv: [],
    send: [],
    toGo: 0,
    waitingForPublisher: null,
    ...overrides,
  };
}

const EMPTY_SNAPSHOT: ExchangeSnapshot = { projects: [], names: [] };

/** Every registered listener per event (a child and its parent may both listen). */
let listeners: Record<string, ((p: unknown) => void)[]> = {};
const fire = (event: string, payload: unknown) => act(() => (listeners[event] ?? []).forEach((h) => h(payload)));

/** Mutable per-test override for the `get_collab_exchange` answer — set it
 *  before `renderTab` when a test needs live in-flight data. */
let exchangeSnapshot: ExchangeSnapshot = EMPTY_SNAPSHOT;

beforeEach(() => {
  listeners = {};
  exchangeSnapshot = EMPTY_SNAPSHOT;
  localStorage.clear();
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((command: string) => {
    switch (command) {
      case 'get_collaboration_dir':
        return Promise.resolve('/collab');
      case 'list_collab_attention':
        return Promise.resolve({ changed: [], awaitingChoice: [], notKept: [], otherFiles: [] });
      case 'get_collab_exchange':
        return Promise.resolve(exchangeSnapshot);
      default:
        return Promise.resolve(null);
    }
  }) as never);
  vi.mocked(api.listen).mockImplementation((<T,>(event: string, cb: (p: T) => void) => {
    (listeners[event] ??= []).push(cb as unknown as (p: unknown) => void);
    return Promise.resolve(() => {});
  }) as never);
});

const navigateMock = vi.fn();
vi.mock('react-router-dom', async (orig) => ({
  ...(await orig<typeof import('react-router-dom')>()),
  useNavigate: () => navigateMock,
}));

function renderTab(
  frames: ProjectFrameView[] | null,
  overrides: { error?: boolean; canModerate?: boolean; onOpen?: (vm: FrameVM) => void } = {},
) {
  const reload = vi.fn();
  const onOpen = overrides.onOpen ?? vi.fn();
  const utils = render(
    <CollabExchangeProvider>
      <MemoryRouter>
        <SessionStateProvider>
          <NotificationProvider>
            <LibraryTab
              projectId="proj-1"
              projectTitle="M42"
              frames={frames}
              error={overrides.error ?? false}
              reload={reload}
              canModerate={overrides.canModerate ?? false}
              onOpen={onOpen}
            />
          </NotificationProvider>
        </SessionStateProvider>
      </MemoryRouter>
    </CollabExchangeProvider>,
  );
  return { reload, onOpen, ...utils };
}

function rowOf(fileName: string): HTMLElement {
  const row = screen.getByText(fileName).closest('tr');
  expect(row).not.toBeNull();
  return row as HTMLElement;
}

describe('LibraryTab — ported from the retired receive tab', () => {
  it('shows the Collaboration-folder banner when unset, and hides it once set', async () => {
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      if (command === 'get_collaboration_dir') return Promise.resolve(null);
      if (command === 'list_collab_attention') {
        return Promise.resolve({ changed: [], awaitingChoice: [], notKept: [], otherFiles: [] });
      }
      if (command === 'get_collab_exchange') return Promise.resolve(exchangeSnapshot);
      return Promise.resolve(null);
    }) as never);

    renderTab([frame({})]);

    expect(await screen.findByText('Set a Collaboration folder first — synced frames land there.')).toBeInTheDocument();
    expect(screen.getByRole('button', { name: /Export for WBPP/ })).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Open File Manager' }));
    expect(navigateMock).toHaveBeenCalledWith('/files');
  });

  it('has no "Project frames" heading and puts Export for WBPP on the group row', async () => {
    renderTab([frame({})]);
    expect(screen.queryByText('Project frames')).toBeNull();
    const exportBtn = await screen.findByRole('button', { name: /Export for WBPP/ });
    expect(exportBtn.closest('div')!.textContent).toContain('Columns');
  });

  it('does not show the banner once a Collaboration folder is set', async () => {
    renderTab([frame({})]);
    await screen.findByText('light_001.fits');
    expect(
      screen.queryByText('Set a Collaboration folder first — synced frames land there.'),
    ).not.toBeInTheDocument();
  });

  it('shows the attention lists above the frames', async () => {
    renderTab([frame({})]);
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('list_collab_attention', { projectId: 'proj-1' }));
  });

  it("reloads the frames when its project's attention or landing changes", async () => {
    const { reload } = renderTab([frame({})]);
    await screen.findByText('light_001.fits');
    await waitFor(() => expect(listeners['collab-attention-changed']).toBeDefined());
    fire('collab-attention-changed', { projectId: 'other' });
    expect(reload).not.toHaveBeenCalled();
    fire('collab-attention-changed', { projectId: 'proj-1' });
    expect(reload).toHaveBeenCalledTimes(1);
    fire('collab-frames-landed', { projectId: 'proj-1', landed: 1, failed: 0, awaitingGc: 0 });
    expect(reload).toHaveBeenCalledTimes(2);
  });
});

describe('LibraryTab — device state', () => {
  it('a frame in the exchange in-flight list renders downloading with its percent', async () => {
    exchangeSnapshot = {
      projects: [
        projectFlows({
          recv: [flow({ inFlight: [{ frameUuid: 'u-dl', fileName: 'dl.fits', size: 100, done: 42 }] })],
        }),
      ],
      names: [],
    };

    renderTab([
      frame({ frameUuid: 'u-dl', fileName: 'dl.fits', localState: 'wanted', onDisk: false }),
    ]);

    await screen.findByText('42%');
  });

  it('a wanted frame with waitingForPublisher reads missing + publisher offline', async () => {
    renderTab([
      frame({
        frameUuid: 'u-miss',
        fileName: 'miss.fits',
        localState: 'wanted',
        onDisk: false,
        waitingForPublisher: true,
        holdersOnline: 0,
      }),
    ]);

    const row = rowOf('miss.fits');
    expect(await within(row).findByText('missing')).toBeInTheDocument();
    expect(within(row).getByText('publisher offline')).toBeInTheDocument();
  });
});

describe('LibraryTab — every project frame', () => {
  it('lists my own published frames beside other members\' — own_held reads Have, own_missing reads missing', async () => {
    renderTab([
      // Grouped by publisher, only the first group opens: "Me" sorts before "Zoe".
      frame({ frameUuid: 'u-z', fileName: 'zoe.fits', publisher: 'Zoe', localState: 'held' }),
      frame({ frameUuid: 'u-m', fileName: 'mine.fits', publisher: 'Me', own: true, localState: 'own_held' }),
      frame({ frameUuid: 'u-g', fileName: 'gone.fits', publisher: 'Me', own: true, localState: 'own_missing', onDisk: false }),
      frame({ frameUuid: 'u-p', fileName: 'pending.fits', publisher: 'Me', own: true, state: 'pending', localState: 'own_held' }),
    ]);

    expect(await screen.findByText('mine.fits')).toBeInTheDocument();
    expect(within(rowOf('mine.fits')).getByText('have')).toBeInTheDocument(); // the ● glyph is now a StatusDot
    expect(screen.getByText('Zoe')).toBeInTheDocument();
    expect(within(rowOf('gone.fits')).getByText('missing')).toBeInTheDocument();
    expect(within(rowOf('gone.fits')).getByText('gone from disk')).toBeInTheDocument();
    expect(screen.queryByText('pending.fits')).toBeNull();
  });
});

describe('LibraryTab — Keep again', () => {
  it('is disabled with no not_kept frame in view', async () => {
    renderTab([frame({ frameUuid: 'u1', fileName: 'a.fits', localState: 'held' })]);
    const btn = await screen.findByRole('button', { name: 'Keep again all 0' });
    expect(btn).toBeDisabled();
  });

  it('with one not_kept frame, invokes keep_collab_frames_again with its uuid', async () => {
    renderTab([
      frame({ frameUuid: 'u1', fileName: 'a.fits', localState: 'held' }),
      frame({ frameUuid: 'u2', fileName: 'b.fits', localState: 'not_kept', onDisk: false }),
    ]);
    const btn = await screen.findByRole('button', { name: 'Keep again all 1' });
    fireEvent.click(btn);
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('keep_collab_frames_again', {
        projectId: 'proj-1',
        frameUuids: ['u2'],
      }),
    );
  });
});

describe('libraryToCome', () => {
  it('counts downloading + queued + missing, not notKept/have', () => {
    const frames: ProjectFrameView[] = [
      frame({ frameUuid: 'have1', localState: 'held' }),
      frame({ frameUuid: 'dl1', localState: 'wanted' }),
      frame({ frameUuid: 'q1', localState: 'wanted' }),
      frame({ frameUuid: 'miss1', localState: 'missing' }),
      frame({ frameUuid: 'nk1', localState: 'not_kept' }),
    ];
    const inFlight = new Map([['dl1', { done: 1, size: 2 }]]);

    expect(libraryToCome(frames, inFlight)).toBe(3);
  });

  it('never counts my own frames, even a missing one', () => {
    const frames: ProjectFrameView[] = [frame({ frameUuid: 'o1', own: true, localState: 'own_missing' })];
    expect(libraryToCome(frames, new Map())).toBe(0);
  });

  it('returns 0 for a null frame list', () => {
    expect(libraryToCome(null, new Map())).toBe(0);
  });
});

describe('LibraryTab — Exclude (canModerate)', () => {
  const exclusionFixture: ProjectFrameView[] = [
    frame({ frameUuid: 'p1', fileName: 'p1.fits', state: 'published', accepted: true }),
    frame({ frameUuid: 'p2', fileName: 'p2.fits', state: 'published', accepted: true }),
    frame({
      frameUuid: 'p3',
      fileName: 'p3.fits',
      state: 'published',
      accepted: false,
      acceptedReason: 'trailed',
    }),
  ];

  it('is not offered without canModerate', async () => {
    renderTab(exclusionFixture, { canModerate: false });
    await screen.findByText('p1.fits');
    expect(screen.queryByRole('button', { name: /Exclude/ })).toBeNull();
  });

  it('with canModerate, opens the dialog with just the eligible frames', async () => {
    renderTab(exclusionFixture, { canModerate: true });
    await screen.findByText('p1.fits');
    fireEvent.click(screen.getByRole('checkbox', { name: 'Select all shown' }));
    fireEvent.click(screen.getByRole('button', { name: 'Exclude 2 of 3' }));
    expect(screen.getByText('Exclude 2 frames from the project')).toBeInTheDocument();
  });
});
