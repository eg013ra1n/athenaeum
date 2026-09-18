// Settings redesign (spec 2026-09-18 §8, plan Task D2 Step 1): pins the
// Logging rewrite onto `useAutosaveDocument`/`LevelSelect` — one base-level
// select, five module rows each defaulting to "inherit the base level", no
// Save button, and a write for every change (debounced 500ms, the
// `useAutosaveDocument` default). The hook's own load/debounce/unmount
// discipline is pinned by `useAutosaveDocument.test.tsx`; this file only
// pins LoggingSettings' wiring on top of it.
import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { act, fireEvent, render, screen } from '@testing-library/react';
import LoggingSettings from './LoggingSettings';
import { api } from '../../api';
import type { LoggingConfigResponse } from '../../types/models';

vi.mock('../../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
}));

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));

// `LoggingSettings` only reads `defaults.logging` to pass through to
// `useAutosaveDocument`'s `defaults` option, which this file's fields never
// exercise (no ResetButton here) — `null` is a legitimate "defaults haven't
// loaded" state per the hook's own contract.
vi.mock('../../settings/SettingsDefaultsContext', () => ({
  useSettingsDefaults: () => ({ defaults: null, error: null }),
}));

function loggingConfigResponse(
  overrides: Partial<LoggingConfigResponse> = {},
): LoggingConfigResponse {
  return { config: { level: 'info', modules: {} }, envOverrideActive: false, ...overrides };
}

/** A handful of microtask hops — enough for the hook's mount-load effect to
 *  settle (same helper `useAutosaveDocument.test.tsx` uses). */
async function flush() {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

function mockGetLoggingConfig(resp: LoggingConfigResponse) {
  vi.mocked(api.invoke).mockImplementation((command: string) => {
    if (command === 'get_logging_config') return Promise.resolve(resp);
    if (command === 'set_logging_config') return Promise.resolve(undefined);
    return Promise.resolve(undefined);
  });
}

beforeEach(() => {
  notifyMock.mockClear();
  vi.mocked(api.invoke).mockReset();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('LoggingSettings', () => {
  it('renders the base level and every module row inheriting it, with no Save button', async () => {
    mockGetLoggingConfig(loggingConfigResponse());
    render(<LoggingSettings />);
    await act(async () => {
      await flush();
    });

    const selects = screen.getAllByRole('combobox') as HTMLSelectElement[];
    // Base level, then the five module rows (Scanner, Plate Solver,
    // Calibration, Archive / File Ops, Transport), in that order.
    expect(selects).toHaveLength(6);
    expect(selects[0].value).toBe('info');
    for (const s of selects.slice(1)) {
      expect(s.value).toBe('inherit');
    }

    expect(screen.queryAllByRole('button')).toHaveLength(0);
  });

  it('changing the base level writes the whole document, debounced', async () => {
    vi.useFakeTimers();
    mockGetLoggingConfig(loggingConfigResponse());
    render(<LoggingSettings />);
    await act(async () => {
      await flush();
    });

    const [baseSelect] = screen.getAllByRole('combobox');
    fireEvent.change(baseSelect, { target: { value: 'debug' } });

    expect(vi.mocked(api.invoke)).not.toHaveBeenCalledWith('set_logging_config', expect.anything());

    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });

    expect(api.invoke).toHaveBeenCalledWith('set_logging_config', {
      config: { level: 'debug', modules: {} },
    });
  });

  it('setting a module override keeps the base level and writes only that key', async () => {
    vi.useFakeTimers();
    mockGetLoggingConfig(loggingConfigResponse());
    render(<LoggingSettings />);
    await act(async () => {
      await flush();
    });

    const [, scannerSelect] = screen.getAllByRole('combobox');
    fireEvent.change(scannerSelect, { target: { value: 'debug' } });

    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });

    expect(api.invoke).toHaveBeenCalledWith('set_logging_config', {
      config: { level: 'info', modules: { scanner: 'debug' } },
    });
  });

  it('selecting Inherit on an overridden module deletes its key', async () => {
    vi.useFakeTimers();
    mockGetLoggingConfig(loggingConfigResponse({ config: { level: 'info', modules: { scanner: 'warn' } } }));
    render(<LoggingSettings />);
    await act(async () => {
      await flush();
    });

    const [, scannerSelect] = screen.getAllByRole('combobox');
    expect((scannerSelect as HTMLSelectElement).value).toBe('warn');
    fireEvent.change(scannerSelect, { target: { value: 'inherit' } });

    await act(async () => {
      await vi.advanceTimersByTimeAsync(500);
    });

    expect(api.invoke).toHaveBeenCalledWith('set_logging_config', {
      config: { level: 'info', modules: {} },
    });
  });

  it('shows the environment-override banner only when active', async () => {
    mockGetLoggingConfig(loggingConfigResponse({ envOverrideActive: true }));
    render(<LoggingSettings />);
    await act(async () => {
      await flush();
    });

    expect(screen.getByText(/overridden by ATHENAEUM_LOG/i)).toBeInTheDocument();
  });

  it('hides the environment-override banner when inactive', async () => {
    mockGetLoggingConfig(loggingConfigResponse({ envOverrideActive: false }));
    render(<LoggingSettings />);
    await act(async () => {
      await flush();
    });

    expect(screen.queryByText(/overridden by ATHENAEUM_LOG/i)).not.toBeInTheDocument();
  });
});
