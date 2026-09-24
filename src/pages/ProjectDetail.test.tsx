import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, act, waitFor } from '@testing-library/react';
import { MemoryRouter, Routes, Route } from 'react-router-dom';
import { NotificationProvider } from '../contexts/NotificationContext';
import { SessionStateProvider } from '../contexts/SessionStateContext';
import { NavHistoryProvider } from '../contexts/NavHistoryContext';
import { ToastStack } from '../components/Toast';
import ProjectDetail from './ProjectDetail';
import { useCollabNotifications } from '../hooks/useCollabNotifications';
import { api } from '../api';
import type {
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
    ...overrides,
  };
}

function detailFixture(): Detail {
  return {
    card: projectCard(),
    members: [],
    thresholdsVersion: null,
    thresholds: [],
    links: [],
    portalBase: 'https://hub.example',
  };
}

function gateFixture(): GateReport {
  return { projectId: 'proj-1', total: 2, publishable: 2, rows: [] };
}

let publishedListener: ((res: unknown) => void) | undefined;

beforeEach(() => {
  publishedListener = undefined;
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

function renderProjectDetail() {
  return render(
    <MemoryRouter initialEntries={['/projects/proj-1']}>
      <NavHistoryProvider>
        <SessionStateProvider>
          <NotificationProvider>
            <CollabNotificationsHarness />
            <Routes>
              <Route path="/projects/:id" element={<ProjectDetail />} />
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
});
