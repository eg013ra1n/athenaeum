import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor, act, within } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { NotificationProvider } from '../../contexts/NotificationContext';
import { ToastStack } from '../Toast';
import CollabAttention from './CollabAttention';
import { api } from '../../api';
import type { CollabAttention as Attention, CollabAttentionChanged, LastCopyView } from '../../types/models';

vi.mock('../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

const attention: Attention = {
  changed: [
    { frameUuid: 'u1', fileName: 'c_a.fits', path: '/c/m31/o/c_a.fits', detectedAt: '2026-09-25T10:00:00Z', newVersionWaiting: true },
  ],
  awaitingChoice: [{ frameUuid: 'u2', fileName: 'c_b.fits', holdersOnline: 0, holdersTotal: 1, atRisk: true }],
  notKept: [{ frameUuid: 'u3', fileName: 'c_c.fits', contentVersion: 1 }],
  otherFiles: [{ path: '/c/m31/stray.fits', seenAt: '2026-09-25T09:00:00Z' }],
};

const empty: Attention = { changed: [], awaitingChoice: [], notKept: [], otherFiles: [] };

let preview: LastCopyView[];
let changedFile: (args: { confirmedDelete: boolean }) => Promise<unknown>;
let emitAttention: ((p: CollabAttentionChanged) => void) | undefined;

beforeEach(() => {
  localStorage.clear();
  emitAttention = undefined;
  preview = [{ frameUuid: 'u2', fileName: 'c_b.fits', holdersOnline: 0, holdersTotal: 1, atRisk: true }];
  changedFile = ({ confirmedDelete }) =>
    confirmedDelete
      ? Promise.resolve({ trashed: false })
      : Promise.reject(new Error('trash_unavailable: the system trash is not available — confirm to delete the changed file'));
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((cmd: string, args?: { confirmedDelete: boolean }) => {
    switch (cmd) {
      case 'list_collab_attention':
        return Promise.resolve(attention);
      case 'preview_collab_stop_keeping':
        return Promise.resolve(preview);
      case 'resolve_collab_changed_file':
        return changedFile(args ?? { confirmedDelete: false });
      default:
        return Promise.resolve(1);
    }
  }) as never);
  vi.mocked(api.listen).mockImplementation(((name: string, h: (p: CollabAttentionChanged) => void) => {
    if (name === 'collab-attention-changed') emitAttention = h;
    return Promise.resolve(() => {});
  }) as never);
});

const renderIt = () =>
  render(
    <MemoryRouter>
      <NotificationProvider>
        <CollabAttention projectId="p1" />
        <ToastStack />
      </NotificationProvider>
    </MemoryRouter>,
  );

const attentionReads = () =>
  vi.mocked(api.invoke).mock.calls.filter(([c]) => c === 'list_collab_attention').length;

