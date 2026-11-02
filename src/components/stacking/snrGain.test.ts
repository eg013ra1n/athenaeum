import { describe, expect, it } from 'vitest';
import {
  SNR_GAIN_OLDER_FORMULA_TOOLTIP,
  SNR_GAIN_STALE_TOOLTIP,
  SNR_GAIN_UNMEASURED_TOOLTIP,
  SNR_GAIN_VERSION,
  snrGainLabel,
} from './snrGain';

describe('snrGainLabel', () => {
  it('is on version 2, the frame-common-set formula', () => {
    expect(SNR_GAIN_VERSION).toBe(2);
  });

  it('shows a gain measured now (v2)', () => {
    expect(snrGainLabel({ snrGain: [12.345], snrGainVersion: 2 })).toEqual({
      text: 'SNR gain 12.35×',
    });
    expect(snrGainLabel({ snrGain: [12.345], snrGainVersion: SNR_GAIN_VERSION }).text).toBe('SNR gain 12.35×');
  });

  it('hides a v1 gain (read over every star) behind "—" with the older-formula tooltip', () => {
    expect(snrGainLabel({ snrGain: [2.52], snrGainVersion: 1 })).toEqual({
      text: 'SNR gain —',
      title: 'measured with an older formula — re-run to get the SNR gain',
    });
    expect(SNR_GAIN_OLDER_FORMULA_TOOLTIP).toBe('measured with an older formula — re-run to get the SNR gain');
  });

  it('hides a gain stored before v0.7.0 (no version key, or 0) behind "—" with its v0.7.0 tooltip', () => {
    expect(snrGainLabel({ snrGain: [751.98] })).toEqual({
      text: 'SNR gain —',
      title: SNR_GAIN_STALE_TOOLTIP,
    });
    expect(snrGainLabel({ snrGain: [751.98], snrGainVersion: 0 })).toEqual({
      text: 'SNR gain —',
      title: 'measured before v0.7.0 — re-run to get the SNR gain',
    });
  });

  it('shows "—" with "not measured for this group" when a current run reports a gain of 0', () => {
    expect(snrGainLabel({ snrGain: [0], snrGainVersion: SNR_GAIN_VERSION })).toEqual({
      text: 'SNR gain —',
      title: 'not measured for this group',
    });
    expect(SNR_GAIN_UNMEASURED_TOOLTIP).toBe('not measured for this group');
  });

  it('shows "—" without a tooltip when a current run has no gain at all', () => {
    expect(snrGainLabel({ snrGain: [], snrGainVersion: SNR_GAIN_VERSION })).toEqual({ text: 'SNR gain —' });
  });

  it('reads a gain of 0 the same under every weight mode (PD-19 b)', () => {
    // The frame-common star set is built in every weight mode, so the label takes the stats alone (the type has no mode): a 0 is "not measured
    // for this group" under Noise / Formula / Exposure / Keyword / None as under the PSF modes.
    expect(snrGainLabel({ snrGain: [0], snrGainVersion: SNR_GAIN_VERSION })).toEqual({
      text: 'SNR gain —',
      title: 'not measured for this group',
    });
    // a colour group, every plane 0: the same text
    expect(snrGainLabel({ snrGain: [0, 0, 0], snrGainVersion: SNR_GAIN_VERSION })).toEqual({
      text: 'SNR gain —',
      title: 'not measured for this group',
    });
    // a measured gain is shown as it is
    expect(snrGainLabel({ snrGain: [7.08], snrGainVersion: SNR_GAIN_VERSION })).toEqual({
      text: 'SNR gain 7.08×',
    });
    // the v0 / v1 branches stay as they were
    expect(snrGainLabel({ snrGain: [0], snrGainVersion: 1 }).title).toBe(SNR_GAIN_OLDER_FORMULA_TOOLTIP);
    expect(snrGainLabel({ snrGain: [0] }).title).toBe(SNR_GAIN_STALE_TOOLTIP);
  });

  it('shows every plane of a colour group, two decimals, " / " between them (PD-20)', () => {
    expect(snrGainLabel({ snrGain: [7.41, 7.99, 7.86], snrGainVersion: SNR_GAIN_VERSION })).toEqual({
      text: 'SNR gain 7.41 / 7.99 / 7.86×',
    });
    expect(snrGainLabel({ snrGain: [7.4149, 8, 7.855], snrGainVersion: SNR_GAIN_VERSION }).text).toBe(
      'SNR gain 7.41 / 8.00 / 7.86×',
    );
    expect(snrGainLabel({ snrGain: [3.1, 4.256], snrGainVersion: SNR_GAIN_VERSION }).text).toBe('SNR gain 3.10 / 4.26×');
  });

  it('puts "—" in the slot of a plane that was not measured and names it in the tooltip (PD-20)', () => {
    const v = SNR_GAIN_VERSION;
    expect(snrGainLabel({ snrGain: [0, 7.99, 7.86], snrGainVersion: v })).toEqual({
      text: 'SNR gain — / 7.99 / 7.86×',
      title: 'R not measured for this group',
    });
    expect(snrGainLabel({ snrGain: [7.41, 0, 7.86], snrGainVersion: v }).title).toBe('G not measured for this group');
    expect(snrGainLabel({ snrGain: [7.41, 7.99, Number.NaN], snrGainVersion: v })).toEqual({
      text: 'SNR gain 7.41 / 7.99 / —',
      title: 'B not measured for this group',
    });
    // several planes are named together
    expect(snrGainLabel({ snrGain: [0, 7.99, 0], snrGainVersion: v })).toEqual({
      text: 'SNR gain — / 7.99 / —',
      title: 'R and B not measured for this group',
    });
    // any other plane count: "plane N", counted from 1
    expect(snrGainLabel({ snrGain: [5, 0], snrGainVersion: v })).toEqual({
      text: 'SNR gain 5.00 / —',
      title: 'plane 2 not measured for this group',
    });
    expect(snrGainLabel({ snrGain: [0, 5, 0, 6], snrGainVersion: v }).title).toBe(
      'plane 1 and plane 3 not measured for this group',
    );
    expect(snrGainLabel({ snrGain: [0, 5, 0, 0], snrGainVersion: v }).title).toBe(
      'plane 1, plane 3 and plane 4 not measured for this group',
    );
  });

  it('shows "SNR gain —" when every plane of a colour group is 0, with the group-level tooltip (PD-20)', () => {
    expect(snrGainLabel({ snrGain: [0, 0, 0], snrGainVersion: SNR_GAIN_VERSION })).toEqual({
      text: 'SNR gain —',
      title: 'not measured for this group',
    });
    expect(snrGainLabel({ snrGain: [0, Number.NaN], snrGainVersion: SNR_GAIN_VERSION })).toEqual({
      text: 'SNR gain —',
      title: 'not measured for this group',
    });
  });

  it('leaves the one-plane label and the v0 / v1 branches as they were', () => {
    expect(snrGainLabel({ snrGain: [12.345], snrGainVersion: SNR_GAIN_VERSION }).text).toBe('SNR gain 12.35×');
    expect(snrGainLabel({ snrGain: [7.41, 7.99, 7.86], snrGainVersion: 1 })).toEqual({
      text: 'SNR gain —',
      title: SNR_GAIN_OLDER_FORMULA_TOOLTIP,
    });
    expect(snrGainLabel({ snrGain: [7.41, 7.99, 7.86] })).toEqual({ text: 'SNR gain —', title: SNR_GAIN_STALE_TOOLTIP });
  });
});
