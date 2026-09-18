// Settings redesign (spec 2026-09-18 §4/§5/§6, plan Task D1): the
// `PlateSolveConfig` fields commit through `useAutosaveDocument` — no Save
// button, no "settings saved" banner. Three registered sections
// (`plateSolving.catalog/solver/inputGate`); `onResetAll` on the first
// (`plateSolving.catalog`) calls the document's whole-config `resetAll`.
// The Star Catalog section's download UI is unchanged — it's an action,
// not a setting (spec §5).
import { useState, useEffect, useCallback, useRef, type KeyboardEvent } from 'react';
import { CheckCircle, Download, Info } from 'lucide-react';
import { api } from '../../api';
import type { PlateSolveConfig, FovSummary } from '../../types/plate-solve';
import type { CatalogStatusInfo, CatalogDownloadProgress } from '../../types/helpers';
import { buildTierRows, recommendFromFov } from './cameraPresets';
import { useAutosaveDocument } from '../../hooks/useAutosaveDocument';
import { useSettingsDefaults } from '../../settings/SettingsDefaultsContext';
import { fieldMeta } from '../../settings/registry';
import { intCodec, floatCodec, type Codec } from '../../settings/codecs';
import { SettingsSection } from '../settings/SettingsSection';
import { Checkbox } from '../settings/Checkbox';
import { ResetButton, SavedTick } from '../settings/ResetButton';

function formatStarCount(n: number): string {
  if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(1)}B`;
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(0)}K`;
  return String(n);
}

function formatSize(bytes: number): string {
  if (bytes === 0) return '—';
  if (bytes >= 1e9) return `${(bytes / 1e9).toFixed(1)} GB`;
  if (bytes >= 1e6) return `${(bytes / 1e6).toFixed(0)} MB`;
  return `${Math.round(bytes / 1024)} KB`;
}

function formatElapsed(ms: number): string {
  const s = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(s / 3600);
  const m = Math.floor((s % 3600) / 60);
  const sec = s % 60;
  if (h > 0) return `${h}h ${m}m`;
  if (m > 0) return `${m}m ${String(sec).padStart(2, '0')}s`;
  return `${sec}s`;
}

function getDownloadStatusText(progress: CatalogDownloadProgress | null): string {
  if (!progress) return 'Starting — connecting to the catalog server…';
  const tierLabel =
    progress.nTiers > 1
      ? `Tier ${progress.tierIndex + 1}/${progress.nTiers} (${progress.tierDensity.toLocaleString()} stars/deg²) — `
      : '';
  switch (progress.phase) {
    case 'tier':
      return `${tierLabel}Preparing tier…`;
    case 'downloading':
      return `${tierLabel}Downloading · ${(progress.current / 1048576).toFixed(0)} / ${(progress.total / 1048576).toFixed(0)} MB`;
    case 'verifying':
      return `${tierLabel}Verifying integrity…`;
    case 'extracting':
      return `${tierLabel}Extracting…`;
    case 'complete':
      return 'Finishing…';
    case 'error':
      return `${tierLabel}Download failed.`;
    default:
      return `${tierLabel}Working…`;
  }
}

/** Draft/blur/Enter/Escape discipline for one numeric input bound to a path
 *  of the `useAutosaveDocument` doc — see `AnalysisSettingsPanel.tsx`'s own
 *  copy of this helper for the full rationale (kept file-local in both
 *  panels rather than shared, per this task's file scope). */
function useNumberDraft(value: number, codec: Codec<number>, onCommit: (n: number) => void) {
  const [draft, setDraft] = useState(String(value));
  const [error, setError] = useState<string | null>(null);

  useEffect(() => {
    setDraft(String(value));
    setError(null);
  }, [value]);

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
  onCommit: (n: number) => void;
  isDefault: boolean;
  defaultValue: number;
  onReset: () => void;
  disabled?: boolean;
  help?: string;
}

function DocNumberField({
  section,
  field,
  value,
  codec,
  min,
  max,
  step,
  onCommit,
  isDefault,
  defaultValue,
  onReset,
  disabled,
  help,
}: DocNumberFieldProps) {
  const meta = fieldMeta(section, field);
  const { draft, setDraft, error, commit, handleKeyDown } = useNumberDraft(value, codec, onCommit);

  return (
    <div>
      <div className="flex items-center justify-between gap-2 mb-1">
        <label className="block text-sm font-medium text-content-secondary">{meta.label}</label>
        <ResetButton visible={!isDefault} defaultLabel={String(defaultValue)} onReset={onReset} disabled={disabled} />
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
        className="w-full bg-surface-hover border border-border rounded px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-accent disabled:opacity-50 disabled:cursor-not-allowed"
      />
      <p className="mt-1 text-xs text-content-muted">{help ?? meta.help}</p>
      {error && <p className="text-xs text-error mt-1">{error}</p>}
    </div>
  );
}

