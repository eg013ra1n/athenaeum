// Settings redesign (spec 2026-09-18 §6, plan Task E1 Step 3): a KV section
// with no `onResetAll` of its own still gets a "Reset all" once its fields
// have registered themselves (`ResetAllContext`) — `general.autoMerge` is
// the simplest real one (two `SettingToggle`s, both with real KV defaults).
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { AutoMergeSection } from './AutoMergeSection';
import { SettingsDefaultsProvider, type SettingsDefaults } from '../../../settings/SettingsDefaultsContext';
import { api } from '../../../api';

vi.mock('../../../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
}));

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../../../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));

const DEFAULTS_FIXTURE = {
  kv: {
    'auto_merge.on_button_click': 'false',
    'auto_merge.on_monitor_detect': 'false',
  },
} as unknown as SettingsDefaults;

function mockInvoke(command: string, _args?: Record<string, unknown>): Promise<unknown> {
  switch (command) {
    case 'get_settings_defaults':
      return Promise.resolve(DEFAULTS_FIXTURE);
    case 'get_setting':
      // Both fields start non-default so there is something to reset.
      return Promise.resolve('true');
    case 'set_setting':
      return Promise.resolve(undefined);
    default:
      return Promise.resolve(null);
  }
}

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(mockInvoke as never);
  vi.mocked(api.listen).mockResolvedValue(() => {});
});

describe("a KV section's Reset all", () => {
  it('calls every field\'s reset', async () => {
    render(
      <SettingsDefaultsProvider>
        <AutoMergeSection />
      </SettingsDefaultsProvider>,
    );

    // "Reset all" only appears once both `SettingToggle`s have registered
    // themselves with the section (after `get_settings_defaults` resolves).
    const resetAllButton = await screen.findByRole('button', { name: 'Reset all' });
    fireEvent.click(resetAllButton);

    const confirmButton = await screen.findByRole('button', { name: 'Reset' });
    fireEvent.click(confirmButton);

    await waitFor(() => {
      const setCalls = vi
        .mocked(api.invoke)
        .mock.calls.filter(([cmd]) => cmd === 'set_setting')
        .map(([, callArgs]) => callArgs as { key: string; value: string });
      const keys = setCalls.map((c) => c.key);
      expect(keys).toContain('auto_merge.on_button_click');
      expect(keys).toContain('auto_merge.on_monitor_detect');
      // Every field resets to ITS OWN default, not merely gets touched.
      for (const call of setCalls) {
        expect(call.value).toBe('false');
      }
    });
  });
});
