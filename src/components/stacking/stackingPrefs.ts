// Stacking-tab UI preferences, persisted per browser/profile in
// localStorage (same defensive-total-reader convention as
// `export/lightCalPrefs.ts` — an unset, unparsable or out-of-range value
// resolves to the documented default rather than throwing).
//
// Created in Plan 5b Task 3 for the inspector's selected board row; Task 4
// extends this file with the Frames table's collapsed state and the
// Results panel's last-selected run, rather than starting a second prefs
// module. Every key below is a flat `athenaeum.stacking.<name>` string,
// matching Task 3's own `STACKING_SELECTED_STAGE_KEY` and
// `export/lightCalPrefs.ts`'s convention (individual keys, not one JSON
// blob) — the Task 4 brief's "keys under `athenaeum.stacking.v1`" describes
// a namespace prefix, not a literal key Task 3 actually shipped, so this
// keeps the real, already-committed convention rather than introducing a
// second, incompatible one alongside it.

import type { BoardStage } from './stageSummary';

/** localStorage key for the last-selected inspector stage. */
export const STACKING_SELECTED_STAGE_KEY = 'athenaeum.stacking.selectedStage';

const VALID_STAGES: readonly BoardStage[] = [
  'masters',
  'calibrate',
  'debayer',
  'measure',
  'reference',
  'register',
  'normalize',
  'integrate',
  'drizzle',
  'output',
];

/** Read the last-selected board/inspector stage (default `'calibrate'` when
 *  unset/corrupt). */
export function readSelectedStage(): BoardStage {
  try {
    const raw = localStorage.getItem(STACKING_SELECTED_STAGE_KEY);
    return raw !== null && (VALID_STAGES as readonly string[]).includes(raw) ? (raw as BoardStage) : 'calibrate';
  } catch (err) {
    console.warn('[stackingPrefs] read selected stage failed:', err);
    return 'calibrate';
  }
}

/** Persist the selected board/inspector stage. Best-effort: a storage
 *  failure loses the memory of the choice, never the choice itself. */
export function writeSelectedStage(stage: BoardStage): void {
  try {
    localStorage.setItem(STACKING_SELECTED_STAGE_KEY, stage);
  } catch (err) {
    console.warn('[stackingPrefs] write selected stage failed:', err);
  }
}

/** localStorage key for the Frames table's collapsed state. */
export const STACKING_FRAMES_COLLAPSED_KEY = 'athenaeum.stacking.framesCollapsed';

/** Read whether the Frames table is collapsed (default `false` — expanded —
 *  when unset/corrupt). */
export function readFramesCollapsed(): boolean {
  try {
    return localStorage.getItem(STACKING_FRAMES_COLLAPSED_KEY) === 'true';
  } catch (err) {
    console.warn('[stackingPrefs] read frames-collapsed failed:', err);
    return false;
  }
}

/** Persist whether the Frames table is collapsed. */
export function writeFramesCollapsed(collapsed: boolean): void {
  try {
    localStorage.setItem(STACKING_FRAMES_COLLAPSED_KEY, String(collapsed));
  } catch (err) {
    console.warn('[stackingPrefs] write frames-collapsed failed:', err);
  }
}

/** localStorage key for the inspector's collapsed state below the `lg`
 *  breakpoint (Task 5, Decisions item 3). */
export const STACKING_INSPECTOR_COLLAPSED_KEY = 'athenaeum.stacking.inspectorCollapsed';

/** Read whether the (narrow-layout) inspector disclosure is collapsed
 *  (default `false` — expanded — when unset/corrupt, matching the Frames
 *  table's own default above). */
export function readInspectorCollapsed(): boolean {
  try {
    return localStorage.getItem(STACKING_INSPECTOR_COLLAPSED_KEY) === 'true';
  } catch (err) {
    console.warn('[stackingPrefs] read inspector-collapsed failed:', err);
    return false;
  }
}

/** Persist whether the (narrow-layout) inspector disclosure is collapsed. */
export function writeInspectorCollapsed(collapsed: boolean): void {
  try {
    localStorage.setItem(STACKING_INSPECTOR_COLLAPSED_KEY, String(collapsed));
  } catch (err) {
    console.warn('[stackingPrefs] write inspector-collapsed failed:', err);
  }
}

/** localStorage key prefix for the Results panel's last-selected run — one
 *  entry per frame set (a run id from one set is meaningless for another,
 *  unlike the selected stage, which is one global preference). */
const STACKING_SELECTED_RUN_KEY_PREFIX = 'athenaeum.stacking.selectedRun.';

/** Read the last-selected run id for `setId` (`null` when unset/corrupt —
 *  the Results panel then falls back to the newest run). */
export function readSelectedRunId(setId: number): number | null {
  try {
    const raw = localStorage.getItem(`${STACKING_SELECTED_RUN_KEY_PREFIX}${setId}`);
    // Fix round 1, Minor #9: `Number('')` is `0`, not `NaN` — an empty
    // string must be rejected explicitly before it silently reads back as
    // a real (and almost certainly wrong) run id.
    if (raw === null || raw === '') return null;
    const n = Number(raw);
    return Number.isFinite(n) ? n : null;
  } catch (err) {
    console.warn('[stackingPrefs] read selected run failed:', err);
    return null;
  }
}

/** Persist the last-selected run id for `setId`. */
export function writeSelectedRunId(setId: number, runId: number): void {
  try {
    localStorage.setItem(`${STACKING_SELECTED_RUN_KEY_PREFIX}${setId}`, String(runId));
  } catch (err) {
    console.warn('[stackingPrefs] write selected run failed:', err);
  }
}
