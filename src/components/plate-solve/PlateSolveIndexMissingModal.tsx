import { useEffect, useState } from 'react';
import { useNavigate } from 'react-router-dom';
import { AlertCircle, Download, Settings as SettingsIcon, X } from 'lucide-react';
import { api } from '../../api';
import { usePlateSolveProgressContext } from '../../contexts/PlateSolveProgressContext';
import type { CatalogStatusInfo } from '../../types/helpers';
import type { FovSummary } from '../../types/plate-solve';
import { recommendFromFov, type TierRecommendation } from './cameraPresets';

/**
 * Renders when the queue refused to enqueue a plate-solve batch — most often
 * because the solver star-catalog tiers haven't been downloaded yet.
 * Provides two CTAs: a one-click "Download now" that kicks off the same tier
 * the Settings panel would recommend and takes the user to the progress view
 * in Settings, and an "Open Settings" shortcut to browse and choose tiers
 * manually.
 *
 * Mounted once at the Layout level so every plate-solve entry point benefits
 * without per-page wiring.
 */
export function PlateSolveIndexMissingModal() {
  const navigate = useNavigate();
  const { precheckError, dismissPrecheckError } = usePlateSolveProgressContext();

  const isCatalogMissing = precheckError?.kind === 'catalog_missing';

  // Same recommendation the Settings panel computes — from the live catalog
  // status (manifest tiers + install state) and the scanned light frames'
  // narrowest FOV — so "Download now" always requests exactly the tier the
  // panel would show as recommended, never a hard-coded density. Fetched
  // lazily, only once the modal actually has something to show, since the
  // component stays mounted for the app's whole lifetime.
  const [recommendation, setRecommendation] = useState<TierRecommendation | null>(null);

  useEffect(() => {
    if (!isCatalogMissing) return;
    let cancelled = false;
    (async () => {
      const [catalogs, fov] = await Promise.all([
        api.invoke<CatalogStatusInfo[]>('get_catalog_status').catch((err) => {
          console.error('[PlateSolveIndexMissingModal] get_catalog_status failed:', err);
          return [] as CatalogStatusInfo[];
        }),
        api.invoke<FovSummary>('get_frame_fov_summary').catch((err) => {
          console.error('[PlateSolveIndexMissingModal] get_frame_fov_summary failed:', err);
          return null;
        }),
      ]);
      if (cancelled) return;
      setRecommendation(recommendFromFov(fov?.min_fov_deg, catalogs));
    })();
    return () => {
      cancelled = true;
    };
  }, [isCatalogMissing]);

  if (!precheckError) return null;

  const title = isCatalogMissing ? 'Star catalog not downloaded' : 'Plate solve unavailable';

  const goToSettings = () => {
    dismissPrecheckError();
    navigate('/settings?tab=plate_solving');
  };

  const downloadNow = () => {
    if (recommendation) {
      api.invoke('download_catalog_layers', { targetDensity: recommendation.density }).catch((err) => {
        console.error('[PlateSolveIndexMissingModal] download_catalog_layers failed:', err);
      });
    }
    dismissPrecheckError();
    navigate('/settings?tab=plate_solving');
  };

  // Honest copy: only call it "recommended" when it's actually based on the
  // user's own scanned frames' FOV — otherwise it's just the smallest tier
  // the catalog publishes, so say "the base tier" instead.
  const tierLabel = recommendation
    ? `${recommendation.density.toLocaleString()} stars/deg²`
    : null;
  const tierPhrase = recommendation
    ? recommendation.isRecommendation
      ? `the recommended star-catalog set (${tierLabel})`
      : `the base star-catalog tier (${tierLabel})`
    : 'the base star-catalog set';

  return (
    <div
      className="fixed inset-0 bg-black/50 flex items-center justify-center z-50 p-4"
      onClick={dismissPrecheckError}
    >
      <div
        className="bg-surface-elevated rounded-lg max-w-md w-full p-6 border border-border"
        onClick={e => e.stopPropagation()}
      >
        <div className="flex items-start gap-3 mb-4">
          <AlertCircle size={22} className="text-warning flex-shrink-0 mt-0.5" />
          <div className="flex-1">
            <h3 className="text-lg font-semibold text-content">{title}</h3>
            <p className="text-sm text-content-muted mt-2 leading-relaxed">
              {isCatalogMissing ? (
                <>
                  Plate solving needs the star-catalog tiers (Gaia DR3,{' '}
                  <code>stars.smac</code>) on disk. Download {tierPhrase} now, or open{' '}
                  <span className="text-content-secondary">
                    Settings &rarr; Plate Solving
                  </span>{' '}
                  to choose which tiers to install.
                </>
              ) : (
                precheckError.message
              )}
            </p>
          </div>
          <button
            onClick={dismissPrecheckError}
            className="text-content-muted hover:text-content flex-shrink-0"
            aria-label="Dismiss"
          >
            <X size={18} />
          </button>
        </div>

        <div className="flex gap-3 justify-end mt-6">
          <button
            onClick={dismissPrecheckError}
            className="px-4 py-2 bg-surface-hover hover:bg-surface-hover/70 text-content-secondary rounded-lg transition"
          >
            Not now
          </button>
          {isCatalogMissing ? (
            <>
              <button
                onClick={goToSettings}
                className="inline-flex items-center gap-2 px-4 py-2 border border-border hover:bg-surface-hover text-content-secondary rounded-lg transition"
              >
                <SettingsIcon size={16} />
                Open Settings
              </button>
              <button
                onClick={downloadNow}
                disabled={!recommendation}
                title="Downloads this tier and every lower one not yet installed"
                className="inline-flex items-center gap-2 px-4 py-2 bg-accent hover:bg-accent-hover disabled:opacity-50 text-surface rounded-lg transition"
                autoFocus
              >
                <Download size={16} />
                Download now
              </button>
            </>
          ) : (
            <button
              onClick={goToSettings}
              className="inline-flex items-center gap-2 px-4 py-2 bg-accent hover:bg-accent-hover text-surface rounded-lg transition"
              autoFocus
            >
              <SettingsIcon size={16} />
              Open Plate-Solve Settings
            </button>
          )}
        </div>
      </div>
    </div>
  );
}
