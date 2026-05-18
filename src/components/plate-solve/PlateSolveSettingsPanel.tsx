import { useState, useEffect, useCallback } from 'react';
import { Save, RotateCw, CheckCircle, AlertCircle, Package, Download, Database } from 'lucide-react';
import { api } from '../../api';
import type {
  PlateSolveConfig,
  CatalogStatusInfo,
  CatalogDownloadProgress,
  QuadIndexStatus,
  QuadIndexProgressEvent,
} from '../../types/plate-solve';

// Fallback default shown while loading, matching backend defaults.
const DEFAULT_CONFIG: PlateSolveConfig = {
  max_image_stars: 300,
  min_matched_stars: 6,
  verification_tolerance_px: 10.0,
  index_mag_limit: 13.0,
  hash_tolerance: 0.005,
  sip_order: 3,
  use_fast_detection: true,
  autofind_tolerance_deg: 0.5,
  min_inlier_ratio: 0.10,
  retry_passes: [50, 150, 300, 600],
  base_verification_tolerance_arcsec: 8.0,
  fallback_to_blind_scale: true,
};

// Static metadata for catalogs that are not dynamically fetched.
const GAIA_CATALOG_META: CatalogStatusInfo = {
  name: 'Gaia DR3',
  installed: false,
  epoch: 2016,
  star_count_approx: 300_000_000,
  mag_limit: 16.0,
};

const TYCHO2_FALLBACK_META: CatalogStatusInfo = {
  name: 'Tycho-2',
  installed: false,
  epoch: 2000,
  star_count_approx: 2539913,
  mag_limit: 12.5,
};

