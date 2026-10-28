// Settings → General → Archive: the zip extraction cache limit (a KV number), its size, and Clear cache.
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { ArchiveSection } from './ArchiveSection';
import { SettingsDefaultsProvider, type SettingsDefaults } from '../../../settings/SettingsDefaultsContext';
import { api } from '../../../api';

vi.mock('../../../api', () => ({ api: { invoke: vi.fn(), listen: vi.fn() } }));

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../../../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));

const DEFAULTS = {
  kv: { 'archive.compression': 'store', 'archive.zip_cache_limit_gb': '32' },
} as unknown as SettingsDefaults;

let clearFails = false;

beforeEach(() => {
  notifyMock.mockReset();
  clearFails = false;
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(((cmd: string, args?: { key?: string }) => {
    switch (cmd) {
      case 'get_settings_defaults':
        return Promise.resolve(DEFAULTS);
      case 'get_setting':
        return Promise.resolve(args?.key === 'archive.zip_cache_limit_gb' ? '32' : 'store');
      case 'get_zip_cache_status':
        return Promise.resolve({ bytes: 3 * 1024 * 1024 * 1024, entries: 120, limitBytes: 32 * 1024 ** 3 });
      case 'clear_zip_cache':
        return clearFails
          ? Promise.reject('disk busy')
          : Promise.resolve({ bytes: 0, entries: 0, limitBytes: 32 * 1024 ** 3 });
      default:
        return Promise.resolve(undefined);
    }
  }) as never);
  vi.mocked(api.listen).mockResolvedValue(() => {});
});

const calls = (cmd: string) => vi.mocked(api.invoke).mock.calls.filter(([c]) => c === cmd);

function renderSection() {
  return render(
    <SettingsDefaultsProvider>
      <ArchiveSection />
    </SettingsDefaultsProvider>,
  );
}

describe('ArchiveSection — zip cache', () => {
  it('shows the cache size and clears it', async () => {
    renderSection();
    expect(await screen.findByTestId('zip-cache-status')).toHaveTextContent('3.00 GB in 120 unpacked frames');
    fireEvent.click(screen.getByRole('button', { name: /Clear cache/ }));
    await waitFor(() => expect(calls('clear_zip_cache')).toHaveLength(1));
    await waitFor(() => expect(screen.getByTestId('zip-cache-status')).toHaveTextContent('Empty.'));
    expect(screen.getByRole('button', { name: /Clear cache/ })).toBeDisabled();
    expect(notifyMock).not.toHaveBeenCalled();
  });

  it('reports a failed clear through notify() only', async () => {
    clearFails = true;
    renderSection();
    await screen.findByTestId('zip-cache-status');
    fireEvent.click(screen.getByRole('button', { name: /Clear cache/ }));
    await waitFor(() => expect(notifyMock).toHaveBeenCalledTimes(1));
    expect(notifyMock.mock.calls[0][0]).toMatchObject({ tone: 'warning', detail: 'disk busy' });
  });

  it('writes the limit as archive.zip_cache_limit_gb and never an invalid draft', async () => {
    renderSection();
    const input = (await screen.findByRole('spinbutton')) as HTMLInputElement;
    fireEvent.change(input, { target: { value: '64' } });
    fireEvent.blur(input);
    await waitFor(() => expect(calls('set_setting')).toHaveLength(1));
    expect(calls('set_setting')[0][1]).toEqual({ key: 'archive.zip_cache_limit_gb', value: '64' });
    fireEvent.change(input, { target: { value: '0' } });
    fireEvent.blur(input);
    await new Promise((r) => setTimeout(r, 400));
    expect(calls('set_setting')).toHaveLength(1);
  });
});
