// Settings redesign (spec 2026-09-18 §4/§5/§6, plan Task D1): every
// `AnalysisConfig` field commits through `useAutosaveDocument` — no Save
// button, no "settings saved" banner. Five registered sections
// (`analysis.detection/measurement/psf/batch/rejection`); `onResetAll` on
// the first calls the document's whole-config `resetAll` (see that option's
// own comment for why the reset special-cases `batch_concurrency`).
//
// `analysis.rejection_defaults` and `analysis.fwhm_default_unit` are two
// plain KV settings (not part of `AnalysisConfig`) that predate this
// redesign and carry no Rust-registered default — `useSettingField`'s own
// `isDefault`/`reset()` degrade gracefully for such a key (see the hook's
// doc comment), so this panel uses the two hooks for their write/notify
// plumbing only and keeps its own composite local state for the five
// threshold sub-fields the JSON blob packs together.
import { useState, useEffect, useCallback, useRef, type KeyboardEvent } from 'react';
import { api } from '../../api';
import type { AnalysisConfig } from '../../types/analysis-config';
import { THRESHOLD_FIELDS, EMPTY_THRESHOLDS, type RejectionThresholds, type ThresholdFieldKey } from '../calibration/RejectionThresholdBar';
import { useAutosaveDocument } from '../../hooks/useAutosaveDocument';
import { useSettingField } from '../../hooks/useSettingField';
import { useSettingsDefaults } from '../../settings/SettingsDefaultsContext';
import { fieldMeta } from '../../settings/registry';
import { intCodec, floatCodec, stringCodec, enumCodec, type Codec } from '../../settings/codecs';
import { SettingsSection } from '../settings/SettingsSection';
import { Checkbox } from '../settings/Checkbox';
import { ResetButton, SavedTick } from '../settings/ResetButton';

export type FwhmUnit = 'px' | 'arcsec';

export const FWHM_UNIT_SETTING_KEY = 'analysis.fwhm_default_unit';
const REJECTION_DEFAULTS_KEY = 'analysis.rejection_defaults';

/** Draft/blur/Enter/Escape discipline for one numeric input, generalized
 *  over a plain `(value, onCommit)` pair so both a typed-document field
 *  (`DocNumberField` below, committing through `patch()`) and the batch-
 *  concurrency manual input (which needs its label/reset row laid out
 *  alongside the "Auto" checkbox rather than in its own block) can share it. */
function useNumberDraft(value: number, codec: Codec<number>, onCommit: (n: number) => void) {
  const [draft, setDraft] = useState(String(value));
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setDraft(String(value));
    setError(null);
  }, [value]);

  // "Latest ref": a caller typically passes a fresh `onCommit` closure
  // every render (e.g. `(n) => patch({ max_stars: n })`) — reading it
  // through a ref keeps `commit`'s identity stable without a stale-closure
  // risk (same idiom as `useSettingField`/`useAutosaveDocument`).
  const onCommitRef = useRef(onCommit);
  onCommitRef.current = onCommit;

  const commit = useCallback(() => {
    const parsed = codec.parse(draft);
    if (parsed instanceof Error) {
      setError(parsed.message);
      return;
    }
    const validationError = codec.validate?.(parsed);
    if (validationError) {
      setError(validationError);
      return;
    }
    setError(null);
    if (parsed !== value) onCommitRef.current(parsed);
  }, [codec, draft, value]);

  const handleKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      commit();
    } else if (e.key === 'Escape') {
      e.preventDefault();
      setDraft(String(value));
      setError(null);
    }
  };

  return { draft, setDraft, error, commit, handleKeyDown };
}

interface DocNumberFieldProps {
  section: string;
  field: string;
  value: number;
  codec: Codec<number>;
  min?: number;
  max?: number;
  step?: number;
  unit?: string;
  onCommit: (n: number) => void;
  isDefault: boolean;
  defaultValue: number;
  onReset: () => void;
  disabled?: boolean;
}

/** A numeric field bound to one path of the `useAutosaveDocument` doc —
 *  commits on blur/Enter through `onCommit` (which the caller wires to
 *  `patch({...})`), Escape restores. Label/help come from the registry. */
