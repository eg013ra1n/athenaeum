import { useCallback, useEffect, useMemo, useState, type JSX } from 'react';
import { createPortal } from 'react-dom';
import { api } from '../../../api';
import BlinkViewer from '../../BlinkViewer';
import type { BlinkAction, BlinkFrame } from '../../blink/types';
import { useNotifications } from '../../../contexts/NotificationContext';
import { formatTimestamp } from '../../../utils/dateFormatting';
import type { CollabBlinkEntry, CollabFrameRef } from '../../../types/models';
import ExcludeDialog from './ExcludeDialog';
import { blinkBadge, blinkRef, refKey, type BlinkTable } from './blinkEligibility';
import type { FrameVM } from './frames';
import { useWithhold } from './useWithhold';

interface Row { e: CollabBlinkEntry; ref: CollabFrameRef; vmKey: string }

/** Spec 2026-10-01 §9 — Blink over a project table's selection, with the role's actions.
 *  Mount one per open: Blink snapshots its frames at mount, so the viewer is rendered
 *  only after the entries resolved. */
export default function ProjectBlink({ projectId, table, vms, lookup, canModerate, onClose, onChanged }: {
  projectId: string;
  table: BlinkTable;
  vms: FrameVM[]; // the rows the Blink action ran on
  lookup: (vmKey: string) => FrameVM | undefined; // the tab's CURRENT rows, every segment
  canModerate: boolean;
  onClose: () => void;
  onChanged: () => void;
}): JSX.Element {
  const { notify } = useNotifications();
  const [rows, setRows] = useState<Row[] | null>(null);
  const [excluding, setExcluding] = useState<FrameVM[] | null>(null);
  const withhold = useWithhold(projectId, onChanged);

  useEffect(() => {
    let cancelled = false;
    const picks = vms.flatMap((v) => {
      const ref = blinkRef(v, table);
      return ref ? [{ ref, vmKey: v.key }] : [];
    });
    const byKey = new Map(picks.map((p) => [refKey(p.ref), p]));
    api
      .invoke<CollabBlinkEntry[]>('get_collab_blink_frames', { projectId, refs: picks.map((p) => p.ref) })
      .then((got) => {
        if (cancelled) return;
        const next = got.flatMap((e) => {
          const p = byKey.get(e.key);
          return p ? [{ e, ref: p.ref, vmKey: p.vmKey }] : [];
        });
        if (next.length === 0) {
          console.warn('[projects] blink: none of the selection is on this device', { projectId, asked: picks.length });
          notify({ title: 'Nothing to blink', detail: 'None of these files is on this device.', kind: 'project', tone: 'warning' });
          onClose();
          return;
        }
        setRows(next);
      })
      .catch((err) => {
        if (cancelled) return;
        console.error('[projects] get_collab_blink_frames failed:', err);
        notify({
          title: 'Could not open Blink',
          detail: err instanceof Error ? err.message : String(err),
          kind: 'project', tone: 'warning', hasErrors: true,
        });
        onClose();
      });
    return () => { cancelled = true; };
  }, []); // eslint-disable-line react-hooks/exhaustive-deps -- once per open

  const vmKeyOf = useMemo(() => new Map((rows ?? []).map((r) => [r.e.key, r.vmKey])), [rows]);
  const vmOf = useCallback((f: BlinkFrame): FrameVM | undefined => {
    const k = f.key ? vmKeyOf.get(f.key) : undefined;
    return k ? lookup(k) : undefined;
  }, [vmKeyOf, lookup]);

  const frames: BlinkFrame[] = useMemo(() => (rows ?? []).map(({ e, ref, vmKey }) => {
    const vm = lookup(vmKey);
    return {
      ...e.entry,
      key: e.key,
      source: e.source,
      imageRef: e.source === 'raw' ? undefined : { projectId, frame: ref },
      badge: vm ? blinkBadge(vm) : undefined,
    };
  }), [rows, lookup, projectId]);

  const own = table !== 'library';
  const viewOnly = !canModerate && (table === 'published' || table === 'library');
  const actions: BlinkAction[] = [];
  if (own) {
    actions.push({
      id: 'withhold', label: (n) => `Don't publish (${n})`, tone: 'warn',
      eligible: (f) => {
        const v = vmOf(f);
        return !!v?.own && (v.own.segment === 'ready' || v.own.segment === 'review') && !v.own.withheld;
      },
      run: async (fs) => {
        const targets = fs.flatMap((f) => {
          const v = vmOf(f);
          return v?.frameId != null ? [{ frameId: v.frameId, prepared: v.own?.calibratedPath != null }] : [];
        });
        await withhold.dontPublish(targets);
      },
    });
    actions.push({
      id: 'release', label: (n) => `Release (${n})`, tone: 'default',
      eligible: (f) => vmOf(f)?.own?.withheld === true,
      run: async (fs) => {
        const ids = fs.flatMap((f) => { const id = vmOf(f)?.frameId; return id != null ? [id] : []; });
        await withhold.release(ids);
      },
    });
  }
  if (canModerate) {
    actions.push({
      id: 'exclude', label: (n) => `Exclude from project (${n})`, tone: 'danger',
      eligible: (f) => {
        const v = vmOf(f);
        return !!v && v.pubState === 'published' && !v.excluded && v.frameUuid !== null
          && (own ? v.own?.segment === 'published' : true);
      },
      run: (fs) => setExcluding(fs.flatMap((f) => { const v = vmOf(f); return v ? [v] : []; })),
    });
  }

  const contextLabel = (f: BlinkFrame): string => {
    const v = vmOf(f);
    const parts: string[] = [];
    if (f.source === 'replica') {
      const who = v?.lib?.receivedFromMember ?? v?.publisher ?? 'a member';
      const at = v?.lib?.receivedAt;
      parts.push(`received from ${who}${at ? ` · ${formatTimestamp(at, { seconds: true })}` : ''}`);
    } else if (f.source === 'calibrated') {
      parts.push(v?.own?.segment === 'review' ? 'exactly the file that will be published' : 'the calibrated file of this frame');
    } else {
      parts.push('the raw frame on this device');
    }
    if (viewOnly) {
      parts.push(table === 'published' ? 'Only a moderator can exclude published frames.' : 'Only a moderator can exclude frames.');
    }
    return parts.join(' · ');
  };

  return (
    <>
      {rows && createPortal(
        <BlinkViewer frames={frames} onClose={onClose} actions={actions} contextLabel={contextLabel} viewOnly={viewOnly} />,
        document.body,
      )}
      {withhold.dialog}
      {excluding && (
        <ExcludeDialog projectId={projectId} frames={excluding} onClose={() => setExcluding(null)} onDone={() => onChanged()} />
      )}
    </>
  );
}
