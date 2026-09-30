import { useEffect, useRef, useState } from 'react';
import { Link } from 'react-router-dom';
import { Copy, Loader2, X } from 'lucide-react';
import { api } from '../../../api';
import { formatTimestamp } from '../../../utils/dateFormatting';
import { getFilterColor } from '../../../utils/filterColors';
import { formatBytes } from '../format';
import type { CollabPeersChanged, FrameHolderView } from '../../../types/models';
import type { FrameVM } from './frames';
import ExcludeDialog from './ExcludeDialog';

type Holders = 'loading' | 'error' | FrameHolderView[];

/**
 * Right-side frame detail panel (spec 2026-09-30 Task 9). Opens from a
 * clicked row of any collab project table (Tasks 10, 11). Read-only except
 * for the Restore/Exclude actions gated on `canModerate` (coordinator, or an
 * account with the hub's `data.moderate` capability) and the local-path copy
 * button — everything else is a straight `FrameVM` read.
 */
export default function FrameDrawer({
  projectId,
  frame,
  canModerate,
  onClose,
  onChanged,
}: {
  projectId: string;
  frame: FrameVM;
  canModerate: boolean;
  onClose: () => void;
  onChanged: () => void;
}) {
  const [holders, setHolders] = useState<Holders>('loading');
  const [restoring, setRestoring] = useState(false);
  const [restoreError, setRestoreError] = useState<string | null>(null);
  const [excludeOpen, setExcludeOpen] = useState(false);

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== 'Escape') return;
      // The dialog owns Escape while it is open (its own reason and its own
      // partial-failure message would otherwise vanish with the drawer).
      if (excludeOpen) return;
      onClose();
    };
    document.addEventListener('keydown', onKey);
    return () => document.removeEventListener('keydown', onKey);
  }, [onClose, excludeOpen]);

  // Holders exist only for a frame in this device's project mirror: an own
  // frame not yet published has a uuid but nothing to ask about.
  const holdersFor = frame.hasProjectRow ? frame.frameUuid : null;

  useEffect(() => {
    const frameUuid = holdersFor;
    if (!frameUuid) {
      setHolders([]);
      return;
    }
    let cancelled = false;
    setHolders('loading');
    api
      .invoke<FrameHolderView[]>('get_collab_frame_holders', { projectId, frameUuid })
      .then((rows) => {
        if (!cancelled) setHolders(rows);
      })
      .catch((err) => {
        // Never swallow: log first, then show the inline error state.
        console.error('[drawer] holders failed:', err);
        if (!cancelled) setHolders('error');
      });
    return () => {
      cancelled = true;
    };
  }, [projectId, holdersFor]);

  // Live presence/holder changes (core: `collab-peers-changed`, throttled to
  // one per project per second) re-read holders for the still-open frame.
  // Refs let the listener subscribe once (StrictMode-safe, CLAUDE.md
  // pattern) while always reading the latest project/frame; `holders` is
  // only replaced once the new rows arrive, so this never flashes back
  // through "Loading holders…".
  const projectIdRef = useRef(projectId);
  projectIdRef.current = projectId;
  const frameUuidRef = useRef(holdersFor);
  frameUuidRef.current = holdersFor;

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<CollabPeersChanged>('collab-peers-changed', (p) => {
        if (cancelled || p.projectId !== projectIdRef.current) return;
        const frameUuid = frameUuidRef.current;
        if (!frameUuid) return;
        api
          .invoke<FrameHolderView[]>('get_collab_frame_holders', {
            projectId: projectIdRef.current,
            frameUuid,
          })
          .then((rows) => {
            if (!cancelled) setHolders(rows);
          })
          .catch((err) => {
            // Never swallow — log, but keep the currently-shown rows rather
            // than replacing them with an error state on a transient refresh
            // failure (the initial load's own failure still sets 'error').
            console.error('[drawer] holders refresh failed:', err);
          });
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[drawer] collab-peers-changed listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, []);

  const doRestore = async () => {
    const frameUuid = frame.frameUuid;
    if (!frameUuid) return;
    setRestoring(true);
    setRestoreError(null);
    try {
      await api.invoke('restore_collab_frame', { projectId, frameUuid });
      onChanged();
    } catch (err) {
      console.error('[drawer] restore failed:', err);
      setRestoreError(err instanceof Error ? err.message : String(err));
    } finally {
      setRestoring(false);
    }
  };

  const copyPath = () => {
    const path = frame.own?.path;
    if (!path) return;
    try {
      navigator.clipboard
        .writeText(path)
        .catch((err) => console.error('[drawer] copy path failed:', err));
    } catch (err) {
      console.error('[drawer] copy path failed:', err);
    }
  };

  const showExclusionSection = frame.excluded || (canModerate && frame.pubState === 'published');

  return (
    <aside aria-label="Frame details" className="fixed inset-y-0 right-0 z-40 w-[26rem] max-w-[90vw] overflow-y-auto border-l border-border bg-surface-elevated p-4">
      <div className="flex items-start justify-between gap-2">
        <h2 className="break-all font-mono text-sm text-content">{frame.fileName}</h2>
        <button
          onClick={onClose}
          aria-label="Close"
          className="shrink-0 text-content-muted transition-colors hover:text-content"
        >
          <X size={18} />
        </button>
      </div>

      {/* Identity */}
      <section className="mt-3 space-y-1 text-sm">
        <div className="flex items-center gap-1 text-content-secondary">
          <span
            className="inline-block h-2 w-2 rounded-full"
            style={{ backgroundColor: getFilterColor(frame.filter) }}
          />
          <span>
            {frame.filter}
            {!frame.filterMapped && ' (unmapped)'}
          </span>
        </div>
        <div className="text-content-secondary">{frame.camera === '' ? 'Unknown camera' : frame.camera}</div>
        {frame.night && <div className="text-content-secondary">{frame.night}</div>}
        {frame.exptimeSec !== null && <div className="text-content-secondary">{frame.exptimeSec}s</div>}
        {frame.byteSize !== null && <div className="text-content-secondary">{formatBytes(frame.byteSize)}</div>}
        {frame.publisher && <div className="text-content-secondary">Publisher: {frame.publisher}</div>}
        {frame.own && (
          <div className="text-content-secondary">
            Object: {frame.setName ?? '—'}
            {frame.setId !== null && (
              <>
                {' · '}
                <Link to={`/objects/${frame.setId}`} className="text-accent hover:underline">
                  Open object
                </Link>
              </>
            )}
          </div>
        )}
        {frame.own?.path && (
          <div className="flex items-start gap-1.5">
            <span className="break-all font-mono text-xs text-content-muted">{frame.own.path}</span>
            <button
              onClick={copyPath}
              aria-label="Copy path"
              className="mt-0.5 shrink-0 text-content-muted transition-colors hover:text-content"
            >
              <Copy size={12} />
            </button>
          </div>
        )}
      </section>

      {/* Metrics */}
      <section className="mt-4">
        <h3 className="mb-1 text-xs font-semibold uppercase text-content-muted">Metrics</h3>
        <div className="grid grid-cols-2 gap-x-2 gap-y-0.5 text-sm">
          <span className="text-content-muted">FWHM</span>
          <span>{frame.fwhm !== null ? `${frame.fwhm.toFixed(2)}″` : '—'}</span>
          <span className="text-content-muted">Ecc</span>
          <span>{frame.ecc !== null ? frame.ecc.toFixed(2) : '—'}</span>
          <span className="text-content-muted">Stars</span>
          <span>{frame.stars ?? '—'}</span>
          <span className="text-content-muted">SNR</span>
          <span>{frame.snr ?? '—'}</span>
        </div>
      </section>

      {/* Gate (own frames) */}
      {frame.own && (
        <section className="mt-4">
          <h3 className="mb-1 text-xs font-semibold uppercase text-content-muted">Gate</h3>
          {frame.own.failures
            .filter((f) => f.kind !== 'threshold')
            .map((f, i) => (
              <div key={`${f.kind}-${i}`} className="text-sm text-error">
                ✕ {f.text}
              </div>
            ))}
          {frame.own.rules.length === 0 ? (
            <p className="mt-1 text-sm text-content-muted">No quality rules set.</p>
          ) : (
            <table className="mt-1 w-full text-xs">
              <thead>
                <tr className="text-left text-content-muted">
                  <th className="pr-2 font-normal">Rule</th>
                  <th className="pr-2 font-normal">Value</th>
                  <th className="pr-2 font-normal">Needs</th>
                  <th className="font-normal" />
                </tr>
              </thead>
              <tbody>
                {frame.own.rules.map((r) => (
                  <tr key={r.metricKey}>
                    <td className="pr-2 py-0.5 text-content">{r.label}</td>
                    <td className="pr-2 py-0.5 text-content">{r.value ?? '—'}</td>
                    <td className="pr-2 py-0.5 text-content-muted">{r.needs}</td>
                    <td className="py-0.5">
                      {r.pass === true && <span className="text-success">✓</span>}
                      {r.pass === false && <span className="text-error">✕</span>}
                      {r.pass === null && (
                        <span className="text-content-muted" title="not evaluated — see above">
                          —
                        </span>
                      )}
                    </td>
                  </tr>
                ))}
              </tbody>
            </table>
          )}
          {frame.own.pubState === 'published' && (
            <div className="mt-2 space-y-0.5 text-xs text-content-muted">
              {frame.own.contentVersion !== null && <div>v{frame.own.contentVersion}</div>}
              {frame.own.publishedAt && <div>Published {formatTimestamp(frame.own.publishedAt)}</div>}
              {frame.own.lastError && <div className="text-error">{frame.own.lastError}</div>}
            </div>
          )}
        </section>
      )}

      {/* Exclusion (announced frames) */}
      {showExclusionSection && (
        <section className="mt-4">
          {frame.excluded ? (
            <div className="rounded border border-warning/40 bg-warning/10 p-2 text-sm text-warning">
              Excluded — {frame.acceptedReason}
              {canModerate && (
                <div className="mt-1.5">
                  <button
                    type="button"
                    onClick={() => void doRestore()}
                    disabled={restoring}
                    className="inline-flex items-center gap-1 rounded border border-border px-2 py-1 text-xs text-content-secondary transition-colors hover:bg-surface-hover disabled:opacity-50"
                  >
                    {restoring && <Loader2 size={12} className="animate-spin" />} Restore
                  </button>
                </div>
              )}
              {restoreError && <p className="mt-1 text-xs text-error">{restoreError}</p>}
            </div>
          ) : (
            <button
              type="button"
              onClick={() => setExcludeOpen(true)}
              className="rounded border border-error/50 px-2 py-1 text-xs text-error transition-colors hover:bg-error/10"
            >
              Exclude…
            </button>
          )}
        </section>
      )}

      {/* Who holds it */}
      {holdersFor && (
        <section className="mt-4">
          <h3 className="mb-1 text-xs font-semibold uppercase text-content-muted">Who holds it</h3>
          {holders === 'loading' && <p className="text-sm text-content-muted">Loading holders…</p>}
          {holders === 'error' && <p className="text-sm text-error">Could not load holders.</p>}
          {Array.isArray(holders) && holders.length === 0 && (
            <p className="text-sm text-content-muted">Nobody else holds it yet.</p>
          )}
          {Array.isArray(holders) && holders.length > 0 && (
            <ul className="space-y-1">
              {holders.map((h, i) => (
                <li key={i} className="flex items-center gap-1.5 text-sm">
                  <span
                    className={`h-1.5 w-1.5 shrink-0 rounded-full ${h.online ? 'bg-success' : 'bg-border'}`}
                  />
                  <span className="text-content">{h.memberName ?? 'Unknown member'}</span>
                  <span className="text-content-muted">{h.deviceName ?? h.deviceShort}</span>
                  {h.isPublisher && (
                    <span className="rounded bg-surface-hover px-1 py-0.5 text-[10px] text-content-muted">
                      publisher
                    </span>
                  )}
                  <span className="ml-auto text-xs text-content-muted">v{h.contentVersion}</span>
                </li>
              ))}
            </ul>
          )}
        </section>
      )}

      {/* Provenance (library frames) */}
      {frame.lib?.receivedAt && (
        <section className="mt-4 text-sm text-content-muted">
          Received {formatTimestamp(frame.lib.receivedAt)} from{' '}
          {frame.lib.receivedFromMember ??
            (frame.lib.receivedFromDevice === 'local'
              ? "this device's files"
              : (frame.lib.receivedFromDevice?.slice(0, 8) ?? 'unknown'))}
        </section>
      )}

      {excludeOpen && (
        <ExcludeDialog
          projectId={projectId}
          frames={[frame]}
          onClose={() => setExcludeOpen(false)}
          onDone={() => onChanged()}
        />
      )}
    </aside>
  );
}
