// Settings redesign (spec 2026-09-18 §3/§10, plan Task C1 Step 5): renders
// every tab and asserts the set of registered section ids equals the
// registry exactly — a section that exists in `SETTINGS_SECTIONS` but is
// never actually rendered by some tab (or vice versa) fails this. Until the
// remaining D-tasks migrate their panels onto real `SettingsSection`s, the
// panels that span several registry sections (`LoggingSettings`,
// `StackingSection`) register through the `RegisteredSections` shim instead
// — see each tab file's own comment. `AnalysisSettingsPanel`,
// `PlateSolveSettingsPanel` (Task D1), `CalibrationMatchingConfig`,
// `AccountSection`, `SyncSection` and `TransfersSection` (Task D3) render
// real `SettingsSection`s now — no shim left in `AnalysisTab`/
// `PlateSolvingTab`/`CalibrationTab`/`TransfersTab`, so their mock responses
// below must be real enough for each component to reach its own render (a
// `null` `get_analysis_config`/`get_plate_solve_config`/
// `get_calibration_matching_config` would strand the panel on its loading
// state forever and never register its section ids). Both migrated panels'
// mount loads are async, so the assertion below runs inside `waitFor`.
import { describe, expect, it, vi } from 'vitest';
import { render, waitFor } from '@testing-library/react';
import { SettingsDefaultsProvider } from '../../../settings/SettingsDefaultsContext';
import { SETTINGS_SECTIONS } from '../../../settings/registry';
import { renderedSectionIds } from '../renderedSections';
import { api } from '../../../api';
import type { PlateSolveConfig } from '../../../types/plate-solve';
import type { AnalysisConfig } from '../../../types/analysis-config';
import type { CalibrationMatchingConfig as CalibrationMatchingConfigType } from '../../../types/calibration-config';

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

// `PlateSolveSettingsPanel` (Task D1) gates its whole render on
// `useAutosaveDocument`'s `doc !== null` — a full, real fixture so the
// mount load resolves to something the panel's three sections (and their
// numeric fields' `codec.parse`) can render without crashing.
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

// `AnalysisSettingsPanel` (Task D1) gates its whole render the same way —
// a full fixture so `get_analysis_config` resolves to something every
// `DocNumberField` can format.
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

// `LoggingSettings` also has no null guard (`resp.config.level` runs
// unconditionally once the mount read resolves).
const LOGGING_CONFIG_RESPONSE_FIXTURE = { config: { level: 'info', modules: {} }, envOverrideActive: false };

// `CalibrationMatchingConfig` DOES have a `!config` guard — unlike the two
// above, it renders its `SettingsSection` (and so registers
// `calibration.matching`) only once `get_calibration_matching_config`
// resolves to something truthy. Minimal but structurally complete: the six
// groups' own components handle a missing per-type entry
// (`lights`/`flats`/`darks`/`clustering`/`behavioral_options`/
// `master_preferences` are all optional-keyed maps), but `scoring` and
// `warnings` are read unconditionally and must be fully populated.
const CALIBRATION_MATCHING_CONFIG_FIXTURE: CalibrationMatchingConfigType = {
  version: 1,
  lights: {},
  flats: {},
  darks: {},
  behavioral_options: {},
  master_preferences: {},
  clustering: {},
  scoring: {
    temperature_match_weight: 0.3,
    temperature_scale: 2.0,
    exposure_match_weight: 0.4,
    exposure_scale: 1.0,
  },
  warnings: {
    flat_date_warning_days: 30,
    dark_date_warning_days: 365,
    darkflat_date_warning_days: 365,
  },
};

function mockInvoke(command: string, _args?: Record<string, unknown>): Promise<unknown> {
  switch (command) {
    case 'get_settings_defaults':
      // `AnalysisSettingsPanel`/`PlateSolveSettingsPanel` (Task D1) read
      // `defaults.analysis`/`defaults.plateSolve` directly (for the
      // per-field reset affordance's default label) — a `kv`-only response
      // leaves those `undefined` and crashes the panel's render the moment
      // it tries `defaults?.analysis.detection_sigma`. Real fixtures for
      // every typed field the response carries, mirroring `SettingsDefaults`.
      return Promise.resolve({
        kv: {},
        analysis: ANALYSIS_CONFIG_FIXTURE,
        plateSolve: PLATE_SOLVE_CONFIG_FIXTURE,
        calibrationMatching: CALIBRATION_MATCHING_CONFIG_FIXTURE,
      });
    case 'get_setting':
      return Promise.resolve('');
    case 'get_analysis_config':
      return Promise.resolve(ANALYSIS_CONFIG_FIXTURE);
    case 'get_plate_solve_config':
      return Promise.resolve(PLATE_SOLVE_CONFIG_FIXTURE);
    case 'get_logging_config':
      return Promise.resolve(LOGGING_CONFIG_RESPONSE_FIXTURE);
    case 'get_calibration_matching_config':
      return Promise.resolve(CALIBRATION_MATCHING_CONFIG_FIXTURE);
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
  it('renders exactly the registered sections — no more, no fewer', async () => {
    renderAllTabs();

    // `AnalysisSettingsPanel`/`PlateSolveSettingsPanel` (Task D1) gate their
    // sections behind their mount-load promise (`useAutosaveDocument`),
    // unlike the still-shimmed panels which register synchronously — give
    // the mocked loads a tick to resolve before asserting.
    await waitFor(() => {
      const registered = new Set(SETTINGS_SECTIONS.map((s) => s.id));
      const rendered = renderedSectionIds();
      const missing = [...registered].filter((id) => !rendered.has(id));
      expect(missing, `registered but never rendered: ${missing.join(', ')}`).toEqual([]);
    });

    const rendered = renderedSectionIds();
    const registered = new Set(SETTINGS_SECTIONS.map((s) => s.id));

    const missing = [...registered].filter((id) => !rendered.has(id));
    const extra = [...rendered].filter((id) => !registered.has(id));

    expect(missing, `registered but never rendered: ${missing.join(', ')}`).toEqual([]);
    expect(extra, `rendered but not in the registry: ${extra.join(', ')}`).toEqual([]);
  });
});
