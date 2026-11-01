import type { GroupStats, WeightMode } from '../../types/stacking';

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

/** Tooltip for a gain of 0 under a weight mode outside the PSF family
 *  (Noise, Formula, Exposure, Keyword, None): those modes never build the
 *  frame-common star set the gain is read on (PD-19). */
export const SNR_GAIN_WEIGHT_MODE_TOOLTIP =
  'not measured under this weight mode — the frame-common star set it needs is built by the PSF weight modes only';

/** The weight modes that build the frame-common star set (`defers_weight_gate`
 *  in `stacking/weights.rs`). */
const PSF_WEIGHT_MODES: readonly WeightMode[] = ['psfSignalWeight', 'psfSnr'];

/** The tooltip of a group (or a plane) without a measured gain: the weight
 *  mode's reason outside the PSF family, else "not measured for this group".
 *  An unknown mode (the run's summary has not loaded) reads as the latter. */
function unmeasuredTitle(weightMode: WeightMode | undefined): string {
  return weightMode !== undefined && !PSF_WEIGHT_MODES.includes(weightMode)
    ? SNR_GAIN_WEIGHT_MODE_TOOLTIP
    : SNR_GAIN_UNMEASURED_TOOLTIP;
}

/** Plane names for the tooltip: R / G / B for three planes, else "plane 1" … */
function planeName(index: number, planes: number): string {
  return planes === 3 ? ['R', 'G', 'B'][index] : `plane ${index + 1}`;
}

/** "R", "R and B", "plane 1, plane 3 and plane 4". */
function joinNames(names: string[]): string {
  return names.length < 2 ? names.join('') : `${names.slice(0, -1).join(', ')} and ${names[names.length - 1]}`;
}

const isMeasured = (gain: number): boolean => Number.isFinite(gain) && gain > 0;

/** The Results card's SNR gain text, plus a tooltip when the figure is not
 *  shown. A stats document stored before v0.7.0 carries no `snrGainVersion`
 *  (the field reads `undefined` at runtime although the generated type says
 *  `number`): its `snrGain` is the old unpaired power ratio (set 109 read
 *  751.98× for 208 frames), so it shows "—" rather than passing for the
 *  same metric; a version 1 gain was read over every star and shows "—"
 *  too, and a current gain of 0 means "not measured".
 *
 *  A group with several planes shows every plane's gain, "SNR gain 7.41 /
 *  7.99 / 7.86×" (PD-20): an unmeasured plane is "—" in its slot, named in
 *  the tooltip; all planes unmeasured reads "SNR gain —". `weightMode` is the
 *  run's `summary.config.measurement.weightMode`: a gain of 0 outside the PSF
 *  family says so (PD-19). */
export function snrGainLabel(
  stats: Pick<GroupStats, 'snrGain'> & { snrGainVersion?: number },
  weightMode?: WeightMode,
): {
  text: string;
  title?: string;
} {
  if (stats.snrGainVersion !== SNR_GAIN_VERSION) {
    const title = stats.snrGainVersion ? SNR_GAIN_OLDER_FORMULA_TOOLTIP : SNR_GAIN_STALE_TOOLTIP;
    return { text: 'SNR gain —', title };
  }
  const gains = stats.snrGain;
  if (gains.length === 0) return { text: 'SNR gain —' };
  if (gains.length === 1) {
    return isMeasured(gains[0])
      ? { text: `SNR gain ${gains[0].toFixed(2)}×` }
      : { text: 'SNR gain —', title: unmeasuredTitle(weightMode) };
  }
  const missing = gains.flatMap((gain, i) => (isMeasured(gain) ? [] : [i]));
  if (missing.length === gains.length) return { text: 'SNR gain —', title: unmeasuredTitle(weightMode) };
  const text = `SNR gain ${gains.map((gain) => (isMeasured(gain) ? gain.toFixed(2) : '—')).join(' / ')}×`;
  if (missing.length === 0) return { text };
  const names = joinNames(missing.map((i) => planeName(i, gains.length)));
  return { text, title: `${names} ${SNR_GAIN_UNMEASURED_TOOLTIP}` };
}
