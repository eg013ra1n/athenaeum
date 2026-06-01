import { useEffect, useRef } from 'react';
import { useNavigate } from 'react-router-dom';
import {
  X,
  Trash2,
  AlertCircle,
  Bell,
  FolderPlus,
  Download,
  GitMerge,
  ScanLine,
  FileOutput,
  Activity,
  Crosshair,
  Target,
  Archive,
  FolderInput,
  Layers,
  type LucideIcon,
} from 'lucide-react';
import { useNotifications, type NotificationKind } from '../contexts/NotificationContext';
import { formatTimestamp } from '../utils/dateFormatting';

const KIND_ICON: Record<NotificationKind, LucideIcon> = {
  files: FolderPlus,
  update: Download,
  merge: GitMerge,
  scan: ScanLine,
  export: FileOutput,
  analysis: Activity,
  platesolve: Crosshair,
  autofind: Target,
  archive: Archive,
  fileop: FolderInput,
  registration: Layers,
  generic: Bell,
};

/**
 * Global notification center — a right-side slide-over opened from the bell.
 * Rendered at app root (outside the clipped sidebar) so it is always visible.
 */
export function NotificationPanel() {
  const {
    notifications,
    panelOpen,
    closePanel,
    clearNotifications,
    dismissNotification,
  } = useNotifications();
  const navigate = useNavigate();
  const closeBtnRef = useRef<HTMLButtonElement>(null);

  useEffect(() => {
    if (!panelOpen) return;
    closeBtnRef.current?.focus();
    const onKey = (e: KeyboardEvent) => {
      if (e.key === 'Escape') closePanel();
    };
    window.addEventListener('keydown', onKey);
    return () => window.removeEventListener('keydown', onKey);
  }, [panelOpen, closePanel]);

  return (
    <>
      {panelOpen && (
        <div
          className="fixed inset-0 z-[55] bg-black/50"
          onClick={closePanel}
          aria-hidden="true"
        />
      )}
      <aside
        role="dialog"
        aria-label="Notifications"
        aria-hidden={!panelOpen}
        className={`fixed inset-y-0 right-0 z-[60] flex w-96 max-w-[90vw] flex-col border-l border-border bg-surface-elevated shadow-xl transition-transform duration-200 ${
          panelOpen ? 'translate-x-0' : 'translate-x-full pointer-events-none'
        }`}
      >
        <div className="flex items-center justify-between border-b border-border px-4 py-3">
          <h2 className="text-sm font-semibold">Notifications</h2>
          <div className="flex items-center gap-1">
            <button
              type="button"
              onClick={clearNotifications}
              disabled={notifications.length === 0}
              className="flex items-center gap-1 rounded px-2 py-1 text-xs text-content-muted transition-colors hover:bg-surface-hover hover:text-content disabled:cursor-not-allowed disabled:opacity-40"
            >
              <Trash2 size={13} />
              Clear all
            </button>
            <button
              ref={closeBtnRef}
              type="button"
              onClick={closePanel}
              aria-label="Close notifications"
              className="rounded p-1 text-content-muted transition-colors hover:bg-surface-hover hover:text-content"
            >
              <X size={16} />
            </button>
          </div>
        </div>

        <div className="flex-1 overflow-y-auto">
          {notifications.length === 0 ? (
            <p className="px-4 py-10 text-center text-sm text-content-muted">
              No notifications
            </p>
          ) : (
            <ul className="divide-y divide-border">
              {notifications.map((n) => {
                const Icon = KIND_ICON[n.kind] ?? Bell;
                const body = (
                  <div className="flex items-start gap-3">
                    <Icon
                      size={16}
                      className={`mt-0.5 shrink-0 ${n.hasErrors ? 'text-error' : 'text-content-muted'}`}
                    />
                    <div className="min-w-0 flex-1">
                      <p className="flex items-center gap-1 text-sm">
                        {n.hasErrors && (
                          <AlertCircle size={13} className="shrink-0 text-error" />
                        )}
                        <span className="truncate">{n.title}</span>
                      </p>
                      <p
                        className="mt-0.5 truncate text-xs text-content-muted"
                        title={n.detail}
                      >
                        {n.detail}
                      </p>
                      <p className="mt-1 text-[10px] uppercase tracking-wide text-content-muted">
                        {formatTimestamp(n.occurredAt)}
                      </p>
                    </div>
                    <button
                      type="button"
                      aria-label="Dismiss notification"
                      onClick={(e) => {
                        e.stopPropagation();
                        dismissNotification(n.id);
                      }}
                      className="shrink-0 rounded p-1 text-content-muted opacity-60 transition-opacity hover:bg-surface-hover hover:opacity-100"
                    >
                      <X size={13} />
                    </button>
                  </div>
                );
                return (
                  <li key={n.id}>
                    {n.link ? (
                      <button
                        type="button"
                        onClick={() => {
                          navigate(n.link!);
                          closePanel();
                        }}
                        className="w-full px-4 py-3 text-left transition-colors hover:bg-surface-hover"
                      >
                        {body}
                      </button>
                    ) : (
                      <div className="px-4 py-3">{body}</div>
                    )}
                  </li>
                );
              })}
            </ul>
          )}
        </div>
      </aside>
    </>
  );
}
