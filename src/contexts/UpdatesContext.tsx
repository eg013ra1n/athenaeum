// In-app updates (spec §2.5, §5): the last check, the dialog's mode and the
// install phase, shared by the launch hook, the About page and the dialog.
// Listeners use the cancelled-flag pattern from CLAUDE.md.

import { createContext, useCallback, useContext, useEffect, useMemo, useRef, useState, type ReactNode } from 'react';
import { api } from '../api';
import type { UpdateCheck, WhatsNew } from '../types/models';

export type DialogMode = 'closed' | 'available' | 'whatsNew';

export type InstallPhase =
  | { kind: 'idle' }
  | { kind: 'downloading'; downloaded: number; total: number | null }
  | { kind: 'ready'; version: string }
  | { kind: 'failed'; message: string };

interface ProgressEvent {
  downloaded: number | null;
  total: number | null;
  finished?: boolean;
}

interface UpdatesContextValue {
  /** Last `check_for_updates` result, `null` before the first check. */
  check: UpdateCheck | null;
  /** What the dialog shows in `whatsNew` mode. */
  whatsNew: WhatsNew | null;
  dialog: DialogMode;
  phase: InstallPhase;
  /** Error text of the last manual check, if any. */
  checkError: string | null;
  checking: boolean;
  runCheck: () => Promise<UpdateCheck | null>;
  openAvailable: () => void;
  /** Open `whatsNew` mode with the given notes (launch: the once-per-version result). */
  openWhatsNew: (w: WhatsNew) => void;
  /** Open `whatsNew` mode with the running build's notes (About: View release notes). */
  openReleaseNotes: () => Promise<void>;
  close: () => void;
  install: () => Promise<void>;
  restart: () => Promise<void>;
}

const UpdatesContext = createContext<UpdatesContextValue | null>(null);

export function UpdatesProvider({ children }: { children: ReactNode }) {
  const [check, setCheck] = useState<UpdateCheck | null>(null);
  const [whatsNew, setWhatsNew] = useState<WhatsNew | null>(null);
  const [dialog, setDialog] = useState<DialogMode>('closed');
  const [phase, setPhase] = useState<InstallPhase>({ kind: 'idle' });
  const [checkError, setCheckError] = useState<string | null>(null);
  const [checking, setChecking] = useState(false);
  const checkRef = useRef<UpdateCheck | null>(null);

  useEffect(() => {
    let cancelled = false;
    let unlistenProgress: (() => void) | undefined;
    let unlistenReady: (() => void) | undefined;
    api
      .listen<ProgressEvent>('update-progress', (p) => {
        if (cancelled) return;
        if (p.finished || p.downloaded === null) return;
        setPhase({ kind: 'downloading', downloaded: p.downloaded, total: p.total });
      })
      .then((fn) => { if (cancelled) fn(); else unlistenProgress = fn; })
      .catch((err) => console.error('[updates] listen update-progress failed:', err));
    api
      .listen<{ version: string }>('update-ready', (p) => {
        if (cancelled) return;
        setPhase({ kind: 'ready', version: p.version });
      })
      .then((fn) => { if (cancelled) fn(); else unlistenReady = fn; })
      .catch((err) => console.error('[updates] listen update-ready failed:', err));
    return () => { cancelled = true; unlistenProgress?.(); unlistenReady?.(); };
  }, []);

  const runCheck = useCallback(async (): Promise<UpdateCheck | null> => {
    setChecking(true);
    setCheckError(null);
    try {
      const result = await api.invoke<UpdateCheck>('check_for_updates');
      checkRef.current = result;
      setCheck(result);
      return result;
    } catch (err) {
      const msg = typeof err === 'string' ? err : 'Failed to check for updates';
      console.error('check_for_updates:', err);
      setCheckError(msg);
      return null;
    } finally {
      setChecking(false);
    }
  }, []);

  const openAvailable = useCallback(() => setDialog('available'), []);
  const openWhatsNew = useCallback((w: WhatsNew) => { setWhatsNew(w); setDialog('whatsNew'); }, []);
  const openReleaseNotes = useCallback(async () => {
    try {
      const w = await api.invoke<WhatsNew>('get_release_notes');
      setWhatsNew(w);
      setDialog('whatsNew');
    } catch (err) {
      console.error('get_release_notes:', err);
    }
  }, []);
  const close = useCallback(() => setDialog('closed'), []);

  const install = useCallback(async () => {
    const current = checkRef.current;
    if (!current) return;
    setPhase({ kind: 'downloading', downloaded: 0, total: null });
    try {
      // The Tauri command takes `channel` directly (not a wrapped `args`
      // struct) — see `crates/athenaeum-tauri/src/commands/updates.rs`'s
      // `install_update(app, state, channel: Channel)`.
      await api.invoke('install_update', { channel: current.channel });
      // `update-ready` sets the ready phase; on Windows the app exits before.
    } catch (err) {
      const message = typeof err === 'string' ? err : 'The update could not be installed';
      console.error('install_update:', err);
      setPhase({ kind: 'failed', message });
    }
  }, []);

  const restart = useCallback(async () => {
    try {
      await api.invoke('restart_app');
    } catch (err) {
      console.error('restart_app:', err);
      setPhase({ kind: 'failed', message: typeof err === 'string' ? err : 'Restart failed' });
    }
  }, []);

  const value = useMemo<UpdatesContextValue>(
    () => ({ check, whatsNew, dialog, phase, checkError, checking, runCheck, openAvailable, openWhatsNew, openReleaseNotes, close, install, restart }),
    [check, whatsNew, dialog, phase, checkError, checking, runCheck, openAvailable, openWhatsNew, openReleaseNotes, close, install, restart],
  );
  return <UpdatesContext.Provider value={value}>{children}</UpdatesContext.Provider>;
}

export function useUpdates(): UpdatesContextValue {
  const ctx = useContext(UpdatesContext);
  if (!ctx) throw new Error('useUpdates must be used within UpdatesProvider');
  return ctx;
}
