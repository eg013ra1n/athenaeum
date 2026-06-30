import { useNavigate } from 'react-router-dom';
import { AlertCircle, Download, Settings as SettingsIcon, X } from 'lucide-react';
import { api } from '../../api';
import { usePlateSolveProgressContext } from '../../contexts/PlateSolveProgressContext';

/**
 * Renders when the queue refused to enqueue a plate-solve batch — most often
 * because the solver star-catalog tiers haven't been downloaded yet.
 * Provides two CTAs: a one-click "Download now" that kicks off the base tier
 * set and takes the user to the progress view in Settings, and an "Open
 * Settings" shortcut to browse and choose tiers manually.
 *
 * Mounted once at the Layout level so every plate-solve entry point benefits
 * without per-page wiring.
 */
export function PlateSolveIndexMissingModal() {
  const navigate = useNavigate();
  const { precheckError, dismissPrecheckError } = usePlateSolveProgressContext();

  if (!precheckError) return null;

  const isCatalogMissing = precheckError.kind === 'catalog_missing';
  const title = isCatalogMissing
    ? 'Star catalog not downloaded'
    : 'Plate solve unavailable';

  const goToSettings = () => {
    dismissPrecheckError();
    navigate('/settings?tab=plate_solving');
  };

  // Fire the recommended base tier download (targetDensity 2000 = base + Δ1)
  // and navigate to Settings so the user can watch progress and choose more tiers.
  const downloadNow = () => {
    api.invoke('download_catalog_layers', { targetDensity: 2000 }).catch((err) => {
      console.error('[PlateSolveIndexMissingModal] download_catalog_layers failed:', err);
    });
    dismissPrecheckError();
    navigate('/settings?tab=plate_solving');
  };

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
                  <code>stars.smac</code>) on disk. Download the recommended
                  star-catalog set now, or open{' '}
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
                className="inline-flex items-center gap-2 px-4 py-2 bg-accent hover:bg-accent-hover text-surface rounded-lg transition"
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
