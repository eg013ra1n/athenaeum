import type { GroupStats } from '../../types/stacking';

/** The `GroupStats.snrGainVersion` of a gain measured now: the paired SNR
 *  factor √(PSF SNR master / PSF SNR best sub) read over the group's
 *  frame-common star set (Plan D D1b)
 *  (`stacking::integrate::SNR_GAIN_VERSION`). Keep the two in step.
 *  1 = the same factor over every star of the two images (v0.7.0). */
export const SNR_GAIN_VERSION = 2;

/** Tooltip for a gain stored by a run before v0.7.0 (version 0). */
export const SNR_GAIN_STALE_TOOLTIP = 'measured before v0.7.0 — re-run to get the SNR gain';

/** Tooltip for a gain measured by an earlier formula (version 1, over every
 *  star): not comparable with the frame-common-set figure. */
export const SNR_GAIN_OLDER_FORMULA_TOOLTIP =
  'measured with an older formula — re-run to get the SNR gain';

/** Tooltip for a current run whose pair could not be measured: the backend
 *  reports 0 for a plane without a frame-common star set, or with fewer than
 *  20 stars paired. */
export const SNR_GAIN_UNMEASURED_TOOLTIP = 'not measured for this group';

/** The Results card's SNR gain text, plus a tooltip when the figure is not
 *  shown. A stats document stored before v0.7.0 carries no `snrGainVersion`
 *  (the field reads `undefined` at runtime although the generated type says
 *  `number`): its `snrGain` is the old unpaired power ratio (set 109 read
 *  751.98× for 208 frames), so it shows "—" rather than passing for the
 *  same metric; a version 1 gain was read over every star and shows "—"
 *  too, and a current gain of 0 means "not measured". */
export function snrGainLabel(stats: Pick<GroupStats, 'snrGain'> & { snrGainVersion?: number }): {
  text: string;
  title?: string;
} {
  if (stats.snrGainVersion !== SNR_GAIN_VERSION) {
    const title = stats.snrGainVersion ? SNR_GAIN_OLDER_FORMULA_TOOLTIP : SNR_GAIN_STALE_TOOLTIP;
    return { text: 'SNR gain —', title };
  }
  if (stats.snrGain.length === 0) return { text: 'SNR gain —' };
  const gain = stats.snrGain[0];
  if (!Number.isFinite(gain) || gain <= 0) {
    return { text: 'SNR gain —', title: SNR_GAIN_UNMEASURED_TOOLTIP };
  }
  return { text: `SNR gain ${gain.toFixed(2)}×` };
}
