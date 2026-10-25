import type { GroupStats } from '../../types/stacking';

/** The `GroupStats.snrGainVersion` of a gain measured now: the paired SNR
 *  factor √(PSF SNR master / PSF SNR best sub), v0.7.0
 *  (`stacking::integrate::SNR_GAIN_VERSION`). Keep the two in step. */
export const SNR_GAIN_VERSION = 1;

/** Tooltip for a gain stored by a run before v0.7.0. */
export const SNR_GAIN_STALE_TOOLTIP = 'measured before v0.7.0 — re-run to get the SNR gain';

/** The Results card's SNR gain text, plus a tooltip when the figure is not
 *  shown. A stats document stored before v0.7.0 carries no `snrGainVersion`
 *  (the field reads `undefined` at runtime although the generated type says
 *  `number`): its `snrGain` is the old unpaired power ratio (set 109 read
 *  751.98× for 208 frames), so it shows "—" rather than passing for the
 *  same metric. */
export function snrGainLabel(stats: Pick<GroupStats, 'snrGain'> & { snrGainVersion?: number }): {
  text: string;
  title?: string;
} {
  if (stats.snrGainVersion !== SNR_GAIN_VERSION) {
    return { text: 'SNR gain —', title: SNR_GAIN_STALE_TOOLTIP };
  }
  if (stats.snrGain.length === 0) return { text: 'SNR gain —' };
  return { text: `SNR gain ${stats.snrGain[0].toFixed(2)}×` };
}
