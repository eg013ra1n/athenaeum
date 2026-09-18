// Settings redesign (spec 2026-09-18 §5, plan Task D1 Step 3): the same
// commit-on-blur/Enter/Escape pin as `AnalysisSettingsPanel.test.tsx`, plus
// the "Refuse trailed frames" checkbox committing immediately (a discrete
// control), for the other panel this task migrated.
import { describe, expect, it, vi, beforeEach } from 'vitest';
import { render, screen, fireEvent, waitFor } from '@testing-library/react';
import { SettingsDefaultsProvider } from '../../settings/SettingsDefaultsContext';
import { PlateSolveSettingsPanel } from './PlateSolveSettingsPanel';
import { api } from '../../api';
import type { PlateSolveConfig } from '../../types/plate-solve';

vi.mock('../../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
}));

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));

const PLATE_SOLVE_CONFIG_FIXTURE: PlateSolveConfig = {
  sip_order: 3,
  base_verification_tolerance_arcsec: 8.0,
  autofind_tolerance_deg: 0.5,
  batch_concurrency: 0,
  blind_gate_enabled: true,
  blind_rms_max_px_mult: 2.5,
  blind_min_inlier_ratio: 0.04,
  blind_inlier_floor: 6,
  blind_scale_sanity_min: 0.05,
  blind_scale_sanity_max: 60.0,
  blind_scale_header_tol: 8.0,
  input_gate_enabled: true,
  input_max_eccentricity: 0.85,
  input_min_trail_r2: 0.65,
  camera_defaults: {},
  bright_cache_path: null,
} as unknown as PlateSolveConfig;

function mockInvoke(command: string, _args?: Record<string, unknown>): Promise<unknown> {
  switch (command) {
    case 'get_settings_defaults':
      return Promise.resolve({ kv: {}, plateSolve: PLATE_SOLVE_CONFIG_FIXTURE });
    case 'get_plate_solve_config':
      return Promise.resolve(PLATE_SOLVE_CONFIG_FIXTURE);
    case 'get_catalog_status':
      return Promise.resolve([]);
    case 'set_plate_solve_config':
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
      <PlateSolveSettingsPanel />
    </SettingsDefaultsProvider>,
  );
}

describe('PlateSolveSettingsPanel', () => {
  it('commits SIP Distortion Order on blur, once, with the whole config', async () => {
    renderPanel();

    // `3` (the fixture's `sip_order`) is unique across the panel's numeric
    // fields at mount (autofind is 0.5, tolerance 8, batch concurrency 0).
    const input = await screen.findByDisplayValue('3');

    fireEvent.change(input, { target: { value: '4' } });
    fireEvent.blur(input);

    await waitFor(() => {
      const calls = vi.mocked(api.invoke).mock.calls.filter(([cmd]) => cmd === 'set_plate_solve_config');
      expect(calls).toHaveLength(1);
      expect(calls[0][1]).toEqual({ config: { ...PLATE_SOLVE_CONFIG_FIXTURE, sip_order: 4 } });
    });
  });

  it('rejects an out-of-range SIP order inline and never commits it', async () => {
    renderPanel();
    const input = await screen.findByDisplayValue('3');

    fireEvent.change(input, { target: { value: '9' } });
    fireEvent.blur(input);

    expect(await screen.findByText(/Must be a whole number between 2 and 5/)).toBeInTheDocument();
    await new Promise((r) => setTimeout(r, 600));
    expect(vi.mocked(api.invoke)).not.toHaveBeenCalledWith('set_plate_solve_config', expect.anything());
  });

  it('toggles "Refuse trailed frames before solving" immediately, no blur needed', async () => {
    renderPanel();
    await screen.findByDisplayValue('3');

    const checkbox = screen.getByRole('checkbox', { name: 'Refuse trailed frames before solving' });
    expect(checkbox).toBeChecked();

    fireEvent.click(checkbox);

    await waitFor(() => {
      const calls = vi.mocked(api.invoke).mock.calls.filter(([cmd]) => cmd === 'set_plate_solve_config');
      expect(calls).toHaveLength(1);
      expect(calls[0][1]).toEqual({ config: { ...PLATE_SOLVE_CONFIG_FIXTURE, input_gate_enabled: false } });
    });
  });
});
