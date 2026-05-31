import { useNavigate } from 'react-router-dom';
import { AlertCircle, Settings as SettingsIcon, X } from 'lucide-react';
import { usePlateSolveProgressContext } from '../../contexts/PlateSolveProgressContext';

/**
 * Renders when the queue refused to enqueue a plate-solve batch — most often
 * because the solver star catalog (`stars.smac`) hasn't been downloaded yet.
 * Provides a single CTA that deep-links into Settings → Plate Solving so the
 * user can download it without hunting through the Settings page.
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
                  Plate solving needs the star catalog (Gaia DR3,{' '}
                  <code>stars.smac</code>) on disk. Open{' '}
                  <span className="text-content-secondary">
                    Settings &rarr; Plate Solving
                  </span>{' '}
                  to download it — a one-time prebuilt download.
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
          <button
            onClick={goToSettings}
            className="inline-flex items-center gap-2 px-4 py-2 bg-accent hover:bg-accent-hover text-white rounded-lg transition"
            autoFocus
          >
            <SettingsIcon size={16} />
            Open Plate-Solve Settings
          </button>
        </div>
      </div>
    </div>
  );
}
