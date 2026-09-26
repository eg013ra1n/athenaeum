import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useId,
  useMemo,
  useRef,
  useState,
  type ReactNode,
} from 'react';
import { HardDrive, Loader2, RefreshCw, ShieldAlert } from 'lucide-react';
import { api } from '../../api';
import { useNotifications } from '../../contexts/NotificationContext';
import { ConfirmDialog } from '../ConfirmDialog';
import { formatTimestamp } from '../../utils/dateFormatting';
import type {
  CollabLiveStatus,
  CollabStorageStatus,
  ReplaceOutcomeView,
} from '../../types/models';

/**
 * The Collaboration folder's owner (spec §9.1, §9.5, L9): the device-replace
 * prompt, the take-over of a folder written by a device this account does not
 * list, and the swapped-disk explanation — one dialog, mounted once in
 * `Layout.tsx`.
 *
 * Reads: `get_collab_storage_status` is PASSIVE (no hub call, no write) and is
 * read on mount and whenever a `collab-live-status` event changes the
 * `(storage, storageReason)` pair — never on a timer, never for an event that
 * repeats the pair (a reconnect, a countdown). `check_collab_folder_owner` asks the hub (a 401 there
 * clears the session) and runs ONLY from a user's "Check again" click.
 *
 * `DeviceReplaceContext` lets `CollabLiveStatus` show a "Replace a device…"
 * link and open this dialog even when the replace is not prompted (offline
 * for 7 days or less) or when the exchange is off (the reinstall flow: a
 * refused designation of a folder another device wrote).
 */

/** What the folder-owner dialog has to offer, for the status line's link. */
export type FolderOwnerPending = 'replace' | 'resolve' | null;

interface DeviceReplaceContextValue {
  present: boolean;
  pending: FolderOwnerPending;
  openSeq: number;
  requestOpen: () => void;
  publish: (pending: FolderOwnerPending) => void;
}

const noop = () => {};

const DeviceReplaceContext = createContext<DeviceReplaceContextValue>({
  present: false,
  pending: null,
  openSeq: 0,
  requestOpen: noop,
  publish: noop,
});

export function DeviceReplaceProvider({ children }: { children: ReactNode }) {
  const [pending, setPending] = useState<FolderOwnerPending>(null);
  const [openSeq, setOpenSeq] = useState(0);
  const requestOpen = useCallback(() => setOpenSeq((n) => n + 1), []);
  const value = useMemo(
    () => ({ present: true, pending, openSeq, requestOpen, publish: setPending }),
    [pending, openSeq, requestOpen],
  );
  return <DeviceReplaceContext.Provider value={value}>{children}</DeviceReplaceContext.Provider>;
}

/** `available` is false outside `DeviceReplaceProvider` (then `requestOpen`
 *  does nothing and no link should be shown). */
export function useDeviceReplace(): {
  available: boolean;
  pending: FolderOwnerPending;
  requestOpen: () => void;
} {
  const ctx = useContext(DeviceReplaceContext);
  return { available: ctx.present, pending: ctx.pending, requestOpen: ctx.requestOpen };
}

