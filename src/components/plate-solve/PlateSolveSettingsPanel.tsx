import { useState, useEffect, useCallback } from 'react';
import { Save, RotateCw, CheckCircle, AlertCircle, Package, Download } from 'lucide-react';
import { api } from '../../api';
import type {
  PlateSolveConfig,
  CatalogStatusInfo,
  CatalogDownloadProgress,
} from '../../types/plate-solve';

// Fallback default shown while loading, matching backend defaults. The full
// config object is replaced by `get_plate_solve_config` on mount; saves spread
// the loaded object, so backend-only fields (blind-gate thresholds, bright
// cache path) round-trip untouched even though they're not typed here.
const DEFAULT_CONFIG: PlateSolveConfig = {
  sip_order: 3,
  autofind_tolerance_deg: 0.5,
  base_verification_tolerance_arcsec: 8.0,
};

// Fallback metadata shown before `get_catalog_status` resolves (or if it fails).
const STAR_CATALOG_FALLBACK: CatalogStatusInfo = {
  name: 'Gaia DR3 (stars.smac)',
  installed: false,
  epoch: 2016,
  star_count_approx: 0,
  mag_limit: 19.0,
};

function formatStarCount(n: number): string {
  if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(1)}B`;
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(0)}K`;
  return String(n);
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

  // Download the prebuilt solver star catalog (`stars.smac`). Emits the shared
  // `catalog-download-progress` event; the invoke resolves when the whole
  // command finishes (the catalog may already be present → no progress).
  const downloadStarCatalog = useCallback(async () => {
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
      await api.invoke('download_gaia_dr3_prebuilt_catalog');
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

  const catalog = catalogs[0] ?? STAR_CATALOG_FALLBACK;
  const catalogInstalled = catalog.installed;

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
          <div className="rounded-lg border border-border bg-surface px-4 py-3 space-y-3">
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-3">
                <Package size={18} className={catalogInstalled ? 'text-accent' : 'text-content-muted'} />
                <div>
                  <p className="font-medium text-sm">{catalog.name}</p>
                  <p className="text-xs text-content-muted">
                    Epoch {catalog.epoch}
                    {catalog.star_count_approx > 0 && (
                      <> &middot; {formatStarCount(catalog.star_count_approx)} stars</>
                    )}{' '}
                    &middot; mag &le;{catalog.mag_limit}
                  </p>
                </div>
              </div>
              {catalogInstalled ? (
                <span className="text-xs font-semibold px-2 py-0.5 rounded-full bg-success/20 text-success border border-success/30">
                  Installed
                </span>
              ) : (
                <span className="text-xs font-semibold px-2 py-0.5 rounded-full bg-surface-hover text-content-muted border border-border">
                  Not installed
                </span>
              )}
            </div>

            {/* Download controls — only when the catalog is not installed */}
            {!catalogInstalled && (
              <div className="space-y-2">
                {downloadError && <p className="text-xs text-red-400">{downloadError}</p>}
                {downloading ? (
                  <div className="space-y-1.5">
                    <div className="flex items-center gap-2 text-xs text-content-muted">
                      <div className="animate-spin rounded-full h-3 w-3 border-b-2 border-violet-500 flex-shrink-0" />
                      <span>
                        {!downloadProgress
                          ? 'Starting — connecting to the catalog server…'
                          : downloadProgress.phase === 'downloading'
                            ? `Downloading archive · ${(downloadProgress.current / 1048576).toFixed(0)} / ${(downloadProgress.total / 1048576).toFixed(0)} MB`
                            : downloadProgress.phase === 'verifying'
                              ? 'Verifying download integrity…'
                              : downloadProgress.phase === 'extracting'
                                ? 'Extracting star catalog…'
                                : downloadProgress.phase === 'complete'
                                  ? 'Finishing…'
                                  : 'Working…'}
                      </span>
                    </div>
                    <div className="w-full h-1.5 bg-surface-hover rounded-full overflow-hidden">
                      <div
                        className="h-full bg-violet-500 rounded-full transition-all duration-300"
                        style={{ width: downloadProgress ? `${downloadProgress.percent}%` : '4%' }}
                      />
                    </div>
                    <p className="text-xs text-content-muted flex justify-between">
                      <span>
                        {downloadStartedAt != null
                          ? `elapsed ${formatElapsed(nowTs - downloadStartedAt)} · resumable — safe to leave running`
                          : 'resumable — safe to leave running'}
                      </span>
                      {downloadProgress && <span>{downloadProgress.percent.toFixed(0)}%</span>}
                    </p>
                  </div>
                ) : (
                  <div className="flex items-start gap-3">
                    <button
                      onClick={downloadStarCatalog}
                      disabled={downloading}
                      className="flex items-center gap-2 px-3 py-1.5 bg-violet-600 hover:bg-violet-700 disabled:opacity-50 rounded-lg text-xs font-medium transition-colors text-white"
                    >
                      <Download size={13} />
                      Download Star Catalog
                    </button>
                    <p className="text-xs text-content-muted leading-relaxed pt-0.5">
                      Single prebuilt download (resumable). Required before any frame can be
                      plate-solved.
                    </p>
                  </div>
                )}
              </div>
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
              clamped to [4, 20] px. Default 8.0".
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
              Autofind Object Tolerance (°)
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
              Maximum great-circle distance (in degrees) between a frame's
              RA/Dec and a named DSO for the &quot;Autofind Object&quot; batch
              action to accept the match as a label. Tighter values reject
              more frames; looser values risk labelling unrelated fields with
              distant objects. Default 0.5°.
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