export function PlateSolveSettingsPanel() {
  const { defaults } = useSettingsDefaults();

  const { doc: config, patch, error: docError, savedAt, isDefault, resetField, resetAll } = useAutosaveDocument<PlateSolveConfig>({
    load: () => api.invoke<PlateSolveConfig>('get_plate_solve_config'),
    save: (c) => api.invoke('set_plate_solve_config', { config: c }),
    resetAll: async () => {
      await api.invoke('reset_plate_solve_config');
    },
    defaults: defaults?.plateSolve ?? null,
    label: 'Plate Solving settings',
  });

  // Catalog state — unchanged from before the redesign; independent of the
  // typed-config document above.
  const [catalogs, setCatalogs] = useState<CatalogStatusInfo[]>([]);
  const [catalogsLoading, setCatalogsLoading] = useState(true);
  const [downloading, setDownloading] = useState(false);
  const [downloadProgress, setDownloadProgress] = useState<CatalogDownloadProgress | null>(null);
  const [downloadError, setDownloadError] = useState<string | null>(null);
  const [downloadStartedAt, setDownloadStartedAt] = useState<number | null>(null);
  const [nowTs, setNowTs] = useState<number>(() => Date.now());

  // FOV summary from scanned light frames — drives auto tier recommendation.
  const [fovSummary, setFovSummary] = useState<FovSummary | null>(null);

  // Derived values (not state — recomputed on each render from inputs).
  const recommendation = recommendFromFov(fovSummary?.min_fov_deg, catalogs);
  const hasRecommendation = recommendation.isRecommendation;
  const recommended = recommendation.density;
  const tierRows = buildTierRows(catalogs);
  const needsDownload = tierRows.some((t) => t.density <= recommended && !t.installed);

  const loadCatalogStatus = useCallback(async () => {
    try {
      setCatalogsLoading(true);
      const result = await api.invoke<CatalogStatusInfo[]>('get_catalog_status');
      setCatalogs(result);
    } catch (err) {
      console.error('Failed to load catalog status:', err);
      setCatalogs([]);
    } finally {
      setCatalogsLoading(false);
    }
  }, []);

  const loadFovSummary = useCallback(async () => {
    try {
      const s = await api.invoke<FovSummary>('get_frame_fov_summary');
      setFovSummary(s);
    } catch (err) {
      console.error('[PlateSolveSettingsPanel] Failed to load FOV summary:', err);
      setFovSummary(null);
    }
  }, []);

  useEffect(() => {
    void loadCatalogStatus();
    void loadFovSummary();
  }, [loadCatalogStatus, loadFovSummary]);

  // Tick once a second while a catalog download is active so the elapsed
  // timer keeps moving even during the long first wait (liveness).
  useEffect(() => {
    if (!downloading) return;
    const id = setInterval(() => setNowTs(Date.now()), 1000);
    return () => clearInterval(id);
  }, [downloading]);

  // Reflect ANY catalog download for the panel's whole lifetime — including one
  // kicked off elsewhere. StrictMode-safe cancelled-flag form (see CLAUDE.md).
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<CatalogDownloadProgress>('catalog-download-progress', (payload) => {
        if (cancelled) return;
        if (payload.phase === 'complete') {
          setDownloading(false);
          setDownloadProgress(null);
          setDownloadStartedAt(null);
          void loadCatalogStatus();
        } else if (payload.phase === 'error') {
          setDownloading(false);
          setDownloadProgress(null);
          setDownloadStartedAt(null);
          setDownloadError('Download failed. Please check your connection and try again.');
        } else {
          setDownloading(true);
          setDownloadError(null);
          setDownloadProgress(payload);
          setDownloadStartedAt((prev) => prev ?? Date.now());
        }
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[PlateSolveSettingsPanel] catalog-download listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [loadCatalogStatus]);

  const downloadStarCatalog = useCallback(async (targetDensity: number) => {
    setDownloading(true);
    setDownloadError(null);
    setDownloadProgress(null);
    setDownloadStartedAt(Date.now());
    setNowTs(Date.now());
    try {
      await api.invoke('download_catalog_layers', { targetDensity });
      setDownloading(false);
      setDownloadProgress(null);
      setDownloadStartedAt(null);
      void loadCatalogStatus();
    } catch (err) {
      console.error('Failed to start star catalog download:', err);
      setDownloadError(String(err));
      setDownloading(false);
      setDownloadProgress(null);
      setDownloadStartedAt(null);
    }
  }, [loadCatalogStatus]);

  if (!config) {
    return (
      <div className="flex items-center justify-center py-12 text-content-muted">
        <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-accent mr-3" />
        Loading plate solve configuration...
      </div>
    );
  }

  return (
    <div className="space-y-6">
      {docError && <p className="text-xs text-error">{docError}</p>}

      {/* Star Catalog — carries the whole-panel reset (see
          `AnalysisSettingsPanel.tsx`'s matching comment on why the first
          section's own registry title, not the whole config, is what
          `SettingsSection`'s confirm dialog names). */}
      <SettingsSection id="plateSolving.catalog" onResetAll={resetAll} actions={<SavedTick savedAt={savedAt} />}>
        {catalogsLoading ? (
          <div className="flex items-center gap-2 text-sm text-content-muted py-2">
            <div className="animate-spin rounded-full h-4 w-4 border-b-2 border-accent" />
            Checking catalog status...
          </div>
        ) : (
          <div className="rounded-lg border border-border bg-surface px-4 py-4 space-y-5">
            {fovSummary && fovSummary.computable_count > 0 ? (
              <div className="flex items-center gap-3 px-3 py-2.5 bg-accent/5 border border-accent/20 rounded-lg">
                <div className="flex-1 min-w-0 flex items-baseline gap-1 text-xs text-content-secondary">
                  <span className="min-w-0 truncate">
                    From your{' '}
                    <span className="font-medium text-content">{fovSummary.computable_count}</span>{' '}
                    light frame{fovSummary.computable_count === 1 ? '' : 's'} — narrowest field{' '}
                    <span className="font-medium text-content">
                      {fovSummary.min_fov_deg!.toFixed(2)}&deg;
                    </span>
                    {fovSummary.narrowest_instrume ? ` (${fovSummary.narrowest_instrume})` : ''}
                  </span>
                  <span className="flex-shrink-0 whitespace-nowrap">
                    &rarr; recommended:{' '}
                    <span className="font-medium text-content">
                      {recommended.toLocaleString()} stars/deg&sup2;
                    </span>
                  </span>
                </div>
                {needsDownload ? (
                  <button
                    onClick={() => downloadStarCatalog(recommended)}
                    disabled={downloading}
                    title="Downloads this tier and every lower one not yet installed"
                    className="flex items-center gap-1.5 px-2.5 py-1.5 bg-accent hover:bg-accent-hover disabled:opacity-50 rounded text-xs font-medium transition-colors text-surface flex-shrink-0"
                  >
                    <Download size={12} />
                    Download
                  </button>
                ) : (
                  <span className="inline-flex items-center gap-1.5 text-xs text-success flex-shrink-0">
                    <CheckCircle size={13} />
                    Installed
                  </span>
                )}
              </div>
            ) : (
              <p className="flex items-center gap-1.5 text-xs text-content-muted">
                <Info size={13} className="flex-shrink-0" />
                No frames with usable optics yet — pick a tier below.
              </p>
            )}

            <div>
              <div className="text-xs font-semibold uppercase tracking-wide text-content-muted mb-2">
                Catalog Tiers
              </div>
              <div className="overflow-x-auto">
                <table className="w-full text-xs">
                  <thead>
                    <tr className="text-content-muted border-b border-border">
                      <th className="text-left pb-1.5 pr-4 font-medium">Tier</th>
                      <th className="text-center pb-1.5 px-4 font-medium">Status</th>
                      <th className="text-right pb-1.5 pr-4 font-medium">Stars</th>
                      <th className="text-right pb-1.5 font-medium">Size</th>
                    </tr>
                  </thead>
                  <tbody>
                    {tierRows.map((tier) => {
                      const isRecommended = hasRecommendation && tier.density === recommended;
                      return (
                        <tr
                          key={tier.density}
                          className={`border-b border-border/40 ${isRecommended ? 'bg-accent/5' : ''}`}
                        >
                          <td className="py-2 pr-4 align-top">
                            <span
                              className={`font-medium ${isRecommended ? 'text-accent' : 'text-content'}`}
                            >
                              {tier.density.toLocaleString()}{' '}
                              <span className="font-normal text-content-muted">
                                stars/deg&sup2;
                              </span>
                            </span>
                            <span className="ml-2 text-content-muted">
                              &middot; min FOV {tier.min_fov_deg.toFixed(2)}&deg;
                            </span>
                            {isRecommended && (
                              <span className="ml-2 text-[10px] font-semibold text-accent uppercase tracking-wide">
                                recommended
                              </span>
                            )}
                          </td>
                          <td className="py-2 px-4 align-top text-center">
                            {tier.installed ? (
                              <span className="inline-flex items-center gap-1 text-success">
                                <CheckCircle size={12} />
                                Installed
                              </span>
                            ) : (
                              <button
                                onClick={() => downloadStarCatalog(tier.density)}
                                disabled={downloading}
                                title="Downloads this tier and every lower one not yet installed"
                                className="inline-flex items-center gap-1 font-medium text-accent hover:text-accent-hover disabled:opacity-50 transition-colors"
                              >
                                <Download size={11} />
                                Download
                              </button>
                            )}
                          </td>
                          <td className="py-2 pr-4 text-right text-content-muted tabular-nums align-top">
                            {tier.star_count_approx > 0
                              ? formatStarCount(tier.star_count_approx)
                              : '—'}
                          </td>
                          <td className="py-2 text-right text-content-muted tabular-nums align-top">
                            {tier.size_bytes != null ? formatSize(tier.size_bytes) : '—'}
                          </td>
                        </tr>
                      );
                    })}
                  </tbody>
                </table>
              </div>
              {!catalogsLoading && catalogs.length === 0 && (
                <p className="text-xs text-content-muted mt-2">
                  Couldn&apos;t read installed-catalog status (catalog server unreachable) — install
                  state shown as &ldquo;Needed&rdquo;; the recommendation is computed from your light
                  frames&apos; FOV.
                </p>
              )}
            </div>

            {downloadError && (
              <p className="text-xs text-error">{downloadError}</p>
            )}
            {downloading ? (
              <div className="space-y-1.5">
                <div className="flex items-center gap-2 text-xs text-content-muted">
                  <div className="animate-spin rounded-full h-3 w-3 border-b-2 border-accent flex-shrink-0" />
                  <span>{getDownloadStatusText(downloadProgress)}</span>
                </div>
                <div className="w-full h-1.5 bg-surface-hover rounded-full overflow-hidden">
                  <div
                    className="h-full bg-accent rounded-full transition-all duration-300"
                    style={{ width: downloadProgress ? `${downloadProgress.percent}%` : '4%' }}
                  />
                </div>
                <p className="text-xs text-content-muted flex justify-between">
                  <span>
                    {downloadStartedAt != null
                      ? `elapsed ${formatElapsed(nowTs - downloadStartedAt)} · resumable — safe to leave running`
                      : 'resumable — safe to leave running'}
                  </span>
                  {downloadProgress && (
                    <span>{downloadProgress.percent.toFixed(0)}%</span>
                  )}
                </p>
              </div>
            ) : !needsDownload && !downloadError ? (
              <p className="text-xs text-success flex items-center gap-1.5">
                <CheckCircle size={13} />
                Recommended catalog tiers installed and up to date.
              </p>
            ) : null}
          </div>
        )}
      </SettingsSection>

      {/* Solver Parameters */}
      <SettingsSection id="plateSolving.solver" actions={<SavedTick savedAt={savedAt} />}>
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
          <DocNumberField
            section="plateSolving.solver"
            field="verificationTolerance"
            value={config.base_verification_tolerance_arcsec}
            codec={floatCodec(2, 30)}
            min={2}
            max={30}
            step={0.5}
            onCommit={(n) => patch({ base_verification_tolerance_arcsec: n })}
            isDefault={isDefault('base_verification_tolerance_arcsec')}
            defaultValue={defaults?.plateSolve?.base_verification_tolerance_arcsec ?? config.base_verification_tolerance_arcsec}
            onReset={() => resetField('base_verification_tolerance_arcsec')}
            help="Base angular tolerance for the persisted-solve confidence gate. The actual pixel tolerance adapts per frame: base / pixel_scale, clamped to [4, 20] px. Default 8.0″."
          />
          <DocNumberField
            section="plateSolving.solver"
            field="sipOrder"
            value={config.sip_order}
            codec={intCodec(2, 5)}
            min={2}
            max={5}
            step={1}
            onCommit={(n) => patch({ sip_order: n })}
            isDefault={isDefault('sip_order')}
            defaultValue={defaults?.plateSolve?.sip_order ?? config.sip_order}
            onReset={() => resetField('sip_order')}
          />
          <DocNumberField
            section="plateSolving.solver"
            field="autofindTolerance"
            value={config.autofind_tolerance_deg}
            codec={floatCodec(0.05, 5)}
            min={0.05}
            max={5}
            step={0.05}
            onCommit={(n) => patch({ autofind_tolerance_deg: n })}
            isDefault={isDefault('autofind_tolerance_deg')}
            defaultValue={defaults?.plateSolve?.autofind_tolerance_deg ?? config.autofind_tolerance_deg}
            onReset={() => resetField('autofind_tolerance_deg')}
          />
          <DocNumberField
            section="plateSolving.solver"
            field="batchConcurrency"
            value={config.batch_concurrency}
            codec={intCodec(0, 16)}
            min={0}
            max={16}
            step={1}
            onCommit={(n) => patch({ batch_concurrency: n })}
            isDefault={isDefault('batch_concurrency')}
            defaultValue={defaults?.plateSolve?.batch_concurrency ?? config.batch_concurrency}
            onReset={() => resetField('batch_concurrency')}
          />
        </div>
      </SettingsSection>

      {/* Input Gate */}
      <SettingsSection id="plateSolving.inputGate" actions={<SavedTick savedAt={savedAt} />}>
        <div className="flex items-start justify-between gap-2">
          <Checkbox
            checked={config.input_gate_enabled}
            onChange={(checked) => patch({ input_gate_enabled: checked })}
            label={fieldMeta('plateSolving.inputGate', 'inputGateEnabled').label}
          />
          <ResetButton
            visible={!isDefault('input_gate_enabled')}
            defaultLabel={(defaults?.plateSolve?.input_gate_enabled ?? config.input_gate_enabled) ? 'On' : 'Off'}
            onReset={() => resetField('input_gate_enabled')}
          />
        </div>
        <p className="mt-2 mb-4 text-xs text-content-muted">
          A frame is refused when its own analysis reports a median star
          eccentricity of at least the first value <strong>and</strong> a trail-fit
          R&sup2; of at least the second &mdash; both, never either alone. Measured on
          real frames: ones that solve correctly sit at eccentricity
          0.62&ndash;0.72 with R&sup2; 0.30&ndash;0.57, hopeless ones at
          0.90&ndash;0.96 with 0.73&ndash;0.93. Loosening these lets more frames
          attempt a solve. Tightening them will <em>not</em> reliably catch more
          trailed frames &mdash; the analysis under-reports eccentricity on exactly
          those frames, so it rates many of them as round.
        </p>
        <div
          className={`grid grid-cols-1 gap-4 sm:grid-cols-2 ${
            config.input_gate_enabled ? '' : 'opacity-50'
          }`}
        >
          <DocNumberField
            section="plateSolving.inputGate"
            field="maxEccentricity"
            value={config.input_max_eccentricity}
            codec={floatCodec(0, 1)}
            min={0}
            max={1}
            step={0.01}
            disabled={!config.input_gate_enabled}
            onCommit={(n) => patch({ input_max_eccentricity: n })}
            isDefault={isDefault('input_max_eccentricity')}
            defaultValue={defaults?.plateSolve?.input_max_eccentricity ?? config.input_max_eccentricity}
            onReset={() => resetField('input_max_eccentricity')}
          />
          <DocNumberField
            section="plateSolving.inputGate"
            field="minTrailR2"
            value={config.input_min_trail_r2}
            codec={floatCodec(0, 1)}
            min={0}
            max={1}
            step={0.01}
            disabled={!config.input_gate_enabled}
            onCommit={(n) => patch({ input_min_trail_r2: n })}
            isDefault={isDefault('input_min_trail_r2')}
            defaultValue={defaults?.plateSolve?.input_min_trail_r2 ?? config.input_min_trail_r2}
            onReset={() => resetField('input_min_trail_r2')}
          />
        </div>
      </SettingsSection>
    </div>
  );
}
