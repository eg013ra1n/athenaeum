import { useEffect, useRef, useState } from 'react';
import { Bell, AlertCircle } from 'lucide-react';
import { useNotifications } from '../contexts/NotificationContext';

/**
 * Sidebar bell icon showing recent monitor-detected events.
 * - Badge shows unread count until the dropdown is opened.
 * - Dropdown lists the most recent events with timestamps.
 */
export function NotificationBell({ collapsed }: { collapsed: boolean }) {
  const { notifications, unreadCount, markAllRead, clearNotifications } =
    useNotifications();
  const [open, setOpen] = useState(false);
  const rootRef = useRef<HTMLDivElement>(null);

  useEffect(() => {
    if (!open) return;
    const handleClick = (e: MouseEvent) => {
      if (rootRef.current && !rootRef.current.contains(e.target as Node)) {
        setOpen(false);
      }
    };
    window.addEventListener('mousedown', handleClick);
    return () => window.removeEventListener('mousedown', handleClick);
  }, [open]);

  const toggle = () => {
    const next = !open;
    setOpen(next);
    if (next && unreadCount > 0) markAllRead();
  };

  return (
    <div ref={rootRef} className={`relative ${collapsed ? 'px-2' : 'px-4'} py-2`}>
      <button
        type="button"
        onClick={toggle}
        aria-label="Notifications"
        title={unreadCount > 0 ? `${unreadCount} new` : 'Notifications'}
        className={`flex items-center ${collapsed ? 'justify-center px-0' : 'gap-3 px-4'} py-3 rounded-lg transition-colors text-content-secondary hover:bg-surface-hover w-full`}
      >
        <div className="relative shrink-0">
          <Bell size={20} />
          {unreadCount > 0 && (
            <span className="absolute -right-1.5 -top-1.5 flex h-4 min-w-4 items-center justify-center rounded-full bg-accent px-1 text-[10px] font-semibold leading-none text-surface">
              {unreadCount > 9 ? '9+' : unreadCount}
            </span>
          )}
        </div>
        {!collapsed && <span>Notifications</span>}
      </button>

      {open && (
        <div
          className="absolute bottom-full left-full ml-2 mb-2 w-80 origin-bottom-left rounded-lg border border-border bg-surface-elevated shadow-xl z-50"
          role="dialog"
          aria-label="Recent notifications"
        >
          <div className="flex items-center justify-between border-b border-border px-4 py-2">
            <h3 className="text-sm font-medium">Recent activity</h3>
            {notifications.length > 0 && (
              <button
                type="button"
                onClick={clearNotifications}
                className="text-xs text-content-muted hover:text-content"
              >
                Clear
              </button>
            )}
          </div>
          <div className="max-h-80 overflow-y-auto">
            {notifications.length === 0 ? (
              <p className="px-4 py-6 text-center text-sm text-content-muted">
                No recent activity
              </p>
            ) : (
              <ul className="divide-y divide-border">
                {notifications.map((n) => (
                  <li key={n.id} className="px-4 py-3">
                    <div className="flex items-start gap-2">
                      {n.hasErrors && (
                        <AlertCircle size={14} className="mt-0.5 shrink-0 text-amber-500" />
                      )}
                      <div className="min-w-0 flex-1">
                        <p className="text-sm">{n.title}</p>
                        <p className="mt-0.5 truncate text-xs text-content-muted" title={n.detail}>
                          {n.detail}
                        </p>
                        <p className="mt-1 text-[10px] uppercase tracking-wide text-content-muted">
                          {formatTimestamp(n.occurredAt)}
                        </p>
                      </div>
                    </div>
                  </li>
                ))}
              </ul>
            )}
          </div>
        </div>
      )}
    </div>
  );
}

function formatTimestamp(iso: string): string {
  const date = new Date(iso);
  if (Number.isNaN(date.getTime())) return iso;
  const pad = (n: number) => n.toString().padStart(2, '0');
  return `${date.getFullYear()}-${pad(date.getMonth() + 1)}-${pad(date.getDate())} ${pad(date.getHours())}:${pad(date.getMinutes())}`;
}
