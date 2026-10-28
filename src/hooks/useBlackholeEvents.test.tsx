// Black Hole state of a page's files: file-level ids and the frames of a zip that is in the Black Hole
// (owner ruling A) count the same.
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { act, renderHook, waitFor } from '@testing-library/react';
import { api } from '../api';
import { useBlackholeEvents } from './useBlackholeEvents';

vi.mock('../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

const listeners = new Map<string, (p: unknown) => void>();
let fileLevel: number[];
let zipLevel: number[];
const IDS = [1, 2, 3];

beforeEach(() => {
  listeners.clear();
  fileLevel = [1];
  zipLevel = [3];
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation((async (cmd: string) => {
    if (cmd === 'get_blackholed_file_ids') return fileLevel;
    if (cmd === 'get_zip_blackholed_file_ids') return zipLevel;
    return null;
  }) as never);
  vi.mocked(api.listen).mockImplementation(((name: string, cb: (p: unknown) => void) => {
    listeners.set(name, cb);
    return Promise.resolve(() => { listeners.delete(name); });
  }) as never);
});

describe('useBlackholeEvents', () => {
  it('counts frames of a Black Hole zip like file-level Black Hole ids', async () => {
    const { result } = renderHook(() => useBlackholeEvents(IDS));
    await waitFor(() => expect(result.current.isLoaded).toBe(true));
    expect([...result.current.blackholedFileIds].sort()).toEqual([1, 3]);
  });

  it('refetches the zip ids on a whole-zip event and follows file events', async () => {
    const { result } = renderHook(() => useBlackholeEvents(IDS));
    await waitFor(() => expect(result.current.blackholedFileIds.has(3)).toBe(true));
    zipLevel = [];
    await act(async () => { listeners.get('blackhole-changed')?.({ id: 9, zip_id: 4, action: 'zip-restored' }); });
    await waitFor(() => expect(result.current.blackholedFileIds.has(3)).toBe(false));
    expect(result.current.blackholedFileIds.has(1)).toBe(true);
    act(() => listeners.get('blackhole-changed')?.({ file_id: 2, action: 'blackholed' }));
    expect(result.current.blackholedFileIds.has(2)).toBe(true);
    act(() => listeners.get('blackhole-changed')?.({ file_id: 1, action: 'restored' }));
    expect(result.current.blackholedFileIds.has(1)).toBe(false);
  });
});
