import { describe, expect, it } from 'vitest';
import { MEMBER_TONES, memberTone } from './memberColors';

const ALL = [
  { accountId: 'acc-b', displayName: 'Bob' },
  { accountId: 'acc-a', displayName: 'Alice' },
  { accountId: 'acc-c', displayName: 'Carol' },
];

describe('memberTone', () => {
  it('is stable across calls for the same member set', () => {
    const first = memberTone('acc-b', ALL);
    const second = memberTone('acc-b', [...ALL]); // fresh array, same members
    expect(first).toBe(second);
  });

  it('differs for two different members', () => {
    expect(memberTone('acc-a', ALL)).not.toBe(memberTone('acc-b', ALL));
  });

  it('is stable regardless of input order (sorts by displayName then accountId)', () => {
    const shuffled = [ALL[2], ALL[0], ALL[1]];
    expect(memberTone('acc-a', ALL)).toBe(memberTone('acc-a', shuffled));
    expect(memberTone('acc-c', ALL)).toBe(memberTone('acc-c', shuffled));
  });

  it('assigns tones in displayName order, wrapping modulo the palette length', () => {
    // Sorted by displayName: Alice(0) Bob(1) Carol(2)
    expect(memberTone('acc-a', ALL)).toBe(MEMBER_TONES[0]);
    expect(memberTone('acc-b', ALL)).toBe(MEMBER_TONES[1]);
    expect(memberTone('acc-c', ALL)).toBe(MEMBER_TONES[2]);
  });

  it('falls back to the first tone for a member missing from the list', () => {
    expect(memberTone('nobody', ALL)).toBe(MEMBER_TONES[0]);
  });

  it('every tone is a design-token class, never a raw hex value', () => {
    for (const tone of MEMBER_TONES) {
      expect(tone).toMatch(/^bg-/);
      expect(tone).not.toMatch(/^#/);
    }
  });
});
