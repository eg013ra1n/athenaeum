import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useRef,
  useState,
  type ReactNode,
} from 'react';
import { api } from '../api';

/** Backend event payload for `monitor-detected`. */
export interface MonitorDetectedEvent {
  cycle_id: string;
  root_id: number;
  root_path: string;
  new_files: number;
  errors: string[];
  timestamp: string;
}

/** Backend event payload for `auto-merge-complete`. */
export interface AutoMergeCompleteEvent {
  frames_set_id: number;
  frames_set_name: string | null;
  /** "button" | "monitor" */
  source: string;
  added_count: number;
  skipped_count: number;
  threshold_arcmin: number;
  timestamp: string;
}

/** Short-lived toast shown bottom-right. */
export interface Toast {
  id: string;
  message: string;
  tone: 'info' | 'warning';
}

/** Persisted entry shown in the notification bell dropdown. */
export interface Notification {
  id: string;
  title: string;
  detail: string;
  /** ISO 8601 timestamp. */
  occurredAt: string;
  /** `true` until the user opens the bell dropdown. */
  unread: boolean;
  /** `true` when the scan emitted errors. */
  hasErrors: boolean;
}

interface NotificationContextValue {
  toasts: Toast[];
  notifications: Notification[];
  unreadCount: number;
  dismissToast: (id: string) => void;
  markAllRead: () => void;
  clearNotifications: () => void;
}

const NotificationContext = createContext<NotificationContextValue | null>(null);

const TOAST_TIMEOUT_MS = 5000;
const MAX_HISTORY = 50;

export function NotificationProvider({ children }: { children: ReactNode }) {
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [notifications, setNotifications] = useState<Notification[]>([]);
  const seenCycleIds = useRef<Set<string>>(new Set());

  const dismissToast = useCallback((id: string) => {
    setToasts((prev) => prev.filter((t) => t.id !== id));
  }, []);

  const addEvent = useCallback(
    (event: MonitorDetectedEvent) => {
      // Dedupe: each cycle_id is emitted once per root, but defensively we
      // track what we've already shown in case the same event re-arrives.
      const key = `${event.cycle_id}:${event.root_id}`;
      if (seenCycleIds.current.has(key)) return;
      seenCycleIds.current.add(key);

      const rootName =
        event.root_path.split(/[\\/]/).filter(Boolean).pop() ?? event.root_path;
      const fileWord = event.new_files === 1 ? 'frame' : 'frames';
      const title = `${event.new_files} new ${fileWord} in ${rootName}`;
      const detail = event.errors.length
        ? `${event.root_path} · ${event.errors.length} error${event.errors.length === 1 ? '' : 's'}`
        : event.root_path;

      const notification: Notification = {
        id: key,
        title,
        detail,
        occurredAt: event.timestamp,
        unread: true,
        hasErrors: event.errors.length > 0,
      };

      setNotifications((prev) => [notification, ...prev].slice(0, MAX_HISTORY));

      const toastId = `toast-${key}`;
      const toast: Toast = {
        id: toastId,
        message: title,
        tone: notification.hasErrors ? 'warning' : 'info',
      };
      setToasts((prev) => [...prev, toast]);
      setTimeout(() => dismissToast(toastId), TOAST_TIMEOUT_MS);
    },
    [dismissToast],
  );

  const addMergeEvent = useCallback(
    (event: AutoMergeCompleteEvent) => {
      const key = `merge-${event.frames_set_id}-${event.timestamp}`;
      if (seenCycleIds.current.has(key)) return;
      seenCycleIds.current.add(key);

      const label = event.frames_set_name ?? `Set #${event.frames_set_id}`;
      const fileWord = event.added_count === 1 ? 'frame' : 'frames';
      const sourceLabel = event.source === 'monitor' ? ' (auto)' : '';
      const title = `${event.added_count} ${fileWord} merged into ${label}${sourceLabel}`;
      const detail = event.skipped_count
        ? `threshold ${event.threshold_arcmin.toFixed(1)}′ · ${event.skipped_count} skipped`
        : `threshold ${event.threshold_arcmin.toFixed(1)}′`;

      const notification: Notification = {
        id: key,
        title,
        detail,
        occurredAt: event.timestamp,
        unread: true,
        hasErrors: false,
      };

      setNotifications((prev) => [notification, ...prev].slice(0, MAX_HISTORY));

      const toastId = `toast-${key}`;
      const toast: Toast = { id: toastId, message: title, tone: 'info' };
      setToasts((prev) => [...prev, toast]);
      setTimeout(() => dismissToast(toastId), TOAST_TIMEOUT_MS);
    },
    [dismissToast],
  );

  useEffect(() => {
    let unlistenMonitor: (() => void) | undefined;
    let unlistenMerge: (() => void) | undefined;
    let cancelled = false;
    api
      .listen<MonitorDetectedEvent>('monitor-detected', (payload) => {
        if (!cancelled) addEvent(payload);
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlistenMonitor = fn;
      })
      .catch((err) => console.error('[NotificationContext] listen failed:', err));
    api
      .listen<AutoMergeCompleteEvent>('auto-merge-complete', (payload) => {
        if (!cancelled) addMergeEvent(payload);
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlistenMerge = fn;
      })
      .catch((err) => console.error('[NotificationContext] listen failed:', err));
    return () => {
      cancelled = true;
      unlistenMonitor?.();
      unlistenMerge?.();
    };
  }, [addEvent, addMergeEvent]);

  const markAllRead = useCallback(() => {
    setNotifications((prev) => prev.map((n) => ({ ...n, unread: false })));
  }, []);

  const clearNotifications = useCallback(() => {
    setNotifications([]);
    seenCycleIds.current.clear();
  }, []);

  const unreadCount = notifications.filter((n) => n.unread).length;

  return (
    <NotificationContext.Provider
      value={{
        toasts,
        notifications,
        unreadCount,
        dismissToast,
        markAllRead,
        clearNotifications,
      }}
    >
      {children}
    </NotificationContext.Provider>
  );
}

export function useNotifications() {
  const ctx = useContext(NotificationContext);
  if (!ctx) throw new Error('useNotifications must be used within NotificationProvider');
  return ctx;
}
