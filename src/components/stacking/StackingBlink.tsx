// Blink over the Frames table's rows (spec §6.7): the originals of a frame set or a project, in the viewer's project
// mode. Its actions write through the table's own writers (Exclude, Include, the reference star) — never the Black
// Hole, never a file deletion. Mount one per open (keyed by an open counter): the viewer snapshots its entries when
// it opens, so it is rendered only after the entries resolved, and the badges and eligibility follow the live
// exclusion list by entry key.
import { useCallback, useEffect, useMemo, useState, type JSX } from 'react';
import { createPortal } from 'react-dom';
import { api } from '../../api';
import BlinkViewer from '../BlinkViewer';
import type { BlinkAction, BlinkFrame } from '../blink/types';
import { useNotifications } from '../../contexts/NotificationContext';
import type { CollabBlinkEntry, FileWithFrame } from '../../types/models';
import type { PlanGroup, StackingRunDetail } from '../../types/stacking';
import { HANDLE_KEY_PREFIX, type FrameRow } from './frameRows';
import { NOT_GROUPED_TITLE, planGroupTitle } from './frameView';
import type { StackSubject } from './subject';

export interface StackingBlinkProps {
  subject: StackSubject;
  /** The rows to blink, in the table's order. Read once, when Blink opens. */
  rows: FrameRow[];
  /** The plan's groups: the strip names a frame's group the way the table's headers do. */
  planGroups: PlanGroup[];
  /** The run whose rows these are (`null` before any run). Read by the Aligned switch. */
  runDetail: StackingRunDetail | null;
  /** The live exclusion keys: the badges and the Exclude / Include eligibility follow them. */
  excludedKeys: string[];
  /** The key of the manual reference in effect, live; `null` for none. The pinned frame is not offered again. */
  pinnedKey: string | null;
  /** A run is active: Blink shows no action. */
  disabled: boolean;
  onSetExcluded: (keys: string[], excluded: boolean) => void;
  onPin: (key: string) => void;
  onClose: () => void;
}

/** A row and the viewer entry it resolved to (the entry carries the row's key, and no badge yet). */
interface Resolved {
  row: FrameRow;
  entry: BlinkFrame;
}

/** What loading found: how many rows could be asked for, and the entries in the rows' order. */
interface Loaded {
  asked: number;
  resolved: Resolved[];
}

/** A frame set's entries: one read by frame id, which answers in date order, put back in the rows' order. */
async function loadSetEntries(rows: FrameRow[]): Promise<Loaded> {
  const frameIds = rows.flatMap((r) => (r.frameId === null ? [] : [r.frameId]));
  const got =
    frameIds.length === 0 ? [] : await api.invoke<FileWithFrame[]>('get_files_with_frames_by_ids', { frameIds });
  const byFrameId = new Map<number, FileWithFrame>();
  for (const fw of got) if (fw.frame?.id != null) byFrameId.set(fw.frame.id, fw);
  const resolved = rows.flatMap((row) => {
    const fw = row.frameId === null ? undefined : byFrameId.get(row.frameId);
    return fw ? [{ row, entry: { ...fw, key: row.key } }] : [];
  });
  return { asked: frameIds.length, resolved };
}

/** A project's entries, by frame uuid: a row keyed `h:<handle>` has no uuid (its frame left the project) and is not
 *  asked for. Core keys every entry by the uuid it answers. */
async function loadProjectEntries(projectId: string, rows: FrameRow[]): Promise<Loaded> {
  const askable = rows.filter((r) => !r.key.startsWith(HANDLE_KEY_PREFIX));
  const got =
    askable.length === 0
      ? []
      : await api.invoke<CollabBlinkEntry[]>('get_collab_blink_frames', {
          projectId,
          refs: askable.map((r) => ({ frameId: null, frameUuid: r.key })),
        });
  const byKey = new Map(got.map((e) => [e.key, e]));
  const unmatched = got.filter((e) => !askable.some((r) => r.key === e.key)).map((e) => e.key);
  if (unmatched.length > 0) {
    console.error('[StackingBlink] entries match no requested frame', { projectId, keys: unmatched });
  }
  const resolved = askable.flatMap((row) => {
    const e = byKey.get(row.key);
    if (!e) return [];
    const entry: BlinkFrame = {
      ...e.entry,
      key: e.key,
      source: e.source,
      imageRef: e.source === 'raw' ? undefined : { projectId, frame: { frameId: null, frameUuid: row.key } },
    };
    return [{ row, entry }];
  });
  return { asked: askable.length, resolved };
}

