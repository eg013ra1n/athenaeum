import { describe, expect, it, vi, beforeEach, afterEach } from 'vitest';
import { act, renderHook } from '@testing-library/react';
import type { ReactNode } from 'react';
import { useSettingField } from './useSettingField';
import { boolCodec, intCodec } from '../settings/codecs';
import { SettingsDefaultsProvider, type SettingsDefaults } from '../settings/SettingsDefaultsContext';
import { api } from '../api';

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));

vi.mock('../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));

vi.mock('../api', () => ({
  api: { invoke: vi.fn() },
}));

// Real registry entries (`src/settings/registry.ts`) — `useSettingField`
// calls `fieldMeta(section, field)` internally and throws on a miss, so the
// section/field pairs below must exist there.
const BOOL_FIELD = ['general.updates', 'autoCheck', 'updates.auto_check'] as const;
const INT_FIELD = ['blink.viewer', 'threads', 'blink.threads'] as const;

const DEFAULTS_FIXTURE = {
  kv: {
    'updates.auto_check': 'true',
    'blink.threads': '4',
  },
} as unknown as SettingsDefaults;

function wrapper({ children }: { children: ReactNode }) {
  return <SettingsDefaultsProvider>{children}</SettingsDefaultsProvider>;
}

/** A handful of microtask hops — enough for the provider's and the field's
 *  own mount-read chains (`await api.invoke(...)` then a `setState`) to
 *  settle, whether or not fake timers are active. */
async function flush() {
  await Promise.resolve();
  await Promise.resolve();
  await Promise.resolve();
}

type SetSettingHandler = (args: { key: string; value: string }) => void | Promise<void>;

function mockInvoke(settingValues: Record<string, string>, setSetting?: SetSettingHandler) {
  vi.mocked(api.invoke).mockImplementation(((command: string, args?: Record<string, unknown>) => {
    switch (command) {
      case 'get_settings_defaults':
        return Promise.resolve(DEFAULTS_FIXTURE);
      case 'get_setting': {
        const key = args?.key as string;
        const defaultValue = args?.defaultValue as string;
        return Promise.resolve(Object.prototype.hasOwnProperty.call(settingValues, key) ? settingValues[key] : defaultValue);
      }
      case 'set_setting':
        // Deferred so a handler that throws synchronously (a failure test)
        // becomes a rejected promise rather than a synchronous throw.
        return Promise.resolve().then(() => setSetting?.(args as { key: string; value: string }));
      default:
        return Promise.reject(new Error(`unexpected api.invoke("${command}")`));
    }
  }) as typeof api.invoke);
}

function setSettingCalls() {
  return vi.mocked(api.invoke).mock.calls.filter(([cmd]) => cmd === 'set_setting');
}

beforeEach(() => {
  notifyMock.mockClear();
  vi.mocked(api.invoke).mockReset();
});

afterEach(() => {
  vi.useRealTimers();
});

describe('useSettingField — discrete commit (setValue)', () => {
  it('writes once after the 300ms debounce; two calls in the window write the last value', async () => {
    vi.useFakeTimers();
    mockInvoke({ 'updates.auto_check': 'false' });

    const { result } = renderHook(() => useSettingField(...BOOL_FIELD, boolCodec), { wrapper });
    await act(async () => {
      await flush();
    });
    expect(result.current.value).toBe(false);

    act(() => {
      void result.current.setValue(true);
    });
    act(() => {
      void result.current.setValue(false);
    });
    act(() => {
      void result.current.setValue(true);
    });

    expect(setSettingCalls()).toHaveLength(0);

    await act(async () => {
      await vi.advanceTimersByTimeAsync(300);
    });

    const calls = setSettingCalls();
    expect(calls).toHaveLength(1);
    expect(calls[0][1]).toEqual({ key: 'updates.auto_check', value: 'true' });
  });
});

describe('useSettingField — text/number commit (draft/commit/escape)', () => {
  it('does not write on setDraft; commits a valid draft; rejects and never writes an invalid one; escape restores', async () => {
    mockInvoke({ 'blink.threads': '4' });

    const { result } = renderHook(() => useSettingField(...INT_FIELD, intCodec(1, 32)), { wrapper });
    await act(async () => {
      await flush();
    });
    expect(result.current.value).toBe(4);
    expect(result.current.draft).toBe('4');

    act(() => {
      result.current.setDraft('12');
    });
    expect(setSettingCalls()).toHaveLength(0);

    await act(async () => {
      await result.current.commit();
    });
    expect(setSettingCalls()).toHaveLength(1);
    expect(setSettingCalls()[0][1]).toEqual({ key: 'blink.threads', value: '12' });
    expect(result.current.value).toBe(12);

    act(() => {
      result.current.setDraft('99');
    });
    await act(async () => {
      await result.current.commit();
    });
    // Still just the one write from before — the invalid commit wrote nothing.
    expect(setSettingCalls()).toHaveLength(1);
    expect(result.current.error).toBe('Must be a whole number between 1 and 32');
    expect(result.current.draft).toBe('99');

    act(() => {
      result.current.escape();
    });
    expect(result.current.draft).toBe('12');
    expect(result.current.error).toBeNull();
  });
});

describe('useSettingField — reset', () => {
  it('writes the context default through set_setting and isDefault becomes true', async () => {
    mockInvoke({ 'updates.auto_check': 'false' });

    const { result } = renderHook(() => useSettingField(...BOOL_FIELD, boolCodec), { wrapper });
    await act(async () => {
      await flush();
    });
    expect(result.current.value).toBe(false);
    expect(result.current.isDefault).toBe(false);
    expect(result.current.defaultValue).toBe(true);

    await act(async () => {
      await result.current.reset();
    });

    const calls = setSettingCalls();
    expect(calls).toHaveLength(1);
    expect(calls[0][1]).toEqual({ key: 'updates.auto_check', value: 'true' });
    expect(result.current.value).toBe(true);
    expect(result.current.isDefault).toBe(true);
  });
});

describe('useSettingField — failure', () => {
  it('a rejected write sets error, keeps the draft, and notifies once', async () => {
    mockInvoke({ 'updates.auto_check': 'false' }, () => {
      throw new Error('offline');
    });

    const { result } = renderHook(() => useSettingField(...BOOL_FIELD, boolCodec), { wrapper });
    await act(async () => {
      await flush();
    });

    await act(async () => {
      await result.current.reset();
    });

    expect(result.current.error).toBe('offline');
    // The optimistic draft/value stay as the user's attempted change.
    expect(result.current.draft).toBe('true');
    expect(notifyMock).toHaveBeenCalledTimes(1);
    expect(notifyMock.mock.calls[0][0]).toMatchObject({
      title: 'Setting not saved',
      dedupeKey: 'updates.auto_check',
    });
  });
});

describe('useSettingField — write/read overrides', () => {
  it('routes the write through opts.write instead of set_setting', async () => {
    vi.useFakeTimers();
    mockInvoke({});
    const customWrite = vi.fn().mockResolvedValue(undefined);

    const { result } = renderHook(
      () => useSettingField(...INT_FIELD, intCodec(0, 32), { write: customWrite }),
      { wrapper },
    );
    await act(async () => {
      await flush();
    });

    act(() => {
      void result.current.setValue(7);
    });
    await act(async () => {
      await vi.advanceTimersByTimeAsync(300);
    });

    expect(customWrite).toHaveBeenCalledWith(7);
    expect(setSettingCalls()).toHaveLength(0);
  });

  it('routes the mount read through opts.read instead of get_setting', async () => {
    const customRead = vi.fn().mockResolvedValue('9');
    mockInvoke({});

    const { result } = renderHook(
      () => useSettingField(...INT_FIELD, intCodec(0, 32), { read: customRead }),
      { wrapper },
    );
    await act(async () => {
      await flush();
    });

    expect(customRead).toHaveBeenCalledTimes(1);
    expect(result.current.value).toBe(9);
    expect(vi.mocked(api.invoke).mock.calls.some(([cmd]) => cmd === 'get_setting')).toBe(false);
  });
});
