import { describe, expect, it, vi } from 'vitest';
import { renderHook, waitFor } from '@testing-library/react';
import { useExportProgress } from './useExportProgress';
import { api } from '../api';

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));
vi.mock('../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

describe('useExportProgress', () => {
  it('the project (-1) completion notification carries counts and the first 3 warnings', async () => {
    const handlers: Record<string, (p: unknown) => void> = {};
    vi.mocked(api.listen).mockImplementation(((ev: string, cb: (p: unknown) => void) => {
      handlers[ev] = cb;
      return Promise.resolve(() => {});
    }) as never);
    renderHook(() => useExportProgress());
    await waitFor(() => expect(handlers['export-complete']).toBeDefined());
    handlers['export-complete']({
      frameSetId: -1, success: true, filesOrganized: 3, outputDir: '/o', error: null,
      warnings: ['w1', 'w2', 'w3', 'w4', 'w5'],
      linked: 1, copied: 0, rewritten: 2, failed: 0,
    });
    const arg = notifyMock.mock.calls[notifyMock.mock.calls.length - 1][0];
    expect(arg.detail).toBe('/o · 1 linked · 2 with FILTER rewritten · w1 · w2 · w3 · +2 more');
  });
});
