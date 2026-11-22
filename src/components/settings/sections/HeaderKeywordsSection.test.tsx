import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { act, fireEvent, render, screen } from '@testing-library/react';
import { HeaderKeywordsSection } from './HeaderKeywordsSection';
import { SettingsDefaultsProvider } from '../../../settings/SettingsDefaultsContext';
import { DEFAULT_HEADER_CHAINS, HEADER_FIELDS } from '../../../settings/headerFields';
import { api } from '../../../api';
import { routeInvoke } from '../../../test/apiMock';
import type { HeaderFallbackConfig, HeaderFallbackCounts, HeaderFallbackProgress } from '../../../types/models';

vi.mock('../../../api');

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../../../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));

function defaultDoc(): HeaderFallbackConfig {
  return {
    fields: Object.fromEntries(
      HEADER_FIELDS.map((f) => [f, { order: [...DEFAULT_HEADER_CHAINS[f]], retired: [] }]),
    ),
  };
}

const NO_CHANGE: HeaderFallbackCounts = {
  byField: [],
  framesChanged: 0,
  valuesKept: 0,
  setsRegrouped: 0,
  framesWithoutHeader: 0,
};

const SOME_CHANGE: HeaderFallbackCounts = {
  byField: [
    { field: 'ccdTemp', frames: 290 },
    { field: 'exptime', frames: 22 },
  ],
  framesChanged: 312,
  valuesKept: 9,
  setsRegrouped: 3,
  framesWithoutHeader: 2,
};

let preview: HeaderFallbackCounts;
let listeners: Record<string, (p: HeaderFallbackProgress) => void>;

function callsOf(command: string) {
  return vi.mocked(api.invoke).mock.calls.filter(([cmd]) => cmd === command);
}

function savedOrder(call: number, field: string): string[] {
  const args = callsOf('set_header_fallbacks')[call]![1] as { config: HeaderFallbackConfig };
  return args.config.fields[field as 'ccdTemp']!.order;
}

/** A few microtask hops: a chain of awaits inside the hooks settles whether or not timers are faked. */
async function settle() {
  await act(async () => {
    for (let i = 0; i < 6; i++) await Promise.resolve();
  });
}

async function advance(ms: number) {
  await act(async () => {
    await vi.advanceTimersByTimeAsync(ms);
  });
}

async function renderSection() {
  render(
    <SettingsDefaultsProvider>
      <HeaderKeywordsSection />
    </SettingsDefaultsProvider>,
  );
  await settle();
}

function deferred<T>() {
  let resolve!: (v: T) => void;
  let reject!: (e: unknown) => void;
  const promise = new Promise<T>((res, rej) => {
    resolve = res;
    reject = rej;
  });
  return { promise, resolve, reject };
}

function applyButton(): HTMLButtonElement {
  return screen.getByRole('button', { name: 'Apply to catalog' }) as HTMLButtonElement;
}

function chip(keyword: string): HTMLElement {
  const el = screen.getByText(keyword).closest('li');
  if (!el) throw new Error(`no chip for ${keyword}`);
  return el as HTMLElement;
}

function typeKeyword(field: string, text: string) {
  const input = screen.getByLabelText(`Add a keyword to ${field}`);
  fireEvent.change(input, { target: { value: text } });
  fireEvent.keyDown(input, { key: 'Enter' });
}

beforeEach(() => {
  vi.useFakeTimers();
  notifyMock.mockClear();
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.listen).mockReset();
  preview = NO_CHANGE;
  listeners = {};
  routeInvoke({
    get_settings_defaults: () => ({ kv: {}, headerFallbacks: defaultDoc() }),
    get_header_fallbacks: () => defaultDoc(),
    set_header_fallbacks: undefined,
    reset_header_fallbacks: undefined,
    preview_header_fallbacks: () => preview,
    apply_header_fallbacks: () => SOME_CHANGE,
  });
  vi.mocked(api.listen).mockImplementation((async (event: string, cb: (p: HeaderFallbackProgress) => void) => {
    listeners[event] = cb;
    return () => {};
  }) as never);
});

