import { act, renderHook } from '@testing-library/react';
import { beforeEach, describe, expect, it } from 'vitest';
import { NotificationProvider, useNotifications } from './NotificationContext';

const wrapper = ({ children }: { children: React.ReactNode }) => <NotificationProvider>{children}</NotificationProvider>;
const STORAGE_KEY = 'athenaeum.notifications.v1';
const stored = () => JSON.parse(localStorage.getItem(STORAGE_KEY) ?? '{}') as { seen: string[] };

beforeEach(() => localStorage.clear());

describe('NotificationContext dedupe', () => {
  it('a repeated dedupeKey is suppressed', () => {
    const { result } = renderHook(() => useNotifications(), { wrapper });
    act(() => result.current.notify({ title: 'a', detail: '', dedupeKey: 'k' }));
    act(() => result.current.notify({ title: 'a again', detail: '', dedupeKey: 'k' }));
    expect(result.current.notifications).toHaveLength(1);
  });

  it('a key suppressed again after 199 other new keys is still suppressed (kept recent)', () => {
    const { result } = renderHook(() => useNotifications(), { wrapper });
    act(() => result.current.notify({ title: 'first', detail: '', dedupeKey: 'k0' }));
    for (let i = 1; i < 200; i++) {
      act(() => result.current.notify({ title: `n${i}`, detail: '', dedupeKey: `k${i}`, toast: false }));
    }
    // Hit k0 again (suppressed, moved to newest), then add one more key.
    act(() => result.current.notify({ title: 'dup', detail: '', dedupeKey: 'k0' }));
    act(() => result.current.notify({ title: 'n200', detail: '', dedupeKey: 'k200', toast: false }));
    expect(stored().seen).toContain('k0');
    expect(stored().seen[stored().seen.length - 1]).toBe('k200');
    const before = result.current.notifications.length;
    act(() => result.current.notify({ title: 'dup2', detail: '', dedupeKey: 'k0' }));
    expect(result.current.notifications).toHaveLength(before);
    expect(result.current.notifications.filter((n) => n.title === 'dup' || n.title === 'dup2')).toHaveLength(0);
  });
});
