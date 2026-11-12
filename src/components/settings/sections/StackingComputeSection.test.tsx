import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { StackingComputeSection } from './StackingComputeSection';
import { SettingsDefaultsProvider, type SettingsDefaults } from '../../../settings/SettingsDefaultsContext';
import { api } from '../../../api';
import { routeInvoke } from '../../../test/apiMock';

vi.mock('../../../api');

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../../../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));

const DEFAULTS_FIXTURE = { kv: { 'compute.memory_budget_mb': '0' } } as unknown as SettingsDefaults;

function setCalls() {
  return vi.mocked(api.invoke).mock.calls.filter(([cmd]) => cmd === 'set_setting');
}

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
  routeInvoke({ get_settings_defaults: DEFAULTS_FIXTURE, get_setting: '0', set_setting: undefined });
  vi.mocked(api.listen).mockResolvedValue(() => {});
});

function renderSection() {
  return render(
    <SettingsDefaultsProvider>
      <StackingComputeSection />
    </SettingsDefaultsProvider>,
  );
}

describe('StackingComputeSection', () => {
  it('renders the field and commits an integer on blur', async () => {
    renderSection();
    const input = (await screen.findByRole('spinbutton')) as HTMLInputElement;
    await screen.findByText('Frame memory budget (MB)');
    fireEvent.change(input, { target: { value: '1' } });
    fireEvent.blur(input);
    await waitFor(() => expect(setCalls()).toHaveLength(1));
    expect(setCalls()[0]![1]).toEqual({ key: 'compute.memory_budget_mb', value: '1' });
  });

  it('never writes an invalid draft', async () => {
    renderSection();
    const input = (await screen.findByRole('spinbutton')) as HTMLInputElement;
    fireEvent.change(input, { target: { value: '-5' } });
    fireEvent.blur(input);
    fireEvent.change(input, { target: { value: '1.5' } });
    fireEvent.blur(input);
    await new Promise((r) => setTimeout(r, 400));
    expect(setCalls()).toHaveLength(0);
  });
});
