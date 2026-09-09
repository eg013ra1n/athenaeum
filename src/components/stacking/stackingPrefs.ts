// Stacking-tab UI preferences, persisted per browser/profile in
// localStorage (same defensive-total-reader convention as
// `export/lightCalPrefs.ts` — an unset, unparsable or out-of-range value
// resolves to the documented default rather than throwing).
//
// Created in Plan 5b Task 3 for the inspector's selected board row; Task 4
// extends this file with further tab prefs (e.g. the Frames table's
// filters) rather than starting a second prefs module.

import type { BoardStage } from './stageSummary';

/** localStorage key for the last-selected inspector stage. */
export const STACKING_SELECTED_STAGE_KEY = 'athenaeum.stacking.selectedStage';

const VALID_STAGES: readonly BoardStage[] = [
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
