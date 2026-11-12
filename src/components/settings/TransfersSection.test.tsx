// Settings → Transfers: the two collaboration stream limits (L11) are KV
// `SettingNumber` fields through their own setters — commit on blur, an
// out-of-range draft never writes, a refused write surfaces through
// `notify()` only (docs/settings/README.md).
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { SettingsDefaultsProvider } from '../../settings/SettingsDefaultsContext';
import TransfersSection from './TransfersSection';
import { api } from '../../api';
import { routeInvoke } from '../../test/apiMock';

vi.mock('../../api');
vi.mock('../../api/desktop', () => ({ pickDirectory: vi.fn() }));

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));

const KV: Record<string, string> = {
  'sync.max_upload_bytes_per_sec': '0',
  'sync.max_concurrent_receives': '2',
  'collab.max_upload_streams': '8',
  'collab.max_receive_streams': '8',
};

beforeEach(() => {
  notifyMock.mockReset();
  vi.mocked(api.invoke).mockReset();
  routeInvoke({
    get_settings_defaults: { kv: KV },
    get_setting: (args?: { key?: string }) => KV[args?.key ?? ''] ?? '',
    get_transfer_paths: () => Promise.reject('not in this test'),
    get_transfer_storage: () => Promise.reject('not in this test'),
  });
  vi.mocked(api.listen).mockImplementation((() => Promise.resolve(() => {})) as never);
});

/** The number input of the field whose label reads `label`. */
async function inputFor(label: string): Promise<HTMLInputElement> {
  const labelEl = await screen.findByText(label);
  const field = labelEl.closest('div')?.parentElement;
  const input = field?.querySelector('input[type="number"]');
  expect(input).not.toBeNull();
  await waitFor(() => expect((input as HTMLInputElement).value).toBe('8'));
  return input as HTMLInputElement;
}

const renderIt = () =>
  render(
    <SettingsDefaultsProvider>
      <TransfersSection />
    </SettingsDefaultsProvider>,
  );

describe('TransfersSection — collaboration streams', () => {
  it('writes the upload limit through its own setter on blur, and never an out-of-range draft', async () => {
    renderIt();
    const input = await inputFor('Simultaneous collaboration uploads');

    fireEvent.change(input, { target: { value: '65' } });
    fireEvent.blur(input);
    await new Promise((r) => setTimeout(r, 0));
    expect(api.invoke).not.toHaveBeenCalledWith('set_collab_max_upload_streams', expect.anything());

    fireEvent.change(input, { target: { value: '12' } });
    fireEvent.blur(input);
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('set_collab_max_upload_streams', { maxUploadStreams: 12 }),
    );
  });

  it('writes the receive limit through its own setter, bounded 1..32', async () => {
    renderIt();
    const input = await inputFor('Simultaneous collaboration downloads');

    fireEvent.change(input, { target: { value: '33' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    await new Promise((r) => setTimeout(r, 0));
    expect(api.invoke).not.toHaveBeenCalledWith('set_collab_max_receive_streams', expect.anything());

    fireEvent.change(input, { target: { value: '4' } });
    fireEvent.keyDown(input, { key: 'Enter' });
    await waitFor(() =>
      expect(api.invoke).toHaveBeenCalledWith('set_collab_max_receive_streams', { maxReceiveStreams: 4 }),
    );
  });

  it('a refused write is one notification, no banner', async () => {
    routeInvoke({
      get_settings_defaults: { kv: KV },
      get_setting: (args?: { key?: string }) => KV[args?.key ?? ''] ?? '',
      set_collab_max_upload_streams: () => Promise.reject('collab.max_upload_streams must be 1..=64'),
      get_transfer_paths: () => Promise.reject('not in this test'),
      get_transfer_storage: () => Promise.reject('not in this test'),
    });
    renderIt();
    const input = await inputFor('Simultaneous collaboration uploads');
    fireEvent.change(input, { target: { value: '16' } });
    fireEvent.blur(input);
    await waitFor(() => expect(notifyMock).toHaveBeenCalledTimes(1));
    expect(notifyMock.mock.calls[0][0]).toMatchObject({ title: 'Setting not saved', tone: 'warning' });
  });
});
