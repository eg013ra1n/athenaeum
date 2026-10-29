// The browser's zip view: a zip is one row, opening it lists its entries read-only, and F8 on the row moves
// the whole zip to the Black Hole after a confirmation.
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor, within } from '@testing-library/react';
import { api } from '../../api';
import type { ScanRootWithAvailability } from '../../types/helpers';
import DualPaneFileBrowser from './DualPaneFileBrowser';

vi.mock('../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));
vi.mock('../../api/desktop', () => ({ revealItemInDir: vi.fn() }));
vi.mock('../../utils/platform', () => ({ isTauri: true, isMac: false }));
vi.mock('../BlinkViewer', () => ({ default: () => null }));
vi.mock('../transfers/SendToNodeDialog', () => ({ SendToNodeDialog: () => null }));
vi.mock('./MetadataPane', () => ({ default: () => null }));
vi.mock('./CatalogSearch', () => ({ default: () => null }));

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));

const ROOT = '/astro';
const ZIP = '/astro/night.zip';
const roots = [{ id: 1, path: ROOT, is_available: true }] as unknown as ScanRootWithAvailability[];

function frameFile(path: string, filename: string) {
  return {
    file: { id: 1, path, filename, size: 2048, format: 'FITS' },
    frame: { id: 1, imagetyp: 'LIGHT', object: 'M31', date_obs: '2025-10-12T20:00:00', instrume: 'C', override_: false },
  };
}

let createdByOperation: number | null = null;
let inBlackHole = false;

beforeEach(() => {
  notifyMock.mockReset();
  createdByOperation = null;
  inBlackHole = false;
  localStorage.clear();
  URL.createObjectURL = vi.fn(() => 'blob:preview');
  URL.revokeObjectURL = vi.fn();
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((cmd: string, args?: { innerDir?: string }) => {
    switch (cmd) {
      case 'get_directory_contents':
        return Promise.resolve({
          subdirectories: [],
          files: [],
          zips: [{ path: ZIP, size: 1_000_000, frameEntries: 3, createdByOperation, inBlackHole }],
        });
      case 'get_zip_entry_header':
        return Promise.resolve({
          entry: (args as { entry?: string } | undefined)?.entry,
          frame: { object: 'M31', filter: 'Ha', imagetyp: 'Light', exptime: 300, date_obs: '2025-10-12T20:00:00' },
          header: 'SIMPLE  = T\nOBJECT  = M31',
        });
      case 'preview_zip_entry':
        return Promise.resolve(new Uint8Array([0xff, 0xd8, 0xff, 0xd9]));
      case 'list_zip_entries':
        return Promise.resolve(
          args?.innerDir === 'Lights'
            ? { subdirectories: [], files: [frameFile(`${ZIP}/Lights/L_001.fits`, 'L_001.fits')], otherEntries: [] }
            : {
                subdirectories: ['Lights'],
                files: [frameFile(`${ZIP}/top.fits`, 'top.fits')],
                otherEntries: [
                  { name: 'notes.txt', size: 5, released: false, copyPath: null, readable: false },
                  { name: 'L_007.fits', size: 9, released: true, copyPath: '/astro/out/L_007.fits', readable: false },
                  { name: 'L_008.fits', size: 9, released: false, copyPath: null, readable: true },
                ],
              },
        );
      default:
        return Promise.resolve(null);
    }
  }) as never);
  vi.mocked(api.listen).mockImplementation((() => Promise.resolve(() => {})) as never);
});

const calls = (cmd: string) => vi.mocked(api.invoke).mock.calls.filter(([c]) => c === cmd);
const lastCall = (cmd: string) => { const c = calls(cmd); return c[c.length - 1]; };
const MUTATIONS = ['move_to_black_hole', 'bulk_move_to_black_hole', 'move_zip_to_black_hole', 'rename_path', 'mkdir_in_scan_root', 'enqueue_move_operation'];

async function openZip() {
  render(<DualPaneFileBrowser scanRoots={roots} />);
  const rows = await screen.findAllByText('night.zip');
  fireEvent.doubleClick(rows[0]);
  await waitFor(() => expect(calls('list_zip_entries')).not.toHaveLength(0));
}

