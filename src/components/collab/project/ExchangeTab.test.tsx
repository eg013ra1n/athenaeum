import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { CollabExchangeProvider } from '../../../contexts/CollabExchangeContext';
import { api } from '../../../api';
import ExchangeTab from './ExchangeTab';
import type {
  ExchangeSnapshot,
  FlowView,
  InFlightView,
  MemberSummary,
  ProjectFlows,
  ReceiveSessionView,
  SessionSourceView,
} from '../../../types/models';

vi.mock('../../../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
}));

afterEach(cleanup);

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

function inFlightItem(overrides: Partial<InFlightView> = {}): InFlightView {
  return { frameUuid: 'f-1', fileName: 'light_003.fits', size: 100, done: 40, ...overrides };
}

function sessionSource(overrides: Partial<SessionSourceView> = {}): SessionSourceView {
  return { device: 'dev-src', memberName: null, deviceName: null, bytes: 0, ...overrides };
}

function receiveSession(overrides: Partial<ReceiveSessionView> = {}): ReceiveSessionView {
  return {
    id: 1,
    projectId: 'proj-1',
    projectTitle: 'M31',
    startedAt: '2026-09-30T10:00:00Z',
    finishedAt: '2026-09-30T10:05:00Z',
    frames: 10,
    bytes: 1024,
    failed: 0,
    sources: [],
    ...overrides,
  };
}

function member(overrides: Partial<MemberSummary> = {}): MemberSummary {
  return {
    accountId: 'acc-1',
    displayName: 'Kostya',
    dataRole: 'send_receive',
    coordinator: false,
    devices: [],
    online: true,
    lastSeenAt: null,
    publishedFrames: 0,
    secondsByFilter: {},
    qualityByCamera: [],
    holdsFrames: 0,
    holdsBytes: 0,
    holdsShare: 0,
    ...overrides,
  };
}

const EMPTY_SNAPSHOT: ExchangeSnapshot = { projects: [], names: [] };

/** Every registered listener per event. */
let listeners: Record<string, ((p: unknown) => void)[]> = {};

let exchangeSnapshot: ExchangeSnapshot = EMPTY_SNAPSHOT;
let receiveSessions: ReceiveSessionView[] = [];

beforeEach(() => {
  listeners = {};
  exchangeSnapshot = EMPTY_SNAPSHOT;
  receiveSessions = [];
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((command: string) => {
    switch (command) {
      case 'get_collab_exchange':
        return Promise.resolve(exchangeSnapshot);
      case 'list_collab_receive_sessions':
        return Promise.resolve(receiveSessions);
      default:
        return Promise.resolve(null);
    }
  }) as never);
  vi.mocked(api.listen).mockReset();
  vi.mocked(api.listen).mockImplementation((<T,>(event: string, cb: (p: T) => void) => {
    (listeners[event] ??= []).push(cb as unknown as (p: unknown) => void);
    return Promise.resolve(() => {});
  }) as never);
});

function renderTab(
  overrides: { canReceive?: boolean; members?: MemberSummary[] | null; projectId?: string } = {},
) {
  return render(
    <CollabExchangeProvider>
      <ExchangeTab
        projectId={overrides.projectId ?? 'proj-1'}
        canReceive={overrides.canReceive ?? true}
        members={overrides.members ?? null}
      />
    </CollabExchangeProvider>,
  );
}

