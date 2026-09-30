import { Fragment, useEffect, useRef, useState, type ReactNode } from 'react';
import { Link } from 'react-router-dom';
import { Copy, Loader2 } from 'lucide-react';
import { api } from '../../../api';
import { formatTimestamp } from '../../../utils/dateFormatting';
import { Button, Chip, EmptyState, FilterDot, KV, MemberDot, SidePanel, StatusDot } from '../../ui';
import { formatSize } from '../format';
import type { CollabPeersChanged, FrameHolderView } from '../../../types/models';
import { COLUMNS, WEEKDAY, effectiveStatus, statusTone, type FrameVM } from './frames';
import { useMemberColor } from './MemberColorsContext';
import ExcludeDialog from './ExcludeDialog';

type Holders = 'loading' | 'error' | FrameHolderView[];

const nightLabel = (n: string) => `${n} · ${WEEKDAY[new Date(`${n}T00:00:00Z`).getUTCDay()]}`;

/**
 * Docked frame detail panel (wave 5.5 Task 8), beside any collab project
 * table. Escape is owned by SidePanel's overlay stack. Read-only except
 * for the Restore/Exclude actions gated on `canModerate` (coordinator, or an
 * account with the hub's `data.moderate` capability) and the local-path copy
 * button — everything else is a straight `FrameVM` read.
 */
