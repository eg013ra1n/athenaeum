import type { LightFrameRef } from './frameRows';

/** What a stacking run is about: one frame set (the Stacking tab of an
 *  object) or one collaboration project (the Stacking tab of a project).
 *  Wave 6c: the run hook, the selected-run memory and the notification link
 *  are all keyed by the subject, so two runs in flight never share a slot. */
export type StackSubject =
  | { kind: 'frameSet'; framesSetId: number; lightFrames: LightFrameRef[] }
  | { kind: 'project'; projectId: string; title: string };

/** `set:<id>` for a frame set, `project:<projectId>` for a project. */
export type SubjectKey = string;

export function subjectKey(s: StackSubject): SubjectKey {
  return s.kind === 'frameSet' ? `set:${s.framesSetId}` : `project:${s.projectId}`;
}

/** The subject a `stacking-progress` / `stacking-complete` event belongs to.
 *  The backend sets exactly one of the two ids; an event with neither or both
 *  is logged and yields `null`, and the caller drops it. */
export function eventSubjectKey(e: { setId: number | null; projectId: string | null }): SubjectKey | null {
  if (e.setId != null && e.projectId == null) return `set:${e.setId}`;
  if (e.setId == null && e.projectId != null) return `project:${e.projectId}`;
  console.error('[subject] stacking event carries neither or both of setId and projectId:', e);
  return null;
}

/** The route of the subject's Stacking tab. */
export function subjectLink(s: StackSubject): string {
  return s.kind === 'frameSet'
    ? `/objects/${s.framesSetId}?tab=stacking`
    : `/projects/${s.projectId}?tab=stacking`;
}