/** An entry's badge: the table's status chip label, then `· out of next run` when the frame is in the list. */
function badgeOf(row: FrameRow, listed: boolean): string | undefined {
  const label = row.outcome?.label;
  if (!listed) return label;
  return label ? `${label} · out of next run` : 'out of next run';
}

export function StackingBlink({
  subject, rows, planGroups, excludedKeys, pinnedKey, disabled, onSetExcluded, onPin, onClose,
}: StackingBlinkProps): JSX.Element | null {
  const { notify } = useNotifications();
  const [resolved, setResolved] = useState<Resolved[] | null>(null);

  useEffect(() => {
    let cancelled = false;
    const notAvailable = (count: number) =>
      notify({
        title: 'Some frames are not on this device',
        detail: `${count} of ${rows.length} frames are not on this device.`,
        kind: 'stacking',
        tone: 'warning',
      });
    (subject.kind === 'frameSet' ? loadSetEntries(rows) : loadProjectEntries(subject.projectId, rows))
      .then(({ asked, resolved: found }) => {
        if (cancelled) return;
        // Rows with no uuid are skipped without a word; the count is of the frames that were asked for and did
        // not come back. With nothing to open the notice still says so, and Blink closes.
        const missing = asked - found.length;
        if (found.length === 0) {
          notAvailable(asked === 0 ? rows.length : missing);
          onClose();
          return;
        }
        if (missing > 0) notAvailable(missing);
        setResolved(found);
      })
      .catch((err) => {
        if (cancelled) return;
        console.error('[StackingBlink] loading the frames failed:', err);
        notify({
          title: 'Could not open Blink',
          detail: err instanceof Error ? err.message : String(err),
          kind: 'stacking',
          tone: 'warning',
          hasErrors: true,
        });
        onClose();
      });
    return () => {
      cancelled = true;
    };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps -- once per open

  const rowByKey = useMemo(() => new Map(rows.map((r) => [r.key, r])), [rows]);
  const listed = useMemo(() => new Set(excludedKeys), [excludedKeys]);
  const rowOf = useCallback((f: BlinkFrame) => (f.key === undefined ? undefined : rowByKey.get(f.key)), [rowByKey]);

  const frames = useMemo<BlinkFrame[]>(
    () => (resolved ?? []).map(({ row, entry }) => ({ ...entry, badge: badgeOf(row, listed.has(row.key)) })),
    [resolved, listed],
  );

  const actions = useMemo<BlinkAction[]>(() => {
    if (disabled) return [];
    const keysOf = (fs: BlinkFrame[]) => fs.flatMap((f) => (f.key === undefined ? [] : [f.key]));
    const isListed = (f: BlinkFrame) => f.key !== undefined && listed.has(f.key);
    return [
      {
        id: 'exclude',
        label: (n) => `Exclude from next run (${n})`,
        tone: 'warn',
        eligible: (f) => rowOf(f)?.editable === true && !isListed(f),
        run: (fs) => onSetExcluded(keysOf(fs), true),
      },
      {
        id: 'include',
        label: (n) => `Include (${n})`,
        tone: 'default',
        eligible: (f) => rowOf(f)?.editable === true && isListed(f),
        run: (fs) => onSetExcluded(keysOf(fs), false),
      },
      {
        // The pin toggles: offered on the pinned frame it would clear the reference, which the table's star does.
        id: 'reference',
        label: () => 'Use as reference',
        tone: 'default',
        single: true,
        eligible: (f) => rowOf(f)?.pinnable === true && f.key !== pinnedKey,
        run: (fs) => {
          const [key] = keysOf(fs);
          if (key !== undefined) onPin(key);
        },
      },
    ];
  }, [disabled, listed, pinnedKey, rowOf, onSetExcluded, onPin]);

  /** `<group title> · <the outcome's reason>`. */
  const contextLabel = useCallback(
    (f: BlinkFrame): string => {
      const row = rowOf(f);
      if (!row) return '';
      const group = row.groupKey === null ? NOT_GROUPED_TITLE : planGroupTitle(planGroups, row.groupKey);
      return [group, row.outcome?.reason].filter(Boolean).join(' · ');
    },
    [rowOf, planGroups],
  );

  if (resolved === null) return null;
  return createPortal(
    <BlinkViewer
      frames={frames}
      onClose={onClose}
      actions={actions}
      contextLabel={contextLabel}
      frameSetId={subject.kind === 'frameSet' ? subject.framesSetId : undefined}
    />,
    document.body,
  );
}
