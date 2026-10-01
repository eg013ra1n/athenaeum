import type { CollabFrameRef } from '../../../types/models';
import type { FrameVM } from './frames';

export type BlinkTable = 'ready' | 'review' | 'published' | 'held' | 'library';

/** Spec 2026-10-01 §9.4 — what Blink asks core for; null when the file is not on this device.
 *  Own frames go by catalog id (core picks raw / calibrated), library frames by uuid. */
export function blinkRef(v: FrameVM, table: BlinkTable): CollabFrameRef | null {
  if (table === 'library') {
    const ls = v.lib?.localState;
    return v.frameUuid && (ls === 'held' || ls === 'own_held') ? { frameId: null, frameUuid: v.frameUuid } : null;
  }
  const o = v.own;
  if (!o || v.frameId === null) return null;
  const byId = { frameId: v.frameId, frameUuid: null };
  switch (table) {
    case 'ready':
    case 'held':
      return o.path != null && !o.failures.some((f) => f.kind === 'blackHole') ? byId : null;
    case 'review':
      return (o.calibratedPath ?? o.path) != null ? byId : null;
    case 'published':
      return o.localState === 'own_held' || o.localState === 'own_changed' ? byId : null;
  }
}

/** An entry's state chip. "changed on disk" is an own frame's only: Library
 *  blinks just `held` / `own_held` replicas (§9.4). */
export function blinkBadge(v: FrameVM): string | undefined {
  if (v.own?.withheld) return 'withheld';
  if (v.excluded) return 'excluded';
  if (v.own?.localState === 'own_changed') return 'changed on disk';
  return undefined;
}

/** Mirrors `api::collab_blink` — an entry's key is its ref's uuid, else `f<frameId>`. */
export function refKey(r: CollabFrameRef): string {
  return r.frameUuid ?? `f${r.frameId}`;
}
