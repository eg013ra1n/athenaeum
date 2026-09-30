import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { cleanup, render, screen } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { CollabExchangeProvider } from '../../contexts/CollabExchangeContext';
import { api } from '../../api';
import { CollabTrafficGroups } from './CollabTrafficGroups';
import type { ExchangeSnapshot, FlowView, ProjectFlows } from '../../types/models';

vi.mock('../../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
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
  snapshot = { projects: [], names: [] };
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((command: string) => {
    if (command === 'get_collab_exchange') return Promise.resolve(snapshot);
    return Promise.resolve(null);
  }) as never);
  vi.mocked(api.listen).mockReset();
  vi.mocked(api.listen).mockImplementation((() => Promise.resolve(() => {})) as never);
});

function renderGroups(projectTitles: Record<string, string> = {}) {
  return render(
    <MemoryRouter>
      <CollabExchangeProvider>
        <CollabTrafficGroups projectTitles={projectTitles} />
      </CollabExchangeProvider>
    </MemoryRouter>,
  );
}

describe('CollabTrafficGroups', () => {
  it('renders nothing with no flows', async () => {
    const { container } = renderGroups();
    await vi.waitFor(() => expect(api.invoke).toHaveBeenCalledWith('get_collab_exchange', { projectId: null }));
    expect(container.firstChild).toBeNull();
  });

  it('renders one group per project with flows, linking to its Exchange tab', async () => {
    snapshot = {
      projects: [
        projectFlows({
          projectId: 'proj-1',
          recv: [flow({ projectId: 'proj-1', device: 'devA', direction: 'recv', rateBps: 1024 })],
        }),
        projectFlows({
          projectId: 'proj-2',
          send: [flow({ projectId: 'proj-2', device: 'devB', direction: 'send', rateBps: 2048 })],
        }),
      ],
      names: [],
    };

    renderGroups({ 'proj-1': 'M31 Deep Field', 'proj-2': 'NGC 7000' });

    expect(await screen.findByText(/M31 Deep Field/)).toBeInTheDocument();
    expect(await screen.findByText(/NGC 7000/)).toBeInTheDocument();

    const links = screen.getAllByText('Open in project →');
    expect(links).toHaveLength(2);
    expect(links[0].closest('a')).toHaveAttribute('href', '/projects/proj-1?tab=exchange');
    expect(links[1].closest('a')).toHaveAttribute('href', '/projects/proj-2?tab=exchange');
  });

  it('renders the project peer rows in the group body', async () => {
    snapshot = {
      projects: [
        projectFlows({
          projectId: 'proj-1',
          recv: [flow({ projectId: 'proj-1', device: 'devA', direction: 'recv' })],
        }),
      ],
      names: [{ projectId: 'proj-1', device: 'devA', memberName: 'Kostya', deviceName: 'kostya-obs' }],
    };

    renderGroups({ 'proj-1': 'M31 Deep Field' });

    expect(await screen.findByText('↓ from Kostya')).toBeInTheDocument();
  });
});
