import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen, waitFor } from '@testing-library/react';
import { CollabExchangeProvider } from '../../contexts/CollabExchangeContext';
import { api } from '../../api';
import { TransferIndicator } from './TransferIndicator';
import type { ExchangeSnapshot, FlowView, ProjectFlows, SyncStatus } from '../../types/models';

vi.mock('../../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
}));

function emptyStatus(): SyncStatus {
  return {
    devPairingEnabled: false,
    transportStarted: true,
    pairingTicket: null,
    receivedTotal: 0,
    sender: {
      started: true,
      queued: 0,
      transferring: 0,
      confirmedTotal: 0,
      failedTotal: 0,
      cancelledTotal: 0,
      active: [],
    },
    receiver: { started: true, active: [], queued: [], receivedTotal: 0 },
    transport: { status: 'relay_connected', relayUrl: 'relay.example', lastError: null },
  };
}

let mockStatus: SyncStatus | null = emptyStatus();
let mockVisible = true;
const openPanel = vi.fn();

// The sidebar indicator reads BOTH `useTransfers` (personal-sync status) and
// `useCollabExchange` (this task). `useTransfers` is mocked directly — the
// codebase's convention is to mock the underlying hook module rather than
// export the raw context object — while `useCollabExchange` is exercised for
// real through its provider (below), driven by a mocked `api`.
vi.mock('../../contexts/TransfersContext', () => ({
  useTransfers: () => ({ status: mockStatus, visible: mockVisible, openPanel }),
}));

afterEach(cleanup);

function flow(overrides: Partial<FlowView> = {}): FlowView {
  return {
    projectId: 'proj-1',
    device: 'devA',
    direction: 'recv',
    bytesSession: 0,
    rateBps: 1000,
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

let snapshot: ExchangeSnapshot = { projects: [], names: [] };

beforeEach(() => {
  mockStatus = emptyStatus();
  mockVisible = true;
  snapshot = { projects: [], names: [] };
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((command: string) => {
    if (command === 'get_collab_exchange') return Promise.resolve(snapshot);
    return Promise.resolve(null);
  }) as never);
  vi.mocked(api.listen).mockReset();
  vi.mocked(api.listen).mockImplementation((() => Promise.resolve(() => {})) as never);
});

function renderIndicator(collapsed = false) {
  return render(
    <CollabExchangeProvider>
      <TransferIndicator collapsed={collapsed} />
    </CollabExchangeProvider>,
  );
}

describe('TransferIndicator', () => {
  it('icon is muted when nothing personal or collab is moving', async () => {
    renderIndicator();
    const label = await screen.findByText('Transfers');
    const svg = label.closest('button')?.querySelector('svg');
    expect(svg).toHaveClass('text-content-muted');
  });

  it('icon is text-accent with only collab traffic and personal up = 0', async () => {
    snapshot = {
      projects: [
        projectFlows({
          recv: [flow({ device: 'devA', direction: 'recv', moving: true, rateBps: 500 })],
        }),
      ],
      names: [],
    };

    renderIndicator();

    const label = await screen.findByText('Transfers');
    await waitFor(() => {
      const svg = label.closest('button')?.querySelector('svg');
      expect(svg).toHaveClass('text-accent');
    });
  });

  it('shows a third combined-rate item when collab is active', async () => {
    snapshot = {
      projects: [
        projectFlows({
          recv: [flow({ device: 'devA', direction: 'recv', moving: true, rateBps: 500 })],
        }),
      ],
      names: [],
    };

    renderIndicator();

    expect(await screen.findByText('500 B/s')).toBeInTheDocument();
  });

  it('the title gains a Collaboration line when collab is active', async () => {
    snapshot = {
      projects: [
        projectFlows({
          recv: [flow({ device: 'devA', direction: 'recv', moving: true, rateBps: 500 })],
        }),
      ],
      names: [],
    };

    renderIndicator();

    const label = await screen.findByText('Transfers');
    const button = label.closest('button');
    await waitFor(() => expect(button?.getAttribute('title') ?? '').toContain('Collaboration'));
  });
});
