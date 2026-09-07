import { ArrowLeft, ArrowRight } from 'lucide-react';
import { useNavHistory } from '../contexts/NavHistoryContext';
import { isMac } from '../utils/platform';

const BACK_HINT = isMac ? 'Back (Backspace or ⌘←)' : 'Back (Backspace or Alt+←)';
const FORWARD_HINT = isMac ? 'Forward (⌘→)' : 'Forward (Alt+→)';

const BUTTON_CLASS =
  'flex h-7 w-7 items-center justify-center rounded-lg text-content-muted transition-colors ' +
  'hover:bg-surface-hover hover:text-content ' +
  'disabled:pointer-events-none disabled:opacity-30';

/**
 * The app's back/forward pair, for the header row of a page.
 *
 * Sized to sit inside an existing title row (28px against a ≥32px line) so a
 * page gains navigation without gaining height — there is no global toolbar.
 * The buttons and the keyboard shortcuts in `useGlobalNavKeys` drive the same
 * session-scoped stack, so neither can leave the app.
 */
export function HistoryNav({
  className = '',
  fallback,
}: {
  className?: string;
  /**
   * Route to fall back to when there is no history to step back through — for
   * detail pages that must always offer a way out, even when opened as the
   * session's first page (a deep link, or a reload in the web build).
   */
  fallback?: string;
}) {
  const { canBack, canForward, back, forward, backOr } = useNavHistory();

  return (
    <div className={`flex shrink-0 items-center gap-0.5 ${className}`}>
      <button
        type="button"
        onClick={fallback ? () => backOr(fallback) : back}
        disabled={!canBack && !fallback}
        aria-label="Back"
        title={BACK_HINT}
        className={BUTTON_CLASS}
      >
        <ArrowLeft size={18} />
      </button>
      <button
        type="button"
        onClick={forward}
        disabled={!canForward}
        aria-label="Forward"
        title={FORWARD_HINT}
        className={BUTTON_CLASS}
      >
        <ArrowRight size={18} />
      </button>
    </div>
  );
}

export default HistoryNav;
