// Settings → Transfers (task 15; settings redesign Task D3). Four
// `SettingsSection`s now — Folders, Upload speed limit, Simultaneous
// incoming transfers, Transfer storage — the registry supplies each card's
// title/description.
//
// The two Folders cards are new (transfer-prepare spec §6.3–6.5: the outgoing
// staging folder and the incoming working folder). Upload speed limit and
// Simultaneous incoming transfers are `SettingNumber` KV fields now — no
// local Save state, no Save buttons — through `useSettingField`'s own
// draft/blur/Enter/reset discipline (spec §5). Storage keeps its own local
// state (a footprint readout + a cleanup action is not a setting). Sync
// keeps account status + pairing.
//
// Everything on this tab is device-local and account-independent, so it loads
// on mount: the folders must be configurable before this machine is paired.

import { useCallback, useEffect, useRef, useState } from 'react';
import { Loader2, Trash2 } from 'lucide-react';
import { api } from '../../api';
import { pickDirectory } from '../../api/desktop';
import { isTauri } from '../../utils/platform';
import { FolderBrowserModal } from '../FolderBrowserModal';
import { FolderCard } from '../stacking/FolderCard';
import { formatBytes } from '../transfers/presentation';
import { useNotifications } from '../../contexts/NotificationContext';
import { SettingsSection } from './SettingsSection';
import { SettingNumber } from './SettingNumber';
import { intCodec, type Codec } from '../../settings/codecs';
import type {
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

/**
 * A `Codec<number>` over decimal MB/s — the field's own draft/default text is
 * in MB/s, but the wire value (both the generic KV default and what a custom
 * `read`/`write` override exchanges) is BYTES/s. `parse`/`format` work in
 * MB/s throughout; the `read`/`write` overrides below do the bytes↔MB
 * conversion, so the codec never sees a raw bytes string except through the
 * `defaults.kv` fallback — which is exact only because the default is `0`
 * (unlimited) either way. Empty/`0` = unlimited, matching the pre-redesign
 * field's semantics exactly.
 */
const uploadMbCodec: Codec<number> = {
  parse(raw) {
    const trimmed = raw.trim();
    if (trimmed === '') return 0;
    const n = Number(trimmed);
    if (!Number.isFinite(n) || n < 0) {
      return new Error('Enter a number in MB/s, or leave the field empty for unlimited.');
    }
    if (n > 0 && n < MIN_LIMIT_MB) {
      return new Error(`Minimum limit is ${MIN_LIMIT_MB} MB/s. Use 0 (or leave empty) for unlimited.`);
    }
    if (!Number.isSafeInteger(Math.round(n * BYTES_PER_MB))) {
      return new Error('That limit is too large — enter a realistic MB/s value.');
    }
    return n;
  },
  format(value) {
    return value === 0 ? '' : String(value);
  },
};

// Simultaneous incoming transfers (W2 T2.7). `sync.max_concurrent_receives` is
// stored as a plain integer string; the server accepts 1..=8 and the receiver's
// getter clamps into the same window.
const MIN_RECEIVES = 1;
const MAX_RECEIVES = 8;

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

  // ── render ───────────────────────────────────────────────────────────────────

  return (
    <>
      {/* Folders (§6.3–6.4): where sends are staged and where downloads are
          verified before they land. */}
      <SettingsSection id="transfers.folders">
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
      </SettingsSection>

      {/* Upload speed limit (W1): one device-wide cap on sync UPLOAD bandwidth.
          Shown in decimal MB/s, stored as bytes/s; empty or 0 = unlimited. */}
      <SettingsSection id="transfers.upload">
        <SettingNumber
          section="transfers.upload"
          field="limit"
          settingKey="sync.max_upload_bytes_per_sec"
          codec={uploadMbCodec}
          unit="MB/s"
          step={0.1}
          min={0}
          placeholder="Unlimited"
          write={async (mbps) => {
            const bytesPerSec = mbps <= 0 ? 0 : Math.round(mbps * BYTES_PER_MB);
            await api.invoke('set_sync_upload_limit', { bytesPerSec });
          }}
          read={async () => {
            const raw = await api.invoke<string>('get_setting', {
              key: 'sync.max_upload_bytes_per_sec',
              defaultValue: '0',
            });
            return bytesToMbInput(raw ?? '0');
          }}
        />
      </SettingsSection>

      {/* Simultaneous incoming transfers (W2 T2.7): how many inbound transfers
          download at once. Integer 1..=8, live-applied by the receive gate. */}
      <SettingsSection id="transfers.receiving">
        <SettingNumber
          section="transfers.receiving"
          field="concurrent"
          settingKey="sync.max_concurrent_receives"
          codec={intCodec(MIN_RECEIVES, MAX_RECEIVES)}
          min={MIN_RECEIVES}
          max={MAX_RECEIVES}
          step={1}
          write={(n) => api.invoke('set_sync_max_concurrent_receives', { maxConcurrentReceives: n })}
        />
      </SettingsSection>

      {/* Transfer storage (B7): footprint + one-click reclaim of finished-transfer temp data. */}
      <SettingsSection id="transfers.storage">
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
      </SettingsSection>

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
    </>
  );
}
