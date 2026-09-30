import { useCallback, useEffect, useId, useRef, useState, type ReactNode } from 'react';
import { AlertTriangle, FileQuestion, FileWarning, Trash2 } from 'lucide-react';
import { api } from '../../api';
import { useNotifications } from '../../contexts/NotificationContext';
import { ConfirmDialog } from '../ConfirmDialog';
import { formatTimestamp } from '../../utils/dateFormatting';
import type {
  ChangedFileOutcome,
  ChangedFileView,
  CollabAttention as Attention,
  CollabAttentionChanged,
  LastCopyView,
} from '../../types/models';

/**
 * The project's replicas that need the user (spec §9.4, L4–L6), above the
 * Receive tab's frame list; each list is hidden while empty:
 *
 * - **Changed files** (L5) — a replica edited in place, set aside and no
 *   longer served: "Re-fetch original" (the edited file goes to the system
 *   trash, else is deleted after confirmation) or "Delete".
 * - **Waiting for your choice** (L4) — a mass or repeated deletion: "Re-fetch"
 *   or "Stop keeping", per frame and for all. "Stop keeping" shows the
 *   last-copy warning first when a frame has fewer than 2 other holders.
 * - **Not kept** (L6) — reversible: "Keep again", per frame and for all.
 * - **Other files** — files in the folder that no frame references; listed,
 *   never deleted.
 *
 * Reloads on mount and on `collab-attention-changed` for this project.
 * Per-row buttons are named with their file ("Re-fetch c_b.fits"), bulk
 * buttons say "all"; the list under an open confirmation is `aria-hidden`.
 */

/** Core's refusal when the system trash cannot take the changed file
 *  (`storage_task::TRASH_UNAVAILABLE`). */
const TRASH_UNAVAILABLE = 'trash_unavailable';

/** At-risk frames named in the last-copy warning; the rest are counted, so
 *  the confirm's buttons stay on screen however many frames are dropped. */
const LAST_COPY_NAMED = 10;

function errMsg(err: unknown): string {
  return err instanceof Error ? err.message : String(err);
}

interface Confirmation {
  title: string;
  message: string;
  confirmText: string;
  onConfirm: () => void;
}

const SMALL_BTN =
  'rounded border border-border px-2 py-0.5 text-xs text-content-secondary transition-colors hover:bg-surface-hover disabled:cursor-not-allowed disabled:opacity-50';
const SMALL_BTN_DANGER =
  'rounded border border-error/50 px-2 py-0.5 text-xs text-error transition-colors hover:bg-error/10 disabled:cursor-not-allowed disabled:opacity-50';