function formatStarCount(n: number): string {
  if (n >= 1_000_000_000) return `${(n / 1_000_000_000).toFixed(1)}B`;
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(0)}K`;
  return String(n);
}

function formatBytes(bytes: number): string {
  if (bytes >= 1_073_741_824) return `${(bytes / 1_073_741_824).toFixed(1)} GB`;
  if (bytes >= 1_048_576) return `${(bytes / 1_048_576).toFixed(1)} MB`;
  if (bytes >= 1_024) return `${(bytes / 1_024).toFixed(0)} KB`;
  return `${bytes} B`;
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
  const [downloadKind, setDownloadKind] = useState<'tycho2' | 'gaia' | null>(null);
  const [downloadProgress, setDownloadProgress] = useState<CatalogDownloadProgress | null>(null);
  const [downloadError, setDownloadError] = useState<string | null>(null);

  // Quad index state
  const [quadIndexStatus, setQuadIndexStatus] = useState<QuadIndexStatus | null>(null);
  const [quadIndexLoading, setQuadIndexLoading] = useState(true);
  const [buildingIndex, setBuildingIndex] = useState(false);
  const [indexBuildProgress, setIndexBuildProgress] = useState<QuadIndexProgressEvent | null>(null);
  const [indexBuildError, setIndexBuildError] = useState<string | null>(null);

  useEffect(() => {
    loadConfig();
    loadCatalogStatus();
    loadQuadIndexStatus();
  }, []);

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
      // On error fall back to an empty list — the UI will show the download button
      setCatalogs([]);
    } finally {
      setCatalogsLoading(false);
    }
  };

  const loadQuadIndexStatus = async () => {
    try {
      setQuadIndexLoading(true);
      const result = await api.invoke<QuadIndexStatus>('get_quad_index_status');
      setQuadIndexStatus(result);
    } catch (err) {
      console.error('Failed to load quad index status:', err);
      setQuadIndexStatus(null);
    } finally {
      setQuadIndexLoading(false);
    }
  };

  // Shared download driver for both catalogs — identical flow, only the
  // backend command differs. Both emit the same `catalog-download-progress`
  // event; `downloadKind` scopes the progress UI to the active card.
  const runCatalogDownload = useCallback(
    async (kind: 'tycho2' | 'gaia') => {
      setDownloadKind(kind);
      setDownloading(true);
      setDownloadError(null);
      setDownloadProgress(null);

      // Track whether the operation ended via a phase event so we know
      // whether to clean up the listener ourselves.
      let resolvedViaEvent = false;
      let unlisten: (() => void) | null = null;
      try {
        unlisten = await api.listen<CatalogDownloadProgress>('catalog-download-progress', (payload) => {
          setDownloadProgress(payload);
          if (payload.phase === 'complete') {
            resolvedViaEvent = true;
            setDownloading(false);
            setDownloadProgress(null);
            setDownloadKind(null);
            unlisten?.();
            loadCatalogStatus();
          } else if (payload.phase === 'error') {
            resolvedViaEvent = true;
            setDownloading(false);
            setDownloadProgress(null);
            setDownloadKind(null);
            setDownloadError('Download failed. Please check your connection and try again.');
            unlisten?.();
          }
        });
        await api.invoke(kind === 'tycho2' ? 'download_tycho2_catalog' : 'download_gaia_dr3_catalog');
      } catch (err) {
        console.error(`Failed to start ${kind} download:`, err);
        setDownloadError(String(err));
        setDownloading(false);
        setDownloadProgress(null);
        setDownloadKind(null);
        if (!resolvedViaEvent) {
          unlisten?.();
        }
      }
    },
    [],
  );

  const handleDownloadTycho2 = useCallback(
    () => runCatalogDownload('tycho2'),
    [runCatalogDownload],
  );
  const handleDownloadGaia = useCallback(
    () => runCatalogDownload('gaia'),
    [runCatalogDownload],
  );

  const handleBuildQuadIndex = useCallback(async () => {
    setBuildingIndex(true);
    setIndexBuildError(null);
    setIndexBuildProgress(null);

    let resolvedViaEvent = false;
    let unlisten: (() => void) | null = null;
    try {
      // Persist the current config (including index_mag_limit) BEFORE
      // starting the rebuild — the backend reads the saved config when
      // it builds, so changes made in the inline control wouldn't take
      // effect otherwise.
      await api.invoke('set_plate_solve_config', { config });

      unlisten = await api.listen<QuadIndexProgressEvent>('quad-index-progress', (payload) => {
        setIndexBuildProgress(payload);
        if (payload.phase === 'complete') {
          resolvedViaEvent = true;
          unlisten?.();
          // Status will be refreshed after the invoke resolves
        }
      });

      const status = await api.invoke<QuadIndexStatus>('build_quad_index');
      setQuadIndexStatus(status);
    } catch (err) {
      console.error('Failed to build quad index:', err);
      setIndexBuildError(String(err));
    } finally {
      setBuildingIndex(false);
      setIndexBuildProgress(null);
      if (!resolvedViaEvent) {
        unlisten?.();
      }
    }
  }, [config]);

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

  const tycho2 = catalogs.find((c) => c.name === 'Tycho-2') ?? TYCHO2_FALLBACK_META;
  const tycho2Installed = tycho2.installed;
  const gaia = catalogs.find((c) => c.name === 'Gaia DR3') ?? GAIA_CATALOG_META;
  const gaiaInstalled = gaia.installed;

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

      {/* Catalog Status */}
      <section>
        <h4 className="text-sm font-semibold uppercase tracking-wider text-content-muted mb-3">
          Star Catalogs
        </h4>
        {catalogsLoading ? (
          <div className="flex items-center gap-2 text-sm text-content-muted py-2">
            <div className="animate-spin rounded-full h-4 w-4 border-b-2 border-accent" />
            Checking catalog status...
          </div>
        ) : (
          <div className="space-y-2">
            {/* Tycho-2 — dynamic based on get_catalog_status result */}
            <div className="rounded-lg border border-border bg-surface px-4 py-3 space-y-3">
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-3">
                  <Package size={18} className={tycho2Installed ? 'text-accent' : 'text-content-muted'} />
                  <div>
                    <p className="font-medium text-sm">{tycho2.name}</p>
                    <p className="text-xs text-content-muted">
                      Epoch {tycho2.epoch} &middot; {formatStarCount(tycho2.star_count_approx)} stars &middot; mag &le;{tycho2.mag_limit}
                    </p>
                  </div>
                </div>
                {tycho2Installed ? (
                  <span className="text-xs font-semibold px-2 py-0.5 rounded-full bg-success/20 text-success border border-success/30">
                    Installed
                  </span>
                ) : (
                  <span className="text-xs font-semibold px-2 py-0.5 rounded-full bg-surface-hover text-content-muted border border-border">
                    Not installed
                  </span>
                )}
              </div>

              {/* Download controls — only when Tycho-2 is not installed */}
              {!tycho2Installed && (
                <div className="space-y-2">
                  {downloadError && (
                    <p className="text-xs text-red-400">{downloadError}</p>
                  )}
                  {downloading && downloadKind === 'tycho2' && downloadProgress ? (
                    <div className="space-y-1.5">
                      <p className="text-xs text-content-muted">
                        {downloadProgress.phase === 'downloading'
                          ? `Downloading file ${downloadProgress.current}/${downloadProgress.total}`
                          : 'Converting stars...'}
                      </p>
                      <div className="w-full h-1.5 bg-surface-hover rounded-full overflow-hidden">
                        <div
                          className="h-full bg-violet-500 rounded-full transition-all duration-300"
                          style={{ width: `${downloadProgress.percent}%` }}
                        />
                      </div>
                      <p className="text-xs text-content-muted text-right">
                        {downloadProgress.percent.toFixed(0)}%
                      </p>
                    </div>
                  ) : (
                    <div className="flex items-start gap-3">
                      <button
                        onClick={handleDownloadTycho2}
                        disabled={downloading}
                        className="flex items-center gap-2 px-3 py-1.5 bg-violet-600 hover:bg-violet-700 disabled:opacity-50 rounded-lg text-xs font-medium transition-colors text-white"
                      >
                        <Download size={13} />
                        Download Tycho-2 Catalog
                      </button>
                      <p className="text-xs text-content-muted leading-relaxed pt-0.5">
                        ~160 MB download + local conversion required.
                      </p>
                    </div>
                  )}
                </div>
              )}
            </div>

            {/* Gaia DR3 — deep catalog; functional download */}
            <div className="rounded-lg border border-border bg-surface px-4 py-3 space-y-3">
              <div className="flex items-center justify-between">
                <div className="flex items-center gap-3">
                  <Package size={18} className={gaiaInstalled ? 'text-accent' : 'text-content-muted'} />
                  <div>
                    <p className="font-medium text-sm">{gaia.name}</p>
                    <p className="text-xs text-content-muted">
                      Epoch {gaia.epoch} &middot; {formatStarCount(gaia.star_count_approx)} stars &middot; mag &le;{gaia.mag_limit}
                    </p>
                  </div>
                </div>
                {gaiaInstalled ? (
                  <span className="text-xs font-semibold px-2 py-0.5 rounded-full bg-success/20 text-success border border-success/30">
                    Installed
                  </span>
                ) : (
                  <span className="text-xs font-semibold px-2 py-0.5 rounded-full bg-surface-hover text-content-muted border border-border">
                    Not installed
                  </span>
                )}
              </div>

              {!gaiaInstalled && (
                <div className="space-y-2">
                  {downloadError && downloadKind === 'gaia' && (
                    <p className="text-xs text-red-400">{downloadError}</p>
                  )}
                  {downloading && downloadKind === 'gaia' && downloadProgress ? (
                    <div className="space-y-1.5">
                      <p className="text-xs text-content-muted">
                        {downloadProgress.phase === 'downloading'
                          ? `Downloading tile ${downloadProgress.current}/${downloadProgress.total}`
                          : downloadProgress.phase === 'converting'
                            ? 'Converting stars...'
                            : 'Working...'}
                      </p>
                      <div className="w-full h-1.5 bg-surface-hover rounded-full overflow-hidden">
                        <div
                          className="h-full bg-violet-500 rounded-full transition-all duration-300"
                          style={{ width: `${downloadProgress.percent}%` }}
                        />
                      </div>
                      <p className="text-xs text-content-muted text-right">
                        {downloadProgress.percent.toFixed(0)}%
                      </p>
                    </div>
                  ) : (
                    <div className="flex items-start gap-3">
                      <button
                        onClick={handleDownloadGaia}
                        disabled={downloading}
                        className="flex items-center gap-2 px-3 py-1.5 bg-violet-600 hover:bg-violet-700 disabled:opacity-50 rounded-lg text-xs font-medium transition-colors text-white"
                      >
                        <Download size={13} />
                        Download Gaia DR3 Catalog
                      </button>
                      <p className="text-xs text-content-muted leading-relaxed pt-0.5">
                        ~4 GB, several hours (resumable — safe to close and resume). Deep
                        catalog needed for long-focal-length / headerless fields Tycho-2
                        cannot solve.
                      </p>
                    </div>
                  )}
                </div>
              )}
            </div>
          </div>
        )}
      </section>

      {/* Quad Index */}
      <section>
        <h4 className="text-sm font-semibold uppercase tracking-wider text-content-muted mb-3">
          Quad Index
        </h4>
        {quadIndexLoading ? (
          <div className="flex items-center gap-2 text-sm text-content-muted py-2">
            <div className="animate-spin rounded-full h-4 w-4 border-b-2 border-accent" />
            Checking index status...
          </div>
        ) : (
          <div className="rounded-lg border border-border bg-surface px-4 py-3 space-y-3">
            {/* Status row */}
            <div className="flex items-center justify-between">
              <div className="flex items-center gap-3">
                <Database
                  size={18}
                  className={quadIndexStatus?.built ? 'text-accent' : 'text-content-muted'}
                />
                <div>
                  <p className="font-medium text-sm">Quad Index</p>
                  {quadIndexStatus?.built ? (
                    <p className="text-xs text-content-muted">
                      {quadIndexStatus.quadCount.toLocaleString()} quads &middot; {formatBytes(quadIndexStatus.sizeBytes)}
                    </p>
                  ) : (
                    <p className="text-xs text-content-muted">Not built</p>
                  )}
                </div>
              </div>
              {quadIndexStatus?.built ? (
                <span className="text-xs font-semibold px-2 py-0.5 rounded-full bg-success/20 text-success border border-success/30">
                  Built
                </span>
              ) : (
                <span className="text-xs font-semibold px-2 py-0.5 rounded-full bg-surface-hover text-content-muted border border-border">
                  Not built
                </span>
              )}
            </div>

            {/* Build controls */}
            <div className="space-y-2">
              {indexBuildError && (
                <p className="text-xs text-red-400">{indexBuildError}</p>
              )}
              {buildingIndex && indexBuildProgress ? (
                <div className="space-y-1.5">
                  <p className="text-xs text-content-muted">
                    {indexBuildProgress.phase === 'reading'
                      ? `Reading pixel ${indexBuildProgress.pixel.toLocaleString()}/${indexBuildProgress.total.toLocaleString()} (${indexBuildProgress.quadsSoFar.toLocaleString()} quads so far)`
                      : indexBuildProgress.phase === 'writing'
                      ? 'Writing index to disk...'
                      : 'Complete'}
                  </p>
                  <div className="w-full h-1.5 bg-surface-hover rounded-full overflow-hidden">
                    <div
                      className="h-full bg-violet-500 rounded-full transition-all duration-300"
                      style={{ width: `${indexBuildProgress.percent}%` }}
                    />
                  </div>
                  <p className="text-xs text-content-muted text-right">
                    {indexBuildProgress.percent.toFixed(0)}%
                  </p>
                </div>
              ) : (
                <div className="space-y-2">
                  <div className="flex flex-wrap items-center gap-3">
                    <button
                      onClick={handleBuildQuadIndex}
                      disabled={buildingIndex || !tycho2Installed}
                      title={
                        !tycho2Installed
                          ? 'Install the Tycho-2 catalog first'
                          : quadIndexStatus?.built
                          ? 'Rebuild the quad index using the magnitude limit on the right'
                          : 'Build the quad index'
                      }
                      className="flex items-center gap-2 px-3 py-1.5 bg-violet-600 hover:bg-violet-700 disabled:opacity-50 rounded-lg text-xs font-medium transition-colors text-white"
                    >
                      <Database size={13} />
                      {buildingIndex
                        ? 'Building...'
                        : quadIndexStatus?.built
                        ? 'Rebuild Quad Index'
                        : 'Build Quad Index'}
                    </button>
                    <label className="flex items-center gap-2 text-xs text-content-secondary">
                      <span>at mag &le;</span>
                      <input
                        type="number"
                        min={8}
                        max={13}
                        step={0.5}
                        value={config.index_mag_limit}
                        onChange={(e) =>
                          setField(
                            'index_mag_limit',
                            parseFloat(e.target.value) || 0
                          )
                        }
                        disabled={buildingIndex || !tycho2Installed}
                        className="w-16 bg-surface-hover border border-border rounded px-2 py-1 text-xs focus:outline-none focus:ring-2 focus:ring-accent disabled:opacity-50"
                      />
                    </label>
                  </div>
                  {!tycho2Installed && (
                    <p className="text-xs text-content-muted leading-relaxed">
                      Requires the Tycho-2 catalog to be installed first.
                    </p>
                  )}
                  {tycho2Installed && (
                    <p className="text-xs text-content-muted leading-relaxed">
                      {quadIndexStatus?.built
                        ? 'Change the magnitude limit above and click Rebuild to use a deeper or shallower index. Deeper indexes (mag 12–13) cover long-exposure dense fields; shallower (mag 11) is smaller and faster. 13.0 is the Tycho-2 ceiling.'
                        : 'Builds the all-sky star quad index from the Tycho-2 catalog. Default mag 13 covers long-exposure dense fields; build time ~15–40 s.'}
                    </p>
                  )}
                </div>
              )}
            </div>
          </div>
        )}
      </section>

      {/* Solver Parameters */}
      <section>
        <h4 className="text-sm font-semibold uppercase tracking-wider text-content-muted mb-3">
          Solver Parameters
        </h4>
        <div className="grid grid-cols-1 gap-4 sm:grid-cols-2">

          {/* Max Image Stars */}
          <div>
            <label className="block text-sm font-medium text-content-secondary mb-1">
              Max Image Stars
            </label>
            <input
              type="number"
              min={10}
              max={1000}
              value={config.max_image_stars}
              onChange={(e) => setField('max_image_stars', parseInt(e.target.value, 10) || 0)}
              className="w-full bg-surface-hover border border-border rounded px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-accent"
            />
            <p className="mt-1 text-xs text-content-muted">
              Maximum number of detected stars used for quad building. Lower values are faster.
            </p>
          </div>

          {/* Min Matched Stars */}
          <div>
            <label className="block text-sm font-medium text-content-secondary mb-1">
              Min Matched Stars (floor)
            </label>
            <input
              type="number"
              min={4}
              max={100}
              value={config.min_matched_stars}
              onChange={(e) => setField('min_matched_stars', parseInt(e.target.value, 10) || 0)}
              className="w-full bg-surface-hover border border-border rounded px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-accent"
            />
            <p className="mt-1 text-xs text-content-muted">
              Absolute minimum inliers required, regardless of field density.
              The actual threshold is the larger of this value and the
              density-aware requirement (below). Default 6.
            </p>
          </div>

          {/* Min Inlier Ratio */}
          <div>
            <label className="block text-sm font-medium text-content-secondary mb-1">
              Min Inlier Ratio (dense fields)
            </label>
            <input
              type="number"
              min={0.02}
              max={0.5}
              step={0.01}
              value={config.min_inlier_ratio ?? 0.10}
              onChange={(e) => setField('min_inlier_ratio', parseFloat(e.target.value) || 0)}
              className="w-full bg-surface-hover border border-border rounded px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-accent"
            />
            <p className="mt-1 text-xs text-content-muted">
              For dense fields (&gt;100 catalog stars in FOV), the minimum
              fraction of catalog stars that must match. Default 0.10 (10%).
              Sparse fields use an absolute floor; this gate only tightens
              acceptance in star-rich regions.
            </p>
          </div>

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
                setField(
                  'base_verification_tolerance_arcsec',
                  parseFloat(e.target.value) || 0
                )
              }
              className="w-full bg-surface-hover border border-border rounded px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-accent"
            />
            <p className="mt-1 text-xs text-content-muted">
              Base angular tolerance for counting a catalog star as a
              verification match. The actual pixel tolerance adapts per
              frame: <code>base / pixel_scale</code>, clamped to [4, 20] px.
              Default 8.0".
            </p>
          </div>

          {/* Retry Passes */}
          <div>
            <label className="block text-sm font-medium text-content-secondary mb-1">
              Retry Passes (star counts)
            </label>
            <input
              type="text"
              value={(config.retry_passes ?? [50, 150, 300, 600]).join(', ')}
              onChange={(e) => {
                const parsed = e.target.value
                  .split(',')
                  .map((s) => parseInt(s.trim(), 10))
                  .filter((n) => Number.isFinite(n) && n > 0);
                setField('retry_passes', parsed.length > 0 ? parsed : [50, 150, 300, 600]);
              }}
              className="w-full bg-surface-hover border border-border rounded px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-accent"
            />
            <p className="mt-1 text-xs text-content-muted">
              Progressive star-count retry passes (comma-separated). The
              solver tries the first value first, escalating only when
              acceptance fails. Default <code>50, 150, 300, 600</code> — the
              small first pass targets dense galactic-plane fields where only
              the very brightest stars reliably match the catalog.
            </p>
          </div>

          {/* Index Mag Limit */}
          <div>
            <label className="block text-sm font-medium text-content-secondary mb-1">
              Index Magnitude Limit
            </label>
            <input
              type="number"
              min={6}
              max={13}
              step={0.5}
              value={config.index_mag_limit}
              onChange={(e) => setField('index_mag_limit', parseFloat(e.target.value) || 0)}
              className="w-full bg-surface-hover border border-border rounded px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-accent"
            />
            <p className="mt-1 text-xs text-content-muted">
              Faintest magnitude included when building the quad index.
              Default 13.0 — the practical ceiling of Tycho-2 (beyond this
              the catalog itself has no more stars). Covers long-exposure
              frames where the brightest visual stars are saturated and
              the detector's top-N falls into the mag 9–13 range.
              Changing this requires a rebuild.
            </p>
          </div>

          {/* Hash Tolerance */}
          <div>
            <label className="block text-sm font-medium text-content-secondary mb-1">
              Hash Tolerance
            </label>
            <input
              type="number"
              min={0.001}
              max={0.05}
              step={0.001}
              value={config.hash_tolerance}
              onChange={(e) => setField('hash_tolerance', parseFloat(e.target.value) || 0)}
              className="w-full bg-surface-hover border border-border rounded px-3 py-2 text-sm focus:outline-none focus:ring-2 focus:ring-accent"
            />
            <p className="mt-1 text-xs text-content-muted">
              Fractional tolerance when comparing geometric quad codes. Increase slightly for noisier data.
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
              Polynomial order for SIP distortion coefficients (2&ndash;5). Set to 2 to disable higher-order correction.
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
              onChange={(e) =>
                setField('autofind_tolerance_deg', parseFloat(e.target.value) || 0)
              }
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

          {/* Blind-solve fallback */}
          <div className="sm:col-span-2">
            <label className="flex items-start gap-2.5 cursor-pointer">
              <input
                type="checkbox"
                checked={config.fallback_to_blind_scale ?? true}
                onChange={(e) =>
                  setField('fallback_to_blind_scale', e.target.checked)
                }
                className="mt-0.5 h-4 w-4 rounded border-border bg-surface-hover text-accent focus:ring-2 focus:ring-accent"
              />
              <span>
                <span className="block text-sm font-medium text-content-secondary">
                  Fall back to a blind solve when the focal-length hint fails
                  (recommended)
                </span>
                <span className="mt-1 block text-xs text-content-muted">
                  When a solve using the FITS FOCALLEN fails, retry with the
                  scale hint cleared, then a full blind solve (scale and
                  position prior cleared). A wrong FOCALLEN &mdash; focal
                  reducer, wrong rig profile, or binning mismatch &mdash;
                  otherwise filters out every correct candidate and a
                  solvable frame fails permanently. On success the corrected
                  focal length is written back to the frame. Default: on.
                </span>
              </span>
            </label>
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
