import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, act, within } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import ReceiveTab from './ReceiveTab';
import { api } from '../../api';
import type { CollabReplicationPaused, ProjectFrameView } from '../../types/models';

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
    holderCount: 3,
    onDisk: true,
    awaitingGc: false,
    locallyDeclined: false,
    byteSize: 1024,
    contentVersion: 1,
    lastError: null,
    fwhmArcsec: 2.1,
    eccentricity: 0.3,
    starsDetected: 500,
    ...overrides,
  };
}

let pausedListener: ((p: CollabReplicationPaused) => void) | undefined;

beforeEach(() => {
  pausedListener = undefined;
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((command: string) => {
    switch (command) {
      case 'get_collaboration_dir':
        return Promise.resolve('/collab');
      case 'sync_project_now':
        return Promise.resolve(undefined);
      case 'resolve_collab_loss':
        return Promise.resolve(undefined);
      default:
        return Promise.resolve(null);
    }
  }) as never);
  vi.mocked(api.listen).mockImplementation((<T,>(event: string, cb: (p: T) => void) => {
    if (event === 'collab-replication-paused') {
      pausedListener = cb as unknown as (p: CollabReplicationPaused) => void;
    }
    return Promise.resolve(() => {});
  }) as never);
});

function renderTab(frames: ProjectFrameView[] | null) {
  const reload = vi.fn();
  render(
    <MemoryRouter>
      <ReceiveTab projectId="proj-1" projectTitle="M42" frames={frames} reload={reload} />
    </MemoryRouter>,
  );
  return { reload };
}

describe('ReceiveTab', () => {
  it('renders each on-disk state label', async () => {
    const frames = [
      frame({ frameUuid: 'a', fileName: 'on-disk.fits', onDisk: true }),
      frame({ frameUuid: 'b', fileName: 'not-on-disk.fits', onDisk: false }),
      frame({ frameUuid: 'c', fileName: 'awaiting.fits', onDisk: false, awaitingGc: true }),
      frame({ frameUuid: 'd', fileName: 'declined.fits', onDisk: false, locallyDeclined: true }),
    ];
    renderTab(frames);

    const onDiskRow = (await screen.findByTitle('on-disk.fits')).closest('tr');
    expect(onDiskRow).not.toBeNull();
    expect(within(onDiskRow as HTMLElement).getByText('On disk')).toBeInTheDocument();

    const notOnDiskRow = screen.getByTitle('not-on-disk.fits').closest('tr');
    expect(within(notOnDiskRow as HTMLElement).getByText('Not on disk')).toBeInTheDocument();

    const awaitingRow = screen.getByTitle('awaiting.fits').closest('tr');
    expect(within(awaitingRow as HTMLElement).getByText('Waiting for cleanup')).toBeInTheDocument();

    const declinedRow = screen.getByTitle('declined.fits').closest('tr');
    expect(within(declinedRow as HTMLElement).getByText('Not kept')).toBeInTheDocument();
  });

  it('shows the paused banner and calls resolve_collab_loss with restore', async () => {
    renderTab([frame({})]);
    await screen.findByTitle('light_001.fits');

    expect(pausedListener).toBeDefined();
    act(() => {
      pausedListener?.({ projectId: 'proj-1', missing: 5, missingBytes: 2 * 1024 * 1024 * 1024 });
    });

    expect(await screen.findByText(/Replication paused: 5 frames missing/)).toBeInTheDocument();

    fireEvent.click(screen.getByRole('button', { name: 'Restore' }));
    expect(api.invoke).toHaveBeenCalledWith('resolve_collab_loss', {
      projectId: 'proj-1',
      action: 'restore',
    });
  });

  it('calls resolve_collab_loss with stopHolding', async () => {
    renderTab([frame({})]);
    await screen.findByTitle('light_001.fits');

    act(() => {
      pausedListener?.({ projectId: 'proj-1', missing: 5, missingBytes: 2 * 1024 * 1024 * 1024 });
    });
    await screen.findByText(/Replication paused: 5 frames missing/);

    fireEvent.click(screen.getByRole('button', { name: 'Stop keeping' }));
    expect(api.invoke).toHaveBeenCalledWith('resolve_collab_loss', {
      projectId: 'proj-1',
      action: 'stopHolding',
    });
  });
});
