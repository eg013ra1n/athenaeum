// Readable entries of a zip that the catalog holds no row for are ordinary rows of the pane selection: click,
// Shift-click, Cmd+A and the arrows treat them like files, their metadata is in the metadata pane (⌘I), double-click
// opens the preview dialog, and no command that takes paths ever receives an entry key.
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor, within } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { api } from '../../api';
import type { ScanRootWithAvailability } from '../../types/helpers';
import DualPaneFileBrowser from './DualPaneFileBrowser';

vi.mock('../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));
vi.mock('../../api/desktop', () => ({ revealItemInDir: vi.fn(() => Promise.resolve()) }));
vi.mock('../../utils/platform', () => ({ isTauri: true, isMac: false }));
vi.mock('../BlinkViewer', () => ({ default: () => null }));
vi.mock('../transfers/SendToNodeDialog', () => ({ SendToNodeDialog: () => null }));
vi.mock('./CatalogSearch', () => ({ default: () => null }));
vi.mock('../plate-solve/PlateSolveBatchPanel', () => ({ PlateSolveBatchPanel: () => null }));

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));

const ROOT = '/astro';
const ZIP = '/astro/night.zip';
const roots = [{ id: 1, path: ROOT, is_available: true }] as unknown as ScanRootWithAvailability[];

const EXPOSURES: Record<string, number> = { 'E_001.fits': 15, 'E_002.fits': 30, 'E_003.fits': 15 };

beforeEach(() => {
  notifyMock.mockReset();
  localStorage.clear();
  URL.createObjectURL = vi.fn(() => 'blob:preview');
  URL.revokeObjectURL = vi.fn();
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((cmd: string, args?: { entry?: string; innerDir?: string }) => {
    switch (cmd) {
      case 'get_directory_contents':
        return Promise.resolve({
          subdirectories: [],
          files: [],
          zips: [{ path: ZIP, size: 1_000_000, frameEntries: 4, createdByOperation: null, inBlackHole: false }],
        });
      case 'list_zip_entries':
        return Promise.resolve({
          subdirectories: [],
          files: [
            {
              file: { id: 5, path: `${ZIP}/top.fits`, filename: 'top.fits', size: 2048, format: 'FITS' },
              frame: { id: 5, imagetyp: 'LIGHT', object: 'M31', date_obs: '2025-10-12T20:00:00', instrume: 'C', override_: false },
            },
          ],
          otherEntries: [
            { name: 'E_001.fits', size: 11, released: false, copyPath: null, readable: true },
            { name: 'E_002.fits', size: 12, released: false, copyPath: null, readable: true },
            { name: 'E_003.fits', size: 13, released: false, copyPath: null, readable: true },
            { name: 'notes.txt', size: 5, released: false, copyPath: null, readable: false },
          ],
        });
      case 'get_zip_entry_header':
        return Promise.resolve({
          entry: args?.entry,
          frame: {
            object: 'GAIA',
            filter: 'L',
            imagetyp: 'Light',
            exptime: EXPOSURES[args?.entry ?? ''] ?? null,
            date_obs: '2025-10-25T21:00:00',
          },
          header: 'SIMPLE  = T\nOBJECT  = GAIA',
        });
      case 'preview_zip_entry':
        return Promise.resolve(new Uint8Array([0xff, 0xd8, 0xff, 0xd9]));
      default:
        return Promise.resolve(null);
    }
  }) as never);
  vi.mocked(api.listen).mockImplementation((() => Promise.resolve(() => {})) as never);
});

const calls = (cmd: string) => vi.mocked(api.invoke).mock.calls.filter(([c]) => c === cmd);
const MUTATIONS = ['move_to_black_hole', 'bulk_move_to_black_hole', 'move_zip_to_black_hole', 'rename_path', 'mkdir_in_scan_root', 'enqueue_move_operation', 'resolve_frame_ids_for_paths'];
const rowOf = (name: string) => screen.getAllByText(name)[0].closest('button') as HTMLElement;

