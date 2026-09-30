import { describe, expect, it } from 'vitest';
import { MEMBER_PALETTE, memberColor } from './memberColors';

const all = [
  { accountId: 'a-kostya', displayName: 'Kostya' },
  { accountId: 'a-you', displayName: 'Vilen' },
  { accountId: 'a-andrei', displayName: 'Andrei' },
];

describe('memberColor (spec §4.4)', () => {
  it('always gives this account the accent colour', () => {
    expect(memberColor('a-you', all, 'a-you')).toBe('#88c0d0');
  });
  it('gives the others the remaining palette by displayName order', () => {
    expect(memberColor('a-andrei', all, 'a-you')).toBe(MEMBER_PALETTE[1]);
    expect(memberColor('a-kostya', all, 'a-you')).toBe(MEMBER_PALETTE[2]);
  });
  it('without a known self, everyone takes palette order from index 0', () => {
    expect(memberColor('a-andrei', all, null)).toBe(MEMBER_PALETTE[0]);
  });
  it('an unknown account gets the first slot, never undefined', () => {
    expect(memberColor('nobody', all, 'a-you')).toBe(MEMBER_PALETTE[0]);
  });
});
