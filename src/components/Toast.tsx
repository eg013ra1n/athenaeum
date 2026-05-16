import { X } from 'lucide-react';
import { useNavigate } from 'react-router-dom';
import { useNotifications } from '../contexts/NotificationContext';

/**
 * Stack of transient toast notifications, rendered bottom-right.
 * Toasts auto-dismiss after 5s (see NotificationContext), but the user can
 * also dismiss them manually. A toast with a `link` is clickable and
 * navigates to that in-app route (then dismisses).
 */
export function ToastStack() {
  const { toasts, dismissToast } = useNotifications();
  const navigate = useNavigate();
  if (toasts.length === 0) return null;

  return (
    <div className="pointer-events-none fixed bottom-6 right-6 z-50 flex flex-col gap-2">
      {toasts.map((toast) => {
        const toneClasses =
          toast.tone === 'success'
            ? 'bg-success border-success text-surface'
            : toast.tone === 'warning'
              ? 'bg-amber-900/95 border-amber-700 text-amber-50'
              : 'bg-surface-elevated border-border text-content';
        return (
          <div
            key={toast.id}
            role="status"
            className={`pointer-events-auto flex items-start gap-3 rounded-lg border px-4 py-3 shadow-lg backdrop-blur max-w-sm ${toneClasses}`}
          >
            {toast.link ? (
              <button
                type="button"
                onClick={() => {
                  navigate(toast.link!);
                  dismissToast(toast.id);
                }}
                className="flex-1 text-left text-sm font-medium leading-snug underline-offset-2 hover:underline"
              >
                {toast.message}
              </button>
            ) : (
              <div className="flex-1 text-sm leading-snug">{toast.message}</div>
            )}
            <button
              type="button"
              onClick={() => dismissToast(toast.id)}
              aria-label="Dismiss"
              className="shrink-0 rounded p-1 opacity-70 transition-opacity hover:opacity-100 hover:bg-white/10"
            >
              <X size={14} />
            </button>
          </div>
        );
      })}
    </div>
  );
}
