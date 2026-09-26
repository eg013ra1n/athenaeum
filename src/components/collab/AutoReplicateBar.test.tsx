import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { NotificationProvider } from '../../contexts/NotificationContext';
import { ToastStack } from '../Toast';
import AutoReplicateBar from './AutoReplicateBar';
import { api } from '../../api';

vi.mock('../../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
}));

beforeEach(() => {
  localStorage.clear();
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((command: string) => {
    if (command === 'collab_sync_now') {
      return Promise.reject('the collab store is not mounted');
    }
    return Promise.resolve(null);
  }) as never);
  vi.mocked(api.listen).mockImplementation((() => Promise.resolve(() => {})) as never);
});

function renderBar() {
  return render(
    <MemoryRouter>
      <NotificationProvider>
        <AutoReplicateBar
          projectId="proj-1"
          autoReplicate
          autoPublish
          publishedBytes={null}
          onToggled={() => {}}
          onSynced={() => {}}
        />
        <ToastStack />
      </NotificationProvider>
    </MemoryRouter>,
  );
}

describe('AutoReplicateBar', () => {
  it('every failed "Sync now" is its own toast — a persisted key never swallows a later failure (final review I4)', async () => {
    const first = renderBar();
    fireEvent.click(await screen.findByRole('button', { name: 'Sync now' }));
    expect(await screen.findAllByRole('status')).toHaveLength(1);
    // "Sync now" is the one global command (L10) — no project argument.
    expect(api.invoke).toHaveBeenCalledWith('collab_sync_now');
    expect(api.invoke).not.toHaveBeenCalledWith('sync_project_now', expect.anything());

    // A second failure in the same session…
    await waitFor(() => expect(screen.getByRole('button', { name: 'Sync now' })).not.toBeDisabled());
    fireEvent.click(screen.getByRole('button', { name: 'Sync now' }));
    await waitFor(() => expect(screen.getAllByRole('status')).toHaveLength(2));
    first.unmount();

    // …and one after a restart, with the persisted notification history.
    renderBar();
    fireEvent.click(await screen.findByRole('button', { name: 'Sync now' }));
    const toasts = await screen.findAllByRole('status');
    expect(toasts).toHaveLength(1);
    expect(toasts[0]).toHaveTextContent('Sync now failed');
  });
});
