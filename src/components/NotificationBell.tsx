import { Bell } from 'lucide-react';
import { useNotifications } from '../contexts/NotificationContext';

/**
 * Sidebar bell button. Shows the unread badge and opens the global
 * notification panel (rendered at app root in Layout, so it is not clipped
 * by the sidebar's overflow).
 */
export function NotificationBell({ collapsed }: { collapsed: boolean }) {
  const { unreadCount, openPanel } = useNotifications();

  return (
    <div className={`relative ${collapsed ? 'px-2' : 'px-4'} py-2`}>
      <button
        type="button"
        onClick={openPanel}
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
    </div>
  );
}
