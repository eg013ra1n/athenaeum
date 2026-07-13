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

/** Source category — drives the icon shown in the notification panel. */
export type NotificationKind =
  | 'files'
  | 'update'
  | 'merge'
  | 'scan'
  | 'export'
  | 'analysis'
  | 'platesolve'
  | 'autofind'
  | 'archive'
  | 'fileop'
  | 'registration'
  | 'masterbuild'
  | 'calibration'
  | 'sync'
  | 'project'
  | 'generic';

/** Short-lived toast shown bottom-right. */
export interface Toast {
  id: string;
  message: string;
  tone: 'info' | 'warning' | 'success';
  /** Optional in-app route; when set the toast is clickable and navigates. */
  link?: string;
}

/** Persisted entry shown in the notification panel. */
export interface Notification {
  id: string;
  title: string;
  detail: string;
  /** ISO 8601 timestamp. */
  occurredAt: string;
  /** `true` until the user opens the notification panel. */
  unread: boolean;
  /** `true` when the source reported errors. */
  hasErrors: boolean;
  kind: NotificationKind;
  /** Optional in-app route; when set the entry is clickable and navigates. */
  link?: string;
}

export interface NotifyInput {
  title: string;
  detail: string;
  tone?: 'info' | 'warning' | 'success';
  hasErrors?: boolean;
  link?: string;
  kind?: NotificationKind;
  /** When `false`, only the history entry is added (no transient toast). */
  toast?: boolean;
  /** When set, suppress duplicates with the same key (e.g. an operation id). */
  dedupeKey?: string;
}

/** The `notify` callable — exported so helpers can accept it as a parameter. */
export type NotifyLike = (input: NotifyInput) => void;

interface NotificationContextValue {
  toasts: Toast[];
  notifications: Notification[];
  unreadCount: number;
  panelOpen: boolean;
  dismissToast: (id: string) => void;
  markAllRead: () => void;
  clearNotifications: () => void;
  dismissNotification: (id: string) => void;
  openPanel: () => void;
  closePanel: () => void;
  /** Raise a toast + persistent history entry from non-event code. */
  notify: (input: NotifyInput) => void;
}

const NotificationContext = createContext<NotificationContextValue | null>(null);

const TOAST_TIMEOUT_MS = 5000;
const MAX_HISTORY = 100;
const SEEN_CAP = 200;
const STORAGE_KEY = 'athenaeum.notifications.v1';

interface PersistShape {
  v: number;
  notifications: Notification[];
  seen: string[];
}

/** Defensive read — any malformed/missing data starts empty, never throws. */
function loadPersisted(): { notifications: Notification[]; seen: string[] } {
  try {
    const raw =
      typeof localStorage !== 'undefined'
        ? localStorage.getItem(STORAGE_KEY)
        : null;
    if (!raw) return { notifications: [], seen: [] };
    const parsed = JSON.parse(raw) as Partial<PersistShape>;
    if (!parsed || parsed.v !== 1) return { notifications: [], seen: [] };
    const notifications = Array.isArray(parsed.notifications)
      ? parsed.notifications
          .filter(
            (n): n is Notification =>
              !!n && typeof n.id === 'string' && typeof n.title === 'string',
          )
          .map((n) => ({ ...n, kind: n.kind ?? 'generic' }))
          .slice(0, MAX_HISTORY)
      : [];
    const seen = Array.isArray(parsed.seen)
      ? parsed.seen.filter((s): s is string => typeof s === 'string').slice(-SEEN_CAP)
      : [];
    return { notifications, seen };
  } catch (err) {
    console.error('[NotificationContext] failed to load persisted state:', err);
    return { notifications: [], seen: [] };
  }
}

function savePersisted(notifications: Notification[], seen: string[]) {
  try {
    if (typeof localStorage === 'undefined') return;
    const payload: PersistShape = {
      v: 1,
      notifications: notifications.slice(0, MAX_HISTORY),
      seen: seen.slice(-SEEN_CAP),
    };
    localStorage.setItem(STORAGE_KEY, JSON.stringify(payload));
  } catch (err) {
    console.error('[NotificationContext] failed to persist state:', err);
  }
}

