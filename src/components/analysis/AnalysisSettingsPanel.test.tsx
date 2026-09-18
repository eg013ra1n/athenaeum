// Settings redesign (spec 2026-09-18 §5, plan Task D1 Step 3): pins the one
// behaviour the panel exists to deliver — a numeric field commits on blur,
// exactly once, with the whole `AnalysisConfig` document (`patch()` merges
// the one changed field into the last-loaded doc, `useAutosaveDocument`
// saves it whole). Typing alone must never write.
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { SettingsDefaultsProvider } from '../../settings/SettingsDefaultsContext';
import { AnalysisSettingsPanel } from './AnalysisSettingsPanel';
import { api } from '../../api';
import type { AnalysisConfig } from '../../types/analysis-config';

vi.mock('../../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
}));

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));

const ANALYSIS_CONFIG_FIXTURE: AnalysisConfig = {
  detection_sigma: 5.0,
  min_star_area: 5,
  max_star_area: 2000,
  saturation_fraction: 0.95,
  max_stars: 500,
  trail_threshold: 0.5,
  mrs_layers: 0,
  measure_cap: 2000,
  fit_max_iter: 25,
  fit_tolerance: 1e-4,
  fit_max_rejects: 5,
  batch_concurrency: 0,
};

function mockInvoke(command: string, _args?: Record<string, unknown>): Promise<unknown> {
  switch (command) {
    case 'get_settings_defaults':
      return Promise.resolve({ kv: {}, analysis: ANALYSIS_CONFIG_FIXTURE });
    case 'get_analysis_config':
      return Promise.resolve(ANALYSIS_CONFIG_FIXTURE);
    case 'get_setting':
      return Promise.resolve('');
    case 'set_analysis_config':
    case 'delete_setting':
      return Promise.resolve(undefined);
    default:
      return Promise.resolve(null);
  }
}

beforeEach(() => {
  vi.mocked(api.invoke).mockReset();
  vi.mocked(api.invoke).mockImplementation(mockInvoke as never);
  vi.mocked(api.listen).mockResolvedValue(() => {});
  notifyMock.mockClear();
});

function renderPanel() {
  return render(
    <SettingsDefaultsProvider>
      <AnalysisSettingsPanel />
    </SettingsDefaultsProvider>,
  );
}

describe('AnalysisSettingsPanel', () => {
  it('commits Max Stars on blur, once, with the whole config', async () => {
    renderPanel();

    // `500` (the fixture's `max_stars`) is unique across every rendered
    // field, so `getByDisplayValue` finds the Max Stars input without
    // relying on label/input association (the fields aren't `htmlFor`-linked).
    const input = await screen.findByDisplayValue('500');

    fireEvent.change(input, { target: { value: '750' } });
    expect(vi.mocked(api.invoke)).not.toHaveBeenCalledWith('set_analysis_config', expect.anything());

    fireEvent.blur(input);

    await waitFor(() => {
      const calls = vi.mocked(api.invoke).mock.calls.filter(([cmd]) => cmd === 'set_analysis_config');
      expect(calls).toHaveLength(1);
      expect(calls[0][1]).toEqual({ config: { ...ANALYSIS_CONFIG_FIXTURE, max_stars: 750 } });
    });
  });

  it('restores the draft on Escape without committing', async () => {
    renderPanel();
    const input = await screen.findByDisplayValue('500');

    fireEvent.change(input, { target: { value: '999' } });
    fireEvent.keyDown(input, { key: 'Escape' });
    expect(input).toHaveValue(500);

    fireEvent.blur(input);
    await new Promise((r) => setTimeout(r, 600));
    expect(vi.mocked(api.invoke)).not.toHaveBeenCalledWith('set_analysis_config', expect.anything());
  });

  it('commits Max Stars on Enter without waiting for blur', async () => {
    renderPanel();
    const input = await screen.findByDisplayValue('500');

    fireEvent.change(input, { target: { value: '640' } });
    fireEvent.keyDown(input, { key: 'Enter' });

    await waitFor(() => {
      const calls = vi.mocked(api.invoke).mock.calls.filter(([cmd]) => cmd === 'set_analysis_config');
      expect(calls).toHaveLength(1);
      expect(calls[0][1]).toEqual({ config: { ...ANALYSIS_CONFIG_FIXTURE, max_stars: 640 } });
    });
  });

  it('rejects an out-of-range value inline and never commits it', async () => {
    renderPanel();
    const input = await screen.findByDisplayValue('500');

    fireEvent.change(input, { target: { value: '99999' } });
    fireEvent.blur(input);

    expect(await screen.findByText(/Must be a whole number between 10 and 2000/)).toBeInTheDocument();
    await new Promise((r) => setTimeout(r, 600));
    expect(vi.mocked(api.invoke)).not.toHaveBeenCalledWith('set_analysis_config', expect.anything());
  });

  it('ticks the "Auto" checkbox immediately (a discrete control, no blur needed)', async () => {
    renderPanel();
    await screen.findByDisplayValue('500');

    const autoCheckbox = screen.getByRole('checkbox', { name: 'Auto' });
    expect(autoCheckbox).toBeChecked();

    fireEvent.click(autoCheckbox);

    await waitFor(() => {
      const calls = vi.mocked(api.invoke).mock.calls.filter(([cmd]) => cmd === 'set_analysis_config');
      expect(calls).toHaveLength(1);
      expect(calls[0][1]).toEqual({ config: { ...ANALYSIS_CONFIG_FIXTURE, batch_concurrency: 3 } });
    });
  });
});
