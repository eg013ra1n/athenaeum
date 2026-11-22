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
    first.focus();
    fireEvent.keyDown(first, { key: 'ArrowRight' });
    await settle();
    // Focus follows the chip it moved.
    expect((document.activeElement as HTMLElement).dataset.keyword).toBe('CCD-TEMP');
    await advance(500);
    expect(callsOf('set_header_fallbacks')).toHaveLength(1);
    expect(savedOrder(0, 'ccdTemp')).toEqual(['DET-TEMP', 'CCD-TEMP']);
  });

  it('moves a chip by drag and drop', async () => {
    await renderSection();
    const dataTransfer = { setData: vi.fn(), getData: vi.fn(() => ''), effectAllowed: '', dropEffect: '' };
    fireEvent.dragStart(chip('CCD-TEMP'), { dataTransfer });
    fireEvent.dragOver(chip('DET-TEMP'), { dataTransfer });
    fireEvent.drop(chip('DET-TEMP'), { dataTransfer });
    await advance(500);
    expect(savedOrder(0, 'ccdTemp')).toEqual(['DET-TEMP', 'CCD-TEMP']);
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
      apply_header_fallbacks: () => Promise.reject(new Error('FITS keyword rules are already being applied')),
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
      dedupeKey: 'header-fallbacks-preview',
    });
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
});