function DocNumberField({
  section,
  field,
  value,
  codec,
  min,
  max,
  step,
  unit,
  onCommit,
  isDefault,
  defaultValue,
  onReset,
  disabled,
}: DocNumberFieldProps) {
  const meta = fieldMeta(section, field);
  const { draft, setDraft, error, commit, handleKeyDown } = useNumberDraft(value, codec, onCommit);

  return (
    <div>
      <div className="flex items-center justify-between gap-2 mb-1">
        <label className="block text-xs text-content-secondary">
          {meta.label}
          {unit && <span className="text-content-muted"> ({unit})</span>}
        </label>
        <ResetButton
          visible={!isDefault}
          defaultLabel={unit ? `${defaultValue} ${unit}` : String(defaultValue)}
          onReset={onReset}
          disabled={disabled}
        />
      </div>
      <input
        type="number"
        value={draft}
        min={min}
        max={max}
        step={step}
        disabled={disabled}
        onChange={(e) => setDraft(e.target.value)}
        onBlur={commit}
        onKeyDown={handleKeyDown}
        className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent disabled:opacity-50 disabled:cursor-not-allowed"
      />
      {meta.help && <p className="text-xs text-content-muted mt-1">{meta.help}</p>}
      {error && <p className="text-xs text-error mt-1">{error}</p>}
    </div>
  );
}

