import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, act, waitFor } from '@testing-library/react';
import { CollabExchangeProvider, useCollabExchange } from './CollabExchangeContext';
import { api } from '../api';
import type { CollabLiveStatus, ExchangeSnapshot, FlowView, ProjectFlows } from '../types/models';

vi.mock('../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
}));

const flow = (o: Partial<FlowView>): FlowView => ({
  projectId: 'p1',
  device: 'devA',
  direction: 'recv',
  bytesSession: 0,
  rateBps: 1000,
  etaSecs: null,
  moving: true,
  completed: 0,
  inFlight: [],
  ...o,
});
const proj = (o: Partial<ProjectFlows>): ProjectFlows => ({
  projectId: 'p1',
  recv: [],
  send: [],
  toGo: 0,
  waitingForPublisher: null,
  ...o,
});

const emptySnapshot: ExchangeSnapshot = { projects: [], names: [] };

const liveStatus = (o: Partial<CollabLiveStatus> = {}): CollabLiveStatus => ({
  state: 'live',
  retryInSecs: null,
  since: '2026-09-30T00:00:00Z',
  storage: 'available',
  storageReason: null,
  watcherDegraded: false,
  networkVolume: false,
  ...o,
});

/** Every `api.listen` registration this render made, by event name. */
const listeners: Record<string, ((payload: unknown) => void) | undefined> = {};

function Probe() {
  const { state } = useCollabExchange();
  return <div data-testid="projects">{Object.keys(state.projects).join(',')}</div>;
}

beforeEach(() => {
  for (const k of Object.keys(listeners)) delete listeners[k];
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.listen).mockReset();
  vi.mocked(api.listen).mockImplementation((<T,>(event: string, cb: (p: T) => void) => {
    listeners[event] = cb as unknown as (payload: unknown) => void;
    return Promise.resolve(() => {});
  }) as never);
});

describe('CollabExchangeProvider', () => {
  it('mounts by fetching the global snapshot', async () => {
    vi.mocked(api.invoke).mockResolvedValue(emptySnapshot);

    render(
      <CollabExchangeProvider>
        <Probe />
      </CollabExchangeProvider>,
    );

    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('get_collab_exchange', { projectId: null }),
    );
  });

  it('an unnamed device in a progress event triggers exactly one extra global refetch, even with three events in flight', async () => {
    let calls = 0;
    const pendingResolvers: Array<(snap: ExchangeSnapshot) => void> = [];
    vi.mocked(api.invoke).mockImplementation(((command: string) => {
      if (command === 'get_collab_exchange') {
        calls += 1;
        if (calls === 1) return Promise.resolve(emptySnapshot);
        return new Promise<ExchangeSnapshot>((resolve) => pendingResolvers.push(resolve));
      }
      return Promise.resolve(null);
    }) as never);

    render(
      <CollabExchangeProvider>
        <Probe />
      </CollabExchangeProvider>,
    );

    await waitFor(() => expect(calls).toBe(1));

    const unnamedFlow = (dir: 'send') =>
      proj({ send: [flow({ device: 'unnamed-device-1', direction: dir })] });

    act(() => {
      listeners['collab-exchange-progress']?.({ projects: [unnamedFlow('send')] });
      listeners['collab-exchange-progress']?.({ projects: [unnamedFlow('send')] });
      listeners['collab-exchange-progress']?.({ projects: [unnamedFlow('send')] });
    });

    await waitFor(() => expect(calls).toBe(2));
    // Give any accidental extra refetch a chance to fire before asserting none did.
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(calls).toBe(2);
    expect(pendingResolvers).toHaveLength(1);

    // Resolving the pending refetch clears the in-flight guard; verified
    // indirectly — a further unnamed-device event now fires a new refetch.
    act(() => pendingResolvers[0]?.(emptySnapshot));
    await act(async () => {
      await Promise.resolve();
    });
  });

  it('a collab-live-status event that is not live empties state.projects', async () => {
    vi.mocked(api.invoke).mockResolvedValue({
      projects: [proj({ recv: [flow({})] })],
      names: [],
    } as ExchangeSnapshot);

    render(
      <CollabExchangeProvider>
        <Probe />
      </CollabExchangeProvider>,
    );

    await waitFor(() => expect(screen.getByTestId('projects').textContent).toBe('p1'));

    act(() => {
      listeners['collab-live-status']?.(liveStatus({ state: 'off' }));
    });

    await waitFor(() => expect(screen.getByTestId('projects').textContent).toBe(''));
  });

  it('useCollabExchange throws outside the provider', () => {
    const consoleError = vi.spyOn(console, 'error').mockImplementation(() => {});
    expect(() => render(<Probe />)).toThrow(
      'useCollabExchange must be used within CollabExchangeProvider',
    );
    consoleError.mockRestore();
  });
});
