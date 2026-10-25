import { describe, expect, it } from 'vitest';
import { SNR_GAIN_STALE_TOOLTIP, SNR_GAIN_VERSION, snrGainLabel } from './snrGain';

describe('snrGainLabel', () => {
  it('shows a gain measured now', () => {
    expect(snrGainLabel({ snrGain: [12.345], snrGainVersion: SNR_GAIN_VERSION })).toEqual({
      text: 'SNR gain 12.35×',
    });
  });

  it('hides a gain stored before v0.7.0 (no version key) behind "—" with a tooltip', () => {
    expect(snrGainLabel({ snrGain: [751.98] })).toEqual({
      text: 'SNR gain —',
      title: SNR_GAIN_STALE_TOOLTIP,
    });
    expect(snrGainLabel({ snrGain: [751.98], snrGainVersion: 0 }).text).toBe('SNR gain —');
  });

  it('shows "—" without a tooltip when a current run has no gain', () => {
    expect(snrGainLabel({ snrGain: [], snrGainVersion: SNR_GAIN_VERSION })).toEqual({ text: 'SNR gain —' });
  });
});
