import { act, fireEvent, render, screen, waitFor, within } from '@testing-library/react';
import { MemoryRouter } from 'react-router-dom';
import { afterEach, beforeAll, beforeEach, describe, expect, it, vi } from 'vitest';
import { useState } from 'react';
import { api } from '../api';
import { routeInvoke } from '../test/apiMock';
import BlinkViewer from './BlinkViewer';
import { DialogShell } from './ui';
import type { BlinkAction, BlinkFrame } from './blink/types';
import type { File as CatalogFile, Frame } from '../types/models';

vi.mock('../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn(() => Promise.resolve(() => {})) } }));
vi.mock('../utils/platform', () => ({ isTauri: true }));

// What the canvas was asked to draw, first argument of every drawImage.
const drawn: unknown[] = [];

beforeAll(() => {
  URL.createObjectURL = vi.fn(() => 'blob:x');
  URL.revokeObjectURL = vi.fn();
  // jsdom has no 2D context: a context whose every method is a no-op, drawImage noting its image.
  HTMLCanvasElement.prototype.getContext = vi.fn(
    () => new Proxy({}, {
      get: (_t, k) => {
        if (k === 'canvas') return document.createElement('canvas');
        if (k === 'drawImage') return (img: unknown) => { drawn.push(img); };
        return () => {};
      },
      set: () => true,
    }),
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
  drawn.length = 0;
  vi.mocked(api.invoke).mockReset();
  const jpegBytes = () => new Uint8Array([0xff, 0xd8]);
  routeInvoke({
    get_blink_threads_max: 2,
    get_setting: '',
    get_collab_frame_image: jpegBytes,
    read_fits_image_rustafits: jpegBytes,
    get_stacking_aligned_image: jpegBytes,
    get_blackholed_file_ids: () => [],
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

  it('a single-target action is offered only while exactly one entry of the selection is eligible', async () => {
    const run = vi.fn();
    const pick: BlinkAction = { id: 'pick', label: () => 'Pick', eligible: () => true, tone: 'default', single: true, run };
    renderBlink({ actions: [pick] });
    fireEvent.keyDown(window, { key: 's' }); // one entry selected
    fireEvent.click(await screen.findByRole('button', { name: 'Pick' }));
    await waitFor(() => expect(run).toHaveBeenCalledWith([expect.objectContaining({ key: 'f1' })]));

    fireEvent.keyDown(window, { key: 'a', ctrlKey: true }); // both selected: two eligible
    await waitFor(() => expect(screen.queryByRole('button', { name: 'Pick' })).toBeNull());
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

  it('a frame whose zip is in the Black Hole shows as rejected and offers neither Blackhole nor Restore', async () => {
    routeInvoke({
      get_blink_threads_max: 2,
      get_setting: '',
      read_fits_image_rustafits: () => new Uint8Array([0xff, 0xd8]),
      get_blackholed_file_ids: () => [],
      get_zip_blackholed_file_ids: () => [7],
    });
    const inZip = { id: 7, path: '/z/night.zip/L/a.fits', filename: 'a.fits', format: 'FITS', archive_zip_path: '/z/night.zip', archive_path_in_zip: 'L/a.fits', archived_in_operation: null } as unknown as CatalogFile;
    renderBlink({ frames: [entry(1, { imageRef: undefined, key: undefined, file: inZip })] });
    await waitFor(() => expect(invoked('get_zip_blackholed_file_ids')).toHaveLength(1));
    // The row carries the Black Hole mark (the trash icon replaces the checkbox) and says why.
    expect(await screen.findByTitle(/its zip is in the Black Hole/)).toBeInTheDocument();
    fireEvent.keyDown(window, { key: 's' });
    await waitFor(() => expect(screen.queryByText(/Blackhole \(\d+\)/)).toBeNull());
    expect(screen.queryByText(/Restore \(\d+\)/)).toBeNull();
  });

  it('a key pressed on the speed slider still drives Blink', async () => {
    renderBlink({ frames: [entry(1, { imageRef: undefined, key: undefined, file: { id: 7, path: '/r/a.fits', filename: 'a.fits', format: 'FITS' } as CatalogFile })] });
    const slider = screen.getByRole('slider');
    slider.focus();
    fireEvent.keyDown(slider, { key: 's' });
    expect(await screen.findByText(/Blackhole \(1\)/)).toBeInTheDocument();
  });
});

describe('BlinkViewer — frame navigation keys', () => {
  // Raw catalog frames (no project image ref) with a capture time, so the Time sort orders them.
  const raw = (i: number, dateObs: string): BlinkFrame => entry(i, {
    imageRef: undefined, key: undefined,
    file: { id: 10 + i, path: `/r/r_${i}.fits`, filename: `r_${i}.fits`, format: 'FITS' } as CatalogFile,
    frame: { id: 100 + i, date_obs: dateObs } as Frame,
  });
  const three = () => [raw(1, '2025-01-01T01:00:00'), raw(2, '2025-01-01T02:00:00'), raw(3, '2025-01-01T03:00:00')];
  const counter = () => screen.getByText(/^\/ \d+$/).parentElement!.textContent;

  it('the arrow keys step frames while every image is still caching; left / right set the speed', async () => {
    const neverCached = () => new Promise(() => {});
    routeInvoke({
      get_blink_threads_max: 2,
      get_setting: '',
      get_blackholed_file_ids: () => [],
      read_fits_image_rustafits: neverCached,
      get_frame_preview: neverCached,
    });
    renderBlink({ frames: three() });
    await waitFor(() => expect(invoked('read_fits_image_rustafits').length).toBeGreaterThan(0));
    expect(counter()).toBe('1 / 3');

    fireEvent.keyDown(window, { key: 'ArrowRight' });
    expect(counter()).toBe('1 / 3');
    expect(screen.getByText('3 FPS')).toBeInTheDocument();

    fireEvent.keyDown(window, { key: 'ArrowDown' });
    fireEvent.keyDown(window, { key: 'ArrowDown' });
    expect(counter()).toBe('3 / 3');
    fireEvent.keyDown(window, { key: 'ArrowUp' });
    expect(counter()).toBe('2 / 3');
    expect(screen.getByTitle('Next frame (↓)')).toBeEnabled();
  });

  it('the counter and Prev / Next follow the frame list order, not the order the frames came in', async () => {
    renderBlink({ frames: three() });
    expect(counter()).toBe('1 / 3');
    fireEvent.click(screen.getByRole('button', { name: /^Time/ })); // newest first: r_3, r_2, r_1
    // r_1 is current and now last in the list: the counter says so and Next is the end.
    expect(counter()).toBe('3 / 3');
    expect(screen.getByTitle('Next frame (↓)')).toBeDisabled();
    expect(screen.getByTitle('Previous frame (↑)')).toBeEnabled();
    fireEvent.click(screen.getByTitle('Previous frame (↑)'));
    expect(counter()).toBe('2 / 3');
  });
});

describe('BlinkViewer — aligned frames of a stacking run', () => {
  // Stacking entries are raw catalog frames with the run's verdict on them.
  const aligned = (i: number, a: BlinkFrame['aligned']): BlinkFrame => entry(i, { imageRef: undefined, source: 'raw', aligned: a });
  const available = (i: number) => aligned(i, { runId: 8, frameId: i });
  const SWITCH_TO_ORIGINAL = 'Show original frames';
  const SWITCH_TO_ALIGNED = 'Show aligned frames (run #8)';

  it('aligned_mode_loads_run_frame', async () => {
    renderBlink({ actions: [], frames: [aligned(1, { runId: 8, frameId: 7 })] });
    await waitFor(() => expect(invoked('get_stacking_aligned_image')).toHaveLength(1));
    // The preview carries no resolution, like the collab frame image.
    expect(invoked('get_stacking_aligned_image')[0][1]).toEqual({ runId: 8, frameId: 7 });
    expect(invoked('read_fits_image_rustafits')).toHaveLength(0);

    fireEvent.click(screen.getByTitle(/Switch to full resolution/));
    await waitFor(() =>
      expect(invoked('get_stacking_aligned_image').map(([, a]) => a)).toContainEqual({ runId: 8, frameId: 7, resolution: 'full' }),
    );
    expect(invoked('read_fits_image_rustafits')).toHaveLength(0);
  });

  it('the switch is a toggle button named by its title', async () => {
    renderBlink({ actions: [], frames: [available(1)] });
    const showingAligned = await screen.findByRole('button', { name: SWITCH_TO_ORIGINAL });
    expect(showingAligned).toHaveAttribute('aria-pressed', 'true');

    fireEvent.click(showingAligned);

    expect(screen.getByRole('button', { name: SWITCH_TO_ALIGNED })).toHaveAttribute('aria-pressed', 'false');
  });

  it('toggle_reloads_original', async () => {
    renderBlink({ actions: [], frames: [available(1), available(2)] });
    await waitFor(() => expect(invoked('get_stacking_aligned_image')).toHaveLength(2));
    fireEvent.click(screen.getByTitle(/Switch to full resolution/));
    await waitFor(() => expect(invoked('get_stacking_aligned_image')).toHaveLength(3));
    vi.mocked(URL.revokeObjectURL).mockClear();

    fireEvent.click(screen.getByTitle(SWITCH_TO_ORIGINAL));

    // The cached previews and the full-resolution image of the other variant are released.
    expect(vi.mocked(URL.revokeObjectURL).mock.calls.length).toBeGreaterThanOrEqual(3);
    expect(screen.getByTitle(/Switch to full resolution/)).toBeInTheDocument();
    // The current frame and the preload both go through the original path.
    await waitFor(() => expect(invoked('read_fits_image_rustafits').map(([, a]) => a)).toEqual(
      expect.arrayContaining([{ path: '/c/c_1.fits' }, { path: '/c/c_2.fits' }]),
    ));
    expect(invoked('get_stacking_aligned_image')).toHaveLength(3);
    expect(screen.getByTitle(SWITCH_TO_ALIGNED)).toBeInTheDocument();

    // And back: the aligned frames are asked for again.
    fireEvent.click(screen.getByTitle(SWITCH_TO_ALIGNED));
    await waitFor(() => expect(invoked('get_stacking_aligned_image').length).toBeGreaterThanOrEqual(5));
  });

  it('a load of the other variant that lands after the toggle is dropped', async () => {
    const sized = (n: number) => new Uint8Array(n);
    let landLate: () => void = () => {};
    routeInvoke({
      get_blink_threads_max: 2,
      get_setting: '',
      get_stacking_aligned_image: () => new Promise((r) => { landLate = () => r(sized(3)); }),
      read_fits_image_rustafits: () => sized(2),
    });
    const created = vi.mocked(URL.createObjectURL);
    created.mockClear();
    renderBlink({ actions: [], frames: [available(1)] });
    await waitFor(() => expect(invoked('get_stacking_aligned_image')).toHaveLength(1));

    fireEvent.click(screen.getByTitle(SWITCH_TO_ORIGINAL));
    await waitFor(() => expect(invoked('read_fits_image_rustafits')).toHaveLength(1));
    landLate();
    await waitFor(() => expect(created).toHaveBeenCalled());
    await new Promise((r) => setTimeout(r, 20));
    const sizes = created.mock.calls.map(([b]) => (b as Blob).size);
    expect(sizes).toEqual([2]);
  });

  it('aligned_disables_annotations', async () => {
    // A flat, so that the contour button is live on the original.
    renderBlink({ actions: [], frames: [{ ...available(1), frame: { id: 101, imagetyp: 'FLAT' } as unknown as Frame }] });
    await waitFor(() => expect(invoked('get_stacking_aligned_image')).toHaveLength(1));
    const buttons = () => screen.getAllByTitle('Not available on aligned frames');
    expect(buttons()).toHaveLength(2);
    for (const b of buttons()) expect(b).toBeDisabled();

    fireEvent.keyDown(window, { key: 'a' });
    await new Promise((r) => setTimeout(r, 10));
    expect(invoked('get_frame_star_metrics')).toHaveLength(0);

    // The original shows them again, and the stars follow the key.
    fireEvent.click(screen.getByTitle(SWITCH_TO_ORIGINAL));
    expect(screen.queryByTitle('Not available on aligned frames')).toBeNull();
    expect(screen.getByTitle('Show star annotations (A)')).toBeEnabled();
    expect(screen.getByTitle(/Show flat contour plot/)).toBeEnabled();
    fireEvent.keyDown(window, { key: 'a' });
    await waitFor(() => expect(invoked('get_frame_star_metrics')).toHaveLength(1));
  });

  it('no_switch_without_aligned_entries', async () => {
    const none = { actions: [], frames: [entry(1, { imageRef: undefined, source: 'raw' })] };
    const { unmount } = renderBlink(none);
    await waitFor(() => expect(invoked('read_fits_image_rustafits').length).toBeGreaterThan(0));
    expect(screen.queryByTitle(/aligned frames|original frames/)).toBeNull();
    expect(screen.getByTitle('Show star annotations (A)')).toBeEnabled();
    unmount();

    // Entries that only say why they are originals do not offer the switch either.
    renderBlink({ actions: [], frames: [aligned(1, { runId: 8, reason: 'a later run replaced it' })] });
    await waitFor(() => expect(invoked('read_fits_image_rustafits').length).toBeGreaterThan(1));
    expect(screen.queryByTitle(/aligned frames|original frames/)).toBeNull();
    expect(screen.getByTitle('Show star annotations (A)')).toBeEnabled();
    expect(invoked('get_stacking_aligned_image')).toHaveLength(0);
    // With no switch to say it, the strip still says why this entry is an original (spec 6.7: an older run).
    expect(screen.getByText('original — a later run replaced it')).toBeInTheDocument();
  });

  it('an entry without an aligned frame loads its original and says why in the strip', async () => {
    renderBlink({
      actions: [],
      frames: [
        aligned(1, { runId: 8, reason: 'registration failed in run #8' }),
        available(2),
      ],
    });
    await waitFor(() => expect(invoked('get_stacking_aligned_image')).toHaveLength(1));
    expect(invoked('get_stacking_aligned_image')[0][1]).toEqual({ runId: 8, frameId: 2 });
    expect(invoked('read_fits_image_rustafits')[0][1]).toEqual({ path: '/c/c_1.fits' });
    expect(screen.getByText('original — registration failed in run #8')).toBeInTheDocument();

    // On the original the strip stays quiet: every entry is an original there.
    fireEvent.click(screen.getByTitle(SWITCH_TO_ORIGINAL));
    expect(screen.queryByText(/^original — /)).toBeNull();
  });
});

describe('BlinkViewer — a decode in flight when the aligned switch flips', () => {
  const aligned = (i: number): BlinkFrame => entry(i, { imageRef: undefined, source: 'raw', aligned: { runId: 8, frameId: i } });
  // jsdom decodes nothing: every Image made waits in `decodes` until the test fires its load or error.
  let decodes: HTMLImageElement[];
  const srcs = () => decodes.map((d) => d.getAttribute('src'));

  beforeEach(() => {
    decodes = [];
    vi.stubGlobal('Image', function FakeImage() {
      const img = document.createElement('img');
      Object.defineProperty(img, 'width', { value: 100 });
      Object.defineProperty(img, 'height', { value: 100 });
      decodes.push(img);
      return img;
    });
    // The blob URL tells the variants apart: the aligned image is 3 bytes, the original 2.
    vi.mocked(URL.createObjectURL).mockImplementation((b) => `blob:${(b as Blob).size}`);
    routeInvoke({
      get_blink_threads_max: 2,
      get_setting: '',
      get_stacking_aligned_image: () => new Uint8Array(3),
      read_fits_image_rustafits: () => new Uint8Array(2),
    });
  });
  afterEach(() => {
    vi.unstubAllGlobals();
    vi.mocked(URL.createObjectURL).mockImplementation(() => 'blob:x');
  });

  /** The aligned preview has landed and its decode is pending; the switch flips to Original. */
  async function flipWhileTheAlignedDecodePends() {
    renderBlink({ actions: [], frames: [aligned(1)] });
    await waitFor(() => expect(srcs()).toEqual(['blob:3']));
    fireEvent.click(screen.getByTitle('Show original frames'));
    await waitFor(() => expect(invoked('read_fits_image_rustafits')).toHaveLength(1));
  }
  const fire = (img: HTMLImageElement, type: 'load' | 'error') => act(async () => { img.dispatchEvent(new Event(type)); });

  it('stale_decode_load_is_not_painted', async () => {
    await flipWhileTheAlignedDecodePends();
    // The original's decode starts although the aligned one has not finished.
    await waitFor(() => expect(srcs()).toEqual(['blob:3', 'blob:2']));
    const [leaving, arriving] = decodes;

    await fire(leaving, 'load');
    expect(drawn).toEqual([]);

    await fire(arriving, 'load');
    expect(drawn.length).toBeGreaterThan(0);
    expect(drawn.every((d) => d === arriving)).toBe(true);
    expect(screen.queryByText(/Failed to load/)).toBeNull();
  });

  it('stale_decode_error_shows_no_banner', async () => {
    await flipWhileTheAlignedDecodePends();
    await waitFor(() => expect(srcs()).toEqual(['blob:3', 'blob:2']));
    const [leaving, arriving] = decodes;

    // Revoking the aborted blob makes the browser fail the pending decode.
    await fire(leaving, 'error');
    expect(screen.queryByText(/Failed to load/)).toBeNull();

    await fire(arriving, 'load');
    expect(drawn.length).toBeGreaterThan(0);
    expect(drawn.every((d) => d === arriving)).toBe(true);
    expect(screen.queryByText(/Failed to load/)).toBeNull();
  });
});
