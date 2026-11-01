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
});