export function AnalysisSettingsPanel() {
  const { defaults } = useSettingsDefaults();

  const [rejectionDefaults, setRejectionDefaults] = useState<RejectionThresholds>(EMPTY_THRESHOLDS);
  const [fwhmUnit, setFwhmUnit] = useState<FwhmUnit>('px');
  const lastCommittedRejectionRef = useRef<RejectionThresholds>(EMPTY_THRESHOLDS);

  const { doc: config, patch, error: docError, savedAt, isDefault, resetField, resetAll } = useAutosaveDocument<AnalysisConfig>({
    load: () => api.invoke<AnalysisConfig>('get_analysis_config'),
    save: (c) => api.invoke('set_analysis_config', { config: c }),
    resetAll: async () => {
      // `reset_analysis_config` persists and returns `AnalysisConfig::default()`,
      // whose `batch_concurrency` is the computed core count, not 0 — the
      // "Auto" checkbox reads `batch_concurrency === 0`, and the backend
      // already treats 0 as "auto, compute at run time". Show and persist 0
      // so Reset leaves Auto ticked instead of showing a manual value with
      // nothing to explain it (unchanged from the pre-redesign handler).
      const result = await api.invoke<AnalysisConfig>('reset_analysis_config');
      if (result.batch_concurrency !== 0) {
        await api.invoke('set_analysis_config', { config: { ...result, batch_concurrency: 0 } });
      }
      // The two legacy rejection-default keys aren't part of `AnalysisConfig`
      // — a whole-config reset clears them too, exactly as the old handler did.
      await api.invoke('delete_setting', { key: REJECTION_DEFAULTS_KEY });
      await api.invoke('delete_setting', { key: FWHM_UNIT_SETTING_KEY });
      setRejectionDefaults(EMPTY_THRESHOLDS);
      lastCommittedRejectionRef.current = EMPTY_THRESHOLDS;
      setFwhmUnit('px');
    },
    defaults: defaults?.analysis ?? null,
    label: 'Analysis settings',
  });

  // Legacy load for the two rejection-default keys — the exact merge the
  // pre-redesign `loadConfig` used (inline `fwhm_unit` in the JSON wins,
  // the standalone key is the fallback for older saves).
  useEffect(() => {
    let cancelled = false;
    (async () => {
      try {
        const unit = await api.invoke<string>('get_setting', { key: FWHM_UNIT_SETTING_KEY, defaultValue: 'px' });
        const prefersArcsec = unit === 'arcsec';
        const json = await api.invoke<string>('get_setting', { key: REJECTION_DEFAULTS_KEY, defaultValue: '' });
        if (cancelled) return;
        let resolvedUnit: FwhmUnit = prefersArcsec ? 'arcsec' : 'px';
        let merged: RejectionThresholds = EMPTY_THRESHOLDS;
        if (json) {
          const saved = JSON.parse(json) as Partial<RejectionThresholds>;
          merged = { ...EMPTY_THRESHOLDS, ...saved };
          if (saved.fwhm_unit === 'arcsec' || saved.fwhm_unit === 'px') {
            resolvedUnit = saved.fwhm_unit;
          }
          merged.fwhm_unit = merged.fwhm ? resolvedUnit : undefined;
        }
        setRejectionDefaults(merged);
        lastCommittedRejectionRef.current = merged;
        setFwhmUnit(resolvedUnit);
      } catch (err) {
        console.error('[AnalysisSettingsPanel] failed to load rejection defaults:', err);
      }
    })();
    return () => {
      cancelled = true;
    };
  }, []);

  // Two `useSettingField`s carry the write/notify side of the two legacy
  // keys above. `analysis.rejection_defaults` is one JSON blob five
  // registered fields share (`fwhm`/`eccentricity`/`frameSnr`/`snrWeight`/
  // `trailed`) — `fwhm` stands in for the hook's own `fieldMeta` plumbing
  // since none of the five is "the whole blob" on its own; its `meta` is
  // never rendered here (each sub-field gets its own label straight from
  // the registry below).
  const rejectionJsonField = useSettingField<string>('analysis.rejection', 'fwhm', REJECTION_DEFAULTS_KEY, stringCodec());
  const fwhmUnitField = useSettingField<FwhmUnit>('analysis.rejection', 'fwhmUnit', FWHM_UNIT_SETTING_KEY, enumCodec(['px', 'arcsec'] as const));
  // "Latest ref" pattern (see `useSettingField`'s own comment on the same
  // idiom): `rejectionJsonField` is a fresh object every render, so
  // `commitRejection` reads its `setValue` through a ref instead of taking
  // a dependency on the whole object — keeps `commitRejection`'s identity
  // stable without risking a stale closure over an old `setValue`.
  const setRejectionJsonRef = useRef(rejectionJsonField.setValue);
  setRejectionJsonRef.current = rejectionJsonField.setValue;

  const commitRejection = useCallback((next: RejectionThresholds, unit: FwhmUnit) => {
    lastCommittedRejectionRef.current = next;
    const numericKeys: ThresholdFieldKey[] = ['fwhm', 'eccentricity', 'frame_snr', 'snr_weight', 'trail'];
    const hasAny = numericKeys.some((k) => next[k] !== '');
    let json = '';
    if (hasAny) {
      const toSave: Partial<RejectionThresholds> = {};
      for (const k of numericKeys) {
        const v = next[k];
        if (v !== '') (toSave as Record<string, string>)[k] = v;
      }
      if (toSave.fwhm) toSave.fwhm_unit = unit;
      json = JSON.stringify(toSave);
    }
    void setRejectionJsonRef.current(json);
  }, []);

  const handleUnitChange = (unit: FwhmUnit) => {
    setFwhmUnit(unit);
    void fwhmUnitField.setValue(unit);
    // The unit is embedded in the JSON alongside `fwhm` — re-commit so the
    // two never drift apart (a discrete control, so this fires immediately).
    commitRejection(rejectionDefaults, unit);
  };

  const handleThresholdKeyDown = (e: KeyboardEvent<HTMLInputElement>) => {
    if (e.key === 'Enter') {
      e.preventDefault();
      commitRejection(rejectionDefaults, fwhmUnit);
    } else if (e.key === 'Escape') {
      e.preventDefault();
      setRejectionDefaults(lastCommittedRejectionRef.current);
    }
  };

  const hasAnyThreshold = (['fwhm', 'eccentricity', 'frame_snr', 'snr_weight', 'trail'] as const).some(
    (k) => rejectionDefaults[k] !== '',
  );

  const clearRejectionDefaults = () => {
    setRejectionDefaults(EMPTY_THRESHOLDS);
    commitRejection(EMPTY_THRESHOLDS, fwhmUnit);
  };

  const batchConcurrency = useNumberDraft(config?.batch_concurrency ?? 3, intCodec(1, 16), (n) => patch({ batch_concurrency: n }));

  if (!config) {
    return (
      <div className="text-center py-8 text-content-muted">
        Loading analysis settings...
      </div>
    );
  }

  const rejectionSavedAt = Math.max(rejectionJsonField.savedAt ?? 0, fwhmUnitField.savedAt ?? 0) || null;
  const rejectionError = fwhmUnitField.error ?? rejectionJsonField.error;

  return (
    <div className="space-y-6">
      {docError && <p className="text-xs text-error">{docError}</p>}

      {/* Star Detection Parameters — also carries the whole-panel reset:
          `SettingsSection`'s own confirm dialog names this section
          ("Star Detection Parameters") since its copy comes from the
          registry, which this task does not touch — the reset itself does
          reset the whole Analysis configuration (every section below),
          not just this one. */}
      <SettingsSection
        id="analysis.detection"
        onResetAll={resetAll}
        resetScopeLabel="This resets the whole Analysis configuration back to its default."
        actions={<SavedTick savedAt={savedAt} />}
      >
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
          <DocNumberField
            section="analysis.detection"
            field="detectionSigma"
            value={config.detection_sigma}
            codec={floatCodec(1, 20)}
            min={1}
            max={20}
            step={0.5}
            onCommit={(n) => patch({ detection_sigma: n })}
            isDefault={isDefault('detection_sigma')}
            defaultValue={defaults?.analysis?.detection_sigma ?? config.detection_sigma}
            onReset={() => resetField('detection_sigma')}
          />
          <DocNumberField
            section="analysis.detection"
            field="maxStars"
            value={config.max_stars}
            codec={intCodec(10, 2000)}
            min={10}
            max={2000}
            step={10}
            onCommit={(n) => patch({ max_stars: n })}
            isDefault={isDefault('max_stars')}
            defaultValue={defaults?.analysis?.max_stars ?? config.max_stars}
            onReset={() => resetField('max_stars')}
          />
          <DocNumberField
            section="analysis.detection"
            field="minStarArea"
            value={config.min_star_area}
            codec={intCodec(1, 100000)}
            min={1}
            step={1}
            onCommit={(n) => patch({ min_star_area: n })}
            isDefault={isDefault('min_star_area')}
            defaultValue={defaults?.analysis?.min_star_area ?? config.min_star_area}
            onReset={() => resetField('min_star_area')}
          />
          <DocNumberField
            section="analysis.detection"
            field="maxStarArea"
            value={config.max_star_area}
            codec={intCodec(1, 100000)}
            min={1}
            step={10}
            onCommit={(n) => patch({ max_star_area: n })}
            isDefault={isDefault('max_star_area')}
            defaultValue={defaults?.analysis?.max_star_area ?? config.max_star_area}
            onReset={() => resetField('max_star_area')}
          />
          <DocNumberField
            section="analysis.detection"
            field="saturationFraction"
            value={config.saturation_fraction}
            codec={floatCodec(0.5, 1.0)}
            min={0.5}
            max={1.0}
            step={0.01}
            onCommit={(n) => patch({ saturation_fraction: n })}
            isDefault={isDefault('saturation_fraction')}
            defaultValue={defaults?.analysis?.saturation_fraction ?? config.saturation_fraction}
            onReset={() => resetField('saturation_fraction')}
          />
          <DocNumberField
            section="analysis.detection"
            field="trailThreshold"
            value={config.trail_threshold}
            codec={floatCodec(0, 1)}
            min={0}
            max={1}
            step={0.05}
            onCommit={(n) => patch({ trail_threshold: n })}
            isDefault={isDefault('trail_threshold')}
            defaultValue={defaults?.analysis?.trail_threshold ?? config.trail_threshold}
            onReset={() => resetField('trail_threshold')}
          />
        </div>
      </SettingsSection>

      {/* Measurement Method */}
      <SettingsSection id="analysis.measurement" actions={<SavedTick savedAt={savedAt} />}>
        <DocNumberField
          section="analysis.measurement"
          field="mrsLayers"
          value={config.mrs_layers}
          codec={intCodec(0, 10)}
          min={0}
          max={10}
          step={1}
          onCommit={(n) => patch({ mrs_layers: n })}
          isDefault={isDefault('mrs_layers')}
          defaultValue={defaults?.analysis?.mrs_layers ?? config.mrs_layers}
          onReset={() => resetField('mrs_layers')}
        />
      </SettingsSection>

      {/* PSF Fitting */}
      <SettingsSection id="analysis.psf" actions={<SavedTick savedAt={savedAt} />}>
        <div className="grid grid-cols-1 gap-4 md:grid-cols-2 xl:grid-cols-3">
          <DocNumberField
            section="analysis.psf"
            field="measureCap"
            value={config.measure_cap}
            codec={intCodec(0, 100000)}
            min={0}
            max={100000}
            step={100}
            onCommit={(n) => patch({ measure_cap: n })}
            isDefault={isDefault('measure_cap')}
            defaultValue={defaults?.analysis?.measure_cap ?? config.measure_cap}
            onReset={() => resetField('measure_cap')}
          />
          <DocNumberField
            section="analysis.psf"
            field="fitMaxIter"
            value={config.fit_max_iter}
            codec={intCodec(1, 200)}
            min={1}
            max={200}
            step={5}
            onCommit={(n) => patch({ fit_max_iter: n })}
            isDefault={isDefault('fit_max_iter')}
            defaultValue={defaults?.analysis?.fit_max_iter ?? config.fit_max_iter}
            onReset={() => resetField('fit_max_iter')}
          />
          <DocNumberField
            section="analysis.psf"
            field="fitTolerance"
            value={config.fit_tolerance}
            codec={floatCodec(1e-8, 1e-2)}
            min={1e-8}
            max={0.01}
            step={0.0001}
            onCommit={(n) => patch({ fit_tolerance: n })}
            isDefault={isDefault('fit_tolerance')}
            defaultValue={defaults?.analysis?.fit_tolerance ?? config.fit_tolerance}
            onReset={() => resetField('fit_tolerance')}
          />
          <DocNumberField
            section="analysis.psf"
            field="fitMaxRejects"
            value={config.fit_max_rejects}
            codec={intCodec(1, 100)}
            min={1}
            max={100}
            step={1}
            onCommit={(n) => patch({ fit_max_rejects: n })}
            isDefault={isDefault('fit_max_rejects')}
            defaultValue={defaults?.analysis?.fit_max_rejects ?? config.fit_max_rejects}
            onReset={() => resetField('fit_max_rejects')}
          />
        </div>
      </SettingsSection>

      {/* Batch Processing */}
      <SettingsSection id="analysis.batch" actions={<SavedTick savedAt={savedAt} />}>
        <div>
          <div className="flex items-center justify-between gap-2 mb-1">
            <label className="block text-xs text-content-secondary">
              {fieldMeta('analysis.batch', 'concurrentFrames').label}
            </label>
            <ResetButton
              visible={!isDefault('batch_concurrency')}
              defaultLabel={String(defaults?.analysis?.batch_concurrency ?? config.batch_concurrency)}
              onReset={() => resetField('batch_concurrency')}
            />
          </div>
          <div className="flex items-center gap-3">
            <Checkbox
              checked={config.batch_concurrency === 0}
              onChange={(checked) => patch({ batch_concurrency: checked ? 0 : 3 })}
              label="Auto"
            />
            {config.batch_concurrency !== 0 && (
              <input
                type="number"
                value={batchConcurrency.draft}
                min={1}
                max={16}
                step={1}
                onChange={(e) => batchConcurrency.setDraft(e.target.value)}
                onBlur={batchConcurrency.commit}
                onKeyDown={batchConcurrency.handleKeyDown}
                className="flex-1 bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
              />
            )}
          </div>
          <p className="text-xs text-content-muted mt-1">
            {config.batch_concurrency === 0
              ? 'Automatically set based on CPU cores (~1 frame per 3 cores).'
              : 'Number of frames analyzed simultaneously. Higher values use more CPU and memory (~200MB per frame).'}
          </p>
          {batchConcurrency.error && <p className="text-xs text-error mt-1">{batchConcurrency.error}</p>}
        </div>
      </SettingsSection>

      {/* Default Rejection Thresholds */}
      <SettingsSection id="analysis.rejection" actions={<SavedTick savedAt={rejectionSavedAt} />}>
        {/* FWHM unit toggle */}
        <div className="flex items-center gap-3 mb-3">
          <span className="text-xs text-content-secondary">{fieldMeta('analysis.rejection', 'fwhmUnit').label}:</span>
          <div className="flex rounded-lg border border-border overflow-hidden">
            <button
              type="button"
              onClick={() => handleUnitChange('px')}
              className={`px-3 py-1 text-xs font-medium transition-colors ${
                fwhmUnit === 'px'
                  ? 'bg-accent/20 text-accent'
                  : 'bg-surface-hover text-content-muted hover:text-content-secondary'
              }`}
            >
              Pixels
            </button>
            <button
              type="button"
              onClick={() => handleUnitChange('arcsec')}
              className={`px-3 py-1 text-xs font-medium border-l border-border transition-colors ${
                fwhmUnit === 'arcsec'
                  ? 'bg-accent/20 text-accent'
                  : 'bg-surface-hover text-content-muted hover:text-content-secondary'
              }`}
            >
              Arcseconds
            </button>
          </div>
          <span className="text-[11px] text-content-muted">
            {fwhmUnit === 'arcsec'
              ? 'Stored value is in arcsec; the Analysis tab opens in arcsec mode when a plate scale is available.'
              : 'Stored value is in pixels (raw FWHM).'}
          </span>
        </div>

        <div className="grid grid-cols-3 gap-3">
          {THRESHOLD_FIELDS.map((rawField) => {
            const field =
              rawField.key === 'fwhm' && fwhmUnit === 'arcsec'
                ? { ...rawField, label: 'FWHM (") >', placeholder: '"' }
                : rawField;
            return (
              <div key={field.key}>
                <label className="block text-xs text-content-secondary mb-1">{field.label}</label>
                <input
                  type="number"
                  step={field.step}
                  min={field.min}
                  max={field.max}
                  value={rejectionDefaults[field.key]}
                  onChange={(e) => setRejectionDefaults((prev) => ({ ...prev, [field.key]: e.target.value }))}
                  onBlur={() => commitRejection(rejectionDefaults, fwhmUnit)}
                  onKeyDown={handleThresholdKeyDown}
                  placeholder={field.placeholder}
                  className="w-full bg-surface-hover border border-border rounded-lg px-3 py-2 text-sm text-content focus:outline-none focus:border-accent"
                />
              </div>
            );
          })}
          {/* `trail` is a checkbox on the Analysis tab's own threshold bar
           *  (`RejectionThresholdBar`'s "Trailed" control) rather than a
           *  `THRESHOLD_FIELDS` entry — matched here so its default can
           *  actually be set from Settings. A discrete control: commits
           *  immediately. */}
          <div>
            <label className="block text-xs text-content-secondary mb-1">
              {fieldMeta('analysis.rejection', 'trailed').label}
            </label>
            <div className="flex items-center h-[38px]">
              <Checkbox
                checked={rejectionDefaults.trail === 'true'}
                onChange={(checked) => {
                  const next = { ...rejectionDefaults, trail: checked ? 'true' : '' };
                  setRejectionDefaults(next);
                  commitRejection(next, fwhmUnit);
                }}
                label="Reject trailed frames"
              />
            </div>
          </div>
        </div>
        {hasAnyThreshold && (
          <button
            type="button"
            onClick={clearRejectionDefaults}
            className="mt-2 text-xs text-content-muted hover:text-content-secondary transition-colors"
          >
            Clear all defaults
          </button>
        )}
        {rejectionError && <p className="text-xs text-error mt-2">{rejectionError}</p>}
      </SettingsSection>
    </div>
  );
}
