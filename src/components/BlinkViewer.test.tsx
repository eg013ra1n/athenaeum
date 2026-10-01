import { fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { useState } from 'react';
import { api } from '../api';
import BlinkViewer from './BlinkViewer';
import { DialogShell } from './ui';
import type { BlinkAction, BlinkFrame } from './blink/types';
import type { File as CatalogFile, Frame } from '../types/models';

vi.mock('../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn(() => Promise.resolve(() => {})) } }));
vi.mock('../utils/platform', () => ({ isTauri: true }));

beforeAll(() => {
  URL.createObjectURL = vi.fn(() => 'blob:x');
  URL.revokeObjectURL = vi.fn();
  // jsdom has no 2D context: a context whose every method is a no-op.
  HTMLCanvasElement.prototype.getContext = vi.fn(
    () => new Proxy({}, { get: (_t, k) => (k === 'canvas' ? document.createElement('canvas') : () => {}), set: () => true }),
  ) as never;
});

function entry(i: number, o: Partial<BlinkFrame> = {}): BlinkFrame {
  return {
    file: { id: null, path: `/c/c_${i}.fits`, filename: `c_${i}.fits`, format: 'FITS' } as CatalogFile,
    frame: { id: 100 + i } as Frame,
    key: `f${i}`, source: 'calibrated',
    imageRef: { projectId: 'p1', frame: { frameId: i, frameUuid: null } },
    ...o,
  };
}

const invoked = (cmd: string) => vi.mocked(api.invoke).mock.calls.filter(([c]) => c === cmd);

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(async (cmd: string) => {
    if (cmd === 'get_blink_threads_max') return 2;
    if (cmd === 'get_setting') return '';
    if (cmd === 'get_collab_frame_image' || cmd === 'read_fits_image_rustafits') return new Uint8Array([0xff, 0xd8]);
    if (cmd === 'get_blackholed_file_ids') return [];
    return null;
  });
});
afterEach(() => vi.restoreAllMocks());

const withhold = (run = vi.fn()): BlinkAction => ({
  id: 'withhold', label: (n) => `Don't publish (${n})`, eligible: (f) => f.badge !== 'withheld', tone: 'warn', run,
});
const renderBlink = (p: Partial<React.ComponentProps<typeof BlinkViewer>> = {}) =>
  render(<MemoryRouter><BlinkViewer frames={[entry(1), entry(2)]} onClose={vi.fn()} {...p} /></MemoryRouter>);

