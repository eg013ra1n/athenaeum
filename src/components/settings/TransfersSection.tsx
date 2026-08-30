// Settings → Transfers (task 15). Renders inner content only — the host
// card/heading are supplied by `Settings.tsx`, matching the `SyncSection` /
// `LoggingSettings` pattern.
//
// The two Folders cards are new (transfer-prepare spec §6.3–6.5: the outgoing
// staging folder and the incoming working folder). Bandwidth, Receiving and
// Storage moved here from `SyncSection` unchanged — same commands, same
// validation, same notifications. Sync keeps account status + pairing.
//
// Everything on this tab is device-local and account-independent, so it loads
// on mount: the folders must be configurable before this machine is paired.

import { useCallback, useEffect, useRef, useState } from 'react';
import { AlertTriangle, FolderOpen, Loader2, RotateCcw, Save, Trash2 } from 'lucide-react';
import { api } from '../../api';
import { pickDirectory } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import { FolderBrowserModal } from '../FolderBrowserModal';
import { formatBytes } from '../transfers/presentation';
import { useNotifications } from '../../contexts/NotificationContext';
import type {
  PathSetting,
  TransferCleanup,
  TransferPaths,
  TransferStorage,
} from '../../types/models';

/** Tauri and Axum both reject with a plain string, not an `Error`. */
function errMsg(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

// Upload speed limit (W1). The setting `sync.max_upload_bytes_per_sec` is stored
// as BYTES per second ("0" = unlimited); the field shows DECIMAL megabytes per
// second — 1 MB/s = 1_000_000 bytes/s, the convention ISPs and network gear use,
// NOT 1 MiB/s = 1_048_576. Keep the two directions symmetric.
const BYTES_PER_MB = 1_000_000;

/** Client mirror of the server floor (100000 bytes/s), so the common mistake
 *  gets an inline answer instead of a round-trip error. */
const MIN_LIMIT_MB = 0.1;

/** bytes/s (as stored, a string) → the MB/s text shown in the field. `0` and
 *  anything unparseable render as empty, which the field labels "Unlimited". */
function bytesToMbInput(raw: string): string {
  const bytes = Number(raw);
  if (!Number.isFinite(bytes) || bytes <= 0) return '';
  // Trim float artifacts: 500000 / 1e6 must read "0.5", not "0.5000000000000001".
  return String(Number((bytes / BYTES_PER_MB).toFixed(3)));
}

// Simultaneous incoming transfers (W2 T2.7). `sync.max_concurrent_receives` is
// stored as a plain integer string; the server accepts 1..=8 and the receiver's
// getter clamps into the same window. Mirror both here so a bad value never
// round-trips and the field never shows a cap the receiver isn't using.
const MIN_RECEIVES = 1;
const MAX_RECEIVES = 8;
const DEFAULT_RECEIVES = '2';

/** Stored value → the integer text shown in the field, clamped the same way the
 *  receiver clamps it (`SettingsManager::get_sync_max_concurrent_receives`), so
 *  a hand-edited or migrated row displays the cap actually in force. Anything
 *  unparseable falls back to the default. */
function receivesToInput(raw: string): string {
  const n = Number(raw);
  if (!Number.isFinite(n) || !Number.isInteger(n)) return DEFAULT_RECEIVES;
  return String(Math.min(MAX_RECEIVES, Math.max(MIN_RECEIVES, n)));
}

/** One folder card: effective path, default hint, Choose… / Use default, restart badge. */
function FolderCard({
  title,
  hint,
  setting,
  onChoose,
  onReset,
  error,
  busy,
}: {
  title: string;
  hint: string;
  setting: PathSetting;
  onChoose: () => void;
  onReset: () => void;
  error: string | null;
  busy: boolean;
}) {
  return (
    <div className="rounded-lg border border-border bg-surface p-3">
      <div className="flex items-center justify-between gap-3">
        <h4 className="text-sm font-medium text-content-secondary">{title}</h4>
        {setting.restartRequired && (
          <span className="inline-flex items-center gap-1 rounded px-1.5 py-0.5 text-[10px] font-medium bg-warning/15 text-warning">
            <AlertTriangle size={11} /> Restart Athenaeum to apply
          </span>
        )}
      </div>
      <p className="mt-1 font-mono text-xs text-content break-all" title={setting.effective}>
        {setting.effective}
      </p>
      <p className="mt-1 text-[11px] text-content-muted">
        {setting.configured ? `Default: ${setting.default}` : 'Default location'} · {hint}
      </p>
      {error && <p className="mt-1 text-[11px] text-error">{error}</p>}
      <div className="mt-2 flex items-center gap-2">
        <button
          type="button"
          onClick={onChoose}
          disabled={busy}
          className="inline-flex items-center gap-1 rounded border border-border bg-surface-elevated px-2 py-1 text-xs text-content hover:bg-surface disabled:opacity-50"
        >
          <FolderOpen size={12} /> Choose…
        </button>
        {setting.configured && (
          <button
            type="button"
            onClick={onReset}
            disabled={busy}
            className="inline-flex items-center gap-1 rounded px-2 py-1 text-xs text-content-muted hover:text-content disabled:opacity-50"
          >
            <RotateCcw size={12} /> Use default
          </button>
        )}
      </div>
    </div>
  );
}

export default function TransfersSection() {
  const { notify } = useNotifications();
  const mounted = useRef(true);

  // Transfer folders (§6.3–6.4).
  const [paths, setPaths] = useState<TransferPaths | null>(null);
  const [pathError, setPathError] = useState<{ outgoing: string | null; working: string | null }>({
    outgoing: null,
    working: null,
  });
  const [savingPaths, setSavingPaths] = useState(false);
  const [browsing, setBrowsing] = useState<'outgoing' | 'working' | null>(null);
  /** Why the cards are missing, when the read itself failed. */
  const [pathsLoadError, setPathsLoadError] = useState<string | null>(null);

  // Transfer storage (B7): the on-disk footprint of the transfer temp data +
  // the one-click "clean up finished transfers" reclaim.
  const [storage, setStorage] = useState<TransferStorage | null>(null);
  const [cleaning, setCleaning] = useState(false);

  // Leftovers (§6.5): what the folders a move left behind still hold.
  const [cleaningLeftovers, setCleaningLeftovers] = useState(false);
  const [leftoverError, setLeftoverError] = useState<string | null>(null);

  // Upload speed limit (W1) — text-backed, not number-backed, so the field can
  // legitimately be empty (= unlimited). `savedUploadMb` is the last persisted
  // value and drives the Save button's dirty state.
  const [uploadMb, setUploadMb] = useState('');
  const [savedUploadMb, setSavedUploadMb] = useState('');
  const [uploadError, setUploadError] = useState<string | null>(null);
  const [savingUpload, setSavingUpload] = useState(false);

  // Simultaneous incoming transfers (W2 T2.7) — same text-backed shape as the
  // upload limit: `savedReceives` is the last persisted value and drives the
  // Save button's dirty state.
  const [receives, setReceives] = useState(DEFAULT_RECEIVES);
  const [savedReceives, setSavedReceives] = useState(DEFAULT_RECEIVES);
  const [receivesError, setReceivesError] = useState<string | null>(null);
  const [savingReceives, setSavingReceives] = useState(false);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const refreshPaths = useCallback(async () => {
    try {
      const p = await api.invoke<TransferPaths>('get_transfer_paths');
      if (mounted.current) {
        setPaths(p);
        setPathsLoadError(null);
      }
    } catch (err) {
      console.error('[transfers] get_transfer_paths failed:', err);
      if (mounted.current) setPathsLoadError(errMsg(err));
    }
  }, []);

  // Transfer-storage footprint — best-effort, degrades to null on failure.
  const refreshStorage = useCallback(async () => {
    try {
      const s = await api.invoke<TransferStorage>('get_transfer_storage');
      if (mounted.current) setStorage(s ?? null);
    } catch (err) {
      console.error('[transfers] transfer storage poll failed:', err);
    }
  }, []);

  useEffect(() => {
    refreshPaths();
    refreshStorage();
    (async () => {
      try {
        const [uploadVal, receivesVal] = await Promise.all([
          // Settings cross the boundary as STRINGS; "0" is the unlimited sentinel.
          api.invoke<string>('get_setting', {
            key: 'sync.max_upload_bytes_per_sec',
            defaultValue: '0',
          }),
          // No dedicated getter command — the cap is read through generic
          // `get_setting`, same default ("2") the backend applies.
          api.invoke<string>('get_setting', {
            key: 'sync.max_concurrent_receives',
            defaultValue: DEFAULT_RECEIVES,
          }),
        ]);
        if (!mounted.current) return;
        const mb = bytesToMbInput(uploadVal ?? '0');
        setUploadMb(mb);
        setSavedUploadMb(mb);
        const lanes = receivesToInput(receivesVal ?? DEFAULT_RECEIVES);
        setReceives(lanes);
        setSavedReceives(lanes);
      } catch (err) {
        console.error('[transfers] load transfer settings failed:', err);
      }
    })();
  }, [refreshPaths, refreshStorage]);

  // `undefined` = leave that folder as it is (the current `configured` value is
  // resent), `null` = reset it to the default, a string = set it. Nothing is
  // written unless BOTH values validate server-side, so one shared busy flag.
  const applyPaths = async (
    outgoing: string | null | undefined,
    working: string | null | undefined,
  ) => {
    if (!paths) return;
    setSavingPaths(true);
    setPathError({ outgoing: null, working: null });
    try {
      const next = await api.invoke<TransferPaths>('set_transfer_paths', {
        outgoing: outgoing === undefined ? paths.outgoing.configured : outgoing,
        working: working === undefined ? paths.working.configured : working,
      });
      if (!mounted.current) return;
      setPaths(next);
      notify({
        kind: 'sync',
        tone: 'success',
        title: 'Transfer folders saved',
        // `NotifyInput.detail` is required, so the non-restart case says what
        // actually happened instead of dropping the line.
        detail: next.working.restartRequired
          ? 'The working folder applies after a restart.'
          : 'The new folders apply to the next transfer.',
      });
      refreshStorage();
    } catch (err) {
      console.error('[transfers] set_transfer_paths failed:', err);
      // The backend prefixes every validation message with the folder's label,
      // which is what routes it to the card that caused it.
      const msg = errMsg(err);
      if (mounted.current) {
        setPathError(
          msg.startsWith('Incoming working folder')
            ? { outgoing: null, working: msg }
            : { outgoing: msg, working: null },
        );
      }
    } finally {
      if (mounted.current) setSavingPaths(false);
    }
  };

  // Desktop gets the native picker; the web build browses the same allowed
  // roots `set_transfer_paths` validates against (scope "scan").
  const choose = async (which: 'outgoing' | 'working') => {
    if (isTauri) {
      try {
        const picked = await pickDirectory();
        if (!picked) return;
        await applyPaths(
          which === 'outgoing' ? picked : undefined,
          which === 'working' ? picked : undefined,
        );
      } catch (err) {
        console.error('[transfers] folder picker failed:', err);
        if (mounted.current) {
          const msg = errMsg(err);
          setPathError(
            which === 'working' ? { outgoing: null, working: msg } : { outgoing: msg, working: null },
          );
        }
      }
    } else {
      setBrowsing(which);
    }
  };

  const handleCleanup = async () => {
    setCleaning(true);
    try {
      const result = await api.invoke<TransferCleanup>('cleanup_finished_transfers');
      // D2: payload dirs are the SEND side, staging trees the RECEIVE side — a
      // receive-only device only ever has the latter, so both are reported. Freed
      // bytes are the two together; released downloads are the delayed half.
      const freedBytes = result.payloadBytes + result.stagingBytes;
      const parts: string[] = [];
      if (result.payloadDirs > 0) {
        parts.push(`${result.payloadDirs} package${result.payloadDirs === 1 ? '' : 's'}`);
      }
      if (result.stagingDirs > 0) {
        parts.push(`${result.stagingDirs} received batch${result.stagingDirs === 1 ? '' : 'es'}`);
      }
      const what = parts.length > 0 ? ` (${parts.join(', ')})` : '';
      const tags =
        result.tagsReleased > 0
          ? `, released ${result.tagsReleased} partial download${result.tagsReleased === 1 ? '' : 's'} — those bytes return within about 15 minutes`
          : '';
      notify({
        title: 'Finished transfers cleaned up',
        detail: `Freed ${formatBytes(freedBytes)}${what}${tags}`,
        kind: 'sync',
        tone: 'success',
      });
      await refreshStorage();
    } catch (err) {
      console.error('[transfers] cleanup finished transfers failed:', err);
      notify({
        title: 'Cleanup failed',
        detail: errMsg(err),
        kind: 'sync',
        tone: 'warning',
      });
    } finally {
      if (mounted.current) setCleaning(false);
    }
  };

  // §6.5. A refusal here is the "transport still bound under a leftover folder"
  // Conflict, whose message IS the restart hint — show it inline, next to the
  // row it belongs to, rather than as a toast that outlives the card.
  const handleCleanupLeftovers = async () => {
    setCleaningLeftovers(true);
    setLeftoverError(null);
    try {
      const freed = await api.invoke<number>('cleanup_transfer_leftovers');
      notify({
        title: 'Previous transfer folders cleaned up',
        detail: `Freed ${formatBytes(freed)}`,
        kind: 'sync',
        tone: 'success',
      });
      await refreshStorage();
    } catch (err) {
      console.error('[transfers] cleanup transfer leftovers failed:', err);
      if (mounted.current) setLeftoverError(errMsg(err));
    } finally {
      if (mounted.current) setCleaningLeftovers(false);
    }
  };

  const handleSaveUploadLimit = async () => {
    const raw = uploadMb.trim();
    // Empty or 0 → unlimited. Anything else must parse and clear the same floor
    // the server enforces, checked here so the common case never round-trips.
    let bytesPerSec = 0;
    if (raw !== '') {
      const mbps = Number(raw);
      if (!Number.isFinite(mbps) || mbps < 0) {
        setUploadError('Enter a number in MB/s, or leave the field empty for unlimited.');
        return;
      }
      if (mbps > 0 && mbps < MIN_LIMIT_MB) {
        setUploadError(
          `Minimum limit is ${MIN_LIMIT_MB} MB/s. Use 0 (or leave empty) for unlimited.`,
        );
        return;
      }
      bytesPerSec = Math.round(mbps * BYTES_PER_MB);
      if (!Number.isSafeInteger(bytesPerSec)) {
        setUploadError('That limit is too large — enter a realistic MB/s value.');
        return;
      }
    }
    setUploadError(null);
    setSavingUpload(true);
    try {
      await api.invoke('set_sync_upload_limit', { bytesPerSec });
      // Canonicalise the field to what was actually stored (0 → empty = Unlimited).
      const shown = bytesToMbInput(String(bytesPerSec));
      if (mounted.current) {
        setUploadMb(shown);
        setSavedUploadMb(shown);
      }
      notify({
        title: 'Upload speed limit saved',
        detail:
          bytesPerSec === 0
            ? 'Sync uploads from this device are unlimited.'
            : `Sync uploads from this device are capped at ${shown} MB/s.`,
        kind: 'sync',
        tone: 'success',
      });
    } catch (err) {
      console.error('[transfers] set upload limit failed:', err);
      const msg = errMsg(err);
      if (mounted.current) setUploadError(msg);
      notify({
        title: 'Could not save upload speed limit',
        detail: msg,
        kind: 'sync',
        tone: 'warning',
      });
    } finally {
      if (mounted.current) setSavingUpload(false);
    }
  };

  const handleSaveConcurrentReceives = async () => {
    const raw = receives.trim();
    const n = Number(raw);
    // Client mirror of the server's 1..=8 guard, so a typo answers inline
    // instead of round-tripping. Whole numbers only — there is no half a lane.
    if (raw === '' || !Number.isFinite(n) || !Number.isInteger(n)) {
      setReceivesError(`Enter a whole number between ${MIN_RECEIVES} and ${MAX_RECEIVES}.`);
      return;
    }
    if (n < MIN_RECEIVES || n > MAX_RECEIVES) {
      setReceivesError(`Must be between ${MIN_RECEIVES} and ${MAX_RECEIVES}.`);
      return;
    }
    setReceivesError(null);
    setSavingReceives(true);
    try {
      await api.invoke('set_sync_max_concurrent_receives', { maxConcurrentReceives: n });
      const shown = String(n);
      if (mounted.current) {
        setReceives(shown);
        setSavedReceives(shown);
      }
      notify({
        title: 'Simultaneous incoming transfers saved',
        detail:
          n === 1
            ? 'Incoming transfers download one at a time.'
            : `Up to ${n} incoming transfers download at once.`,
        kind: 'sync',
        tone: 'success',
      });
    } catch (err) {
      console.error('[transfers] set max concurrent receives failed:', err);
      const msg = errMsg(err);
      if (mounted.current) setReceivesError(msg);
      notify({
        title: 'Could not save simultaneous incoming transfers',
        detail: msg,
        kind: 'sync',
        tone: 'warning',
      });
    } finally {
      if (mounted.current) setSavingReceives(false);
    }
  };

  // ── render ───────────────────────────────────────────────────────────────────

  return (
    <div className="space-y-6">
      {/* Folders (§6.3–6.4): where sends are staged and where downloads are
          verified before they land. */}
      <div>
        <h4 className="text-sm font-medium text-content-secondary mb-2">Folders</h4>
        <div className="space-y-3">
          {paths && (
            <>
              <FolderCard
                title="Outgoing staging folder"
                hint="Prepared sends are staged here until the receiver confirms them."
                setting={paths.outgoing}
                onChoose={() => choose('outgoing')}
                onReset={() => applyPaths(null, undefined)}
                error={pathError.outgoing}
                busy={savingPaths}
              />
              <FolderCard
                title="Incoming working folder"
                hint="Downloads are verified here before landing in your Incoming folder. Same disk as Incoming = no extra copy."
                setting={paths.working}
                onChoose={() => choose('working')}
                onReset={() => applyPaths(undefined, null)}
                error={pathError.working}
                busy={savingPaths}
              />
            </>
          )}
          {!paths && pathsLoadError && (
            <p className="text-xs text-error">
              Could not read the transfer folders: {pathsLoadError}
            </p>
          )}
        </div>
      </div>

      {/* Upload speed limit (W1): one device-wide cap on sync UPLOAD bandwidth.
          Shown in decimal MB/s, stored as bytes/s; empty or 0 = unlimited. */}
      <div>
        <h4 className="text-sm font-medium text-content-secondary mb-2">Upload speed limit</h4>
        <div className="flex items-center gap-3">
          <div className="flex items-center gap-2">
            <input
              type="number"
              inputMode="decimal"
              value={uploadMb}
              onChange={(e) => {
                setUploadMb(e.target.value);
                setUploadError(null);
              }}
              onKeyDown={(e) => {
                if (e.key === 'Enter') handleSaveUploadLimit();
              }}
              step="0.1"
              min="0"
              placeholder="Unlimited"
              aria-label="Upload speed limit in megabytes per second"
              className={`w-32 rounded-md border bg-surface-hover px-2.5 py-1.5 text-sm text-content focus:outline-none focus:border-accent transition-colors ${
                uploadError ? 'border-error' : 'border-border'
              }`}
            />
            <span className="text-sm text-content-muted">MB/s</span>
          </div>
          <button
            type="button"
            onClick={handleSaveUploadLimit}
            disabled={savingUpload || uploadMb.trim() === savedUploadMb}
            className="flex-shrink-0 inline-flex items-center gap-1.5 rounded-md border border-border px-2.5 py-1.5 text-xs text-content-secondary hover:bg-surface-hover disabled:opacity-50 transition-colors"
          >
            {savingUpload ? (
              <Loader2 size={13} className="animate-spin" />
            ) : (
              <Save size={13} />
            )}
            Save
          </button>
        </div>
        {uploadError && <p className="mt-1.5 text-xs text-error">{uploadError}</p>}
        <p className="mt-1.5 text-xs text-content-muted">
          Caps this device's total sync upload bandwidth. Uploads only — downloads are capped by
          the sending device's limit. Empty or <span className="text-content-secondary">0</span>{' '}
          means unlimited.
        </p>
      </div>

      {/* Simultaneous incoming transfers (W2 T2.7): how many inbound transfers
          download at once. Integer 1..=8, live-applied by the receive gate. */}
      <div>
        <h4 className="text-sm font-medium text-content-secondary mb-2">
          Simultaneous incoming transfers
        </h4>
        <div className="flex items-center gap-3">
          <input
            type="number"
            inputMode="numeric"
            value={receives}
            onChange={(e) => {
              setReceives(e.target.value);
              setReceivesError(null);
            }}
            onKeyDown={(e) => {
              if (e.key === 'Enter') handleSaveConcurrentReceives();
            }}
            step="1"
            min={MIN_RECEIVES}
            max={MAX_RECEIVES}
            aria-label="Number of simultaneous incoming transfers"
            className={`w-20 rounded-md border bg-surface-hover px-2.5 py-1.5 text-sm text-content focus:outline-none focus:border-accent transition-colors ${
              receivesError ? 'border-error' : 'border-border'
            }`}
          />
          <button
            type="button"
            onClick={handleSaveConcurrentReceives}
            disabled={savingReceives || receives.trim() === savedReceives}
            className="flex-shrink-0 inline-flex items-center gap-1.5 rounded-md border border-border px-2.5 py-1.5 text-xs text-content-secondary hover:bg-surface-hover disabled:opacity-50 transition-colors"
          >
            {savingReceives ? (
              <Loader2 size={13} className="animate-spin" />
            ) : (
              <Save size={13} />
            )}
            Save
          </button>
        </div>
        {receivesError && <p className="mt-1.5 text-xs text-error">{receivesError}</p>}
        <p className="mt-1.5 text-xs text-content-muted">
          How many incoming transfers download at once. Others wait their turn — transfers from the
          same device always arrive in order. Default{' '}
          <span className="text-content-secondary">2</span>; raise it only if this machine's disk
          keeps up.
        </p>
      </div>

      {/* Transfer storage (B7): footprint + one-click reclaim of finished-transfer temp data. */}
      <div>
        <h4 className="text-sm font-medium text-content-secondary mb-2">Transfer storage</h4>
        <div className="flex items-center justify-between gap-3">
          <p className="text-sm text-content-muted">
            {storage ? (
              <>
                <span className="text-content-secondary">{storage.packagesCount}</span> package
                {storage.packagesCount === 1 ? '' : 's'} ·{' '}
                <span className="text-content-secondary">{formatBytes(storage.packagesBytes)}</span> on
                disk · received{' '}
                <span className="text-content-secondary">{formatBytes(storage.stagingBytes)}</span> ·
                blobs <span className="text-content-secondary">{formatBytes(storage.blobsBytes)}</span>
              </>
            ) : (
              'Calculating…'
            )}
          </p>
          <button
            type="button"
            onClick={handleCleanup}
            disabled={cleaning}
            className="flex-shrink-0 inline-flex items-center gap-1.5 rounded-md border border-border px-2.5 py-1.5 text-xs text-content-secondary hover:bg-surface-hover disabled:opacity-50 transition-colors"
          >
            {cleaning ? (
              <Loader2 size={13} className="animate-spin" />
            ) : (
              <Trash2 size={13} />
            )}
            Clean up finished transfers
          </button>
        </div>
        {/* §6.5: bytes a folder move left behind in the previous / default folders. */}
        {storage && storage.leftoverBytes > 0 && (
          <div className="mt-2 flex items-center justify-between gap-3 text-xs text-content-muted">
            <span>
              Leftovers in previous folders:{' '}
              <span className="text-content-secondary">{formatBytes(storage.leftoverBytes)}</span>
            </span>
            <button
              type="button"
              onClick={handleCleanupLeftovers}
              disabled={cleaningLeftovers}
              className="flex-shrink-0 inline-flex items-center gap-1.5 rounded-md border border-border px-2.5 py-1.5 text-xs text-content-secondary hover:bg-surface-hover disabled:opacity-50 transition-colors"
            >
              {cleaningLeftovers ? (
                <Loader2 size={13} className="animate-spin" />
              ) : (
                <Trash2 size={13} />
              )}
              Clean up
            </button>
          </div>
        )}
        {leftoverError && <p className="mt-1.5 text-xs text-error">{leftoverError}</p>}
        <p className="mt-1.5 text-xs text-content-muted">
          Removes finished transfers' temporary payloads and releases orphaned download data.
          Received files and transfer history are untouched.
        </p>
      </div>

      {/* Web mode: the folder browser walks the same allowed roots
          `set_transfer_paths` validates against. */}
      <FolderBrowserModal
        isOpen={browsing !== null}
        scope="scan"
        onSelect={(path) => {
          const which = browsing;
          setBrowsing(null);
          if (!which) {
            console.error('[transfers] folder selected with no target — dropping', path);
            return;
          }
          void applyPaths(
            which === 'outgoing' ? path : undefined,
            which === 'working' ? path : undefined,
          );
        }}
        onClose={() => setBrowsing(null)}
      />
    </div>
  );
}