describe('CollabAttention', () => {
  it('lists the four kinds', async () => {
    renderIt();
    expect(await screen.findByText('Changed files')).toBeInTheDocument();
    expect(screen.getByText('A new version is waiting')).toBeInTheDocument();
    expect(screen.getByText('Waiting for your choice')).toBeInTheDocument();
    expect(screen.getByText(/Nothing else is paused/)).toBeInTheDocument();
    expect(screen.getByText('Not kept')).toBeInTheDocument();
    expect(screen.getByText('Other files')).toBeInTheDocument();
    expect(screen.getByText(/The app never deletes them/)).toBeInTheDocument();
    expect(screen.getByText('/c/m31/stray.fits')).toBeInTheDocument();
    expect(api.invoke).toHaveBeenCalledWith('list_collab_attention', { projectId: 'p1' });
  });

  it('renders nothing when nothing needs attention', async () => {
    vi.mocked(api.invoke).mockImplementation((() => Promise.resolve(empty)) as never);
    const { container } = renderIt();
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('list_collab_attention', { projectId: 'p1' }));
    expect(container.querySelector('section')).toBeNull();
  });

  it('reloads on its own project\'s attention event only', async () => {
    renderIt();
    await screen.findByText('Changed files');
    expect(attentionReads()).toBe(1);
    act(() => emitAttention?.({ projectId: 'other' }));
    act(() => emitAttention?.({ projectId: 'p1' }));
    await waitFor(() => expect(attentionReads()).toBe(2));
  });

  it('per-row and bulk buttons have distinct accessible names', async () => {
    renderIt();
    await screen.findByText('Changed files');
    for (const name of ['Re-fetch all', 'Stop keeping all', 'Re-fetch', 'Stop keeping', 'Keep again', 'Keep all again', 'Re-fetch original', 'Delete']) {
      expect(screen.getAllByRole('button', { name })).toHaveLength(1);
    }
  });

  it('stop keeping warns about the last copy before it acts', async () => {
    renderIt();
    fireEvent.click(await screen.findByRole('button', { name: 'Stop keeping all' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('preview_collab_stop_keeping', { projectId: 'p1', frameUuids: ['u2'] }),
    );
    expect(await screen.findByText(/fewer than 2 other copies/)).toBeInTheDocument();
    expect(screen.getByText(/0 online, 1 in total/)).toBeInTheDocument();
    expect(api.invoke).not.toHaveBeenCalledWith('resolve_collab_deletions', expect.anything());
    fireEvent.click(screen.getByRole('button', { name: 'Stop keeping' }));
    // The frames the user was warned about are exactly the ones dropped.
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('resolve_collab_deletions', {
        projectId: 'p1',
        frameUuids: ['u2'],
        action: 'stopKeeping',
      }),
    );
  });

  it('stop keeping acts directly when every frame has enough other copies', async () => {
    preview = [{ frameUuid: 'u2', fileName: 'c_b.fits', holdersOnline: 2, holdersTotal: 3, atRisk: false }];
    renderIt();
    fireEvent.click(await screen.findByRole('button', { name: 'Stop keeping' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('resolve_collab_deletions', {
        projectId: 'p1',
        frameUuids: ['u2'],
        action: 'stopKeeping',
      }),
    );
    expect(screen.queryByText(/fewer than 2 other copies/)).not.toBeInTheDocument();
  });

  it('re-fetch works per frame and for all', async () => {
    renderIt();
    fireEvent.click(await screen.findByRole('button', { name: 'Re-fetch' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('resolve_collab_deletions', { projectId: 'p1', frameUuids: ['u2'], action: 'refetch' }),
    );
    fireEvent.click(screen.getByRole('button', { name: 'Re-fetch all' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('resolve_collab_deletions', { projectId: 'p1', frameUuids: null, action: 'refetch' }),
    );
  });

  it('re-fetch original falls back to a confirmed delete when there is no trash', async () => {
    renderIt();
    fireEvent.click(await screen.findByRole('button', { name: 'Re-fetch original' }));
    expect(await screen.findByText(/system trash is not available/)).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: 'Delete and re-fetch' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('resolve_collab_changed_file', {
        projectId: 'p1',
        frameUuid: 'u1',
        action: 'refetchOriginal',
        confirmedDelete: true,
      }),
    );
    const toasts = await screen.findAllByRole('status');
    expect(toasts[0]).toHaveTextContent('c_a.fits was deleted; the original is being re-fetched');
  });

  it('a re-fetch that trashed the changed file says so', async () => {
    changedFile = () => Promise.resolve({ trashed: true });
    renderIt();
    fireEvent.click(await screen.findByRole('button', { name: 'Re-fetch original' }));
    const toasts = await screen.findAllByRole('status');
    expect(toasts[0]).toHaveTextContent('c_a.fits moved to the Trash; the original is being re-fetched');
    expect(api.invoke).toHaveBeenCalledWith('resolve_collab_changed_file', {
      projectId: 'p1',
      frameUuid: 'u1',
      action: 'refetchOriginal',
      confirmedDelete: false,
    });
  });

  it('delete asks first, then deletes without a re-fetch', async () => {
    renderIt();
    fireEvent.click(await screen.findByRole('button', { name: 'Delete' }));
    const confirm = await screen.findByText('Delete the changed file? It will not be re-fetched.');
    expect(api.invoke).not.toHaveBeenCalledWith('resolve_collab_changed_file', expect.anything());
    const dialog = confirm.closest('div') as HTMLElement;
    fireEvent.click(within(dialog.parentElement as HTMLElement).getByRole('button', { name: 'Delete' }));
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('resolve_collab_changed_file', {
        projectId: 'p1',
        frameUuid: 'u1',
        action: 'delete',
        confirmedDelete: true,
      }),
    );
  });

  it('keep again works per frame and for all', async () => {
    renderIt();
    fireEvent.click(await screen.findByRole('button', { name: 'Keep again' }));
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('keep_collab_frames_again', { projectId: 'p1', frameUuids: ['u3'] }));
    fireEvent.click(screen.getByRole('button', { name: 'Keep all again' }));
    await waitFor(() => expect(api.invoke).toHaveBeenCalledWith('keep_collab_frames_again', { projectId: 'p1', frameUuids: null }));
  });

  it('a failed action is one warning notification', async () => {
    vi.mocked(api.invoke).mockImplementation(((cmd: string) => {
      if (cmd === 'list_collab_attention') return Promise.resolve(attention);
      if (cmd === 'keep_collab_frames_again') return Promise.reject('collaboration project refused');
      return Promise.resolve(1);
    }) as never);
    renderIt();
    fireEvent.click(await screen.findByRole('button', { name: 'Keep all again' }));
    const toasts = await screen.findAllByRole('status');
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toHaveTextContent('Keep again failed');
  });
});
