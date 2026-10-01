import { useCallback, useMemo, useRef, useState } from 'react';
import type { BlinkAction, BlinkFrame, ToolBarProps } from './types';

/**
 * BlinkViewer's project mode (spec 2026-10-01 §9.2): a caller that passes
 * `actions` (even empty) gets a snapshot of `frames`, live entries matched by
 * `key`, and its actions offered on the selection in place of the Black Hole.
 * Without `actions`, `frames` is read live and no action is offered — Blink
 * behaves exactly as before.
 *
 * `selectedFrames` holds indexes into the returned `fitsFrames`.
 */
export function useBlinkProjectMode(
  frames: BlinkFrame[],
  actions: BlinkAction[] | undefined,
  selectedFrames: Set<number>,
) {
  /** Project mode (spec §9.2): a caller that passes `actions` (even empty). */
  const projectMode = actions !== undefined;

  // Project mode: the action whose run is in flight. The ref refuses a
  // second run even when the busy action's button has left the toolbar (its
  // entries stopped being eligible mid-run).
  const [actionBusy, setActionBusy] = useState<string | null>(null);
  const actionBusyRef = useRef<string | null>(null);

  // Project mode snapshots the entries when Blink opens (spec §9.2): project
  // actions never remove entries, so indexes — and every index-keyed cache —
  // stay stable while the caller reloads its rows. Badges come from the live
  // prop, matched by `key`. Without `actions`, `frames` is read live as before.
  const snapshot = useRef(frames);
  const base = projectMode ? snapshot.current : frames;

  // Filter FITS and XISF files
  const fitsFrames = useMemo(
    () => base.filter((f) => f.file.format === 'FITS' || f.file.format === 'XISF'),
    [base],
  );

  const liveByKey = useMemo(
    () => new Map(frames.filter((f) => f.key).map((f) => [f.key!, f])),
    [frames],
  );
  /** The live entry for a snapshot entry. Render, eligibility and action
   * arguments go through it; image loading never does. */
  const view = useCallback(
    (f: BlinkFrame): BlinkFrame => (f.key && liveByKey.get(f.key)) || f,
    [liveByKey],
  );

  // Project mode: each caller action offers the eligible entries of the
  // selection, read from the live entries; one with none is hidden.
  const selectedViews = useMemo(
    () => [...selectedFrames].map((i) => fitsFrames[i]).filter(Boolean).map(view),
    [selectedFrames, fitsFrames, view],
  );
  const projectActions: NonNullable<ToolBarProps['projectActions']> | undefined = useMemo(() => actions?.flatMap((a) => {
    const eligible = selectedViews.filter(a.eligible);
    if (eligible.length === 0) return [];
    return [{
      id: a.id,
      label: a.label(eligible.length),
      tone: a.tone,
      busy: actionBusy === a.id,
      disabled: actionBusy !== null,
      onClick: () => {
        if (actionBusyRef.current !== null) return;
        actionBusyRef.current = a.id;
        setActionBusy(a.id);
        Promise.resolve()
          .then(() => a.run(eligible))
          .catch((err) => console.error(`[blink] action ${a.id} failed:`, err))
          .finally(() => {
            actionBusyRef.current = null;
            setActionBusy(null);
          });
      },
    }];
  }), [actions, selectedViews, actionBusy]);

  // The frame list renders the live entries (badges) over the snapshot's indexes.
  const listFrames = useMemo(() => fitsFrames.map(view), [fitsFrames, view]);

  return { projectMode, fitsFrames, view, projectActions, listFrames };
}
