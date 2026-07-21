import { useEffect, useState } from 'react';
import { api } from '../../api';
import { FileTree } from './FileTree';
import { TransferLog } from './TransferLog';
import { TransferDetails } from './TransferDetails';
import { outcomeChipClass, outcomeLabel } from './presentation';
import type { UnifiedRow } from './types';
import type { Direction, TransferEventEntry, TransferFileEntry } from '../../types/models';

type DetailTab = 'files' | 'log' | 'details';

interface TransferDetailProps {
  item: UnifiedRow;
  /** Live per-file overlay, keyed packageKey → relPath → bytes (from the page hook). */
  liveFiles: Map<string, Map<string, { bytesDone: number; bytesTotal: number }>>;
  onClose: () => void;
}

/**
 * The bottom detail pane of the master-detail Transfers page (§D8, tier 2). Tabs
 * Files / Log / Details. For a LIVE row it fetches `list_transfer_files` +
 * `list_transfer_events` by id and re-fetches on the selected row's discrete
 * state transitions (cheap, event-driven — never a poll). For a merged-in
 * HISTORY group with no live id it falls back to the group's per-frame outcome
 * list and a quiet "log available for recent transfers" note.
 */
export function TransferDetail({ item, liveFiles, onClose }: TransferDetailProps) {
  const [tab, setTab] = useState<DetailTab>('files');
  const [files, setFiles] = useState<TransferFileEntry[]>([]);
  const [events, setEvents] = useState<TransferEventEntry[]>([]);
  const [loadingFiles, setLoadingFiles] = useState(false);
  const [loadingEvents, setLoadingEvents] = useState(false);

  const live = item.kind === 'live' ? item.row : null;
  const direction: Direction = live?.kind === 'outbound' ? 'sent' : 'received';
  const id = live?.id;

  // Selection changed to a NON-live (history) row → drop any stale live detail.
  useEffect(() => {
    if (!live) {
      setFiles([]);
      setEvents([]);
    }
  }, [live]);

  // Fetch + re-fetch the live row's files/events. Deps include the selected
  // row's discrete transition signals (raw + display state, finishNonce) — those
  // are exactly when new per-file verdicts and new log events appear, so this is
  // an event-driven refresh, not a poll. Byte-level progress rides the
  // `liveFiles` overlay and needs no re-fetch. StrictMode-safe cancelled flag.
  useEffect(() => {
    if (id == null) return;
    let cancelled = false;
    setLoadingFiles(true);
    setLoadingEvents(true);
    api
      .invoke<TransferFileEntry[]>('list_transfer_files', { direction, id })
      .then((f) => {
        if (!cancelled) setFiles(f);
      })
      .catch((err) => {
        console.error('[TransferDetail] list_transfer_files failed:', err);
        if (!cancelled) setFiles([]);
      })
      .finally(() => {
        if (!cancelled) setLoadingFiles(false);
      });
    api
      .invoke<TransferEventEntry[]>('list_transfer_events', { direction, id })
      .then((e) => {
        if (!cancelled) setEvents(e);
      })
      .catch((err) => {
        console.error('[TransferDetail] list_transfer_events failed:', err);
        if (!cancelled) setEvents([]);
      })
      .finally(() => {
        if (!cancelled) setLoadingEvents(false);
      });
    return () => {
      cancelled = true;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [item.selKey, id, direction, live?.state, live?.displayState, live?.finishNonce]);

  const active = !!live && !live.terminal;
  const liveKey = live ? (live.kind === 'outbound' ? String(live.id) : live.packageId ?? '') : '';
  const liveOverlay = liveKey ? liveFiles.get(liveKey) : undefined;

  const title =
    item.kind === 'live'
      ? item.row.displayName ?? item.row.packageShort
      : item.group.batchName ?? 'Transfer detail';

  // All-duplicate transfer (§D6): every listed file's outcome is `duplicate`, i.e.
  // the peer already held every frame and nothing was re-transferred. Shown as a
  // Files-tab subline. Only fires once outcomes are settled (an in-flight file has
  // a `null` outcome, so a still-moving batch never reads as all-duplicate).
  const fileOutcomes: Array<string | null> =
    item.kind === 'live' ? files.map((f) => f.outcome) : item.group.rows.map((r) => r.outcome);
  const allDuplicate = fileOutcomes.length > 0 && fileOutcomes.every((o) => o === 'duplicate');

  return (
    <div className="flex h-full flex-col">
      <div className="flex shrink-0 items-center justify-between border-b border-border px-3 py-2">
        <div className="flex items-center gap-1">
          {(['files', 'log', 'details'] as const).map((t) => (
            <button
              key={t}
              type="button"
              onClick={() => setTab(t)}
              className={`rounded px-2.5 py-1 text-xs font-medium capitalize transition-colors ${
                tab === t
                  ? 'bg-surface-hover text-content'
                  : 'text-content-muted hover:text-content'
              }`}
            >
              {t}
            </button>
          ))}
        </div>
        <span className="min-w-0 flex items-center gap-2">
          <span className="truncate text-xs text-content-muted" title={title}>
            {title}
          </span>
          <button
            type="button"
            onClick={onClose}
            className="shrink-0 rounded px-2 py-1 text-xs text-content-muted transition-colors hover:bg-surface-hover hover:text-content"
          >
            Close
          </button>
        </span>
      </div>

      <div className="min-h-0 flex-1 overflow-y-auto px-3 py-1">
        {tab === 'files' && (
          <>
            {allDuplicate && (
              <p className="px-1 pt-2 text-xs text-content-muted">
                Peer already had every file — nothing was re-transferred.
              </p>
            )}
            {item.kind === 'live' ? (
              <FileTree entries={files} liveOverlay={liveOverlay} active={active} />
            ) : (
              <HistoryFileList item={item} />
            )}
          </>
        )}
        {tab === 'log' &&
          (item.kind === 'live' ? (
            <TransferLog events={events} loading={loadingEvents} />
          ) : (
            <TransferLog events={[]} loading={false} unavailable />
          ))}
        {tab === 'details' && <TransferDetails item={item} />}
      </div>

      {tab === 'files' && item.kind === 'live' && loadingFiles && files.length === 0 && (
        <p className="shrink-0 px-3 py-1 text-[11px] text-content-muted">Loading files…</p>
      )}
    </div>
  );
}

/** Files tab for a merged-history group with no live row id: the group's
 *  per-frame outcome list (history data — basenames + settled verdicts). */
function HistoryFileList({ item }: { item: Extract<UnifiedRow, { kind: 'history' }> }) {
  const rows = item.group.rows;
  if (rows.length === 0) {
    return <p className="px-1 py-3 text-xs text-content-muted">No file detail.</p>;
  }
  return (
    <ul className="space-y-0.5 py-1">
      {rows.map((r, i) => (
        <li key={`${r.frameUuid}-${i}`} className="flex items-center gap-2 px-1 py-1 text-xs">
          <span className="min-w-0 flex-1 truncate text-content-secondary" title={r.filename}>
            {r.filename}
          </span>
          <span
            className={`shrink-0 rounded px-1.5 py-0.5 text-[10px] font-medium ${outcomeChipClass(r.outcome)}`}
            title={r.outcome}
          >
            {outcomeLabel(r.outcome)}
          </span>
        </li>
      ))}
    </ul>
  );
}