async function openZip() {
  render(<MemoryRouter><DualPaneFileBrowser scanRoots={roots} /></MemoryRouter>);
  fireEvent.doubleClick((await screen.findAllByText('night.zip'))[0]);
  await waitFor(() => expect(calls('list_zip_entries')).not.toHaveLength(0));
  await screen.findAllByText('E_001.fits');
}

describe('DualPaneFileBrowser — zip entries without a catalog row', () => {
  it('Cmd+A in the zip selects the rowless entries with the files; non-frame entries stay out', async () => {
    await openZip();
    fireEvent.keyDown(window, { key: 'a', ctrlKey: true });
    for (const name of ['top.fits', 'E_001.fits', 'E_002.fits', 'E_003.fits']) {
      expect(rowOf(name)).toHaveClass('bg-accent/30');
    }
    expect(screen.getByText(/4 selected \(/)).toBeInTheDocument();
    expect(screen.getAllByText('notes.txt')[0].closest('div')).toHaveAttribute('aria-disabled', 'true');
  });

  it('a click selects the row without drawing a line under it or reading a header', async () => {
    await openZip();
    fireEvent.click(screen.getAllByText('E_002.fits')[0]);
    expect(rowOf('E_002.fits')).toHaveClass('bg-accent/30');
    expect(screen.queryByTestId('zip-entry-summary')).toBeNull();
    expect(screen.queryByText(/Reading the header/)).toBeNull();
    expect(calls('get_zip_entry_header')).toHaveLength(0);
    // A second click on the same row is just another click: no dialog.
    fireEvent.click(screen.getAllByText('E_002.fits')[0]);
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(calls('preview_zip_entry')).toHaveLength(0);
  });

  it('Shift-click spans a file row and the entry rows; arrows walk into the entries', async () => {
    await openZip();
    fireEvent.click(screen.getAllByText('top.fits')[0]);
    fireEvent.click(screen.getAllByText('E_002.fits')[0], { shiftKey: true });
    expect(rowOf('top.fits')).toHaveClass('bg-accent/30');
    expect(rowOf('E_001.fits')).toHaveClass('bg-accent/30');
    expect(rowOf('E_002.fits')).toHaveClass('bg-accent/30');
    expect(rowOf('E_003.fits')).not.toHaveClass('bg-accent/30');

    fireEvent.click(screen.getAllByText('E_001.fits')[0]);
    fireEvent.keyDown(window, { key: 'ArrowDown' });
    expect(rowOf('E_002.fits')).toHaveClass('bg-accent/30');
    expect(rowOf('E_001.fits')).not.toHaveClass('bg-accent/30');
    // Cmd-click adds one.
    fireEvent.click(screen.getAllByText('E_003.fits')[0], { ctrlKey: true });
    expect(rowOf('E_002.fits')).toHaveClass('bg-accent/30');
    expect(rowOf('E_003.fits')).toHaveClass('bg-accent/30');
  });

  it('the filter narrows the entry rows like file rows', async () => {
    await openZip();
    fireEvent.change(screen.getAllByPlaceholderText('Filter this folder…')[0], { target: { value: 'E_002' } });
    await waitFor(() => expect(screen.queryByText('E_001.fits')).toBeNull());
    fireEvent.keyDown(window, { key: 'a', ctrlKey: true });
    expect(rowOf('E_002.fits')).toHaveClass('bg-accent/30');
    expect(screen.getByText(/1 selected \(/)).toBeInTheDocument();
  });

  it('double-click opens the preview and metadata dialog; Enter does too', async () => {
    await openZip();
    fireEvent.doubleClick(screen.getAllByText('E_001.fits')[0]);
    const dialog = await screen.findByRole('dialog');
    expect(calls('preview_zip_entry')[0][1]).toEqual({ zipPath: ZIP, entry: 'E_001.fits', resolution: 'preview' });
    await within(dialog).findByText('GAIA');
    fireEvent.keyDown(document, { key: 'Escape' });
    await waitFor(() => expect(screen.queryByRole('dialog')).toBeNull());

    fireEvent.click(screen.getAllByText('E_003.fits')[0]);
    fireEvent.keyDown(window, { key: 'Enter' });
    await waitFor(() => expect(calls('preview_zip_entry')).toHaveLength(2));
    expect(calls('preview_zip_entry')[1][1]).toEqual({ zipPath: ZIP, entry: 'E_003.fits', resolution: 'preview' });
  });

  it('⌘I with entries selected shows their headers in the metadata pane', async () => {
    await openZip();
    fireEvent.keyDown(window, { key: 'a', ctrlKey: true });
    fireEvent.click(screen.getAllByText('top.fits')[0], { ctrlKey: true }); // leave the three entries only
    fireEvent.keyDown(window, { key: 'i', ctrlKey: true });
    const pane = await screen.findByTestId('zip-entries-metadata');
    await waitFor(() => expect(calls('get_zip_entry_header')).toHaveLength(3));
    const asked = calls('get_zip_entry_header').map(([, a]) => a).sort((x, y) => (x as { entry: string }).entry.localeCompare((y as { entry: string }).entry));
    expect(asked).toEqual([
      { zipPath: ZIP, entry: 'E_001.fits' },
      { zipPath: ZIP, entry: 'E_002.fits' },
      { zipPath: ZIP, entry: 'E_003.fits' },
    ]);
    expect(pane).toHaveTextContent('3 entries read from night.zip');
    await waitFor(() => expect(pane).toHaveTextContent('GAIA'));
    expect(pane).toHaveTextContent('OBJECT');
    expect(pane).toHaveTextContent('EXPTIME (s)(varies)');
    expect(pane).toHaveTextContent('IMAGETYPLight');
    // Read-only and no catalog write.
    expect(within(pane).queryByRole('button', { name: /save/i })).toBeNull();
    for (const cmd of MUTATIONS) expect(calls(cmd)).toHaveLength(0);
  });

  it('⌘I with one entry shows its fields and header; a mixed selection keeps the editor and adds the entries', async () => {
    await openZip();
    fireEvent.click(screen.getAllByText('E_001.fits')[0]);
    fireEvent.keyDown(window, { key: 'i', ctrlKey: true });
    const pane = await screen.findByTestId('zip-entries-metadata');
    await waitFor(() => expect(pane).toHaveTextContent('EXPTIME (s)15'));
    expect(pane).toHaveTextContent('1 entry read from night.zip');
    expect(within(pane).getByText('Header')).toBeInTheDocument();

    fireEvent.click(screen.getAllByText('top.fits')[0], { ctrlKey: true });
    await screen.findByText(/1 file selected/);
    expect(screen.getByTestId('zip-entries-metadata')).toHaveTextContent('1 entry read from night.zip');
  });

  it('Move, Delete, Rename, Send and Reveal act on no entry key', async () => {
    await openZip();
    fireEvent.keyDown(window, { key: 'a', ctrlKey: true });
    for (const key of ['F6', 'F8', 'F2', 'F7', 'F3']) fireEvent.keyDown(window, { key });
    fireEvent.click(screen.getByRole('button', { name: /^Send/ }));
    await new Promise((r) => setTimeout(r, 20));
    for (const cmd of MUTATIONS) expect(calls(cmd)).toHaveLength(0);
    expect(screen.queryByRole('dialog')).toBeNull();
    expect(document.querySelector('.fixed.inset-0')).toBeNull();
    const { revealItemInDir } = await import('../../api/desktop');
    // F3 shows the zip itself, never an entry key.
    for (const [path] of vi.mocked(revealItemInDir).mock.calls) expect(path).toBe(ZIP);
  });
});