afterEach(() => {
  vi.useRealTimers();
});

describe('HeaderKeywordsSection', () => {
  it('renders one row per field with default chips that have no remove button', async () => {
    await renderSection();
    expect(document.querySelectorAll('[data-field-row]')).toHaveLength(12);
    expect(screen.getByText('CCD-TEMP')).toBeTruthy();
    expect(screen.getByText('PIXSIZE2')).toBeTruthy();
    expect(screen.queryByLabelText('Remove CCD-TEMP')).toBeNull();
    expect(screen.getByText('Sensor temperature')).toBeTruthy();
    expect(screen.getByText('Telescope')).toBeTruthy();
  });

  it('moves a chip with the arrow keys and saves the new order', async () => {
    await renderSection();
    const first = chip('CCD-TEMP');
    // jsdom keeps focus on a keyed node that React moves, and React itself calls focus() to restore the focused node,
    // so nothing is focused here: the only focus() call is the one the chip's own effect makes.
    const focused: HTMLElement[] = [];
    vi.spyOn(HTMLElement.prototype, 'focus').mockImplementation(function (this: HTMLElement) {
      focused.push(this);
    });
    fireEvent.keyDown(first, { key: 'ArrowRight' });
    await settle();
    // Focus follows the chip it moved.
    expect(focused.map((el) => el.dataset.keyword)).toEqual(['CCD-TEMP']);
    expect(chip('CCD-TEMP')).toBe(focused[0]);
    vi.restoreAllMocks();
    await advance(500);
    expect(callsOf('set_header_fallbacks')).toHaveLength(1);
    expect(savedOrder(0, 'ccdTemp')).toEqual(['DET-TEMP', 'CCD-TEMP']);
    // The move is announced.
    expect(screen.getByText('Moved CCD-TEMP to position 2 of 2.')).toBeTruthy();
  });

  it('labels each list with the field label and keeps every chip and remove button in the Tab order', async () => {
    await renderSection();
    expect(screen.getByRole('list', { name: 'Sensor temperature keywords' })).toBeTruthy();
    expect(screen.queryByLabelText(/^ccdTemp keywords/)).toBeNull();
    typeKeyword('Gain', 'my-gain');
    expect(chip('GAIN').tabIndex).toBe(0);
    expect(chip('MY-GAIN').tabIndex).toBe(0);
    expect(screen.getByLabelText('Remove MY-GAIN').tabIndex).toBe(0);
  });

  it('moves a chip by pointer drag: one save on release, none while it moves', async () => {
    await renderSection();
    // Chips side by side, 60 px wide: CCD-TEMP 0-60, DET-TEMP 60-120.
    const rects: Record<string, [number, number]> = { 'CCD-TEMP': [0, 60], 'DET-TEMP': [60, 120] };
    for (const [kw, [l, r]] of Object.entries(rects)) {
      chip(kw).getBoundingClientRect = () =>
        ({ left: l, right: r, top: 0, bottom: 20, width: r - l, height: 20, x: l, y: 0 }) as DOMRect;
    }
    const capture = vi.fn();
    chip('CCD-TEMP').setPointerCapture = capture;

    fireEvent.pointerDown(chip('CCD-TEMP'), { pointerId: 1, button: 0, clientX: 30, clientY: 10 });
    expect(capture).toHaveBeenCalledWith(1);
    // Short of DET-TEMP's midpoint (90): nothing moves.
    fireEvent.pointerMove(chip('CCD-TEMP'), { pointerId: 1, clientX: 80, clientY: 10 });
    expect(screen.getAllByRole('listitem')[0]!.getAttribute('data-keyword')).toBe('CCD-TEMP');
    // Past it: the dragged chip takes DET-TEMP's place, shown at once.
    fireEvent.pointerMove(chip('CCD-TEMP'), { pointerId: 1, clientX: 100, clientY: 10 });
    expect(screen.getAllByRole('listitem')[0]!.getAttribute('data-keyword')).toBe('DET-TEMP');
    await advance(2000);
    expect(callsOf('set_header_fallbacks')).toHaveLength(0);

    fireEvent.pointerUp(chip('CCD-TEMP'), { pointerId: 1, clientX: 100, clientY: 10 });
    await advance(500);
    expect(callsOf('set_header_fallbacks')).toHaveLength(1);
    expect(savedOrder(0, 'ccdTemp')).toEqual(['DET-TEMP', 'CCD-TEMP']);
    expect(screen.getByText('Moved CCD-TEMP to position 2 of 2.')).toBeTruthy();
  });

  it('cancels a pointer drag with Escape and saves nothing', async () => {
    await renderSection();
    const rects: Record<string, [number, number]> = { 'CCD-TEMP': [0, 60], 'DET-TEMP': [60, 120] };
    for (const [kw, [l, r]] of Object.entries(rects)) {
      chip(kw).getBoundingClientRect = () =>
        ({ left: l, right: r, top: 0, bottom: 20, width: r - l, height: 20, x: l, y: 0 }) as DOMRect;
    }
    chip('CCD-TEMP').setPointerCapture = vi.fn();
    fireEvent.pointerDown(chip('CCD-TEMP'), { pointerId: 1, button: 0, clientX: 30, clientY: 10 });
    fireEvent.pointerMove(chip('CCD-TEMP'), { pointerId: 1, clientX: 100, clientY: 10 });
    expect(screen.getAllByRole('listitem')[0]!.getAttribute('data-keyword')).toBe('DET-TEMP');
    fireEvent.keyDown(document.body, { key: 'Escape' });
    expect(screen.getAllByRole('listitem')[0]!.getAttribute('data-keyword')).toBe('CCD-TEMP');
    // A release after the cancel is no drop.
    fireEvent.pointerUp(chip('CCD-TEMP'), { pointerId: 1, clientX: 100, clientY: 10 });
    await advance(2000);
    expect(callsOf('set_header_fallbacks')).toHaveLength(0);
  });

  it('adds a keyword upper-cased and removes it with its button', async () => {
    await renderSection();
    typeKeyword('Sensor temperature', 'my-temp');
    expect(screen.getByText('MY-TEMP')).toBeTruthy();
    await advance(500);
    expect(savedOrder(0, 'ccdTemp')).toEqual(['CCD-TEMP', 'DET-TEMP', 'MY-TEMP']);

    fireEvent.click(screen.getByLabelText('Remove MY-TEMP'));
    await advance(500);
    expect(screen.queryByText('MY-TEMP')).toBeNull();
    expect(callsOf('set_header_fallbacks')).toHaveLength(2);
    expect(savedOrder(1, 'ccdTemp')).toEqual(['CCD-TEMP', 'DET-TEMP']);
  });

  it('removes a user keyword with Delete from the keyboard', async () => {
    await renderSection();
    typeKeyword('Gain', 'my-gain');
    const added = chip('MY-GAIN');
    added.focus();
    fireEvent.keyDown(added, { key: 'Delete' });
    await advance(500);
    expect(screen.queryByText('MY-GAIN')).toBeNull();
    // A default keyword has no remove key.
    fireEvent.keyDown(chip('GAIN'), { key: 'Delete' });
    await advance(500);
    expect(screen.getByText('GAIN')).toBeTruthy();
  });

  it('adding_a_case_variant_of_a_default_shows_an_inline_error', async () => {
    await renderSection();
    typeKeyword('Sensor temperature', 'ccd-temp ');
    expect(screen.getByText('CCD-TEMP is already in this list.')).toBeTruthy();
    await advance(2000);
    expect(callsOf('set_header_fallbacks')).toHaveLength(0);
  });

  it('refuses an empty keyword and one with an equals sign, saving nothing', async () => {
    await renderSection();
    typeKeyword('Gain', '   ');
    expect(screen.getByText('Keywords can\'t be empty or contain "=".')).toBeTruthy();
    typeKeyword('Gain', 'A=B');
    expect(screen.getByText('Keywords can\'t be empty or contain "=".')).toBeTruthy();
    await advance(2000);
    expect(callsOf('set_header_fallbacks')).toHaveLength(0);
  });

  it('shows the reset button only for a field off its default', async () => {
    await renderSection();
    expect(screen.queryAllByLabelText(/Reset to default/)).toHaveLength(0);
    typeKeyword('Gain', 'egain');
    const reset = screen.getByLabelText('Reset to default (GAIN)');
    expect(screen.getAllByLabelText(/Reset to default/)).toHaveLength(1);
    fireEvent.click(reset);
    expect(screen.queryByText('EGAIN')).toBeNull();
    expect(screen.queryAllByLabelText(/Reset to default/)).toHaveLength(0);
  });

  it('hides the preview line when nothing changes', async () => {
    await renderSection();
    expect(callsOf('preview_header_fallbacks')).toHaveLength(1);
    expect(screen.queryByText('Apply to catalog')).toBeNull();
    expect(screen.queryByText(/read differently/)).toBeNull();
  });

  it('shows counts and opens the confirm with the calibration line', async () => {
    preview = SOME_CHANGE;
    await renderSection();
    expect(screen.getByText('312 frames read differently under these rules:')).toBeTruthy();
    expect(screen.getByText('Sensor temperature 290 · Exposure 22')).toBeTruthy();
    expect(screen.getByText('9 values you set yourself are kept.')).toBeTruthy();

    fireEvent.click(screen.getByText('Apply to catalog'));
    expect(screen.getByText('Apply FITS keywords to the catalog?')).toBeTruthy();
    expect(screen.getByText(/Sensor temperature: 290 frames/)).toBeTruthy();
    expect(screen.getByText(/Exposure: 22 frames/)).toBeTruthy();
    expect(screen.getByText(/Values you set yourself stay as they are \(9\)\./)).toBeTruthy();
    expect(
      screen.getByText(
        /Darks, flats and bias in 3 calibration sets will be grouped again\. Lights linked to them are matched again at the next matching\./,
      ),
    ).toBeTruthy();
    expect(screen.getByText(/2 frames have no stored header and are not changed\./)).toBeTruthy();
    expect(callsOf('apply_header_fallbacks')).toHaveLength(0);
  });

  it('says one frame and one value in the singular', async () => {
    preview = { ...NO_CHANGE, byField: [{ field: 'gain', frames: 1 }], framesChanged: 1, valuesKept: 1 };
    await renderSection();
    expect(screen.getByText('1 frame reads differently under these rules:')).toBeTruthy();
    expect(screen.getByText('1 value you set yourself is kept.')).toBeTruthy();
  });

  it('says one frame, one calibration set and one frame without a header in the singular', async () => {
    preview = {
      byField: [{ field: 'gain', frames: 1 }],
      framesChanged: 1,
      valuesKept: 0,
      setsRegrouped: 1,
      framesWithoutHeader: 1,
    };
    routeInvoke({
      get_settings_defaults: () => ({ kv: {}, headerFallbacks: defaultDoc() }),
      get_header_fallbacks: () => defaultDoc(),
      preview_header_fallbacks: () => preview,
      apply_header_fallbacks: () => preview,
    });
    await renderSection();
    fireEvent.click(screen.getByText('Apply to catalog'));
    expect(screen.getByText(/Gain: 1 frame(?!s)/)).toBeTruthy();
    expect(screen.getByText(/Darks, flats and bias in 1 calibration set will be grouped again\./)).toBeTruthy();
    expect(screen.getByText(/1 frame has no stored header and is not changed\./)).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Apply' }));
    await settle();
    expect(notifyMock).toHaveBeenCalledWith(
      expect.objectContaining({ detail: '1 frame changed, 1 calibration set grouped again.' }),
    );
  });

  it('keeps Apply shut until the edit is saved and its preview has landed (the confirm never shows old rules)', async () => {
    preview = SOME_CHANGE;
    const NEW_COUNTS: HeaderFallbackCounts = {
      byField: [{ field: 'ccdTemp', frames: 7 }],
      framesChanged: 7,
      valuesKept: 0,
      setsRegrouped: 0,
      framesWithoutHeader: 0,
    };
    const second = deferred<HeaderFallbackCounts>();
    let previews = 0;
    routeInvoke({
      get_settings_defaults: () => ({ kv: {}, headerFallbacks: defaultDoc() }),
      get_header_fallbacks: () => defaultDoc(),
      set_header_fallbacks: undefined,
      preview_header_fallbacks: () => (previews++ === 0 ? SOME_CHANGE : second.promise),
      apply_header_fallbacks: () => NEW_COUNTS,
    });
    await renderSection();
    expect(applyButton().disabled).toBe(false);

    // An edit: the button shuts at once, before the debounced save.
    fireEvent.keyDown(chip('CCD-TEMP'), { key: 'ArrowRight' });
    expect(applyButton().disabled).toBe(true);
    fireEvent.click(applyButton());
    expect(screen.queryByText('Apply FITS keywords to the catalog?')).toBeNull();

    // The save has landed, the follow-up preview has not been asked yet.
    await advance(500);
    expect(callsOf('set_header_fallbacks')).toHaveLength(1);
    expect(applyButton().disabled).toBe(true);
    fireEvent.click(applyButton());
    expect(screen.queryByText('Apply FITS keywords to the catalog?')).toBeNull();

    // The preview is out, not answered.
    await advance(800);
    expect(callsOf('preview_header_fallbacks')).toHaveLength(2);
    expect(applyButton().disabled).toBe(true);
    fireEvent.click(applyButton());
    expect(screen.queryByText('Apply FITS keywords to the catalog?')).toBeNull();

    // Both have landed: the confirm shows the new counts, and one Apply runs.
    await act(async () => {
      second.resolve(NEW_COUNTS);
    });
    await settle();
    expect(applyButton().disabled).toBe(false);
    fireEvent.click(applyButton());
    expect(screen.getByText('Apply FITS keywords to the catalog?')).toBeTruthy();
    expect(screen.getByText(/Sensor temperature: 7 frames/)).toBeTruthy();
    expect(screen.queryByText(/Exposure: 22 frames/)).toBeNull();
    expect(callsOf('apply_header_fallbacks')).toHaveLength(0);
    fireEvent.click(screen.getByRole('button', { name: 'Apply' }));
    await settle();
    expect(callsOf('apply_header_fallbacks')).toHaveLength(1);
  });

  it('closes an open confirm when an edit follows, rather than applying other rules', async () => {
    preview = SOME_CHANGE;
    await renderSection();
    fireEvent.click(applyButton());
    expect(screen.getByText('Apply FITS keywords to the catalog?')).toBeTruthy();
    fireEvent.keyDown(chip('CCD-TEMP'), { key: 'ArrowRight' });
    expect(screen.queryByText('Apply FITS keywords to the catalog?')).toBeNull();
    await advance(500);
    await advance(800);
    await settle();
    // It does not come back on its own.
    expect(screen.queryByText('Apply FITS keywords to the catalog?')).toBeNull();
    expect(callsOf('apply_header_fallbacks')).toHaveLength(0);
  });

  it('applies, shows progress and notifies once', async () => {
    preview = SOME_CHANGE;
    let finish: (c: HeaderFallbackCounts) => void = () => {};
    routeInvoke({
      get_settings_defaults: () => ({ kv: {}, headerFallbacks: defaultDoc() }),
      get_header_fallbacks: () => defaultDoc(),
      preview_header_fallbacks: () => preview,
      apply_header_fallbacks: () => new Promise<HeaderFallbackCounts>((resolve) => { finish = resolve; }),
    });
    await renderSection();
    expect(callsOf('preview_header_fallbacks')).toHaveLength(1);

    fireEvent.click(screen.getByText('Apply to catalog'));
    fireEvent.click(screen.getByRole('button', { name: 'Apply' }));
    await settle();
    expect(callsOf('apply_header_fallbacks')).toHaveLength(1);
    expect(screen.queryByText('Apply FITS keywords to the catalog?')).toBeNull();
    expect((screen.getByRole('button', { name: /Applying/ }) as HTMLButtonElement).disabled).toBe(true);

    act(() => listeners['header-fallbacks-progress']!({ phase: 'frames', done: 120, total: 312 }));
    expect(screen.getByRole('button', { name: 'Applying… 120 of 312 frames' })).toBeTruthy();
    act(() => listeners['header-fallbacks-progress']!({ phase: 'calibration', done: 0, total: 3 }));
    expect(screen.getByRole('button', { name: 'Grouping calibration frames…' })).toBeTruthy();
    expect(notifyMock).not.toHaveBeenCalled();

    preview = NO_CHANGE;
    await act(async () => {
      finish(SOME_CHANGE);
    });
    await settle();
    expect(notifyMock).toHaveBeenCalledTimes(1);
    expect(notifyMock).toHaveBeenCalledWith({
      kind: 'generic',
      tone: 'success',
      title: 'FITS keywords applied',
      detail: '312 frames changed, 3 calibration sets grouped again.',
    });
    // The preview is asked again and now has nothing to show.
    expect(callsOf('preview_header_fallbacks')).toHaveLength(2);
    expect(screen.queryByText('Apply to catalog')).toBeNull();
  });

  it('shows the server text when an Apply is refused', async () => {
    preview = SOME_CHANGE;
    routeInvoke({
      get_settings_defaults: () => ({ kv: {}, headerFallbacks: defaultDoc() }),
      get_header_fallbacks: () => defaultDoc(),
      preview_header_fallbacks: () => preview,
      apply_header_fallbacks: () => Promise.reject('FITS keyword rules are already being applied'),
    });
    await renderSection();
    fireEvent.click(screen.getByText('Apply to catalog'));
    fireEvent.click(screen.getByRole('button', { name: 'Apply' }));
    await settle();
    expect(notifyMock).toHaveBeenCalledTimes(1);
    expect(notifyMock).toHaveBeenCalledWith({
      kind: 'generic',
      tone: 'warning',
      hasErrors: true,
      title: 'FITS keywords not applied',
      detail: 'FITS keyword rules are already being applied',
    });
    expect(screen.getByText('Apply to catalog')).toBeTruthy();
  });

  it('warns once when the preview fails', async () => {
    routeInvoke({
      get_settings_defaults: () => ({ kv: {}, headerFallbacks: defaultDoc() }),
      get_header_fallbacks: () => defaultDoc(),
      preview_header_fallbacks: () => Promise.reject(new Error('catalog busy')),
    });
    await renderSection();
    expect(notifyMock).toHaveBeenCalledTimes(1);
    expect(notifyMock).toHaveBeenCalledWith({
      kind: 'generic',
      tone: 'warning',
      title: 'FITS keyword preview failed',
      detail: 'catalog busy',
      dedupeKey: expect.stringMatching(/^header-fallbacks-preview-/),
    });
  });

  it('uses a notification key of its own per mount, so a later visit is not silenced', async () => {
    routeInvoke({
      get_settings_defaults: () => ({ kv: {}, headerFallbacks: defaultDoc() }),
      get_header_fallbacks: () => defaultDoc(),
      preview_header_fallbacks: () => Promise.reject(new Error('catalog busy')),
    });
    const first = render(
      <SettingsDefaultsProvider>
        <HeaderKeywordsSection />
      </SettingsDefaultsProvider>,
    );
    await settle();
    first.unmount();
    await renderSection();
    expect(notifyMock).toHaveBeenCalledTimes(2);
    const keys = notifyMock.mock.calls.map(([n]) => (n as { dedupeKey: string }).dedupeKey);
    expect(keys[0]).not.toBe(keys[1]);
  });

  it('hides the line and Apply when the preview of the saved rules fails', async () => {
    preview = SOME_CHANGE;
    let fail = false;
    routeInvoke({
      get_settings_defaults: () => ({ kv: {}, headerFallbacks: defaultDoc() }),
      get_header_fallbacks: () => defaultDoc(),
      set_header_fallbacks: undefined,
      preview_header_fallbacks: () => (fail ? Promise.reject(new Error('catalog busy')) : preview),
    });
    await renderSection();
    expect(screen.getByText('Apply to catalog')).toBeTruthy();
    fail = true;
    fireEvent.keyDown(chip('CCD-TEMP'), { key: 'ArrowRight' });
    await advance(500);
    await advance(800);
    await settle();
    expect(notifyMock).toHaveBeenCalledTimes(1);
    expect(screen.queryByText('Apply to catalog')).toBeNull();
    expect(screen.queryByText(/read differently/)).toBeNull();
  });

  it('reset_all_refreshes_the_preview', async () => {
    await renderSection();
    expect(callsOf('preview_header_fallbacks')).toHaveLength(1);
    expect(screen.queryByText('Apply to catalog')).toBeNull();

    preview = SOME_CHANGE;
    fireEvent.click(screen.getByText('Reset all'));
    expect(screen.getByText('Reset FITS keywords?')).toBeTruthy();
    expect(screen.getByText(/Catalog values change only when you apply\./)).toBeTruthy();
    fireEvent.click(screen.getByRole('button', { name: 'Reset' }));
    await settle();
    expect(callsOf('reset_header_fallbacks')).toHaveLength(1);
    expect(callsOf('preview_header_fallbacks')).toHaveLength(2);
    expect(screen.getByText('312 frames read differently under these rules:')).toBeTruthy();
    // A reset is not a save.
    await advance(2000);
    expect(callsOf('set_header_fallbacks')).toHaveLength(0);
    expect(callsOf('preview_header_fallbacks')).toHaveLength(2);
  });

  it('re-runs the preview once after a burst of saves', async () => {
    await renderSection();
    expect(callsOf('preview_header_fallbacks')).toHaveLength(1);
    for (const key of ['ArrowRight', 'ArrowLeft', 'ArrowRight']) {
      const first = key === 'ArrowLeft' ? chip('CCD-TEMP') : screen.getAllByRole('listitem')[0]!;
      fireEvent.keyDown(first, { key });
      await advance(500);
    }
    expect(callsOf('set_header_fallbacks')).toHaveLength(3);
    // No preview inside the 800 ms window of the last save.
    expect(callsOf('preview_header_fallbacks')).toHaveLength(1);
    await advance(800);
    expect(callsOf('preview_header_fallbacks')).toHaveLength(2);
    await advance(3000);
    expect(callsOf('preview_header_fallbacks')).toHaveLength(2);
  });

  describe('preview calls', () => {
    it('keeps one call in flight, queues a single follow-up and drops the outdated answer', async () => {
      const first = deferred<HeaderFallbackCounts>();
      const next = deferred<HeaderFallbackCounts>();
      const answers = [first, next];
      routeInvoke({
        get_settings_defaults: () => ({ kv: {}, headerFallbacks: defaultDoc() }),
        get_header_fallbacks: () => defaultDoc(),
        set_header_fallbacks: undefined,
        preview_header_fallbacks: () => (answers.shift() ?? deferred<HeaderFallbackCounts>()).promise,
      });
      await renderSection();
      expect(callsOf('preview_header_fallbacks')).toHaveLength(1);

      // Two saves while the first call is out: both triggers coalesce, no second call yet.
      fireEvent.keyDown(chip('CCD-TEMP'), { key: 'ArrowRight' });
      await advance(500);
      await advance(800);
      fireEvent.keyDown(chip('CCD-TEMP'), { key: 'ArrowLeft' });
      await advance(500);
      await advance(800);
      expect(callsOf('set_header_fallbacks')).toHaveLength(2);
      expect(callsOf('preview_header_fallbacks')).toHaveLength(1);

      // The first answer is for rules that have changed since: dropped, and exactly one follow-up starts.
      await act(async () => {
        first.resolve(SOME_CHANGE);
      });
      await settle();
      expect(screen.queryByText(/read differently/)).toBeNull();
      expect(callsOf('preview_header_fallbacks')).toHaveLength(2);

      await act(async () => {
        next.resolve({ ...NO_CHANGE, byField: [{ field: 'gain', frames: 5 }], framesChanged: 5 });
      });
      await settle();
      expect(screen.getByText('5 frames read differently under these rules:')).toBeTruthy();
      expect(applyButton().disabled).toBe(false);
      await advance(3000);
      expect(callsOf('preview_header_fallbacks')).toHaveLength(2);
    });
  });
});
