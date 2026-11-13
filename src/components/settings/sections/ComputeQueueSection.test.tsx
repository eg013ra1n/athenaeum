import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { ComputeQueueSection } from './ComputeQueueSection';
import { SettingsDefaultsProvider, type SettingsDefaults } from '../../../settings/SettingsDefaultsContext';
import { api } from '../../../api';
import { routeInvoke } from '../../../test/apiMock';

vi.mock('../../../api');

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../../../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));

const DEFAULTS_FIXTURE = { kv: { 'compute.max_concurrent': '1' } } as unknown as SettingsDefaults;

function callsOf(command: string) {
  return vi.mocked(api.invoke).mock.calls.filter(([cmd]) => cmd === command);
}

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
  routeInvoke({
    get_settings_defaults: DEFAULTS_FIXTURE,
    get_setting: '1',
    set_setting: undefined,
    set_compute_max_concurrent: undefined,
  });
  vi.mocked(api.listen).mockResolvedValue(() => {});
});

function renderSection() {
  return render(
    <SettingsDefaultsProvider>
      <ComputeQueueSection />
    </SettingsDefaultsProvider>,
  );
}

describe('ComputeQueueSection', () => {
  it('writes through the command that resizes the live queue, never the generic one', async () => {
    renderSection();
    const input = (await screen.findByRole('spinbutton')) as HTMLInputElement;
    await screen.findByText('Heavy jobs at the same time');
    fireEvent.change(input, { target: { value: '2' } });
    fireEvent.blur(input);
    await waitFor(() => expect(callsOf('set_compute_max_concurrent')).toHaveLength(1));
    expect(callsOf('set_compute_max_concurrent')[0]![1]).toEqual({ n: 2 });
    expect(callsOf('set_setting')).toHaveLength(0);
  });

  it('never writes a value outside 1 to 8', async () => {
    renderSection();
    const input = (await screen.findByRole('spinbutton')) as HTMLInputElement;
    for (const value of ['0', '9', '1.5']) {
      fireEvent.change(input, { target: { value } });
      fireEvent.blur(input);
    }
    await new Promise((r) => setTimeout(r, 400));
    expect(callsOf('set_compute_max_concurrent')).toHaveLength(0);
    expect(callsOf('set_setting')).toHaveLength(0);
  });
});