describe('DualPaneFileBrowser — zips', () => {
  it('enters a zip on double-click and lists its entries read-only', async () => {
    await openZip();
    expect(calls('list_zip_entries')[0][1]).toEqual({ zipPath: ZIP, innerDir: '' });
    expect((await screen.findAllByText('top.fits')).length).toBeGreaterThan(0);
    expect(screen.getAllByText('Lights').length).toBeGreaterThan(0);
    // Non-frame entries are listed, never openable.
    const other = screen.getAllByText('notes.txt')[0].closest('div');
    expect(other).toHaveAttribute('aria-disabled', 'true');

    // Select the entry, then try every mutation key: nothing is called, the reason is on the status line.
    fireEvent.click(screen.getAllByText('top.fits')[0]);
    for (const key of ['F8', 'F2', 'F7', 'F6']) fireEvent.keyDown(window, { key });
    for (const cmd of MUTATIONS) expect(calls(cmd)).toHaveLength(0);
    // The plain-file prompts (Black Hole confirm, Rename, New folder, Move) are the local `Modal`, a
    // full-screen overlay with no dialog role: without the in-zip guards one of them would be on screen.
    expect(document.querySelector('.fixed.inset-0')).toBeNull();
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(screen.getByTestId('zip-note')).toHaveTextContent('Inside a zip');
    const blocked = screen.getAllByTitle(/Inside a zip/).filter((el) => el.tagName === 'BUTTON');
    expect(blocked).toHaveLength(4); // Rename, Move, Mkdir, Delete
    blocked.forEach((b) => expect(b).toBeDisabled());
  });

  it('marks an unpacked entry with a live copy and locates it; an entry without a link reads like a frame', async () => {
    await openZip();
    const present = (await screen.findAllByText('L_007.fits'))[0].closest('div') as HTMLElement;
    expect(within(present).getByText('unpacked')).toBeInTheDocument();
    // No live link: a normal frame row, no chip, no Locate, and no "copy missing" warning any more.
    const unlinked = screen.getAllByText('L_008.fits')[0].closest('div') as HTMLElement;
    expect(within(unlinked).queryByText(/unpacked/)).toBeNull();
    expect(within(unlinked).queryByText('Locate')).toBeNull();
    expect(screen.queryByText(/rescan to restore/)).toBeNull();
    // An entry that was never unpacked carries neither.
    const plain = screen.getAllByText('notes.txt')[0].closest('div') as HTMLElement;
    expect(within(plain).queryByText(/unpacked/)).toBeNull();
    expect(within(plain).queryByText('Locate')).toBeNull();

    const before = calls('get_directory_contents').length;
    fireEvent.click(within(present).getByText('Locate'));
    await waitFor(() => expect(calls('get_directory_contents').length).toBeGreaterThan(before));
    expect(lastCall('get_directory_contents')[1]).toMatchObject({ directoryPath: '/astro/out' });
    for (const cmd of MUTATIONS) expect(calls(cmd)).toHaveLength(0);
  });

  it('shows an entry without a row as an ordinary row: no extra line, the preview on double-click', async () => {
    await openZip();
    const row = (await screen.findAllByText('L_008.fits'))[0];
    const button = row.closest('button') as HTMLElement;
    fireEvent.click(row);
    expect(button).toHaveClass('bg-accent/30');
    // Metadata is the metadata pane's job: a pick reads nothing and draws nothing under the row.
    expect(screen.queryByTestId('zip-entry-summary')).toBeNull();
    expect(calls('get_zip_entry_header')).toHaveLength(0);
    expect(calls('preview_zip_entry')).toHaveLength(0);

    fireEvent.doubleClick(row);
    await waitFor(() => expect(calls('preview_zip_entry')).toHaveLength(1));
    expect(calls('preview_zip_entry')[0][1]).toEqual({ zipPath: ZIP, entry: 'L_008.fits', resolution: 'preview' });
    expect(await screen.findByRole('dialog')).toBeInTheDocument();
    // Nothing was written to the catalog.
    for (const cmd of MUTATIONS) expect(calls(cmd)).toHaveLength(0);
  });

  it('opens inner folders and leaves the zip with up', async () => {
    await openZip();
    const lights = (await screen.findAllByText('Lights'))[0];
    fireEvent.doubleClick(lights);
    await waitFor(() => expect(lastCall('list_zip_entries')[1]).toEqual({ zipPath: ZIP, innerDir: 'Lights' }));
    expect((await screen.findAllByText('L_001.fits')).length).toBeGreaterThan(0);

    fireEvent.keyDown(window, { key: 'ArrowUp', altKey: true });
    await waitFor(() => expect(lastCall('list_zip_entries')[1]).toEqual({ zipPath: ZIP, innerDir: '' }));
    const before = calls('get_directory_contents').length;
    fireEvent.keyDown(window, { key: 'ArrowUp', altKey: true });
    await waitFor(() => expect(screen.queryByTestId('zip-note')).toBeNull());
    await waitFor(() => expect(calls('get_directory_contents').length).toBeGreaterThanOrEqual(before));
    expect(screen.getAllByText('night.zip').length).toBeGreaterThan(0);
  });

  it('F8 on a zip row asks for confirmation with the frame count', async () => {
    render(<DualPaneFileBrowser scanRoots={roots} />);
    fireEvent.click((await screen.findAllByText('night.zip'))[0]);
    fireEvent.keyDown(window, { key: 'F8' });
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByText(/Move night\.zip to the Black Hole\?/)).toBeInTheDocument();
    expect(within(dialog).getByLabelText('Zips to move')).toHaveTextContent('3 frames');
    expect(within(dialog).queryByRole('alert')).toBeNull();
    expect(calls('move_zip_to_black_hole')).toHaveLength(0);

    fireEvent.click(within(dialog).getByRole('button', { name: 'Move to Black Hole' }));
    await waitFor(() => expect(calls('move_zip_to_black_hole')).toHaveLength(1));
    expect(calls('move_zip_to_black_hole')[0][1]).toEqual({ zipPath: ZIP, fromWhere: 'file_browser' });
  });

  it('warns that an archive written by Move and ZIP is the only copy', async () => {
    createdByOperation = 7;
    render(<DualPaneFileBrowser scanRoots={roots} />);
    fireEvent.click((await screen.findAllByText('night.zip'))[0]);
    fireEvent.keyDown(window, { key: 'F8' });
    const dialog = await screen.findByRole('dialog');
    expect(within(dialog).getByRole('alert')).toHaveTextContent('This is the only copy of an archived frame set');
  });

  it('shows a refused zip move instead of swallowing it', async () => {
    const base = vi.mocked(api.invoke).getMockImplementation()!;
    vi.mocked(api.invoke).mockImplementation(((cmd: string, args?: unknown) =>
      cmd === 'move_zip_to_black_hole' ? Promise.reject('the zip is already in the Black Hole') : base(cmd, args as never)) as never);
    render(<DualPaneFileBrowser scanRoots={roots} />);
    fireEvent.click((await screen.findAllByText('night.zip'))[0]);
    fireEvent.keyDown(window, { key: 'F8' });
    fireEvent.click(within(await screen.findByRole('dialog')).getByRole('button', { name: 'Move to Black Hole' }));
    await waitFor(() => expect(notifyMock).toHaveBeenCalled());
    expect(notifyMock.mock.calls[0][0]).toMatchObject({ hasErrors: true, tone: 'warning' });
    expect(notifyMock.mock.calls[0][0].detail).toContain('already in the Black Hole');
  });

  it('reveals a virtual path by opening its zip at the entry folder and selecting it', async () => {
    render(<DualPaneFileBrowser scanRoots={roots} reveal={{ path: `${ZIP}/Lights/L_001.fits`, token: 1 }} />);
    await waitFor(() => expect(calls('list_zip_entries')).not.toHaveLength(0));
    expect(calls('list_zip_entries')[0][1]).toEqual({ zipPath: ZIP, innerDir: 'Lights' });
    const row = (await screen.findAllByText('L_001.fits'))[0].closest('button');
    await waitFor(() => expect(row).toHaveClass('bg-accent/30'));
    expect(screen.getByTestId('zip-note')).toBeInTheDocument();
  });

  it('falls back to the folder when the path only looks like a zip', async () => {
    const base = vi.mocked(api.invoke).getMockImplementation()!;
    vi.mocked(api.invoke).mockImplementation(((cmd: string, args?: unknown) =>
      cmd === 'list_zip_entries' ? Promise.reject('not a zip') : base(cmd, args as never)) as never);
    render(<DualPaneFileBrowser scanRoots={roots} reveal={{ path: '/astro/x.zip/a.fits', token: 1 }} />);
    await waitFor(() => expect(calls('get_directory_contents').some(([, a]) => (a as { directoryPath: string }).directoryPath === '/astro/x.zip')).toBe(true));
    expect(screen.queryByTestId('zip-note')).toBeNull();
  });

  it('marks a zip that is in the Black Hole and does not move it a second time', async () => {
    inBlackHole = true;
    render(<DualPaneFileBrowser scanRoots={roots} />);
    expect((await screen.findAllByText('Black Hole')).length).toBeGreaterThan(0);
    fireEvent.click((await screen.findAllByText('night.zip'))[0]);
    fireEvent.keyDown(window, { key: 'F8' });
    await waitFor(() => expect(notifyMock).toHaveBeenCalled());
    expect(notifyMock.mock.calls[0][0].title).toBe('That zip is already in the Black Hole');
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(calls('move_zip_to_black_hole')).toHaveLength(0);
  });

  it('reveals a file of one of our own zips by the zip the caller names, selecting its original path', async () => {
    const ORIGINAL = '/astro/M31/Lights/L_001.fits';
    const base = vi.mocked(api.invoke).getMockImplementation()!;
    vi.mocked(api.invoke).mockImplementation(((cmd: string, args?: { innerDir?: string }) =>
      cmd === 'list_zip_entries'
        ? Promise.resolve({ subdirectories: [], files: [frameFile(ORIGINAL, 'L_001.fits')], otherEntries: [] })
        : base(cmd, args as never)) as never);
    render(<DualPaneFileBrowser scanRoots={roots} reveal={{ path: ORIGINAL, token: 1, zip: { zipPath: ZIP, innerDir: 'Lights' } }} />);
    await waitFor(() => expect(calls('list_zip_entries')).not.toHaveLength(0));
    expect(calls('list_zip_entries')[0][1]).toEqual({ zipPath: ZIP, innerDir: 'Lights' });
    const row = (await screen.findAllByText('L_001.fits'))[0].closest('button');
    await waitFor(() => expect(row).toHaveClass('bg-accent/30'));
    expect(screen.getByTestId('zip-note')).toBeInTheDocument();
  });

  it('tries the next .zip component when the leftmost one is a folder', async () => {
    const base = vi.mocked(api.invoke).getMockImplementation()!;
    vi.mocked(api.invoke).mockImplementation(((cmd: string, args?: { zipPath?: string; innerDir?: string }) =>
      cmd === 'list_zip_entries' && args?.zipPath === '/astro/old.zip'
        ? Promise.reject('not a zip')
        : base(cmd, args as never)) as never);
    render(<DualPaneFileBrowser scanRoots={roots} reveal={{ path: '/astro/old.zip/night.zip/Lights/L_001.fits', token: 1 }} />);
    await waitFor(() => expect(calls('list_zip_entries').length).toBe(2));
    expect(calls('list_zip_entries')[0][1]).toEqual({ zipPath: '/astro/old.zip', innerDir: 'night.zip/Lights' });
    expect(calls('list_zip_entries')[1][1]).toEqual({ zipPath: '/astro/old.zip/night.zip', innerDir: 'Lights' });
    await waitFor(() => expect(screen.getByTestId('zip-note')).toBeInTheDocument());
  });

  it('Send on a zip row or inside a zip says the frames are inside a zip', async () => {
    render(<DualPaneFileBrowser scanRoots={roots} />);
    fireEvent.click((await screen.findAllByText('night.zip'))[0]);
    fireEvent.click(screen.getByRole('button', { name: /^Send/ }));
    await waitFor(() => expect(notifyMock).toHaveBeenCalled());
    expect(notifyMock.mock.calls[0][0].title).toBe('Nothing sent: the frames are inside a zip');
    expect(notifyMock.mock.calls[0][0].detail).toContain('unpack it first');
    expect(calls('resolve_frame_ids_for_paths')).toHaveLength(0);
  });
});
