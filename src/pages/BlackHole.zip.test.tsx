// Black Hole page: the "Zip archives" group restores and voids whole zips by their row id, and a refused
// Void of a single zip frame shows the refusal text.
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor, within } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { api } from '../api';
import BlackHole from './BlackHole';

vi.mock('../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));
vi.mock('../components/HistoryNav', () => ({ HistoryNav: () => null }));
const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));

const ZIP = {
  id: 41,
  zipPath: '/astro/night.zip',
  size: 5_000_000,
  frameEntries: 12,
  archiveOperationId: 9,
  fromWhere: 'file_browser',
  movedAt: '2026-10-20T10:00:00Z',
};
const ZIP_FRAME = {
  id: 7,
  file_id: 70,
  filename: 'L_001.fits',
  original_path: '/astro/other.zip/Lights/L_001.fits',
  from_where: 'blink',
  moved_at: '2026-10-21T10:00:00Z',
  file_size: 2048,
  archive_zip_path: '/astro/other.zip',
};
// A frame of one of OUR zips: its original path is on disk, only archive_zip_path says it is in a zip.
const OUR_ZIP_FRAME = {
  ...ZIP_FRAME,
  id: 8,
  file_id: 80,
  filename: 'L_002.fits',
  original_path: '/astro/M31/Lights/L_002.fits',
  archive_zip_path: '/astro/night.zip',
};
let frames: unknown[] = [ZIP_FRAME];
const VOID_REFUSAL = 'this frame is inside other.zip; only the whole zip can be removed';

let listener: ((p: unknown) => void) | null = null;

beforeEach(() => {
  notifyMock.mockReset();
  listener = null;
  frames = [ZIP_FRAME];
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((cmd: string) => {
    switch (cmd) {
      case 'get_black_hole_files':
        return Promise.resolve(frames);
      case 'get_black_hole_zips':
        return Promise.resolve([ZIP]);
      case 'send_to_void':
        return Promise.reject(VOID_REFUSAL);
      default:
        return Promise.resolve(null);
    }
  }) as never);
  vi.mocked(api.listen).mockImplementation(((_e: string, cb: (p: unknown) => void) => {
    listener = cb;
    return Promise.resolve(() => {});
  }) as never);
});

const calls = (cmd: string) => vi.mocked(api.invoke).mock.calls.filter(([c]) => c === cmd);

function renderPage() {
  return render(<MemoryRouter><BlackHole /></MemoryRouter>);
}

describe('BlackHole — zip archives', () => {
  it('lists the zip with its frame count, the only-copy mark and the exclusion line', async () => {
    renderPage();
    const group = await screen.findByTestId('zip-group');
    expect(within(group).getByText('night.zip')).toBeInTheDocument();
    expect(within(group).getByText('12 frames')).toBeInTheDocument();
    expect(within(group).getByText('only copy')).toBeInTheDocument();
    expect(group).toHaveTextContent('left out of stacking, export and masters');
  });

  it('restores a selected zip by its row id', async () => {
    renderPage();
    const group = await screen.findByTestId('zip-group');
    fireEvent.click(within(group).getByRole('button', { name: 'Select night.zip' }));
    fireEvent.click(screen.getByRole('button', { name: /^Restore$/ }));
    fireEvent.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'Confirm' }));
    await waitFor(() => expect(calls('restore_zip_from_black_hole')).toHaveLength(1));
    expect(calls('restore_zip_from_black_hole')[0][1]).toEqual({ id: 41 });
  });

  it('voids a selected zip by its row id', async () => {
    renderPage();
    const group = await screen.findByTestId('zip-group');
    fireEvent.click(within(group).getByRole('button', { name: 'Select night.zip' }));
    fireEvent.click(screen.getByRole('button', { name: /Send to Void/ }));
    const dialog = await screen.findByRole('dialog');
    expect(dialog).toHaveTextContent('12 frames');
    fireEvent.click(within(dialog).getByRole('button', { name: 'Confirm' }));
    await waitFor(() => expect(calls('send_zip_to_void')).toHaveLength(1));
    expect(calls('send_zip_to_void')[0][1]).toEqual({ id: 41 });
  });

  it('shows the refusal when a single zip frame cannot be voided', async () => {
    renderPage();
    await screen.findByTestId('zip-group');
    expect(await screen.findByText('in zip')).toBeInTheDocument();
    fireEvent.click(screen.getByRole('button', { name: /Select All$/ }));
    // Only the file; leave the zip out of this Void.
    fireEvent.click(screen.getByRole('button', { name: 'Deselect all zips' }));
    fireEvent.click(screen.getByRole('button', { name: /Send to Void/ }));
    const dialog = await screen.findByRole('dialog');
    fireEvent.click(within(dialog).getByRole('button', { name: 'Confirm' }));
    await waitFor(() => expect(calls('send_to_void')).toHaveLength(1));
    expect(await screen.findByText(new RegExp(VOID_REFUSAL))).toBeInTheDocument();
  });

  it('reloads the zip rows when a zip action arrives', async () => {
    renderPage();
    await screen.findByTestId('zip-group');
    const before = calls('get_black_hole_zips').length;
    listener?.({ id: 41, zip_id: 3, action: 'zip-restored' });
    await waitFor(() => expect(calls('get_black_hole_zips').length).toBe(before + 1));
  });

  it('marks a frame of one of our own zips as in a zip, though its original path is on disk', async () => {
    frames = [OUR_ZIP_FRAME];
    renderPage();
    await screen.findByTestId('zip-group');
    expect(await screen.findByText('in zip')).toBeInTheDocument();
  });

  it('Void with a zip and one of its own frames selected voids the zip and skips the frame', async () => {
    frames = [OUR_ZIP_FRAME];
    renderPage();
    await screen.findByTestId('zip-group');
    fireEvent.click(screen.getByRole('button', { name: /Select All$/ }));
    fireEvent.click(screen.getByRole('button', { name: /Send to Void/ }));
    fireEvent.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'Confirm' }));
    await waitFor(() => expect(calls('send_zip_to_void')).toHaveLength(1));
    expect(calls('send_zip_to_void')[0][1]).toEqual({ id: 41 });
    expect(calls('send_to_void')).toHaveLength(0);
    expect(screen.queryByText(/Query returned no rows/)).toBeNull();
  });
});