export function NotificationProvider({ children }: { children: ReactNode }) {
  const [boot] = useState(loadPersisted); // lazy: parse localStorage once
  const [toasts, setToasts] = useState<Toast[]>([]);
  const [notifications, setNotifications] = useState<Notification[]>(
    boot.notifications,
  );
  const [panelOpen, setPanelOpen] = useState(false);
  const seenRef = useRef<Set<string>>(new Set(boot.seen));

  // Persist whenever history changes (captures the current dedupe set too —
  // `seen` only ever grows alongside a notification push or a clear).
  useEffect(() => {
    savePersisted(notifications, Array.from(seenRef.current));
  }, [notifications]);

  const dismissToast = useCallback((id: string) => {
    setToasts((prev) => prev.filter((t) => t.id !== id));
  }, []);

  const pushNotification = useCallback(
    (
      input: NotifyInput & { dedupeKey?: string; occurredAt?: string },
    ) => {
      if (input.dedupeKey) {
        if (seenRef.current.has(input.dedupeKey)) return;
        seenRef.current.add(input.dedupeKey);
        if (seenRef.current.size > SEEN_CAP) {
          seenRef.current = new Set(
            Array.from(seenRef.current).slice(-SEEN_CAP),
          );
        }
      }
      const id =
        input.dedupeKey ??
        `ntf-${Date.now()}-${Math.random().toString(36).slice(2)}`;
      const notification: Notification = {
        id,
        title: input.title,
        detail: input.detail,
        occurredAt: input.occurredAt ?? new Date().toISOString(),
        unread: true,
        hasErrors: input.hasErrors ?? false,
        kind: input.kind ?? 'generic',
        link: input.link,
      };
      setNotifications((prev) => [notification, ...prev].slice(0, MAX_HISTORY));

      if (input.toast !== false) {
        const toastId = `toast-${id}`;
        const toast: Toast = {
          id: toastId,
          message: input.title,
          tone: input.tone ?? (input.hasErrors ? 'warning' : 'info'),
          link: input.link,
        };
        setToasts((prev) => [...prev, toast]);
        setTimeout(() => dismissToast(toastId), TOAST_TIMEOUT_MS);
      }
    },
    [dismissToast],
  );

  const notify = useCallback(
    (input: NotifyInput) => pushNotification(input),
    [pushNotification],
  );

  const addEvent = useCallback(
    (event: MonitorDetectedEvent) => {
      const rootName =
        event.root_path.split(/[\\/]/).filter(Boolean).pop() ?? event.root_path;
      const fileWord = event.new_files === 1 ? 'frame' : 'frames';
      const hasErrors = event.errors.length > 0;
      pushNotification({
        title: `${event.new_files} new ${fileWord} in ${rootName}`,
        detail: hasErrors
          ? `${event.root_path} · ${event.errors.length} error${event.errors.length === 1 ? '' : 's'}`
          : event.root_path,
        hasErrors,
        kind: 'files',
        occurredAt: event.timestamp,
        dedupeKey: `${event.cycle_id}:${event.root_id}`,
      });
    },
    [pushNotification],
  );

  const addMergeEvent = useCallback(
    (event: AutoMergeCompleteEvent) => {
      const label = event.frames_set_name ?? `Set #${event.frames_set_id}`;
      const fileWord = event.added_count === 1 ? 'frame' : 'frames';
      const sourceLabel = event.source === 'monitor' ? ' (auto)' : '';
      pushNotification({
        title: `${event.added_count} ${fileWord} merged into ${label}${sourceLabel}`,
        detail: event.skipped_count
          ? `threshold ${event.threshold_arcmin.toFixed(1)}′ · ${event.skipped_count} skipped`
          : `threshold ${event.threshold_arcmin.toFixed(1)}′`,
        kind: 'merge',
        occurredAt: event.timestamp,
        dedupeKey: `merge-${event.frames_set_id}-${event.timestamp}`,
      });
    },
    [pushNotification],
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
    setNotifications((prev) =>
      prev.some((n) => n.unread)
        ? prev.map((n) => (n.unread ? { ...n, unread: false } : n))
        : prev,
    );
  }, []);

  const clearNotifications = useCallback(() => {
    seenRef.current.clear();
    setNotifications([]);
  }, []);

  const dismissNotification = useCallback((id: string) => {
    setNotifications((prev) => prev.filter((n) => n.id !== id));
  }, []);

  const openPanel = useCallback(() => {
    setPanelOpen(true);
    markAllRead();
  }, [markAllRead]);

  const closePanel = useCallback(() => setPanelOpen(false), []);

  const unreadCount = notifications.filter((n) => n.unread).length;

  return (
    <NotificationContext.Provider
      value={{
        toasts,
        notifications,
        unreadCount,
        panelOpen,
        dismissToast,
        markAllRead,
        clearNotifications,
        dismissNotification,
        openPanel,
        closePanel,
        notify,
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