describe('BlinkViewer — project mode', () => {
  it('with actions: no Black Hole call, no Blackhole/Restore/Locate, the action shows its eligible count', async () => {
    renderBlink({ actions: [withhold()] });
    await waitFor(() => expect(invoked('get_collab_frame_image').length).toBeGreaterThan(0));
    expect(invoked('get_blackholed_file_ids')).toHaveLength(0);
    fireEvent.keyDown(window, { key: 's' });
    expect(await screen.findByRole('button', { name: "Don't publish (1)" })).toBeInTheDocument();
    expect(screen.queryByText(/Blackhole/)).toBeNull();
    expect(screen.queryByTitle('Locate in file browser')).toBeNull();
  });

  it('both image loads use get_collab_frame_image with the frame ref', async () => {
    renderBlink({ actions: [] });
    await waitFor(() =>
      expect(invoked('get_collab_frame_image')[0][1]).toEqual({ projectId: 'p1', frame: { frameId: 1, frameUuid: null } }),
    );
    fireEvent.click(screen.getByTitle(/Switch to full resolution/));
    await waitFor(() =>
      expect(invoked('get_collab_frame_image').some(([, a]) => (a as { resolution?: string }).resolution === 'full')).toBe(true),
    );
    expect(invoked('read_fits_image_rustafits')).toHaveLength(0);
  });

  it('a raw entry without imageRef keeps today\'s load path', async () => {
    renderBlink({ actions: [], frames: [entry(1, { imageRef: undefined, source: 'raw' })] });
    await waitFor(() => expect(invoked('read_fits_image_rustafits').length).toBeGreaterThan(0));
    expect(invoked('get_collab_frame_image')).toHaveLength(0);
  });

  it('an action with no eligible entry in the selection is hidden; run gets the eligible entries only', async () => {
    const run = vi.fn();
    renderBlink({ actions: [withhold(run)], frames: [entry(1, { badge: 'withheld' }), entry(2)] });
    fireEvent.keyDown(window, { key: 's' }); // selects entry 1 (withheld)
    expect(screen.queryByRole('button', { name: /Don't publish/ })).toBeNull();
    fireEvent.keyDown(window, { key: 'a', ctrlKey: true });
    fireEvent.click(await screen.findByRole('button', { name: "Don't publish (1)" }));
    await waitFor(() => expect(run).toHaveBeenCalledWith([expect.objectContaining({ key: 'f2' })]));
  });

  it('a running action disables the buttons until its run settles', async () => {
    let release: () => void = () => {};
    const run = vi.fn(() => new Promise<void>((r) => { release = r; }));
    renderBlink({ actions: [withhold(run)] });
    fireEvent.keyDown(window, { key: 's' });
    const button = await screen.findByRole('button', { name: "Don't publish (1)" });
    fireEvent.click(button);
    expect(button).toBeDisabled();
    await waitFor(() => expect(run).toHaveBeenCalledTimes(1));
    fireEvent.click(button);
    release();
    await waitFor(() => expect(screen.getByRole('button', { name: "Don't publish (1)" })).toBeEnabled());
    expect(run).toHaveBeenCalledTimes(1);
  });

  it('while an action runs every project action is disabled — even one that turns eligible mid-run', async () => {
    let release: () => void = () => {};
    const run = vi.fn(() => new Promise<void>((r) => { release = r; }));
    const releaseRun = vi.fn();
    const releaseAction: BlinkAction = {
      id: 'release', label: (n) => `Release (${n})`, eligible: (f) => f.badge === 'withheld', tone: 'default', run: releaseRun,
    };
    const blink = (frames: BlinkFrame[]) => (
      <MemoryRouter><BlinkViewer frames={frames} onClose={vi.fn()} actions={[withhold(run), releaseAction]} /></MemoryRouter>
    );
    const { rerender } = render(blink([entry(1), entry(2)]));
    fireEvent.keyDown(window, { key: 's' }); // selects c_1
    fireEvent.click(await screen.findByRole('button', { name: "Don't publish (1)" }));
    await waitFor(() => expect(run).toHaveBeenCalledTimes(1));

    // The caller's reload lands while the run is still going: c_1 is withheld,
    // so Don't publish hides and Release appears.
    rerender(blink([entry(1, { badge: 'withheld' }), entry(2)]));
    const rel = await screen.findByRole('button', { name: 'Release (1)' });
    expect(rel).toBeDisabled();
    fireEvent.click(rel);
    expect(releaseRun).not.toHaveBeenCalled();

    release();
    await waitFor(() => expect(screen.getByRole('button', { name: 'Release (1)' })).toBeEnabled());
  });

  it('a fresh frames array — reordered, new badges — keeps the position, the selection and the loaded images', async () => {
    const ctx = (f: BlinkFrame) => `at ${f.file.filename}`;
    const blink = (frames: BlinkFrame[]) => (
      <MemoryRouter><BlinkViewer frames={frames} onClose={vi.fn()} actions={[withhold()]} contextLabel={ctx} /></MemoryRouter>
    );
    const { rerender } = render(blink([entry(1), entry(2)]));
    await waitFor(() => expect(invoked('get_collab_frame_image')).toHaveLength(2)); // both cached before measuring
    fireEvent.keyDown(window, { key: 'ArrowDown' }); // current: c_2
    fireEvent.keyDown(window, { key: 's' }); // selects c_2
    expect(screen.getByText('at c_2.fits')).toBeInTheDocument();
    expect(screen.getByText('1 selected')).toBeInTheDocument();
    const loads = invoked('get_collab_frame_image').length;

    // The caller's reload: the same keys in another order, c_1 newly withheld.
    rerender(blink([entry(2), entry(1, { badge: 'withheld' })]));
    // The badge lands on c_1's row, matched by key, not by index.
    expect(await within(screen.getByTitle('c_1.fits')).findByText('withheld')).toBeInTheDocument();
    expect(within(screen.getByTitle('c_2.fits')).queryByText('withheld')).toBeNull();
    expect(screen.getByText('at c_2.fits')).toBeInTheDocument(); // the strip still shows the current file
    expect(screen.getByText('1 selected')).toBeInTheDocument();
    // The selection is still c_2 (eligible), not whatever now sits at its index.
    expect(screen.getByRole('button', { name: "Don't publish (1)" })).toBeInTheDocument();

    // Another reload: the selected entry itself is now withheld.
    rerender(blink([entry(2, { badge: 'withheld' }), entry(1)]));
    expect(await within(screen.getByTitle('c_2.fits')).findByText('withheld')).toBeInTheDocument();
    await waitFor(() => expect(screen.queryByRole('button', { name: /Don't publish/ })).toBeNull());
    expect(screen.getByText('at c_2.fits')).toBeInTheDocument();
    expect(invoked('get_collab_frame_image').length).toBe(loads);
  });

  it('view only shows the chip and the context label', async () => {
    renderBlink({ actions: [], viewOnly: true, contextLabel: () => 'received from Anna · 2026-09-30 21:14:03' });
    expect(await screen.findByText('View only')).toBeInTheDocument();
    expect(screen.getByText(/received from Anna/)).toBeInTheDocument();
    expect(screen.getAllByText('calibrated').length).toBeGreaterThan(0);
  });

  it('two entries with no file id get distinct list keys', async () => {
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    renderBlink({ actions: [], frames: [entry(1, { key: 'a' }), entry(2, { key: 'b' })] });
    await screen.findAllByText(/c_\d\.fits/);
    expect(err.mock.calls.some((c) => String(c[0]).includes('same key'))).toBe(false);
  });

  it('a keyed entry never collides with another entry\'s file id', async () => {
    // Without `key`, row 0 (file id 1) and row 1 (no file id → its index, 1) share key 1.
    const err = vi.spyOn(console, 'error').mockImplementation(() => {});
    const keyed = entry(1, { key: 'a', file: { id: 1, path: '/c/c_1.fits', filename: 'c_1.fits', format: 'FITS' } as CatalogFile });
    renderBlink({ actions: [], frames: [keyed, entry(2, { key: 'b' })] });
    await screen.findAllByText(/c_\d\.fits/);
    expect(err.mock.calls.some((c) => String(c[0]).includes('same key'))).toBe(false);
  });

  it('keys_typed_in_a_dialog_over_blink_do_not_drive_blink', async () => {
    const onCloseBlink = vi.fn();
    const onCloseDialog = vi.fn();
    function Harness() {
      const [open, setOpen] = useState(true);
      return (
        <MemoryRouter>
          <BlinkViewer frames={[entry(1), entry(2)]} onClose={onCloseBlink} actions={[withhold()]} />
          {open && (
            <DialogShell title="Exclude" onClose={() => { onCloseDialog(); setOpen(false); }}>
              <textarea aria-label="Reason" />
            </DialogShell>
          )}
        </MemoryRouter>
      );
    }
    render(<Harness />);
    const reason = await screen.findByLabelText('Reason');
    reason.focus();
    for (const key of ['s', ' ', 'a', 'ArrowDown', '+']) fireEvent.keyDown(reason, { key });
    expect(screen.queryByRole('button', { name: /Don't publish/ })).toBeNull(); // 's' selected nothing
    fireEvent.keyDown(reason, { key: 'Escape' });
    expect(onCloseDialog).toHaveBeenCalledTimes(1);
    expect(onCloseBlink).not.toHaveBeenCalled();
    fireEvent.keyDown(window, { key: 'Escape' }); // the dialog is gone: Escape is Blink's again
    expect(onCloseBlink).toHaveBeenCalledTimes(1);
  });

  it('a key typed in a text field outside any dialog does not drive Blink', async () => {
    render(
      <MemoryRouter>
        <BlinkViewer frames={[entry(1), entry(2)]} onClose={vi.fn()} actions={[withhold()]} />
        <input aria-label="Note" />
      </MemoryRouter>,
    );
    const note = screen.getByLabelText('Note');
    note.focus();
    fireEvent.keyDown(note, { key: 's' });
    expect(screen.queryByText('1 selected')).toBeNull();
    fireEvent.keyDown(document.body, { key: 's' }); // the same key outside the field selects
    expect(await screen.findByText('1 selected')).toBeInTheDocument();
  });

  it('while a dialog is above Blink, a key pressed outside any field does not drive Blink', async () => {
    render(
      <MemoryRouter>
        <BlinkViewer frames={[entry(1), entry(2)]} onClose={vi.fn()} actions={[withhold()]} />
        <DialogShell title="Exclude" onClose={vi.fn()}><p>body</p></DialogShell>
      </MemoryRouter>,
    );
    await screen.findByRole('dialog');
    fireEvent.keyDown(document.body, { key: 's' });
    expect(screen.queryByText('1 selected')).toBeNull();
    expect(screen.queryByRole('button', { name: /Don't publish/ })).toBeNull();
  });
});

describe('BlinkViewer — existing callers', () => {
  it('without actions it still checks the Black Hole and offers Blackhole on a selection', async () => {
    renderBlink({ frames: [entry(1, { imageRef: undefined, key: undefined, file: { id: 7, path: '/r/a.fits', filename: 'a.fits', format: 'FITS' } as CatalogFile })] });
    await waitFor(() => expect(invoked('get_blackholed_file_ids')).toHaveLength(1));
    fireEvent.keyDown(window, { key: 's' });
    expect(await screen.findByText(/Blackhole \(1\)/)).toBeInTheDocument();
  });

  it('a key pressed on the speed slider still drives Blink', async () => {
    renderBlink({ frames: [entry(1, { imageRef: undefined, key: undefined, file: { id: 7, path: '/r/a.fits', filename: 'a.fits', format: 'FITS' } as CatalogFile })] });
    const slider = screen.getByRole('slider');
    slider.focus();
    fireEvent.keyDown(slider, { key: 's' });
    expect(await screen.findByText(/Blackhole \(1\)/)).toBeInTheDocument();
  });
});
