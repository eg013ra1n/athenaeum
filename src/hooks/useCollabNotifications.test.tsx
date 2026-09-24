import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, act } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { NotificationProvider } from '../contexts/NotificationContext';
import { ToastStack } from '../components/Toast';
import { useCollabNotifications } from './useCollabNotifications';
import { api } from '../api';
import type { CollabReplicationPaused, ProjectCard } from '../types/models';

vi.mock('../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
}));

let pausedListener: ((p: CollabReplicationPaused) => void) | undefined;

function projectCard(overrides: Partial<ProjectCard>): ProjectCard {
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
    candidates: 0,
    publishable: 0,
    autoReplicate: true,
    autoPublish: true,
    fetchedAt: '2026-09-24T00:00:00Z',
    ...overrides,
  };
}

beforeEach(() => {
  pausedListener = undefined;
  localStorage.clear();
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((command: string) => {
    if (command === 'list_collab_projects') {
      return Promise.resolve([projectCard({})]);
    }
    return Promise.resolve([]);
  }) as never);
  vi.mocked(api.listen).mockImplementation((<T,>(event: string, cb: (p: T) => void) => {
    if (event === 'collab-replication-paused') {
      pausedListener = cb as unknown as (p: CollabReplicationPaused) => void;
    }
    return Promise.resolve(() => {});
  }) as never);
});

function Harness() {
  useCollabNotifications();
  return <ToastStack />;
}

function renderHarness() {
  return render(
    <MemoryRouter>
      <NotificationProvider>
        <Harness />
      </NotificationProvider>
    </MemoryRouter>,
  );
}

describe('useCollabNotifications', () => {
  it('collab-replication-paused produces exactly one toast with the link, and a repeated identical event dedupes', async () => {
    renderHarness();

    // Let the mount-time `list_collab_projects` fetch and the `api.listen`
    // registration settle before firing the event.
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });
    expect(pausedListener).toBeDefined();

    act(() => {
      pausedListener?.({ projectId: 'proj-1', missing: 5, missingBytes: 2 * 1024 * 1024 * 1024 });
    });

    const toasts = await screen.findAllByRole('status');
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toHaveTextContent('Replication paused: 5 frames missing (2.00 GB)');

    // The toast is a clickable link to that project's Receive tab.
    const linkButton = screen.getByRole('button', {
      name: 'Replication paused: 5 frames missing (2.00 GB)',
    });
    expect(linkButton).toBeInTheDocument();

    // A repeated, identical event must not add a second toast — the stable
    // `paused-<projectId>-<missing>` dedupe key collapses it.
    act(() => {
      pausedListener?.({ projectId: 'proj-1', missing: 5, missingBytes: 2 * 1024 * 1024 * 1024 });
    });

    expect(screen.getAllByRole('status')).toHaveLength(1);
  });

  it('a different missing count is a new, undeduped toast', async () => {
    renderHarness();
    await act(async () => {
      await Promise.resolve();
      await Promise.resolve();
    });

    act(() => {
      pausedListener?.({ projectId: 'proj-1', missing: 5, missingBytes: 2 * 1024 * 1024 * 1024 });
    });
    await screen.findAllByRole('status');

    act(() => {
      pausedListener?.({ projectId: 'proj-1', missing: 9, missingBytes: 3 * 1024 * 1024 * 1024 });
    });

    expect(screen.getAllByRole('status')).toHaveLength(2);
  });
});
