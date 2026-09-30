import { useCallback, useEffect, useId, useRef, useState, type ReactNode } from 'react';
import { AlertTriangle, ChevronDown, ChevronRight, FileQuestion } from 'lucide-react';
import { api } from '../../api';
import { useNotifications } from '../../contexts/NotificationContext';
import { Button, Card, Chip } from '../ui';
import { ConfirmDialog } from '../ConfirmDialog';
import { formatTimestamp } from '../../utils/dateFormatting';
import type {
  ChangedFileOutcome,
  ChangedFileView,
  CollabAttention as Attention,
  CollabAttentionChanged,
  ForeignFileView,
  LastCopyView,
} from '../../types/models';

/**
 * The project's replicas that need the user (spec §9.4, L4–L6), above the
 * Library tab's frame list; each list is hidden while empty:
 *
 * - **Changed files** (L5) — a replica edited in place, set aside and no
 *   longer served: "Re-fetch original" (the edited file goes to the system
 *   trash, else is deleted after confirmation) or "Delete".
 * - **Waiting for your choice** (L4) — a mass or repeated deletion: "Re-fetch"
 *   or "Stop keeping", per frame and for all. "Stop keeping" shows the
 *   last-copy warning first when a frame has fewer than 2 other holders.
 * - **Not kept** (L6) — reversible: "Keep again", per frame and for all.
 * - **Other files** — files in the folder that no frame references; never
 *   deleted. Informational, so it stays one collapsed line with a count.
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
    <div>
      <div aria-hidden={confirm ? true : undefined}>
        <Card title="Needs your attention">
          {changed.length > 0 && (
            <Group
              label="Changed files"
              note="Edited in place and set aside: these files are not served and nothing is written over them until you choose."
            >
              {changed.map((row) => (
                <Row key={row.frameUuid}>
                  <span className="max-w-[16rem] truncate font-mono text-[12px] text-content" title={row.fileName}>
                    {row.fileName}
                  </span>
                  <span className="min-w-0 flex-1 truncate text-[11.5px] text-content-faint" title={row.path}>
                    {row.path}
                  </span>
                  <span className="text-[11.5px] text-content-faint">{formatTimestamp(row.detectedAt)}</span>
                  {row.newVersionWaiting && <Chip tone="info">A new version is waiting</Chip>}
                  <Button
                    size="sm"
                    disabled={busy}
                    aria-label={`Re-fetch original ${row.fileName}`}
                    onClick={() => void refetchOriginal(row, false)}
                  >
                    Re-fetch original
                  </Button>
                  <Button
                    size="sm"
                    variant="danger"
                    disabled={busy}
                    aria-label={`Delete ${row.fileName}`}
                    onClick={() => deleteChanged(row)}
                  >
                    Delete
                  </Button>
                </Row>
              ))}
            </Group>
          )}

          {awaitingChoice.length > 0 && (
            <Group
              label="Waiting for your choice"
              note={`${awaitingChoice.length} ${awaitingChoice.length === 1 ? 'frame was' : 'frames were'} deleted from the Collaboration folder. Re-fetch them, or stop keeping them. Nothing else is paused.`}
              actions={
                <>
                  <Button size="sm" disabled={busy} onClick={() => void refetch(null)}>
                    Re-fetch all
                  </Button>
                  <Button
                    size="sm"
                    variant="danger"
                    disabled={busy}
                    onClick={() => void stopKeeping(awaitingChoice.map((r) => r.frameUuid))}
                  >
                    Stop keeping all
                  </Button>
                </>
              }
            >
              {awaitingChoice.map((row) => (
                <Row key={row.frameUuid}>
                  <span className="max-w-[16rem] truncate font-mono text-[12px] text-content" title={row.fileName}>
                    {row.fileName}
                  </span>
                  <span className="min-w-0 flex-1 text-[11.5px] text-content-faint">
                    Holders: {row.holdersOnline} online / {row.holdersTotal}
                  </span>
                  {row.atRisk && (
                    <span className="inline-flex items-center gap-1 text-[11.5px] text-warning">
                      <AlertTriangle size={11} /> Last holders
                    </span>
                  )}
                  <Button
                    size="sm"
                    disabled={busy}
                    aria-label={`Re-fetch ${row.fileName}`}
                    onClick={() => void refetch([row.frameUuid])}
                  >
                    Re-fetch
                  </Button>
                  <Button
                    size="sm"
                    variant="danger"
                    disabled={busy}
                    aria-label={`Stop keeping ${row.fileName}`}
                    onClick={() => void stopKeeping([row.frameUuid])}
                  >
                    Stop keeping
                  </Button>
                </Row>
              ))}
            </Group>
          )}

          {notKept.length > 0 && (
            <Group
              label="Not kept"
              note="Frames this device no longer keeps. Keep again fetches them once more."
              actions={
                <Button size="sm" disabled={busy} onClick={() => void keepAgain(null)}>
                  Keep all again
                </Button>
              }
            >
              {notKept.map((row) => (
                <Row key={row.frameUuid}>
                  <span className="max-w-[16rem] truncate font-mono text-[12px] text-content" title={row.fileName}>
                    {row.fileName}
                  </span>
                  <span className="min-w-0 flex-1 text-[11.5px] text-content-faint">v{row.contentVersion}</span>
                  <Button
                    size="sm"
                    disabled={busy}
                    aria-label={`Keep again ${row.fileName}`}
                    onClick={() => void keepAgain([row.frameUuid])}
                  >
                    Keep again
                  </Button>
                </Row>
              ))}
            </Group>
          )}

          {otherFiles.length > 0 && <OtherFiles files={otherFiles} />}
        </Card>
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

const ROW_CLS = 'flex items-center gap-2.5 border-t border-line py-[7px] text-[12.5px] first-of-type:border-t-0';

/** One compact attention row (mockup `.att`). */
function Row({ children }: { children: ReactNode }) {
  return <div className={ROW_CLS}>{children}</div>;
}

