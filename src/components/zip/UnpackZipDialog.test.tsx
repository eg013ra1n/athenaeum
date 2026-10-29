// The Unpack-from-zip dialog: default folder, progress, result with conflicts, per-run notification keys,
// and no Cancel (the run cannot be cancelled).
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { api } from '../../api';
import { UnpackZipDialog } from './UnpackZipDialog';
import { ArchiveProgress } from '../archive/ArchiveProgress';
import type { UnpackOutcome } from '../../types/archive';

vi.mock('../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));
vi.mock('../../api/desktop', () => ({ pickDirectory: vi.fn() }));
vi.mock('../../api/archive', () => ({ cancelArchiveOperation: vi.fn() }));
vi.mock('../../utils/platform', () => ({ isTauri: true, isMac: false }));
vi.mock('../FolderBrowserModal', () => ({ FolderBrowserModal: () => null }));
const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../../contexts/NotificationContext', () => ({ useNotifications: () => ({ notify: notifyMock }) }));

const listeners = new Map<string, (p: unknown) => void>();
let outcome: UnpackOutcome;

beforeEach(() => {
  notifyMock.mockReset();
  listeners.clear();
  outcome = { unpacked: 3, adopted: 0, conflicts: [] };
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation((async (cmd: string) => (cmd === 'unpack_zip_frames' ? outcome : null)) as never);
  vi.mocked(api.listen).mockImplementation(((name: string, cb: (p: unknown) => void) => {
    listeners.set(name, cb);
    return Promise.resolve(() => { listeners.delete(name); });
  }) as never);
});

function renderDialog(onDone = vi.fn(), onClose = vi.fn()) {
  const { unmount } = render(
    <UnpackZipDialog
      target={{ kind: 'frameSet', id: 5 }}
      frameCount={3}
      zipCount={1}
      defaultZipPath="/astro/M31 night.zip"
      onClose={onClose}
      onDone={onDone}
    />,
  );
  return { onDone, onClose, unmount };
}

describe('UnpackZipDialog', () => {
  it('offers a folder named like the zip, next to it, and no Cancel for the run', async () => {
    renderDialog();
    expect(screen.getByLabelText('Unpack to')).toHaveValue('/astro/M31 night');
    const dialog = screen.getByRole('dialog');
    expect(within(dialog).getByText(/3 frames of this set are inside a zip/)).toBeInTheDocument();
    // Before the run the footer's "Cancel" closes the dialog; while it runs nothing can stop it.
    expect(within(dialog).getByRole('button', { name: 'Cancel' })).toBeEnabled();
  });

  it('unpacks to the chosen folder and shows the result, reloading the page on close', async () => {
    outcome = { unpacked: 2, adopted: 0, conflicts: ['L_003.fits: a different file is already there'] };
    const { onDone, onClose } = renderDialog();
    fireEvent.change(screen.getByLabelText('Unpack to'), { target: { value: '/astro/out' } });
    fireEvent.click(screen.getByRole('button', { name: 'Unpack' }));
    await waitFor(() => expect(vi.mocked(api.invoke)).toHaveBeenCalledWith('unpack_zip_frames', { target: { kind: 'frameSet', id: 5 }, destDir: '/astro/out' }));
    expect(await screen.findByText('2 unpacked, 1 left in the zip')).toBeInTheDocument();
    expect(screen.getByText('L_003.fits: a different file is already there')).toBeInTheDocument();
    expect(onDone).not.toHaveBeenCalled(); // reloading now would unmount the dialog before it is read
    // Two buttons are named Close: the title bar's and the footer's.
    fireEvent.click(screen.getAllByRole('button', { name: 'Close' }).pop() as HTMLElement);
    expect(onDone).toHaveBeenCalledTimes(1);
    expect(onClose).toHaveBeenCalledTimes(1);
  });

  it('shows progress while it runs', async () => {
    let finish: (o: UnpackOutcome) => void = () => {};
    vi.mocked(api.invoke).mockImplementation((() => new Promise<UnpackOutcome>((res) => { finish = res; })) as never);
    renderDialog();
    fireEvent.click(screen.getByRole('button', { name: 'Unpack' }));
    await screen.findByText('Starting…');
    act(() => listeners.get('zip-unpack-progress')?.({ current: 1, total: 3, message: 'L_001.fits' }));
    expect(await screen.findByText('1 / 3')).toBeInTheDocument();
    expect(screen.getByText(/can't be cancelled/)).toBeInTheDocument();
    expect(screen.getByRole('button', { name: 'Cancel' })).toBeDisabled();
    await act(async () => finish({ unpacked: 3, adopted: 0, conflicts: [] }));
  });

  it('never passes a dedupe key, so notifications survive a restart that resets the run counter', async () => {
    // NotificationContext persists seen dedupe keys; a key from a per-launch counter would be dropped after
    // a restart. Two runs in two "launches" (module reloaded, counter back at 1) must both notify.
    for (let launch = 0; launch < 2; launch++) {
      vi.resetModules();
      const fresh = await import('./UnpackZipDialog');
      const { unmount } = render(
        <fresh.UnpackZipDialog target={{ kind: 'frameSet', id: 5 }} frameCount={3} zipCount={1} defaultZipPath="/astro/M31 night.zip" onClose={vi.fn()} onDone={vi.fn()} />,
      );
      fireEvent.click(screen.getByRole('button', { name: 'Unpack' }));
      await waitFor(() => expect(notifyMock).toHaveBeenCalledTimes(launch + 1));
      unmount();
    }
    for (const c of notifyMock.mock.calls) expect(c[0].dedupeKey).toBeUndefined();
  });

  it('notifies once for a run whose finished event and result both arrive', async () => {
    renderDialog();
    fireEvent.click(screen.getByRole('button', { name: 'Unpack' }));
    await act(async () => { await Promise.resolve(); });
    act(() => listeners.get('archive-finished')?.({ operation_id: 0, kind: 'unpack', outcome: 'completed', unpacked: 3, adopted: 0, conflicts: 0 }));
    await screen.findByText('3 unpacked');
    expect(notifyMock).toHaveBeenCalledTimes(1);
    expect(notifyMock.mock.calls[0][0]).toMatchObject({ title: 'Unpacked 3 frames', tone: 'success' });
  });

  it('shows a refused request in the dialog and notifies', async () => {
    vi.mocked(api.invoke).mockImplementation((async () => { throw 'the destination is outside the allowed folders'; }) as never);
    renderDialog();
    fireEvent.click(screen.getByRole('button', { name: 'Unpack' }));
    expect(await screen.findByRole('alert')).toHaveTextContent('outside the allowed folders');
    expect(notifyMock).toHaveBeenCalledWith(expect.objectContaining({ title: 'Unpack failed', hasErrors: true }));
  });

  it('notifies a failed run once, with the reason from the rejected call, not from the bare event', async () => {
    let fail: (e: unknown) => void = () => {};
    vi.mocked(api.invoke).mockImplementation((() => new Promise((_, rej) => { fail = rej; })) as never);
    renderDialog();
    fireEvent.click(screen.getByRole('button', { name: 'Unpack' }));
    await screen.findByText('Starting…');
    // The backend emits the event before the call rejects.
    act(() => listeners.get('archive-finished')?.({ operation_id: 0, kind: 'unpack', outcome: 'failed' }));
    expect(notifyMock).not.toHaveBeenCalled();
    await act(async () => fail('database is locked'));
    expect(notifyMock).toHaveBeenCalledTimes(1);
    expect(notifyMock.mock.calls[0][0]).toMatchObject({ title: 'Unpack failed', detail: 'database is locked', hasErrors: true });
  });
});

describe('ArchiveProgress and unpack events', () => {
  it('ignores the finished event of an unpack, which the unpack dialog reports itself', async () => {
    render(<ArchiveProgress operationId={0} />);
    await waitFor(() => expect(listeners.has('archive-finished')).toBe(true));
    act(() => listeners.get('archive-finished')?.({ operation_id: 0, kind: 'unpack', outcome: 'completed' }));
    expect(notifyMock).not.toHaveBeenCalled();
  });
});
