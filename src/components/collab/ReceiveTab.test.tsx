import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, act, waitFor, within } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { NotificationProvider } from '../../contexts/NotificationContext';
import ReceiveTab from './ReceiveTab';
import { api } from '../../api';
import type { ProjectFrameView } from '../../types/models';

vi.mock('../../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
}));

function frame(overrides: Partial<ProjectFrameView>): ProjectFrameView {
  return {
    frameUuid: 'u-1',
    fileName: 'light_001.fits',
    publisher: 'Alice',
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
    ...overrides,
  };
}

/** Every registered listener per event (a child and its parent may both listen). */
let listeners: Record<string, ((p: unknown) => void)[]> = {};
const fire = (event: string, payload: unknown) => act(() => (listeners[event] ?? []).forEach((h) => h(payload)));

beforeEach(() => {
  listeners = {};
  localStorage.clear();
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((command: string) => {
    switch (command) {
      case 'get_collaboration_dir':
        return Promise.resolve('/collab');
      case 'list_collab_attention':
        return Promise.resolve({ changed: [], awaitingChoice: [], notKept: [], otherFiles: [] });
      default:
        return Promise.resolve(null);
    }
  }) as never);
  vi.mocked(api.listen).mockImplementation((<T,>(event: string, cb: (p: T) => void) => {
    (listeners[event] ??= []).push(cb as unknown as (p: unknown) => void);
    return Promise.resolve(() => {});
  }) as never);
});

function renderTab(frames: ProjectFrameView[] | null) {
  const reload = vi.fn();
  render(
    <MemoryRouter>
      <NotificationProvider>
        <ReceiveTab projectId="proj-1" projectTitle="M42" frames={frames} reload={reload} />
      </NotificationProvider>
    </MemoryRouter>,
  );
  return { reload };
}

function rowOf(fileName: string): HTMLElement {
  const row = screen.getByTitle(fileName).closest('tr');
  expect(row).not.toBeNull();
  return row as HTMLElement;
}

describe('ReceiveTab', () => {
  it('renders each local state label', async () => {
    renderTab([
      frame({ frameUuid: 'a', fileName: 'held.fits', localState: 'held' }),
      frame({ frameUuid: 'b', fileName: 'wanted.fits', localState: 'wanted', onDisk: false }),
      frame({
        frameUuid: 'c',
        fileName: 'publisher.fits',
        localState: 'wanted',
        onDisk: false,
        waitingForPublisher: true,
        contentVersion: 2,
      }),
      frame({ frameUuid: 'd', fileName: 'missing.fits', localState: 'missing', onDisk: false }),
      frame({ frameUuid: 'e', fileName: 'choice.fits', localState: 'awaiting_choice', onDisk: false }),
      frame({ frameUuid: 'f', fileName: 'changed.fits', localState: 'quarantined' }),
      frame({ frameUuid: 'g', fileName: 'changed-new.fits', localState: 'quarantined', newVersionWaiting: true }),
      frame({ frameUuid: 'h', fileName: 'declined.fits', localState: 'not_kept', onDisk: false }),
      frame({ frameUuid: 'i', fileName: 'idle.fits', localState: 'idle' }),
    ]);

    await screen.findByTitle('held.fits');
    expect(within(rowOf('held.fits')).getByText('On disk')).toBeInTheDocument();
    expect(within(rowOf('wanted.fits')).getByText('Waiting')).toBeInTheDocument();
    expect(within(rowOf('publisher.fits')).getByText('v2 waiting for the publisher')).toBeInTheDocument();
    expect(within(rowOf('missing.fits')).getByText('Missing')).toBeInTheDocument();
    expect(within(rowOf('choice.fits')).getByText('Waiting for your choice')).toBeInTheDocument();
    expect(within(rowOf('changed.fits')).getByText('Changed')).toBeInTheDocument();
    expect(within(rowOf('changed.fits')).queryByText(/new version waiting/)).not.toBeInTheDocument();
    expect(within(rowOf('changed-new.fits')).getByText(/new version waiting/)).toBeInTheDocument();
    expect(within(rowOf('declined.fits')).getByText('Not kept')).toBeInTheDocument();
    expect(within(rowOf('idle.fits')).getByText('Not replicated')).toBeInTheDocument();
  });

  it('shows the live holder counts', async () => {
    renderTab([frame({ holdersOnline: 1, holdersTotal: 4 })]);
    expect(within(rowOf('light_001.fits')).getByText('1 online / 4')).toBeInTheDocument();
  });

  it('"Sync now" calls the global command and reloads', async () => {
    const { reload } = renderTab([frame({})]);
    await screen.findByTitle('light_001.fits');
    fireEvent.click(screen.getByRole('button', { name: 'Sync now' }));
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('collab_sync_now'));
    await waitFor(() => expect(reload).toHaveBeenCalled());
    expect(api.invoke).not.toHaveBeenCalledWith('sync_project_now', expect.anything());
  });

  it('shows the attention lists above the frames', async () => {
    renderTab([frame({})]);
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('list_collab_attention', { projectId: 'proj-1' }),
    );
  });

  it('reloads the frames when its project\'s attention or landing changes', async () => {
    const { reload } = renderTab([frame({})]);
    await screen.findByTitle('light_001.fits');
    await waitFor(() => expect(listeners['collab-attention-changed']).toBeDefined());
    expect(listeners['collab-replication-paused']).toBeUndefined();
    fire('collab-attention-changed', { projectId: 'other' });
    expect(reload).not.toHaveBeenCalled();
    fire('collab-attention-changed', { projectId: 'proj-1' });
    expect(reload).toHaveBeenCalledTimes(1);
    fire('collab-frames-landed', { projectId: 'proj-1', landed: 1, failed: 0, awaitingGc: 0 });
    expect(reload).toHaveBeenCalledTimes(2);
  });
});
