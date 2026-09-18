// Settings redesign (spec 2026-09-18 §3/§10, plan Task C1 Step 5): renders
// every tab and asserts the set of registered section ids equals the
// registry exactly — a section that exists in `SETTINGS_SECTIONS` but is
// never actually rendered by some tab (or vice versa) fails this. Until the
// D-tasks migrate each hand-written panel onto real `SettingsSection`s, the
// panels that span several registry sections (`AnalysisSettingsPanel`,
// `PlateSolveSettingsPanel`, `CalibrationMatchingConfig`, `LoggingSettings`,
// `StackingSection`, `AccountSection`, `SyncSection`, `TransfersSection`)
// register through the `RegisteredSections` shim instead — see each tab
// file's own comment.
import { describe, expect, it, vi } from 'vitest';
import { render } from '@testing-library/react';
import { SettingsDefaultsProvider } from '../../../settings/SettingsDefaultsContext';
import { SETTINGS_SECTIONS } from '../../../settings/registry';
import { renderedSectionIds } from '../renderedSections';
import { api } from '../../../api';
import type { PlateSolveConfig } from '../../../types/plate-solve';

import { GeneralTab } from './GeneralTab';
import { BlinkTab } from './BlinkTab';
import { AnalysisTab } from './AnalysisTab';
import { PlateSolvingTab } from './PlateSolvingTab';
import { CalibrationTab } from './CalibrationTab';
import { StackingTab } from './StackingTab';
import { TransfersTab } from './TransfersTab';

vi.mock('../../../api', () => ({
  api: { invoke: vi.fn(), listen: vi.fn() },
}));

const { notifyMock } = vi.hoisted(() => ({ notifyMock: vi.fn() }));
vi.mock('../../../contexts/NotificationContext', () => ({
  useNotifications: () => ({ notify: notifyMock }),
}));

// `PlateSolveSettingsPanel` (unlike `AnalysisSettingsPanel`/
// `CalibrationMatchingConfig`) has no `!config` render guard — it seeds
// `useState` with a real default object and overwrites it with whatever
// `get_plate_solve_config` resolves to, so a `null` mock would crash the
// render deref'ing `config.sip_order` etc. Mirrors that panel's own local
// `DEFAULT_CONFIG` (`src/components/plate-solve/PlateSolveSettingsPanel.tsx`).
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

// `LoggingSettings` also has no null guard (`resp.config.level` runs
// unconditionally once the mount read resolves).
const LOGGING_CONFIG_RESPONSE_FIXTURE = { config: { level: 'info', modules: {} }, envOverrideActive: false };

function mockInvoke(command: string, _args?: Record<string, unknown>): Promise<unknown> {
  switch (command) {
    case 'get_settings_defaults':
      // Only `kv` is read by anything C1 wires up (`useSettingField`); the
      // typed fields are consumed by the still-unmigrated panels' own
      // `get_*_config` commands, not from this context yet (Tasks D1–D3).
      return Promise.resolve({ kv: {} });
    case 'get_setting':
      return Promise.resolve('');
    case 'get_plate_solve_config':
      return Promise.resolve(PLATE_SOLVE_CONFIG_FIXTURE);
    case 'get_logging_config':
      return Promise.resolve(LOGGING_CONFIG_RESPONSE_FIXTURE);
    case 'get_catalog_status':
    case 'list_account_devices':
      return Promise.resolve([]);
    case 'get_blink_threads_max':
      return Promise.resolve(8);
    default:
      return Promise.resolve(null);
  }
}

vi.mocked(api.invoke).mockImplementation(mockInvoke as never);
vi.mocked(api.listen).mockResolvedValue(() => {});

function renderAllTabs() {
  return render(
    <SettingsDefaultsProvider>
      <GeneralTab />
      <BlinkTab />
      <AnalysisTab />
      <PlateSolvingTab />
      <CalibrationTab />
      <StackingTab />
      <TransfersTab />
    </SettingsDefaultsProvider>,
  );
}

describe('Settings registry coverage', () => {
  it('renders exactly the registered sections — no more, no fewer', () => {
    renderAllTabs();

    const rendered = renderedSectionIds();
    const registered = new Set(SETTINGS_SECTIONS.map((s) => s.id));

    const missing = [...registered].filter((id) => !rendered.has(id));
    const extra = [...rendered].filter((id) => !registered.has(id));

    expect(missing, `registered but never rendered: ${missing.join(', ')}`).toEqual([]);
    expect(extra, `rendered but not in the registry: ${extra.join(', ')}`).toEqual([]);
  });
});
