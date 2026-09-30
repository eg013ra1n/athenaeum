import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, fireEvent, render, screen, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { NavHistoryProvider } from '../contexts/NavHistoryContext';
import { NotificationProvider } from '../contexts/NotificationContext';
import { TransfersProvider } from '../contexts/TransfersContext';
import { CollabExchangeProvider } from '../contexts/CollabExchangeContext';
import { api } from '../api';
import Transfers from './Transfers';
import type {
  ExchangeSnapshot,
  ReceiveSessionView,
  SyncStatus,
  TerminalTransfers,
} from '../types/models';

vi.mock('../api', () => ({
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

function receiveSession(overrides: Partial<ReceiveSessionView> = {}): ReceiveSessionView {
  return {
    id: 1,
    projectId: 'proj-1',
    projectTitle: 'M31 Deep Field',
    startedAt: '2026-09-30T10:00:00Z',
    finishedAt: '2026-09-30T10:05:00Z',
    frames: 48,
    bytes: 1_000_000,
    failed: 0,
    sources: [
      { device: 'd1', memberName: 'Kostya', deviceName: null, bytes: 500_000 },
      { device: 'd2', memberName: 'Olga', deviceName: null, bytes: 500_000 },
    ],
    ...overrides,
  };
}

const emptyTerminalTransfers: TerminalTransfers = { sent: [], received: [] };
const emptyExchangeSnapshot: ExchangeSnapshot = { projects: [], names: [] };

let receiveSessions: ReceiveSessionView[] = [];

beforeEach(() => {
  receiveSessions = [];
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((command: string) => {
    switch (command) {
      case 'list_terminal_transfers':
        return Promise.resolve(emptyTerminalTransfers);
      case 'get_scan_roots':
        return Promise.resolve([]);
      case 'get_sync_status':
        return Promise.resolve(emptyStatus());
      case 'list_sync_history':
        return Promise.resolve([]);
      case 'get_sync_device_names':
      case 'get_sync_device_capabilities':
        return Promise.resolve({});
      case 'list_collab_projects':
        return Promise.resolve([]);
      case 'get_sync_incoming_dir':
        return Promise.resolve(null);
      case 'get_collab_exchange':
        return Promise.resolve(emptyExchangeSnapshot);
      case 'list_collab_receive_sessions':
        return Promise.resolve(receiveSessions);
      default:
        return Promise.resolve(null);
    }
  }) as never);
  vi.mocked(api.listen).mockReset();
  vi.mocked(api.listen).mockImplementation((() => Promise.resolve(() => {})) as never);
});

afterEach(cleanup);

function renderPage() {
  return render(
    <MemoryRouter initialEntries={['/transfers']}>
      <NavHistoryProvider>
        <NotificationProvider>
          <TransfersProvider>
            <CollabExchangeProvider>
              <Transfers />
            </CollabExchangeProvider>
          </TransfersProvider>
        </NotificationProvider>
      </NavHistoryProvider>
    </MemoryRouter>,
  );
}

describe('Transfers page — collab receive sessions', () => {
  it('fetches sessions on mount and lists one in the unified list', async () => {
    receiveSessions = [receiveSession()];
    renderPage();

    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('list_collab_receive_sessions', {
        projectId: null,
        limit: 200,
      }),
    );
    expect(await screen.findByText('M31 Deep Field')).toBeInTheDocument();
  });

  it('a failed session counts under the Failed filter', async () => {
    receiveSessions = [receiveSession({ failed: 2 })];
    renderPage();

    await screen.findByText('M31 Deep Field');

    fireEvent.click(screen.getByRole('button', { name: /Failed/ }));

    expect(await screen.findByText('M31 Deep Field')).toBeInTheDocument();
  });

  it('a session with no failures does not appear under the Failed filter', async () => {
    receiveSessions = [receiveSession({ failed: 0 })];
    renderPage();

    await screen.findByText('M31 Deep Field');

    fireEvent.click(screen.getByRole('button', { name: /Failed/ }));

    expect(screen.queryByText('M31 Deep Field')).not.toBeInTheDocument();
  });

  it('a session appears under the Completed filter', async () => {
    receiveSessions = [receiveSession()];
    renderPage();

    await screen.findByText('M31 Deep Field');

    fireEvent.click(screen.getByRole('button', { name: /Completed/ }));

    expect(await screen.findByText('M31 Deep Field')).toBeInTheDocument();
  });

  it('selecting a session shows no detail pane', async () => {
    receiveSessions = [receiveSession()];
    renderPage();

    const row = await screen.findByText('M31 Deep Field');
    fireEvent.click(row);

    expect(screen.queryByText('Close')).not.toBeInTheDocument();
  });

  it('re-fetches sessions on collab-frames-landed', async () => {
    renderPage();
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('list_collab_receive_sessions', expect.anything()));

    const calls = vi.mocked(api.listen).mock.calls;
    const landedCall = calls.find(([event]) => event === 'collab-frames-landed');
    expect(landedCall).toBeDefined();
    const handler = landedCall?.[1] as (p: unknown) => void;

    const before = vi.mocked(api.invoke).mock.calls.filter(
      ([cmd]) => cmd === 'list_collab_receive_sessions',
    ).length;

    receiveSessions = [receiveSession({ id: 2 })];
    handler({ projectId: 'proj-1', landed: 1, failed: 0, awaitingGc: 0 });

    await waitFor(() => {
      const after = vi.mocked(api.invoke).mock.calls.filter(
        ([cmd]) => cmd === 'list_collab_receive_sessions',
      ).length;
      expect(after).toBeGreaterThan(before);
    });
  });
});
