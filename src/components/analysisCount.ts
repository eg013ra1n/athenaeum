/**
 * What the Analysis tab writes beside the Analyze action: how many frames have an analysis, how many zero points
 * Analyze would still measure ("N to update"), and what the frames that Analyze cannot help are waiting for. The
 * numbers come from core (`get_zero_point_updates`); this is only their wording and the one decision of which
 * button to show.
 */

import type { ZeroPointUpdates } from '../types/models';
import { pluralize } from './collab/format';

export interface AnalysisCountText {
  /** Every light has an analysis row and no zero point waits to be measured: the button is Re-Analyze. */
  settled: boolean;
  /** `12/30`, or the empty string when nothing is analysed yet (the button says Analyze itself). */
  count: string;
  /** `5 to update` when Analyze would measure zero points; null otherwise. */
  update: string | null;
  /** The one line for frames waiting on a catalogue that is not installed (never "run Analyze"); null otherwise. */
  catalogue: string | null;
  /** Hover text for the count: the zero-point groups and what each is waiting for; null when there is nothing to say. */
  title: string | null;
}

/** A frame set with no zero-point count read yet (or a failed read) counts nothing: the tab shows the plain count. */
export const NO_UPDATES: ZeroPointUpdates = { toUpdate: 0, noCatalogue: 0, measurementFailed: 0 };

export function analysisCountText(
  analysed: number,
  total: number,
  updates: ZeroPointUpdates = NO_UPDATES,
): AnalysisCountText {
  const allAnalysed = analysed >= total;
  const settled = allAnalysed && updates.toUpdate === 0;
  const count = analysed > 0 && !allAnalysed ? `${analysed}/${total}` : '';
  const update = updates.toUpdate > 0 ? `${updates.toUpdate} to update` : null;
  const catalogue =
    updates.noCatalogue > 0
      ? `Install the catalogue (${updates.noCatalogue} ${pluralize(updates.noCatalogue, 'frame')})`
      : null;
  const lines: string[] = [];
  if (updates.toUpdate > 0) {
    lines.push(
      `${updates.toUpdate} ${pluralize(updates.toUpdate, 'frame')} to measure for transparency — Analyze reads only those`,
    );
  }
  if (updates.noCatalogue > 0) {
    lines.push(
      `${updates.noCatalogue} ${pluralize(updates.noCatalogue, 'frame')} wait for the star catalogue — install it to measure transparency`,
    );
  }
  if (updates.measurementFailed > 0) {
    lines.push(
      `the zero point measurement failed on ${updates.measurementFailed} ${pluralize(updates.measurementFailed, 'frame')} — see the logs`,
    );
  }
  return { settled, count, update, catalogue, title: lines.length > 0 ? lines.join('\n') : null };
}