export default function FramePanel({
  projectId,
  frame,
  canModerate,
  onClose,
  onChanged,
  thresholdsVersion,
}: {
  projectId: string;
  frame: FrameVM;
  canModerate: boolean;
  onClose: () => void;
  onChanged: () => void;
  thresholdsVersion: number | null;
}) {
  const [holders, setHolders] = useState<Holders>('loading');
  const [restoring, setRestoring] = useState(false);
  const [restoreError, setRestoreError] = useState<string | null>(null);
  const [excludeOpen, setExcludeOpen] = useState(false);

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

  const colorOf = useMemberColor();
  const H3 = 'mb-1.5 mt-4 text-[13px] font-semibold text-content';
  const status = frame.own
    ? frame.own.segment === 'held' ? <Chip tone="warn">held back</Chip>
      : frame.own.segment === 'ready' ? <Chip tone="info">ready</Chip>
      : <Chip tone={statusTone(effectiveStatus(frame))}>{effectiveStatus(frame)}</Chip>
    : <><Chip tone={statusTone(effectiveStatus(frame))}>{effectiveStatus(frame)}</Chip> {COLUMNS.device.cell(frame)}</>;
  const preconditions: [string, boolean][] = frame.own ? [
    ['Plate-solved', !frame.own.failures.some((f) => f.kind === 'solve')],
    ['Analyzed', !frame.own.failures.some((f) => f.kind === 'analyze')],
    ['Filter mapped', frame.filterMapped],
  ] : [];

  return (
    <SidePanel
      label="Frame details"
      onClose={onClose}
      title={<>
        <div className="break-all font-mono text-[12.5px] text-content">{frame.fileName}</div>
        <div className="mt-1.5 flex flex-wrap items-center gap-1.5">{status}</div>
      </>}
    >
      <h3 className={H3}>Frame</h3>
      <KV items={[
        ...(frame.publisher ? [['Publisher', <span className="inline-flex items-center gap-[5px]"><MemberDot color={colorOf(frame.publisherAccountId ?? frame.publisher)} />{frame.publisher}</span>] as [ReactNode, ReactNode]] : []),
        ...(frame.night ? [['Night', nightLabel(frame.night)] as [ReactNode, ReactNode]] : []),
        ['Filter', <span className="inline-flex items-center"><FilterDot filter={frame.filter} />{frame.filter}{!frame.filterMapped && <span className="ml-1 text-content-faint">(unmapped)</span>}</span>],
        ['Camera', frame.camera === '' ? 'Unknown camera' : frame.camera],
        ...(frame.exptimeSec !== null ? [['Exposure', `${frame.exptimeSec} s`] as [ReactNode, ReactNode]] : []),
        ...(frame.byteSize !== null ? [['Size', formatSize(frame.byteSize)] as [ReactNode, ReactNode]] : []),
        ...(frame.contentVersion !== null ? [['Version', frame.contentVersion > 1 ? `v${frame.contentVersion} · v${frame.contentVersion - 1} superseded` : `v${frame.contentVersion}`] as [ReactNode, ReactNode]] : []),
        ...(frame.publishedAt ? [['Published', formatTimestamp(frame.publishedAt)] as [ReactNode, ReactNode]] : []),
        ...(frame.own ? [['Object', <>{frame.setName ?? '—'}{frame.setId !== null && <> · <Link to={`/objects/${frame.setId}`} className="text-accent hover:underline">Open object</Link></>}</>] as [ReactNode, ReactNode]] : []),
      ]} />

      <h3 className={H3}>Metrics</h3>
      <KV items={[
        ['FWHM', frame.fwhm !== null ? `${frame.fwhm.toFixed(2)}″` : '—'],
        ['Eccentricity', frame.ecc !== null ? frame.ecc.toFixed(2) : '—'],
        ['Stars', frame.stars !== null ? String(frame.stars) : '—'],
        ['SNR', frame.snr !== null ? frame.snr.toFixed(1) : '—'],
      ]} />

      {frame.own && (<>
        <h3 className={H3}>Gate {thresholdsVersion !== null && <span className="font-normal text-content-faint">thresholds v{thresholdsVersion}</span>}</h3>
        <div className="grid grid-cols-[1fr_auto_auto_auto] items-center gap-x-3 gap-y-[3px] text-[12px]">
          <span className="text-[11px] text-content-faint">Rule</span><span className="text-[11px] text-content-faint">Value</span><span className="text-[11px] text-content-faint">Needs</span><span />
          {frame.own.rules.map((r) => (
            <Fragment key={r.metricKey}>
              <span className="text-content-secondary">{r.label}</span>
              <span className="text-content-secondary">{r.value ?? '—'}</span>
              <span className="text-content-faint">{r.needs}</span>
              <span className={r.pass === true ? 'text-success' : r.pass === false ? 'text-error' : 'text-content-faint'}>{r.pass === true ? '✓' : r.pass === false ? '✕' : '—'}</span>
            </Fragment>
          ))}
          {preconditions.map(([label, ok]) => (
            <Fragment key={label}>
              <span className="text-content-secondary">{label}</span>
              <span className="text-content-secondary">{ok ? 'yes' : 'no'}</span>
              <span className="text-content-faint">required</span>
              <span className={ok ? 'text-success' : 'text-error'}>{ok ? '✓' : '✕'}</span>
            </Fragment>
          ))}
        </div>
        {frame.own.lastError && <p className="mt-1.5 text-[12px] text-error">{frame.own.lastError}</p>}
      </>)}

      {holdersFor && (<>
        <h3 className={H3}>Who holds it {Array.isArray(holders) && <span className="font-normal text-content-faint">{holders.filter((h) => h.online).length} online of {holders.length}</span>}</h3>
        {holders === 'loading' && <EmptyState>Loading…</EmptyState>}
        {holders === 'error' && <p className="text-[12.5px] text-error">Could not load holders — see console.</p>}
        {Array.isArray(holders) && holders.length === 0 && <EmptyState>Nobody else holds it yet.</EmptyState>}
        {Array.isArray(holders) && holders.map((h, i) => (
          <div key={i} className="flex items-center gap-2 py-0.5 text-[12.5px]">
            <StatusDot state={h.online ? 'online' : 'offline'} />
            <MemberDot color={colorOf(h.memberName)} />
            <span className="text-content-secondary">{h.memberName ?? 'Unknown member'}</span>
            {h.isPublisher && <Chip tone="mute">publisher</Chip>}
            {h.contentVersion !== frame.contentVersion && <Chip tone="warn">v{h.contentVersion}</Chip>}
            <span className="ml-auto text-content-faint">{h.deviceName ?? h.deviceShort}</span>
          </div>
        ))}
      </>)}

      {(frame.own?.path || frame.lib?.receivedAt) && <h3 className={H3}>On this device</h3>}
      {frame.own?.path && (
        <div className="flex items-start gap-1.5">
          <span className="break-all font-mono text-[11.5px] text-content-faint">{frame.own.path}</span>
          <Button size="sm" aria-label="Copy path" onClick={copyPath}><Copy size={11} /></Button>
        </div>
      )}
      {frame.lib?.receivedAt && (
        <p className="text-[12px] text-content-muted">Received {formatTimestamp(frame.lib.receivedAt)} from {frame.lib.receivedFromMember ?? (frame.lib.receivedFromDevice === 'local' ? "this device's files" : (frame.lib.receivedFromDevice?.slice(0, 8) ?? 'unknown'))}</p>
      )}

      {showExclusionSection && (
        <div className="mt-4">
          {frame.excluded ? (
            <div className="rounded border border-warning/40 bg-warning-muted px-2.5 py-2 text-[12px] text-warning">
              Excluded — {frame.acceptedReason}
              {canModerate && <div className="mt-1.5"><Button onClick={() => void doRestore()} disabled={restoring}>{restoring && <Loader2 size={12} className="animate-spin" />}Restore</Button></div>}
              {restoreError && <p className="mt-1 text-error">{restoreError}</p>}
            </div>
          ) : (
            <Button variant="danger" onClick={() => setExcludeOpen(true)}>Exclude…</Button>
          )}
        </div>
      )}
      {excludeOpen && <ExcludeDialog projectId={projectId} frames={[frame]} onClose={() => setExcludeOpen(false)} onDone={() => onChanged()} />}
    </SidePanel>
  );
}
