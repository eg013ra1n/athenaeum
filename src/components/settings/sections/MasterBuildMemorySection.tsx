// Settings redesign (spec 2026-09-18 §2) — Calibration tab, "Master build
// memory". `integration.band_budget_mb` writes through
// `set_integration_band_budget` (rebuilds the resolved figures immediately),
// not the default `set_setting` — so this refreshes `get_integration_band_budget`
// after every successful write to keep the "Applied: X MB" readout honest.
import { useCallback, useEffect, useState } from 'react';
import { api } from '../../../api';
import { SettingsSection } from '../SettingsSection';
import { SettingNumber } from '../SettingNumber';
import { intCodec } from '../../../settings/codecs';
import type { IntegrationBudgetInfo } from '../../../types/models';

// KEEP IN SYNC with `CONFIGURED_MIN_MB`/`CONFIGURED_MAX_MB` in
// `crates/athenaeum-core/src/integration/band_budget.rs` — those are
// private (not referenceable from TS), so this window is duplicated here by
// hand. A change on one side without the other means the UI confidently
// states the wrong range.
const CONFIGURED_MIN_MB = 256;
const CONFIGURED_MAX_MB = 16384;
const CONCURRENCY_NOTE =
  'because the compute.max_concurrent setting allows more than one heavy job (master build, analysis, …) to run at once';

function budgetNoteFor(info: IntegrationBudgetInfo | null): string | null {
  if (!info) return null;
  if (info.configuredMb === 0) {
    if (info.effectiveMb < info.autoMb) {
      return `Applying ${info.effectiveMb} MB, below the automatic ${info.autoMb} MB, ${CONCURRENCY_NOTE} and this budget is split between them.`;
    }
    return null;
  }
  if (info.effectiveMb === info.configuredMb) return null;
  const clamped = Math.min(CONFIGURED_MAX_MB, Math.max(CONFIGURED_MIN_MB, info.configuredMb));
  const wasClamped = clamped !== info.configuredMb;
  const alsoSplitByConcurrency = info.effectiveMb < clamped;
  if (wasClamped && alsoSplitByConcurrency) {
    return `Applying ${info.effectiveMb} MB: the value was clamped to ${clamped} MB (allowed range ${CONFIGURED_MIN_MB}-${CONFIGURED_MAX_MB} MB) and then split further ${CONCURRENCY_NOTE}.`;
  }
  if (wasClamped) {
    return `Clamped to ${clamped} MB — configured values are limited to the ${CONFIGURED_MIN_MB}-${CONFIGURED_MAX_MB} MB range.`;
  }
  if (alsoSplitByConcurrency) {
    return `Applying ${info.effectiveMb} MB, not the ${info.configuredMb} MB entered, ${CONCURRENCY_NOTE} and this budget is split between them.`;
  }
  return null;
}

export function MasterBuildMemorySection() {
  const [budgetInfo, setBudgetInfo] = useState<IntegrationBudgetInfo | null>(null);

  const loadBudgetInfo = useCallback(async () => {
    try {
      const info = await api.invoke<IntegrationBudgetInfo>('get_integration_band_budget');
      setBudgetInfo(info);
    } catch (err) {
      console.error('[MasterBuildMemorySection] get_integration_band_budget failed:', err);
    }
  }, []);

  useEffect(() => {
    void loadBudgetInfo();
  }, [loadBudgetInfo]);

  const writeBudget = useCallback(
    async (mb: number) => {
      await api.invoke('set_integration_band_budget', { mb });
      await loadBudgetInfo();
    },
    [loadBudgetInfo],
  );

  const note = budgetNoteFor(budgetInfo);

  return (
    <SettingsSection id="calibration.memory">
      <SettingNumber
        section="calibration.memory"
        field="budget"
        settingKey="integration.band_budget_mb"
        codec={intCodec(0, 16384)}
        write={writeBudget}
        unit="MB — 0 = automatic"
      />
      {budgetInfo && (
        <p className="text-xs text-content-muted mt-1">
          Applied: {budgetInfo.effectiveMb} MB — automatic would be {budgetInfo.autoMb} MB
          {budgetInfo.totalRamMb > 0 ? ` (from ${budgetInfo.totalRamMb} MB of RAM)` : ''} on this machine.
        </p>
      )}
      {note && <p className="text-xs text-content-muted mt-1">{note}</p>}
    </SettingsSection>
  );
}
