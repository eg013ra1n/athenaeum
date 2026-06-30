import { useState, useEffect, useCallback } from 'react';
import { Save, RotateCw, CheckCircle, AlertCircle, Download, Maximize2 } from 'lucide-react';
import { api } from '../../api';
import type {
  PlateSolveConfig,
  CatalogStatusInfo,
  CatalogDownloadProgress,
} from '../../types/plate-solve';
import {
  CAMERA_PRESETS,
  pixelScaleArcsec,
  fovDeg,
  recommendTier,
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

  // FOV helper state — seeded from first camera preset
  const [focalMm, setFocalMm] = useState(600);
  const [presetIdx, setPresetIdx] = useState(0);
  const [pixelUm, setPixelUm] = useState(CAMERA_PRESETS[0].pixelUm);
  const [widthPx, setWidthPx] = useState(CAMERA_PRESETS[0].widthPx);
  const [heightPx, setHeightPx] = useState(CAMERA_PRESETS[0].heightPx);
  const [binning, setBinning] = useState(1);

  // Derived FOV values (not state — recomputed on each render from inputs)
  const scale = pixelScaleArcsec(pixelUm, focalMm, binning);
  const fov = fovDeg(scale, widthPx, heightPx);
  const sortedTiers = [...catalogs].sort((a, b) => a.density - b.density);
  const recommended = sortedTiers.length > 0 ? recommendTier(fov, sortedTiers) : 2000;
  const needsDownload = catalogs.length === 0 || catalogs.some((c) => !c.installed);

  useEffect(() => {
    loadConfig();
    loadCatalogStatus();
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

            {/* FOV Helper */}
            <div>
              <div className="flex items-center gap-2 text-xs font-semibold uppercase tracking-wide text-content-muted mb-3">
                <Maximize2 size={12} />
                Field of View Helper
              </div>
              <div className="grid grid-cols-2 sm:grid-cols-3 gap-3 mb-3">
                <div>
                  <label className="block text-xs font-medium text-content-secondary mb-1">
                    Focal length (mm)
                  </label>
                  <input
                    type="number"
                    min={50}
                    max={10000}
                    step={10}
                    value={focalMm}
                    onChange={(e) => setFocalMm(parseFloat(e.target.value) || 0)}
                    className="w-full bg-surface-hover border border-border rounded px-2 py-1.5 text-xs focus:outline-none focus:ring-1 focus:ring-accent"
                  />
                </div>

                <div className="sm:col-span-2">
                  <label className="block text-xs font-medium text-content-secondary mb-1">
                    Camera preset
                  </label>
                  <select
                    value={presetIdx}
                    onChange={(e) => {
                      const idx = parseInt(e.target.value, 10);
                      setPresetIdx(idx);
                      if (idx >= 0 && idx < CAMERA_PRESETS.length) {
                        const p = CAMERA_PRESETS[idx];
                        setPixelUm(p.pixelUm);
                        setWidthPx(p.widthPx);
                        setHeightPx(p.heightPx);
                      }
                    }}
                    className="w-full bg-surface-hover border border-border rounded px-2 py-1.5 text-xs focus:outline-none focus:ring-1 focus:ring-accent"
                  >
                    {CAMERA_PRESETS.map((p, i) => (
                      <option key={i} value={i}>{p.label}</option>
                    ))}
                    <option value={-1}>Custom</option>
                  </select>
                </div>

                <div>
                  <label className="block text-xs font-medium text-content-secondary mb-1">
                    Pixel size (&micro;m)
                  </label>
                  <input
                    type="number"
                    min={1}
                    max={20}
                    step={0.01}
                    value={pixelUm}
                    onChange={(e) => {
                      setPixelUm(parseFloat(e.target.value) || 0);
                      setPresetIdx(-1);
                    }}
                    className="w-full bg-surface-hover border border-border rounded px-2 py-1.5 text-xs focus:outline-none focus:ring-1 focus:ring-accent"
                  />
                </div>

                <div>
                  <label className="block text-xs font-medium text-content-secondary mb-1">
                    Width (px)
                  </label>
                  <input
                    type="number"
                    min={100}
                    max={20000}
                    step={1}
                    value={widthPx}
                    onChange={(e) => {
                      setWidthPx(parseInt(e.target.value, 10) || 0);
                      setPresetIdx(-1);
                    }}
                    className="w-full bg-surface-hover border border-border rounded px-2 py-1.5 text-xs focus:outline-none focus:ring-1 focus:ring-accent"
                  />
                </div>

                <div>
                  <label className="block text-xs font-medium text-content-secondary mb-1">
                    Height (px)
                  </label>
                  <input
                    type="number"
                    min={100}
                    max={20000}
                    step={1}
                    value={heightPx}
                    onChange={(e) => {
                      setHeightPx(parseInt(e.target.value, 10) || 0);
                      setPresetIdx(-1);
                    }}
                    className="w-full bg-surface-hover border border-border rounded px-2 py-1.5 text-xs focus:outline-none focus:ring-1 focus:ring-accent"
                  />
                </div>

                <div>
                  <label className="block text-xs font-medium text-content-secondary mb-1">
                    Binning
                  </label>
                  <select
                    value={binning}
                    onChange={(e) => setBinning(parseInt(e.target.value, 10))}
                    className="w-full bg-surface-hover border border-border rounded px-2 py-1.5 text-xs focus:outline-none focus:ring-1 focus:ring-accent"
                  >
                    <option value={1}>1&times;1</option>
                    <option value={2}>2&times;2</option>
                    <option value={3}>3&times;3</option>
                    <option value={4}>4&times;4</option>
                  </select>
                </div>
              </div>

              {focalMm > 0 && pixelUm > 0 && (
                <div className="flex flex-wrap gap-x-5 gap-y-1 text-xs bg-surface-hover rounded-md px-3 py-2">
                  <span className="text-content-muted">
                    Scale:{' '}
                    <span className="text-content font-medium">{scale.toFixed(2)}&Prime;</span>
                    /px
                  </span>
                  <span className="text-content-muted">
                    FOV:{' '}
                    <span className="text-content font-medium">{fov.toFixed(2)}&deg;</span>
                  </span>
                  <span className="text-content-muted">
                    Recommended tier:{' '}
                    <span className="text-content font-medium">
                      {recommended.toLocaleString()} stars/deg&sup2;
                    </span>
                  </span>
                </div>
              )}
            </div>

            {/* Per-tier table */}
            {sortedTiers.length > 0 ? (
              <div>
                <div className="text-xs font-semibold uppercase tracking-wide text-content-muted mb-2">
                  Catalog Tiers
                </div>
                <div className="overflow-x-auto">
                  <table className="w-full text-xs">
                    <thead>
                      <tr className="text-content-muted border-b border-border">
                        <th className="text-left pb-1.5 pr-4 font-medium">Tier</th>
                        <th className="text-left pb-1.5 pr-4 font-medium">Status</th>
                        <th className="text-right pb-1.5 pr-4 font-medium">Stars</th>
                        <th className="text-right pb-1.5 font-medium">Size</th>
                      </tr>
                    </thead>
                    <tbody>
                      {sortedTiers.map((tier) => {
                        const isRecommended = tier.density === recommended;
                        return (
                          <tr
                            key={tier.density}
                            className={`border-b border-border/40 ${isRecommended ? 'bg-accent/5' : ''}`}
                          >
                            <td className="py-2 pr-4">
                              <span
                                className={`font-medium ${isRecommended ? 'text-accent' : 'text-content'}`}
                              >
                                {tier.density.toLocaleString()}{' '}
                                <span className="font-normal text-content-muted">
                                  stars/deg&sup2;
                                </span>
                              </span>
                              {isRecommended && (
                                <span className="ml-2 text-[10px] font-semibold text-accent uppercase tracking-wide">
                                  recommended
                                </span>
                              )}
                              <div className="text-content-muted mt-0.5">
                                min FOV {tier.min_fov_deg.toFixed(1)}&deg;
                              </div>
                            </td>
                            <td className="py-2 pr-4">
                              {tier.installed ? (
                                <span className="inline-flex items-center gap-1 text-success">
                                  <CheckCircle size={12} />
                                  Installed
                                </span>
                              ) : (
                                <span className="text-content-muted">Needed</span>
                              )}
                            </td>
                            <td className="py-2 pr-4 text-right text-content-muted font-mono">
                              {tier.star_count_approx > 0
                                ? formatStarCount(tier.star_count_approx)
                                : '—'}
                            </td>
                            <td className="py-2 text-right text-content-muted font-mono">
                              {formatSize(tier.size_bytes)}
                            </td>
                          </tr>
                        );
                      })}
                    </tbody>
                  </table>
                </div>
              </div>
            ) : (
              <p className="text-xs text-content-muted">
                Catalog status unavailable. You can still attempt a download below.
              </p>
            )}

            {/* Download controls */}
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
            ) : needsDownload ? (
              <div className="flex items-start gap-3">
                <button
                  onClick={() => downloadStarCatalog(recommended)}
                  disabled={downloading}
                  className="flex items-center gap-2 px-3 py-1.5 bg-accent hover:bg-accent-hover disabled:opacity-50 rounded-lg text-xs font-medium transition-colors text-white flex-shrink-0"
                >
                  <Download size={13} />
                  Download needed set
                </button>
                <p className="text-xs text-content-muted leading-relaxed pt-0.5">
                  Downloads all tiers up to{' '}
                  {recommended.toLocaleString()} stars/deg&sup2; for your rig&apos;s FOV.
                  Resumable — safe to leave running.
                </p>
              </div>
            ) : (
              <p className="text-xs text-success flex items-center gap-1.5">
                <CheckCircle size={13} />
                All catalog tiers are installed.
              </p>
            )}

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