export default function CollabAttention({ projectId }: { projectId: string }) {
  const { notify } = useNotifications();
  const [data, setData] = useState<Attention | null>(null);
  const [loadError, setLoadError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirm, setConfirm] = useState<Confirmation | null>(null);
  const mounted = useRef(true);
  const loadSeq = useRef(0);

  useEffect(() => {
    mounted.current = true;
    return () => {
      mounted.current = false;
    };
  }, []);

  const load = useCallback(async () => {
    const seq = ++loadSeq.current;
    try {
      const a = await api.invoke<Attention>('list_collab_attention', { projectId });
      if (!mounted.current || seq !== loadSeq.current) return;
      setData(a);
      setLoadError(null);
    } catch (err) {
      console.error('[collab] list_collab_attention failed:', err);
      if (mounted.current && seq === loadSeq.current) setLoadError(errMsg(err));
    }
  }, [projectId]);

  useEffect(() => {
    void load();
  }, [load]);

  useEffect(() => {
    let cancelled = false;
    let unlisten: (() => void) | undefined;
    api
      .listen<CollabAttentionChanged>('collab-attention-changed', (p) => {
        if (cancelled || p.projectId !== projectId) return;
        void load();
      })
      .then((fn) => {
        if (cancelled) fn();
        else unlisten = fn;
      })
      .catch((err) => console.error('[collab] attention-changed listen failed:', err));
    return () => {
      cancelled = true;
      unlisten?.();
    };
  }, [projectId, load]);

  const failed = (title: string, err: unknown) => {
    console.error(`[collab] ${title}:`, err);
    notify({
      title,
      detail: errMsg(err),
      kind: 'project',
      tone: 'warning',
      hasErrors: true,
      link: `/projects/${projectId}?tab=library`,
    });
  };

  /** Runs one action with the buttons disabled, then re-reads the lists. */
  const run = async (title: string, action: () => Promise<void>) => {
    setBusy(true);
    try {
      await action();
    } catch (err) {
      failed(title, err);
    } finally {
      if (mounted.current) setBusy(false);
      void load();
    }
  };

  const ask = (c: Confirmation) => setConfirm(c);
  const confirmed = (fn: () => void) => () => {
    setConfirm(null);
    fn();
  };

  // ── Waiting for your choice ────────────────────────────────────────────
  const refetch = (frameUuids: string[] | null) =>
    run('Re-fetch failed', async () => {
      await api.invoke('resolve_collab_deletions', { projectId, frameUuids, action: 'refetch' });
    });

  const dropFrames = (frameUuids: string[]) =>
    run('Stop keeping failed', async () => {
      await api.invoke('resolve_collab_deletions', { projectId, frameUuids, action: 'stopKeeping' });
    });

  /** The last-copy warning (L4) before "Stop keeping": the frames the user is
   *  warned about are exactly the ones dropped. */
  const stopKeeping = async (frameUuids: string[]) => {
    if (frameUuids.length === 0) return;
    setBusy(true);
    let preview: LastCopyView[];
    try {
      preview = await api.invoke<LastCopyView[]>('preview_collab_stop_keeping', { projectId, frameUuids });
    } catch (err) {
      failed('Stop keeping failed', err);
      if (mounted.current) setBusy(false);
      return;
    }
    if (mounted.current) setBusy(false);
    const atRisk = preview.filter((r) => r.atRisk);
    if (atRisk.length === 0) {
      void dropFrames(frameUuids);
      return;
    }
    const lines = atRisk
      .slice(0, LAST_COPY_NAMED)
      .map((r) => `${r.fileName} — fewer than 2 other copies: ${r.holdersOnline} online, ${r.holdersTotal} in total`);
    if (atRisk.length > LAST_COPY_NAMED) {
      lines.push(`…and ${atRisk.length - LAST_COPY_NAMED} more (${atRisk.length} at risk in total)`);
    }
    ask({
      title: frameUuids.length === 1 ? 'Stop keeping this frame?' : `Stop keeping ${frameUuids.length} frames?`,
      message: `Fewer than 2 other members keep ${atRisk.length === 1 ? 'this frame' : 'these frames'}, offline members included. If those copies go too, the frame is lost.\n\n${lines.join('\n')}`,
      confirmText: 'Stop keeping',
      // The FULL previewed list — the names above are only the first ten.
      onConfirm: confirmed(() => void dropFrames(frameUuids)),
    });
  };

  // ── Changed files ──────────────────────────────────────────────────────
  const refetchOriginal = (row: ChangedFileView, confirmedDelete: boolean) =>
    run('Re-fetch original failed', async () => {
      let out: ChangedFileOutcome;
      try {
        out = await api.invoke<ChangedFileOutcome>('resolve_collab_changed_file', {
          projectId,
          frameUuid: row.frameUuid,
          action: 'refetchOriginal',
          confirmedDelete,
        });
      } catch (err) {
        if (!confirmedDelete && errMsg(err).includes(TRASH_UNAVAILABLE)) {
          // Not a failure yet: the user decides whether to delete instead.
          console.warn('[collab] no system trash for the changed file; asking to delete:', err);
          ask({
            title: 'No system trash',
            message: 'The system trash is not available. Delete the changed file and re-fetch the original?',
            confirmText: 'Delete and re-fetch',
            onConfirm: confirmed(() => void refetchOriginal(row, true)),
          });
          return;
        }
        throw err;
      }
      notify({
        title: out.trashed
          ? `${row.fileName} moved to the Trash; the original is being re-fetched`
          : `${row.fileName} was deleted; the original is being re-fetched`,
        detail: row.path,
        kind: 'project',
        tone: 'success',
        link: `/projects/${projectId}?tab=library`,
      });
    });

  const deleteChanged = (row: ChangedFileView) =>
    ask({
      title: `Delete ${row.fileName}`,
      message: 'Delete the changed file? It will not be re-fetched.',
      confirmText: 'Delete',
      onConfirm: confirmed(
        () =>
          void run('Delete failed', async () => {
            await api.invoke<ChangedFileOutcome>('resolve_collab_changed_file', {
              projectId,
              frameUuid: row.frameUuid,
              action: 'delete',
              confirmedDelete: true,
            });
            notify({
              title: `${row.fileName} was deleted`,
              detail: 'It is listed under Not kept — "Keep again" fetches it once more.',
              kind: 'project',
              tone: 'success',
              link: `/projects/${projectId}?tab=library`,
            });
          }),
      ),
    });

  // ── Not kept ───────────────────────────────────────────────────────────
  const keepAgain = (frameUuids: string[] | null) =>
    run('Keep again failed', async () => {
      await api.invoke('keep_collab_frames_again', { projectId, frameUuids });
    });

  if (!data) {
    return loadError ? (
      <p className="text-sm text-error">Could not load the files that need your attention — see console.</p>
    ) : null;
  }
  const { changed, awaitingChoice, notKept, otherFiles } = data;
  if (changed.length + awaitingChoice.length + notKept.length + otherFiles.length === 0) return null;

  return (
    <div className="space-y-3">
      <div className="space-y-3" aria-hidden={confirm ? true : undefined}>
        {changed.length > 0 && (
          <Section
            title="Changed files"
            icon={<FileWarning size={14} className="text-warning" />}
            note="Edited in place and set aside: these files are not served and nothing is written over them until you choose."
            tone="warning"
          >
            <ul className="space-y-1.5">
              {changed.map((row) => (
                <li key={row.frameUuid} className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs">
                  <span className="max-w-[16rem] truncate text-content" title={row.fileName}>
                    {row.fileName}
                  </span>
                  <span className="min-w-0 flex-1 truncate text-content-muted" title={row.path}>
                    {row.path}
                  </span>
                  <span className="text-content-muted">{formatTimestamp(row.detectedAt)}</span>
                  {row.newVersionWaiting && (
                    <span className="rounded bg-accent/20 px-1.5 py-0.5 text-[10px] text-accent">A new version is waiting</span>
                  )}
                  <span className="ml-auto flex gap-1.5">
                    <button
                      type="button"
                      className={SMALL_BTN}
                      disabled={busy}
                      aria-label={`Re-fetch original ${row.fileName}`}
                      onClick={() => void refetchOriginal(row, false)}
                    >
                      Re-fetch original
                    </button>
                    <button
                      type="button"
                      className={SMALL_BTN_DANGER}
                      disabled={busy}
                      aria-label={`Delete ${row.fileName}`}
                      onClick={() => deleteChanged(row)}
                    >
                      Delete
                    </button>
                  </span>
                </li>
              ))}
            </ul>
          </Section>
        )}

        {awaitingChoice.length > 0 && (
          <Section
            title="Waiting for your choice"
            icon={<Trash2 size={14} className="text-warning" />}
            note={`${awaitingChoice.length} ${awaitingChoice.length === 1 ? 'frame was' : 'frames were'} deleted from the Collaboration folder. Re-fetch them, or stop keeping them. Nothing else is paused.`}
            tone="warning"
            actions={
              <>
                <button type="button" className={SMALL_BTN} disabled={busy} onClick={() => void refetch(null)}>
                  Re-fetch all
                </button>
                <button
                  type="button"
                  className={SMALL_BTN_DANGER}
                  disabled={busy}
                  onClick={() => void stopKeeping(awaitingChoice.map((r) => r.frameUuid))}
                >
                  Stop keeping all
                </button>
              </>
            }
          >
            <ul className="space-y-1.5">
              {awaitingChoice.map((row) => (
                <li key={row.frameUuid} className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs">
                  <span className="max-w-[16rem] truncate text-content" title={row.fileName}>
                    {row.fileName}
                  </span>
                  <span className="text-content-muted">
                    Holders: {row.holdersOnline} online / {row.holdersTotal}
                  </span>
                  {row.atRisk && (
                    <span className="inline-flex items-center gap-1 text-warning">
                      <AlertTriangle size={11} /> Last holders
                    </span>
                  )}
                  <span className="ml-auto flex gap-1.5">
                    <button
                      type="button"
                      className={SMALL_BTN}
                      disabled={busy}
                      aria-label={`Re-fetch ${row.fileName}`}
                      onClick={() => void refetch([row.frameUuid])}
                    >
                      Re-fetch
                    </button>
                    <button
                      type="button"
                      className={SMALL_BTN_DANGER}
                      disabled={busy}
                      aria-label={`Stop keeping ${row.fileName}`}
                      onClick={() => void stopKeeping([row.frameUuid])}
                    >
                      Stop keeping
                    </button>
                  </span>
                </li>
              ))}
            </ul>
          </Section>
        )}

        {notKept.length > 0 && (
          <Section
            title="Not kept"
            icon={<Trash2 size={14} className="text-content-muted" />}
            note="Frames this device no longer keeps. Keep again fetches them once more."
            actions={
              <button type="button" className={SMALL_BTN} disabled={busy} onClick={() => void keepAgain(null)}>
                Keep all again
              </button>
            }
          >
            <ul className="space-y-1.5">
              {notKept.map((row) => (
                <li key={row.frameUuid} className="flex flex-wrap items-center gap-x-3 gap-y-1 text-xs">
                  <span className="max-w-[16rem] truncate text-content" title={row.fileName}>
                    {row.fileName}
                  </span>
                  <span className="text-content-muted">v{row.contentVersion}</span>
                  <span className="ml-auto">
                    <button
                      type="button"
                      className={SMALL_BTN}
                      disabled={busy}
                      aria-label={`Keep again ${row.fileName}`}
                      onClick={() => void keepAgain([row.frameUuid])}
                    >
                      Keep again
                    </button>
                  </span>
                </li>
              ))}
            </ul>
          </Section>
        )}

        {otherFiles.length > 0 && (
          <Section
            title="Other files"
            icon={<FileQuestion size={14} className="text-content-muted" />}
            note="Files in the Collaboration folder that belong to no project frame. The app never deletes them."
          >
            <ul className="space-y-1">
              {otherFiles.map((f) => (
                <li key={f.path} className="flex flex-wrap items-center gap-x-3 text-xs">
                  <span className="min-w-0 flex-1 truncate text-content-secondary" title={f.path}>
                    {f.path}
                  </span>
                  <span className="text-content-muted">{formatTimestamp(f.seenAt)}</span>
                </li>
              ))}
            </ul>
          </Section>
        )}
      </div>

      <ConfirmDialog
        isOpen={confirm !== null}
        title={confirm?.title ?? ''}
        message={confirm?.message ?? ''}
        confirmText={confirm?.confirmText}
        confirmDanger
        onConfirm={() => confirm?.onConfirm()}
        onCancel={() => setConfirm(null)}
      />
    </div>
  );
}

function Section({
  title,
  icon,
  note,
  tone,
  actions,
  children,
}: {
  title: string;
  icon: ReactNode;
  note: string;
  tone?: 'warning';
  actions?: ReactNode;
  children: ReactNode;
}) {
  const headingId = useId();
  return (
    <section
      aria-labelledby={headingId}
      className={`space-y-2 rounded border px-3 py-2 ${tone === 'warning' ? 'border-warning/40 bg-warning/5' : 'border-border bg-surface'}`}
    >
      <div className="flex flex-wrap items-center gap-2">
        {icon}
        <h3 id={headingId} className="text-sm font-medium text-content">
          {title}
        </h3>
        {actions && <span className="ml-auto flex gap-1.5">{actions}</span>}
      </div>
      <p className="text-xs text-content-muted">{note}</p>
      {children}
    </section>
  );
}