function errMsg(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

/** Core's typed refusal for a swapped disk (`replace::MARKER_MISMATCH`). */
const MARKER_MISMATCH = 'collab_marker_mismatch';

function pendingOf(s: CollabStorageStatus | null): FolderOwnerPending {
  if (!s) return null;
  if (s.replace && !s.replace.markerMismatch) return 'replace';
  if (s.replace || s.unknownDevice || s.reason === 'other_device') return 'resolve';
  return null;
}

function shortId(id: string): string {
  return id.length > 16 ? `${id.slice(0, 8)}…${id.slice(-6)}` : id;
}

const BTN =
  'inline-flex items-center gap-1.5 rounded border border-border px-3 py-1.5 text-sm text-content-secondary transition-colors hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-50';
const BTN_PRIMARY =
  'inline-flex items-center gap-1.5 rounded bg-accent px-3 py-1.5 text-sm text-surface transition-colors hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-50';
const BTN_DANGER =
  'inline-flex items-center gap-1.5 rounded border border-error/50 px-3 py-1.5 text-sm text-error transition-colors hover:bg-error/10 disabled:cursor-not-allowed disabled:opacity-50';

export default function DeviceReplaceDialog() {
  const { notify } = useNotifications();
  const ctx = useContext(DeviceReplaceContext);
  const { publish, openSeq } = ctx;
  const titleId = useId();

  const [status, setStatus] = useState<CollabStorageStatus | null>(null);
  const [open, setOpen] = useState(false);
  const [busy, setBusy] = useState<'replace' | 'check' | 'takeOver' | null>(null);
  const [confirmTakeOver, setConfirmTakeOver] = useState(false);
  /** "Not now": the prompt stays closed until the next app start. */
  const dismissed = useRef(false);
  const mounted = useRef(true);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  /** The passive read — no hub call, no write. */
  const load = useCallback(async () => {
    try {
      const s = await api.invoke<CollabStorageStatus>('get_collab_storage_status');
      if (mounted.current) setStatus(s);
    } catch (err) {
      console.error('[collab] get_collab_storage_status failed:', err);
    }
  }, []);

  useEffect(() => {
    void load();
  }, [load]);

  // Re-read when the (storage, storageReason) pair changes — StrictMode-safe
  // pattern. The first event after mount always reads (the mount read may
  // predate it).
  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    let lastPair: string | null = null;
    api
      .listen<CollabLiveStatus>('collab-live-status', (p) => {
        if (cancelled) return;
        const pair = `${p.storage}|${p.storageReason ?? ''}`;
        if (pair === lastPair) return;
        lastPair = pair;
        void load();
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[collab] live-status listen failed (folder owner):', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [load]);

  // The status line's link.
  useEffect(() => {
    publish(pendingOf(status));
  }, [status, publish]);

  // The replace prompt opens on its own for a device offline more than 7 days.
  useEffect(() => {
    if (status?.replace?.prompt && !status.replace.markerMismatch && !dismissed.current) setOpen(true);
  }, [status]);

  // "Replace a device…" from the status line: opens whatever is pending.
  const lastSeq = useRef(openSeq);
  useEffect(() => {
    if (openSeq === lastSeq.current) return;
    lastSeq.current = openSeq;
    setOpen(true);
    void load();
  }, [openSeq, load]);

  const close = useCallback((notNow: boolean) => {
    if (notNow) dismissed.current = true;
    setOpen(false);
  }, []);

  // Escape closes the dialog (a window listener, so it works for the prompt
  // that opened on its own before anything in it had focus). While the
  // take-over confirm or an action is running it does nothing — the confirm
  // has its own Cancel. Closing the replace prompt this way counts as
  // "Not now".
  const escapeClosesAsNotNow = status?.replace != null && !status.replace.markerMismatch;
  useEffect(() => {
    if (!open || confirmTakeOver || busy !== null) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === 'Escape') close(escapeClosesAsNotNow);
    };
    window.addEventListener('keydown', onKeyDown);
    return () => window.removeEventListener('keydown', onKeyDown);
  }, [open, confirmTakeOver, busy, close, escapeClosesAsNotNow]);

  const checkAgain = async (root: string | null) => {
    setBusy('check');
    try {
      const s = await api.invoke<CollabStorageStatus>('check_collab_folder_owner', { root });
      if (!mounted.current) return;
      setStatus(s);
      if (pendingOf(s) === null) {
        notify({
          title: 'The Collaboration folder belongs to this device',
          detail: root ?? s.root ?? 'The folder no longer names another device.',
          kind: 'project',
          tone: 'success',
        });
        setOpen(false);
      }
    } catch (err) {
      // The hub could not be asked: the recorded (verified) answer is kept.
      console.error('[collab] check_collab_folder_owner failed:', err);
      notify({
        title: 'Could not check the folder owner',
        detail: `${errMsg(err)} — the last recorded answer is kept.`,
        kind: 'project',
        tone: 'warning',
        hasErrors: true,
      });
    } finally {
      if (mounted.current) setBusy(null);
    }
  };

  const refused = (title: string, err: unknown) => {
    const msg = errMsg(err);
    notify({
      title,
      detail: msg.includes(MARKER_MISMATCH)
        ? 'Another disk is mounted at the Collaboration folder — mount the disk it was set up on.'
        : msg,
      kind: 'project',
      tone: 'warning',
      hasErrors: true,
    });
  };

  /** `root` is the folder the dialog shows: in the one-slot case the offer
   *  can be for a refused folder other than the designated one, and core must
   *  not pick. */
  const replaceDevice = async (deviceId: string, deviceName: string, root: string) => {
    setBusy('replace');
    try {
      const out = await api.invoke<ReplaceOutcomeView>('collab_replace_device', { deviceId, root });
      notify({
        title: `Adopted ${out.adopted} of ${out.scanned} files`,
        detail: `This device replaces ${deviceName}; the files already in the Collaboration folder were adopted without downloading.`,
        kind: 'project',
        tone: 'success',
      });
      if (mounted.current) setOpen(false);
      void load();
    } catch (err) {
      console.error('[collab] collab_replace_device failed:', err);
      refused(`Could not replace ${deviceName}`, err);
    } finally {
      if (mounted.current) setBusy(null);
    }
  };

  const takeOver = async (root: string) => {
    setConfirmTakeOver(false);
    setBusy('takeOver');
    try {
      const out = await api.invoke<ReplaceOutcomeView>('take_over_collab_folder', { root, confirmed: true });
      notify({
        title: `Adopted ${out.adopted} of ${out.scanned} files`,
        detail: `This device took over ${root}.`,
        kind: 'project',
        tone: 'success',
      });
      if (mounted.current) setOpen(false);
      void load();
    } catch (err) {
      console.error('[collab] take_over_collab_folder failed:', err);
      refused('Could not take over the folder', err);
    } finally {
      if (mounted.current) setBusy(null);
    }
  };

  const kind = pendingOf(status);
  if (!open || !status || kind === null) return null;

  const replace = status.replace;
  const unknown = status.unknownDevice;
  const viewPath = replace?.path ?? unknown?.path ?? null;
  const mismatch = replace?.markerMismatch || unknown?.markerMismatch;
  // One record slot (M2): checking the designated folder replaces a pending
  // offer recorded for another folder.
  const otherFolder =
    status.root && status.reason === 'other_device' && viewPath && viewPath !== status.root ? status.root : null;
  const checkButton = (root: string | null, label = 'Check again') => (
    <button type="button" className={BTN} disabled={busy !== null} onClick={() => void checkAgain(root)}>
      {busy === 'check' ? <Loader2 size={14} className="animate-spin" /> : <RefreshCw size={14} />}
      {label}
    </button>
  );

  let title: string;
  let body: ReactNode;
  let actions: ReactNode;
  if (mismatch && viewPath) {
    title = 'Another disk is mounted here';
    body = (
      <p className="text-sm text-content-secondary">
        {`The Collaboration folder's marker does not match the one recorded for it: another disk is mounted at ${viewPath}. Mount the disk this folder was set up on, or choose a different Collaboration folder — a replace or take-over would be refused.`}
      </p>
    );
    actions = (
      <>
        <button type="button" className={BTN} onClick={() => close(false)}>
          Close
        </button>
        {checkButton(viewPath)}
      </>
    );
  } else if (replace) {
    const name = replace.deviceName;
    title = `This device replaces ${name}`;
    body = (
      <>
        <p className="text-sm text-content-secondary">
          {replace.offlineDays !== null
            ? `${name} has been offline for ${replace.offlineDays} days. Replacing it retires that device and adopts the files already in this folder — nothing is downloaded again.`
            : `Replacing ${name} retires that device and adopts the files already in this folder — nothing is downloaded again.`}
        </p>
        {replace.proposeRetire && (
          <p className="text-sm text-warning">{`${name} can be retired (offline for more than 30 days).`}</p>
        )}
        <p className="text-xs text-content-muted">
          {replace.lastSeenAt
            ? `Last seen ${formatTimestamp(replace.lastSeenAt)}, as of the last check — check again if ${name} may have been used since.`
            : `Never seen online, as of the last check.`}
        </p>
        <p className="break-all text-xs text-content-muted">Folder: {replace.path}</p>
      </>
    );
    actions = (
      <>
        <button type="button" className={BTN} onClick={() => close(true)}>
          Not now
        </button>
        {checkButton(replace.path)}
        <button
          type="button"
          className={BTN_PRIMARY}
          disabled={busy !== null}
          onClick={() => void replaceDevice(replace.deviceId, name, replace.path)}
        >
          {busy === 'replace' && <Loader2 size={14} className="animate-spin" />}
          {`Replace ${name}`}
        </button>
      </>
    );
  } else if (unknown) {
    title = 'This folder belongs to another device';
    body = (
      <>
        <p className="text-sm text-content-secondary">
          {`The marker in ${unknown.path} names a device this account does not list (${shortId(unknown.deviceId)}) — another account's device, a revoked device, or a disk from elsewhere.`}
        </p>
        {unknown.recordedOffline && (
          <p className="text-sm text-warning">
            Recorded while offline — the device may belong to this account. Check again once online.
          </p>
        )}
      </>
    );
    actions = (
      <>
        <button type="button" className={BTN} onClick={() => close(false)}>
          Close
        </button>
        {checkButton(unknown.path)}
        <button
          type="button"
          className={BTN_DANGER}
          disabled={busy !== null}
          onClick={() => setConfirmTakeOver(true)}
        >
          {busy === 'takeOver' && <Loader2 size={14} className="animate-spin" />}
          Take over this folder…
        </button>
      </>
    );
  } else {
    title = 'This folder belongs to another device';
    body = (
      <p className="text-sm text-content-secondary">
        {`The Collaboration folder's marker names another device${status.root ? ` (${status.root})` : ''}. Check again to ask the hub whether it is one of this account's devices.`}
      </p>
    );
    actions = (
      <>
        <button type="button" className={BTN} onClick={() => close(false)}>
          Close
        </button>
        {checkButton(status.root)}
      </>
    );
  }

  return (
    <>
      <div
        className="fixed inset-0 z-50 flex items-center justify-center bg-black/40"
        aria-hidden={confirmTakeOver ? true : undefined}
        onClick={() => busy === null && close(kind === 'replace')}
      >
        <div
          role="dialog"
          aria-modal="true"
          aria-labelledby={titleId}
          className="w-[32rem] max-w-[90vw] space-y-3 rounded-lg border border-border bg-surface-elevated p-4"
          onClick={(e) => e.stopPropagation()}
        >
          <div className="flex items-center gap-2">
            {mismatch ? (
              <HardDrive size={16} className="shrink-0 text-warning" />
            ) : (
              <ShieldAlert size={16} className="shrink-0 text-accent" />
            )}
            <h2 id={titleId} className="font-medium text-content">
              {title}
            </h2>
          </div>
          {body}
          {otherFolder && (
            <div className="space-y-2 rounded border border-border px-3 py-2">
              <p className="text-xs text-content-muted">
                {`The Collaboration folder ${otherFolder} names another device too. Checking ${otherFolder} replaces the pending offer for ${viewPath} — the app keeps one folder check at a time.`}
              </p>
              {checkButton(otherFolder, `Check ${otherFolder}`)}
            </div>
          )}
          <div className="flex flex-wrap justify-end gap-2 pt-1">{actions}</div>
        </div>
      </div>
      {unknown && (
        <ConfirmDialog
          isOpen={confirmTakeOver}
          title="Take over this folder?"
          message={`This folder was written by a device this account does not list${
            unknown.recordedOffline
              ? " (recorded while offline — it may be one of this account's devices; Check again while online first)"
              : ''
          }. Taking it over makes this device its owner and adopts the files it holds. If that device still uses this folder, the two will damage each other's data.\n\n${unknown.path}`}
          confirmText="Take over"
          confirmDanger
          onConfirm={() => void takeOver(unknown.path)}
          onCancel={() => setConfirmTakeOver(false)}
        />
      )}
    </>
  );
}
