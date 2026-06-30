import { useState, useEffect, useCallback } from 'react';
import { Save, RotateCw, CheckCircle, AlertCircle, Download, Info } from 'lucide-react';
import { api } from '../../api';
import type {
  PlateSolveConfig,
  CatalogStatusInfo,
  CatalogDownloadProgress,
  FovSummary,
} from '../../types/plate-solve';
import {
  recommendTier,
  TIER_POLICY,
} from './cameraPresets';

// Fallback default shown while loading, matching backend defaults. The full
// config object is replaced by `get_plate_solve_config` on mount; saves spread
// the loaded object, so backend-only fields (blind-gate thresholds, bright
// cache path) round-trip untouched even though they're not typed here.
const DEFAULT_CONFIG: PlateSolveConfig = {
  sip_order: 3,
  autofind_tolerance_deg: 0.5,
  base_verification_tolerance_arcsec: 8.0,
};

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

export function PlateSolveSettingsPanel() {
  const [config, setConfig] = useState<PlateSolveConfig>(DEFAULT_CONFIG);
  const [loading, setLoading] = useState(true);
  const [saving, setSaving] = useState(false);
  const [saved, setSaved] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Catalog state
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
  // The density→FOV mapping is fixed policy (TIER_POLICY), so the recommendation
  // and the tier list always work — even before the catalog server/manifest is
  // reachable. Live install state (and authoritative byte sizes) are merged in
  // from get_catalog_status when available; tier_status reports installed tiers
  // via discover_layers even with no manifest, so "Installed" still shows offline.
  const hasRecommendation = fovSummary?.min_fov_deg != null;
  const recommended = hasRecommendation
    ? recommendTier(fovSummary!.min_fov_deg!, TIER_POLICY)
    : 2000;
  const tierRows = TIER_POLICY.map((p) => {
    const live = catalogs.find((c) => c.density === p.density);
    return {
      density: p.density,
      min_fov_deg: p.min_fov_deg,
      installed: live?.installed ?? false,
      star_count_approx: live?.star_count_approx ?? 0,
      size_bytes: live?.size_bytes,
    };
  });
  // True iff any tier at/below the recommended depth is not yet installed.
  const needsDownload = tierRows.some((t) => t.density <= recommended && !t.installed);

  useEffect(() => {
    loadConfig();
    loadCatalogStatus();
    loadFovSummary();
  }, []);

  // Tick once a second while a catalog download is active so the elapsed
  // timer keeps moving even during the long first wait (liveness).
  useEffect(() => {
    if (!downloading) return;
    const id = setInterval(() => setNowTs(Date.now()), 1000);
    return () => clearInterval(id);
  }, [downloading]);

  const loadConfig = async () => {
    try {
      setLoading(true);
      setError(null);
      const result = await api.invoke<PlateSolveConfig>('get_plate_solve_config');
      setConfig(result);
    } catch (err) {
      setError(String(err));
      console.error('Failed to load plate solve config:', err);
    } finally {
      setLoading(false);
    }
  };

  const handleSave = useCallback(async () => {
    try {
      setSaving(true);
      setError(null);
      setSaved(false);
      await api.invoke('set_plate_solve_config', { config });
      setSaved(true);
      setTimeout(() => setSaved(false), 3000);
    } catch (err) {
      setError(String(err));
      console.error('Failed to save plate solve config:', err);
    } finally {
      setSaving(false);
    }
  }, [config]);

  const handleReset = useCallback(async () => {
    try {
      setError(null);
      const result = await api.invoke<PlateSolveConfig>('reset_plate_solve_config');
      setConfig(result);
      setSaved(true);
      setTimeout(() => setSaved(false), 3000);
    } catch (err) {
      setError(String(err));
      console.error('Failed to reset plate solve config:', err);
    }
  }, []);

  const loadCatalogStatus = async () => {
    try {
      setCatalogsLoading(true);
      const result = await api.invoke<CatalogStatusInfo[]>('get_catalog_status');
      setCatalogs(result);
    } catch (err) {
      console.error('Failed to load catalog status:', err);
      // On error fall back to an empty list — the UI shows the download button.
      setCatalogs([]);
    } finally {
      setCatalogsLoading(false);
    }
  };

  const loadFovSummary = async () => {
    try {
      const s = await api.invoke<FovSummary>('get_frame_fov_summary');
      setFovSummary(s);
    } catch (err) {
      console.error('[PlateSolveSettingsPanel] Failed to load FOV summary:', err);
      setFovSummary(null);
    }
  };

  // Download catalog tiers up to targetDensity. Emits the shared
  // `catalog-download-progress` event; the invoke resolves when the whole
  // command finishes (already-present tiers are skipped).
  const downloadStarCatalog = useCallback(async (targetDensity: number) => {
    setDownloading(true);
    setDownloadError(null);
    setDownloadProgress(null);
    setDownloadStartedAt(Date.now());
    setNowTs(Date.now());

    let resolvedViaEvent = false;
    let unlisten: (() => void) | null = null;
    try {
      unlisten = await api.listen<CatalogDownloadProgress>('catalog-download-progress', (payload) => {
        setDownloadProgress(payload);
        if (payload.phase === 'complete') {
          resolvedViaEvent = true;
          setDownloading(false);
          setDownloadProgress(null);
          setDownloadStartedAt(null);
          unlisten?.();
          loadCatalogStatus();
        } else if (payload.phase === 'error') {
          resolvedViaEvent = true;
          setDownloading(false);
          setDownloadProgress(null);
          setDownloadStartedAt(null);
          setDownloadError('Download failed. Please check your connection and try again.');
          unlisten?.();
        }
      });
      await api.invoke('download_catalog_layers', { targetDensity });
      if (!resolvedViaEvent) {
        setDownloading(false);
        setDownloadProgress(null);
        setDownloadStartedAt(null);
        unlisten?.();
        loadCatalogStatus();
      }
    } catch (err) {
      console.error('Failed to start star catalog download:', err);
      setDownloadError(String(err));
      setDownloading(false);
      setDownloadProgress(null);
      setDownloadStartedAt(null);
      if (!resolvedViaEvent) {
        unlisten?.();
      }
    }
  }, []);

  const setField = <K extends keyof PlateSolveConfig>(key: K, value: PlateSolveConfig[K]) => {
    setConfig((prev) => ({ ...prev, [key]: value }));
  };

  if (loading) {
    return (
      <div className="flex items-center justify-center py-12 text-content-muted">
        <div className="animate-spin rounded-full h-8 w-8 border-b-2 border-accent mr-3" />
        Loading plate solve configuration...
      </div>
    );
  }

  return (
    <div className="space-y-6">
      {/* Error banner */}
      {error && (
        <div className="p-4 bg-error-muted border border-error/50 rounded-lg flex items-start gap-3">
          <AlertCircle className="text-error flex-shrink-0 mt-0.5" size={20} />
          <div>
            <p className="font-medium text-error">Error</p>
            <p className="text-sm text-error/80">{error}</p>
          </div>
        </div>
      )}

      {/* Success banner */}
      {saved && (
        <div className="p-4 bg-success-muted border border-success/50 rounded-lg flex items-start gap-3">
          <CheckCircle className="text-success flex-shrink-0 mt-0.5" size={20} />
          <p className="font-medium text-success">Configuration saved</p>
        </div>
      )}

      {/* Star Catalog */}
      <section>
        <h4 className="text-sm font-semibold uppercase tracking-wider text-content-muted mb-3">
          Star Catalog
        </h4>
        {catalogsLoading ? (
          <div className="flex items-center gap-2 text-sm text-content-muted py-2">
            <div className="animate-spin rounded-full h-4 w-4 border-b-2 border-accent" />
            Checking catalog status...
          </div>
        ) : (
          <div className="rounded-lg border border-border bg-surface px-4 py-4 space-y-5">

            {/* Auto recommendation banner — driven by scanned light-frame FOV data */}
            {fovSummary && fovSummary.computable_count > 0 ? (
              <div className="flex items-center gap-3 px-3 py-2.5 bg-accent/5 border border-accent/20 rounded-lg">
                <div className="flex-1 min-w-0 flex items-baseline gap-1 text-xs text-content-secondary">
                  {/* Lead-in truncates (long INSTRUME) so the whole banner stays one line… */}
                  <span className="min-w-0 truncate">
                    From your{' '}
                    <span className="font-medium text-content">{fovSummary.computable_count}</span>{' '}
                    light frame{fovSummary.computable_count === 1 ? '' : 's'} — narrowest field{' '}
                    <span className="font-medium text-content">
                      {fovSummary.min_fov_deg!.toFixed(2)}&deg;
                    </span>
                    {fovSummary.narrowest_instrume ? ` (${fovSummary.narrowest_instrume})` : ''}
                  </span>
                  {/* …while the recommendation itself is always fully shown. */}
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
                    title="Download the recommended tier set (every tier up to the recommended density)"
                    className="flex items-center gap-1.5 px-2.5 py-1.5 bg-accent hover:bg-accent-hover disabled:opacity-50 rounded text-xs font-medium transition-colors text-white flex-shrink-0"
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

            {/* Per-tier table — always shown (built from the fixed tier policy;
                live install state + byte sizes merged in from get_catalog_status). */}
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
                                title={`Downloads every tier up to ${tier.density.toLocaleString()} stars/deg² (additive — includes lower tiers)`}
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

            {/* Download status — progress bar or all-good confirmation */}
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
      </section>

      {/* Solver Parameters */}
      <section>
        <h4 className="text-sm font-semibold uppercase tracking-wider text-content-muted mb-3">
          Solver Parameters
        </h4>
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">
          {/* Base Verification Tolerance */}
          <div>
            <label className="block text-sm font-medium text-content-secondary mb-1">
              Verification Tolerance (arcsec)
            </label>
            <input
              type="number"
              min={2}
              max={30}
              step={0.5}
              value={config.base_verification_tolerance_arcsec ?? 8.0}
              onChange={(e) =>
                setField('base_verification_tolerance_arcsec', parseFloat(e.target.value) || 0)
              }
              className="w-full bg-surface-hover border border-border rounded px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-accent"
            />
            <p className="mt-1 text-xs text-content-muted">
              Base angular tolerance for the persisted-solve confidence gate. The
              actual pixel tolerance adapts per frame: <code>base / pixel_scale</code>,
              clamped to [4, 20] px. Default 8.0&Prime;.
            </p>
          </div>

          {/* SIP Order */}
          <div>
            <label className="block text-sm font-medium text-content-secondary mb-1">
              SIP Distortion Order
            </label>
            <input
              type="number"
              min={2}
              max={5}
              value={config.sip_order}
              onChange={(e) => setField('sip_order', parseInt(e.target.value, 10) || 2)}
              className="w-full bg-surface-hover border border-border rounded px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-accent"
            />
            <p className="mt-1 text-xs text-content-muted">
              Polynomial order for the SIP distortion fit passed to the solver
              (2&ndash;5). Higher orders fit more distortion but need more matched stars.
            </p>
          </div>

          {/* Autofind Tolerance */}
          <div>
            <label className="block text-sm font-medium text-content-secondary mb-1">
              Autofind Object Tolerance (&deg;)
            </label>
            <input
              type="number"
              min={0.05}
              max={5}
              step={0.05}
              value={config.autofind_tolerance_deg}
              onChange={(e) => setField('autofind_tolerance_deg', parseFloat(e.target.value) || 0)}
              className="w-full bg-surface-hover border border-border rounded px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-accent"
            />
            <p className="mt-1 text-xs text-content-muted">
              Maximum great-circle distance (in degrees) between a frame&apos;s
              RA/Dec and a named DSO for the &quot;Autofind Object&quot; batch
              action to accept the match as a label. Tighter values reject
              more frames; looser values risk labelling unrelated fields with
              distant objects. Default 0.5&deg;.
            </p>
          </div>
        </div>
      </section>

      {/* Action buttons */}
      <div className="flex items-center gap-3 pt-2">
        <button
          onClick={handleSave}
          disabled={saving}
          className="flex items-center gap-2 px-4 py-2 bg-accent hover:bg-accent-hover disabled:opacity-50 rounded-lg text-sm font-medium transition-colors text-white"
        >
          <Save size={16} />
          {saving ? 'Saving...' : 'Save'}
        </button>
        <button
          onClick={handleReset}
          disabled={saving}
          className="flex items-center gap-2 px-4 py-2 border border-border hover:bg-surface-hover disabled:opacity-50 rounded-lg text-sm font-medium transition-colors"
        >
          <RotateCw size={16} />
          Reset to Defaults
        </button>
      </div>
    </div>
  );
}