describe('ExchangeTab — live rows', () => {
  it('a recv flow from a named device renders "↓ from Kostya" and its rate', async () => {
    exchangeSnapshot = {
      projects: [
        projectFlows({
          recv: [flow({ device: 'devA', direction: 'recv', rateBps: 31 * 1024 * 1024 })],
        }),
      ],
      names: [{ projectId: 'proj-1', device: 'devA', memberName: 'Kostya', deviceName: 'kostya-obs' }],
    };

    renderTab();

    expect(await screen.findByText('↓ from Kostya')).toBeInTheDocument();
    expect(screen.getByText('31.0 MB/s')).toBeInTheDocument();
  });

  it('an unnamed device renders its 8-char id', async () => {
    exchangeSnapshot = {
      projects: [
        projectFlows({
          recv: [flow({ device: 'devB1234567890', direction: 'recv' })],
        }),
      ],
      names: [],
    };

    renderTab();

    expect(await screen.findByText('↓ from devB1234')).toBeInTheDocument();
  });

  it('a contributor (canReceive=false) sees the role sentence and still the Sending card', async () => {
    exchangeSnapshot = {
      projects: [
        projectFlows({
          send: [flow({ device: 'devC', direction: 'send', rateBps: 1000 })],
        }),
      ],
      names: [{ projectId: 'proj-1', device: 'devC', memberName: 'Andrei', deviceName: 'andrei-pc' }],
    };

    renderTab({ canReceive: false });

    expect(await screen.findByText('Your role does not receive project data')).toBeInTheDocument();
    expect(
      screen.getByText('Contributors only send. Ask the coordinator for the Processor role to receive the project.'),
    ).toBeInTheDocument();
    expect(screen.getByText('Sending')).toBeInTheDocument();
    expect(await screen.findByText('↑ to Andrei')).toBeInTheDocument();
  });

  it('expanding a row lists its in-flight file', async () => {
    exchangeSnapshot = {
      projects: [
        projectFlows({
          recv: [
            flow({
              device: 'devA',
              direction: 'recv',
              inFlight: [inFlightItem({ fileName: 'light_003.fits' })],
            }),
          ],
        }),
      ],
      names: [{ projectId: 'proj-1', device: 'devA', memberName: 'Kostya', deviceName: 'kostya-obs' }],
    };

    renderTab();

    const rowText = await screen.findByText('↓ from Kostya');
    expect(screen.queryByText('light_003.fits')).not.toBeInTheDocument();

    const button = rowText.closest('button');
    expect(button).not.toBeNull();
    expect(button).toHaveAttribute('aria-expanded', 'false');
    fireEvent.click(button as HTMLButtonElement);

    expect(button).toHaveAttribute('aria-expanded', 'true');
    expect(await screen.findByText('light_003.fits')).toBeInTheDocument();
  });

  it('counts distinct MEMBERS, not flows: two recv flows from two devices of one member read "from 1 member" (singular)', async () => {
    exchangeSnapshot = {
      projects: [
        projectFlows({
          recv: [
            flow({ device: 'devA', direction: 'recv', rateBps: 100 }),
            flow({ device: 'devB', direction: 'recv', rateBps: 100 }),
          ],
        }),
      ],
      names: [
        { projectId: 'proj-1', device: 'devA', memberName: 'Kostya', deviceName: 'kostya-laptop' },
        { projectId: 'proj-1', device: 'devB', memberName: 'Kostya', deviceName: 'kostya-obs' },
      ],
    };

    renderTab();

    expect(await screen.findByText(/from 1 member ·/)).toBeInTheDocument();
    expect(screen.queryByText(/from 2 members ·/)).not.toBeInTheDocument();
  });
});

describe('ExchangeTab — receive history', () => {
  it('lists "Kostya, Olga" for two sources', async () => {
    receiveSessions = [
      receiveSession({
        sources: [
          sessionSource({ device: 'd1', memberName: 'Kostya' }),
          sessionSource({ device: 'd2', memberName: 'Olga' }),
        ],
      }),
    ];

    renderTab();

    expect(await screen.findByText('Kostya, Olga')).toBeInTheDocument();
  });

  it('shows "No receive sessions yet." when the history is empty', async () => {
    receiveSessions = [];
    renderTab();
    expect(await screen.findByText('No receive sessions yet.')).toBeInTheDocument();
  });

  it('clears a stale error once a reload triggered by collab-frames-landed succeeds', async () => {
    let calls = 0;
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      switch (command) {
        case 'get_collab_exchange':
          return Promise.resolve(exchangeSnapshot);
        case 'list_collab_receive_sessions':
          calls += 1;
          if (calls === 1) return Promise.reject(new Error('boom'));
          return Promise.resolve([receiveSession({ id: 9, frames: 42 })]);
        default:
          return Promise.resolve(null);
      }
    }) as never);

    renderTab();

    expect(await screen.findByText('Could not load receive sessions — see console.')).toBeInTheDocument();

    await waitFor(() => expect(listeners['collab-frames-landed']).toBeDefined());
    act(() => {
      (listeners['collab-frames-landed'] ?? []).forEach((h) =>
        h({ projectId: 'proj-1', landed: 1, failed: 0, awaitingGc: 0 }),
      );
    });

    await waitFor(() =>
      expect(screen.queryByText('Could not load receive sessions — see console.')).not.toBeInTheDocument(),
    );
    expect(await screen.findByText('42')).toBeInTheDocument();
    expect(calls).toBe(2);
  });
});

describe('ExchangeTab — summary', () => {
  it('shows "waiting for publisher: 12" in the header after refreshProject resolves', async () => {
    exchangeSnapshot = {
      projects: [projectFlows({ toGo: 5, waitingForPublisher: 12 })],
      names: [],
    };

    renderTab();

    expect(await screen.findByText(/waiting for publisher: 12/, { selector: 'span' })).toBeInTheDocument();
  });
});

describe('ExchangeTab — member tone', () => {
  it('does not crash when a member list is supplied and a label matches it', async () => {
    exchangeSnapshot = {
      projects: [
        projectFlows({
          recv: [flow({ device: 'devA', direction: 'recv' })],
        }),
      ],
      names: [{ projectId: 'proj-1', device: 'devA', memberName: 'Kostya', deviceName: 'kostya-obs' }],
    };

    renderTab({ members: [member({ accountId: 'acc-1', displayName: 'Kostya' })] });

    expect(await screen.findByText('↓ from Kostya')).toBeInTheDocument();
  });
});