/** A list inside the card: 12 px faint label (the note is its tooltip), bulk
 *  buttons to its right, then the rows. */
function Group({
  label,
  note,
  actions,
  children,
}: {
  label: string;
  note: string;
  actions?: ReactNode;
  children: ReactNode;
}) {
  return (
    <div className="mb-2 last:mb-0">
      <div className="flex items-center gap-2 pb-1">
        <span className="text-[12px] text-content-faint" title={note}>
          {label}
        </span>
        {actions && <span className="ml-auto flex items-center gap-1.5">{actions}</span>}
      </div>
      <div>{children}</div>
    </div>
  );
}

/** File name and its folder, split at the last separator (either kind). */
function splitPath(path: string): { name: string; dir: string } {
  const i = Math.max(path.lastIndexOf('/'), path.lastIndexOf('\\'));
  return i < 0 ? { name: path, dir: '' } : { name: path.slice(i + 1), dir: path.slice(0, i) };
}

/** "Other files": nothing to act on, so a collapsed line until opened; the
 *  open list scrolls inside a bounded box instead of pushing the table down. */
function OtherFiles({ files }: { files: ForeignFileView[] }) {
  const [open, setOpen] = useState(false);
  const listId = useId();
  const Chevron = open ? ChevronDown : ChevronRight;
  return (
    <div className="mb-2 last:mb-0">
      <div className="pb-1 text-[12px] text-content-faint" title="Files in the Collaboration folder that no frame references; the app never deletes them.">
        Other files
      </div>
      <button
        type="button"
        className={`${ROW_CLS} w-full text-left text-content-secondary hover:text-content`}
        aria-expanded={open}
        aria-controls={listId}
        onClick={() => setOpen((o) => !o)}
      >
        <Chevron size={14} className="text-content-muted" />
        <FileQuestion size={14} className="text-content-muted" />
        <span>
          {files.length} other {files.length === 1 ? 'file' : 'files'} in the Collaboration folder
        </span>
        <span className="truncate text-[11.5px] text-content-faint">— belong to no project frame; the app never deletes them</span>
      </button>
      {open && (
        <ul id={listId} className="max-h-48 space-y-0.5 overflow-y-auto pr-1">
          {files.map((f) => {
            const { name, dir } = splitPath(f.path);
            return (
              <li key={f.path} className="flex items-center gap-x-3 text-[12px]" title={f.path}>
                <span className="max-w-[18rem] shrink-0 truncate font-mono text-content">{name}</span>
                <span className="min-w-0 flex-1 truncate text-[11.5px] text-content-faint">{dir}</span>
                <span className="shrink-0 text-[11.5px] text-content-faint">{formatTimestamp(f.seenAt)}</span>
              </li>
            );
          })}
        </ul>
      )}
    </div>
  );
}
